// Copyright © 2016-2026 The SENTIENT Authors

//! What earlier migrations and updates left in place, and removing it.
//!
//! Nothing is deleted automatically: a migration keeps the old history (a
//! set-aside `ts_kv_<type>_<stamp>` table, or ClickHouse after leaving it),
//! an update keeps a copy of the database from before it, and a failed
//! update keeps the database it left behind. The operator removes them here
//! once they are sure they no longer need them.

use sentient_backup_core::db::ConnConfig;
use sentient_backup_core::telemetry_move as mv;
use sentient_installer_core::progress::{Progress, ProgressFn};
#[cfg(windows)]
use sentient_installer_core::stack::Flavor;

use crate::installation::{Engine, InstallationRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptKind {
    /// History set aside by a migration.
    History,
    /// A database kept by an update (before it, or the failed one).
    DatabaseCopy,
    /// ClickHouse in WSL2, set up but no longer used.
    ClickHouse,
    /// A ClickHouse database besides the live one (a restore, or the data
    /// from before a restore was put into service).
    ClickhouseDatabase,
}

#[derive(Debug, Clone)]
pub struct Kept {
    pub kind: KeptKind,
    pub name: String,
    pub bytes: Option<i64>,
    pub detail: String,
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(future))
        .map_err(|e| format!("start the runtime: {e}"))
}

fn conn(record: &InstallationRecord, dbname: &str) -> Result<ConnConfig, String> {
    if record.stack.pg_port == 0 {
        return Err(crate::installation::PG_NOT_PUBLISHED.into());
    }
    let password = match record.engine {
        Engine::Docker => sentient_installer_core::distro::docker_ops::read_secrets(&record.stack)?.db_password,
        Engine::Native => record.native_config_with_secrets()?.db_password,
    };
    Ok(ConnConfig {
        host: "127.0.0.1".into(),
        port: record.stack.pg_port,
        dbname: dbname.into(),
        user: record.stack.db_user.clone(),
        password,
    })
}

/// Everything kept on this installation. Blocking: call from a job or a
/// background task.
pub fn list(record: &InstallationRecord) -> Result<Vec<Kept>, String> {
    let db = record.stack.db_name.clone();
    let cfg = conn(record, &db)?;
    let mut kept: Vec<Kept> = block_on(async move {
        let pg = mv::connect(&cfg).await.map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        // Set-aside history: top-level tables only (not partitions), with
        // their partitions or chunks counted in the size.
        for row in pg
            .query(
                "WITH RECURSIVE tree(root, rel) AS ( \
                   SELECT c.oid, c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                   WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') \
                     AND c.relname ~ '^ts_kv_(stock|timescaledb|clickhouse)_[0-9]{12}$' \
                     AND NOT EXISTS (SELECT 1 FROM pg_inherits i WHERE i.inhrelid = c.oid) \
                   UNION ALL SELECT t.root, i.inhrelid FROM tree t JOIN pg_inherits i ON i.inhparent = t.rel) \
                 SELECT r.relname::text, sum(pg_total_relation_size(t.rel))::bigint \
                 FROM tree t JOIN pg_class r ON r.oid = t.root GROUP BY r.relname ORDER BY r.relname",
                &[],
            )
            .await
            .map_err(|e| e.to_string())?
        {
            let name: String = row.get(0);
            let from = name.trim_start_matches("ts_kv_").split('_').next().unwrap_or_default();
            let from = sentient_installer_core::stack::Flavor::parse(from)
                .map(crate::installation::flavor_label)
                .unwrap_or(from)
                .to_string();
            out.push(Kept {
                kind: KeptKind::History,
                detail: format!("History from before the move away from {from}"),
                bytes: row.get(1),
                name,
            });
        }
        let pattern = format!("^{}_(before_update|failed_update|before_restore|rolled_back)_[0-9]{{12}}$", regex_escape(&db));
        for row in pg
            .query(
                "SELECT datname::text, pg_database_size(datname) FROM pg_database WHERE datname ~ $1 ORDER BY datname",
                &[&pattern],
            )
            .await
            .map_err(|e| e.to_string())?
        {
            let name: String = row.get(0);
            let detail = if name.contains("_failed_update_") {
                "The database a failed update left behind"
            } else if name.contains("_rolled_back_") {
                "The database in use before a roll back (what was written after that update)"
            } else if name.contains("_before_restore_") {
                "The database from before a restore was put into service"
            } else {
                "Copy of the database from before an update"
            };
            out.push(Kept { kind: KeptKind::DatabaseCopy, detail: detail.into(), bytes: row.get(1), name });
        }
        Ok::<_, String>(out)
    })??;
    if record.engine == Engine::Docker && sentient_installer_core::distro::docker_ops::clickhouse_left_behind(&record.stack) {
        kept.push(Kept {
            kind: KeptKind::ClickHouse,
            name: "ClickHouse (container)".into(),
            bytes: None,
            detail: "No longer used; holds the history from when this installation used ClickHouse".into(),
        });
    }
    #[cfg(target_os = "linux")]
    if record.engine == Engine::Native && sentient_installer_core::native_linux::clickhouse_left_behind(&record.native_config()) {
        kept.push(Kept {
            kind: KeptKind::ClickHouse,
            name: "ClickHouse (service)".into(),
            bytes: None,
            detail: "No longer used; holds the history from when this installation used ClickHouse".into(),
        });
    }
    #[cfg(windows)]
    if record.engine == Engine::Native
        && record.flavor() != Flavor::Clickhouse
        && sentient_installer_core::clickhouse_wsl::is_installed()
    {
        kept.push(Kept {
            kind: KeptKind::ClickHouse,
            name: "ClickHouse (WSL2)".into(),
            bytes: None,
            detail: "No longer used; holds the history from when this installation used ClickHouse".into(),
        });
    }
    // ClickHouse databases besides SENTIENT's own.
    if record.flavor() == sentient_installer_core::stack::Flavor::Clickhouse {
        if let Ok(setup) = crate::migration::Setup::current(record) {
            let ch = setup.ch();
            let live = ch.database.clone();
            let admin = ch.with_database("default");
            let sizes: std::collections::HashMap<String, i64> = admin
                .text("SELECT database, sum(bytes_on_disk) FROM system.parts WHERE active GROUP BY database FORMAT TSV")
                .map(|t| {
                    t.lines()
                        .filter_map(|l| l.split_once('\t'))
                        .filter_map(|(d, b)| Some((d.to_string(), b.trim().parse().ok()?)))
                        .collect()
                })
                .unwrap_or_default();
            if let Ok(text) = admin.text(&format!(
                "SELECT name FROM system.databases \
                 WHERE name NOT IN ('{live}', 'system', 'default', 'INFORMATION_SCHEMA', 'information_schema') \
                 ORDER BY name FORMAT TSV"
            )) {
                for name in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    kept.push(Kept {
                        kind: KeptKind::ClickhouseDatabase,
                        detail: if name.contains("_before_install_") {
                            "ClickHouse history from an earlier install, set aside when this one started fresh".into()
                        } else {
                            "ClickHouse data: a restore, or the history from before a restore was put into service".into()
                        },
                        bytes: sizes.get(name).copied(),
                        name: name.to_string(),
                    });
                }
            }
        }
    }
    Ok(kept)
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| if c.is_ascii_alphanumeric() || c == '_' { vec![c] } else { vec!['\\', c] })
        .collect()
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Remove one kept item. Cannot be undone.
pub fn remove(record: &InstallationRecord, item: &Kept, sink: ProgressFn) -> Result<(), String> {
    sink(Progress::Step { name: format!("Removing {}", item.name) });
    match item.kind {
        KeptKind::History => {
            let cfg = conn(record, &record.stack.db_name)?;
            let sql = format!("DROP TABLE {}", quote_ident(&item.name));
            block_on(async move {
                let pg = mv::connect(&cfg).await.map_err(|e| e.to_string())?;
                pg.batch_execute(&sql).await.map_err(|e| e.to_string())
            })??;
        }
        KeptKind::ClickhouseDatabase => {
            let ch = crate::migration::Setup::current(record)?.ch();
            ch.with_database("default")
                .text(&format!("DROP DATABASE {} SYNC", item.name))
                .map_err(|e| e.to_string())?;
        }
        KeptKind::DatabaseCopy => {
            let cfg = conn(record, "postgres")?;
            let sql = format!("DROP DATABASE {} WITH (FORCE)", quote_ident(&item.name));
            block_on(async move {
                let pg = mv::connect(&cfg).await.map_err(|e| e.to_string())?;
                pg.batch_execute(&sql).await.map_err(|e| e.to_string())
            })??;
        }
        KeptKind::ClickHouse if record.engine == Engine::Docker => {
            sentient_installer_core::distro::docker_ops::remove_clickhouse(&sink, &record.stack)?
        }
        #[cfg(target_os = "linux")]
        KeptKind::ClickHouse => sentient_installer_core::native_linux::remove_clickhouse(&sink, &record.native_config())?,
        #[cfg(not(target_os = "linux"))]
        KeptKind::ClickHouse => remove_clickhouse(&sink)?,
    }
    sink(Progress::Done { message: format!("Removed {}.", item.name) });
    Ok(())
}

/// ClickHouse in WSL2 under its hidden account: the account deletes its
/// distro (and with it the data), then the account and its boot task go.
pub fn remove_clickhouse(sink: &ProgressFn) -> Result<(), String> {
    #[cfg(windows)]
    {
        use sentient_installer_core::clickhouse_wsl;
        if let Some(password) = crate::secrets::get(crate::secrets::Secret::ClickhouseAccount) {
            let _ = clickhouse_wsl::run_as_account(sink, &password, &crate::self_install::manager_exe(), "unregister");
        }
        clickhouse_wsl::remove_account(sink);
        crate::secrets::forget(crate::secrets::Secret::ClickhouseAccount);
        crate::secrets::forget(crate::secrets::Secret::Clickhouse);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = sink;
        Err("ClickHouse in WSL2 exists on Windows only.".into())
    }
}

/// One item as a line, for the root job's output (crate::rootjob).
pub fn encode(k: &Kept) -> String {
    let kind = match k.kind {
        KeptKind::History => "history",
        KeptKind::DatabaseCopy => "database",
        KeptKind::ClickHouse => "clickhouse",
        KeptKind::ClickhouseDatabase => "chdb",
    };
    format!("KEPT\t{kind}\t{}\t{}\t{}", k.name, k.bytes.map(|b| b.to_string()).unwrap_or_default(), k.detail)
}

pub fn decode(line: &str) -> Option<Kept> {
    let mut f = line.strip_prefix("KEPT\t")?.splitn(4, '\t');
    let kind = match f.next()? {
        "history" => KeptKind::History,
        "database" => KeptKind::DatabaseCopy,
        "chdb" => KeptKind::ClickhouseDatabase,
        _ => KeptKind::ClickHouse,
    };
    let name = f.next()?.to_string();
    let bytes = f.next()?.parse().ok();
    let detail = f.next().unwrap_or_default().to_string();
    Some(Kept { kind, name, bytes, detail })
}

/// "1.2 GB"
pub fn human(bytes: Option<i64>) -> String {
    bytes.map(sentient_backup_core::db::human_bytes).unwrap_or_default()
}
