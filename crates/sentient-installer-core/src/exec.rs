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

/// A root script in a private file (0600, the account's runtime folder),
/// deleted when dropped: bash runs the file, so the script and the secrets
/// in it never show on a command line.
#[cfg(unix)]
struct ScriptFile(std::path::PathBuf, String);

#[cfg(unix)]
impl ScriptFile {
    fn new(script: &str) -> Result<ScriptFile, String> {
        // Inside a Flatpak the root helper runs on the host and cannot read
        // the sandbox's files: the script goes on its command line there.
        if crate::sys::in_flatpak() {
            return Ok(ScriptFile(std::path::PathBuf::new(), script.to_string()));
        }
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("spm-root-{}-{}.sh", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_file(&path);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(|e| format!("write {}: {e}", path.display()))?;
        f.write_all(script.as_bytes()).and_then(|_| f.write_all(b"\n")).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(ScriptFile(path, String::new()))
    }

    /// bash's arguments: the file, or `-c <script>` (a Flatpak).
    fn args(&self) -> Vec<&str> {
        if self.0.as_os_str().is_empty() { vec!["-c", self.1.as_str()] } else { vec![self.0.to_str().unwrap_or_default()] }
    }
}

#[cfg(unix)]
impl Drop for ScriptFile {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
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
    // The script in a private file, never on bash's command line: the
    // passwords and secrets in it would be in every account's `ps` (audit I5).
    let file = ScriptFile::new(script)?;
    if is_root() {
        return stream(sink, "bash", &file.args());
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
    let argv: Vec<String> = std::iter::once("bash".to_string()).chain(file.args().into_iter().map(str::to_string)).collect();
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
    let file = match ScriptFile::new(script) {
        Ok(f) => f,
        Err(e) => return Some((false, String::new(), e)),
    };
    if is_root() {
        return sys::output("bash", &file.args()).map(|(ok, out, err)| (ok, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned()));
    }
    let argv: Vec<String> = std::iter::once("bash".to_string()).chain(file.args().into_iter().map(str::to_string)).collect();
    match crate::elevation::output(&argv) {
        Ok(r) => Some(r),
        Err(e) => Some((false, String::new(), e.to_string())),
    }
}

/// Single-quote a value for a POSIX shell.
#[cfg(unix)]
pub(crate) fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(all(test, unix))]
mod script_file_tests {
    /// A root script goes in a file only its account reads, bash runs it from
    /// there (never from its command line), and it is gone afterwards.
    #[test]
    fn a_root_script_is_a_private_file_removed_after() {
        use std::os::unix::fs::PermissionsExt as _;
        let f = super::ScriptFile::new("echo secret-$((1+1))").unwrap();
        let path = std::path::PathBuf::from(f.args()[0]);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let out = std::process::Command::new("bash").args(f.args()).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "secret-2");
        assert!(!f.args().iter().any(|a| a.contains("secret")));
        drop(f);
        assert!(!path.exists());
    }
}
