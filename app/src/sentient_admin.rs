// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT's own administration command (`sentient-admin`, SENTIENT
//! 4.4.0.3 and later): the system administrator accounts, password reset,
//! unlock, signing everyone out, and the default retention. The Platform
//! Manager never writes passwords or sign-in state into the database
//! itself: SENTIENT's command does, the way the server does, and writes the
//! audit log entry.
//!
//! Where it runs: inside the SENTIENT container (Docker; on Windows in the
//! WSL distro Docker runs in, through wsl.exe), or beside the server with
//! its database address (native; on Linux as root, through
//! crate::rootjob, as the address holds a root-only password). Stopped
//! SENTIENT is not asked, and a stopped WSL distro is never started for
//! it ([`Failure::Stopped`]).

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::installation::{Engine, InstallationRecord};

/// What the command says when the installed SENTIENT does not have it.
pub const MISSING: &str = n!("This SENTIENT version has no administration command (sentient-admin comes with SENTIENT 4.4.0.3). Update SENTIENT first.");

/// What the page says when SENTIENT (or, on Windows, its WSL distro) is
/// stopped: nothing was asked, and nothing was started to ask.
pub const STOPPED: &str = n!("SENTIENT must be running to read and change its settings.");

/// Where sentient-admin is in SENTIENT's image.
const IN_IMAGE: &str = "/usr/share/sentient/bin/sentient-admin";

/// Why the command gave no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// SENTIENT is stopped (its container, its database, or on Windows its
    /// WSL distro, which a read never starts): not asked. Not an error.
    Stopped,
    /// It ran and failed, or could not be run: what it said.
    Error(String),
}

impl Failure {
    /// The words a page shows.
    pub fn message(&self) -> String {
        match self {
            Failure::Stopped => STOPPED.to_string(),
            Failure::Error(e) => e.clone(),
        }
    }
}

impl From<String> for Failure {
    fn from(e: String) -> Self {
        Failure::Error(e)
    }
}

/// How sentient-admin runs for an installation.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// Native Linux without root: the agent's `admin` job, as root
    /// (crate::rootjob), as the database address holds a root-only password.
    RootJob,
    /// Docker: in SENTIENT's container `container`, where Docker runs (on
    /// Windows inside the WSL distro).
    Docker { container: String },
    /// Native: the program beside the server, with the database address.
    Native { exe: std::path::PathBuf, database_url: String },
}

/// What runs: a root job's arguments, or a program with its arguments
/// and variables.
#[derive(Debug, Clone, PartialEq)]
enum Plan {
    Job(Vec<String>),
    Program { program: String, args: Vec<String>, env: Vec<(String, String)> },
}

/// How a command runs in a container here (container, its argv):
/// sentient_installer_core::distro::docker_exec_command.
type DockerExec<'a> = &'a dyn Fn(&str, &[String]) -> (String, Vec<String>);

/// The command `kind` runs for `sentient-admin <args>`; `docker` builds
/// the one in a container.
fn plan(kind: &Kind, args: &[String], docker: DockerExec<'_>) -> Plan {
    match kind {
        Kind::RootJob => Plan::Job(std::iter::once("admin".to_string()).chain(args.iter().cloned()).collect()),
        Kind::Docker { container } => {
            let argv: Vec<String> = std::iter::once(IN_IMAGE.to_string()).chain(args.iter().cloned()).collect();
            let (program, args) = docker(container, &argv);
            Plan::Program { program, args, env: Vec::new() }
        }
        Kind::Native { exe, database_url } => {
            Plan::Program { program: exe.display().to_string(), args: args.to_vec(), env: vec![("DATABASE_URL".into(), database_url.clone())] }
        }
    }
}

/// Whether what the command said means SENTIENT is stopped: its container
/// is not running, Docker is not, or its database refuses connections.
fn stopped_answer(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("is not running")
        || t.contains("cannot connect to the docker daemon")
        || t.contains("connection refused")
        // Docker itself not started yet (its distro just booted, or after
        // a reboot with SENTIENT off): no socket to talk to.
        || t.contains("failed to connect to the docker api")
        || t.contains("is the docker daemon running")
        || t.contains("docker.sock")
        // Its database still starting or gone.
        || t.contains("the database system is starting up")
        || t.contains("the database system is shutting down")
}

/// Run `sentient-admin <args>`, giving it `input` on standard input. Returns
/// what it printed. Blocking. Never starts SENTIENT or its WSL distro to
/// ask (sentient_installer_core::distro::idle): stopped, it is not asked.
pub fn run(record: &InstallationRecord, args: &[String], input: Option<&str>) -> Result<String, Failure> {
    // Development aid: a sentient-admin of a local build, with this
    // process's DATABASE_URL (screens and tests against a scratch
    // database), or SENTIENT stopped (SPM_DEBUG_SENTIENT_ADMIN=stopped).
    #[cfg(debug_assertions)]
    if let Some(bin) = std::env::var_os("SPM_DEBUG_SENTIENT_ADMIN") {
        if bin == "stopped" {
            return Err(Failure::Stopped);
        }
        let mut c = Command::new(bin);
        c.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| e.to_string())?;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            let _ = writeln!(stdin, "{text}");
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(if out.status.success() { &out.stdout } else { &out.stderr }).trim().to_string();
        return if out.status.success() { Ok(text) } else { Err(Failure::Error(text)) };
    }
    let kind = if crate::rootjob::needed(record) {
        Kind::RootJob
    } else {
        match record.engine {
            Engine::Docker => {
                // Windows: a stopped distro stays stopped (any wsl.exe -d
                // starts it, and SENTIENT with it).
                if !sentient_installer_core::distro::idle::running(&sentient_installer_core::distro::docker_distro()) {
                    return Err(Failure::Stopped);
                }
                Kind::Docker { container: record.stack.names().app }
            }
            Engine::Native => {
                let exe = record.install_dir.join("bin").join(if cfg!(windows) { "sentient-admin.exe" } else { "sentient-admin" });
                if !exe.exists() {
                    return Err(MISSING.to_string().into());
                }
                let cfg = record.native_config_with_secrets()?;
                Kind::Native { exe, database_url: format!("postgresql://{}:{}@127.0.0.1:{}/{}", cfg.db_user, cfg.db_password, cfg.pg_port, cfg.db_name) }
            }
        }
    };
    let (program, args, env) = match plan(&kind, args, &sentient_installer_core::distro::docker_exec_command) {
        Plan::Job(job) => {
            return crate::rootjob::run_with_input(&job, input, &sentient_installer_core::progress::noop()).map(|lines| lines.join("\n")).map_err(classify);
        }
        Plan::Program { program, args, env } => (program, args, env),
    };
    // A read never starts a stopped WSL distro (it may have stopped since
    // the check above): left out, SENTIENT is stopped.
    if program.eq_ignore_ascii_case("wsl.exe") && sentient_installer_core::distro::left_out(&args.iter().map(String::as_str).collect::<Vec<_>>()) {
        return Err(Failure::Stopped);
    }
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut cmd = sentient_installer_core::host::host_command_env(&program, &env);
    cmd.args(&args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("start sentient-admin ({program}): {e}"))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = writeln!(stdin, "{text}");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = sentient_installer_core::host::decode(&out.stdout).trim().to_string();
    let stderr = sentient_installer_core::host::decode(&out.stderr).trim().to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    // Docker: the image has no such file (an older SENTIENT).
    let both = format!("{stdout}\n{stderr}");
    let lower = both.to_lowercase();
    if lower.contains("sentient-admin") && (lower.contains("no such file") || lower.contains("not found")) {
        return Err(MISSING.to_string().into());
    }
    Err(classify(if stderr.is_empty() { stdout } else { stderr }))
}

/// A failure's words, as [`Failure`]: SENTIENT stopped, or an error.
fn classify(text: String) -> Failure {
    if text.contains(STOPPED) || stopped_answer(&text) { Failure::Stopped } else { Failure::Error(text) }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A system administrator account, as `list-admins --json` reports it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Admin {
    pub email: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub failed_sign_ins: i64,
    pub locked_since: Option<i64>,
    pub last_sign_in: Option<i64>,
}

/// The default retention, as `retention show --json` reports it (days, 0 = forever).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Retention {
    pub telemetry_days: i64,
    pub alarms_days: i64,
}

pub fn admins(record: &InstallationRecord) -> Result<Vec<Admin>, Failure> {
    let out = run(record, &args(&["list-admins", "--json"]), None)?;
    serde_json::from_str(out.lines().last().unwrap_or("[]")).map_err(|e| Failure::Error(format!("unexpected answer from sentient-admin: {e}")))
}

pub fn reset_password(record: &InstallationRecord, email: &str, password: &str) -> Result<String, Failure> {
    run(record, &args(&["reset-password", "--email", email, "--password-stdin"]), Some(password))
}

pub fn unlock(record: &InstallationRecord, email: &str) -> Result<String, Failure> {
    run(record, &args(&["unlock", "--email", email]), None)
}

pub fn sign_out_all(record: &InstallationRecord) -> Result<String, Failure> {
    run(record, &args(&["sign-out", "--all"]), None)
}

pub fn retention(record: &InstallationRecord) -> Result<Retention, Failure> {
    let out = run(record, &args(&["retention", "show", "--json"]), None)?;
    serde_json::from_str(out.lines().last().unwrap_or("{}")).map_err(|e| Failure::Error(format!("unexpected answer from sentient-admin: {e}")))
}

pub fn set_retention(record: &InstallationRecord, r: Retention) -> Result<String, Failure> {
    run(
        record,
        &args(&["retention", "set", "--telemetry-days", &r.telemetry_days.to_string(), "--alarms-days", &r.alarms_days.to_string()]),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(list: &[&str]) -> Vec<String> {
        args(list)
    }

    #[test]
    fn each_kind_runs_the_command_where_sentient_is() {
        use sentient_installer_core::distro::docker_exec_args;
        let list = a(&["list-admins", "--json"]);
        // Native Linux without root: the agent's admin job, as root.
        assert_eq!(plan(&Kind::RootJob, &list, &|_, _| unreachable!()), Plan::Job(a(&["admin", "list-admins", "--json"])));
        // Windows Docker: inside the distro Docker runs in, never a program
        // on Windows (there is none: "program not found").
        let docker = Kind::Docker { container: "sentient".into() };
        let windows = |c: &str, v: &[String]| docker_exec_args(true, "sentient", false, c, v);
        let Plan::Program { program, args, env } = plan(&docker, &list, &windows) else { panic!("a program") };
        assert_eq!(program, "wsl.exe");
        assert_eq!(args, a(&["-d", "sentient", "-u", "root", "--exec", "docker", "exec", "-i", "sentient", IN_IMAGE, "list-admins", "--json"]));
        assert!(env.is_empty());
        // Linux Docker: Docker's shell, every argument in its one line
        // (bash, or `sg docker` right after setup).
        let linux = |c: &str, v: &[String]| docker_exec_args(false, "", false, c, v);
        let docker = Kind::Docker { container: "st-app".into() };
        assert_eq!(
            plan(&docker, &a(&["unlock", "--email", "a@b"]), &linux),
            Plan::Program { program: "bash".into(), args: a(&["-lc", "exec docker exec -i 'st-app' '/usr/share/sentient/bin/sentient-admin' 'unlock' '--email' 'a@b'"]), env: vec![] }
        );
        let sg = |c: &str, v: &[String]| docker_exec_args(false, "", true, c, v);
        let Plan::Program { program, args, .. } = plan(&docker, &list, &sg) else { panic!("a program") };
        assert_eq!((program.as_str(), args[0].as_str(), args[1].as_str()), ("sg", "docker", "-c"));
        assert!(args[2].ends_with("'list-admins' '--json'"), "{}", args[2]);
        // Native (Windows, Linux as root): beside the server, with the
        // database address, nothing through Docker.
        let exe = std::path::PathBuf::from("C:/SENTIENT/bin/sentient-admin.exe");
        let url = "postgresql://sentient:pw@127.0.0.1:5432/sentient".to_string();
        assert_eq!(
            plan(&Kind::Native { exe: exe.clone(), database_url: url.clone() }, &a(&["sign-out", "--all"]), &|_, _| unreachable!()),
            Plan::Program { program: exe.display().to_string(), args: a(&["sign-out", "--all"]), env: vec![("DATABASE_URL".into(), url)] }
        );
    }

    #[test]
    fn stopped_sentient_is_not_an_error() {
        assert_eq!(classify("Error response from daemon: container 4f2a is not running".into()), Failure::Stopped);
        assert_eq!(classify("Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?".into()), Failure::Stopped);
        assert_eq!(classify("error connecting to database: Connection refused (os error 111)".into()), Failure::Stopped);
        // The rc5 VM: the distro running, Docker not started.
        assert_eq!(
            classify("failed to connect to the docker API at unix:///var/run/docker.sock; check if the path is correct and if the daemon is running: dial unix /var/run/docker.sock: connect: no such file or directory".into()),
            Failure::Stopped
        );
        assert_eq!(classify("Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?".into()), Failure::Stopped);
        assert_eq!(classify("FATAL: the database system is starting up".into()), Failure::Stopped);
        // Across the root job: its words.
        assert_eq!(classify(STOPPED.into()), Failure::Stopped);
        assert_eq!(classify("no such user".into()), Failure::Error("no such user".into()));
        assert_eq!(Failure::Stopped.message(), STOPPED);
    }

    #[test]
    fn the_commands_answers_read() {
        let a: Vec<Admin> = serde_json::from_str(r#"[{"email":"a@b","enabled":true,"failedSignIns":5,"lastSignIn":null,"lockedSince":123}]"#).unwrap();
        assert_eq!(a[0].failed_sign_ins, 5);
        assert_eq!(a[0].locked_since, Some(123));
        let r: Retention = serde_json::from_str(r#"{"telemetryDays":365,"alarmsDays":90}"#).unwrap();
        assert_eq!(r, Retention { telemetry_days: 365, alarms_days: 90 });
    }
}
