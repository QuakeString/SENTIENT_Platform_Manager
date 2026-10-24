// Copyright © 2016-2026 The SENTIENT Authors

//! Native Linux installations keep their passwords and services root-only
//! (the environment file, systemd, `runuser postgres`). The manager runs that
//! work as `pkexec <itself> --job --json …`, one password prompt for the whole
//! job, and streams the child's progress back. Elsewhere, or already as root,
//! the work runs in this process.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};

pub fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

/// This installation's maintenance has to run as root from here.
pub fn needed(record: &InstallationRecord) -> bool {
    cfg!(target_os = "linux") && record.engine == Engine::Native && !is_root()
}

/// Run `--job <args>` as root, forwarding its progress to `sink`. Returns the
/// job's log lines (for jobs that answer with data, like `kept`).
pub fn run(args: &[String], sink: &ProgressFn) -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("find this program: {e}"))?;
    let have_pkexec = Command::new("sh").args(["-c", "command -v pkexec"]).output().map(|o| o.status.success()).unwrap_or(false);
    let mut cmd = if have_pkexec {
        let mut c = Command::new("pkexec");
        c.arg(&exe);
        c
    } else {
        let mut c = Command::new("sudo");
        c.arg("-n").arg(&exe);
        c
    };
    cmd.arg("--job").arg("--json").args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("start the job as administrator: {e}"))?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let err_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut text);
        text
    });
    let mut lines = Vec::new();
    let mut failure = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        match serde_json::from_str::<Progress>(&line) {
            Ok(Progress::Error { message }) => failure = Some(message),
            Ok(Progress::Log { line }) => {
                lines.push(line.clone());
                sink(Progress::Log { line });
            }
            Ok(p) => sink(p),
            Err(_) => sink(Progress::Log { line }),
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let stderr = err_reader.join().unwrap_or_default();
    if status.success() {
        return Ok(lines);
    }
    if have_pkexec && matches!(status.code(), Some(126) | Some(127)) {
        return Err("Administrator authorisation was refused or cancelled.".into());
    }
    Err(failure.unwrap_or_else(|| {
        let e = stderr.trim();
        if e.is_empty() { format!("the job ended with {status}") } else { e.to_string() }
    }))
}
