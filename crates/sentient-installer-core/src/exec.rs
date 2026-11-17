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
    let mut gate = Throttle::default();
    for line in std::io::BufReader::new(reader).lines().map_while(Result::ok) {
        if gate.pass(&line) {
            sink(Progress::Log { line });
        }
    }
}

/// The rate limit of [`throttle_lines`], line by line.
#[derive(Default)]
pub(crate) struct Throttle {
    last: Option<std::time::Instant>,
}

impl Throttle {
    pub(crate) fn pass(&mut self, line: &str) -> bool {
        use std::time::{Duration, Instant};
        let lower = line.to_ascii_lowercase();
        // ">> " marks a milestone echoed by one of our own scripts.
        // "!! " marks our own scripts' error lines: never dropped.
        let important = line.starts_with(">> ")
            || line.starts_with("!! ")
            || lower.contains("error")
            || lower.contains("failed")
            || lower.contains("warning")
            || line.contains("Pulled")
            || line.contains("Pull complete")
            || line.contains("Started")
            || line.contains("Healthy");
        let now = Instant::now();
        let due = self.last.map_or(true, |t| now.duration_since(t) >= Duration::from_millis(150));
        if important || due {
            self.last = Some(now);
        }
        important || due
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

/// Run a script as root, streaming its output.
///
/// Every root step of this process goes to one helper (crate::elevation):
/// the first asks for the password, the rest of the job does not. Outside a
/// user action it never asks; it runs only when a helper is already alive.
#[cfg(unix)]
pub(crate) fn run_privileged(sink: &ProgressFn, script: &str) -> Result<(), String> {
    if is_root() {
        return stream(sink, "bash", &["-c", script]);
    }
    // The password dialog can open behind the window that asked for it, and
    // the job then waits with nothing to show. Say so when it opens.
    let hint = || sink(Progress::Log { line: "Waiting for the administrator password: if nothing moves, look for the password dialog (it may be behind this window).".into() });
    let mut gate = Throttle::default();
    let mut on_line = |_: crate::elevation::Stream, line: String| {
        if gate.pass(&line) {
            sink(Progress::Log { line });
        }
    };
    let argv = ["bash".to_string(), "-c".to_string(), script.to_string()];
    let cancelled_before = crate::cancel::is_cancelled();
    let code = crate::elevation::run(&argv, None, true, &mut on_line, &hint).map_err(|e| e.to_string())?;
    if !cancelled_before && crate::cancel::is_cancelled() {
        Err("Cancelled.".into())
    } else if code == 0 {
        Ok(())
    } else {
        Err(format!("bash exited with {code}"))
    }
}

/// Run a script as root and capture its output (for probes, not long
/// steps). Like [`run_privileged`] it asks for the password only inside a
/// user action; a page read gets None-like failure instead (ok = false,
/// stderr saying why).
#[cfg(unix)]
pub(crate) fn privileged_output(script: &str) -> Option<(bool, String, String)> {
    if is_root() {
        return sys::output("bash", &["-c", script]).map(|(ok, out, err)| {
            (ok, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
        });
    }
    match crate::elevation::output(&["bash".to_string(), "-c".to_string(), script.to_string()]) {
        Ok(r) => Some(r),
        Err(e) => Some((false, String::new(), e.to_string())),
    }
}

/// Single-quote a value for a POSIX shell.
#[cfg(unix)]
pub(crate) fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
