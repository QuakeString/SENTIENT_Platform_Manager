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
    /// A database an earlier install left: keep it (`Some(true)`, checked
    /// against this mode) or set it aside and start fresh (`Some(false)`).
    /// None when there is none.
    #[serde(default)]
    pub existing: Option<bool>,
    /// Start fresh by deleting that database (and the ClickHouse history)
    /// instead of setting it aside.
    #[serde(default)]
    pub delete_existing: bool,
    /// Linux native: a PostgreSQL data folder from an older major, upgraded
    /// or kept aside (sentient_installer_core::old_pg).
    #[serde(default)]
    pub old_pg: sentient_installer_core::old_pg::OldPgChoice,
    /// Windows restarted once already to turn WSL 2 on: a WSL that still
    /// does not run is an error now, not another restart.
    #[serde(default)]
    pub wsl_restarted: bool,
    /// Installed on a server by a Platform Manager over SSH (crate::remote::
    /// install): the agent that runs it is the server's Platform Manager
    /// already, and a server has no desktop, so neither is put in place.
    #[serde(default)]
    pub remote: bool,
}

/// The passwords of a plan, apart from it: a plan sent to a server goes
/// as a file without them, they go on the agent's standard input
/// (crate::remote::install).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InstallSecrets {
    pub admin: String,
    pub db: String,
    pub pg_admin: String,
    #[serde(default)]
    pub clickhouse: String,
}

/// Never printed: a log line or a panic message must not carry them.
impl std::fmt::Debug for InstallSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InstallSecrets { .. }")
    }
}

/// The image a new Docker install runs: `image` at the tag of `version`
/// (the release being installed), so it stays on that release; `image` as
/// it is when no release number is known.
pub fn pinned_image(image: &str, version: Option<&str>) -> String {
    match version.map(str::trim).filter(|v| sentient_installer_core::releases::is_numbered(v)) {
        Some(v) => crate::maintenance_ops::retag(image, v),
        None => image.to_string(),
    }
}

impl InstallPlan {
    /// The plan without its passwords, and the passwords.
    pub fn split_secrets(mut self) -> (InstallPlan, InstallSecrets) {
        let secrets = InstallSecrets {
            admin: std::mem::take(&mut self.admin_password),
            db: std::mem::take(&mut self.stack.db_password),
            pg_admin: std::mem::take(&mut self.pg_superuser_password),
            clickhouse: std::mem::take(&mut self.stack.ch_password),
        };
        self.auto_signin_password.clear();
        (self, secrets)
    }

    /// The passwords [`split_secrets`](Self::split_secrets) took, put back.
    pub fn with_secrets(mut self, s: InstallSecrets) -> InstallPlan {
        self.admin_password = s.admin;
        self.stack.db_password = s.db;
        self.pg_superuser_password = s.pg_admin;
        self.stack.ch_password = s.clickhouse;
        self
    }

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
            old_pg: self.old_pg,
        }
    }

    /// What is saved for the manager: the configuration without secrets.
    fn record(&self) -> InstallationRecord {
        // The configuration as installed, the chosen settings in it (PG
        // tuning, licence server, extra variables): Settings shows them and
        // Apply, Repair and an update keep them (audit I1).
        let mut stack = self.configured_stack();
        stack.db_password.clear();
        stack.ch_password.clear();
        InstallationRecord {
            sentient_version: self.sentient_version.clone(),
            manager_version: crate::updates::manager_version().into(),
            engine: self.engine,
            install_dir: self.install_dir.clone(),
            pg_dir: self.data_dir.clone(),
            state_dir: self.state_dir(),
            stack,
            installed_at: chrono::Utc::now().to_rfc3339(),
            kiosk: Some(self.shortcuts.kiosk),
            changes_done: crate::changes::all_ids(),
            shortcuts: crate::shortcuts::noted(),
            installed: crate::uninstall::noted(),
        }
    }
}

/// The error `run` returns when Windows must restart to finish enabling
/// WSL2. The plan is saved first and setup continues after the next logon.
pub const RESTART_REQUIRED: &str = "Windows must restart to finish turning on WSL 2.";

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
        // This program's own path. In the Microsoft Store build that is the
        // helper copy in ProgramData (crate::store), never the package's
        // program: the elevated window always runs from the helper, outside
        // the package, so this entry reaches the real registry and names a
        // path that stays valid across Store updates.
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

fn pending_progress_path() -> PathBuf {
    crate::installation::record_path().with_file_name("pending-install-progress")
}

/// How far the install's bar had got when it stopped for the restart, so
/// the run after it starts there instead of going back.
pub fn save_pending_progress(percent: f32) {
    if let Some(dir) = pending_progress_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(pending_progress_path(), format!("{percent:.1}"));
}

/// [`save_pending_progress`]'s value; 0 without one. It stays until the
/// plan is done with ([`discard_pending`]), so a continued run that stops
/// again starts there once more.
pub fn take_pending_progress() -> f32 {
    std::fs::read_to_string(pending_progress_path()).ok().and_then(|s| s.trim().parse::<f32>().ok()).unwrap_or(0.)
}

/// The file a Setup continuing the saved plan writes its process id to:
/// while it runs, the plan is not offered again; after it stopped without
/// finishing, it is offered, but not opened by itself again.
fn pending_started_path() -> PathBuf {
    crate::installation::record_path().with_file_name("pending-install.started")
}

/// The plan saved before a restart, if there is one, with its passwords.
/// It is not removed here: a run that is declined, cancelled or fails
/// keeps it, so the install can still be continued from the Platform
/// Manager. A finished install, or a new one started from the beginning,
/// removes it ([`discard_pending`]).
pub fn take_pending() -> Option<InstallPlan> {
    let path = pending_path();
    let mut plan: InstallPlan = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let secrets: PendingSecrets = serde_json::from_str(&crate::secrets::get(Secret::PendingInstall)?).ok()?;
    plan.stack.db_password = secrets.db;
    plan.pg_superuser_password = secrets.pg_admin;
    plan.admin_password = secrets.admin;
    plan.stack.ch_password = secrets.clickhouse;
    let _ = std::fs::write(pending_started_path(), std::process::id().to_string());
    Some(plan)
}

/// Remove the saved plan, its passwords, its progress and the entry that
/// continues it after sign-in.
pub fn discard_pending() {
    let had = pending_path().exists();
    for path in [pending_path(), pending_progress_path(), pending_started_path()] {
        let _ = std::fs::remove_file(path);
    }
    crate::secrets::forget(Secret::PendingInstall);
    #[cfg(windows)]
    if had {
        let _ = std::process::Command::new("reg.exe")
            .args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\RunOnce", "/v", "SENTIENT Setup", "/f"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    let _ = had;
}

/// What the Platform Manager offers for a plan saved before a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOffer {
    /// Nothing to continue.
    None,
    /// Saved, but Windows has not restarted since: the restart comes first.
    AfterRestart,
    /// A "Continue the SENTIENT install" button.
    Button,
    /// Open Setup on its install step and continue, as `--resume` does.
    Resume,
}

/// A plan older than this is offered with a button only, never opened by
/// itself.
pub const RESUME_BY_ITSELF_SECS: u64 = 3 * 24 * 3600;

/// What is known about a saved plan, for [`pending_offer`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PendingFacts {
    /// When the plan was saved (seconds since 1970); None without one.
    pub saved_at: Option<u64>,
    /// Its passwords are in the secret store.
    pub has_secrets: bool,
    /// When Windows last started (seconds since 1970).
    pub booted_at: u64,
    pub now: u64,
    /// SENTIENT is installed (its installation record is there).
    pub installed: bool,
    /// A Setup took the plan up already: Some(true) while it still runs,
    /// Some(false) when it stopped without finishing.
    pub started: Option<bool>,
}

pub fn pending_offer(f: &PendingFacts) -> PendingOffer {
    let Some(saved_at) = f.saved_at else { return PendingOffer::None };
    if !f.has_secrets || f.installed || f.started == Some(true) {
        return PendingOffer::None;
    }
    if saved_at >= f.booted_at {
        return PendingOffer::AfterRestart;
    }
    if f.started == Some(false) || f.now.saturating_sub(saved_at) > RESUME_BY_ITSELF_SECS {
        return PendingOffer::Button;
    }
    PendingOffer::Resume
}

/// [`pending_offer`] for this computer now. Cheap (file dates, and one
/// process looked up while a Setup has the plan), so a page may ask on
/// every paint.
pub fn pending_offer_now() -> PendingOffer {
    let secs = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let saved_at = std::fs::metadata(pending_path()).and_then(|m| m.modified()).ok().map(secs);
    if saved_at.is_none() {
        return PendingOffer::None;
    }
    let started = std::fs::read_to_string(pending_started_path()).ok().map(|pid| {
        pid.trim().parse::<u32>().is_ok_and(|pid| {
            let mut sys = sysinfo::System::new();
            let pid = sysinfo::Pid::from_u32(pid);
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            sys.process(pid).is_some()
        })
    });
    pending_offer(&PendingFacts {
        saved_at,
        has_secrets: crate::secrets::has(Secret::PendingInstall),
        booted_at: sysinfo::System::boot_time(),
        now: secs(std::time::SystemTime::now()),
        installed: crate::installation::record_path().exists(),
        started,
    })
}

/// A start of the Platform Manager with no request of its own (from Start,
/// a shortcut, or the Store package's hand-over): the one that may open
/// Setup to continue a saved install instead.
pub fn plain_start(args: &[String]) -> bool {
    args.iter().skip(1).all(|a| a == "--manager")
}

/// Restart Windows in a few seconds.
pub fn restart_now() {
    #[cfg(windows)]
    let _ = std::process::Command::new("shutdown.exe")
        .args(["/r", "/t", "5", "/c", "Restarting to finish installing SENTIENT"])
        .spawn();
}

/// WSL 2 turned on, or the run stops for the one restart that finishes
/// it (the plan saved; Setup continues after the next sign-in). After that
/// restart a WSL that still does not run is an error. Windows only.
#[cfg(windows)]
fn turn_on_wsl(plan: &InstallPlan, sink: &ProgressFn) -> Result<(), String> {
    use sentient_installer_core::wsl;
    if wsl::is_ready() {
        return Ok(());
    }
    let outcome = wsl::install(sink.clone());
    if outcome.ready {
        return Ok(());
    }
    if plan.wsl_restarted {
        let status = wsl::facts().status;
        return Err(format!(
            "WSL 2 still does not run after the restart. {}",
            wsl::vm_error(&status).unwrap_or_else(|| status.trim().lines().last().unwrap_or("wsl --status gave no answer").to_string())
        ));
    }
    let mut saved = plan.clone();
    saved.wsl_restarted = true;
    save_pending(&saved)?;
    Err(RESTART_REQUIRED.into())
}

/// ClickHouse in Docker inside WSL2, owned by the hidden account
/// (sentient_installer_core::clickhouse_wsl). Windows only.
#[cfg(windows)]
fn install_clickhouse_wsl(plan: &InstallPlan, manager: &std::path::Path, sink: &ProgressFn) -> Result<(), String> {
    turn_on_wsl(plan, sink)?;
    // A reinstall over kept data (the PostgreSQL cluster is there) takes
    // ClickHouse's kept history back too; a fresh install starts clean.
    let reuse = plan.existing == Some(true);
    provision_clickhouse_wsl_with(&plan.configured_stack(), manager, reuse, plan.delete_existing, sink)
}

/// With WSL2 ready: the hidden account, its distro with Docker and ClickHouse
/// (`stack`'s ClickHouse settings and password), started at every boot.
/// Also used by Migration.
#[cfg(windows)]
/// `reuse_kept`: take back a distro disk an earlier install left (its
/// ClickHouse history); otherwise such a disk is kept aside and the distro
/// starts empty.
pub fn provision_clickhouse_wsl(stack: &DeployConfig, manager: &std::path::Path, reuse_kept: bool, sink: &ProgressFn) -> Result<(), String> {
    provision_clickhouse_wsl_with(stack, manager, reuse_kept, false, sink)
}

/// [`provision_clickhouse_wsl`]; `delete_old`: an earlier install's
/// ClickHouse history is deleted instead of set aside.
#[cfg(windows)]
fn provision_clickhouse_wsl_with(stack: &DeployConfig, manager: &std::path::Path, reuse_kept: bool, delete_old: bool, sink: &ProgressFn) -> Result<(), String> {
    use sentient_installer_core::clickhouse_wsl;
    let password = crate::secrets::get(Secret::ClickhouseAccount).unwrap_or_else(clickhouse_wsl::new_password);
    clickhouse_wsl::ensure_account(sink, &password)?;
    crate::secrets::save(Secret::ClickhouseAccount, &password)?;

    // The account's stack: ClickHouse alone, under the reference names.
    let ch = DeployConfig { flavor: Flavor::Clickhouse, ..stack.clone() };
    let config = clickhouse_wsl::work_dir().join("config.json");
    std::fs::write(&config, serde_json::to_string(&ch).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write {}: {e}", config.display()))?;
    // It holds both passwords: Administrators, SYSTEM and the service
    // account only (audit I9).
    sentient_installer_core::native::private_file(&config, Some(clickhouse_wsl::ACCOUNT))?;
    step(sink, "Installing WSL2, Docker and ClickHouse as the service account");
    clickhouse_wsl::run_as_account(sink, &password, manager, if reuse_kept { "provision-reuse" } else { "provision" }, None)?;
    step(sink, "Starting ClickHouse at every boot");
    clickhouse_wsl::register_keepalive(&password)?;
    step(sink, "Stopping ClickHouse cleanly when Windows shuts down");
    if let Err(e) = clickhouse_wsl::install_guard(manager, ch.ch_http_port) {
        sink(Progress::Log { line: format!("note: the ClickHouse shutdown service was not set up: {e}") });
    }
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
        set_aside_clickhouse_history(&ch, delete_old, sink)?;
    }
    Ok(())
}

/// A ClickHouse that already holds SENTIENT's database (an earlier install
/// whose distro is still there) would give a new, empty PostgreSQL history
/// that belongs to devices it does not have. Rename it aside, where Kept
/// lists it for removal, and let the new install start empty.
#[cfg(windows)]
fn set_aside_clickhouse_history(ch: &DeployConfig, delete: bool, sink: &ProgressFn) -> Result<(), String> {
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
    if delete {
        conn.text(&format!("DROP DATABASE {} SYNC", ch.ch_db))
            .map_err(|e| format!("delete the earlier ClickHouse history: {e}"))?;
        sink(Progress::Log { line: "The earlier install's ClickHouse history was deleted.".into() });
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
const STEP_CLICKHOUSE: &str = "Setting up ClickHouse";
const STEP_SENTIENT: &str = "Installing SENTIENT and its database";

const STEP_WSL: &str = "Turning on WSL 2";
const STEP_DOCKER_WSL: &str = "Setting up Docker in WSL 2";

/// What the install's steps depend on besides the plan.
#[derive(Clone, Copy)]
pub struct Host {
    pub windows: bool,
    /// WSL 2 runs already (Windows).
    pub wsl_ready: bool,
    /// Setup runs from the installed Platform Manager.
    pub manager_in_place: bool,
}

impl Host {
    pub fn here(plan: &InstallPlan) -> Self {
        let windows = cfg!(windows);
        let needs_wsl = windows && (plan.engine == Engine::Docker || plan.stack.flavor == Flavor::Clickhouse);
        Host {
            windows,
            wsl_ready: !needs_wsl || sentient_installer_core::wsl::is_ready(),
            manager_in_place: self_install::manager_in_place(),
        }
    }
}

/// The install's steps, declared before it starts (the job view shows them
/// all; crate::jobplan for the other jobs).
pub fn steps(plan: &InstallPlan) -> Vec<sentient_installer_core::progress::PlanStep> {
    steps_on(plan, Host::here(plan))
}

/// [`steps`] on `host`.
pub fn steps_on(plan: &InstallPlan, host: Host) -> Vec<sentient_installer_core::progress::PlanStep> {
    use sentient_installer_core::progress::PlanStep;
    let mut v = Vec::new();
    if plan.bundle.is_none() && plan.engine == Engine::Native {
        if let Some(version) = &plan.download {
            v.push(PlanStep::new(format!("Downloading SENTIENT {version}")).detail("Checked against its SHA-256 and signature").when(&["Downloading SENTIENT"]).weight(3.));
        }
    }
    // Setup's own step: from the installed Platform Manager there is
    // nothing to put in place.
    if !host.manager_in_place && !plan.remote {
        v.push(PlanStep::new("Installing the Platform Manager").weight(0.5));
    }
    let docker_wsl = host.windows && plan.engine == Engine::Docker;
    let hybrid = host.windows && plan.engine == Engine::Native && plan.stack.flavor == Flavor::Clickhouse;
    if (docker_wsl || hybrid) && !host.wsl_ready {
        v.push(
            PlanStep::new(STEP_WSL)
                .detail("Windows restarts once, and Setup continues after you sign in")
                .when(&["Turning on the Windows feature", "Updating the WSL kernel", "Downloading the WSL package", "Installing the WSL package"])
                .weight(2.),
        );
    }
    if hybrid {
        v.push(PlanStep::new(STEP_CLICKHOUSE).detail("Its own WSL distribution and account").weight(4.));
    }
    if docker_wsl {
        v.push(
            PlanStep::new(STEP_DOCKER_WSL)
                .detail("SENTIENT's own Ubuntu distribution with Docker Engine")
                .when(&["Resetting an unresponsive SENTIENT distro", "Taking back the SENTIENT distro", "Downloading Ubuntu", "Creating the SENTIENT WSL distro", "Enabling systemd", "Installing Docker Engine", "Starting Docker"])
                .weight(4.),
        );
    }
    v.push(PlanStep::new(STEP_SENTIENT).detail(match plan.engine {
        Engine::Docker => "The containers, the database and its schema",
        Engine::Native => "PostgreSQL, the SENTIENT service and its schema",
    }).weight(6.));
    v.push(PlanStep::new("Setting the administrator sign-in").detail(plan.admin_email.clone()).weight(0.5));
    if !plan.remote {
        v.push(PlanStep::new("Creating shortcuts").weight(0.5));
    }
    v.push(PlanStep::new("Saving the installation record").weight(0.3));
    v
}

/// Screens: the install's steps without a plan of choices.
#[cfg(debug_assertions)]
pub fn screen_steps() -> Vec<sentient_installer_core::progress::PlanStep> {
    use sentient_installer_core::progress::PlanStep;
    vec![
        PlanStep::new("Installing the Platform Manager").weight(0.5),
        PlanStep::new(STEP_SENTIENT).detail("PostgreSQL, the SENTIENT service and its schema").when(&["Starting PostgreSQL", "Installing PostgreSQL"]).weight(6.),
        PlanStep::new("Setting the administrator sign-in").weight(0.5),
        PlanStep::new("Creating shortcuts").weight(0.5),
        PlanStep::new("Saving the installation record").weight(0.3),
    ]
}

/// The Platform Manager alone.
pub fn manager_only_steps() -> Vec<sentient_installer_core::progress::PlanStep> {
    use sentient_installer_core::progress::PlanStep;
    vec![PlanStep::new("Installing the Platform Manager"), PlanStep::new("Creating shortcuts")]
}

pub fn run(mut plan: InstallPlan, sink: ProgressFn) -> Result<(), String> {
    let began = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // A new install from the beginning replaces a plan an earlier one saved
    // for its restart (a continued plan has `wsl_restarted`).
    if !plan.wsl_restarted && !plan.remote {
        discard_pending();
    }
    // A server's install never meets an earlier SENTIENT there by chance:
    // its database would keep its own password (the new one never works)
    // or be taken over silently (audit I3). It is added, or removed, first.
    if plan.remote && plan.existing.is_none() {
        if let Some(c) = crate::adopt::search().found.first() {
            return Err(format!(
                "This server already has SENTIENT ({}, {}). Nothing was installed: add it as an instance instead (Instances › Add existing), or remove it first.",
                c.kind, c.location
            ));
        }
    }
    if plan.bundle.is_none() && plan.engine == Engine::Native {
        if let Some(version) = plan.download.clone() {
            let (_, dir) = crate::online::download(Some(&version), &sink)?;
            plan.bundle = Some(dir);
        }
    }
    // On a server the agent itself is the Platform Manager there.
    let manager = if plan.remote {
        std::env::current_exe().map_err(|e| format!("locate this program: {e}"))?
    } else if self_install::manager_in_place() {
        self_install::manager_exe()
    } else {
        step(&sink, "Installing the Platform Manager");
        self_install::install_manager()?
    };
    // What is already here, before anything is installed (uninstall removes only what Setup put here).
    crate::uninstall::note_before_install(plan.engine, plan.stack.flavor);
    // ClickHouse first: SENTIENT's schema installer creates its tables there.
    #[cfg(windows)]
    if plan.engine == Engine::Native && plan.stack.flavor == Flavor::Clickhouse {
        if !sentient_installer_core::wsl::is_ready() {
            step(&sink, STEP_WSL);
            turn_on_wsl(&plan, &sink)?;
        }
        step(&sink, STEP_CLICKHOUSE);
        install_clickhouse_wsl(&plan, &manager, &sink)?;
    }
    // Docker in WSL 2: WSL turned on (one restart), then SENTIENT's own
    // distro with Docker Engine (distro::setup).
    #[cfg(windows)]
    if plan.engine == Engine::Docker {
        if !sentient_installer_core::wsl::is_ready() {
            step(&sink, STEP_WSL);
            turn_on_wsl(&plan, &sink)?;
        }
        step(&sink, STEP_DOCKER_WSL);
        distro::setup(sink.clone(), &plan.install_dir)?;
        // The distro, and with it Docker, stays up: WSL ends a distro
        // shortly after its last wsl.exe ends, also while Docker runs
        // there, so this comes before the stack. It starts with Windows,
        // after the start delay, when the operator chose so (an operator
        // screen needs it too, as for the native services); otherwise the
        // Platform Manager's Start runs it (distro::wake_keepalive).
        let start = (plan.shortcuts.autostart || plan.shortcuts.open_at_signin).then_some(plan.shortcuts.start_delay);
        if let Err(e) = distro::register_keepalive(&distro::forwarded_ports(&plan.stack), start) {
            sink(Progress::Log { line: format!("note: SENTIENT will not start by itself with Windows: {e}") });
        }
    }
    let admin_hash = hash_password(&plan.admin_password)?;
    step(&sink, STEP_SENTIENT);

    match plan.engine {
        Engine::Native => {
            let cfg = plan.native_config();
            native::setup(sink.clone(), &cfg)?;
            // Windows applies the PostgreSQL parameters here; on Linux
            // native::setup already wrote them with the cluster.
            if cfg!(windows) {
                native::apply_pg_tuning(&sink, &cfg)?;
            }
            if let Some(keep) = plan.existing {
                native::prepare_existing(&sink, &cfg, keep, plan.delete_existing)?;
            }
            native::deploy(sink.clone(), &cfg)?;
            // Other computers reach SENTIENT through Windows Firewall.
            #[cfg(windows)]
            if let Err(e) = native::open_firewall(&cfg) {
                sink(Progress::Log { line: format!("note: SENTIENT's ports were not opened in Windows Firewall: {e}") });
            }
            step(&sink, "Setting the administrator sign-in");
            admin_sign_in(&sink, plan.existing.is_some(), native::set_admin_account(&cfg, &plan.admin_email, &admin_hash))?;
            // An operator screen needs SENTIENT running after every boot.
            self_install::set_autostart(plan.shortcuts.autostart || plan.shortcuts.open_at_signin, plan.shortcuts.start_delay)?;
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
            // Windows set Docker up above, before this step.
            if !cfg!(windows) {
                distro::setup(sink.clone(), &plan.install_dir)?;
            }
            distro::deploy_with(sink.clone(), &stack, plan.existing)?;
            step(&sink, "Setting the administrator sign-in");
            admin_sign_in(&sink, plan.existing.is_some(), distro::set_admin_account(&stack, &plan.admin_email, &admin_hash))?;
        }
    }

    let url = format!("http://localhost:{}", plan.stack.http_port);
    if !plan.remote {
        step(&sink, "Creating shortcuts");
        self_install::create_shortcuts(&manager, &url, &plan.shortcuts)?;
    }
    if plan.shortcuts.kiosk {
        sentient_installer_core::kiosk::create_shortcut(&sink, &manager, &plan.state_dir())?;
        crate::shortcuts::note(crate::shortcuts::Owner::Sentient, sentient_installer_core::kiosk::shortcut_paths());
    }
    if plan.shortcuts.open_at_signin {
        sentient_installer_core::kiosk::set_open_at_signin(&sink, &manager, Some(true))?;
        crate::shortcuts::note(crate::shortcuts::Owner::Sentient, sentient_installer_core::kiosk::signin_entry_path());
    }
    if !plan.shortcuts.auto_signin.is_empty() {
        let password = if plan.auto_signin_password.is_empty() {
            std::env::var("SPM_AUTOSIGNIN_PASSWORD").unwrap_or_default()
        } else {
            plan.auto_signin_password.clone()
        };
        // SENTIENT is installed by now; a refused password should not undo
        // that, it can be entered again in Platform settings, under Startup.
        if let Err(e) = sentient_installer_core::autologin::set(&sink, Some((&plan.shortcuts.auto_signin, &password))) {
            sink(Progress::Log { line: format!("Automatic sign-in was not turned on: {e} Turn it on in Platform settings, under Startup.") });
        }
    }

    // SENTIENT's Apps & features entry covers the manager installed alone before.
    self_install::remove_manager_entry();
    step(&sink, "Saving the installation record");
    let json = serde_json::to_string_pretty(&plan.record()).map_err(|e| e.to_string())?;
    self_install::write_record(&crate::installation::record_path(), &json)?;

    if !plan.remote {
        discard_pending();
    }
    // Installing the WSL package opens Microsoft's welcome window, which
    // would stay behind Setup's Finish page.
    if cfg!(windows) && (plan.engine == Engine::Docker || plan.stack.flavor == Flavor::Clickhouse) {
        sentient_installer_core::wsl::close_welcome_window(&sink, began);
    }
    sink(Progress::Done { message: format!("SENTIENT is running at {url}") });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> PendingFacts {
        // Saved an hour before the restart, looked at a minute after it.
        PendingFacts { saved_at: Some(10_000), has_secrets: true, booted_at: 13_600, now: 13_660, installed: false, started: None }
    }

    #[test]
    fn a_plan_saved_before_the_restart_is_continued() {
        assert_eq!(pending_offer(&facts()), PendingOffer::Resume);
    }

    #[test]
    fn no_plan_or_no_passwords_or_installed_offers_nothing() {
        assert_eq!(pending_offer(&PendingFacts { saved_at: None, ..facts() }), PendingOffer::None);
        assert_eq!(pending_offer(&PendingFacts { has_secrets: false, ..facts() }), PendingOffer::None);
        assert_eq!(pending_offer(&PendingFacts { installed: true, ..facts() }), PendingOffer::None);
    }

    #[test]
    fn before_the_restart_the_restart_comes_first() {
        // Setup that stopped may still show its restart panel.
        assert_eq!(pending_offer(&PendingFacts { booted_at: 9_000, ..facts() }), PendingOffer::AfterRestart);
        assert_eq!(pending_offer(&PendingFacts { booted_at: 10_000, ..facts() }), PendingOffer::AfterRestart);
    }

    #[test]
    fn a_setup_continuing_it_now_is_left_alone() {
        assert_eq!(pending_offer(&PendingFacts { started: Some(true), ..facts() }), PendingOffer::None);
    }

    #[test]
    fn a_continued_run_that_stopped_or_an_old_plan_gets_a_button_only() {
        // No loop of Setup opening by itself on every start.
        assert_eq!(pending_offer(&PendingFacts { started: Some(false), ..facts() }), PendingOffer::Button);
        let old = PendingFacts { now: 10_000 + RESUME_BY_ITSELF_SECS + 1, ..facts() };
        assert_eq!(pending_offer(&old), PendingOffer::Button);
        let edge = PendingFacts { now: 10_000 + RESUME_BY_ITSELF_SECS, ..facts() };
        assert_eq!(pending_offer(&edge), PendingOffer::Resume);
    }

    #[test]
    fn only_a_plain_start_continues_an_install() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(plain_start(&a(&["spm"])));
        assert!(plain_start(&a(&["spm", "--manager"])));
        assert!(!plain_start(&a(&["spm", "--manager", "--background"])));
        assert!(!plain_start(&a(&["spm", "--request", "show"])));
        assert!(!plain_start(&a(&["spm", "--job", "package-identity"])));
    }

    fn plan(engine: Engine, flavor: Flavor) -> InstallPlan {
        InstallPlan {
            engine,
            sentient_version: "4.4.0.1".into(),
            install_dir: PathBuf::from(r"C:\Program Files\SENTIENT"),
            data_dir: PathBuf::from(r"C:\PostgreSQL\18"),
            stack: DeployConfig { flavor, ..DeployConfig::default() },
            pg_superuser_password: String::new(),
            admin_email: "ada@example.test".into(),
            admin_password: String::new(),
            settings: Vec::new(),
            shortcuts: Shortcuts { autostart: true, desktop_sentient: true, desktop_manager: true, start_menu: true, kiosk: false, open_at_signin: false, auto_signin: String::new(), start_delay: 0 },
            auto_signin_password: String::new(),
            bundle: None,
            download: None,
            existing: None,
            delete_existing: false,
            old_pg: Default::default(),
            wsl_restarted: false,
            remote: false,
        }
    }

    fn names(plan: &InstallPlan, host: Host) -> Vec<String> {
        steps_on(plan, host).into_iter().map(|s| s.name).collect()
    }

    const WINDOWS: Host = Host { windows: true, wsl_ready: true, manager_in_place: true };

    #[test]
    fn windows_docker_without_wsl_turns_it_on_first() {
        let p = plan(Engine::Docker, Flavor::Timescaledb);
        let v = names(&p, Host { wsl_ready: false, ..WINDOWS });
        assert_eq!(v[..3], [STEP_WSL, STEP_DOCKER_WSL, STEP_SENTIENT]);
        assert!(!v.contains(&STEP_CLICKHOUSE.to_string()));
        let wsl = steps_on(&p, Host { wsl_ready: false, ..WINDOWS }).remove(0);
        assert!(wsl.detail.contains("restarts once"));
        assert!(wsl.matches("Turning on the Windows feature VirtualMachinePlatform"));
    }

    #[test]
    fn windows_docker_with_wsl_sets_up_docker_then_sentient() {
        let v = names(&plan(Engine::Docker, Flavor::Clickhouse), WINDOWS);
        assert_eq!(v, [STEP_DOCKER_WSL, STEP_SENTIENT, "Setting the administrator sign-in", "Creating shortcuts", "Saving the installation record"]);
        let docker = steps_on(&plan(Engine::Docker, Flavor::Clickhouse), WINDOWS).remove(0);
        assert!(docker.matches("Installing Docker Engine (a few minutes)"));
        assert!(docker.matches("Downloading Ubuntu base image (~350 MB)"));
    }

    #[test]
    fn the_windows_hybrid_keeps_its_clickhouse_step_and_turns_wsl_on_when_missing() {
        let p = plan(Engine::Native, Flavor::Clickhouse);
        assert_eq!(names(&p, WINDOWS)[..2], [STEP_CLICKHOUSE, STEP_SENTIENT]);
        assert_eq!(names(&p, Host { wsl_ready: false, ..WINDOWS })[..2], [STEP_WSL, STEP_CLICKHOUSE]);
        // Native without ClickHouse never needs WSL.
        assert_eq!(names(&plan(Engine::Native, Flavor::Stock), Host { wsl_ready: false, ..WINDOWS })[0], STEP_SENTIENT);
    }

    // The wizard's placement module is GUI-only; the agent build has none.
    #[cfg(feature = "gui")]
    /// The wizard's choice (a card on System check, a database) as the
    /// plan it builds (Installer::plan → placement::plan_engine), and the
    /// steps that plan runs on this branch's backend.
    fn chosen(host: crate::installer::placement::Host, card: Engine, flavor: Flavor) -> Result<InstallPlan, crate::installer::placement::Why> {
        use crate::installer::placement::{Placement, plan_engine};
        plan_engine(host, Placement::preset(card, host.windows), flavor).map(|engine| plan(engine, flavor))
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_choice_on_system_check_makes_the_plan_the_backend_runs() {
        use crate::installer::placement::{Host as On, Why, Wsl, default_engine};
        let win = On { windows: true, wsl: Wsl::Ready };
        let win_off = On { windows: true, wsl: Wsl::TurnOn };
        let win_none = On { windows: true, wsl: Wsl::Unavailable };
        let linux = On { windows: false, wsl: Wsl::Unavailable };
        // Windows' default: native services, no WSL at all.
        let p = chosen(win, default_engine(), Flavor::Timescaledb).unwrap();
        assert_eq!(p.engine, Engine::Native);
        assert_eq!(names(&p, Host { wsl_ready: false, ..WINDOWS })[0], STEP_SENTIENT);
        // Windows Docker: Docker in WSL 2 (turned on first when it is off).
        let p = chosen(win_off, Engine::Docker, Flavor::Stock).unwrap();
        assert_eq!(p.engine, Engine::Docker);
        assert_eq!(names(&p, Host { wsl_ready: false, ..WINDOWS })[..2], [STEP_WSL, STEP_DOCKER_WSL]);
        // The hybrid: native, ClickHouse in its own WSL distribution.
        let p = chosen(win, Engine::Native, Flavor::Clickhouse).unwrap();
        assert_eq!(p.engine, Engine::Native);
        assert_eq!(names(&p, WINDOWS)[..2], [STEP_CLICKHOUSE, STEP_SENTIENT]);
        // Without WSL 2: neither Docker nor ClickHouse.
        assert_eq!(chosen(win_none, Engine::Docker, Flavor::Stock).err(), Some(Why::NeedsWsl));
        assert_eq!(chosen(win_none, Engine::Native, Flavor::Clickhouse).err(), Some(Why::NeedsWsl));
        // Linux: in the system is the default; TimescaleDB there is refused.
        assert_eq!(chosen(linux, default_engine(), Flavor::Clickhouse).unwrap().engine, Engine::Native);
        assert_eq!(chosen(linux, Engine::Native, Flavor::Timescaledb).err(), Some(Why::NoDistroTimescale));
        assert_eq!(chosen(linux, Engine::Docker, Flavor::Timescaledb).unwrap().engine, Engine::Docker);
    }

    #[test]
    fn linux_docker_has_no_wsl_steps() {
        let linux = Host { windows: false, wsl_ready: false, manager_in_place: false };
        let v = names(&plan(Engine::Docker, Flavor::Clickhouse), linux);
        assert_eq!(v[..2], ["Installing the Platform Manager", STEP_SENTIENT]);
    }

    #[test]
    fn a_saved_plan_from_before_this_field_reads_as_not_restarted() {
        let mut v = serde_json::to_value(plan(Engine::Docker, Flavor::Stock)).unwrap();
        v.as_object_mut().unwrap().remove("wsl_restarted");
        let p: InstallPlan = serde_json::from_value(v).unwrap();
        assert!(!p.wsl_restarted);
    }

    #[test]
    fn new_docker_installs_use_the_release_tag() {
        let default = sentient_installer_core::distro::DeployConfig::default().image;
        assert_eq!(pinned_image(&default, Some("4.4.0.3")), "quakestring/sentient:4.4.0.3");
        assert_eq!(pinned_image("registry:5000/sentient:latest", Some("4.4.0.3")), "registry:5000/sentient:4.4.0.3");
        // No release number known: the image as it is.
        assert_eq!(pinned_image(&default, None), default);
        assert_eq!(pinned_image(&default, Some("in this package")), default);
    }
}

/// The administrator sign-in's result: the new sign-in is set, or (a kept
/// database whose administrator is no longer admin@sentient.local) said so
/// plainly, the kept one staying; a fresh database that did not take it is
/// an error (audit I2: it used to say done and the new sign-in never worked).
fn admin_sign_in(sink: &ProgressFn, kept: bool, r: Result<(), String>) -> Result<(), String> {
    match r {
        Ok(()) => Ok(()),
        Err(_) if kept => {
            sink(Progress::Log { line: "The kept database's administrator is not admin@sentient.local, so the sign-in entered here was not set: sign in with the administrator account it already had.".into() });
            Ok(())
        }
        Err(e) => Err(format!("The administrator sign-in could not be set ({e}).")),
    }
}
