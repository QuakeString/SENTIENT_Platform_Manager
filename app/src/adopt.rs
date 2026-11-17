// Copyright © 2016-2026 The SENTIENT Authors

//! Managing a SENTIENT the Platform Manager did not install: find what runs
//! on this machine, show it, check the Platform Manager can reach its
//! databases, and write the installation record every screen starts from.
//! Nothing is installed, moved or restarted.
//!
//! - Docker: the stack as Docker describes it (distro::adopt). It keeps
//!   running from its own compose files until the operator lets the
//!   Platform Manager take them over (`take_over_compose`).
//! - Docker on Windows: in each running WSL distro of this account other
//!   than the Platform Manager's own (Ubuntu, …), and Docker Desktop
//!   (distro::hosts). A stack found in a distro records it
//!   (`DeployConfig::wsl_distro`) and every Docker command for it runs
//!   there; the distro itself is never stopped, removed or set up. A stack
//!   only Docker Desktop's Windows side shows is listed, but needs Docker
//!   Desktop's WSL integration turned on for a distro first.
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

    /// A native installation that can be added as soon as the PostgreSQL
    /// admin (postgres) password is known (or a new one is set): nothing
    /// else stands in the way.
    pub fn only_needs_pg_admin(&self) -> bool {
        self.record.engine == Engine::Native && self.problem.is_none() && !self.needs_admin && self.sets_admin_password
    }

    /// A key to find the same installation again.
    fn key(&self) -> String {
        format!(
            "{:?}:{}:{}:{}",
            self.record.engine,
            self.record.stack.stack_name,
            self.record.install_dir.display(),
            self.record.stack.wsl_distro.as_deref().unwrap_or("")
        )
    }
}

/// What looking for SENTIENT found, and every place it looked, so a
/// search that finds nothing says what was checked.
#[derive(Debug, Clone, Default)]
pub struct Search {
    pub found: Vec<Candidate>,
    /// One line per place: "WSL distro Ubuntu: Docker, no SENTIENT", …
    pub looked: Vec<String>,
}

impl Search {
    /// The places looked in, as one sentence.
    pub fn looked_text(&self) -> String {
        looked_text(&self.looked)
    }
}

/// "Looked in: a; b; c." (empty when nothing was listed).
pub fn looked_text(looked: &[String]) -> String {
    if looked.is_empty() { String::new() } else { format!("Looked in: {}.", looked.join("; ")) }
}

/// add-if-one's line when the one SENTIENT found needs the PostgreSQL
/// admin password: `@@NEEDS-PG-ADMIN <kind> | <location>`. A window on
/// another computer asks for the password and runs
/// `add-existing --native --pg-admin-password-stdin` (crate::remote).
pub const NEEDS_PG_ADMIN: &str = "@@NEEDS-PG-ADMIN";

/// What a window shows when it asks for the PostgreSQL admin password.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NeedsPgAdmin {
    pub kind: String,
    pub location: String,
}

/// The marker line for `c` (one line; a "|" in the kind becomes "/").
pub fn needs_pg_admin_line(c: &Candidate) -> String {
    let clean = |t: &str| t.replace(['\r', '\n'], " ").replace('|', "/").trim().to_string();
    format!("{NEEDS_PG_ADMIN} {} | {}", clean(&c.kind), c.location.replace(['\r', '\n'], " ").trim())
}

/// The marker line read back; None for any other line.
pub fn parse_needs_pg_admin(line: &str) -> Option<NeedsPgAdmin> {
    let rest = line.trim().strip_prefix(NEEDS_PG_ADMIN)?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let (kind, location) = rest.split_once(" | ").unwrap_or((rest, ""));
    Some(NeedsPgAdmin { kind: kind.trim().to_string(), location: location.trim().to_string() })
}

/// "no SENTIENT", "1 SENTIENT", "2 SENTIENT".
fn count_text(n: usize) -> String {
    if n == 0 { "no SENTIENT".into() } else { format!("{n} SENTIENT") }
}

fn record_for_docker(cfg: &DeployConfig, version: Option<String>) -> InstallationRecord {
    // In the operator's own WSL distro: nothing there is Setup's.
    let added = cfg.wsl_distro.is_some().then_some(crate::uninstall::Installed { postgres: false, clickhouse: false, wsl: false });
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
        kiosk: None,
        changes_done: Vec::new(),
        shortcuts: Vec::new(),
        installed: added,
    }
}

fn docker_candidates() -> Vec<(Candidate, distro::adopt::FoundStack)> {
    docker_search(&mut Vec::new())
}

/// A found stack as a candidate.
fn docker_candidate(found: &distro::adopt::FoundStack) -> Candidate {
    let in_distro = found.cfg.wsl_distro.as_ref().map(|d| format!(" in WSL distro {d}")).unwrap_or_default();
    // Windows: a stack outside any WSL distro is one only Docker Desktop's
    // Windows side shows (desktop_only).
    let desktop = cfg!(windows) && found.cfg.wsl_distro.is_none();
    let location = match &found.cfg.external {
        _ if desktop => "Docker Desktop on Windows".to_string(),
        Some(ext) => format!("compose project {} ({}){in_distro}", ext.project, ext.files.join(", ")),
        None if found.problem.is_some() => format!("Docker{in_distro}"),
        None => format!("the Platform Manager's compose file ({}){in_distro}", found.cfg.stack_dir()),
    };
    let mut notes = found.notes.clone();
    if let Some(d) = &found.cfg.wsl_distro {
        notes.push(format!(
            "Docker runs in your WSL distro {d}: the Platform Manager runs its Docker commands there and never stops, removes or sets up that distro. Keep {d} running for SENTIENT to run."
        ));
    }
    if found.cfg.external.is_some() {
        notes.push(match &found.take_over {
            Ok(()) => "It runs from a compose file the Platform Manager did not write. Changing settings or the database type needs the Platform Manager to take it over first (Repair and uninstall).".into(),
            Err(why) => format!("It runs from a compose file the Platform Manager did not write. Settings and the database type cannot be changed by the Platform Manager: {why}"),
        });
    }
    Candidate {
        record: record_for_docker(&found.cfg, found.version.clone()),
        kind: match &found.cfg.wsl_distro {
            Some(d) => format!("Docker in WSL distro {d} (container {})", found.cfg.app_container()),
            None if desktop => format!("Docker Desktop (container {})", found.cfg.app_container()),
            None => format!("Docker (container {})", found.cfg.app_container()),
        },
        location,
        running: found.running,
        notes,
        problem: found.problem.clone(),
        needs_admin: false,
        sets_admin_password: false,
    }
}

/// Every SENTIENT Docker stack here, with a line per place looked in
/// added to `looked`.
fn docker_search(looked: &mut Vec<String>) -> Vec<(Candidate, distro::adopt::FoundStack)> {
    #[cfg(windows)]
    let found = windows_docker_search(looked);
    #[cfg(not(windows))]
    let found = {
        let found = distro::adopt::find();
        looked.push(if found.is_empty() {
            match distro::hosts::docker_here() {
                distro::hosts::DockerState::Missing => "Docker on this computer: not installed".to_string(),
                distro::hosts::DockerState::NotRunning => "Docker on this computer: not running, or this account may not use it".to_string(),
                distro::hosts::DockerState::Running => "Docker on this computer: no SENTIENT".to_string(),
            }
        } else {
            format!("Docker on this computer: {}", count_text(found.len()))
        });
        found
    };
    found.into_iter().map(|f| (docker_candidate(&f), f)).collect()
}

/// Windows: the running WSL distros of this account (the Platform
/// Manager's own and Docker Desktop's left out, distro::hosts), then
/// Docker Desktop. A container seen twice (Docker Desktop's engine through
/// its WSL integration) is listed once, from the first distro (the default
/// one first).
#[cfg(windows)]
fn windows_docker_search(looked: &mut Vec<String>) -> Vec<distro::adopt::FoundStack> {
    use distro::hosts::{self, Desktop, DockerState};
    let mut out: Vec<distro::adopt::FoundStack> = Vec::new();
    fn seen(out: &[distro::adopt::FoundStack], id: &str) -> bool {
        out.iter().any(|f| distro::hosts::same_container(&f.container_id, id))
    }
    match hosts::wsl_distros() {
        Err(e) => looked.push(format!("WSL distros: none listed ({e})")),
        Ok(list) if list.is_empty() => looked.push("WSL distros: none".into()),
        Ok(list) => {
            for d in list {
                let own = distro::is_own_distro(&d.name);
                if !d.running && !own {
                    // Looking would start it, and its Docker with it (the
                    // Platform Manager's own distro it may start).
                    looked.push(format!("WSL distro {}: stopped, not looked in (start it, then look again)", d.name));
                    continue;
                }
                match hosts::docker_in(&d.name) {
                    DockerState::Missing => looked.push(format!("WSL distro {}: no Docker", d.name)),
                    DockerState::NotRunning => looked.push(format!("WSL distro {}: Docker is not running", d.name)),
                    DockerState::Running => {
                        let new: Vec<_> = distro::adopt::find_in_distro(&d.name).into_iter().filter(|f| !seen(&out, &f.container_id)).collect();
                        looked.push(format!("WSL distro {}: Docker, {}", d.name, count_text(new.len())));
                        out.extend(new);
                    }
                }
            }
        }
    }
    match hosts::docker_desktop() {
        Desktop::NoCli => looked.push("Docker Desktop: not installed".into()),
        Desktop::NotRunning => looked.push("Docker Desktop: not running".into()),
        Desktop::Running(containers) => {
            let only_here: Vec<_> = containers.into_iter().filter(|c| !seen(&out, &c.id)).collect();
            looked.push(format!("Docker Desktop: {}", if only_here.is_empty() { "no other SENTIENT".to_string() } else { count_text(only_here.len()) }));
            for c in only_here {
                out.push(desktop_only(&c));
            }
        }
    }
    out
}

/// A SENTIENT container only Docker Desktop's Windows side shows: listed,
/// not manageable until a WSL distro reaches Docker Desktop's engine.
#[cfg(windows)]
fn desktop_only(c: &distro::hosts::DesktopContainer) -> distro::adopt::FoundStack {
    let mut cfg = DeployConfig {
        stack_name: c.name.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect(),
        image: c.image.clone(),
        ..DeployConfig::default()
    };
    // The container's own name, as Docker Desktop shows it.
    let mut names = cfg.names();
    names.app = c.name.clone();
    cfg.names = Some(names);
    let why = format!(
        "SENTIENT (container {}) runs in Docker Desktop, which the Platform Manager reaches through a WSL distro. Turn on Docker Desktop's WSL integration for one (Docker Desktop, Settings, Resources, WSL integration), start that distro, then look again.",
        c.name
    );
    distro::adopt::FoundStack {
        cfg,
        version: None,
        running: c.running,
        notes: Vec::new(),
        problem: Some(why.clone()),
        take_over: Err(why),
        container_id: c.id.clone(),
    }
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
        kiosk: None,
        changes_done: Vec::new(),
        shortcuts: Vec::new(),
        // Added, not installed: its databases were there before.
        installed: Some(crate::uninstall::Installed { postgres: false, clickhouse: false, wsl: false }),
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
    search().found
}

/// [`find`], with every place looked in.
pub fn search() -> Search {
    let mut found: Vec<Candidate> = Vec::new();
    let mut looked: Vec<String> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let native = native_candidate().map(|(c, _)| c);
        looked.push(format!("systemd service sentient: {}", if native.is_some() { "found" } else { "not installed" }));
        found.extend(native);
    }
    #[cfg(windows)]
    {
        let native = windows_candidate().map(|(c, _)| c);
        looked.push(format!("Windows services (SentientServer): {}", if native.is_some() { "found" } else { "not installed" }));
        found.extend(native);
    }
    found.extend(docker_search(&mut looked).into_iter().map(|(c, _)| c));
    Search { found, looked }
}

/// Adopt the installation `wanted` describes: find it again, check the
/// Platform Manager can reach its databases, and write the record.
pub fn adopt(wanted: &Candidate, pg_admin: Option<String>, sink: ProgressFn) -> Result<InstallationRecord, String> {
    if let Some(old) = crate::installation::InstallationRecord::load() {
        // A record that no longer describes what runs here (an early
        // release's, one whose services are gone) does not stand in the way:
        // it is kept beside the new one.
        if old.platform().snapshot().installed {
            return Err("The Platform Manager already manages a SENTIENT instance on this computer.".into());
        }
        let path = crate::installation::record_path();
        let aside = path.with_extension(format!("json.replaced-{}", chrono::Utc::now().format("%Y%m%d%H%M%S")));
        match std::fs::rename(&path, &aside) {
            Ok(()) => sink(Progress::Log { line: format!("An earlier installation record that no longer matches this computer was kept as {}.", aside.display()) }),
            Err(e) => return Err(format!("An earlier installation record ({}) could not be set aside: {e}", path.display())),
        }
    }
    let candidate = if wanted.record.engine == Engine::Native {
        adopt_native(pg_admin, &sink)?
    } else {
        sink(Progress::Step { name: "Looking at the instance again".into() });
        let key = wanted.key();
        let (candidate, found) = docker_candidates()
            .into_iter()
            .find(|(c, _)| c.key() == key)
            .ok_or("That instance no longer exists.")?;
        if let Some(p) = &candidate.problem {
            return Err(p.clone());
        }
        sink(Progress::Step { name: "Checking the Platform Manager can reach its databases".into() });
        // In the distro it was found in, before any record names it.
        distro::with_docker_distro(found.cfg.wsl_distro.as_deref(), || distro::adopt::verify(&found))?;
        candidate
    };
    sink(Progress::Step { name: "Saving the installation record".into() });
    let json = serde_json::to_string_pretty(&candidate.record).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    // Everything after this in this process runs where the record says.
    distro::use_docker_distro(candidate.record.stack.wsl_distro.as_deref());
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
        Err("A native installation is not supported on this system.".into())
    }
}

/// An adopted Docker stack: let the Platform Manager's own compose file run
/// it from now on (needed before settings or the database type change).
/// `password`: the database password the operator gave (the dialog checked
/// it); else it is found in the containers and compose files (or not needed).
pub fn take_over_compose(record: InstallationRecord, compose: Option<&str>, password: Option<&str>, ch_password: Option<&str>, sink: ProgressFn) -> Result<InstallationRecord, String> {
    if record.engine != Engine::Docker || record.stack.external.is_none() {
        return Err("This installation already runs from the Platform Manager's compose file.".into());
    }
    let distro_name = record.stack.wsl_distro.clone();
    distro::with_docker_distro(distro_name.as_deref(), move || take_over_compose_here(record, compose, password, ch_password, sink))
}

/// [`take_over_compose`], its Docker commands where the stack runs.
fn take_over_compose_here(record: InstallationRecord, compose: Option<&str>, password: Option<&str>, ch_password: Option<&str>, sink: ProgressFn) -> Result<InstallationRecord, String> {
    let mut full = record.stack.clone();
    if let Some(p) = password.filter(|p| !p.is_empty()) {
        distro::adopt::check_db_password(&full, p).map_err(|e| format!("{e} Nothing was changed."))?;
        full.db_password = p.to_string();
    }
    // ClickHouse's password from the containers, when they carry one.
    if let Ok(s) = distro::docker_ops::with_secrets(&record.stack) {
        if full.db_password.is_empty() {
            full.db_password = s.db_password;
        }
        full.ch_password = s.ch_password;
    }
    if let Some(p) = ch_password {
        distro::docker_ops::check_ch_password(&full, p).map_err(|e| format!("{e} Nothing was changed."))?;
        full.ch_password = p.to_string();
    }
    let own = distro::adopt::take_over(sink, &full, compose)?;
    let mut updated = record;
    updated.stack = own;
    updated.stack.db_password.clear();
    updated.stack.ch_password.clear();
    let json = serde_json::to_string_pretty(&updated).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    Ok(updated)
}

/// What taking this installation over will do (crate::manager::repair's
/// confirmation): the compose file and env files found, what is kept.
pub fn take_over_plan(record: &InstallationRecord, compose: Option<&str>) -> Result<distro::adopt::TakeOverPlan, String> {
    if record.engine != Engine::Docker || record.stack.external.is_none() {
        return Err("This installation already runs from the Platform Manager's compose file.".into());
    }
    // The record keeps no passwords: the plan finds the database's (or
    // says it needs it given).
    let plan = distro::with_docker_distro(record.stack.wsl_distro.as_deref(), || distro::adopt::take_over_plan(&record.stack, compose).0);
    Ok(plan)
}

/// Whether PostgreSQL (or ClickHouse, `ch`) takes `password` (the takeover
/// dialog's Check).
pub fn check_takeover_password(record: &InstallationRecord, ch: bool, password: &str) -> Result<(), String> {
    distro::with_docker_distro(record.stack.wsl_distro.as_deref(), || {
        if ch { distro::docker_ops::check_ch_password(&record.stack, password) } else { distro::adopt::check_db_password(&record.stack, password) }
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn native(kind: &str, location: &str) -> Candidate {
        let mut record = native_record(&crate::installation::InstallationRecord::placeholder_native(std::path::Path::new("C:/Program Files/SENTIENT")));
        record.engine = Engine::Native;
        Candidate { record, kind: kind.into(), location: location.into(), running: true, notes: Vec::new(), problem: None, needs_admin: false, sets_admin_password: true }
    }

    #[test]
    fn the_pg_admin_marker_reads_back() {
        let c = native("Windows services (SentientServer)", "C:\\Program Files\\SENTIENT");
        let line = needs_pg_admin_line(&c);
        assert!(line.starts_with("@@NEEDS-PG-ADMIN "));
        assert_eq!(
            parse_needs_pg_admin(&line),
            Some(NeedsPgAdmin { kind: "Windows services (SentientServer)".into(), location: "C:\\Program Files\\SENTIENT".into() })
        );
        // A "|" in the kind cannot split it in the wrong place.
        let line = needs_pg_admin_line(&native("a | b", "D:\\x"));
        assert_eq!(parse_needs_pg_admin(&line).unwrap().location, "D:\\x");
        assert_eq!(parse_needs_pg_admin("@@NEEDS-PG-ADMIN"), Some(NeedsPgAdmin::default()));
        assert_eq!(parse_needs_pg_admin("@@NEEDS-PG-ADMINX y"), None);
        assert_eq!(parse_needs_pg_admin("SENTIENT was found"), None);
    }

    #[test]
    fn only_a_native_candidate_without_a_problem_waits_for_the_password() {
        let mut c = native("Windows services (SentientServer)", "C:\\SENTIENT");
        assert!(c.only_needs_pg_admin());
        c.problem = Some("no psql".into());
        assert!(!c.only_needs_pg_admin());
        let mut c = native("x", "y");
        c.sets_admin_password = false;
        assert!(!c.only_needs_pg_admin());
        let mut c = native("x", "y");
        c.record.engine = Engine::Docker;
        assert!(!c.only_needs_pg_admin());
    }
}
