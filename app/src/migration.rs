// Copyright © 2016-2026 The SENTIENT Authors

//! Changing an installation's database type: PostgreSQL, PostgreSQL +
//! TimescaleDB, PostgreSQL + ClickHouse, in any direction.
//!
//! 1. Check the target can be reached (WSL2 for ClickHouse) while SENTIENT
//!    still runs, and install ClickHouse if it is the target.
//! 2. Write a full backup, telemetry included, next to the installation.
//! 3. Stop SENTIENT. Prepare PostgreSQL's `ts_kv` for the target (a
//!    hypertable or a plain table; the old one is kept under another name),
//!    and bring ClickHouse's latest values over when leaving ClickHouse.
//! 4. Start SENTIENT on the new type: from here it writes there.
//! 5. Copy the history from before the switch, month by month, each month
//!    checked. This can be interrupted and continued (the state file says
//!    what is left).
//!
//! Nothing is deleted: the old history stays where it was (a set-aside
//! table, PostgreSQL's ts_kv, or ClickHouse), and the backup restores the
//! previous type. If anything fails before SENTIENT is switched, it is put
//! back as it was and started.

use std::path::PathBuf;

use sentient_backup_core::backup::{self, BackupOptions, Selection};
use sentient_backup_core::clickhouse::ChConn;
use sentient_backup_core::db::ConnConfig;
use sentient_backup_core::progress as bk;
use sentient_backup_core::telemetry_move as mv;
use sentient_installer_core::native::{self, NativeConfig};
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;
use serde::{Deserialize, Serialize};

use crate::installation::{Engine, InstallationRecord, flavor_label};
use crate::secrets::{self, Secret};

/// What is left of a migration whose history copy has not finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationState {
    pub from: Flavor,
    pub to: Flavor,
    /// Epoch ms: history before this moment is copied, SENTIENT wrote the rest.
    pub cutover: i64,
    /// The PostgreSQL table the old history was kept under, if any.
    pub aside: Option<String>,
    pub backup: PathBuf,
    pub done: bool,
}

pub fn state_path(record: &InstallationRecord) -> PathBuf {
    record.state_dir.join("migration.json")
}

pub fn load_state(record: &InstallationRecord) -> Option<MigrationState> {
    serde_json::from_str(&std::fs::read_to_string(state_path(record)).ok()?).ok()
}

fn save_state(record: &InstallationRecord, state: &MigrationState) -> Result<(), String> {
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(state_path(record), json).map_err(|e| format!("write the migration state: {e}"))
}

/// The types this installation can move to, and why not when it cannot.
pub fn targets(record: &InstallationRecord) -> Result<Vec<Flavor>, &'static str> {
    if record.engine == Engine::Docker {
        return Err("Docker installations change database type by installing the new type and restoring a backup into it; in-place migration covers native installations.");
    }
    if !cfg!(windows) {
        return Err("In-place migration of native Linux installations arrives in a later version.");
    }
    Ok([Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse].into_iter().filter(|f| *f != record.flavor()).collect())
}

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}

fn bridge(sink: ProgressFn) -> bk::ProgressFn {
    std::sync::Arc::new(move |p| {
        sink(match p {
            bk::Progress::Step { name, .. } => Progress::Log { line: name },
            bk::Progress::Log { line } => Progress::Log { line },
            bk::Progress::Percent { value } => Progress::Percent { value },
            bk::Progress::Done { message } => Progress::Log { line: message },
        })
    })
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(future))
        .map_err(|e| format!("start the migration runtime: {e}"))
}

fn pg_conn(cfg: &NativeConfig) -> ConnConfig {
    ConnConfig {
        host: "127.0.0.1".into(),
        port: cfg.pg_port,
        dbname: cfg.db_name.clone(),
        user: cfg.db_user.clone(),
        password: cfg.db_password.clone(),
    }
}

fn ch_conn(cfg: &NativeConfig) -> ChConn {
    ChConn {
        url: format!("http://127.0.0.1:{}", cfg.ch_http_port),
        database: cfg.ch_db.clone(),
        user: cfg.ch_user.clone(),
        password: cfg.ch_password.clone(),
    }
}

/// Run a migration of `record` to `target`. Returns the updated record.
pub fn run(record: InstallationRecord, target: Flavor, sink: ProgressFn) -> Result<InstallationRecord, String> {
    if let Err(why) = targets(&record) {
        return Err(why.into());
    }
    let from = record.flavor();
    if from == target {
        return Err(format!("This installation already uses {}.", flavor_label(target)));
    }
    if load_state(&record).is_some_and(|s| !s.done) {
        return Err("A previous migration has history left to copy; continue it first.".into());
    }
    let old = record.native_config_with_secrets()?;
    let mut new = old.clone();
    new.flavor = target;

    // 1. The target's ClickHouse, while SENTIENT still runs.
    if target == Flavor::Clickhouse {
        prepare_clickhouse(&record, &mut new, &sink)?;
    }

    // 2. A full backup to return to.
    step(&sink, "Backing up everything first");
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dir = record.state_dir.join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let backup_file = dir.join(format!("before-{}-{stamp}.sentient-backup", target.as_str()));
    let opts = BackupOptions {
        output: backup_file.clone(),
        selection: Selection::skipping(&[]),
        file_stores: Vec::new(),
        zstd_level: 6,
        passphrase: None,
        clickhouse: (from == Flavor::Clickhouse).then(|| ch_conn(&old)),
    };
    block_on(backup::run(&pg_conn(&old), &opts, bridge(sink.clone())))?.map_err(|e| format!("backup: {e}"))?;
    log(&sink, format!("Backup: {}", backup_file.display()));

    // 3. Stop, prepare the stores.
    if let Err(e) = native::stop_server(&sink, &old) {
        let _ = native::control(sink.clone(), &old, "start");
        return Err(e);
    }
    let cutover = chrono::Utc::now().timestamp_millis();
    let aside_name = format!("ts_kv_{}_{}", from.as_str(), chrono::Utc::now().format("%Y%m%d%H%M"));
    let prepared = block_on(prepare_stores(&old, &new, from, target, &aside_name, &sink))?;
    let aside = match prepared {
        Ok(aside) => aside,
        Err(e) => {
            log(&sink, format!("Preparing failed ({e}); starting SENTIENT as it was"));
            let _ = native::control(sink.clone(), &old, "start");
            // (start covers PostgreSQL too; it is already running)
            return Err(e);
        }
    };

    // 4. Switch.
    step(&sink, &format!("Starting SENTIENT on {}", flavor_label(target)));
    let mut state = MigrationState { from, to: target, cutover, aside: aside.clone(), backup: backup_file, done: false };
    if let Err(e) = native::deploy(sink.clone(), &new) {
        log(&sink, format!("SENTIENT did not start on the new type ({e}); putting it back"));
        let undo = block_on(undo_stores(&old, aside.as_deref()))?;
        if let Err(u) = undo {
            log(&sink, format!("could not put ts_kv back: {u}"));
        }
        native::deploy(sink.clone(), &old)?;
        return Err(format!("SENTIENT did not start on {}: {e}. It runs on {} again.", flavor_label(target), flavor_label(from)));
    }
    let mut record = record;
    record.stack.flavor = target;
    write_record(&record)?;
    save_state(&record, &state)?;
    if target == Flavor::Clickhouse {
        secrets::save(Secret::Clickhouse, &new.ch_password)?;
    }

    // 5. History.
    copy_history(&record, &new, &mut state, &sink)?;
    sink(Progress::Done {
        message: format!("SENTIENT runs on {} with its full history.", flavor_label(target)),
    });
    Ok(record)
}

/// Continue the history copy of an interrupted migration.
pub fn resume(record: InstallationRecord, sink: ProgressFn) -> Result<(), String> {
    let mut state = load_state(&record).filter(|s| !s.done).ok_or("No migration is waiting to continue.")?;
    let cfg = record.native_config_with_secrets()?;
    copy_history(&record, &cfg, &mut state, &sink)?;
    sink(Progress::Done { message: "The history copy is complete.".into() });
    Ok(())
}

fn write_record(record: &InstallationRecord) -> Result<(), String> {
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)
}

/// ClickHouse for a migration to it: installed (Windows: in WSL2 under the
/// hidden account) with a new password, which `new` then carries.
fn prepare_clickhouse(record: &InstallationRecord, new: &mut NativeConfig, sink: &ProgressFn) -> Result<(), String> {
    #[cfg(windows)]
    {
        use sentient_installer_core::{clickhouse_wsl, distro, wsl};
        step(sink, "Preparing ClickHouse");
        if !wsl::is_ready() {
            let outcome = wsl::install(sink.clone());
            if outcome.reboot_required {
                return Err("WSL2 is now enabled but needs Windows to restart. Restart, then start the migration again; nothing has been changed yet.".into());
            }
        }
        new.ch_password = secrets::get(Secret::Clickhouse).unwrap_or_else(clickhouse_wsl::new_password);
        if distro::clickhouse_answers(new.ch_http_port) {
            log(sink, "ClickHouse is already running");
        } else {
            let mut stack = record.stack.clone();
            stack.flavor = Flavor::Clickhouse;
            stack.ch_password = new.ch_password.clone();
            // The record keeps no passwords; deploy validates them all.
            stack.db_password = new.db_password.clone();
            crate::installer::provision_clickhouse_wsl(&stack, &crate::self_install::manager_exe(), sink)?;
        }
        // Saved before anything depends on it, so a failure later can be retried.
        secrets::save(Secret::Clickhouse, &new.ch_password)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (record, new, sink);
        Err("ClickHouse migration is Windows-only in this version.".into())
    }
}

/// With SENTIENT stopped: shape PostgreSQL's ts_kv for the target and, when
/// leaving ClickHouse, bring its latest values over. Returns the set-aside
/// table, if the old history was kept under another name.
async fn prepare_stores(
    old: &NativeConfig,
    new: &NativeConfig,
    from: Flavor,
    target: Flavor,
    aside_name: &str,
    sink: &ProgressFn,
) -> Result<Option<String>, String> {
    step(sink, "Preparing the database");
    let msink = bridge(sink.clone());
    let pg = mv::connect(&pg_conn(old)).await.map_err(|e| e.to_string())?;
    let mut aside = None;
    // Decided by ts_kv as it is, not by the previous type: an installation
    // that went TimescaleDB → ClickHouse still has its hypertable.
    let layout = mv::layout(&pg, "ts_kv").await.map_err(|e| e.to_string())?;
    match target {
        Flavor::Timescaledb if layout != mv::Layout::Hypertable => {
            let sql_path = timescale_sql(new);
            let sql = std::fs::read_to_string(&sql_path).map_err(|e| format!("read {}: {e}", sql_path.display()))?;
            if mv::set_aside(&pg, aside_name, &msink).await.map_err(|e| e.to_string())? {
                aside = Some(aside_name.to_string());
            }
            mv::create_hypertable(&pg, &sql).await.map_err(|e| e.to_string())?;
        }
        // SENTIENT on plain PostgreSQL writes into monthly partitions.
        Flavor::Stock if layout != mv::Layout::Partitioned => {
            if mv::set_aside(&pg, aside_name, &msink).await.map_err(|e| e.to_string())? {
                aside = Some(aside_name.to_string());
            }
            mv::create_partitioned(&pg).await.map_err(|e| e.to_string())?;
        }
        // Already the right shape; ClickHouse needs nothing from it.
        _ => {}
    }
    if from == Flavor::Clickhouse {
        mv::latest_ch_to_pg(&pg, &ch_conn(old), &msink).await.map_err(|e| e.to_string())?;
    }
    Ok(aside)
}

/// Put ts_kv back as it was after a failed switch.
async fn undo_stores(old: &NativeConfig, aside: Option<&str>) -> Result<(), String> {
    let Some(aside) = aside else { return Ok(()) };
    let pg = mv::connect(&pg_conn(old)).await.map_err(|e| e.to_string())?;
    pg.batch_execute(&format!(
        "DROP TABLE IF EXISTS ts_kv; ALTER TABLE {aside} RENAME TO ts_kv; ALTER INDEX IF EXISTS {aside}_pkey RENAME TO ts_kv_pkey;"
    ))
    .await
    .map_err(|e| e.to_string())
}

fn timescale_sql(cfg: &NativeConfig) -> PathBuf {
    cfg.install_dir.join("data").join("sql").join("schema-ts-timescale.sql")
}

/// Copy the history from before the switch into the store SENTIENT now uses.
fn copy_history(record: &InstallationRecord, cfg: &NativeConfig, state: &mut MigrationState, sink: &ProgressFn) -> Result<(), String> {
    step(sink, "Copying the history from before the switch");
    let msink = bridge(sink.clone());
    let result = block_on(async {
        let pg = mv::connect(&pg_conn(cfg)).await.map_err(|e| e.to_string())?;
        let mut rows = 0u64;
        match (state.from, state.to) {
            (_, Flavor::Clickhouse) => {
                let ch = ch_conn(cfg);
                rows += mv::history_pg_to_ch(&pg, "ts_kv", &ch, state.cutover, &msink).await.map_err(|e| e.to_string())?.rows;
                mv::latest_pg_to_ch(&pg, &ch, &msink).await.map_err(|e| e.to_string())?;
            }
            (Flavor::Clickhouse, _) => {
                let ch = ChConn { password: clickhouse_password()?, ..ch_conn(cfg) };
                rows += mv::history_ch_to_pg(&pg, &ch, state.cutover, &msink).await.map_err(|e| e.to_string())?.rows;
            }
            _ => {}
        }
        if let Some(aside) = &state.aside {
            rows += mv::history_pg_to_pg(&pg, aside, state.cutover, &msink).await.map_err(|e| e.to_string())?.rows;
        }
        Ok::<_, String>(rows)
    })??;
    log(sink, format!("{result} rows copied"));
    state.done = true;
    save_state(record, state)?;
    if let Some(aside) = &state.aside {
        log(sink, format!("The old history is still in the table {aside}; remove it once you no longer need it."));
    }
    Ok(())
}

/// When leaving ClickHouse the record no longer says ClickHouse, so its
/// password comes straight from the secret store.
fn clickhouse_password() -> Result<String, String> {
    secrets::get(Secret::Clickhouse).ok_or_else(|| "The ClickHouse password is not in the secret store.".to_string())
}
