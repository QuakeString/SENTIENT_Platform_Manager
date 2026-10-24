// Copyright © 2016-2026 The SENTIENT Authors

//! A SENTIENT database left by an earlier install ("Uninstall, keep the
//! database"), found by a new install.
//!
//! The operator chooses to keep using it or to start fresh. Starting fresh
//! sets it aside (renamed `<db>_before_install_<time>`, listed under Kept),
//! never deletes it. Keeping it is checked against the deployment mode the
//! new install uses, because the shape of the history table `ts_kv` was
//! fixed when that database was first installed and the schema installer
//! does not touch an installed database:
//!
//! | kept `ts_kv`        | → TimescaleDB          | → ClickHouse | → PostgreSQL only |
//! |---------------------|------------------------|--------------|-------------------|
//! | hypertable          | as is                  | as is        | refused           |
//! | plain table         | converted (rows moved) | as is        | as is             |
//! | partitioned table   | refused                | as is        | as is             |
//!
//! "PostgreSQL only" cannot read a hypertable without TimescaleDB, and a
//! partitioned table cannot become a hypertable in place. A refusal happens
//! before anything is changed.

use crate::progress::{Progress, ProgressFn};
use crate::stack::Flavor;

/// The schema installer's conversion of `ts_kv` into a hypertable, as
/// SENTIENT ships it (data/sql/schema-ts-timescale.sql).
pub const TIMESCALE_SQL: &str = include_str!("schema-ts-timescale.sql");

/// Runs one SQL statement as the PostgreSQL superuser in database `db` and
/// returns its output (unaligned, tuples only), or the error.
pub type Sql<'a> = dyn Fn(&str, &str) -> Result<String, String> + 'a;

/// What the earlier install left in the history table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// No `ts_kv` (an install that stopped before its schema).
    Missing,
    Hypertable,
    Partitioned,
    Plain,
}

impl Shape {
    /// The deployment mode a database of this shape was installed for.
    pub fn installed_for(&self) -> &'static str {
        match self {
            Shape::Missing => "an install that did not finish",
            Shape::Hypertable => "PostgreSQL + TimescaleDB",
            Shape::Partitioned => "PostgreSQL only",
            Shape::Plain => "PostgreSQL + ClickHouse (or PostgreSQL only)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Existing {
    pub database: String,
    pub shape: Shape,
    /// Rows in `ts_kv` (an estimate for a big table).
    pub history_rows: i64,
    pub devices: i64,
    pub dashboards: i64,
    pub bytes: i64,
}

impl Existing {
    pub fn summary(&self) -> String {
        format!(
            "{}: set up for {}; {} devices, {} dashboards, {} history rows, {}",
            self.database,
            self.shape.installed_for(),
            self.devices,
            self.dashboards,
            self.history_rows,
            size(self.bytes)
        )
    }
}

fn size(bytes: i64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", bytes >> 20)
    }
}

/// What keeping `existing` under `flavor` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Fine,
    /// The plain history table becomes a hypertable; its rows move.
    ConvertToHypertable,
    Refused(String),
}

pub fn verdict(existing: &Existing, flavor: Flavor) -> Verdict {
    match (&existing.shape, flavor) {
        (Shape::Hypertable, Flavor::Stock) => Verdict::Refused(format!(
            "{} was installed for PostgreSQL + TimescaleDB: its history table needs TimescaleDB, which \
             “PostgreSQL only” does not have. Choose PostgreSQL + TimescaleDB (or ClickHouse), or Start fresh.",
            existing.database
        )),
        (Shape::Partitioned, Flavor::Timescaledb) => Verdict::Refused(format!(
            "{} was installed for PostgreSQL only: its history table is partitioned by month and cannot \
             become a TimescaleDB table in place. Keep the PostgreSQL only mode, or Start fresh.",
            existing.database
        )),
        (Shape::Plain, Flavor::Timescaledb) => Verdict::ConvertToHypertable,
        _ => Verdict::Fine,
    }
}

/// The database `db`, if the cluster has it.
pub fn inspect(sql: &Sql, db: &str) -> Result<Option<Existing>, String> {
    let exists = sql("postgres", &format!("SELECT count(*) FROM pg_database WHERE datname = {}", quote(db)))?;
    if exists.trim() != "1" {
        return Ok(None);
    }
    let shape = match sql(db, "SELECT relkind FROM pg_class WHERE relname = 'ts_kv' AND relnamespace = 'public'::regnamespace")?.trim() {
        "" => Shape::Missing,
        "p" => Shape::Partitioned,
        _ => {
            let hyper = sql(
                db,
                "SELECT count(*) FROM pg_extension e WHERE e.extname = 'timescaledb' \
                 AND EXISTS (SELECT 1 FROM timescaledb_information.hypertables WHERE hypertable_name = 'ts_kv')",
            )
            // Without the extension the view does not exist: not a hypertable.
            .unwrap_or_default();
            if hyper.trim() == "1" { Shape::Hypertable } else { Shape::Plain }
        }
    };
    let number = |q: &str| sql(db, q).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let history_rows = match shape {
        Shape::Missing => 0,
        // Exact for a small table, the planner's estimate for a big one.
        _ => {
            let estimate = number("SELECT COALESCE(sum(c.reltuples), 0)::bigint FROM pg_class c WHERE c.relname = 'ts_kv' OR c.oid IN (SELECT inhrelid FROM pg_inherits WHERE inhparent = 'ts_kv'::regclass)");
            if estimate < 1_000_000 { number("SELECT count(*) FROM ts_kv") } else { estimate }
        }
    };
    Ok(Some(Existing {
        database: db.to_string(),
        shape,
        history_rows,
        devices: number("SELECT count(*) FROM device"),
        dashboards: number("SELECT count(*) FROM dashboard"),
        bytes: number(&format!("SELECT pg_database_size({})", quote(db))),
    }))
}

/// Start fresh: rename `db` aside so the install creates a new one. Returns
/// the name it is kept under.
pub fn set_aside(sql: &Sql, db: &str, sink: &ProgressFn) -> Result<String, String> {
    let aside = format!("{db}_before_install_{}", chrono::Local::now().format("%Y%m%d%H%M"));
    // Nothing may be connected to a database being renamed.
    let _ = sql(
        "postgres",
        &format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = {} AND pid <> pg_backend_pid()", quote(db)),
    );
    sql("postgres", &format!("ALTER DATABASE {} RENAME TO {}", ident(db), ident(&aside)))
        .map_err(|e| format!("set the earlier database aside: {e}"))?;
    sink(Progress::Log { line: format!("The earlier database is kept as {aside} (listed under Kept on the Migration screen); this install starts a new one.") });
    Ok(aside)
}

/// Keep: make `existing` fit `flavor`, or refuse before changing anything.
/// `timescale_sql` is the schema installer's own conversion script.
pub fn prepare_keep(sql: &Sql, existing: &Existing, flavor: Flavor, timescale_sql: &str, sink: &ProgressFn) -> Result<(), String> {
    match verdict(existing, flavor) {
        Verdict::Fine => {
            sink(Progress::Log { line: format!("Keeping {}.", existing.summary()) });
            Ok(())
        }
        Verdict::Refused(why) => Err(why),
        Verdict::ConvertToHypertable => {
            sink(Progress::Step {
                name: format!("Converting the kept history table to TimescaleDB ({} rows)", existing.history_rows),
            });
            sql(&existing.database, "CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE")?;
            // The script converts an empty table; rows already there move
            // into the new chunks (migrate_data), which takes a while for many.
            // Only the create_hypertable call (the first such line): the
            // script's later calls and comments end the same way.
            let script = timescale_sql.replacen("if_not_exists => true);", "if_not_exists => true,\n                         migrate_data => true);", 1);
            // Without its comments: they are not ASCII (©, —), and psql on
            // Windows reads its command line in the ANSI code page.
            let script: String = script
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .map(|l| format!("{l}\n"))
                .collect();
            sql(&existing.database, &script).map_err(|e| format!("convert ts_kv to a TimescaleDB table: {e}"))?;
            sink(Progress::Log { line: format!("Kept {}; its history table is now a TimescaleDB table.", existing.database) });
            Ok(())
        }
    }
}

/// The kept cluster's SENTIENT role still has the earlier install's
/// password; SENTIENT connects with this install's. Set it, when the role
/// exists (a fresh role is created with it later).
pub fn sync_role_password(sql: &Sql, user: &str, password: &str) -> Result<(), String> {
    let exists = sql("postgres", &format!("SELECT count(*) FROM pg_roles WHERE rolname = {}", quote(user)))?;
    if exists.trim() == "1" {
        sql("postgres", &format!("ALTER ROLE {} WITH PASSWORD {}", ident(user), quote(password)))
            .map_err(|e| format!("set the database user's password: {e}"))?;
    }
    Ok(())
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(shape: Shape) -> Existing {
        Existing { database: "sentient".into(), shape, history_rows: 0, devices: 0, dashboards: 0, bytes: 0 }
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict(&kept(Shape::Plain), Flavor::Timescaledb), Verdict::ConvertToHypertable);
        assert!(matches!(verdict(&kept(Shape::Partitioned), Flavor::Timescaledb), Verdict::Refused(_)));
        assert!(matches!(verdict(&kept(Shape::Hypertable), Flavor::Stock), Verdict::Refused(_)));
        for shape in [Shape::Hypertable, Shape::Plain, Shape::Partitioned, Shape::Missing] {
            assert_eq!(verdict(&kept(shape), Flavor::Clickhouse), Verdict::Fine);
        }
        assert_eq!(verdict(&kept(Shape::Hypertable), Flavor::Timescaledb), Verdict::Fine);
        assert_eq!(verdict(&kept(Shape::Plain), Flavor::Stock), Verdict::Fine);
    }

    #[test]
    fn the_conversion_moves_rows() {
        let script = "SELECT create_hypertable('ts_kv', by_range('ts', 604800000::bigint),\n                         create_default_indexes => false,\n                         if_not_exists => true);";
        let out = TIMESCALE_SQL.replacen("if_not_exists => true);", "if_not_exists => true,\n                         migrate_data => true);", 1);
        assert_eq!(out.matches("migrate_data").count(), 1);
        let at = out.find("migrate_data").unwrap();
        assert!(out[..at].rfind("create_hypertable").is_some());
        let _ = script;
    }
}
