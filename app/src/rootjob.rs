// Copyright © 2016-2026 The SENTIENT Authors

//! Native Linux installations keep their passwords and services root-only
//! (the environment file, systemd, `runuser postgres`). The manager runs that
//! work as `<itself> --job --json …` in its one root helper (one password
//! prompt for the whole job, see sentient_installer_core::elevation), and streams the child's progress back. Elsewhere, or already as root,
//! the work runs in this process.

use std::process::Command;

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
    run_with_input(args, None, sink)
}

/// [`run`], writing `input` to the job's standard input (a passphrase:
/// pkexec clears the environment, and a command line is visible to others).
///
/// The job runs in the process's one root helper
/// (sentient_installer_core::elevation): the first root job or step asks
/// for the password, later ones while it is alive do not. A closed prompt
/// fails with ElevationError::Cancelled's message.
pub fn run_with_input(args: &[String], input: Option<&str>, sink: &ProgressFn) -> Result<Vec<String>, String> {
    use sentient_installer_core::elevation::{self, Stream};
    let exe = std::env::current_exe().map_err(|e| format!("find this program: {e}"))?;
    let mut argv = vec![exe.display().to_string(), "--job".to_string(), "--json".to_string()];
    argv.extend(args.iter().cloned());
    let mut lines = Vec::new();
    let mut failure = None;
    let mut stderr = String::new();
    let hint = || sink(Progress::Log { line: "Waiting for the administrator password: if nothing moves, look for the password dialog (it may be behind this window).".into() });
    // Not cancellable through the helper: root jobs stop through their own
    // means (a move's cancel file), never by a signal mid-copy.
    let code = elevation::run(
        &argv,
        input,
        false,
        &mut |stream, line| match stream {
            Stream::Err => {
                stderr.push_str(&line);
                stderr.push('\n');
            }
            Stream::Out => match serde_json::from_str::<Progress>(&line) {
                Ok(Progress::Error { message }) => failure = Some(message),
                Ok(Progress::Log { line }) => {
                    lines.push(line.clone());
                    sink(Progress::Log { line });
                }
                Ok(p) => sink(p),
                Err(_) => sink(Progress::Log { line }),
            },
        },
        &hint,
    )
    .map_err(|e| e.to_string())?;
    if code == 0 {
        return Ok(lines);
    }
    Err(failure.unwrap_or_else(|| {
        let e = stderr.trim();
        if e.is_empty() { format!("the job ended with exit status {code}") } else { e.to_string() }
    }))
}
