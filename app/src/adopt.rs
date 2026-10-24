// Copyright © 2016-2026 The SENTIENT Authors

//! Managing a SENTIENT the Platform Manager did not install: find what runs
//! on this machine, show it, check the Platform Manager can reach its
//! databases, and write the installation record every screen starts from.
//! Nothing is installed, moved or restarted.
//!
//! - Docker: the stack as Docker describes it (distro::adopt). It keeps
//!   running from its own compose files until the operator lets the
//!   Platform Manager take them over (`take_over_compose`).
//! - Native Linux: `sentient.service` and its environment file
//!   (native_linux::read_installed). The file is root-only, so without root
//!   only the unit is shown and the adopt itself runs as root
//!   (crate::rootjob, `--job add-existing --native`).
//! - Windows: the SentientServer and PostgreSQL services
//!   (native::adopt_windows). Its passwords go to the secret store; the
//!   PostgreSQL admin password is the stored one, the operator's, or — when
//!   neither is known — a new one set through SENTIENT's superuser role.

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
    /// Its details are root-only (native Linux): shown after adopting, which
    /// runs as administrator.
    #[serde(default)]
    pub needs_admin: bool,
    /// Windows: the PostgreSQL admin password the Platform Manager has does
    /// not work (or it has none), so adopting sets a new one unless the
    /// operator gives it.
    #[serde(default)]
    pub sets_admin_password: bool,
}

impl Candidate {
    /// Lines for a summary: (label, value).
    pub fn facts(&self) -> Vec<(&'static str, String)> {
        let st = &self.record.stack;
        if self.needs_admin {
            return vec![
                ("Runs as", self.kind.clone()),
                ("From", self.location.clone()),
                ("State", if self.running { "Running".into() } else { "Stopped".into() }),
                ("Details", "Read as administrator when you choose to manage it (its settings file is root-only).".into()),
            ];
        }
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
        manager_version: crate::updates::manager_version().into(),
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
                needs_admin: false,
                sets_admin_password: false,
            };
            (c, found)
        })
        .collect()
}

fn native_record(cfg: &sentient_installer_core::native::NativeConfig) -> InstallationRecord {
    let stack = DeployConfig {
        flavor: cfg.flavor,
        license_server_url: cfg.license_server_url.clone(),
        db_name: cfg.db_name.clone(),
        db_user: cfg.db_user.clone(),
        db_password: String::new(),
        pg_port: cfg.pg_port,
        pg: cfg.pg.clone(),
        ch_db: cfg.ch_db.clone(),
        ch_user: cfg.ch_user.clone(),
        ch_password: String::new(),
        ch_http_port: cfg.ch_http_port,
        ch_native_port: cfg.ch_native_port,
        ch: cfg.ch.clone(),
        http_port: cfg.http_port,
        mqtt_port: cfg.mqtt_port,
        coap_port: cfg.coap_port,
        tuning: cfg.tuning.clone(),
        ..DeployConfig::default()
    };
    InstallationRecord {
        sentient_version: "unknown".into(),
        manager_version: crate::updates::manager_version().into(),
        engine: Engine::Native,
        install_dir: cfg.install_dir.clone(),
        pg_dir: cfg.pg_dir.clone(),
        state_dir: cfg.state_dir.clone(),
        stack,
        installed_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// The systemd installation: complete as root, the unit alone otherwise.
#[cfg(target_os = "linux")]
fn native_candidate() -> Option<(Candidate, Option<sentient_installer_core::native_linux::FoundNative>)> {
    use sentient_installer_core::native_linux as nl;
    let dir = nl::installed_dir()?;
    let kind = "Native (systemd service sentient)".to_string();
    let location = dir.display().to_string();
    if !crate::rootjob::is_root() {
        let mut record = native_record(&crate::installation::InstallationRecord::placeholder_native(&dir));
        record.install_dir = dir;
        let c = Candidate {
            record,
            kind,
            location,
            running: nl::installed_running(),
            notes: Vec::new(),
            problem: None,
            needs_admin: true,
            sets_admin_password: false,
        };
        return Some((c, None));
    }
    Some(match nl::read_installed()? {
        Ok(found) => {
            let c = Candidate {
                record: native_record(&found.cfg),
                kind,
                location,
                running: found.running,
                notes: found.notes.clone(),
                problem: found.problem.clone(),
                needs_admin: false,
                sets_admin_password: false,
            };
            (c, Some(found))
        }
        Err(e) => {
            let c = Candidate {
                record: native_record(&crate::installation::InstallationRecord::placeholder_native(&dir)),
                kind,
                location,
                running: nl::installed_running(),
                notes: Vec::new(),
                problem: Some(format!("Its settings could not be read: {e}")),
                needs_admin: false,
                sets_admin_password: false,
            };
            (c, None)
        }
    })
}

/// The Windows services' installation.
#[cfg(windows)]
fn windows_candidate() -> Option<(Candidate, Option<sentient_installer_core::native::adopt_windows::FoundWindows>)> {
    use sentient_installer_core::native::adopt_windows as aw;
    let dir = aw::installed_dir()?;
    let kind = "Windows services (SentientServer)".to_string();
    let location = dir.display().to_string();
    Some(match aw::read_installed()? {
        Ok(found) => {
            let stored = crate::secrets::get(crate::secrets::Secret::PostgresAdmin);
            let known = found.problem.is_none() && stored.is_some_and(|p| aw::admin_password_works(&found.cfg, &p));
            let mut record = native_record(&found.cfg);
            record.state_dir = found.cfg.state_dir.clone();
            let c = Candidate {
                record,
                kind,
                location,
                running: found.running,
                notes: found.notes.clone(),
                problem: found.problem.clone(),
                needs_admin: false,
                sets_admin_password: !known,
            };
            (c, Some(found))
        }
        Err(e) => {
            let c = Candidate {
                record: native_record(&crate::installation::InstallationRecord::placeholder_native(&dir)),
                kind,
                location,
                running: aw::installed_running(),
                notes: Vec::new(),
                problem: Some(format!("Its settings could not be read: {e}")),
                needs_admin: false,
                sets_admin_password: false,
            };
            (c, None)
        }
    })
}

/// Every SENTIENT on this machine. Blocking (asks Docker): call from a job
/// or a background task.
pub fn find() -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    #[cfg(target_os = "linux")]
    found.extend(native_candidate().map(|(c, _)| c));
    #[cfg(windows)]
    found.extend(windows_candidate().map(|(c, _)| c));
    found.extend(docker_candidates().into_iter().map(|(c, _)| c));
    found
}

/// Adopt the installation `wanted` describes: find it again, check the
/// Platform Manager can reach its databases, and write the record.
pub fn adopt(wanted: &Candidate, pg_admin: Option<String>, sink: ProgressFn) -> Result<InstallationRecord, String> {
    if crate::installation::InstallationRecord::load().is_some() {
        return Err("The Platform Manager already manages a SENTIENT instance on this computer.".into());
    }
    let candidate = if wanted.record.engine == Engine::Native {
        adopt_native(pg_admin, &sink)?
    } else {
        sink(Progress::Step { name: "Looking at the instance again".into() });
        let key = wanted.key();
        let (candidate, found) = docker_candidates()
            .into_iter()
            .find(|(c, _)| c.key() == key)
            .ok_or("That instance is no longer there.")?;
        if let Some(p) = &candidate.problem {
            return Err(p.clone());
        }
        sink(Progress::Step { name: "Checking the Platform Manager can reach its databases".into() });
        distro::adopt::verify(&found)?;
        candidate
    };
    sink(Progress::Step { name: "Saving the installation record".into() });
    let json = serde_json::to_string_pretty(&candidate.record).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    if let Ok(manager) = crate::self_install::install_manager() {
        crate::self_install::restore_menu_entry(&manager);
    }
    sink(Progress::Done {
        message: format!("Added: the Platform Manager now manages SENTIENT {} ({}).", candidate.record.sentient_version, candidate.kind),
    });
    Ok(candidate.record)
}

/// The native installation, read and checked (as root on Linux; on
/// Windows its passwords are saved to the secret store).
fn adopt_native(pg_admin: Option<String>, sink: &ProgressFn) -> Result<Candidate, String> {
    #[cfg(windows)]
    {
        use crate::secrets::{Secret, get, save};
        use sentient_installer_core::native::adopt_windows as aw;
        sink(Progress::Step { name: "Reading the SENTIENT services' settings".into() });
        let (candidate, found) = windows_candidate().ok_or("There is no SENTIENT service on this computer any more.")?;
        let found = found.ok_or_else(|| candidate.problem.clone().unwrap_or_default())?;
        if let Some(p) = &found.problem {
            return Err(p.clone());
        }
        let mut cfg = found.cfg.clone();
        sink(Progress::Step { name: "Checking the PostgreSQL admin password".into() });
        cfg.pg_superuser_password = match pg_admin.filter(|p| !p.is_empty()) {
            Some(p) if aw::admin_password_works(&cfg, &p) => p,
            Some(_) => return Err("The PostgreSQL admin (postgres) password you gave does not work.".into()),
            None => match get(Secret::PostgresAdmin).filter(|p| aw::admin_password_works(&cfg, p)) {
                Some(p) => p,
                None => {
                    let p = sentient_installer_core::native::generate_secret();
                    aw::set_admin_password(&cfg, &p)?;
                    sink(Progress::Log { line: "The PostgreSQL admin (postgres) password was unknown: a new one was set and saved in the Platform Manager's secret store.".into() });
                    p
                }
            },
        };
        sink(Progress::Step { name: "Checking the Platform Manager can reach its databases".into() });
        aw::verify(&found, &cfg)?;
        sink(Progress::Step { name: "Saving the passwords in the secret store".into() });
        save(Secret::Database, &cfg.db_password)?;
        save(Secret::PostgresAdmin, &cfg.pg_superuser_password)?;
        save(Secret::Jwt, &cfg.jwt_secret)?;
        if cfg.flavor.uses_clickhouse() {
            save(Secret::Clickhouse, &cfg.ch_password)?;
        }
        Ok(candidate)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = pg_admin;
        sink(Progress::Step { name: "Reading the SENTIENT service's settings".into() });
        let (candidate, found) = native_candidate().ok_or("There is no SENTIENT service on this computer any more.")?;
        let found = found.ok_or_else(|| candidate.problem.clone().unwrap_or_else(|| "Its settings are root-only: add it as administrator.".into()))?;
        sink(Progress::Step { name: "Checking the Platform Manager can reach its databases".into() });
        sentient_installer_core::native_linux::verify_installed(&found)?;
        Ok(candidate)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (pg_admin, sink);
        Err("A native installation on this system is not supported yet.".into())
    }
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

/// Adopt `c`, as administrator when its details are root-only.
pub fn adopt_as_needed(c: &Candidate, pg_admin: Option<String>, sink: ProgressFn) -> Result<(), String> {
    if c.needs_admin {
        let args = vec!["add-existing".to_string(), "--native".to_string()];
        return crate::rootjob::run(&args, &sink).map(|_| ());
    }
    adopt(c, pg_admin, sink).map(|_| ())
}
