// Copyright © 2016-2026 The SENTIENT Authors

//! Every long job's steps, declared before it starts (`Progress::Plan`):
//! the job view (crate::manager::jobview) shows them all from its first
//! frame. One place for the window (`Job::set_plan`) and for the agent on
//! a server or a root job (`--job`, crate::headless), so both list the same
//! steps. Each step names the engine's own step that starts it (`when`);
//! the engine's steps inside it show under it while it runs.
//!
//! tests/layout/jobview.sh checks that every long job declares its steps.

use sentient_backup_core::restore_scope::Scope;
use sentient_installer_core::distro::move_data::{Move, Overview, Store};
use sentient_installer_core::progress::PlanStep;

use crate::backups::{self, BackupPrefs, History, Preset};
use crate::installation::{Engine, InstallationRecord};
use crate::restore_flow as rf;

/// What a plan depends on about the installation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub docker: bool,
    pub windows: bool,
}

impl Shape {
    pub fn of(record: &InstallationRecord) -> Self {
        Self { docker: record.engine == Engine::Docker, windows: cfg!(windows) }
    }
}

/// The first step of a job on a server: reaching it and its agent.
pub const CONNECT: &str = "Connect to the server";

pub fn connect() -> PlanStep {
    PlanStep::new(CONNECT).detail("SSH, and the Platform Manager there brought up to date").weight(0.5)
}

/// A backup with the saved settings (`backups::run`).
pub fn backup(prefs: &BackupPrefs, automatic: bool, encrypted: bool) -> Vec<PlanStep> {
    let mut v = vec![PlanStep::new("Connect to the database").detail("Reads what there is to back up").when(&[backups::STEP_WRITE, "Connecting and inspecting"]).weight(0.5)];
    v.push(PlanStep::new("Dump the database").detail(if prefs.preset == Preset::Configuration { "Configuration only" } else { "Devices, dashboards, users, alarms and settings" }).when(&["Dumping database"]).weight(4.));
    if prefs.preset == Preset::Everything && prefs.history != History::None {
        let what = match prefs.history {
            History::LastDays(d) => format!("The last {d} days"),
            _ => "All of it".into(),
        };
        v.push(PlanStep::new("Copy the telemetry history").detail(what).when(&["Exporting telemetry", "Exporting ClickHouse telemetry"]).weight(4.));
    }
    if prefs.preset == Preset::Everything {
        v.push(PlanStep::new("Archive the file stores").detail("Reports and version control").when(&["Archiving file store"]));
    }
    let mut how = vec!["Compressed".to_string()];
    if prefs.part_gb > 0 {
        how.push(format!("split over {} GB", prefs.part_gb));
    }
    if encrypted {
        how.push("encrypted".into());
    }
    v.push(PlanStep::new("Write the backup file").detail(how.join(", ")).when(&["Writing manifest and archive"]).weight(2.));
    v.push(PlanStep::new("Read the file back to check it").detail("SHA-256 of every part").when(&[backups::STEP_CHECK]).weight(2.));
    if let Some(to) = &prefs.copy_to {
        v.push(PlanStep::new("Copy it to the second location").detail(to.display().to_string()).when(&[backups::STEP_COPY]));
    }
    if automatic {
        v.push(PlanStep::new("Remove older automatic backups").detail(format!("Keeps the newest {}", prefs.keep.max(1))).when(&[backups::STEP_PRUNE]).weight(0.5));
    }
    v
}

/// A backup into one file (`backup <file>`, the engine's own steps).
pub fn backup_file() -> Vec<PlanStep> {
    vec![
        PlanStep::new("Connect to the database").when(&["Connecting and inspecting"]).weight(0.5),
        PlanStep::new("Dump the database").when(&["Dumping database"]).weight(4.),
        PlanStep::new("Copy the telemetry history").when(&["Exporting telemetry", "Exporting ClickHouse telemetry"]).weight(4.),
        PlanStep::new("Archive the file stores").when(&["Archiving file store"]),
        PlanStep::new("Write the backup file").when(&["Writing manifest and archive"]).weight(2.),
    ]
}

/// A restore (crate::restore_flow::run), as its request asks.
pub fn restore(req: &rf::Request) -> Vec<PlanStep> {
    let file = req.file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    rf::planned_steps(req)
        .into_iter()
        .map(|name| {
            let (detail, weight) = match name.as_str() {
                rf::STEP_SAFETY => ("Everything, before anything changes".to_string(), 3.),
                rf::STEP_CHECK => (file.clone(), 1.),
                rf::STEP_RESTORE => (if req.scope == Scope::Configuration { "Configuration only".into() } else { "PostgreSQL, telemetry and file stores".into() }, 5.),
                rf::STEP_INTO_LIVE => (req.scope.label(), 5.),
                rf::STEP_KEEP => ("This server's telemetry stays".into(), 2.),
                rf::STEP_SCHEMA => (String::new(), 1.),
                rf::STEP_SWITCH => ("SENTIENT restarts on it; the previous data comes back if it does not answer".into(), 2.),
                rf::STEP_SETTINGS => (format!("{} setting{}", req.use_from_backup.len(), if req.use_from_backup.len() == 1 { "" } else { "s" }), 1.),
                _ => (String::new(), 1.),
            };
            PlanStep::new(name).detail(detail).weight(weight)
        })
        .collect()
}

/// An update of SENTIENT (crate::updating::run_to); `download`: the
/// release downloaded first (native).
pub fn update(shape: Shape, download: Option<&str>, to: Option<&str>) -> Vec<PlanStep> {
    let mut v = Vec::new();
    if shape.docker {
        v.push(PlanStep::new("Getting the new SENTIENT image").detail(to.map(|t| format!("SENTIENT {t}")).unwrap_or_default()).weight(3.));
        v.push(PlanStep::new("Stopping SENTIENT"));
    } else {
        if let Some(d) = download {
            v.push(PlanStep::new(format!("Downloading SENTIENT {d}")).detail("Checked against its SHA-256 and signature").when(&["Downloading SENTIENT"]).weight(3.));
        }
        v.push(PlanStep::new("Unpacking the new SENTIENT").when(&["Unpacking"]));
        v.push(PlanStep::new("Stopping SENTIENT").when(&["Stopping"]));
    }
    v.push(PlanStep::new("Checking the new version's database changes (dry run)").detail("Nothing changes if they would fail").weight(2.));
    v.push(PlanStep::new("Copying the database before the update").detail("Kept until the next update, to go back to").when(&["Copying the database"]).weight(3.));
    if !shape.docker {
        v.push(PlanStep::new("Replacing the program files"));
    }
    v.push(PlanStep::new("Applying the database changes").weight(2.));
    v.push(PlanStep::new("Starting SENTIENT").detail("And checking it stays up").when(&["Starting SENTIENT", "Checking SENTIENT stays up", "Waiting for SENTIENT"]).weight(2.));
    v
}

/// Going back to the version the last update replaced.
pub fn rollback(shape: Shape) -> Vec<PlanStep> {
    vec![
        PlanStep::new("Stopping SENTIENT").when(&["Stopping"]),
        PlanStep::new(if shape.docker { "Putting the previous image back" } else { "Putting the previous program files back" }),
        PlanStep::new("Putting the database from before the update back").detail("The one in use now is kept aside").when(&["Putting the database from before the update back"]).weight(3.),
        PlanStep::new("Starting SENTIENT").when(&["Starting SENTIENT", "Checking SENTIENT stays up", "Waiting for SENTIENT"]).weight(2.),
    ]
}

/// The repair (crate::maintenance_ops::run_repair).
pub const STEP_REPAIR: &str = "Repairing SENTIENT";

pub fn repair(shape: Shape, clickhouse_wsl: bool) -> Vec<PlanStep> {
    let mut v = Vec::new();
    if shape.windows && !shape.docker && clickhouse_wsl {
        v.push(PlanStep::new("Making ClickHouse safe against unclean stops").when(&["Stopping ClickHouse cleanly"]));
    }
    v.push(PlanStep::new(STEP_REPAIR).detail(if shape.docker { "Its compose file written again, the containers started" } else { "Its service and configuration written again, then started" }).weight(4.));
    v.push(PlanStep::new("Restoring the Platform Manager").detail("Its program, menu entry and operator screen"));
    v
}

/// The uninstall (crate::maintenance_ops::run_uninstall).
pub const STEP_UNINSTALL: &str = "Removing SENTIENT";

pub fn uninstall(keep_data: bool) -> Vec<PlanStep> {
    vec![
        PlanStep::new(STEP_UNINSTALL).detail(if keep_data { "The database is kept for a new install" } else { "With its database and telemetry" }).weight(4.),
        PlanStep::new("Removing SENTIENT's shortcuts").detail("Menu entries, the operator screen and sign-in"),
    ]
}

/// The change of database type (crate::migration::run).
pub fn migration() -> Vec<PlanStep> {
    vec![
        PlanStep::new("A full backup, history included").when(&["Backing up"]).weight(3.),
        PlanStep::new("SENTIENT stops").when(&["Stopping", "Preparing ClickHouse", "Preparing the database"]),
        PlanStep::new("SENTIENT starts on the new type").when(&["Starting SENTIENT on"]).weight(2.),
        PlanStep::new("The history is copied month by month").when(&["Copying the history"]).weight(4.),
    ]
}

/// Taking over a compose file someone else wrote (crate::adopt).
pub fn takeover() -> Vec<PlanStep> {
    vec![
        PlanStep::new("Writing the Platform Manager's compose file").when(&["Writing the Platform Manager's compose file"]),
        PlanStep::new("Stopping SENTIENT").detail("Its containers are kept until the new ones answer").when(&["Stopping SENTIENT"]),
        PlanStep::new("Starting SENTIENT from the Platform Manager's compose file").weight(3.),
        PlanStep::new("Saving the installation record").when(&["Looking at the instance again", "Checking the Platform Manager can reach", "Saving the installation record"]),
    ]
}

/// Moving data to other folders: (what is shown, the start of the engine
/// step that is it), in order.
pub fn move_steps(moves: &[Move], ov: Option<&Overview>, windows: bool) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let stops = moves.iter().any(|m| m.store.stops());
    if windows {
        let to = moves.first().map(|m| m.to.clone()).unwrap_or_default();
        return vec![
            ("Stop the SENTIENT service".into(), "Stopping SENTIENT".into()),
            ("Shut down the WSL distribution sentient (wsl --terminate, as sentient-clickhouse)".into(), "Shutting down".into()),
            (format!("Copy ext4.vhdx to {to} and compare SHA-256"), "Copying".into()),
            ("Point the distribution at the new file (wsl --manage sentient --move; export and import on WSL before 2.0)".into(), "Pointing".into()),
            ("Start ClickHouse, then the SENTIENT service".into(), "Starting".into()),
            ("Check SENTIENT and ClickHouse answer".into(), "Checking".into()),
        ];
    }
    if stops {
        v.push(("Stop SENTIENT".to_string(), "Stopping SENTIENT".to_string()));
    }
    for m in moves {
        v.push((format!("Copy {} data to {}", m.store.label(), m.to), format!("Copying {} data", m.store.label())));
    }
    for m in moves {
        v.push((
            if moves.len() == 1 { "Verify sizes and checksums".to_string() } else { format!("Verify {} (sizes and checksums)", m.store.label()) },
            format!("Verifying {}", m.store.label()),
        ));
    }
    for m in moves.iter().filter(|m| m.store != Store::Backups) {
        let vol = ov.and_then(|ov| ov.places.iter().find(|p| p.store == m.store)).map(|p| p.volume.clone()).unwrap_or_default();
        let key = if vol.is_empty() { format!("Pointing the {}", m.store.label()) } else { format!("Pointing the {vol} volume") };
        v.push((format!("Point the {} volume at the new folder (bind mount)", if vol.is_empty() { m.store.label() } else { vol.as_str() }), key));
    }
    if stops {
        v.push(("Start SENTIENT".to_string(), "Starting SENTIENT".to_string()));
        let dbs: Vec<&str> = moves.iter().filter(|m| m.store.is_database()).map(|m| m.store.label()).collect();
        let t = if dbs.is_empty() { "Check SENTIENT answers".to_string() } else { format!("Check SENTIENT answers and {} {} the same rows", dbs.join(" and "), if dbs.len() == 1 { "holds" } else { "hold" }) };
        v.push((t, "Checking SENTIENT answers".to_string()));
    }
    v
}

pub fn move_data(moves: &[Move], ov: Option<&Overview>, windows: bool) -> Vec<PlanStep> {
    move_steps(moves, ov, windows)
        .into_iter()
        .map(|(title, key)| {
            let w = if key.starts_with("Copying") || key.starts_with("Verifying") { 4. } else { 1. };
            PlanStep::new(title).when(&[key.as_str()]).weight(w)
        })
        .collect()
}

/// The changes a newer Platform Manager makes in the installation.
pub fn changes(pending: &[crate::changes::Change]) -> Vec<PlanStep> {
    crate::changes::planned_steps(pending).into_iter().map(PlanStep::new).collect()
}

/// Restarting a server (crate::manager::server_card): the window drives
/// these steps itself; the server is down for most of them. The mockup's
/// six steps, all shown up front.
pub const STEP_RESTART_SENT: &str = "Restart sent";
pub const STEP_WENT_DOWN: &str = "Server went down";
pub const STEP_COMING_BACK: &str = "Waiting for the server to come back";
pub const STEP_RESTARTED: &str = "Check it really restarted";
pub const STEP_RUNNING: &str = "SENTIENT and database running";
pub const STEP_ANSWERS: &str = "SENTIENT answers on its web address";
pub const STEP_SHUTDOWN_SENT: &str = "Shut down sent";

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn server_restart(windows: bool) -> Vec<PlanStep> {
    vec![
        PlanStep::new(STEP_RESTART_SENT).detail(format!("SENTIENT and its database stopped cleanly first, then {}", if windows { "shutdown /r" } else { "systemctl reboot" })).weight(1.),
        PlanStep::new(STEP_WENT_DOWN).detail("Until the connection really drops").weight(0.5),
        PlanStep::new(STEP_COMING_BACK).detail("Reconnecting; gives up after 10 min").weight(3.),
        PlanStep::new(STEP_RESTARTED).detail("Its start time is new").weight(0.5),
        PlanStep::new(STEP_RUNNING).weight(2.),
        PlanStep::new(STEP_ANSWERS).weight(1.),
    ]
}

/// Shutting a server down: sent, then confirmed down (it shows as off).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn server_shutdown(windows: bool) -> Vec<PlanStep> {
    vec![
        PlanStep::new(STEP_SHUTDOWN_SENT).detail(format!("SENTIENT and its database stopped cleanly first, then {}", if windows { "shutdown /s" } else { "systemctl poweroff" })),
        PlanStep::new(STEP_WENT_DOWN).detail("Until the connection really drops").weight(0.5),
    ]
}

/// Restarting Docker (crate::server_ops).
pub fn docker_restart(sentient_on_docker: bool) -> Vec<PlanStep> {
    use crate::server_ops as so;
    let mut v = vec![PlanStep::new(so::STEP_DOCKER_RESTART).detail("Its containers stop and start with it"), PlanStep::new(so::STEP_DOCKER_WAIT).weight(0.5)];
    if sentient_on_docker {
        v.push(PlanStep::new(so::STEP_DOCKER_SENTIENT).detail("Its containers started").weight(2.));
    }
    v
}

/// Installing the system's updates (crate::server_ops).
pub fn system_updates() -> Vec<PlanStep> {
    use crate::server_ops as so;
    vec![
        PlanStep::new(so::STEP_UPD_LOOK).detail("The package lists read again"),
        PlanStep::new(so::STEP_UPD_INSTALL).weight(6.),
        PlanStep::new(so::STEP_UPD_CHECK).weight(0.3),
    ]
}

/// A `--job` run's steps (the agent on a server, a root job), sent before
/// it starts; None for a job too short to have steps.
pub fn for_job(record: &InstallationRecord, args: &[String]) -> Option<Vec<PlanStep>> {
    let shape = Shape::of(record);
    let has = |f: &str| args.iter().any(|a| a == f);
    Some(match args.first()?.as_str() {
        "backup-now" => backup(&BackupPrefs::load(), false, has("--passphrase-stdin")),
        "scheduled-backup" => backup(&BackupPrefs::load(), true, false),
        "backup" => backup_file(),
        "update" => {
            let version = args.windows(2).find(|w| w[0] == "--version").map(|w| w[1].clone());
            let download = (!shape.docker).then(|| version.clone().or_else(|| has("--latest").then(|| "(newest)".to_string()))).flatten();
            update(shape, download.as_deref(), version.as_deref())
        }
        "rollback" => rollback(shape),
        "repair" => repair(shape, record.flavor() == sentient_installer_core::stack::Flavor::Clickhouse),
        "uninstall" => uninstall(!has("--delete-data")),
        "migrate" | "resume-migration" => migration(),
        "take-over-compose" => takeover(),
        "move-data" => {
            let moves = crate::movedata::parse_moves(args.get(1)?).ok()?;
            let ov = crate::movedata::local_overview().ok();
            move_data(&moves, ov.as_ref(), cfg!(windows) && record.engine == Engine::Native)
        }
        "apply-changes" => changes(&crate::changes::pending(record)),
        _ => return None,
    })
}

/// A job on a server before its agent answers with its own steps: the same
/// plans with what is known here (the agent's replaces it).
pub fn guess(args: &[String], docker: bool, windows: bool) -> Vec<PlanStep> {
    let shape = Shape { docker, windows };
    let has = |f: &str| args.iter().any(|a| a == f);
    let mut v = vec![connect()];
    v.extend(match args.first().map(String::as_str).unwrap_or("") {
        "backup-now" => backup(&BackupPrefs::default(), false, has("--passphrase-stdin")),
        "backup" => backup_file(),
        "update" => {
            let version = args.windows(2).find(|w| w[0] == "--version").map(|w| w[1].clone());
            update(shape, (!docker).then_some(version.as_deref()).flatten(), version.as_deref())
        }
        "rollback" => rollback(shape),
        "repair" => repair(shape, false),
        "uninstall" => uninstall(!has("--delete-data")),
        "migrate" => migration(),
        "take-over-compose" => takeover(),
        "move-data" => args.get(1).and_then(|a| crate::movedata::parse_moves(a).ok()).map(|m| move_data(&m, None, windows && !docker)).unwrap_or_default(),
        "docker-restart" => docker_restart(docker),
        "system-updates" => system_updates(),
        _ => Vec::new(),
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_lists_its_steps_with_the_engine_words() {
        let prefs = BackupPrefs::default();
        let p = backup(&prefs, true, true);
        assert!(p.first().unwrap().matches(backups::STEP_WRITE));
        assert!(p.iter().any(|s| s.matches("Dumping database (full)")));
        assert!(p.iter().any(|s| s.matches(backups::STEP_CHECK)));
        assert!(p.last().unwrap().matches(backups::STEP_PRUNE));
        assert!(p.iter().any(|s| s.detail.contains("encrypted")));
    }

    #[test]
    fn an_update_names_the_engine_steps() {
        let d = update(Shape { docker: true, windows: false }, None, Some("4.4.0.3"));
        for name in ["Getting the new SENTIENT image", "Stopping SENTIENT", "Checking the new version's database changes (dry run)", "Copying the database (12 MB) to x", "Applying the database changes", "Checking SENTIENT stays up"] {
            assert!(d.iter().any(|s| s.matches(name)), "{name}");
        }
        let n = update(Shape { docker: false, windows: true }, Some("4.4.0.3"), None);
        assert!(n[0].matches("Downloading SENTIENT 4.4.0.3 (1 of 2: x, 4 MB)"));
        assert!(n.iter().any(|s| s.matches("Replacing the program files")));
    }

    #[test]
    fn a_server_job_starts_with_the_connection() {
        let g = guess(&["backup-now".into()], true, false);
        assert_eq!(g[0].name, CONNECT);
        assert!(g.len() > 3);
        assert_eq!(guess(&["control".into(), "start".into()], true, false).len(), 1);
    }
}
