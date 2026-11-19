// Copyright © 2016-2026 The SENTIENT Authors

//! What is installed on this machine, and the engine that manages it.
//!
//! The installer writes an [`InstallationRecord`] when it finishes
//! (`mode::installation_record()`); everything the manager does afterwards
//! starts from it. The record holds no passwords: database credentials live
//! in the service configuration the installer wrote, and nothing the manager
//! does to a running installation (status, control, logs, update) needs them.

use std::path::PathBuf;

use sentient_installer_core::distro::{self, DeployConfig};
use sentient_installer_core::native::{self, ArtifactSource, NativeConfig};
use sentient_installer_core::progress::ProgressFn;
use sentient_installer_core::stack::Flavor;
use serde::{Deserialize, Serialize};

/// How SENTIENT runs on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// SENTIENT and PostgreSQL as Windows services (or systemd units on Linux).
    Native,
    /// The compose stack under Docker (inside WSL2 on Windows).
    Docker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationRecord {
    /// SENTIENT version installed, e.g. `4.3.0.24`.
    pub sentient_version: String,
    /// Platform Manager version that wrote the record.
    pub manager_version: String,
    pub engine: Engine,
    pub install_dir: PathBuf,
    pub pg_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Everything chosen in the wizard (database, ports, tuning), with the
    /// password fields emptied before saving.
    pub stack: DeployConfig,
    /// RFC 3339.
    pub installed_at: String,
    /// Whether the kiosk shortcut was chosen at install: "Open SENTIENT"
    /// opens the same full-screen window, else the normal browser. None in
    /// records from before the choice was kept.
    #[serde(default)]
    pub kiosk: Option<bool>,
    /// The installation changes (crate::changes) already made here. Records
    /// from before the list start with none, so a newer Platform Manager
    /// offers what earlier ones did not set up.
    #[serde(default)]
    pub changes_done: Vec<String>,
    /// Every file made for shortcuts and for starting at sign-in
    /// (crate::shortcuts), so uninstall removes exactly those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shortcuts: Vec<crate::shortcuts::Entry>,
    /// What Setup itself put on this computer (PostgreSQL, ClickHouse,
    /// WSL) as opposed to what it found there, recorded at install time:
    /// uninstall removes only that (crate::uninstall). None in records
    /// from before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<crate::uninstall::Installed>,
}

impl InstallationRecord {
    /// The record on this machine, if SENTIENT is installed.
    /// `SPM_INSTALLATION_RECORD` points somewhere else (development).
    ///
    /// Also points this process's Docker commands at the WSL distro the
    /// record names (`stack.wsl_distro`, a stack found in the operator's
    /// own distro), or back at the Platform Manager's own: every screen and
    /// job starts from the record, so each Docker job runs where its
    /// installation is.
    pub fn load() -> Option<Self> {
        let record: Option<Self> = std::fs::read_to_string(record_path()).ok().and_then(|text| serde_json::from_str(&text).ok());
        distro::use_docker_distro(record.as_ref().and_then(|r| r.stack.wsl_distro.as_deref()));
        record
    }

    /// Point the backup engine at the pg_dump and pg_restore that match this
    /// installation's PostgreSQL (native Windows has no client tools on PATH;
    /// Docker's are in its container). Called once at start-up, before any
    /// job thread exists.
    pub fn use_pg_tools(&self) {
        if cfg!(windows) && self.engine == Engine::Native {
            let bin = self.pg_dir.join("bin");
            std::env::set_var("SBR_PG_DUMP", bin.join("pg_dump.exe"));
            std::env::set_var("SBR_PG_RESTORE", bin.join("pg_restore.exe"));
        }
        // Docker on Linux: the tools inside the PostgreSQL container, which
        // match its server; the host may have none.
        if !cfg!(windows) && self.engine == Engine::Docker {
            if let Ok((dump, restore)) = distro::docker_ops::pg_tool_wrappers(&self.stack) {
                std::env::set_var("SBR_PG_DUMP", dump);
                std::env::set_var("SBR_PG_RESTORE", restore);
            }
        }
    }

    pub fn flavor(&self) -> Flavor {
        self.stack.flavor
    }

    /// Whether "Open SENTIENT" opens it full screen: as chosen at install;
    /// for an install from before the choice was recorded, whether its kiosk
    /// shortcut is there. Not chosen: the normal browser.
    pub fn opens_as_kiosk(&self) -> bool {
        self.kiosk.unwrap_or_else(sentient_installer_core::kiosk::shortcut_exists)
    }

    pub fn url(&self) -> String {
        format!("http://localhost:{}", self.stack.http_port)
    }

    pub fn platform(&self) -> Platform {
        match self.engine {
            Engine::Docker => Platform::Docker(self.stack.clone()),
            Engine::Native => Platform::Native(Box::new(self.native_config())),
        }
    }

    /// The native configuration with the secrets from the secret store,
    /// for operations that rewrite the service (repair).
    pub fn native_config_with_secrets(&self) -> Result<NativeConfig, String> {
        use crate::secrets::{Secret, get};
        let mut cfg = self.native_config();
        // Linux keeps them only in SENTIENT's environment file (root-only).
        #[cfg(target_os = "linux")]
        {
            let s = sentient_installer_core::native_linux::read_secrets(&cfg)?;
            cfg.db_password = s.db_password;
            cfg.jwt_secret = s.jwt_secret;
            cfg.ch_password = s.ch_password;
            return Ok(cfg);
        }
        #[allow(unreachable_code)]
        let missing = |what: &str| tf!("The {what} is not in the secret store (%ProgramData%\\SENTIENT\\secrets); this cannot continue without it.");
        cfg.db_password = get(Secret::Database).ok_or_else(|| missing("database password"))?;
        cfg.pg_superuser_password = get(Secret::PostgresAdmin).ok_or_else(|| missing(n!("PostgreSQL admin password")))?;
        cfg.jwt_secret = get(Secret::Jwt).ok_or_else(|| missing("sign-in signing secret"))?;
        if cfg.flavor == Flavor::Clickhouse {
            cfg.ch_password = get(Secret::Clickhouse).ok_or_else(|| missing(n!("ClickHouse password")))?;
        }
        Ok(cfg)
    }

    /// A native configuration with nothing known but where it is installed
    /// (an installation found before its settings could be read).
    pub fn placeholder_native(install_dir: &std::path::Path) -> NativeConfig {
        let record = InstallationRecord {
            sentient_version: String::new(),
            manager_version: String::new(),
            engine: Engine::Native,
            install_dir: install_dir.to_path_buf(),
            pg_dir: PathBuf::new(),
            state_dir: PathBuf::from("/var/lib/sentient"),
            stack: DeployConfig::default(),
            installed_at: String::new(),
            kiosk: None,
            changes_done: Vec::new(),
            shortcuts: Vec::new(),
            installed: None,
        };
        record.native_config()
    }

    pub fn native_config(&self) -> NativeConfig {
        let st = &self.stack;
        NativeConfig {
            install_dir: self.install_dir.clone(),
            pg_dir: self.pg_dir.clone(),
            state_dir: self.state_dir.clone(),
            db_name: st.db_name.clone(),
            db_user: st.db_user.clone(),
            db_password: String::new(),
            pg_superuser_password: String::new(),
            http_port: st.http_port,
            mqtt_port: st.mqtt_port,
            coap_port: st.coap_port,
            pg_port: st.pg_port,
            load_demo: false,
            source: ArtifactSource::Remote { base_url: String::new() },
            license_server_url: st.license_server_url.clone(),
            extra_env: Vec::new(),
            jwt_secret: String::new(),
            flavor: st.flavor,
            tuning: st.tuning.clone(),
            pg: st.pg.clone(),
            ch: st.ch.clone(),
            ch_db: st.ch_db.clone(),
            ch_user: st.ch_user.clone(),
            ch_password: String::new(),
            ch_http_port: st.ch_http_port,
            ch_native_port: st.ch_native_port,
            old_pg: Default::default(),
        }
    }
}

/// An adopted Docker stack whose PostgreSQL has no port on this machine.
pub const PG_NOT_PUBLISHED: &str = n!("This installation's PostgreSQL is not published on this machine (its compose file has no ports for it), \
so the Platform Manager cannot connect to it for this. Publish its port 5432 in the compose file, then add the instance again.");

pub fn record_path() -> PathBuf {
    std::env::var_os("SPM_INSTALLATION_RECORD")
        .map(PathBuf::from)
        .unwrap_or_else(crate::mode::installation_record)
}

/// The SENTIENT version a Docker install's container runs (its
/// SENTIENT_BUILD_VERSION, else its numbered image label); None for a
/// native install or when the container does not say. Blocking: asks
/// Docker. The image tag alone says nothing (`latest`, or a tag an older
/// update pointed at another release).
pub fn running_version(record: &InstallationRecord) -> Option<String> {
    if record.engine != Engine::Docker {
        return None;
    }
    // Renders: the version a Docker record's container runs.
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("SPM_DEBUG_RUNNING_VERSION") {
        return Some(v).filter(|v| !v.is_empty());
    }
    sentient_installer_core::distro::docker_ops::running_version(&record.stack)
}

/// The version updates compare with: what runs, else what was recorded.
pub fn comparable_version(record: &InstallationRecord) -> String {
    running_version(record).unwrap_or_else(|| record.sentient_version.clone())
}

/// SENTIENT's version as shown: the version that runs, and the image tag in
/// brackets only when it is not that version: "4.4.0.3", "4.4.0.3 (image
/// tag latest)", "version unknown (image tag latest)". `recorded` stands in
/// for `running` when it is a release number.
pub fn version_text(running: Option<&str>, recorded: &str, tag: &str) -> String {
    let known = running.map(String::from).or_else(|| sentient_installer_core::releases::is_numbered(recorded).then(|| recorded.to_string()));
    match known {
        Some(v) if v == tag => v,
        Some(v) => tf!("{v} (image tag {tag})"),
        None => tf!("version unknown (image tag {tag})"),
    }
}

pub fn flavor_label(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Stock => "PostgreSQL",
        Flavor::Timescaledb => t!("PostgreSQL + TimescaleDB"),
        Flavor::Clickhouse => t!("PostgreSQL + ClickHouse"),
    }
}

/// One managed service or container, the same shape for both engines.
#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub running: bool,
    /// Not running yet but on its way (a service that starts with Windows,
    /// just after a restart).
    pub starting: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub installed: bool,
    /// SENTIENT answers on its HTTP port.
    pub serving: bool,
    pub services: Vec<Service>,
}

/// The engine behind an installation. Every method blocks: call them from a
/// background task, never from render.
#[derive(Clone)]
pub enum Platform {
    Native(Box<NativeConfig>),
    Docker(DeployConfig),
}

impl Platform {
    pub fn snapshot(&self) -> Snapshot {
        // Screens: canned services, "serving;name=running,name=stopped".
        #[cfg(debug_assertions)]
        if let Some(v) = std::env::var_os("SPM_DEBUG_SNAPSHOT") {
            let v = v.to_string_lossy().to_string();
            let (serving, list) = v.split_once(';').unwrap_or(("false", ""));
            let services = list
                .split(',')
                .filter_map(|e| e.split_once('='))
                .map(|(n, st)| Service { name: n.into(), running: st == "running", starting: st == "starting", detail: if st == "running" { t!("Running").into() } else { t!("Stopped").into() } })
                .collect();
            return Snapshot { installed: true, serving: serving == "true", services };
        }
        match self {
            Platform::Native(cfg) => {
                let st = native::status(cfg);
                let mut services: Vec<Service> = st
                    .services
                    .into_iter()
                    .map(|s| Service {
                        running: s.state == "running",
                        starting: s.state == "start_pending",
                        name: s.name,
                        detail: s.status,
                    })
                    .collect();
                // Windows hybrid: ClickHouse lives in WSL2 under its own
                // account; its health is whether it answers.
                if cfg!(windows) && cfg.flavor == Flavor::Clickhouse {
                    let up = distro::clickhouse_answers(cfg.ch_http_port);
                    services.push(Service {
                        name: t!("ClickHouse (WSL2)").into(),
                        running: up,
                        starting: false,
                        detail: if up {
                            tf!("Answering on 127.0.0.1:{}", cfg.ch_http_port)
                        } else {
                            t!("Not answering. It starts about half a minute after boot").into()
                        },
                    });
                }
                Snapshot { installed: st.installed, serving: st.running, services }
            }
            Platform::Docker(cfg) => {
                let st = distro::status(cfg);
                Snapshot {
                    installed: st.installed,
                    serving: st.running,
                    services: st
                        .containers
                        .into_iter()
                        .map(|c| Service {
                            running: c.state == "running",
                            starting: false,
                            name: c.name,
                            detail: c.status,
                        })
                        .collect(),
                }
            }
        }
    }

    /// `start`, `stop` or `restart`.
    pub fn control(&self, sink: ProgressFn, action: &str) -> Result<(), String> {
        match self {
            Platform::Native(cfg) => native::control(sink, cfg, action),
            Platform::Docker(cfg) => distro::control(sink, cfg, action),
        }
    }

    /// `start`, `stop` or `restart` the whole stack in order: SENTIENT,
    /// ClickHouse, PostgreSQL down, the reverse up with a wait for each
    /// (sentient_installer_core::stack_order). A PostgreSQL that holds other
    /// databases stays running unless `stop_shared_pg`.
    pub fn control_stack(&self, sink: ProgressFn, action: &str, stop_shared_pg: bool) -> Result<(), String> {
        match self {
            Platform::Native(cfg) => native::control_stack(sink, &with_db_password(cfg), action, stop_shared_pg),
            Platform::Docker(cfg) => distro::control_stack(sink, cfg, action),
        }
    }

    /// The databases that keep a shared PostgreSQL running when the stack
    /// stops (native Linux; none elsewhere). May ask for the administrator
    /// password.
    pub fn pg_other_databases(&self) -> Result<Vec<String>, String> {
        #[cfg(debug_assertions)]
        if let Some(v) = std::env::var_os("SPM_DEBUG_PG_OTHERS") {
            let v = v.to_string_lossy().to_string();
            if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }
            return match v.strip_prefix("error:") {
                Some(e) => Err(e.to_string()),
                None => Ok(v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()),
            };
        }
        match self {
            Platform::Native(cfg) => native::pg_other_databases(&with_db_password(cfg)),
            Platform::Docker(_) => Ok(Vec::new()),
        }
    }

    /// Start, stop or restart one of the install's own services, by the
    /// name the snapshot lists (anything else is refused).
    pub fn service_control(&self, sink: ProgressFn, name: &str, action: &str) -> Result<(), String> {
        if !self.snapshot().services.iter().any(|s| s.name == name) {
            return Err(nf!("{name} is not one of this installation's services."));
        }
        match self {
            Platform::Native(_) => native::service_control(sink, name, action),
            Platform::Docker(_) => distro::service_control(sink, name, action),
        }
    }

    pub fn logs(&self, tail: u32) -> String {
        match self {
            Platform::Native(cfg) => native::logs(cfg, tail),
            Platform::Docker(cfg) => distro::logs(cfg, tail),
        }
    }

    /// Native installs update from a package folder; Docker pulls the image.
    pub fn needs_package(&self) -> bool {
        matches!(self, Platform::Native(_))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A record for tests.
    pub fn sample() -> InstallationRecord {
        serde_json::from_str(r#"{"sentient_version":"4.4.0.2","manager_version":"0.8.0-rc11","engine":"native","install_dir":"C:/SENTIENT","pg_dir":"C:/SENTIENT/pg","state_dir":"C:/ProgramData/SENTIENT","stack":{},"installed_at":"2026-11-09T10:00:00Z"}"#).unwrap()
    }

    #[test]
    fn the_tag_shows_only_when_it_is_not_the_version() {
        assert_eq!(version_text(Some("4.4.0.3"), "latest", "latest"), "4.4.0.3 (image tag latest)");
        assert_eq!(version_text(Some("4.4.0.3"), "4.4.0.3", "4.4.0.3"), "4.4.0.3");
        // An older update ran 4.4.0.3 under the tag 4.4.0.2.
        assert_eq!(version_text(Some("4.4.0.3"), "4.4.0.3", "4.4.0.2"), "4.4.0.3 (image tag 4.4.0.2)");
        // The container does not say: the recorded release, else unknown.
        assert_eq!(version_text(None, "4.4.0.1", "4.4.0.1"), "4.4.0.1");
        assert_eq!(version_text(None, "4.4.0.1", "latest"), "4.4.0.1 (image tag latest)");
        assert_eq!(version_text(None, "latest", "latest"), "version unknown (image tag latest)");
        assert_eq!(version_text(None, "unknown", "dev"), "version unknown (image tag dev)");
    }

    #[test]
    fn updates_compare_with_what_runs() {
        use sentient_installer_core::releases::{Move, movement};
        let mut r = sample();
        r.engine = Engine::Docker;
        r.sentient_version = "latest".into();
        r.stack.image = "quakestring/sentient:latest".into();
        // "latest" alone reads as 0.0.0.0: everything looks newer.
        assert_eq!(movement("4.4.0.2", &r.sentient_version), Move::Newer);
        // What the container says (the renders' hook stands in for Docker).
        std::env::set_var("SPM_DEBUG_RUNNING_VERSION", "4.4.0.2");
        let installed = comparable_version(&r);
        std::env::remove_var("SPM_DEBUG_RUNNING_VERSION");
        assert_eq!(installed, "4.4.0.2");
        assert_eq!(movement("4.4.0.3", &installed), Move::Newer);
        assert_eq!(movement("4.4.0.2", &installed), Move::Same);
        assert_eq!(movement("4.4.0.1", &installed), Move::Older);
        // A native record is what it recorded.
        assert_eq!(comparable_version(&sample()), "4.4.0.2");
    }

    #[test]
    fn records_from_before_the_kiosk_choice_still_load() {
        let old = r#"{"sentient_version":"4.3.0.58","manager_version":"0.8.0","engine":"native","install_dir":"C:/SENTIENT","pg_dir":"C:/SENTIENT/pg","state_dir":"C:/ProgramData/SENTIENT","stack":{},"installed_at":"2026-11-09T10:00:00Z"}"#;
        let r: InstallationRecord = serde_json::from_str(old).unwrap();
        assert_eq!(r.kiosk, None);
        for chosen in [true, false] {
            let mut r2 = r.clone();
            r2.kiosk = Some(chosen);
            let back: InstallationRecord = serde_json::from_str(&serde_json::to_string(&r2).unwrap()).unwrap();
            assert_eq!(back.kiosk, Some(chosen));
            // A recorded choice wins over whatever shortcut is on the computer.
            assert_eq!(back.opens_as_kiosk(), chosen);
        }
    }

    #[test]
    fn a_docker_record_keeps_the_wsl_distro_it_was_found_in() {
        // 0.9.0's Docker record: no distro, i.e. the Platform Manager's own.
        let old = r#"{"sentient_version":"4.4.0.1","manager_version":"0.9.0","engine":"docker","install_dir":"/opt/sentient","pg_dir":"","state_dir":"/opt/sentient","stack":{"external":{"project":"sentient","files":["/home/op/sentient/docker-compose.yml"],"working_dir":"/home/op/sentient"}},"installed_at":"2026-11-09T10:00:00Z"}"#;
        let r: InstallationRecord = serde_json::from_str(old).unwrap();
        assert_eq!(r.stack.wsl_distro, None);
        assert!(!serde_json::to_string(&r).unwrap().contains("wsl_distro"));
        let mut found = r.clone();
        found.stack.wsl_distro = Some("Ubuntu".into());
        let back: InstallationRecord = serde_json::from_str(&serde_json::to_string(&found).unwrap()).unwrap();
        assert_eq!(back.stack.wsl_distro.as_deref(), Some("Ubuntu"));
        assert_eq!(back.stack.external, r.stack.external);
    }
}

/// Windows keeps the database password in the secret store, not in the
/// record: listing PostgreSQL's databases (as SENTIENT's own user) needs it.
/// Elsewhere the configuration is used as it is.
fn with_db_password(cfg: &NativeConfig) -> NativeConfig {
    #[allow(unused_mut)]
    let mut c = cfg.clone();
    #[cfg(windows)]
    if c.db_password.is_empty() {
        if let Some(p) = crate::secrets::get(crate::secrets::Secret::Database) {
            c.db_password = p;
        }
    }
    c
}
