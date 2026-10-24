// Copyright © 2016-2026 The SENTIENT Authors

//! Managing a SENTIENT the Platform Manager did not install: find what runs
//! on this machine, show it, check the Platform Manager can reach its
//! databases, and write the installation record every screen starts from.
//! Nothing is installed, moved or restarted.
//!
//! - Docker: the stack as Docker describes it (distro::adopt). It keeps
//!   running from its own compose files until the operator lets the
//!   Platform Manager take them over (`take_over_compose`).

use serde::{Deserialize, Serialize};
use sentient_installer_core::distro::{self, DeployConfig};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord, flavor_label};

/// A SENTIENT found on this machine, without its passwords (they are read
/// again when it is adopted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// The record adopting it would write.
    pub record: InstallationRecord,
    /// "Docker", "Native (systemd)", …
    pub kind: String,
    /// Where it runs from (compose project and files, install folder).
    pub location: String,
    pub running: bool,
    /// What the operator should know before adopting.
    pub notes: Vec<String>,
    /// Why it cannot be managed, when it cannot.
    pub problem: Option<String>,
}

impl Candidate {
    /// Lines for a summary: (label, value).
    pub fn facts(&self) -> Vec<(&'static str, String)> {
        let st = &self.record.stack;
        let mut f = vec![
            ("Runs as", self.kind.clone()),
            ("From", self.location.clone()),
            ("Version", self.record.sentient_version.clone()),
            ("Database type", flavor_label(st.flavor).to_string()),
            ("State", if self.running { "Running".into() } else { "Stopped".into() }),
            ("Web port", st.http_port.to_string()),
            (
                "Database",
                match st.pg_port {
                    0 => format!("{} (user {}, not published on this machine)", st.db_name, st.db_user),
                    port => format!("{} (user {}, port {port})", st.db_name, st.db_user),
                },
            ),
        ];
        if st.flavor.uses_clickhouse() {
            f.push(("ClickHouse", format!("{} (user {}, HTTP port {})", st.ch_db, st.ch_user, st.ch_http_port)));
        }
        f
    }

    /// A key to find the same installation again.
    fn key(&self) -> String {
        format!("{:?}:{}:{}", self.record.engine, self.record.stack.stack_name, self.record.install_dir.display())
    }
}

fn record_for_docker(cfg: &DeployConfig, version: Option<String>) -> InstallationRecord {
    let mut stack = cfg.clone();
    stack.db_password.clear();
    stack.ch_password.clear();
    let dir = std::path::PathBuf::from(cfg.stack_dir());
    InstallationRecord {
        sentient_version: version.unwrap_or_else(|| "unknown".into()),
        manager_version: env!("CARGO_PKG_VERSION").into(),
        engine: Engine::Docker,
        install_dir: dir.clone(),
        pg_dir: Default::default(),
        state_dir: dir,
        stack,
        installed_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn docker_candidates() -> Vec<(Candidate, distro::adopt::FoundStack)> {
    if cfg!(windows) {
        // SENTIENT runs natively on Windows; a Docker Desktop stack is not
        // something the Platform Manager manages there.
        return Vec::new();
    }
    distro::adopt::find()
        .into_iter()
        .map(|found| {
            let location = match &found.cfg.external {
                Some(ext) => format!("compose project {} ({})", ext.project, ext.files.join(", ")),
                None if found.problem.is_some() => "Docker".to_string(),
                None => format!("the Platform Manager's compose file ({})", found.cfg.stack_dir()),
            };
            let mut notes = found.notes.clone();
            if found.cfg.external.is_some() {
                notes.push(match &found.take_over {
                    Ok(()) => "It keeps running from its own compose file. Changing settings or the database type needs the Platform Manager to take the compose file over first (Repair and uninstall).".into(),
                    Err(why) => format!("It keeps running from its own compose file. Settings and the database type cannot be changed from here: {why}"),
                });
            }
            let c = Candidate {
                record: record_for_docker(&found.cfg, found.version.clone()),
                kind: format!("Docker (container {})", found.cfg.app_container()),
                location,
                running: found.running,
                notes,
                problem: found.problem.clone(),
            };
            (c, found)
        })
        .collect()
}

/// Every SENTIENT on this machine. Blocking (asks Docker): call from a job
/// or a background task.
pub fn find() -> Vec<Candidate> {
    docker_candidates().into_iter().map(|(c, _)| c).collect()
}

/// Adopt the installation `wanted` describes: find it again, check the
/// Platform Manager can reach its databases, and write the record.
pub fn adopt(wanted: &Candidate, sink: ProgressFn) -> Result<InstallationRecord, String> {
    if crate::installation::InstallationRecord::load().is_some() {
        return Err("The Platform Manager already manages an installation on this computer.".into());
    }
    sink(Progress::Step { name: "Looking at the installation again".into() });
    let key = wanted.key();
    let (candidate, found) = docker_candidates()
        .into_iter()
        .find(|(c, _)| c.key() == key)
        .ok_or("That installation is no longer there.")?;
    if let Some(p) = &candidate.problem {
        return Err(p.clone());
    }
    sink(Progress::Step { name: "Checking the Platform Manager can reach its databases".into() });
    distro::adopt::verify(&found)?;
    sink(Progress::Step { name: "Saving the installation record".into() });
    let json = serde_json::to_string_pretty(&candidate.record).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    if let Ok(manager) = crate::self_install::install_manager() {
        crate::self_install::restore_menu_entry(&manager);
    }
    sink(Progress::Done {
        message: format!("The Platform Manager now manages SENTIENT {} ({}).", candidate.record.sentient_version, candidate.kind),
    });
    Ok(candidate.record)
}

/// An adopted Docker stack: let the Platform Manager's own compose file run
/// it from now on (needed before settings or the database type change).
pub fn take_over_compose(record: InstallationRecord, sink: ProgressFn) -> Result<InstallationRecord, String> {
    if record.engine != Engine::Docker || record.stack.external.is_none() {
        return Err("This installation already runs from the Platform Manager's compose file.".into());
    }
    let full = distro::docker_ops::with_secrets(&record.stack)?;
    let own = distro::adopt::take_over(sink, &full)?;
    let mut updated = record;
    updated.stack = own;
    updated.stack.db_password.clear();
    updated.stack.ch_password.clear();
    let json = serde_json::to_string_pretty(&updated).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    Ok(updated)
}

/// One candidate as a line, for the root job's output (crate::rootjob).
pub fn encode(c: &Candidate) -> String {
    format!("FOUND\t{}", serde_json::to_string(c).unwrap_or_default())
}

pub fn decode(line: &str) -> Option<Candidate> {
    serde_json::from_str(line.strip_prefix("FOUND\t")?).ok()
}
