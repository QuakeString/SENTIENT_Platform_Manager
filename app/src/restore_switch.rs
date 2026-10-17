// Copyright © 2016-2026 The SENTIENT Authors

//! Putting a restored backup into service. A restore goes into a new
//! database next to the live one (PostgreSQL, and ClickHouse on a ClickHouse
//! installation); this makes SENTIENT use it:
//!
//! 1. Stop SENTIENT.
//! 2. PostgreSQL: the live database is renamed `<db>_before_restore_<stamp>`
//!    and the restored one takes its name.
//! 3. ClickHouse: the history and latest-value tables are exchanged with the
//!    restored database's (`EXCHANGE TABLES`, atomic per table); the live
//!    database's views then feed the restored tables. The data from before
//!    stays in the restored database's name.
//! 4. Start SENTIENT and check it answers; if not, everything is swapped back
//!    and the previous data runs again.
//!
//! Nothing is deleted: the data from before is listed under "Kept after
//! earlier changes".

use sentient_backup_core::clickhouse::ChConn;
use sentient_backup_core::db::ConnConfig;
use sentient_backup_core::telemetry_move as mv;
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use crate::installation::InstallationRecord;
use crate::migration::Setup;

/// The ClickHouse tables that hold data (the rest are views and a Null table).
const CH_DATA_TABLES: [&str; 2] = ["ts_kv", "ts_kv_latest_agg"];

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(future))
        .map_err(|e| format!("start the runtime: {e}"))
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// SQL on the maintenance database, as SENTIENT's database user (a superuser
/// on every engine).
async fn admin_sql(pg: &ConnConfig, sql: &str) -> Result<(), String> {
    let cfg = ConnConfig { dbname: "postgres".into(), ..pg.clone() };
    let client = mv::connect(&cfg).await.map_err(|e| e.to_string())?;
    client.batch_execute(sql).await.map_err(|e| e.to_string())
}

async fn database_exists(pg: &ConnConfig, name: &str) -> Result<bool, String> {
    let cfg = ConnConfig { dbname: "postgres".into(), ..pg.clone() };
    let client = mv::connect(&cfg).await.map_err(|e| e.to_string())?;
    let row = client
        .query_one("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)", &[&name])
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.get(0))
}

/// Rename `from` to `to`, with no one connected to `from`.
async fn rename_database(pg: &ConnConfig, from: &str, to: &str) -> Result<(), String> {
    let (f, t) = (quote_ident(from), quote_ident(to));
    admin_sql(
        pg,
        &format!(
            "ALTER DATABASE {f} WITH ALLOW_CONNECTIONS false; \
             SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid();",
            from.replace('\'', "''")
        ),
    )
    .await?;
    let result = admin_sql(pg, &format!("ALTER DATABASE {f} RENAME TO {t}")).await;
    let renamed = if result.is_ok() { &t } else { &f };
    let _ = admin_sql(pg, &format!("ALTER DATABASE {renamed} WITH ALLOW_CONNECTIONS true")).await;
    result
}

fn ch_exchange(ch: &ChConn, restored: &str) -> Result<(), String> {
    for table in CH_DATA_TABLES {
        ch.text(&format!("EXCHANGE TABLES {}.{table} AND {restored}.{table}", ch.database))
            .map_err(|e| format!("exchange {table}: {e}"))?;
    }
    Ok(())
}

fn ch_database_exists(ch: &ChConn, name: &str) -> Result<bool, String> {
    let n = ch
        .with_database("default")
        .text(&format!("SELECT count() FROM system.databases WHERE name = '{name}'"))
        .map_err(|e| e.to_string())?;
    Ok(n.trim() != "0")
}

fn wait_answering(port: u16) -> bool {
    for _ in 0..90 {
        if sentient_installer_core::native::http_answers(port) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    false
}

async fn schema_version(pg: &ConnConfig) -> Result<i64, String> {
    let client = mv::connect(pg).await.map_err(|e| e.to_string())?;
    let row = client
        .query_opt("SELECT schema_version FROM tb_schema_settings LIMIT 1", &[])
        .await
        .map_err(|e| format!("read the schema version of {}: {e}", pg.dbname))?;
    row.map(|r| r.get(0)).ok_or_else(|| format!("{} has no schema version", pg.dbname))
}

/// After a restore into `target`: its schema against the running SENTIENT's.
/// A backup from a newer SENTIENT cannot run here; one from an older SENTIENT
/// is upgraded (natively with the installed schema tool; on Docker SENTIENT
/// upgrades it itself when it starts on it). `live` signs in to SENTIENT's
/// database; `ch_password` is ClickHouse's, on a ClickHouse installation.
pub(crate) fn match_schema(
    record: &InstallationRecord,
    live: &ConnConfig,
    ch_password: &str,
    target: &str,
    sink: &ProgressFn,
) -> Result<(), String> {
    let restored = ConnConfig { dbname: target.to_string(), ..live.clone() };
    let (have, got) = block_on(async { Ok::<_, String>((schema_version(live).await?, schema_version(&restored).await?)) })??;
    if got > have {
        return Err(format!(
            "The backup is from a newer SENTIENT (schema {got}) than this one (schema {have}). Update SENTIENT, then put the restored database {target} into service."
        ));
    }
    if got == have {
        return Ok(());
    }
    match record.engine {
        crate::installation::Engine::Docker => {
            sink(Progress::Log { line: format!("The backup is from an older SENTIENT (schema {got}); SENTIENT upgrades it to schema {have} when it starts on it.") });
            Ok(())
        }
        crate::installation::Engine::Native => {
            step(sink, &format!("Upgrading the restored data from schema {got} to {have}"));
            let mut cfg = record.native_config();
            cfg.db_password = live.password.clone();
            cfg.ch_password = ch_password.to_string();
            let out = sentient_installer_core::native::safe_update::upgrade_restored(&cfg, target)
                .map_err(|e| format!("The restored data could not be upgraded to this SENTIENT's schema: {e}"))?;
            for line in out.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev() {
                sink(Progress::Log { line: line.to_string() });
            }
            Ok(())
        }
    }
}

/// Make SENTIENT use the restored database `name`. Returns the name the data
/// from before is kept under.
pub fn run(record: &InstallationRecord, name: &str, sink: ProgressFn) -> Result<String, String> {
    if !valid_name(name) {
        return Err(format!("'{name}' is not a database name this can use."));
    }
    let setup = Setup::current(record)?;
    let pg = setup.pg();
    let live = pg.dbname.clone();
    if name == live {
        return Err("That is the database SENTIENT already uses.".into());
    }
    if !block_on(database_exists(&pg, name))?? {
        return Err(format!("There is no PostgreSQL database called {name}; restore a backup into it first."));
    }
    let ch = (setup.flavor() == Flavor::Clickhouse).then(|| setup.ch());
    if let Some(ch) = &ch {
        if !ch_database_exists(ch, name)? {
            return Err(format!(
                "There is no ClickHouse database called {name}: the backup was restored without its ClickHouse history."
            ));
        }
    }

    setup.stop(&sink).inspect_err(|_| setup.restart_as_was(&sink))?;
    let aside = format!("{live}_before_restore_{}", chrono::Local::now().format("%Y%m%d%H%M"));

    step(&sink, "Switching PostgreSQL to the restored database");
    let switched = block_on(async {
        rename_database(&pg, &live, &aside).await?;
        if let Err(e) = rename_database(&pg, name, &live).await {
            let _ = rename_database(&pg, &aside, &live).await;
            return Err(e);
        }
        Ok::<_, String>(())
    })?;
    if let Err(e) = switched {
        setup.restart_as_was(&sink);
        return Err(format!("Could not switch the databases ({e}); SENTIENT runs on its data as before."));
    }
    let undo_pg = || {
        let _ = block_on(async {
            let _ = rename_database(&pg, &live, name).await;
            let _ = rename_database(&pg, &aside, &live).await;
        });
    };

    if let Some(ch) = &ch {
        step(&sink, "Switching ClickHouse to the restored history");
        if let Err(e) = ch_exchange(ch, name) {
            // A partial exchange is undone by exchanging again.
            let _ = ch_exchange(ch, name);
            undo_pg();
            setup.restart_as_was(&sink);
            return Err(format!("Could not switch ClickHouse ({e}); SENTIENT runs on its data as before."));
        }
    }

    step(&sink, "Starting SENTIENT on the restored data");
    setup.restart_as_was(&sink);
    if !wait_answering(record.stack.http_port) {
        sink(Progress::Log { line: "SENTIENT did not answer on the restored data; putting the previous data back".into() });
        let _ = setup.stop(&sink);
        if let Some(ch) = &ch {
            let _ = ch_exchange(ch, name);
        }
        undo_pg();
        setup.restart_as_was(&sink);
        let back = if wait_answering(record.stack.http_port) {
            "It runs on its previous data again"
        } else {
            "Its previous data is back in place but SENTIENT is not answering yet; check Status and the logs"
        };
        return Err(format!("SENTIENT did not start on the restored data. {back}; the restored database is unchanged."));
    }

    let ch_note = if ch.is_some() { format!(", and ClickHouse's in the database {name}") } else { String::new() };
    sink(Progress::Done {
        message: format!("SENTIENT runs on the restored data. The data from before is kept as {aside}{ch_note}."),
    });
    Ok(aside)
}
