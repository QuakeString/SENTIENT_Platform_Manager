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
pub const CONNECT: &str = n!("Connect to the server");

pub fn connect() -> PlanStep {
    PlanStep::new(CONNECT).detail(n!("SSH, and the Platform Manager there brought up to date")).weight(0.5)
}

/// A backup with the saved settings (`backups::run`).
pub fn backup(prefs: &BackupPrefs, automatic: bool, encrypted: bool) -> Vec<PlanStep> {
    let mut v = vec![PlanStep::new(n!("Connect to the database")).detail(n!("Reads what there is to back up")).when(&[backups::STEP_WRITE, n!("Connecting and inspecting")]).weight(0.5)];
    v.push(PlanStep::new(n!("Dump the database")).detail(if prefs.preset == Preset::Configuration { n!("Configuration only") } else { n!("Devices, dashboards, users, alarms and settings") }).when(&[n!("Dumping database")]).weight(4.));
    if prefs.preset == Preset::Everything && prefs.history != History::None {
        let what = match prefs.history {
            History::LastDays(d) => nf!("The last {d} days"),
            _ => n!("All of it").into(),
        };
        v.push(PlanStep::new(n!("Copy the telemetry history")).detail(what).when(&[n!("Exporting telemetry"), n!("Exporting ClickHouse telemetry")]).weight(4.));
    }
    if prefs.preset == Preset::Everything {
        v.push(PlanStep::new(n!("Archive the file stores")).detail(n!("Reports and version control")).when(&[n!("Archiving file store")]));
    }
    let mut how = vec![n!("Compressed").to_string()];
    if prefs.part_gb > 0 {
        how.push(nf!("split over {} GB", prefs.part_gb));
    }
    if encrypted {
        how.push(n!("encrypted").into());
    }
    v.push(PlanStep::new(n!("Write the backup file")).detail(how.join(", ")).when(&[n!("Writing manifest and archive")]).weight(2.));
    v.push(PlanStep::new(n!("Read the file back to check it")).detail(n!("SHA-256 of every part")).when(&[backups::STEP_CHECK]).weight(2.));
    if let Some(to) = &prefs.copy_to {
        v.push(PlanStep::new(n!("Copy it to the second location")).detail(to.display().to_string()).when(&[backups::STEP_COPY]));
    }
    if automatic {
        v.push(PlanStep::new(n!("Remove older automatic backups")).detail(nf!("Keeps the newest {}", prefs.keep.max(1))).when(&[backups::STEP_PRUNE]).weight(0.5));
    }
    v
}

/// A backup into one file (`backup <file>`, the engine's own steps).
pub fn backup_file() -> Vec<PlanStep> {
    vec![
        PlanStep::new(n!("Connect to the database")).when(&[n!("Connecting and inspecting")]).weight(0.5),
        PlanStep::new(n!("Dump the database")).when(&[n!("Dumping database")]).weight(4.),
        PlanStep::new(n!("Copy the telemetry history")).when(&[n!("Exporting telemetry"), n!("Exporting ClickHouse telemetry")]).weight(4.),
        PlanStep::new(n!("Archive the file stores")).when(&[n!("Archiving file store")]),
        PlanStep::new(n!("Write the backup file")).when(&[n!("Writing manifest and archive")]).weight(2.),
    ]
}

/// A restore (crate::restore_flow::run), as its request asks.
pub fn restore(req: &rf::Request) -> Vec<PlanStep> {
    let file = req.file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    rf::planned_steps(req)
        .into_iter()
        .map(|name| {
            let (detail, weight) = match name.as_str() {
                rf::STEP_SAFETY => (n!("Everything, before anything changes").to_string(), 3.),
                rf::STEP_CHECK => (file.clone(), 1.),
                rf::STEP_RESTORE => (if req.scope == Scope::Configuration { n!("Configuration only").into() } else { n!("PostgreSQL, telemetry and file stores").into() }, 5.),
                rf::STEP_INTO_LIVE => (req.scope.label(), 5.),
                rf::STEP_KEEP => (n!("This server's telemetry stays").into(), 2.),
                rf::STEP_SCHEMA => (String::new(), 1.),
                rf::STEP_SWITCH => (n!("SENTIENT restarts on it; the previous data comes back if it does not answer").into(), 2.),
                rf::STEP_SETTINGS => (if req.use_from_backup.len() == 1 { nf!("{} setting", req.use_from_backup.len()) } else { nf!("{} settings", req.use_from_backup.len()) }, 1.),
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
        v.push(PlanStep::new(n!("Getting the new SENTIENT image")).detail(to.map(|t| nf!("SENTIENT {t}")).unwrap_or_default()).weight(3.));
        v.push(PlanStep::new(n!("Stopping SENTIENT")));
    } else {
        if let Some(d) = download {
            v.push(PlanStep::new(nf!("Downloading SENTIENT {d}")).detail(n!("Checked against its SHA-256 and signature")).when(&[n!("Downloading SENTIENT")]).weight(3.));
        }
        v.push(PlanStep::new(n!("Unpacking the new SENTIENT")).when(&[n!("Unpacking")]));
        v.push(PlanStep::new(n!("Stopping SENTIENT")).when(&[n!("Stopping")]));
    }
    v.push(PlanStep::new(n!("Checking the new version's database changes (dry run)")).detail(n!("Nothing changes if they would fail")).weight(2.));
    v.push(PlanStep::new(n!("Copying the database before the update")).detail(n!("Kept until the next update, to go back to")).when(&[n!("Copying the database")]).weight(3.));
    if !shape.docker {
        v.push(PlanStep::new(n!("Replacing the program files")));
    }
    v.push(PlanStep::new(n!("Applying the database changes")).weight(2.));
    v.push(PlanStep::new(n!("Starting SENTIENT")).detail(n!("And checking it stays up")).when(&[n!("Starting SENTIENT"), n!("Checking SENTIENT stays up"), n!("Waiting for SENTIENT")]).weight(2.));
    v
}

/// A Docker install moved to the release `to` (crate::updating::docker_step):
/// the update, the update then the new image tag, or the tag alone.
pub fn docker_to(shape: Shape, to: &str, how: crate::updating::DockerStep) -> Vec<PlanStep> {
    use crate::updating::DockerStep;
    let tag = PlanStep::new(nf!("Setting the image tag to {to}")).detail(n!("The compose file names this release from now on")).when(&[n!("Setting the image tag"), n!("Tagging the image SENTIENT runs"), n!("Writing ")]);
    match how {
        DockerStep::Update | DockerStep::AlreadyRunning => update(shape, None, Some(to)),
        DockerStep::UpdateAndPin => {
            let mut v = update(shape, None, Some(to));
            v.push(tag);
            v
        }
        DockerStep::QuickPin => vec![
            tag,
            PlanStep::new(n!("Starting SENTIENT")).detail(n!("Under the new tag, and checking it stays up")).when(&[n!("Starting SENTIENT"), n!("Checking SENTIENT stays up"), n!("Waiting for SENTIENT")]).weight(2.),
        ],
    }
}

/// Going back to the version the last update replaced.
pub fn rollback(shape: Shape) -> Vec<PlanStep> {
    vec![
        PlanStep::new(n!("Stopping SENTIENT")).when(&[n!("Stopping")]),
        PlanStep::new(if shape.docker { n!("Putting the previous image back") } else { n!("Putting the previous program files back") }),
        PlanStep::new(n!("Putting the database from before the update back")).detail(n!("The one in use now is kept aside")).when(&[n!("Putting the database from before the update back")]).weight(3.),
        PlanStep::new(n!("Starting SENTIENT")).when(&[n!("Starting SENTIENT"), n!("Checking SENTIENT stays up"), n!("Waiting for SENTIENT")]).weight(2.),
    ]
}

/// The repair (crate::maintenance_ops::run_repair).
pub const STEP_REPAIR: &str = n!("Repairing SENTIENT");

pub fn repair(shape: Shape, clickhouse_wsl: bool) -> Vec<PlanStep> {
    let mut v = Vec::new();
    if shape.windows && !shape.docker && clickhouse_wsl {
        v.push(PlanStep::new(n!("Making ClickHouse safe against unclean stops")).when(&[n!("Stopping ClickHouse cleanly")]));
    }
    v.push(PlanStep::new(STEP_REPAIR).detail(if shape.docker { n!("Its compose file written again, the containers started") } else { n!("Its service and configuration written again, then started") }).weight(4.));
    v.push(PlanStep::new(n!("Restoring the Platform Manager")).detail(n!("Its program, menu entry and operator screen")));
    v
}

/// The uninstall (crate::maintenance_ops::run_uninstall).
pub const STEP_UNINSTALL: &str = n!("Removing SENTIENT");

pub fn uninstall(keep_data: bool) -> Vec<PlanStep> {
    vec![
        PlanStep::new(STEP_UNINSTALL).detail(if keep_data { n!("The database is kept for a new install") } else { n!("With its database and telemetry") }).weight(4.),
        PlanStep::new(n!("Removing SENTIENT's shortcuts")).detail(n!("Menu entries, the operator screen and sign-in")),
    ]
}

/// The change of database type (crate::migration::run).
pub fn migration() -> Vec<PlanStep> {
    vec![
        PlanStep::new(n!("A full backup, history included")).when(&[n!("Backing up")]).weight(3.),
        PlanStep::new(n!("SENTIENT stops")).when(&[n!("Stopping"), n!("Preparing ClickHouse"), n!("Preparing the database")]),
        PlanStep::new(n!("SENTIENT starts on the new type")).when(&[n!("Starting SENTIENT on")]).weight(2.),
        PlanStep::new(n!("The history is copied month by month")).when(&[n!("Copying the history")]).weight(4.),
    ]
}

/// Taking over a compose file someone else wrote (crate::adopt).
pub fn takeover() -> Vec<PlanStep> {
    vec![
        PlanStep::new(n!("Writing the Platform Manager's compose file")).when(&[n!("Writing the Platform Manager's compose file")]),
        PlanStep::new(n!("Stopping SENTIENT")).detail(n!("Its containers are kept until the new ones answer")).when(&[n!("Stopping SENTIENT")]),
        PlanStep::new(n!("Starting SENTIENT from the Platform Manager's compose file")).weight(3.),
        PlanStep::new(n!("Saving the installation record")).when(&[n!("Looking at the instance again"), n!("Checking the Platform Manager can reach"), n!("Saving the installation record")]),
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
            (n!("Stop the SENTIENT service").into(), n!("Stopping SENTIENT").into()),
            (n!("Shut down the WSL distribution sentient (wsl --terminate, as sentient-clickhouse)").into(), n!("Shutting down").into()),
            (nf!("Copy ext4.vhdx to {to} and compare SHA-256"), n!("Copying").into()),
            (n!("Point the distribution at the new file (wsl --manage sentient --move; export and import on WSL before 2.0)").into(), n!("Pointing").into()),
            (n!("Start ClickHouse, then the SENTIENT service").into(), n!("Starting").into()),
            (n!("Check SENTIENT and ClickHouse answer").into(), n!("Checking").into()),
        ];
    }
    if stops {
        v.push((n!("Stop SENTIENT").to_string(), n!("Stopping SENTIENT").to_string()));
    }
    for m in moves {
        v.push((nf!("Copy {} data to {}", m.store.label(), m.to), nf!("Copying {} data", m.store.label())));
    }
    for m in moves {
        v.push((
            if moves.len() == 1 { n!("Verify sizes and checksums").to_string() } else { nf!("Verify {} (sizes and checksums)", m.store.label()) },
            nf!("Verifying {}", m.store.label()),
        ));
    }
    for m in moves.iter().filter(|m| m.store != Store::Backups) {
        let vol = ov.and_then(|ov| ov.places.iter().find(|p| p.store == m.store)).map(|p| p.volume.clone()).unwrap_or_default();
        let key = if vol.is_empty() { nf!("Pointing the {}", m.store.label()) } else { nf!("Pointing the {vol} volume") };
        v.push((nf!("Point the {} volume at the new folder (bind mount)", if vol.is_empty() { m.store.label() } else { vol.as_str() }), key));
    }
    if stops {
        v.push((n!("Start SENTIENT").to_string(), n!("Starting SENTIENT").to_string()));
        let dbs: Vec<&str> = moves.iter().filter(|m| m.store.is_database()).map(|m| m.store.label()).collect();
        let t = match dbs.as_slice() {
            [] => n!("Check SENTIENT answers").to_string(),
            [one] => nf!("Check SENTIENT answers and {one} holds the same rows"),
            [a, b] => nf!("Check SENTIENT answers and {a} and {b} hold the same rows"),
            _ => nf!("Check SENTIENT answers and {} hold the same rows", dbs.join(", ")),
        };
        v.push((t, n!("Checking SENTIENT answers").to_string()));
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
pub const STEP_RESTART_SENT: &str = n!("Restart sent");
pub const STEP_WENT_DOWN: &str = n!("Server went down");
pub const STEP_COMING_BACK: &str = n!("Waiting for the server to come back");
pub const STEP_RESTARTED: &str = n!("Check it really restarted");
pub const STEP_RUNNING: &str = n!("SENTIENT and database running");
pub const STEP_ANSWERS: &str = n!("SENTIENT answers on its web address");
pub const STEP_SHUTDOWN_SENT: &str = n!("Shut down sent");

#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn server_restart(windows: bool) -> Vec<PlanStep> {
    vec![
        PlanStep::new(STEP_RESTART_SENT).detail(nf!("SENTIENT and its database stopped cleanly first, then {}", if windows { "shutdown /r" } else { "systemctl reboot" })).weight(1.),
        PlanStep::new(STEP_WENT_DOWN).detail(n!("Until the connection really drops")).weight(0.5),
        PlanStep::new(STEP_COMING_BACK).detail(n!("Reconnecting; gives up after 10 min")).weight(3.),
        PlanStep::new(STEP_RESTARTED).detail(n!("Its start time is new")).weight(0.5),
        PlanStep::new(STEP_RUNNING).weight(2.),
        PlanStep::new(STEP_ANSWERS).weight(1.),
    ]
}

/// Shutting a server down: sent, then confirmed down (it shows as off).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn server_shutdown(windows: bool) -> Vec<PlanStep> {
    vec![
        PlanStep::new(STEP_SHUTDOWN_SENT).detail(nf!("SENTIENT and its database stopped cleanly first, then {}", if windows { "shutdown /s" } else { "systemctl poweroff" })),
        PlanStep::new(STEP_WENT_DOWN).detail(n!("Until the connection really drops")).weight(0.5),
    ]
}

/// Restarting Docker (crate::server_ops).
pub fn docker_restart(sentient_on_docker: bool) -> Vec<PlanStep> {
    use crate::server_ops as so;
    let mut v = vec![PlanStep::new(so::STEP_DOCKER_RESTART).detail(n!("Its containers stop and start with it")), PlanStep::new(so::STEP_DOCKER_WAIT).weight(0.5)];
    if sentient_on_docker {
        v.push(PlanStep::new(so::STEP_DOCKER_SENTIENT).detail(n!("Its containers started")).weight(2.));
    }
    v
}

/// Installing the system's updates (crate::server_ops).
pub fn system_updates() -> Vec<PlanStep> {
    use crate::server_ops as so;
    vec![
        PlanStep::new(so::STEP_UPD_LOOK).detail(n!("The package lists read again")),
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
            if let (true, Some(v)) = (shape.docker, &version) {
                let running = crate::installation::running_version(record);
                let tag = sentient_installer_core::distro::docker_ops::image_tag(&record.stack.image);
                return Some(docker_to(shape, v, crate::updating::docker_step(v, running.as_deref(), tag, record.stack.external.is_some())));
            }
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
