// Copyright © 2016-2026 The SENTIENT Authors

//! A fresh SENTIENT on a server (Status › Install SENTIENT,
//! crate::manager::remote_install): what the agent finds there for it, what
//! can be chosen on that server, and the install itself, run there by the
//! agent as a job.
//!
//! The install is Setup's own (crate::installer::plan::run, with
//! `remote`): the window sends the plan as a file without a password in it,
//! and the passwords on the agent's standard input first
//! (`--job install-secrets`), which keeps them in a file only root reads
//! (0600) until `--job install <plan> --staged` takes them and removes it.
//! Nothing secret is on a command line, in a log or in this computer's
//! profile. The job runs detached on the server (crate::remote::start_job),
//! so a dropped connection or a sleeping laptop does not stop it; its id
//! carries [`JOB_TAG`], so a window opened later knows the running job is
//! an install and shows it again ([`running_install`]).

// The window's half (choices, re-attaching) is not in the agent build.
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use sentient_installer_core::stack::Flavor;

use sentient_installer_core::glibc::{self, GlibcVersion, Libc, NativeGlibc};

use crate::installation::Engine;

/// The tag on an install job's id (`20261119101500-install`).
pub const JOB_TAG: &str = "install";

/// The file `install-secrets` keeps the passwords in (crate::gateway_record::
/// staged_path's folder).
pub const SECRETS_FILE: &str = "install.secret";

/// One System check row, as the agent read it on the server
/// (sentient_installer_core::checks::Check).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FactCheck {
    pub id: String,
    pub label: String,
    /// pass | setup | fail | warn | unknown.
    pub status: String,
    pub detail: String,
    #[serde(default)]
    pub state: Option<String>,
}

impl FactCheck {
    pub fn blocks(&self) -> bool {
        self.status == "fail"
    }
}

/// What the agent found on the server for an install
/// (`--job install-facts-json`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ServerFacts {
    /// `docker` runs (its client answers).
    pub docker: bool,
    #[serde(default)]
    pub docker_version: String,
    /// `docker compose` answers.
    #[serde(default)]
    pub compose: bool,
    /// systemd runs the server (PID 1).
    pub systemd: bool,
    #[serde(default)]
    pub os_name: String,
    #[serde(default)]
    pub distro: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub disk_free: Option<u64>,
    #[serde(default)]
    pub mem_total: Option<u64>,
    /// The System check for "In the system", and for Docker (empty without it).
    #[serde(default)]
    pub native_checks: Vec<FactCheck>,
    #[serde(default)]
    pub docker_checks: Vec<FactCheck>,
    /// The C library (`ldd --version` on the server): a native SENTIENT
    /// needs a new enough glibc. Unknown from an agent older than 0.9.3.
    #[serde(default)]
    pub libc: sentient_installer_core::glibc::Libc,
}

impl ServerFacts {
    /// The checks for `engine`.
    pub fn checks(&self, engine: Engine) -> &[FactCheck] {
        match engine {
            Engine::Native => &self.native_checks,
            Engine::Docker => &self.docker_checks,
        }
    }

    fn os(&self) -> &str {
        if self.os_name.is_empty() { "this server" } else { &self.os_name }
    }

    /// Whether a native SENTIENT `version` (needing glibc `min`) runs on
    /// the server's C library. Alpine is musl even when the agent did not
    /// say (an older one).
    pub fn native_glibc(&self, version: &str, min: Option<GlibcVersion>) -> NativeGlibc {
        let libc = match &self.libc {
            Libc::Unknown if self.distro == "alpine" => Libc::Musl { version: String::new() },
            l => l.clone(),
        };
        glibc::decide(version, min, &libc, &self.os_name, glibc::Here::Server)
    }
}

/// Whether "In the system" or Docker can be chosen on the server: Err is
/// the (i)'s text (a title line, a blank line, why and what to do).
pub fn engine_offer(f: &ServerFacts, engine: Engine, server: &str) -> Result<(), String> {
    match engine {
        Engine::Docker if !f.docker => Err(format!(
            "Docker is not on this server\n\n“docker” was not found on {server}. Install Docker Engine on the server and press Check again, or install in the system."
        )),
        Engine::Docker if !f.compose => Err(format!(
            "Docker Compose is missing\n\nDocker is on {server}, but not its Compose plugin. Install docker-compose-plugin there and press Check again, or install in the system."
        )),
        Engine::Native if !f.systemd => Err(format!(
            "systemd does not run this server\n\nIn the system installs SENTIENT and its databases as systemd services, and systemd is not running on {server}.{}",
            if f.docker { " Docker is there: choose Docker." } else { "" }
        )),
        _ => Ok(()),
    }
}

/// Whether `flavor` can be installed with `engine` on the server: Err is
/// the (i)'s text.
pub fn flavor_offer(f: &ServerFacts, engine: Engine, flavor: Flavor) -> Result<(), String> {
    match (engine, flavor) {
        (Engine::Native, Flavor::Timescaledb) => Err(format!(
            "Not with “In the system” on {}\n\nTimescaleDB is not in the distribution’s packages. {}",
            f.os(),
            if engine_offer(f, Engine::Docker, "").is_ok() {
                "Docker is on this server: choose Docker on System check to use it."
            } else {
                "It runs with Docker: install Docker Engine on the server, then choose Docker on System check."
            }
        )),
        _ => Ok(()),
    }
}

/// The card System check starts on: "In the system" where it can run,
/// else Docker.
pub fn default_engine(f: &ServerFacts) -> Engine {
    if engine_offer(f, Engine::Native, "").is_ok() || engine_offer(f, Engine::Docker, "").is_err() { Engine::Native } else { Engine::Docker }
}

/// The database Database starts on: PostgreSQL + ClickHouse (recommended)
/// where it can run, else the first that can.
pub fn default_flavor(f: &ServerFacts, engine: Engine) -> Flavor {
    [Flavor::Clickhouse, Flavor::Stock, Flavor::Timescaledb].into_iter().find(|fl| flavor_offer(f, engine, *fl).is_ok()).unwrap_or(Flavor::Stock)
}

/// The install job running on the server, from its status's running job
/// (an id with [`JOB_TAG`]).
pub fn running_install(running_job: Option<&str>) -> Option<&str> {
    running_job.filter(|id| id.rsplit_once('-').is_some_and(|(_, tag)| tag == JOB_TAG))
}

/// The install job's arguments: the plan file, and `--staged` (its
/// passwords went ahead on standard input). Never a secret.
pub fn job_args(plan_file: &str) -> Vec<String> {
    vec!["install".into(), plan_file.into(), "--staged".into()]
}

/// The ports a configuration binds on the server, as `install-facts-json`
/// takes them (`8080:Web,1883:MQTT,…`; the names without spaces).
pub fn ports_arg(ports: &[(u16, &str)]) -> String {
    ports.iter().map(|(p, n)| format!("{p}:{}", n.replace([' ', ',', ':', '\''], "-"))).collect::<Vec<_>>().join(",")
}

/// [`ports_arg`] read back.
pub fn parse_ports(arg: &str) -> Vec<(u16, String)> {
    arg.split(',').filter_map(|p| p.split_once(':')).filter_map(|(p, n)| Some((p.trim().parse().ok()?, n.replace('-', " ")))).collect()
}

// ---------------------------------------------------------------------------
// On the server (the agent's jobs, crate::headless)
// ---------------------------------------------------------------------------

/// What `install-facts-json` prints: this server's facts for an install,
/// the System check run for both ways (`ports`: the ones to check free;
/// none, the defaults).
pub fn gather(ports: &[(u16, String)]) -> ServerFacts {
    use sentient_installer_core::checks;
    let quiet = |prog: &str, args: &[&str]| {
        std::process::Command::new(prog).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let docker_version = quiet("docker", &["--version"]);
    let compose = docker_version.is_some() && quiet("docker", &["compose", "version"]).is_some();
    let systemd = std::path::Path::new("/run/systemd/system").is_dir();
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |k: &str| release.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim_matches('"').to_string()).unwrap_or_default();
    let mon = crate::sysmon::sample(std::path::Path::new("/"));
    let ports = (!ports.is_empty()).then_some(ports);
    let rows = |mode: &str| -> Vec<FactCheck> {
        checks::run_for(mode, false, ports).into_iter().filter_map(|c| serde_json::to_value(&c).ok().and_then(|v| serde_json::from_value(v).ok())).collect()
    };
    ServerFacts {
        docker: docker_version.is_some(),
        docker_version: docker_version.unwrap_or_default(),
        compose,
        systemd,
        os_name: field("PRETTY_NAME"),
        distro: field("ID"),
        arch: std::env::consts::ARCH.to_string(),
        disk_free: (mon.disk_total > 0).then_some(mon.disk_free),
        mem_total: (mon.mem_total > 0).then_some(mon.mem_total),
        native_checks: if systemd { rows("linux-native") } else { Vec::new() },
        docker_checks: if compose { rows("linux-docker") } else { Vec::new() },
        libc: glibc::local(),
    }
}

/// Keep the passwords for the install job (`install-secrets`): a new file
/// only this account reads (0600).
pub fn stage(s: &crate::installer::InstallSecrets) -> Result<(), String> {
    stage_private_at(&crate::gateway_record::staged_path(SECRETS_FILE), s, "The passwords could not be kept for the install")
}

/// What [`stage`] kept, removed as it is read (`install --staged`).
pub fn take_staged() -> Result<crate::installer::InstallSecrets, String> {
    take_private_at(&crate::gateway_record::staged_path(SECRETS_FILE)).ok_or_else(|| "The passwords did not reach the server. Install again.".to_string())
}

/// Write `value` as JSON to a new file at `path` that only this account
/// reads (0600). One already there is removed first, so a file or link
/// someone else left at the name is never written through.
pub fn stage_private_at<T: Serialize>(path: &std::path::Path, value: &T, what: &str) -> Result<(), String> {
    use std::io::Write as _;
    let fail = |e: std::io::Error| format!("{what}: {e}");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let _ = std::fs::remove_file(path);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(fail)?;
    f.write_all(serde_json::to_string(value).unwrap_or_default().as_bytes()).map_err(fail)
}

/// What [`stage_private_at`] wrote, the file removed as it is read
/// (whether it reads or not): taken once.
pub fn take_private_at<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let text = std::fs::read_to_string(path);
    let _ = std::fs::remove_file(path);
    text.ok().and_then(|t| serde_json::from_str(&t).ok())
}

/// `install --staged`: the plan file read (and removed: it was put there
/// for this job), its passwords taken from where `install-secrets` kept them.
pub fn staged_plan(path: &str) -> Result<crate::installer::InstallPlan, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"));
    let _ = std::fs::remove_file(path);
    let plan: crate::installer::InstallPlan = serde_json::from_str(&text?).map_err(|e| format!("the plan: {e}"))?;
    Ok(plan.with_secrets(take_staged()?))
}

// ---------------------------------------------------------------------------
// From this computer (crate::manager::remote_install)
// ---------------------------------------------------------------------------

/// The server's facts for an install, the chosen ports checked free.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn facts(inst: &super::Instance, ports: &[(u16, &str)]) -> Result<ServerFacts, String> {
    #[cfg(debug_assertions)]
    if let Some(f) = debug_facts() {
        return Ok(f);
    }
    let job = if ports.is_empty() { "install-facts-json".to_string() } else { format!("install-facts-json {}", ports_arg(ports)) };
    super::read_json(inst, &job)
}

/// Screens: canned facts (SPM_DEBUG_INSTALL_FACTS=nodocker|docker).
#[cfg(debug_assertions)]
pub fn debug_facts() -> Option<ServerFacts> {
    let v = std::env::var("SPM_DEBUG_INSTALL_FACTS").ok()?;
    let docker = v == "docker";
    let row = |id: &str, label: &str, status: &str, detail: &str| FactCheck { id: id.into(), label: label.into(), status: status.into(), detail: detail.into(), state: None };
    Some(ServerFacts {
        docker,
        docker_version: if docker { "Docker version 27.3.1".into() } else { String::new() },
        compose: docker,
        systemd: true,
        os_name: if std::env::var_os("SPM_DEBUG_GLIBC").is_some() { "Debian GNU/Linux 12 (bookworm)".into() } else { "Ubuntu 24.04 LTS".into() },
        distro: "ubuntu".into(),
        arch: "x86_64".into(),
        libc: glibc::local(),
        disk_free: Some(433_000_000_000),
        mem_total: Some(15_600_000_000),
        native_checks: vec![
            row("systemd", "systemd", "pass", "systemd 255 runs the server."),
            row("root", "Privileges", "pass", "The agent runs as root (sudo for ops)."),
            row("packages", "Package manager", "pass", "apt-get, from Ubuntu's packages."),
            row("disk", "Disk space", "pass", "433 GB free on /."),
            row("ports", "Required ports", "pass", "8080, 1883, 8883, 5683, 5684 and 5432 are free."),
            row("download", "Download hosts", "pass", "github.com and packages.clickhouse.com answer from the server."),
        ],
        docker_checks: Vec::new(),
    })
}

/// Start the install on the server: the passwords on the agent's standard
/// input first, the plan (without them) as a file in the account's home,
/// then the job, detached. Returns the job's id. Blocking.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn start(inst: &super::Instance, plan: crate::installer::InstallPlan) -> Result<String, String> {
    let (bare, secrets) = plan.split_secrets();
    let line = serde_json::to_string(&secrets).map_err(|e| e.to_string())?;
    drop(secrets);
    super::job_on_stdin(inst, &["--json".to_string(), "install-secrets".to_string()], &format!("{line}\n"))?;
    drop(line);
    let sep = if super::is_windows(inst) { "\\" } else { "/" };
    let file = format!("{}{sep}.spm-install-plan-{}.json", super::home(inst)?.trim().trim_end_matches(['/', '\\']), chrono::Utc::now().timestamp_millis());
    super::put_text(inst, &file, &serde_json::to_string(&bare).map_err(|e| e.to_string())?)?;
    let args = job_args(&file);
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    super::start_job_as(inst, &a, Some(JOB_TAG))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installer::{InstallPlan, InstallSecrets, Shortcuts};
    use sentient_installer_core::distro::DeployConfig;

    const ADMIN: &str = "Admin-Pw-7q2Lx9";
    const DB: &str = "Db-Pw-K3m8Zr";
    const PG_ADMIN: &str = "PgAdmin-Pw-W4n6Ty";
    const CH: &str = "Ch-Pw-B9v1Qs";

    fn plan() -> InstallPlan {
        InstallPlan {
            engine: Engine::Native,
            sentient_version: "4.4.0.3".into(),
            install_dir: "/opt/sentient".into(),
            data_dir: Default::default(),
            stack: DeployConfig { flavor: Flavor::Clickhouse, db_password: DB.into(), ch_password: CH.into(), ..DeployConfig::default() },
            pg_superuser_password: PG_ADMIN.into(),
            admin_email: "sysadmin@dfal.local".into(),
            admin_password: ADMIN.into(),
            settings: Vec::new(),
            shortcuts: Shortcuts { autostart: false, desktop_sentient: false, desktop_manager: false, start_menu: false, kiosk: false, open_at_signin: false, auto_signin: String::new(), start_delay: 0 },
            auto_signin_password: String::new(),
            bundle: None,
            download: Some("4.4.0.3".into()),
            existing: None,
            delete_existing: false,
            old_pg: Default::default(),
            wsl_restarted: false,
            remote: true,
            pack: None,
        }
    }

    #[test]
    fn the_plan_sent_to_the_server_has_no_secret() {
        let (bare, secrets) = plan().split_secrets();
        let json = serde_json::to_string(&bare).unwrap();
        for s in [ADMIN, DB, PG_ADMIN, CH] {
            assert!(!json.contains(s), "{s} is in the plan file");
        }
        // The job's arguments carry only the file's name.
        assert_eq!(job_args("/home/ops/.spm-install-plan-1.json"), ["install", "/home/ops/.spm-install-plan-1.json", "--staged"]);
        assert!(!format!("{secrets:?}").contains(ADMIN));
        // Put back on the server, the plan is whole again.
        let back: InstallPlan = serde_json::from_str(&json).unwrap();
        let whole = back.with_secrets(secrets);
        assert_eq!((whole.admin_password.as_str(), whole.stack.db_password.as_str(), whole.pg_superuser_password.as_str(), whole.stack.ch_password.as_str()), (ADMIN, DB, PG_ADMIN, CH));
        assert!(whole.remote && whole.admin_email == "sysadmin@dfal.local");
    }

    #[cfg(unix)]
    #[test]
    fn staged_secrets_are_root_only_and_taken_once() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("spm-install-stage-{}", std::process::id()));
        let path = dir.join(SECRETS_FILE);
        let (_, secrets) = plan().split_secrets();
        // Something left at the name is replaced, never written through.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        stage_private_at(&path, &secrets, "kept").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let taken: InstallSecrets = take_private_at(&path).unwrap();
        assert_eq!(taken.admin, ADMIN);
        assert!(!path.exists());
        assert!(take_private_at::<InstallSecrets>(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn facts(docker: bool, compose: bool, systemd: bool) -> ServerFacts {
        ServerFacts { docker, compose, systemd, os_name: "Debian GNU/Linux 12 (bookworm)".into(), ..ServerFacts::default() }
    }

    #[test]
    fn options_the_server_cannot_run_are_off_with_the_reason() {
        // No Docker: in the system only, and no TimescaleDB.
        let f = facts(false, false, true);
        let why = engine_offer(&f, Engine::Docker, "PLANT_B").unwrap_err();
        assert!(why.starts_with("Docker is not on this server\n\n") && why.contains("PLANT_B"));
        assert!(engine_offer(&f, Engine::Native, "PLANT_B").is_ok());
        assert_eq!(default_engine(&f), Engine::Native);
        let ts = flavor_offer(&f, Engine::Native, Flavor::Timescaledb).unwrap_err();
        assert!(ts.starts_with("Not with “In the system” on Debian GNU/Linux 12"));
        assert!(ts.contains("install Docker Engine"));
        assert!(flavor_offer(&f, Engine::Native, Flavor::Clickhouse).is_ok() && flavor_offer(&f, Engine::Native, Flavor::Stock).is_ok());
        assert_eq!(default_flavor(&f, Engine::Native), Flavor::Clickhouse);
        // Docker there: TimescaleDB with Docker, and the (i) says so.
        let f = facts(true, true, true);
        assert!(engine_offer(&f, Engine::Docker, "x").is_ok());
        assert!(flavor_offer(&f, Engine::Docker, Flavor::Timescaledb).is_ok());
        assert!(flavor_offer(&f, Engine::Native, Flavor::Timescaledb).unwrap_err().contains("Docker is on this server"));
        // Docker without Compose, and a server systemd does not run.
        assert!(engine_offer(&facts(true, false, true), Engine::Docker, "x").unwrap_err().starts_with("Docker Compose is missing"));
        let f = facts(true, true, false);
        assert!(engine_offer(&f, Engine::Native, "x").unwrap_err().contains("choose Docker"));
        assert_eq!(default_engine(&f), Engine::Docker);
    }

    #[test]
    fn native_is_off_on_an_older_glibc_and_on_alpine() {
        let min = glibc::min_glibc(Some("2.38"), "4.4.0.3");
        let mut f = facts(true, true, true);
        f.libc = Libc::Glibc { version: "2.36".into() };
        let why = f.native_glibc("4.4.0.3", min);
        assert!(why.blocked().is_some_and(|t| t.starts_with("SENTIENT 4.4.0.3 needs glibc 2.38 or newer; this server has 2.36 (Debian GNU/Linux 12 (bookworm))")));
        f.libc = Libc::Glibc { version: "2.39".into() };
        assert_eq!(f.native_glibc("4.4.0.3", min), NativeGlibc::Allowed);
        // An agent older than 0.9.3 does not say: a warning, except on Alpine (musl).
        let old: ServerFacts = serde_json::from_str(r#"{"docker":true,"systemd":true,"os_name":"Debian GNU/Linux 12 (bookworm)","distro":"debian"}"#).unwrap();
        assert_eq!(old.libc, Libc::Unknown);
        assert!(matches!(old.native_glibc("4.4.0.3", min), NativeGlibc::Unknown(_)));
        let alpine = ServerFacts { distro: "alpine".into(), os_name: "Alpine Linux v3.22".into(), ..old };
        assert!(alpine.native_glibc("4.4.0.3", min).blocked().is_some_and(|t| t.contains("has musl (Alpine Linux v3.22)")));
        // The agent's answer read back.
        let json = serde_json::to_string(&ServerFacts { libc: Libc::Glibc { version: "2.36".into() }, ..ServerFacts::default() }).unwrap();
        assert!(json.contains(r#""libc":{"kind":"glibc","version":"2.36"}"#));
    }

    #[test]
    fn a_running_install_is_known_by_its_job_id() {
        assert_eq!(running_install(Some("20261119101500-install")), Some("20261119101500-install"));
        assert_eq!(running_install(Some("20261119101500")), None);
        assert_eq!(running_install(Some("20261119101500-gateway")), None);
        assert_eq!(running_install(None), None);
    }

    #[test]
    fn a_window_opened_later_finds_the_running_install_again() {
        // The id the install starts under, as the server lists its unit
        // (systemd) or its process (a per-user agent), read back by the
        // status every window reads (RemotePages re-attaches with it).
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-19T10:15:00Z").unwrap().with_timezone(&chrono::Utc);
        let id = super::super::job_id(at, Some(JOB_TAG));
        assert_eq!(id, "20261119101500-install");
        let unit = format!("spm-job-{id}.service loaded active running /opt/sentient-platform-manager/sentient-platform-manager --job --json install /home/ops/.spm-install-plan-1.json --staged");
        let found = super::super::running_of(&unit);
        assert_eq!(running_install(found.as_deref()), Some(id.as_str()));
        assert_eq!(running_install(super::super::running_of(&format!("user-job-{id}")).as_deref()), Some(id.as_str()));
        // Another job running there is not an install.
        let other = super::super::job_id(at, None);
        assert_eq!(running_install(super::super::running_of(&format!("spm-job-{other}.service loaded active running")).as_deref()), None);
    }

    #[test]
    fn ports_go_to_the_check_without_spaces() {
        let a = ports_arg(&[(8080, "Web"), (8883, "MQTT over TLS")]);
        assert_eq!(a, "8080:Web,8883:MQTT-over-TLS");
        assert_eq!(parse_ports(&a), [(8080, "Web".to_string()), (8883, "MQTT over TLS".to_string())]);
    }

    #[test]
    fn checks_read_back_from_the_agent() {
        let c = sentient_installer_core::checks::run_for("linux-native", true, Some(&[(1, "x".to_string())]));
        let rows: Vec<FactCheck> = c.into_iter().filter_map(|c| serde_json::to_value(&c).ok().and_then(|v| serde_json::from_value(v).ok())).collect();
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| ["pass", "setup", "fail", "warn", "unknown"].contains(&r.status.as_str())));
    }

    /// End to end on the throwaway test bed (a bare Debian with systemd,
    /// no Docker): SPM_E2E="port", SPM_E2E_KEY, SPM_E2E_SUDO, SPM_AGENT_DIR
    /// (the static agent), XDG_CONFIG_HOME scratch. Installs SENTIENT with
    /// PostgreSQL in the system as the flow does, follows it after a
    /// "dropped" follow by its id, checks nothing secret stayed on the
    /// server, and removes it again (SPM_E2E_KEEP=1 keeps it).
    #[test]
    #[ignore]
    fn installs_sentient_on_the_test_bed() {
        use super::super::*;
        let port: u16 = std::env::var("SPM_E2E").expect("SPM_E2E").parse().unwrap();
        let _ = std::fs::remove_file(known_hosts());
        let mut inst = Instance {
            id: format!("e2e{port}"),
            name: "PLANT_B".into(),
            color: PALETTE[0].into(),
            os: Os::Linux,
            host: "127.0.0.1".into(),
            port,
            user: "ops".into(),
            auth: Auth::Key,
            key_file: Some(std::env::var("SPM_E2E_KEY").unwrap().into()),
            key_has_passphrase: false,
            host_key: None,
            detected: None,
        };
        let (keys, fp) = scan("127.0.0.1", port).unwrap();
        trust(&keys).unwrap();
        inst.host_key = Some(fp);
        inst.detected = Some(detect(&inst, None).unwrap());
        set_secret(&sudo_key(&inst.id), &std::env::var("SPM_E2E_SUDO").unwrap());
        let sink: ProgressFn = std::sync::Arc::new(|p| eprintln!("  {p:?}"));
        deploy(&inst, &sink).unwrap();
        // What the flow reads on System check.
        let facts = super::facts(&inst, &[]).unwrap();
        eprintln!("facts: docker={} compose={} systemd={} os={} disk={:?}", facts.docker, facts.compose, facts.systemd, facts.os_name, facts.disk_free);
        for c in &facts.native_checks {
            eprintln!("  check {} {} {}", c.status, c.label, c.detail);
        }
        assert!(facts.systemd && !facts.docker);
        assert!(engine_offer(&facts, Engine::Docker, &inst.name).is_err());
        assert!(flavor_offer(&facts, Engine::Native, Flavor::Timescaledb).is_err());
        assert!(!facts.native_checks.iter().any(|c| c.blocks()), "{:?}", facts.native_checks);
        let busy = super::facts(&inst, &[(22, "SSH")]).unwrap();
        assert!(busy.native_checks.iter().any(|c| c.id == "ports" && c.status != "pass"), "port 22 is in use there: {:?}", busy.native_checks);
        // Staged passwords: root's only, until the job takes them.
        let (_, secrets) = plan().split_secrets();
        job_on_stdin(&inst, &["--json".to_string(), "install-secrets".to_string()], &format!("{}\n", serde_json::to_string(&secrets).unwrap())).unwrap();
        let mode = sudo_run(&inst, "stat -c '%a %U' /var/lib/sentient-platform-manager/install.secret", None).unwrap();
        assert_eq!(mode.trim(), "600 root");
        // The install, as the flow starts it (PostgreSQL only: the lighter one).
        let version = std::env::var("SPM_E2E_VERSION").ok().or_else(|| crate::updates::newest_sentient(sentient_installer_core::releases::Channel::Stable).unwrap()).expect("a SENTIENT release");
        let mut p = plan();
        p.stack.flavor = Flavor::Stock;
        p.sentient_version = version.clone();
        p.download = Some(version.clone());
        let id = start(&inst, p).unwrap();
        assert_eq!(running_install(Some(&id)), Some(id.as_str()));
        std::thread::sleep(std::time::Duration::from_secs(5));
        // A window opened now finds it by the status's running job.
        let st = status(&inst).unwrap();
        assert_eq!(running_install(st.running_job.as_deref()), Some(id.as_str()), "{st:?}");
        // The passwords were taken; the plan file is gone.
        let left = sudo_run(&inst, "ls /var/lib/sentient-platform-manager/install.secret /home/ops/.spm-install-plan-* 2>/dev/null; true", None).unwrap();
        assert!(left.trim().is_empty(), "left on the server: {left}");
        follow(&inst, &id, &sink).unwrap();
        let st = status(&inst).unwrap();
        eprintln!("after: {st:?}");
        assert!(st.installed && st.sentient_version.contains(&version), "{st:?}");
        // Nothing secret in the job's log, the journal or the record.
        for s in [ADMIN, DB, PG_ADMIN] {
            // The pattern split in two quoted halves: this command's own
            // line (sudo logs it) never holds the password whole.
            let (a, b) = s.split_at(s.len() / 2);
            let pat = format!("'{a}''{b}'");
            let hits = sudo_run(&inst, &format!("grep -rl -- {pat} /var/lib/sentient-platform-manager /home/ops 2>/dev/null; journalctl --no-pager 2>/dev/null | grep -c -- {pat}; true"), None).unwrap();
            assert!(hits.lines().all(|l| l.trim() == "0" || l.trim().is_empty()), "{s} found: {hits}");
        }
        if std::env::var_os("SPM_E2E_KEEP").is_none() {
            let id = start_job(&inst, &["uninstall", "--delete-data"]).unwrap();
            follow(&inst, &id, &sink).unwrap();
            assert!(!status(&inst).unwrap().installed);
        }
    }
}
