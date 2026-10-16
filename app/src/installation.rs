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
}

impl InstallationRecord {
    /// The record on this machine, if SENTIENT is installed.
    /// `SPM_INSTALLATION_RECORD` points somewhere else (development).
    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(record_path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Point the backup engine at the pg_dump and pg_restore beside this
    /// installation's PostgreSQL (native Windows has no client tools on
    /// PATH). Called once at start-up, before any job thread exists.
    pub fn use_pg_tools(&self) {
        if cfg!(windows) && self.engine == Engine::Native {
            let bin = self.pg_dir.join("bin");
            std::env::set_var("SBR_PG_DUMP", bin.join("pg_dump.exe"));
            std::env::set_var("SBR_PG_RESTORE", bin.join("pg_restore.exe"));
        }
    }

    pub fn flavor(&self) -> Flavor {
        self.stack.flavor
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
        let missing = |what: &str| format!("The {what} is not in the secret store (%ProgramData%\\SENTIENT\\secrets); this cannot continue without it.");
        let mut cfg = self.native_config();
        cfg.db_password = get(Secret::Database).ok_or_else(|| missing("database password"))?;
        cfg.pg_superuser_password = get(Secret::PostgresAdmin).ok_or_else(|| missing("PostgreSQL admin password"))?;
        cfg.jwt_secret = get(Secret::Jwt).ok_or_else(|| missing("sign-in signing secret"))?;
        if cfg.flavor == Flavor::Clickhouse {
            cfg.ch_password = get(Secret::Clickhouse).ok_or_else(|| missing("ClickHouse password"))?;
        }
        Ok(cfg)
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
        }
    }
}

pub fn record_path() -> PathBuf {
    std::env::var_os("SPM_INSTALLATION_RECORD")
        .map(PathBuf::from)
        .unwrap_or_else(crate::mode::installation_record)
}

pub fn flavor_label(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Stock => "PostgreSQL",
        Flavor::Timescaledb => "PostgreSQL + TimescaleDB",
        Flavor::Clickhouse => "PostgreSQL + ClickHouse",
    }
}

/// One managed service or container, the same shape for both engines.
#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub running: bool,
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
        match self {
            Platform::Native(cfg) => {
                let st = native::status(cfg);
                let mut services: Vec<Service> = st
                    .services
                    .into_iter()
                    .map(|s| Service {
                        running: s.state == "running",
                        name: s.name,
                        detail: s.status,
                    })
                    .collect();
                // Windows hybrid: ClickHouse lives in WSL2 under its own
                // account; its health is whether it answers.
                if cfg!(windows) && cfg.flavor == Flavor::Clickhouse {
                    let up = distro::clickhouse_answers(cfg.ch_http_port);
                    services.push(Service {
                        name: "ClickHouse (WSL2)".into(),
                        running: up,
                        detail: if up {
                            format!("Answering on 127.0.0.1:{}", cfg.ch_http_port)
                        } else {
                            "Not answering — it starts about half a minute after boot".into()
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
