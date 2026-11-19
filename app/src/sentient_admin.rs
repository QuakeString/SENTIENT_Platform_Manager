// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT's own administration command (`sentient-admin`, SENTIENT
//! 4.4.0.3 and later): the system administrator accounts, password reset,
//! unlock, signing everyone out, and the default retention. The Platform
//! Manager never writes passwords or sign-in state into the database
//! itself: SENTIENT's command does, the way the server does, and writes the
//! audit log entry.
//!
//! Where it runs: inside the SENTIENT container (Docker), or beside the
//! server with its database address (native; on Linux as root, through
//! crate::rootjob, as the address holds a root-only password).

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::installation::{Engine, InstallationRecord};

/// What the command says when the installed SENTIENT does not have it.
pub const MISSING: &str = n!("This SENTIENT version has no administration command (sentient-admin comes with SENTIENT 4.4.0.3). Update SENTIENT first.");

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

/// Run `sentient-admin <args>`, giving it `input` on standard input. Returns
/// what it printed. Blocking.
pub fn run(record: &InstallationRecord, args: &[String], input: Option<&str>) -> Result<String, String> {
    if crate::rootjob::needed(record) {
        let mut job = vec!["admin".to_string()];
        job.extend(args.iter().cloned());
        return crate::rootjob::run_with_input(&job, input, &sentient_installer_core::progress::noop()).map(|lines| lines.join("\n"));
    }
    // Development aid: a sentient-admin of a local build, with this
    // process's DATABASE_URL (screens and tests against a scratch database).
    #[cfg(debug_assertions)]
    if let Some(bin) = std::env::var_os("SPM_DEBUG_SENTIENT_ADMIN") {
        let mut c = Command::new(bin);
        c.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| e.to_string())?;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            let _ = writeln!(stdin, "{text}");
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(if out.status.success() { &out.stdout } else { &out.stderr }).trim().to_string();
        return if out.status.success() { Ok(text) } else { Err(text) };
    }
    let mut cmd = match record.engine {
        Engine::Docker => {
            let mut c = sentient_installer_core::host::host_command("docker");
            c.args(["exec", "-i", &record.stack.names().app, "/usr/share/sentient/bin/sentient-admin"]);
            c
        }
        Engine::Native => {
            let exe = record.install_dir.join("bin").join(if cfg!(windows) { "sentient-admin.exe" } else { "sentient-admin" });
            if !exe.exists() {
                return Err(MISSING.into());
            }
            let cfg = record.native_config_with_secrets()?;
            let url = format!("postgresql://{}:{}@127.0.0.1:{}/{}", cfg.db_user, cfg.db_password, cfg.pg_port, cfg.db_name);
            #[allow(unused_mut)]
            let mut c = sentient_installer_core::host::host_command_env(exe, &[("DATABASE_URL", url.as_str())]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            c
        }
    };
    cmd.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| nf!("start sentient-admin: {e}"))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = writeln!(stdin, "{text}");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    // Docker: the image has no such file (an older SENTIENT).
    let both = format!("{stdout}\n{stderr}").to_lowercase();
    if both.contains("sentient-admin") && (both.contains("no such file") || both.contains("not found")) {
        return Err(MISSING.into());
    }
    Err(if stderr.is_empty() { stdout } else { stderr })
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

pub fn admins(record: &InstallationRecord) -> Result<Vec<Admin>, String> {
    let out = run(record, &args(&["list-admins", "--json"]), None)?;
    serde_json::from_str(out.lines().last().unwrap_or("[]")).map_err(|e| nf!("unexpected answer from sentient-admin: {e}"))
}

pub fn reset_password(record: &InstallationRecord, email: &str, password: &str) -> Result<String, String> {
    run(record, &args(&["reset-password", "--email", email, "--password-stdin"]), Some(password))
}

pub fn unlock(record: &InstallationRecord, email: &str) -> Result<String, String> {
    run(record, &args(&["unlock", "--email", email]), None)
}

pub fn sign_out_all(record: &InstallationRecord) -> Result<String, String> {
    run(record, &args(&["sign-out", "--all"]), None)
}

pub fn retention(record: &InstallationRecord) -> Result<Retention, String> {
    let out = run(record, &args(&["retention", "show", "--json"]), None)?;
    serde_json::from_str(out.lines().last().unwrap_or("{}")).map_err(|e| nf!("unexpected answer from sentient-admin: {e}"))
}

pub fn set_retention(record: &InstallationRecord, r: Retention) -> Result<String, String> {
    run(
        record,
        &args(&["retention", "set", "--telemetry-days", &r.telemetry_days.to_string(), "--alarms-days", &r.alarms_days.to_string()]),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_answers_read() {
        let a: Vec<Admin> = serde_json::from_str(r#"[{"email":"a@b","enabled":true,"failedSignIns":5,"lastSignIn":null,"lockedSince":123}]"#).unwrap();
        assert_eq!(a[0].failed_sign_ins, 5);
        assert_eq!(a[0].locked_since, Some(123));
        let r: Retention = serde_json::from_str(r#"{"telemetryDays":365,"alarmsDays":90}"#).unwrap();
        assert_eq!(r, Retention { telemetry_days: 365, alarms_days: 90 });
    }
}
