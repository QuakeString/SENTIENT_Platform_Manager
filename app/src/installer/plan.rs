// Copyright © 2016-2026 The SENTIENT Authors

//! The install itself: everything the wizard collected, as plain data that
//! can cross to a background thread, and the ordered tasks that turn it into
//! a running SENTIENT. Nothing on the machine changes before [`run`].

use std::path::PathBuf;

use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
use sentient_installer_core::distro::{self, DeployConfig};
use sentient_installer_core::native::{self, ArtifactSource, NativeConfig};
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::EnvVar;
#[cfg(windows)]
use sentient_installer_core::stack::Flavor;

use crate::catalog::PG_PREFIX;
use crate::installation::{Engine, InstallationRecord};
use crate::secrets::Secret;
use crate::self_install;

pub use crate::self_install::Shortcuts;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
    /// The automatic sign-in account's Windows password. Never written to
    /// disk: a plan resumed after a restart, or run with `--job install`,
    /// takes it from SPM_AUTOSIGNIN_PASSWORD.
    #[serde(skip, default)]
    pub auto_signin_password: String,
    pub bundle: Option<PathBuf>,
    /// No package: this SENTIENT version is downloaded from the releases
    /// repository first (crate::online), and the download is the package.
    #[serde(default)]
    pub download: Option<String>,
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

/// The error `run` returns when Windows must restart to finish enabling
/// WSL2. The plan is saved first and setup continues after the next logon.
pub const RESTART_REQUIRED: &str = "Windows must restart to finish enabling WSL2.";

/// Passwords of a saved plan, kept apart from it: they go to the secret
/// store (crate::secrets), the rest to a plain file.
#[derive(serde::Serialize, serde::Deserialize)]
struct PendingSecrets {
    db: String,
    pg_admin: String,
    admin: String,
    clickhouse: String,
}

fn pending_path() -> PathBuf {
    crate::installation::record_path().with_file_name("pending-install.json")
}

/// Save the plan and arrange for setup to continue after the restart.
fn save_pending(plan: &InstallPlan) -> Result<(), String> {
    let secrets = PendingSecrets {
        db: plan.stack.db_password.clone(),
        pg_admin: plan.pg_superuser_password.clone(),
        admin: plan.admin_password.clone(),
        clickhouse: plan.stack.ch_password.clone(),
    };
    crate::secrets::save(Secret::PendingInstall, &serde_json::to_string(&secrets).map_err(|e| e.to_string())?)?;
    let mut bare = plan.clone();
    bare.stack.db_password.clear();
    bare.stack.ch_password.clear();
    bare.pg_superuser_password.clear();
    bare.admin_password.clear();
    let path = pending_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&bare).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let value = format!("\"{}\" --resume", exe.display());
        let ok = std::process::Command::new("reg.exe")
            .args([
                "add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\RunOnce",
                "/v", "SENTIENT Setup", "/t", "REG_SZ", "/d", &value, "/f",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err("could not arrange for setup to continue after the restart".into());
        }
    }
    Ok(())
}

/// The plan saved before a restart, if there is one; removed once read.
pub fn take_pending() -> Option<InstallPlan> {
    let path = pending_path();
    let mut plan: InstallPlan = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let secrets: PendingSecrets = serde_json::from_str(&crate::secrets::get(Secret::PendingInstall)?).ok()?;
    plan.stack.db_password = secrets.db;
    plan.pg_superuser_password = secrets.pg_admin;
    plan.admin_password = secrets.admin;
    plan.stack.ch_password = secrets.clickhouse;
    let _ = std::fs::remove_file(&path);
    crate::secrets::forget(Secret::PendingInstall);
    Some(plan)
}

/// Restart Windows in a few seconds.
pub fn restart_now() {
    #[cfg(windows)]
    let _ = std::process::Command::new("shutdown.exe")
        .args(["/r", "/t", "5", "/c", "Restarting to finish installing SENTIENT"])
        .spawn();
}

/// ClickHouse in Docker inside WSL2, owned by the hidden account
/// (sentient_installer_core::clickhouse_wsl). Windows only.
#[cfg(windows)]
fn install_clickhouse_wsl(plan: &InstallPlan, manager: &std::path::Path, sink: &ProgressFn) -> Result<(), String> {
    use sentient_installer_core::wsl;
    if !wsl::is_ready() {
        let outcome = wsl::install(sink.clone());
        if outcome.reboot_required {
            save_pending(plan)?;
            return Err(RESTART_REQUIRED.into());
        }
    }
    // A reinstall over kept data (the PostgreSQL cluster is there) takes
    // ClickHouse's kept history back too; a fresh install starts clean.
    let reuse = plan.data_dir.join("data").join("PG_VERSION").exists();
    provision_clickhouse_wsl(&plan.configured_stack(), manager, reuse, sink)
}

/// With WSL2 ready: the hidden account, its distro with Docker and ClickHouse
/// (`stack`'s ClickHouse settings and password), started at every boot.
/// Also used by Migration.
#[cfg(windows)]
/// `reuse_kept`: take back a distro disk an earlier install left (its
/// ClickHouse history); otherwise such a disk is kept aside and the distro
/// starts empty.
pub fn provision_clickhouse_wsl(stack: &DeployConfig, manager: &std::path::Path, reuse_kept: bool, sink: &ProgressFn) -> Result<(), String> {
    use sentient_installer_core::clickhouse_wsl;
    let password = crate::secrets::get(Secret::ClickhouseAccount).unwrap_or_else(clickhouse_wsl::new_password);
    clickhouse_wsl::ensure_account(sink, &password)?;
    crate::secrets::save(Secret::ClickhouseAccount, &password)?;

    // The account's stack: ClickHouse alone, under the reference names.
    let ch = DeployConfig { flavor: Flavor::Clickhouse, ..stack.clone() };
    let config = clickhouse_wsl::work_dir().join("config.json");
    std::fs::write(&config, serde_json::to_string(&ch).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write {}: {e}", config.display()))?;
    step(sink, "Installing WSL2, Docker and ClickHouse as the service account");
    clickhouse_wsl::run_as_account(sink, &password, manager, if reuse_kept { "provision-reuse" } else { "provision" })?;
    step(sink, "Starting ClickHouse at every boot");
    clickhouse_wsl::register_keepalive(&password)?;
    let mut answering = false;
    for _ in 0..60 {
        if distro::clickhouse_answers(ch.ch_http_port) {
            answering = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if !answering {
        return Err(format!("ClickHouse is installed but does not answer on 127.0.0.1:{}", ch.ch_http_port));
    }
    if !reuse_kept {
        set_aside_clickhouse_history(&ch, sink)?;
    }
    Ok(())
}

/// A ClickHouse that already holds SENTIENT's database (an earlier install
/// whose distro is still there) would give a new, empty PostgreSQL history
/// that belongs to devices it does not have. Rename it aside, where Kept
/// lists it for removal, and let the new install start empty.
#[cfg(windows)]
fn set_aside_clickhouse_history(ch: &DeployConfig, sink: &ProgressFn) -> Result<(), String> {
    let conn = sentient_backup_core::clickhouse::ChConn {
        url: format!("http://127.0.0.1:{}", ch.ch_http_port),
        database: "default".into(),
        user: ch.ch_user.clone(),
        password: ch.ch_password.clone(),
    };
    let exists = conn
        .text(&format!("SELECT count() FROM system.tables WHERE database = '{}'", ch.ch_db.replace('\'', "")))
        .map(|t| t.trim() != "0")
        .unwrap_or(false);
    if !exists {
        return Ok(());
    }
    let aside = format!("{}_before_install_{}", ch.ch_db, chrono::Local::now().format("%Y%m%d%H%M"));
    conn.text(&format!("RENAME DATABASE {} TO {}", ch.ch_db, aside))
        .map_err(|e| format!("set the earlier ClickHouse history aside: {e}"))?;
    sink(Progress::Log { line: format!("ClickHouse already held SENTIENT history from an earlier install; it is kept as {aside} (listed under Kept)") });
    Ok(())
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

/// Install the Platform Manager alone, without SENTIENT: the program, its
/// shortcuts and (Windows) its Apps & features entry. No installation
/// record: the manager then offers to install SENTIENT.
pub fn run_manager_only(shortcuts: Shortcuts, sink: ProgressFn) -> Result<(), String> {
    step(&sink, "Installing the Platform Manager");
    let manager = self_install::install_manager()?;
    step(&sink, "Creating shortcuts");
    self_install::create_manager_shortcuts(&manager, &shortcuts)?;
    self_install::register_manager_entry(&manager);
    sink(Progress::Done { message: "The Platform Manager is installed.".into() });
    Ok(())
}

/// Run the install. Each task reports through `sink`; the first failure stops
/// the run and is returned.
pub fn run(mut plan: InstallPlan, sink: ProgressFn) -> Result<(), String> {
    if plan.bundle.is_none() && plan.engine == Engine::Native {
        if let Some(version) = plan.download.clone() {
            let (_, dir) = crate::online::download(Some(&version), &sink)?;
            plan.bundle = Some(dir);
        }
    }
    step(&sink, "Installing the Platform Manager");
    let manager = self_install::install_manager()?;
    // ClickHouse first: SENTIENT's schema installer creates its tables there.
    #[cfg(windows)]
    if plan.stack.flavor == Flavor::Clickhouse {
        install_clickhouse_wsl(&plan, &manager, &sink)?;
    }
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
            self_install::set_autostart(&[native::APP_SERVICE, native::PG_SERVICE], plan.shortcuts.autostart);
            // Backup, restore and repair read these from the credential
            // store; a failure only means the operator is asked later.
            for (secret, value) in [
                (Secret::Database, &plan.stack.db_password),
                (Secret::PostgresAdmin, &plan.pg_superuser_password),
                (Secret::Jwt, &cfg.jwt_secret),
                (Secret::Clickhouse, &plan.stack.ch_password),
            ] {
                if value.is_empty() {
                    continue;
                }
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
    if plan.shortcuts.kiosk {
        sentient_installer_core::kiosk::create_shortcut(&sink, plan.stack.http_port, &plan.state_dir())?;
    }
    if plan.shortcuts.open_at_signin {
        sentient_installer_core::kiosk::set_open_at_signin(&sink, &manager, Some(true))?;
    }
    if !plan.shortcuts.auto_signin.is_empty() {
        let password = if plan.auto_signin_password.is_empty() {
            std::env::var("SPM_AUTOSIGNIN_PASSWORD").unwrap_or_default()
        } else {
            plan.auto_signin_password.clone()
        };
        // SENTIENT is installed by now; a refused password should not undo
        // that, it can be entered again under Control.
        if let Err(e) = sentient_installer_core::autologin::set(&sink, Some((&plan.shortcuts.auto_signin, &password))) {
            sink(Progress::Log { line: format!("Automatic sign-in was not turned on: {e} Turn it on under Control.") });
        }
    }

    // SENTIENT's Apps & features entry covers the manager installed alone before.
    self_install::remove_manager_entry();
    step(&sink, "Saving the installation record");
    let json = serde_json::to_string_pretty(&plan.record()).map_err(|e| e.to_string())?;
    self_install::write_record(&crate::installation::record_path(), &json)?;

    sink(Progress::Done { message: format!("SENTIENT is running at {url}") });
    Ok(())
}
