// Copyright © 2016-2026 The SENTIENT Authors

//! One administrator elevation per job.
//!
//! A native Linux install or repair has a dozen root steps (packages, the
//! database cluster, units, ports, files). Each used to be its own pkexec,
//! so each asked for the password. Now the first root step of a job starts
//! ONE helper, `pkexec <this program> --root-session`, and every later root
//! step of this process is sent to it instead of to a new pkexec.
//!
//! The helper talks over its standard input and output only: pipes this
//! process created and alone holds, so nothing else on the computer can
//! reach it (there is no socket to connect to). It runs one command at a
//! time, streams its output back as JSON lines, and ends when this process
//! ends (its standard input closes), or after [`IDLE`] without work.
//!
//! Viewing never asks: a password prompt is only put up inside a user
//! action ([`user_action`], which every job holds). Outside one, a root
//! step uses the helper when one is alive and is otherwise refused with
//! [`ElevationError::NotNow`], which pages show as "not available".

use serde::{Deserialize, Serialize};

/// Why an administrator step did not run. Travels as its message (the
/// engines' errors are strings): [`ElevationError::from_message`] gives it
/// back, for a failure panel to offer "Ask again".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevationError {
    /// The password prompt was closed (pkexec 126/127, or its agent ended
    /// with 2 and no output).
    Cancelled,
    /// Nothing the user started asks for it (a page view, a poll).
    NotNow,
    /// There is no way to become administrator here.
    Unavailable(String),
    /// The helper failed or stopped.
    Failed(String),
}

impl ElevationError {
    pub const CANCELLED_MESSAGE: &'static str = "The password prompt was closed.";
    pub const NOT_NOW_MESSAGE: &'static str = "This needs the administrator password, which is asked only for an action you start.";

    /// The typed error back from a job's failure message.
    pub fn from_message(message: &str) -> Option<Self> {
        let m = message.trim();
        if m.starts_with(Self::CANCELLED_MESSAGE) {
            Some(Self::Cancelled)
        } else if m.starts_with(Self::NOT_NOW_MESSAGE) {
            Some(Self::NotNow)
        } else {
            None
        }
    }

    /// The message is a closed password prompt (the failure offers "Ask again").
    pub fn is_cancelled(message: &str) -> bool {
        Self::from_message(message) == Some(Self::Cancelled)
    }
}

impl std::fmt::Display for ElevationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str(Self::CANCELLED_MESSAGE),
            Self::NotNow => f.write_str(Self::NOT_NOW_MESSAGE),
            Self::Unavailable(m) | Self::Failed(m) => f.write_str(m),
        }
    }
}

// ---------------------------------------------------------------------------
// Who may put up the prompt
// ---------------------------------------------------------------------------

thread_local! {
    static ACTION: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
static PROCESS_ACTION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Held while a user action runs on this thread: root steps may ask for the
/// password. Jobs take it (app jobs.rs); page reads and polls do not.
#[must_use]
pub struct ActionScope(());

impl Drop for ActionScope {
    fn drop(&mut self) {
        ACTION.with(|a| a.set(a.get().saturating_sub(1)));
    }
}

pub fn user_action() -> ActionScope {
    ACTION.with(|a| a.set(a.get() + 1));
    ActionScope(())
}

/// The whole process is one user action (a command typed in a terminal).
pub fn allow_in_process() {
    PROCESS_ACTION.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn may_prompt() -> bool {
    PROCESS_ACTION.load(std::sync::atomic::Ordering::SeqCst) || ACTION.with(|a| a.get() > 0)
}

// ---------------------------------------------------------------------------
// The protocol
// ---------------------------------------------------------------------------

/// The argument that makes this program the root helper.
pub const SESSION_ARG: &str = "--root-session";

/// A helper with nothing to do for this long ends; the next root step asks again.
pub const IDLE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ToHelper {
    Run { id: u64, argv: Vec<String>, input: Option<String> },
    Cancel { id: u64 },
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FromHelper {
    Ready,
    Out { id: u64, line: String },
    Err { id: u64, line: String },
    Exit { id: u64, code: i32 },
}

/// Which stream a line of a root step came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

#[cfg(unix)]
pub use unix::*;

/// Windows: the whole program runs elevated (one UAC prompt when it starts,
/// crate app::elevate), so there is no helper.
#[cfg(not(unix))]
mod other {
    use super::*;
    pub fn serve_if_requested(_args: &[String]) -> Option<i32> {
        None
    }
    pub fn alive() -> bool {
        false
    }
    pub fn end() {}
    pub fn prompts() -> u32 {
        0
    }
    pub fn steps() -> u32 {
        0
    }
    pub fn set_helper(_argv: Vec<String>) {}
    pub fn run(
        _argv: &[String],
        _input: Option<&str>,
        _cancellable: bool,
        _on_line: &mut dyn FnMut(Stream, String),
        _on_prompt: &dyn Fn(),
    ) -> Result<i32, ElevationError> {
        Err(ElevationError::Unavailable("There is no administrator helper on this system.".into()))
    }
    pub fn output(_argv: &[String]) -> Result<(bool, String, String), ElevationError> {
        Err(ElevationError::Unavailable("There is no administrator helper on this system.".into()))
    }
}
#[cfg(not(unix))]
pub use other::*;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::TryLockError;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    // -----------------------------------------------------------------------
    // The helper (runs as root)
    // -----------------------------------------------------------------------

    /// Run as the helper when `args[1]` asks for it; returns the exit code.
    pub fn serve_if_requested(args: &[String]) -> Option<i32> {
        (args.get(1).map(String::as_str) == Some(SESSION_ARG)).then(serve)
    }

    enum Incoming {
        Msg(ToHelper),
        Eof,
    }

    type Out = Arc<Mutex<std::io::Stdout>>;

    fn send(out: &Out, frame: &FromHelper) {
        // The window may be gone: its pipe then refuses the write, and the
        // helper only finishes the step it is in.
        if let Ok(line) = serde_json::to_string(frame) {
            let mut o = out.lock().unwrap_or_else(|p| p.into_inner());
            let _ = writeln!(o, "{line}");
            let _ = o.flush();
        }
    }

    /// The helper: read commands from standard input, one at a time.
    pub fn serve() -> i32 {
        let out: Out = Arc::new(Mutex::new(std::io::stdout()));
        let (tx, rx) = mpsc::channel::<Incoming>();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if let Ok(msg) = serde_json::from_str::<ToHelper>(&line) {
                    if tx.send(Incoming::Msg(msg)).is_err() {
                        return;
                    }
                }
            }
            let _ = tx.send(Incoming::Eof);
        });
        send(&out, &FromHelper::Ready);
        loop {
            match rx.recv_timeout(IDLE) {
                Ok(Incoming::Msg(ToHelper::Run { id, argv, input })) => {
                    if run_one(id, &argv, input.as_deref(), &rx, &out) {
                        return 0;
                    }
                }
                Ok(Incoming::Msg(ToHelper::Cancel { .. })) => {}
                Ok(Incoming::Eof) | Err(_) => return 0,
            }
        }
    }

    fn signal_group(pgid: u32, sig: &str) {
        let _ = Command::new("kill").args([sig, "--", &format!("-{pgid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    /// Run one step. Cancel stops it (TERM to its process group, KILL ten
    /// seconds later). When the window goes away the step is let finish, as
    /// Setup's Cancel promises ("stops after the current task"), within ten
    /// minutes; true then: the helper ends.
    fn run_one(id: u64, argv: &[String], input: Option<&str>, rx: &Receiver<Incoming>, out: &Out) -> bool {
        use std::os::unix::process::CommandExt;
        let Some((prog, args)) = argv.split_first() else {
            send(out, &FromHelper::Exit { id, code: 127 });
            return false;
        };
        let spawned = Command::new(prog)
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                send(out, &FromHelper::Err { id, line: format!("could not run {prog}: {e}") });
                send(out, &FromHelper::Exit { id, code: 127 });
                return false;
            }
        };
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            let _ = writeln!(stdin, "{text}");
        }
        let forward = |r: Box<dyn std::io::Read + Send>, err: bool| {
            let out = out.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(r);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let line = String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']).to_string();
                            send(&out, &if err { FromHelper::Err { id, line } } else { FromHelper::Out { id, line } });
                        }
                    }
                }
            })
        };
        let t1 = forward(Box::new(child.stdout.take().expect("piped")), false);
        let t2 = forward(Box::new(child.stderr.take().expect("piped")), true);
        let pgid = child.id();
        let mut eof = false;
        let mut eof_at: Option<Instant> = None;
        let mut kill_at: Option<Instant> = None;
        let status = loop {
            if let Ok(Some(status)) = child.try_wait() {
                break status;
            }
            let incoming = if eof { None } else {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => Some(Incoming::Eof),
                }
            };
            if eof {
                std::thread::sleep(Duration::from_millis(100));
            }
            match incoming {
                Some(Incoming::Msg(ToHelper::Cancel { id: c })) if c == id && kill_at.is_none() => {
                    signal_group(pgid, "-TERM");
                    kill_at = Some(Instant::now() + Duration::from_secs(10));
                }
                Some(Incoming::Msg(ToHelper::Run { id: other, .. })) => {
                    // One at a time: the window waits for each step's exit.
                    send(out, &FromHelper::Err { id: other, line: "the administrator helper is busy".into() });
                    send(out, &FromHelper::Exit { id: other, code: 1 });
                }
                Some(Incoming::Eof) => {
                    eof = true;
                    eof_at = Some(Instant::now());
                }
                _ => {}
            }
            if eof_at.is_some_and(|t| t.elapsed() > Duration::from_secs(600)) && kill_at.is_none() {
                signal_group(pgid, "-TERM");
                kill_at = Some(Instant::now() + Duration::from_secs(10));
            }
            if kill_at.is_some_and(|t| Instant::now() >= t) {
                signal_group(pgid, "-KILL");
                kill_at = Some(Instant::now() + Duration::from_secs(3600));
            }
        };
        let _ = t1.join();
        let _ = t2.join();
        let code = status.code().unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            128 + status.signal().unwrap_or(0)
        });
        send(out, &FromHelper::Exit { id, code });
        eof
    }

    // -----------------------------------------------------------------------
    // The window's side
    // -----------------------------------------------------------------------

    enum Frame {
        Msg(FromHelper),
        Noise(String),
    }

    struct Session {
        child: Child,
        stdin: ChildStdin,
        frames: Receiver<Frame>,
        next: u64,
    }

    static SESSION: Mutex<Option<Session>> = Mutex::new(None);
    static HELPER: Mutex<Option<Vec<String>>> = Mutex::new(None);
    static PROMPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static STEPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// How many root steps this process has sent to a helper.
    pub fn steps() -> u32 {
        STEPS.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The helper to elevate (tests: another binary that serves the session).
    /// Default: this program with [`SESSION_ARG`].
    pub fn set_helper(argv: Vec<String>) {
        *HELPER.lock().unwrap_or_else(|p| p.into_inner()) = Some(argv);
    }

    /// How many helpers this process has started (password prompts).
    pub fn prompts() -> u32 {
        PROMPTS.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A helper is alive: root steps run without asking.
    pub fn alive() -> bool {
        match SESSION.try_lock() {
            Ok(mut g) => g.as_mut().is_some_and(Session::alive),
            // Busy with a step: alive.
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Poisoned(p)) => p.into_inner().as_mut().is_some_and(Session::alive),
        }
    }

    /// End the helper now (it also ends with this process).
    pub fn end() {
        let mut g = SESSION.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mut s) = g.take() {
            drop(s.stdin);
            let _ = s.child.wait();
        }
    }

    fn have(bin: &str) -> bool {
        crate::sys::host_command("sh").args(["-c", &format!("command -v {bin}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    impl Session {
        fn alive(&mut self) -> bool {
            matches!(self.child.try_wait(), Ok(None))
        }

        fn start(on_prompt: &dyn Fn()) -> Result<Session, ElevationError> {
            let helper = match HELPER.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                Some(h) => h,
                None => {
                    let exe = std::env::current_exe().map_err(|e| ElevationError::Failed(format!("find this program: {e}")))?;
                    vec![exe.display().to_string(), SESSION_ARG.to_string()]
                }
            };
            let pkexec = have("pkexec");
            let mut cmd = if pkexec {
                crate::sys::host_command("pkexec")
            } else if have("sudo") {
                let mut c = crate::sys::host_command("sudo");
                c.arg("-n");
                c
            } else {
                return Err(ElevationError::Unavailable("Neither pkexec nor sudo is available to become administrator.".into()));
            };
            if pkexec {
                on_prompt();
            }
            PROMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut child = cmd
                .args(&helper)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| ElevationError::Failed(format!("ask for the administrator password: {e}")))?;
            let stdin = child.stdin.take().expect("piped");
            let stdout = child.stdout.take().expect("piped");
            let stderr = child.stderr.take().expect("piped");
            let (tx, frames) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let f = serde_json::from_str::<FromHelper>(&line).map(Frame::Msg).unwrap_or(Frame::Noise(line));
                    if tx.send(f).is_err() {
                        return;
                    }
                }
            });
            let errors = Arc::new(Mutex::new(String::new()));
            let errs = errors.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut e = errs.lock().unwrap_or_else(|p| p.into_inner());
                    if e.len() < 8192 {
                        e.push_str(&line);
                        e.push('\n');
                    }
                }
            });
            let mut noise = String::new();
            loop {
                match frames.recv() {
                    Ok(Frame::Msg(FromHelper::Ready)) => return Ok(Session { child, stdin, frames, next: 1 }),
                    Ok(Frame::Msg(_)) => {}
                    Ok(Frame::Noise(l)) => {
                        noise.push_str(&l);
                        noise.push('\n');
                    }
                    Err(_) => {
                        let status = child.wait().map_err(|e| ElevationError::Failed(e.to_string()))?;
                        // Let the stderr reader catch up.
                        std::thread::sleep(Duration::from_millis(50));
                        let err = errors.lock().unwrap_or_else(|p| p.into_inner()).trim().to_string();
                        return Err(classify(pkexec, status.code(), &noise, &err));
                    }
                }
            }
        }

        fn exec(
            &mut self,
            argv: &[String],
            input: Option<&str>,
            cancellable: bool,
            on_line: &mut dyn FnMut(Stream, String),
        ) -> Result<i32, ElevationError> {
            let id = self.next;
            self.next += 1;
            STEPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let msg = serde_json::to_string(&ToHelper::Run { id, argv: argv.to_vec(), input: input.map(str::to_string) })
                .map_err(|e| ElevationError::Failed(e.to_string()))?;
            let stopped = || ElevationError::Failed("The administrator helper stopped.".into());
            writeln!(self.stdin, "{msg}").and_then(|_| self.stdin.flush()).map_err(|_| stopped())?;
            // A cancel asked before this step began is the caller's to report.
            let cancelled_before = crate::cancel::is_cancelled();
            let mut cancel_sent = false;
            loop {
                match self.frames.recv_timeout(Duration::from_millis(200)) {
                    Ok(Frame::Msg(FromHelper::Out { id: i, line })) if i == id => on_line(Stream::Out, line),
                    Ok(Frame::Msg(FromHelper::Err { id: i, line })) if i == id => on_line(Stream::Err, line),
                    Ok(Frame::Msg(FromHelper::Exit { id: i, code })) if i == id => return Ok(code),
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => {
                        if cancellable && !cancelled_before && !cancel_sent && crate::cancel::is_cancelled() {
                            cancel_sent = true;
                            let c = serde_json::to_string(&ToHelper::Cancel { id }).unwrap_or_default();
                            let _ = writeln!(self.stdin, "{c}").and_then(|_| self.stdin.flush());
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return Err(stopped()),
                }
            }
        }
    }

    /// What a helper that ended before it was ready means.
    pub(crate) fn classify(pkexec: bool, code: Option<i32>, out: &str, err: &str) -> ElevationError {
        match code {
            // pkexec: 126 the dialog was dismissed, 127 not authorised. An
            // agent that was closed can also end it with 2 and say nothing.
            Some(126) | Some(127) if pkexec => ElevationError::Cancelled,
            Some(2) if pkexec && out.trim().is_empty() && err.trim().is_empty() => ElevationError::Cancelled,
            _ if !pkexec && err.contains("password") => ElevationError::Unavailable(
                "sudo needs a password here and there is no password dialog (pkexec). Run this from a terminal with sudo.".into(),
            ),
            _ => {
                let e = err.trim();
                ElevationError::Failed(if e.is_empty() {
                    format!("The administrator helper ended with {}", code.map(|c| c.to_string()).unwrap_or_else(|| "a signal".into()))
                } else {
                    e.to_string()
                })
            }
        }
    }

    /// Run `argv` as root through the helper, starting it (one password
    /// prompt) when none is alive and this is a user action. Lines of
    /// output go to `on_line`; the answer is the command's exit code.
    /// `cancellable`: a Cancel (crate::cancel) during the step stops it.
    pub fn run(
        argv: &[String],
        input: Option<&str>,
        cancellable: bool,
        on_line: &mut dyn FnMut(Stream, String),
        on_prompt: &dyn Fn(),
    ) -> Result<i32, ElevationError> {
        let may = may_prompt();
        let mut guard = if may {
            SESSION.lock().unwrap_or_else(|p| p.into_inner())
        } else {
            // A page read never waits behind a long step.
            match SESSION.try_lock() {
                Ok(g) => g,
                Err(TryLockError::Poisoned(p)) => p.into_inner(),
                Err(TryLockError::WouldBlock) => return Err(ElevationError::NotNow),
            }
        };
        if guard.as_mut().is_some_and(|s| !s.alive()) {
            *guard = None;
        }
        if guard.is_none() {
            if !may {
                return Err(ElevationError::NotNow);
            }
            *guard = Some(Session::start(on_prompt)?);
        }
        let result = guard.as_mut().expect("started").exec(argv, input, cancellable, on_line);
        if result.is_err() {
            // The helper is gone or broken: the next step starts another.
            if let Some(mut s) = guard.take() {
                let _ = s.child.kill();
                let _ = s.child.wait();
            }
        }
        result
    }

    /// [`run`] capturing the output: (success, stdout, stderr).
    pub fn output(argv: &[String]) -> Result<(bool, String, String), ElevationError> {
        let (mut out, mut err) = (String::new(), String::new());
        let code = run(
            argv,
            None,
            false,
            &mut |s, line| {
                let buf = if s == Stream::Out { &mut out } else { &mut err };
                buf.push_str(&line);
                buf.push('\n');
            },
            &|| {},
        )?;
        Ok((code == 0, out, err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        assert_eq!(ElevationError::from_message(&ElevationError::Cancelled.to_string()), Some(ElevationError::Cancelled));
        assert_eq!(ElevationError::from_message(&ElevationError::NotNow.to_string()), Some(ElevationError::NotNow));
        assert!(ElevationError::is_cancelled("The password prompt was closed."));
        assert_eq!(ElevationError::from_message("bash exited with 1"), None);
    }

    #[test]
    fn protocol_frames() {
        let m = serde_json::to_string(&ToHelper::Run { id: 3, argv: vec!["bash".into(), "-c".into(), "echo 'a\nb'".into()], input: None }).unwrap();
        assert!(!m.contains('\n'), "one frame per line");
        assert_eq!(serde_json::from_str::<ToHelper>(&m).unwrap(), ToHelper::Run { id: 3, argv: vec!["bash".into(), "-c".into(), "echo 'a\nb'".into()], input: None });
        assert_eq!(serde_json::to_string(&FromHelper::Ready).unwrap(), "\"ready\"");
    }

    #[test]
    fn scope_nests() {
        assert!(!ACTION.with(|a| a.get() > 0));
        {
            let _a = user_action();
            let _b = user_action();
            assert!(ACTION.with(|a| a.get() == 2));
        }
        assert!(ACTION.with(|a| a.get() == 0));
    }

    #[cfg(unix)]
    #[test]
    fn closed_prompt_is_cancelled() {
        assert_eq!(classify(true, Some(126), "", ""), ElevationError::Cancelled);
        assert_eq!(classify(true, Some(127), "", "Error executing command as another user: Not authorized"), ElevationError::Cancelled);
        assert_eq!(classify(true, Some(2), "", ""), ElevationError::Cancelled);
        assert!(matches!(classify(true, Some(2), "", "usage"), ElevationError::Failed(_)));
        assert!(matches!(classify(false, Some(1), "", "sudo: a password is required"), ElevationError::Unavailable(_)));
    }
}
