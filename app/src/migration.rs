// Copyright © 2016-2026 The SENTIENT Authors

//! Changing an installation's database type: PostgreSQL, PostgreSQL +
//! TimescaleDB, PostgreSQL + ClickHouse, in any direction.
//!
//! 1. Check the target can be reached (WSL2 for ClickHouse) while SENTIENT
//!    still runs, and install ClickHouse if it is the target.
//! 2. Write a full backup, telemetry included, next to the installation.
//! 3. Stop SENTIENT. Prepare PostgreSQL's `ts_kv` for the target (a
//!    hypertable or a partitioned table; the old one is kept under another
//!    name), and bring ClickHouse's latest values over when leaving it.
//! 4. Start SENTIENT on the new type: from here it writes there.
//! 5. Copy the history from before the switch, month by month, each month
//!    checked. This can be interrupted and continued (the state file says
//!    what is left).
//!
//! Nothing is deleted: the old history stays where it was (a set-aside
//! table, PostgreSQL's ts_kv, or ClickHouse), and the backup restores the
//! previous type. If anything fails before SENTIENT is switched, it is put
//! back as it was and started.
//!
//! Native Windows installations and Docker installations on Linux; the steps
//! that differ (stop, start, PostgreSQL's image) are in [`Setup`].

use std::path::PathBuf;

use sentient_backup_core::backup::{self, BackupOptions, Selection};
use sentient_backup_core::clickhouse::ChConn;
use sentient_backup_core::db::ConnConfig;
use sentient_backup_core::progress as bk;
use sentient_backup_core::restore::TelemetryInto;
use sentient_backup_core::telemetry_move as mv;
use sentient_installer_core::distro::{self, DeployConfig, docker_ops};
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
    /// Leaving ClickHouse on Docker or Linux: its password, which the new
    /// configuration no longer holds (Windows keeps it in the secret store).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub old_ch_password: String,
}

/// Where a migration keeps its state and its backup: the installation's state
/// folder, or a Docker stack's own folder (the operator can write there).
pub fn work_dir(record: &InstallationRecord) -> PathBuf {
    match record.engine {
        Engine::Native => record.state_dir.clone(),
        Engine::Docker => PathBuf::from(record.stack.stack_dir()),
    }
}

pub fn state_path(record: &InstallationRecord) -> PathBuf {
    work_dir(record).join("migration.json")
}

pub fn load_state(record: &InstallationRecord) -> Option<MigrationState> {
    serde_json::from_str(&std::fs::read_to_string(state_path(record)).ok()?).ok()
}

fn save_state(record: &InstallationRecord, state: &MigrationState) -> Result<(), String> {
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    let path = state_path(record);
    std::fs::write(&path, json).map_err(|e| format!("write the migration state: {e}"))?;
    #[cfg(unix)]
    if !state.old_ch_password.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// The types this installation can move to, and why not when it cannot.
pub fn targets(record: &InstallationRecord) -> Result<Vec<Flavor>, &'static str> {
    let all = [Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse];
    match record.engine {
        Engine::Native if cfg!(windows) => Ok(all.into_iter().filter(|f| *f != record.flavor()).collect()),
        // TimescaleDB is not packaged by Linux distributions.
        Engine::Native => Ok([Flavor::Stock, Flavor::Clickhouse].into_iter().filter(|f| *f != record.flavor()).collect()),
        Engine::Docker if cfg!(windows) => {
            Err("SENTIENT runs natively on Windows; in-place migration covers native Windows installations and Docker on Linux.")
        }
        Engine::Docker => Ok(all.into_iter().filter(|f| *f != record.flavor()).collect()),
    }
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

// ---------------------------------------------------------------------------
// The installation, whichever engine runs it
// ---------------------------------------------------------------------------

/// One configuration of the installation, passwords included.
#[derive(Clone)]
pub(crate) enum Setup {
    Native(Box<NativeConfig>),
    Docker(DeployConfig),
}

impl Setup {
    /// The installation as it is, with its passwords.
    pub(crate) fn current(record: &InstallationRecord) -> Result<Setup, String> {
        match record.engine {
            Engine::Native => Ok(Setup::Native(Box::new(record.native_config_with_secrets()?))),
            Engine::Docker if record.stack.pg_port == 0 => Err(crate::installation::PG_NOT_PUBLISHED.into()),
            Engine::Docker => Ok(Setup::Docker(docker_ops::with_secrets(&record.stack)?)),
        }
    }

    pub(crate) fn flavor(&self) -> Flavor {
        match self {
            Setup::Native(c) => c.flavor,
            Setup::Docker(c) => c.flavor,
        }
    }

    fn with_flavor(&self, flavor: Flavor) -> Setup {
        match self {
            Setup::Native(c) => Setup::Native(Box::new(NativeConfig { flavor, ..(**c).clone() })),
            Setup::Docker(c) => {
                let mut d = DeployConfig { flavor, ..c.clone() };
                // Once a database has had TimescaleDB, PostgreSQL stays on the
                // image that can read it (the old hypertable is kept).
                if c.flavor == Flavor::Timescaledb {
                    d.keep_timescale_image = true;
                }
                Setup::Docker(d)
            }
        }
    }

    pub(crate) fn pg(&self) -> ConnConfig {
        let (port, db, user, password) = match self {
            Setup::Native(c) => (c.pg_port, &c.db_name, &c.db_user, &c.db_password),
            Setup::Docker(c) => (c.pg_port, &c.db_name, &c.db_user, &c.db_password),
        };
        ConnConfig { host: "127.0.0.1".into(), port, dbname: db.clone(), user: user.clone(), password: password.clone() }
    }

    pub(crate) fn ch(&self) -> ChConn {
        let (port, db, user, password) = match self {
            Setup::Native(c) => (c.ch_http_port, &c.ch_db, &c.ch_user, &c.ch_password),
            Setup::Docker(c) => (c.ch_http_port, &c.ch_db, &c.ch_user, &c.ch_password),
        };
        ChConn { url: format!("http://127.0.0.1:{port}"), database: db.clone(), user: user.clone(), password: password.clone() }
    }

    fn set_ch_password(&mut self, password: String) {
        match self {
            Setup::Native(c) => c.ch_password = password,
            Setup::Docker(c) => c.ch_password = password,
        }
    }

    /// Stop SENTIENT alone; PostgreSQL keeps running.
    pub(crate) fn stop(&self, sink: &ProgressFn) -> Result<(), String> {
        match self {
            Setup::Native(c) => native::stop_server(sink, c),
            Setup::Docker(c) => docker_ops::stop_app(sink, c),
        }
    }

    /// Start SENTIENT as it was configured, after a failed step.
    pub(crate) fn restart_as_was(&self, sink: &ProgressFn) {
        let _ = match self {
            Setup::Native(c) => native::control(sink.clone(), c, "start"),
            Setup::Docker(c) => distro::control(sink.clone(), c, "start"),
        };
    }

    /// Write this configuration and start SENTIENT on it.
    fn deploy(&self, sink: &ProgressFn) -> Result<(), String> {
        match self {
            Setup::Native(c) => native::deploy(sink.clone(), c),
            Setup::Docker(c) => distro::deploy(sink.clone(), c),
        }
    }

    /// Before ts_kv is prepared: PostgreSQL on the image this configuration
    /// needs (Docker: the TimescaleDB image for TimescaleDB).
    fn prepare_postgres(&self, old: &Setup, sink: &ProgressFn) -> Result<(), String> {
        if let (Setup::Docker(new), Setup::Docker(old)) = (self, old) {
            let wants = |c: &DeployConfig| c.flavor == Flavor::Timescaledb || c.keep_timescale_image;
            if wants(new) != wants(old) {
                docker_ops::recreate_postgres(sink, new)?;
            }
        }
        Ok(())
    }

    /// SENTIENT's own TimescaleDB setup (data/sql/schema-ts-timescale.sql).
    fn timescale_sql(&self) -> Result<String, String> {
        match self {
            Setup::Native(c) => native_timescale_sql(c),
            Setup::Docker(c) => docker_timescale_sql(c),
        }
    }
}

fn native_timescale_sql(c: &NativeConfig) -> Result<String, String> {
    let path = c.install_dir.join("data").join("sql").join("schema-ts-timescale.sql");
    std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

fn docker_timescale_sql(c: &DeployConfig) -> Result<String, String> {
    docker_ops::image_file(c, "/usr/share/sentient/data/sql/schema-ts-timescale.sql")
}

/// How this installation keeps telemetry, for a restore of a backup made on
/// another database type (the restore converts it).
pub(crate) fn telemetry_into(record: &InstallationRecord) -> Result<TelemetryInto, String> {
    Ok(match record.flavor() {
        Flavor::Clickhouse => TelemetryInto::Clickhouse,
        Flavor::Stock => TelemetryInto::Partitioned,
        Flavor::Timescaledb => TelemetryInto::Hypertable {
            timescale_sql: match record.engine {
                Engine::Native => native_timescale_sql(&record.native_config())?,
                Engine::Docker => docker_timescale_sql(&record.stack)?,
            },
        },
    })
}

fn write_record(record: &InstallationRecord) -> Result<(), String> {
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)
}

// ---------------------------------------------------------------------------
// The migration
// ---------------------------------------------------------------------------

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
    let old = Setup::current(&record)?;
    let mut new = old.with_flavor(target);

    // 1. The target's ClickHouse, while SENTIENT still runs.
    if target == Flavor::Clickhouse {
        prepare_clickhouse(&record, &mut new, &sink)?;
    }

    // 2. A full backup to return to.
    step(&sink, "Backing up everything first");
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dir = work_dir(&record).join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let backup_file = dir.join(format!("before-{}-{stamp}.sentient-backup", target.as_str()));
    let opts = BackupOptions {
        output: backup_file.clone(),
        selection: Selection::skipping(&[]),
        file_stores: Vec::new(),
        zstd_level: 6,
        passphrase: None,
        clickhouse: (from == Flavor::Clickhouse).then(|| old.ch()),
    };
    block_on(backup::run(&old.pg(), &opts, bridge(sink.clone())))?.map_err(|e| format!("backup: {e}"))?;
    log(&sink, format!("Backup: {}", backup_file.display()));

    // 3. Stop, prepare the stores.
    if let Err(e) = old.stop(&sink) {
        old.restart_as_was(&sink);
        return Err(e);
    }
    let cutover = chrono::Utc::now().timestamp_millis();
    let aside_name = format!("ts_kv_{}_{}", from.as_str(), chrono::Utc::now().format("%Y%m%d%H%M"));
    let prepared = new
        .prepare_postgres(&old, &sink)
        .and_then(|_| block_on(prepare_stores(&old, &new, from, target, &aside_name, &sink))?);
    let aside = match prepared {
        Ok(aside) => aside,
        Err(e) => {
            log(&sink, format!("Preparing failed ({e}); starting SENTIENT as it was"));
            if let Setup::Docker(c) = &old {
                // PostgreSQL may be on the new image already.
                let _ = docker_ops::recreate_postgres(&sink, c);
            }
            old.restart_as_was(&sink);
            return Err(e);
        }
    };

    // 4. Switch.
    step(&sink, &format!("Starting SENTIENT on {}", flavor_label(target)));
    let mut state = MigrationState {
        from,
        to: target,
        cutover,
        aside: aside.clone(),
        backup: backup_file,
        done: false,
        // Windows keeps it in the secret store; elsewhere the configuration
        // that held it is rewritten by the switch.
        old_ch_password: match (&old, from) {
            (Setup::Docker(c), Flavor::Clickhouse) => c.ch_password.clone(),
            (Setup::Native(c), Flavor::Clickhouse) if !cfg!(windows) => c.ch_password.clone(),
            _ => String::new(),
        },
    };
    if let Err(e) = new.deploy(&sink) {
        log(&sink, format!("SENTIENT did not start on the new type ({e}); putting it back"));
        let _ = old.stop(&sink);
        let undo = block_on(undo_stores(&old, aside.as_deref()))?;
        if let Err(u) = undo {
            log(&sink, format!("could not put ts_kv back: {u}"));
        }
        old.deploy(&sink)?;
        return Err(format!("SENTIENT did not start on {}: {e}. It runs on {} again.", flavor_label(target), flavor_label(from)));
    }
    let mut record = record;
    record.stack.flavor = target;
    if let Setup::Docker(c) = &new {
        record.stack.keep_timescale_image = c.keep_timescale_image;
    }
    write_record(&record)?;
    save_state(&record, &state)?;
    if target == Flavor::Clickhouse && record.engine == Engine::Native {
        secrets::save(Secret::Clickhouse, &new.ch().password)?;
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
    let setup = Setup::current(&record)?;
    copy_history(&record, &setup, &mut state, &sink)?;
    sink(Progress::Done { message: "The history copy is complete.".into() });
    Ok(())
}

/// ClickHouse for a migration to it, with a password `new` then carries.
/// Windows: installed in WSL2 under the hidden account. Docker: a service of
/// the stack, created when the new configuration is deployed.
fn prepare_clickhouse(record: &InstallationRecord, new: &mut Setup, sink: &ProgressFn) -> Result<(), String> {
    match new {
        Setup::Docker(c) => {
            if c.ch_password.is_empty() {
                c.ch_password = native::generate_secret().chars().take(32).collect();
            }
            let _ = (record, sink);
            Ok(())
        }
        Setup::Native(c) => prepare_clickhouse_wsl(record, c, sink),
    }
}

#[cfg(windows)]
fn prepare_clickhouse_wsl(record: &InstallationRecord, new: &mut NativeConfig, sink: &ProgressFn) -> Result<(), String> {
    use sentient_installer_core::{clickhouse_wsl, wsl};
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
        crate::installer::provision_clickhouse_wsl(&stack, &crate::self_install::manager_exe(), false, sink)?;
    }
    // Saved before anything depends on it, so a failure later can be retried.
    secrets::save(Secret::Clickhouse, &new.ch_password)?;
    Ok(())
}

/// Linux: ClickHouse as a native service, installed by the same setup step
/// as a fresh install (the vendor's pinned static build); deploy then creates
/// its user and database.
#[cfg(not(windows))]
fn prepare_clickhouse_wsl(_: &InstallationRecord, new: &mut NativeConfig, sink: &ProgressFn) -> Result<(), String> {
    step(sink, "Preparing ClickHouse");
    if new.ch_password.is_empty() {
        new.ch_password = native::generate_secret().chars().take(32).collect();
    }
    if distro::clickhouse_answers(new.ch_http_port) {
        log(sink, "ClickHouse is already running");
        return Ok(());
    }
    native::setup(sink.clone(), new)
}

/// With SENTIENT stopped: shape PostgreSQL's ts_kv for the target and, when
/// leaving ClickHouse, bring its latest values over. Returns the set-aside
/// table, if the old history was kept under another name.
async fn prepare_stores(
    old: &Setup,
    new: &Setup,
    from: Flavor,
    target: Flavor,
    aside_name: &str,
    sink: &ProgressFn,
) -> Result<Option<String>, String> {
    step(sink, "Preparing the database");
    let msink = bridge(sink.clone());
    let pg = mv::connect(&old.pg()).await.map_err(|e| e.to_string())?;
    let mut aside = None;
    // Decided by ts_kv as it is, not by the previous type: an installation
    // that went TimescaleDB → ClickHouse still has its hypertable.
    let layout = mv::layout(&pg, "ts_kv").await.map_err(|e| e.to_string())?;
    match target {
        Flavor::Timescaledb if layout != mv::Layout::Hypertable => {
            let sql = new.timescale_sql()?;
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
        mv::latest_ch_to_pg(&pg, &old.ch(), &msink).await.map_err(|e| e.to_string())?;
    }
    Ok(aside)
}

/// Put ts_kv back as it was after a failed switch.
async fn undo_stores(old: &Setup, aside: Option<&str>) -> Result<(), String> {
    let Some(aside) = aside else { return Ok(()) };
    let pg = mv::connect(&old.pg()).await.map_err(|e| e.to_string())?;
    pg.batch_execute(&format!(
        "DROP TABLE IF EXISTS ts_kv; ALTER TABLE {aside} RENAME TO ts_kv; ALTER INDEX IF EXISTS {aside}_pkey RENAME TO ts_kv_pkey;"
    ))
    .await
    .map_err(|e| e.to_string())
}

/// Copy the history from before the switch into the store SENTIENT now uses.
fn copy_history(record: &InstallationRecord, setup: &Setup, state: &mut MigrationState, sink: &ProgressFn) -> Result<(), String> {
    step(sink, "Copying the history from before the switch");
    let msink = bridge(sink.clone());
    let old_ch = (state.from == Flavor::Clickhouse).then(|| clickhouse_password(record, state)).transpose()?;
    let result = block_on(async {
        let pg = mv::connect(&setup.pg()).await.map_err(|e| e.to_string())?;
        let mut rows = 0u64;
        match (state.from, state.to) {
            (_, Flavor::Clickhouse) => {
                let ch = setup.ch();
                rows += mv::history_pg_to_ch(&pg, "ts_kv", &ch, state.cutover, &msink).await.map_err(|e| e.to_string())?.rows;
                mv::latest_pg_to_ch(&pg, &ch, &msink).await.map_err(|e| e.to_string())?;
            }
            (Flavor::Clickhouse, _) => {
                let ch = ChConn { password: old_ch.clone().unwrap_or_default(), ..setup.ch() };
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
    state.old_ch_password.clear();
    save_state(record, state)?;
    if let Some(aside) = &state.aside {
        log(sink, format!("The old history is still in the table {aside}; remove it once you no longer need it."));
    }
    Ok(())
}

/// When leaving ClickHouse the configuration no longer holds its password:
/// Windows keeps it in the secret store, Docker in the migration state.
fn clickhouse_password(record: &InstallationRecord, state: &MigrationState) -> Result<String, String> {
    if !state.old_ch_password.is_empty() {
        return Ok(state.old_ch_password.clone());
    }
    let _ = record;
    secrets::get(Secret::Clickhouse).ok_or_else(|| "The ClickHouse password is not in the secret store.".to_string())
}
