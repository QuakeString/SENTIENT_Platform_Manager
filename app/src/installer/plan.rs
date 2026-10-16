// Copyright © 2016-2026 The SENTIENT Authors

//! The install itself: everything the wizard collected, as plain data that
//! can cross to a background thread, and the ordered tasks that turn it into
//! a running SENTIENT. Nothing on the machine changes before [`run`].

use std::path::PathBuf;

use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
use sentient_installer_core::distro::{self, DeployConfig};
use sentient_installer_core::native::{self, ArtifactSource, NativeConfig};
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::{EnvVar, Flavor};

use crate::catalog::PG_PREFIX;
use crate::installation::{Engine, InstallationRecord};
use crate::secrets::Secret;
use crate::self_install;

pub use crate::self_install::Shortcuts;

#[derive(Clone)]
pub struct InstallPlan {
    /// Native services, or (Linux only) the Docker stack.
    pub engine: Engine,
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
    /// The wizard's configuration with the chosen settings folded in:
    /// PostgreSQL parameters into `pg`, the licence server into its field,
    /// everything else into `tuning.extra_env`, which every engine writes
    /// last so it wins over derived values.
    fn configured_stack(&self) -> DeployConfig {
        let mut st = self.stack.clone();
        for (name, value) in &self.settings {
            match name.strip_prefix(PG_PREFIX) {
                Some("max_connections") => {
                    if let Ok(n) = value.parse() {
                        st.pg.pg_max_connections = n
                    }
                }
                Some("shared_buffers") => st.pg.pg_shared_buffers = value.clone(),
                Some("work_mem") => st.pg.pg_work_mem = value.clone(),
                Some("effective_cache_size") => st.pg.pg_effective_cache_size = value.clone(),
                Some(_) => {}
                None if name == "LICENSE_SERVER_URL" => st.license_server_url = value.clone(),
                None => st.tuning.extra_env.push(EnvVar { name: name.clone(), value: value.clone() }),
            }
        }
        st
    }

    fn state_dir(&self) -> PathBuf {
        if cfg!(windows) { self.install_dir.join("state") } else { PathBuf::from("/var/lib/sentient") }
    }

    fn native_config(&self) -> NativeConfig {
        let st = self.configured_stack();
        let (tuning, pg, license_server_url) = (st.tuning.clone(), st.pg.clone(), st.license_server_url.clone());
        NativeConfig {
            install_dir: self.install_dir.clone(),
            pg_dir: self.data_dir.clone(),
            state_dir: self.state_dir(),
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
            engine: self.engine,
            install_dir: self.install_dir.clone(),
            pg_dir: self.data_dir.clone(),
            state_dir: self.state_dir(),
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
    if cfg!(windows) && plan.stack.flavor == Flavor::Clickhouse {
        return Err(
            "PostgreSQL + ClickHouse on Windows (ClickHouse in Docker inside WSL2) is the next phase \
             of this work. Choose PostgreSQL only or PostgreSQL + TimescaleDB for now."
                .into(),
        );
    }

    step(&sink, "Installing the Platform Manager");
    let manager = self_install::install_manager()?;
    let admin_hash = hash_password(&plan.admin_password)?;

    match plan.engine {
        Engine::Native => {
            let cfg = plan.native_config();
            native::setup(sink.clone(), &cfg)?;
            // Windows applies the PostgreSQL parameters here; on Linux
            // native::setup already wrote them with the cluster.
            if cfg!(windows) {
                native::apply_pg_tuning(&sink, &cfg)?;
            }
            native::deploy(sink.clone(), &cfg)?;
            step(&sink, "Setting the administrator sign-in");
            native::set_admin_account(&cfg, &plan.admin_email, &admin_hash)?;
            if plan.shortcuts.kiosk {
                sentient_installer_core::kiosk::create_shortcut(&sink, plan.stack.http_port, &cfg.state_dir)?;
            }
            self_install::set_autostart(&[native::APP_SERVICE, native::PG_SERVICE], plan.shortcuts.autostart);
            // Backup, restore and repair read these from the credential
            // store; a failure only means the operator is asked later.
            for (secret, value) in [
                (Secret::Database, &plan.stack.db_password),
                (Secret::PostgresAdmin, &plan.pg_superuser_password),
                (Secret::Jwt, &cfg.jwt_secret),
            ] {
                if let Err(e) = crate::secrets::save(secret, value) {
                    sink(Progress::Log { line: format!("note: {e}") });
                }
            }
        }
        Engine::Docker => {
            let stack = plan.configured_stack();
            distro::setup(sink.clone(), &plan.install_dir)?;
            distro::deploy(sink.clone(), &stack)?;
            step(&sink, "Setting the administrator sign-in");
            distro::set_admin_account(&stack, &plan.admin_email, &admin_hash)?;
        }
    }

    step(&sink, "Creating shortcuts");
    let url = format!("http://localhost:{}", plan.stack.http_port);
    self_install::create_shortcuts(&manager, &url, &plan.shortcuts)?;

    step(&sink, "Saving the installation record");
    let json = serde_json::to_string_pretty(&plan.record()).map_err(|e| e.to_string())?;
    self_install::write_record(&crate::installation::record_path(), &json)?;

    sink(Progress::Done { message: format!("SENTIENT is running at {url}") });
    Ok(())
}
