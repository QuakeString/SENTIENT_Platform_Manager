//! Process plumbing shared by the engines: live line streaming into a
//! progress sink, and — on Unix — running a script as root through polkit.

use std::process::Stdio;

use crate::progress::{Progress, ProgressFn};
use crate::sys;

/// Spawn `program args…`, stream stdout/stderr line-by-line into the sink,
/// and fail on a non-zero exit. Cancel-aware: the child's pid is registered so
/// a Cancel request can kill it, and cancellation wins over the exit status.
pub(crate) fn stream(sink: &ProgressFn, program: &str, args: &[&str]) -> Result<(), String> {
    let mut child = sys::command(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    crate::cancel::register_pid(child.id());
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (s1, s2) = (sink.clone(), sink.clone());
    let t1 = std::thread::spawn(move || throttle_lines(stdout, &s1));
    let t2 = std::thread::spawn(move || throttle_lines(stderr, &s2));
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = t1.join();
    let _ = t2.join();
    crate::cancel::clear_pid();
    if crate::cancel::is_cancelled() {
        Err("Cancelled.".into())
    } else if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {}", status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into())))
    }
}

/// Forward lines to the sink at a bounded rate. `docker compose pull` and
/// package managers emit hundreds of transient progress lines a second;
/// forwarding every one floods the IPC channel and spikes the UI's CPU. Lines
/// that look like errors or milestones always pass through.
pub(crate) fn throttle_lines<R: std::io::Read>(reader: R, sink: &ProgressFn) {
    use std::io::BufRead;
    use std::time::{Duration, Instant};
    let mut last: Option<Instant> = None;
    for line in std::io::BufReader::new(reader).lines().map_while(Result::ok) {
        let lower = line.to_ascii_lowercase();
        // ">> " marks a milestone echoed by one of our own scripts.
        let important = line.starts_with(">> ")
            || lower.contains("error")
            || lower.contains("failed")
            || lower.contains("warning")
            || line.contains("Pulled")
            || line.contains("Pull complete")
            || line.contains("Started")
            || line.contains("Healthy");
        let now = Instant::now();
        let due = last.map_or(true, |t| now.duration_since(t) >= Duration::from_millis(150));
        if important || due {
            last = Some(now);
            sink(Progress::Log { line });
        }
    }
}

/// Emit captured output as log lines, skipping blanks.
#[allow(dead_code)]
pub(crate) fn emit(sink: &ProgressFn, bytes: &[u8]) {
    for line in sys::decode(bytes).lines() {
        let l = line.trim();
        if !l.is_empty() {
            sink(Progress::Log { line: l.into() });
        }
    }
}

/// Is `bin` on PATH?
#[cfg(unix)]
pub(crate) fn have(bin: &str) -> bool {
    sys::output("sh", &["-c", &format!("command -v {bin}")]).map(|(ok, _, _)| ok).unwrap_or(false)
}

#[cfg(unix)]
pub(crate) fn is_root() -> bool {
    sys::output("id", &["-u"])
        .map(|(ok, out, _)| ok && String::from_utf8_lossy(&out).trim() == "0")
        .unwrap_or(false)
}

/// How to run `script` as root from a desktop session.
///
/// pkexec puts up the desktop's own password dialog and — through polkit's
/// `auth_admin_keep` — remembers the answer for a few minutes, so a multi-step
/// install asks once rather than per step. `sudo -n` is the headless
/// fallback (a harness running under sudo, or a passwordless rule); it fails
/// fast instead of hanging on a prompt nobody can see.
#[cfg(unix)]
pub(crate) fn privileged_args(script: &str) -> (String, Vec<String>) {
    if is_root() {
        ("bash".into(), vec!["-c".into(), script.into()])
    } else if have("pkexec") {
        ("pkexec".into(), vec!["bash".into(), "-c".into(), script.into()])
    } else {
        ("sudo".into(), vec!["-n".into(), "bash".into(), "-c".into(), script.into()])
    }
}

/// Run a script as root, streaming its output.
#[cfg(unix)]
pub(crate) fn run_privileged(sink: &ProgressFn, script: &str) -> Result<(), String> {
    let (prog, args) = privileged_args(script);
    // The password dialog can open behind the window that asked for it, and
    // the job then waits with nothing to show. Say so, once per run.
    static HINTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if prog == "pkexec" && !HINTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        sink(Progress::Log { line: "Waiting for the administrator password: if nothing moves, look for the password dialog (it may be behind this window).".into() });
    }
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    stream(sink, &prog, &argrefs).map_err(|e| {
        // pkexec itself exits 126 when the dialog is dismissed and 127 when
        // authorisation fails; the same codes from bash mean something else.
        if prog == "pkexec" && (e.contains("exited with 126") || e.contains("exited with 127")) {
            "Administrator authorisation was refused or cancelled.".to_string()
        } else {
            e
        }
    })
}

/// Run a script as root and capture its output (for probes, not long steps).
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn privileged_output(script: &str) -> Option<(bool, String, String)> {
    let (prog, args) = privileged_args(script);
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    sys::output(&prog, &argrefs).map(|(ok, out, err)| {
        (ok, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    })
}

/// Single-quote a value for a POSIX shell.
#[cfg(unix)]
pub(crate) fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
