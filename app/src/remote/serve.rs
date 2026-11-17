// Copyright © 2016-2026 The SENTIENT Authors

//! `--job serve`: the agent's side of the one connection per server
//! (crate::remote::conn).
//!
//! The Platform Manager keeps one `ssh` running this for as long as it runs.
//! It reads requests from standard input and writes answers to standard
//! output, one JSON object per line each. Every request carries an id and
//! runs on its own thread, so independent requests overlap and answers come
//! back in any order. A request is what one ssh call did before: run a job
//! of this program (`--job …`, a process of its own: the job code is
//! unchanged), a shell command, or a PowerShell script, with optional
//! standard input; the answer is its exit code and output.
//!
//! The first line written is [`Hello`]: a Platform Manager that is not
//! sent it (an older agent answers `--job serve` with its usage text) uses
//! one ssh call per request, as before. Standard input closing (the
//! Platform Manager closed the connection, or quit) ends it.
//!
//! Lines that are not a request are ignored and never echoed: sudo, when it
//! did not need the password it was handed, leaves that line here.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The serve protocol; the Platform Manager uses an agent only when it
/// speaks the same one.
pub const SERVE: u32 = 1;

/// The first line the agent writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hello {
    pub spm_serve: u32,
    /// Running as root (Linux) or elevated (Windows): requests for the
    /// administrator are run as they are. A per-user agent (no sudo) is not.
    pub root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Work {
    /// `--job <args…>` of this program.
    Job(Vec<String>),
    /// `sh -c <text>` (Linux): as the signed-in account, or as root.
    Sh { cmd: String, root: bool },
    /// `powershell -EncodedCommand <text>` (Windows).
    Ps(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub id: u64,
    pub work: Work,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Response {
    pub id: u64,
    /// The exit code (None: killed by a signal, or it did not start).
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub out: String,
    #[serde(default)]
    pub err: String,
    /// Not run here: the caller makes its own ssh call for it instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// Run the serve loop on this process's standard input and output.
pub fn run() -> i32 {
    let me = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from(std::env::args().next().unwrap_or_default()));
    let host = Host { exe: me, root: is_root(), account: account() };
    let stdin = std::io::stdin();
    serve(stdin.lock(), std::io::stdout(), Arc::new(host));
    0
}

/// Where requests run.
pub struct Host {
    /// This program (its path from the start: a replaced file still runs).
    pub exe: std::path::PathBuf,
    pub root: bool,
    /// The signed-in account under sudo (name, home): shell commands for
    /// it run as it, the way its own ssh call ran them.
    pub account: Option<(String, String)>,
}

/// The loop itself: generic over the streams for the tests.
pub fn serve<R: BufRead, W: Write + Send + 'static>(input: R, output: W, host: Arc<Host>) {
    let out = Arc::new(Mutex::new(output));
    write_line(&out, &serde_json::to_string(&Hello { spm_serve: SERVE, root: host.root }).unwrap_or_default());
    let mut workers = Vec::new();
    for line in input.lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Request>(line.trim()) else { continue };
        let (out, host) = (out.clone(), host.clone());
        workers.push(std::thread::spawn(move || {
            let r = handle(&host, req);
            write_line(&out, &serde_json::to_string(&r).unwrap_or_default());
        }));
        workers.retain(|w| !w.is_finished());
    }
    // Answers still on their way go out before the end.
    for w in workers {
        let _ = w.join();
    }
}

fn write_line<W: Write>(out: &Mutex<W>, line: &str) {
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

fn handle(host: &Host, req: Request) -> Response {
    let refuse = |why: &str| Response { id: req.id, refused: Some(why.to_string()), ..Default::default() };
    let mut c = match &req.work {
        Work::Job(args) => {
            let mut c = Command::new(&host.exe);
            c.arg("--job").args(args);
            c
        }
        Work::Sh { cmd, root } => {
            if cfg!(windows) {
                return refuse("no shell on Windows");
            }
            if *root && !host.root {
                return refuse("not root");
            }
            match (&host.account, root) {
                (Some((user, home)), false) => {
                    let mut c = Command::new("runuser");
                    c.args(["-u", user, "--", "sh", "-c", cmd]).env("HOME", home).env("USER", user).env("LOGNAME", user).current_dir(home);
                    c
                }
                _ => {
                    let mut c = Command::new("sh");
                    c.args(["-c", cmd]);
                    c
                }
            }
        }
        Work::Ps(encoded) => {
            if !cfg!(windows) {
                return refuse("no PowerShell here");
            }
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded]);
            c
        }
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.stdin(if req.stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        // runuser missing (a minimal system): the caller's own ssh call does it.
        Err(_) if matches!(&req.work, Work::Sh { root: false, .. }) && host.account.is_some() => return refuse("no runuser"),
        Err(e) => return Response { id: req.id, code: None, err: format!("could not start: {e}"), ..Default::default() },
    };
    if let (Some(text), Some(mut s)) = (req.stdin, child.stdin.take()) {
        // Written from its own thread: a large input and a large output
        // would otherwise wait on each other.
        std::thread::spawn(move || {
            let _ = s.write_all(text.as_bytes());
        });
    }
    match child.wait_with_output() {
        Ok(o) => Response {
            id: req.id,
            code: o.status.code(),
            out: String::from_utf8_lossy(&o.stdout).to_string(),
            err: String::from_utf8_lossy(&o.stderr).to_string(),
            refused: None,
        },
        Err(e) => Response { id: req.id, code: None, err: e.to_string(), ..Default::default() },
    }
}

#[cfg(windows)]
fn is_root() -> bool {
    crate::elevate::is_admin()
}

#[cfg(not(windows))]
fn is_root() -> bool {
    // The uid without libc: /proc/self/status's "Uid:" line (real uid first).
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Uid:").and_then(|v| v.split_whitespace().next().map(|u| u == "0"))))
        .unwrap_or(false)
}

/// The account sudo was run by, and its home (from the user database).
fn account() -> Option<(String, String)> {
    if cfg!(windows) || !is_root() {
        return None;
    }
    let user = std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty() && u != "root")?;
    let out = Command::new("getent").args(["passwd", &user]).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).to_string();
    let home = line.trim().split(':').nth(5).filter(|h| !h.is_empty())?.to_string();
    Some((user, home))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_answers_round_trip_as_one_line_each() {
        let r = Request { id: 7, work: Work::Job(vec!["status-json".into()]), stdin: None };
        let line = serde_json::to_string(&r).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), r);
        let a = Response { id: 7, code: Some(0), out: "a\nb\n".into(), err: String::new(), refused: None };
        let line = serde_json::to_string(&a).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), a);
        let sh = serde_json::to_string(&Request { id: 1, work: Work::Sh { cmd: "true".into(), root: true }, stdin: Some("x".into()) }).unwrap();
        assert_eq!(sh, r#"{"id":1,"work":{"sh":{"cmd":"true","root":true}},"stdin":"x"}"#);
    }

    /// The loop over in-process pipes: hello first, junk (a password line)
    /// ignored and never echoed, one answer per request.
    #[test]
    fn hello_first_and_junk_ignored() {
        let input = "secret-password\n{\"id\":3,\"work\":{\"ps\":\"x\"}}\n";
        let buf = Shared::default();
        let host = Arc::new(Host { exe: "/nonexistent".into(), root: false, account: None });
        serve(std::io::Cursor::new(input.as_bytes().to_vec()), buf.clone(), host);
        let text = buf.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(serde_json::from_str::<Hello>(lines[0]).unwrap(), Hello { spm_serve: SERVE, root: false });
        assert_eq!(lines.len(), 2);
        assert!(!text.contains("secret-password"));
        let a: Response = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(a.id, 3);
        // PowerShell is refused off Windows; on Windows it ran (and failed on "x").
        if cfg!(windows) {
            assert!(a.refused.is_none());
        } else {
            assert!(a.refused.is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_requests_run_with_stdin_and_root_is_refused_when_not_root() {
        let input = "{\"id\":1,\"work\":{\"sh\":{\"cmd\":\"cat; echo oops >&2; exit 4\",\"root\":false}},\"stdin\":\"hi\"}\n{\"id\":2,\"work\":{\"sh\":{\"cmd\":\"true\",\"root\":true}}}\n";
        let buf = Shared::default();
        serve(std::io::Cursor::new(input.as_bytes().to_vec()), buf.clone(), Arc::new(Host { exe: "/bin/false".into(), root: false, account: None }));
        let answers: Vec<Response> = buf.text().lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect();
        let one = answers.iter().find(|a| a.id == 1).unwrap();
        assert_eq!((one.code, one.out.as_str(), one.err.trim()), (Some(4), "hi", "oops"));
        assert!(answers.iter().find(|a| a.id == 2).unwrap().refused.is_some());
    }

    /// A writer the test reads back.
    #[derive(Clone, Default)]
    pub(crate) struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Shared {
        pub(crate) fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
        }
    }
    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
