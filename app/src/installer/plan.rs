// Copyright © 2016-2026 The SENTIENT Authors

//! The install itself: everything the wizard collected, as plain data that
//! can cross to a background thread, and the ordered tasks that turn it into
//! a running SENTIENT. Nothing on the machine changes before [`run`].

use std::path::PathBuf;

use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
use sentient_installer_core::distro::DeployConfig;
use sentient_installer_core::native::{self, ArtifactSource, NativeConfig};
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::{EnvVar, Flavor};

use crate::catalog::PG_PREFIX;
use crate::installation::{Engine, InstallationRecord};
use crate::secrets::Secret;
use crate::self_install;

#[derive(Clone)]
pub struct Shortcuts {
    pub autostart: bool,
    pub desktop_sentient: bool,
    pub desktop_manager: bool,
    pub start_menu: bool,
    pub kiosk: bool,
}

#[derive(Clone)]
pub struct InstallPlan {
    pub sentient_version: String,
    pub install_dir: PathBuf,
    pub data_dir: PathBuf,
    pub stack: DeployConfig,
    pub pg_superuser_password: String,
    pub admin_email: String,
    pub admin_password: String,
    /// Settings with a value (recommended or typed), by variable name.
    /// `pg:` names are PostgreSQL server parameters.
    pub settings: Vec<(String, String)>,
    pub shortcuts: Shortcuts,
    pub bundle: Option<PathBuf>,
}

impl InstallPlan {
    fn native_config(&self) -> NativeConfig {
        let st = &self.stack;
        let mut tuning = st.tuning.clone();
        let mut pg = st.pg.clone();
        let mut license_server_url = st.license_server_url.clone();
        for (name, value) in &self.settings {
            match name.strip_prefix(PG_PREFIX) {
                Some("max_connections") => {
                    if let Ok(n) = value.parse() {
                        pg.pg_max_connections = n
                    }
                }
                Some("shared_buffers") => pg.pg_shared_buffers = value.clone(),
                Some("work_mem") => pg.pg_work_mem = value.clone(),
                Some("effective_cache_size") => pg.pg_effective_cache_size = value.clone(),
                Some(_) => {}
                None if name == "LICENSE_SERVER_URL" => license_server_url = value.clone(),
                None => tuning.extra_env.push(EnvVar { name: name.clone(), value: value.clone() }),
            }
        }
        NativeConfig {
            install_dir: self.install_dir.clone(),
            pg_dir: self.data_dir.clone(),
            state_dir: self.install_dir.join("state"),
            db_name: st.db_name.clone(),
            db_user: st.db_user.clone(),
            db_password: st.db_password.clone(),
            pg_superuser_password: self.pg_superuser_password.clone(),
            http_port: st.http_port,
            mqtt_port: st.mqtt_port,
            coap_port: st.coap_port,
            pg_port: st.pg_port,
            load_demo: st.load_demo,
            source: match &self.bundle {
                Some(dir) => ArtifactSource::Bundled { dir: dir.clone() },
                None => ArtifactSource::Remote { base_url: String::new() },
            },
            license_server_url,
            // Operator settings ride in `tuning.extra_env`, which the engine
            // appends last and lets win over derived values.
            extra_env: Vec::new(),
            jwt_secret: native::generate_secret(),
            flavor: st.flavor,
            tuning,
            pg,
            ch: st.ch.clone(),
            ch_db: st.ch_db.clone(),
            ch_user: st.ch_user.clone(),
            ch_password: st.ch_password.clone(),
            ch_http_port: st.ch_http_port,
            ch_native_port: st.ch_native_port,
        }
    }

    /// What is saved for the manager: the configuration without secrets.
    fn record(&self) -> InstallationRecord {
        let mut stack = self.stack.clone();
        stack.db_password.clear();
        stack.ch_password.clear();
        InstallationRecord {
            sentient_version: self.sentient_version.clone(),
            manager_version: env!("CARGO_PKG_VERSION").into(),
            engine: Engine::Native,
            install_dir: self.install_dir.clone(),
            pg_dir: self.data_dir.clone(),
            state_dir: self.install_dir.join("state"),
            stack,
            installed_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("hash the administrator password: {e}"))
}

/// Run the install. Each task reports through `sink`; the first failure stops
/// the run and is returned.
pub fn run(plan: InstallPlan, sink: ProgressFn) -> Result<(), String> {
    if plan.stack.flavor == Flavor::Clickhouse {
        return Err(
            "PostgreSQL + ClickHouse on Windows (ClickHouse in Docker inside WSL2) is the next phase \
             of this work. Choose PostgreSQL only or PostgreSQL + TimescaleDB for now."
                .into(),
        );
    }
    let cfg = plan.native_config();

    step(&sink, "Installing the Platform Manager");
    let manager = self_install::install_manager()?;

    native::setup(sink.clone(), &cfg)?;
    native::apply_pg_tuning(&sink, &cfg)?;
    native::deploy(sink.clone(), &cfg)?;

    step(&sink, "Setting the administrator sign-in");
    native::set_admin_account(&cfg, &plan.admin_email, &hash_password(&plan.admin_password)?)?;

    step(&sink, "Creating shortcuts");
    let url = format!("http://localhost:{}", plan.stack.http_port);
    let s = &plan.shortcuts;
    if s.start_menu {
        self_install::shortcut(&manager, self_install::start_menu(), self_install::PRODUCT, "--manager")?;
        self_install::web_shortcut(self_install::start_menu(), "SENTIENT", &url)?;
    }
    if s.desktop_manager {
        self_install::shortcut(&manager, self_install::desktop(), self_install::PRODUCT, "--manager")?;
    }
    if s.desktop_sentient {
        self_install::web_shortcut(self_install::desktop(), "SENTIENT", &url)?;
    }
    if s.kiosk {
        sentient_installer_core::kiosk::create_shortcut(&sink, plan.stack.http_port, &cfg.state_dir)?;
    }
    self_install::set_service_start(native::APP_SERVICE, s.autostart);
    self_install::set_service_start(native::PG_SERVICE, s.autostart);

    // Backup, restore and repair read these from the credential store; a
    // failure only means the operator is asked for them later.
    for (secret, value) in [
        (Secret::Database, &plan.stack.db_password),
        (Secret::PostgresAdmin, &plan.pg_superuser_password),
        (Secret::Jwt, &cfg.jwt_secret),
    ] {
        if let Err(e) = crate::secrets::save(secret, value) {
            sink(Progress::Log { line: format!("note: {e}") });
        }
    }

    step(&sink, "Saving the installation record");
    let path = crate::installation::record_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(&plan.record()).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;

    sink(Progress::Done { message: format!("SENTIENT is running at {url}") });
    Ok(())
}
