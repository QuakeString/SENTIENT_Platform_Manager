// Copyright © 2016-2026 The SENTIENT Authors

//! One open connection per server.
//!
//! Every ssh call costs a handshake: several round trips, and sudo's on
//! top. So each added server gets one long-lived `ssh` running the agent's
//! serve mode (crate::remote::serve), started once (elevated the way a job
//! is: the sudo password, when one is kept, goes once on its standard
//! input, never on a command line), and every request is a line on it with
//! an id; independent requests run at once over it and are answered in any
//! order. The same on Linux and Windows (Windows' OpenSSH has no
//! ControlMaster).
//!
//! - A supervisor thread per server keeps it open: it reconnects at once
//!   when it drops, then with a growing pause (1 s up to 15 s) while the
//!   server does not answer; a request made while it is down starts the
//!   next attempt at once and waits for it, so a page says the connection
//!   is lost only when reconnecting failed, as an ssh call of its own did.
//! - An agent that does not speak serve mode (an older one answers with its
//!   usage text), none set up, sudo refusing, or `SPM_NO_SERVE` set: the
//!   callers make one ssh call per request, as before ([`Fail::PerCall`]).
//! - Closed when the server is removed or edited, when its sudo password
//!   changes, and after the agent was replaced (the next request starts the
//!   new one); quitting the Platform Manager closes standard input, which
//!   ends the agent there.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::serve::{Hello, Request, Response, Work, SERVE};
use super::Instance;

/// Why a request did not go over the connection.
#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    /// No connection for this server: make the ssh call of its own.
    PerCall,
    /// The server cannot be reached, or the connection dropped while the
    /// request was on it (it is not sent again: it may have run).
    Err(String),
}

/// One running agent at the other end of a pipe.
pub(crate) struct Pipe {
    pub input: Box<dyn Write + Send>,
    pub output: Box<dyn Read + Send>,
    /// Exit code, once it ended (waits).
    pub wait: Box<dyn FnMut() -> Option<i32> + Send>,
    pub kill: Box<dyn FnMut() + Send>,
    /// What it wrote to standard error so far (the last few KB).
    pub stderr: Arc<Mutex<String>>,
}

pub(crate) type Opener = Arc<dyn Fn() -> std::io::Result<Pipe> + Send + Sync>;

/// How long a request waits for a connection being made.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// How long the agent may take to say hello (ssh's own ConnectTimeout is 12 s; sudo after it).
const HELLO_WAIT: Duration = Duration::from_secs(25);
/// An agent without serve mode is asked again after this long.
const NO_SERVE_AGAIN: Duration = Duration::from_secs(600);

struct Link {
    hello: Hello,
    input: Mutex<Option<Box<dyn Write + Send>>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Response>>>,
    next: AtomicU64,
    dead: AtomicBool,
    stderr: Arc<Mutex<String>>,
}

enum State {
    Connecting,
    Up(Arc<Link>),
    NoServe(Instant),
    Down(String),
    Closed,
}

struct Slot {
    /// What the connection was made with (host, port, user, sign-in, …):
    /// another one closes it.
    key: String,
    opener: Opener,
    state: Mutex<State>,
    cv: Condvar,
    kick: AtomicBool,
    /// Connection attempts finished.
    attempts: AtomicU64,
    /// Last attempt's end.
    last: Mutex<Option<Instant>>,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

static SLOTS: Mutex<Option<HashMap<String, Arc<Slot>>>> = Mutex::new(None);

fn slots() -> MutexGuard<'static, Option<HashMap<String, Arc<Slot>>>> {
    SLOTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The one-call-per-request path only (timing, or a problem in this one).
pub fn disabled() -> bool {
    // Unit tests make their ssh calls one by one (their stand-ins), unless
    // one asks for the connection.
    if cfg!(test) && std::env::var_os("SPM_TEST_SERVE").is_none() {
        return true;
    }
    std::env::var_os("SPM_NO_SERVE").is_some()
}

fn key_of(inst: &Instance) -> String {
    format!("{}|{}|{}|{}|{:?}|{:?}|{:?}|{}", inst.id, inst.host, inst.port, inst.user, inst.auth, inst.key_file, super::is_windows(inst), inst.key_has_passphrase)
}

/// The server's connection: made and kept open from now on.
fn slot_for(inst: &Instance) -> Arc<Slot> {
    let key = key_of(inst);
    let mut g = slots();
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(s) = map.get(&inst.id) {
        if s.key == key {
            return s.clone();
        }
    }
    let i = inst.clone();
    let slot = new_slot(key, Arc::new(move || ssh_pipe(&i)));
    if let Some(old) = map.insert(inst.id.clone(), slot.clone()) {
        shut(&old);
    }
    slot
}

fn new_slot(key: String, opener: Opener) -> Arc<Slot> {
    let slot = Arc::new(Slot {
        key,
        opener,
        state: Mutex::new(State::Connecting),
        cv: Condvar::new(),
        kick: AtomicBool::new(false),
        attempts: AtomicU64::new(0),
        last: Mutex::new(None),
    });
    let s = slot.clone();
    std::thread::spawn(move || supervise(s));
    slot
}

/// Open the connection to each server now (the Platform Manager starts).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn open_all(list: &[Instance]) {
    if disabled() {
        return;
    }
    for inst in list {
        slot_for(inst);
    }
}

/// Close a server's connection (removed, edited, its agent replaced). A
/// request on it still gets its answer: the agent ends when it answered
/// what it has.
pub fn close(id: &str) {
    let old = slots().as_mut().and_then(|m| m.remove(id));
    if let Some(s) = old {
        shut(&s);
    }
}

fn shut(s: &Slot) {
    let prev = std::mem::replace(&mut *s.lock(), State::Closed);
    s.cv.notify_all();
    if let State::Up(link) = prev {
        // End of input: the agent finishes what it has, then ends.
        link.input.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Close every connection (the Platform Manager quits).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn close_all() {
    let all: Vec<Arc<Slot>> = slots().take().map(|m| m.into_values().collect()).unwrap_or_default();
    for s in all {
        shut(&s);
    }
}

/// Keep the connection open until it is closed.
fn supervise(slot: Arc<Slot>) {
    let mut pause = Duration::from_secs(1);
    loop {
        if matches!(*slot.lock(), State::Closed) {
            return;
        }
        {
            let mut st = slot.lock();
            if matches!(*st, State::Closed) {
                return;
            }
            *st = State::Connecting;
        }
        slot.cv.notify_all();
        let t0 = Instant::now();
        let r = connect(&slot);
        let who = slot.key.split('|').next().unwrap_or("");
        crate::timing::took(&format!("conn-{} id={who}", match &r { Opened::Up(_) => "up", Opened::NoServe => "noserve", Opened::Down(_) => "down" }), t0);
        *slot.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        slot.attempts.fetch_add(1, Ordering::SeqCst);
        match r {
            Opened::Up(link) => {
                pause = Duration::from_secs(1);
                let mut st = slot.lock();
                if matches!(*st, State::Closed) {
                    link.input.lock().unwrap_or_else(|e| e.into_inner()).take();
                    return;
                }
                *st = State::Up(link.clone());
                slot.cv.notify_all();
                // Until it drops (the reader notifies) or is closed.
                while !link.dead.load(Ordering::SeqCst) && !matches!(*st, State::Closed) {
                    st = slot.cv.wait(st).unwrap_or_else(|e| e.into_inner());
                }
                if matches!(*st, State::Closed) {
                    return;
                }
                // Dropped: again at once.
            }
            Opened::NoServe => {
                let mut st = slot.lock();
                if !matches!(*st, State::Closed) {
                    *st = State::NoServe(Instant::now());
                }
                slot.cv.notify_all();
                return;
            }
            Opened::Down(e) => {
                let mut st = slot.lock();
                if matches!(*st, State::Closed) {
                    return;
                }
                *st = State::Down(e);
                slot.cv.notify_all();
                let until = Instant::now() + pause;
                pause = (pause * 2).min(Duration::from_secs(15));
                while !slot.kick.swap(false, Ordering::SeqCst) && !matches!(*st, State::Closed) {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    st = slot.cv.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        }
    }
}

enum Opened {
    Up(Arc<Link>),
    /// The agent there does not serve (old, none, refused): per call.
    NoServe,
    /// ssh could not connect: why, in plain words.
    Down(String),
}

fn connect(slot: &Arc<Slot>) -> Opened {
    let pipe = match (slot.opener)() {
        Ok(p) => p,
        Err(e) => return Opened::Down(format!("start ssh: {e}")),
    };
    let Pipe { input, output, mut wait, mut kill, stderr } = pipe;
    let (hello_tx, hello_rx) = mpsc::channel::<Option<Hello>>();
    let link_slot: Arc<Mutex<Option<Arc<Link>>>> = Arc::new(Mutex::new(None));
    let (ls, sl) = (link_slot.clone(), slot.clone());
    // The reader: hello first, then the answers, each to its request.
    std::thread::spawn(move || {
        let mut lines = BufReader::new(output).lines();
        let hello = loop {
            match lines.next() {
                Some(Ok(l)) => {
                    if let Ok(h) = serde_json::from_str::<Hello>(l.trim()) {
                        break Some(h);
                    }
                    // Anything before hello (a usage text) is not it.
                    if l.contains("usage: --job") || l.contains("Unknown job") {
                        break None;
                    }
                }
                _ => break None,
            }
        };
        let ok = hello.as_ref().is_some_and(|h| h.spm_serve == SERVE);
        let _ = hello_tx.send(hello);
        if !ok {
            return;
        }
        // Wait for the link to be made from the hello.
        let link = loop {
            if let Some(l) = ls.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                break l;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        for line in lines {
            let Ok(line) = line else { break };
            if let Ok(r) = serde_json::from_str::<Response>(line.trim()) {
                // A streaming request's line: it stays pending until its last answer.
                let tx = {
                    let mut p = link.pending.lock().unwrap_or_else(|e| e.into_inner());
                    if r.line.is_some() { p.get(&r.id).cloned() } else { p.remove(&r.id) }
                };
                if let Some(tx) = tx {
                    let _ = tx.send(r);
                }
            }
        }
        {
            let mut p = link.pending.lock().unwrap_or_else(|e| e.into_inner());
            link.dead.store(true, Ordering::SeqCst);
            p.clear();
        }
        // Wake the supervisor (it waits on the slot's state lock).
        let _g = sl.lock();
        sl.cv.notify_all();
    });
    let hello = match hello_rx.recv_timeout(HELLO_WAIT) {
        Ok(h) => h,
        Err(_) => {
            kill();
            return Opened::Down(super::explain("connection timed out").unwrap_or_default());
        }
    };
    match hello {
        Some(h) if h.spm_serve == SERVE => {
            let link = Arc::new(Link { hello: h, input: Mutex::new(Some(input)), pending: Mutex::new(HashMap::new()), next: AtomicU64::new(1), dead: AtomicBool::new(false), stderr });
            *link_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.clone());
            // Reap it when it ends (its pipe closed).
            std::thread::spawn(move || {
                let _ = wait();
            });
            Opened::Up(link)
        }
        // Another serve protocol: per call.
        Some(_) => {
            drop(input);
            kill();
            let _ = wait();
            Opened::NoServe
        }
        None => {
            drop(input);
            // ssh's own failures exit 255; anything else is the agent's (or
            // the shell's) answer: no serve mode there.
            let code = wait_or_kill(&mut wait, &mut kill);
            if code == Some(255) {
                let err = super::clean_stderr(&stderr.lock().unwrap_or_else(|e| e.into_inner()));
                Opened::Down(super::explain(&err).unwrap_or(if err.is_empty() { "ssh failed".into() } else { err }))
            } else {
                Opened::NoServe
            }
        }
    }
}

fn wait_or_kill(wait: &mut Box<dyn FnMut() -> Option<i32> + Send>, kill: &mut Box<dyn FnMut() + Send>) -> Option<i32> {
    // The output already ended: the process ends with it (a usage text
    // printed, an exit); one that keeps going is ended.
    let (tx, rx) = mpsc::channel();
    let mut w = std::mem::replace(wait, Box::new(|| None));
    std::thread::spawn(move || {
        let _ = tx.send(w());
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(c) => c,
        Err(_) => {
            kill();
            rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
        }
    }
}

/// What the agent at the other end is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peer {
    pub root: bool,
}

/// Send one request over the server's connection and wait for its answer.
pub fn call(inst: &Instance, work: Work, stdin: Option<String>) -> Result<(Peer, Response), Fail> {
    if disabled() {
        return Err(Fail::PerCall);
    }
    call_on(&slot_for(inst), work, stdin)
}

fn call_on(slot: &Arc<Slot>, work: Work, stdin: Option<String>) -> Result<(Peer, Response), Fail> {
    let t0 = Instant::now();
    let what = match &work {
        Work::Job(a) => format!("job {}", a.first().map(String::as_str).unwrap_or("")),
        Work::Sh { .. } => "sh".to_string(),
        Work::Ps(_) => "ps".to_string(),
        Work::Cancel(_) => "cancel".to_string(),
    };
    let r = call_link(slot, work, stdin);
    crate::timing::took(&format!("conn-call {what} ok={}", r.is_ok()), t0);
    r
}

fn call_link(slot: &Arc<Slot>, work: Work, stdin: Option<String>) -> Result<(Peer, Response), Fail> {
    let link = wait_up(slot)?;
    let peer = Peer { root: link.hello.root };
    let lost = || {
        let err = super::clean_stderr(&link.stderr.lock().unwrap_or_else(|e| e.into_inner()));
        Fail::Err(super::explain(&err).unwrap_or_else(|| super::explain("connection reset").unwrap_or_default()))
    };
    let id = link.next.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    {
        let mut p = link.pending.lock().unwrap_or_else(|e| e.into_inner());
        if link.dead.load(Ordering::SeqCst) {
            return Err(lost());
        }
        p.insert(id, tx);
    }
    let line = serde_json::to_string(&Request { id, work, stdin, stream: false }).map_err(|e| Fail::Err(e.to_string()))?;
    let sent = {
        let mut i = link.input.lock().unwrap_or_else(|e| e.into_inner());
        match i.as_mut() {
            Some(w) => writeln!(w, "{line}").and_then(|_| w.flush()).is_ok(),
            None => false,
        }
    };
    if !sent {
        link.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        // Closed before it went: nothing ran, so it is made on its own.
        return Err(if link.dead.load(Ordering::SeqCst) { lost() } else { Fail::PerCall });
    }
    match rx.recv() {
        Ok(r) if r.refused.is_some() => Err(Fail::PerCall),
        Ok(r) => Ok((peer, r)),
        Err(_) => Err(lost()),
    }
}

/// A streaming request on a server's connection ([`stream`]): its lines
/// come on [`Stream::lines`]; dropped (or [`Stream::cancel`]), the agent is
/// told to end it.
pub struct Stream {
    link: Arc<Link>,
    id: u64,
    pub lines: mpsc::Receiver<Response>,
    done: bool,
}

impl Stream {
    /// End it there (the job's input closed); its last answer still comes.
    pub fn cancel(&mut self) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        let id = self.link.next.fetch_add(1, Ordering::SeqCst);
        if let Ok(line) = serde_json::to_string(&Request { id, work: Work::Cancel(self.id), stdin: None, stream: false }) {
            let mut i = self.link.input.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(w) = i.as_mut() {
                let _ = writeln!(w, "{line}").and_then(|_| w.flush());
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.cancel();
        self.link.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// Run `--job <args>` on the server with its output answered line by line
/// as it comes. [`Fail::PerCall`]: no connection, or its agent does not
/// stream (an older one): the caller reads the way it did before.
pub fn stream(inst: &Instance, args: Vec<String>) -> Result<Stream, Fail> {
    if disabled() {
        return Err(Fail::PerCall);
    }
    stream_on(&slot_for(inst), args)
}

fn stream_on(slot: &Arc<Slot>, args: Vec<String>) -> Result<Stream, Fail> {
    let link = wait_up(slot)?;
    if !link.hello.stream {
        return Err(Fail::PerCall);
    }
    let id = link.next.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    {
        let mut p = link.pending.lock().unwrap_or_else(|e| e.into_inner());
        if link.dead.load(Ordering::SeqCst) {
            return Err(Fail::Err(super::explain("connection reset").unwrap_or_default()));
        }
        p.insert(id, tx);
    }
    let line = serde_json::to_string(&Request { id, work: Work::Job(args), stdin: None, stream: true }).map_err(|e| Fail::Err(e.to_string()))?;
    let sent = {
        let mut i = link.input.lock().unwrap_or_else(|e| e.into_inner());
        match i.as_mut() {
            Some(w) => writeln!(w, "{line}").and_then(|_| w.flush()).is_ok(),
            None => false,
        }
    };
    if !sent {
        link.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        return Err(Fail::PerCall);
    }
    Ok(Stream { link, id, lines: rx, done: false })
}

/// The connection, once it is up (waiting while it is being made).
fn wait_up(slot: &Arc<Slot>) -> Result<Arc<Link>, Fail> {
    let deadline = Instant::now() + CONNECT_WAIT;
    let mut kicked_at: Option<u64> = None;
    let mut st = slot.lock();
    loop {
        match &*st {
            State::Up(l) if !l.dead.load(Ordering::SeqCst) => return Ok(l.clone()),
            State::Closed => return Err(Fail::PerCall),
            State::NoServe(at) if at.elapsed() < NO_SERVE_AGAIN => return Err(Fail::PerCall),
            State::NoServe(_) => {
                // Asked again after a while (the agent may be updated since).
                *st = State::Connecting;
                let s = slot.clone();
                std::thread::spawn(move || supervise(s));
            }
            State::Down(e) => match kicked_at {
                Some(n) if slot.attempts.load(Ordering::SeqCst) > n => return Err(Fail::Err(e.clone())),
                Some(_) => {}
                None => {
                    // The supervisor waits out its pause: try again now,
                    // unless an attempt just ended.
                    let recent = slot.last.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                    if recent {
                        return Err(Fail::Err(e.clone()));
                    }
                    kicked_at = Some(slot.attempts.load(Ordering::SeqCst));
                    slot.kick.store(true, Ordering::SeqCst);
                    slot.cv.notify_all();
                }
            },
            _ => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Fail::Err(super::explain("connection timed out").unwrap_or_default()));
        }
        st = slot.cv.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// The real thing: `ssh` running `--job serve` on the server, started the
/// way a job is (crate::remote::agent_cmd; Windows: this Platform Manager's
/// own agent there, a console program).
fn ssh_pipe(inst: &Instance) -> std::io::Result<Pipe> {
    let (remote, pw) = if super::is_windows(inst) {
        // Written for the server's default shell (cmd or PowerShell): the
        // agent's input and output must be its own, so not wrapped in
        // another PowerShell (crate::remote::winshell).
        // VERIFY: under a PowerShell DefaultShell (powershell -c "& 'exe' …")
        // the agent's line-based stdin/stdout reach it promptly.
        (super::winshell::program(super::winshell::of(inst), super::win::EXE, "--job serve"), None)
    } else {
        let pw = super::sudo_password(inst);
        (super::agent_cmd(inst, "--job serve", pw.is_some()), pw)
    };
    // Keep-alives often: a dead link is found within 15 s.
    let mut c = super::ssh_with(inst, &["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=3"], &remote, None);
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = c.spawn()?;
    let mut input = child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
    if let Some(pw) = pw {
        // sudo's password, first on standard input (never on a command
        // line). Three times: a wrong one is asked for again (sudo's default
        // three tries) and the connection's input stays open, so sudo would
        // wait for the next try instead of refusing at once. The agent
        // ignores the lines sudo did not read, and never echoes them.
        for _ in 0..3 {
            writeln!(input, "{pw}")?;
        }
        input.flush()?;
    }
    let output = child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
    let err = child.stderr.take().ok_or_else(|| std::io::Error::other("no stderr"))?;
    let stderr = Arc::new(Mutex::new(String::new()));
    let s = stderr.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            let mut t = s.lock().unwrap_or_else(|e| e.into_inner());
            t.push_str(&line);
            t.push('\n');
            if t.len() > 4096 {
                let cut = t.len() - 4096;
                let cut = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(t.len());
                t.drain(..cut);
            }
        }
    });
    let child = Arc::new(Mutex::new(child));
    let (c1, c2) = (child.clone(), child);
    Ok(Pipe {
        input: Box::new(input),
        output: Box::new(output),
        wait: Box::new(move || {
            // Polled, so a kill from another thread gets the lock.
            loop {
                match c1.lock().unwrap_or_else(|e| e.into_inner()).try_wait() {
                    Ok(Some(s)) => return s.code(),
                    Ok(None) => {}
                    Err(_) => return None,
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }),
        kill: Box::new(move || {
            let _ = c2.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }),
        stderr,
    })
}

/// Split a job's argument text the way the server's shell would, for the
/// simple text the jobs are given: words, '…' and "…" quoting. None when
/// it holds anything a shell would treat specially (that one goes on its
/// own ssh call, through the shell, as before).
pub fn split_args(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if word {
                    out.push(std::mem::take(&mut cur));
                    word = false;
                }
            }
            '\'' | '"' => {
                word = true;
                loop {
                    match chars.next() {
                        Some(q) if q == c => break,
                        Some(x) if c == '"' && matches!(x, '$' | '`' | '\\') => return None,
                        Some(x) => cur.push(x),
                        None => return None,
                    }
                }
            }
            '$' | '`' | '\\' | ';' | '|' | '&' | '<' | '>' | '*' | '?' | '(' | ')' | '{' | '}' | '~' | '#' | '[' | ']' | '\n' | '!' => return None,
            c => {
                word = true;
                cur.push(c);
            }
        }
    }
    if word {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::remote::serve::{self, Host};

    #[test]
    fn job_text_splits_like_the_shell_or_not_at_all() {
        assert_eq!(split_args("--job status-json").unwrap(), vec!["--job", "status-json"]);
        assert_eq!(split_args("--job dir-list '/srv/my backups'  x").unwrap(), vec!["--job", "dir-list", "/srv/my backups", "x"]);
        assert_eq!(split_args("--job takeover-plan --compose \"/a b/c.yml\"").unwrap(), vec!["--job", "takeover-plan", "--compose", "/a b/c.yml"]);
        assert_eq!(split_args("--job licence-api --request-b64 eyJh+/=").unwrap(), vec!["--job", "licence-api", "--request-b64", "eyJh+/="]);
        assert_eq!(split_args("''").unwrap(), vec![""]);
        for special in ["a $HOME", "a; rm", "a | b", "'open", "a*", "\"$x\"", "a\\b"] {
            assert!(split_args(special).is_none(), "{special}");
        }
    }

    /// An in-process agent: the serve loop over pipes, its jobs run by `exe`.
    #[cfg(unix)]
    fn in_process(exe: &str) -> Opener {
        in_process_cut(exe, Arc::new(Mutex::new(Vec::new())))
    }

    /// The same, and `cut` severs every connection it made (the answers
    /// stop: as if ssh died).
    #[cfg(unix)]
    fn in_process_cut(exe: &str, cut: Arc<Mutex<Vec<Arc<Mutex<Option<std::io::PipeWriter>>>>>>) -> Opener {
        let exe = exe.to_string();
        Arc::new(move || {
            let (req_r, req_w) = std::io::pipe()?;
            let (ans_r, ans_w) = std::io::pipe()?;
            // A relay between the agent's answers and the reader, cut by dropping its writer.
            let (relay_r, relay_w) = std::io::pipe()?;
            let relay_w = Arc::new(Mutex::new(Some(relay_w)));
            cut.lock().unwrap().push(relay_w.clone());
            std::thread::spawn(move || {
                let mut ans_r = ans_r;
                let mut buf = [0u8; 4096];
                while let Ok(n) = ans_r.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    match relay_w.lock().unwrap().as_mut() {
                        Some(w) => {
                            let _ = w.write_all(&buf[..n]);
                        }
                        None => break,
                    }
                }
                relay_w.lock().unwrap().take();
            });
            let ans_r = relay_r;
            let host = Arc::new(Host { exe: exe.clone().into(), root: true, account: None });
            let h = std::thread::spawn(move || serve::serve(BufReader::new(req_r), ans_w, host));
            let h = Arc::new(Mutex::new(Some(h)));
            let h2 = h.clone();
            Ok(Pipe {
                input: Box::new(req_w),
                output: Box::new(ans_r),
                wait: Box::new(move || {
                    if let Some(h) = h2.lock().unwrap().take() {
                        let _ = h.join();
                    }
                    Some(0)
                }),
                kill: Box::new(|| {}),
                stderr: Arc::new(Mutex::new(String::new())),
            })
        })
    }

    #[cfg(unix)]
    fn sh(cmd: &str) -> Work {
        Work::Sh { cmd: cmd.into(), root: false }
    }

    #[cfg(unix)]
    #[test]
    fn parallel_requests_are_answered_out_of_order() {
        let slot = new_slot("t".into(), in_process("/bin/sh"));
        let s1 = slot.clone();
        let slow = std::thread::spawn(move || call_on(&s1, sh("sleep 0.6; echo slow"), None));
        std::thread::sleep(Duration::from_millis(150));
        let t = Instant::now();
        let (peer, fast) = call_on(&slot, sh("echo fast"), None).unwrap();
        assert!(peer.root);
        assert_eq!(fast.out, "fast\n");
        // The fast one did not wait for the slow one before it.
        assert!(t.elapsed() < Duration::from_millis(400), "{:?}", t.elapsed());
        let (_, slow) = slow.join().unwrap().unwrap();
        assert_eq!(slow.out, "slow\n");
        // Stdin reaches the request; ids are not reused.
        let (_, cat) = call_on(&slot, sh("cat"), Some("text in".into())).unwrap();
        assert_eq!(cat.out, "text in");
        let State::Up(link) = &*slot.lock() else { panic!("not up") };
        assert_eq!(link.next.load(Ordering::SeqCst), 4);
        drop(link);
    }

    #[cfg(unix)]
    #[test]
    fn jobs_run_the_agent_program() {
        let dir = std::env::temp_dir().join(format!("spm-conn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("agent");
        std::fs::write(&exe, "#!/bin/sh\necho \"$1|$2|$3\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let slot = new_slot("t".into(), in_process(exe.to_str().unwrap()));
        let (_, r) = call_on(&slot, Work::Job(split_args("dir-list '/a b'").unwrap()), None).unwrap();
        assert_eq!((r.code, r.out.as_str()), (Some(0), "--job|dir-list|/a b\n"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A streaming job over the connection: its lines as they come, and
    /// cancelled, it ends (its input closed) and says so with its last answer.
    #[cfg(unix)]
    #[test]
    fn a_stream_brings_lines_as_they_come_until_cancelled() {
        let dir = std::env::temp_dir().join(format!("spm-conn-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("agent");
        std::fs::write(&exe, "#!/bin/sh\necho first\nsleep 0.3\necho second\ncat >/dev/null\necho ended\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let slot = new_slot("t".into(), in_process(exe.to_str().unwrap()));
        let mut st = stream_on(&slot, vec!["logs-follow".into()]).unwrap();
        let next = |st: &Stream| st.lines.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next(&st).line.as_deref(), Some("first"));
        let t = Instant::now();
        assert_eq!(next(&st).line.as_deref(), Some("second"));
        assert!(t.elapsed() >= Duration::from_millis(150), "came as printed, not at the end");
        // Other requests still go while it runs.
        assert_eq!(call_on(&slot, sh("echo beside"), None).unwrap().1.out, "beside\n");
        st.cancel();
        assert_eq!(next(&st).line.as_deref(), Some("ended"));
        let last = next(&st);
        assert_eq!((last.line, last.code), (None, Some(0)));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An agent that does not stream (its hello says nothing): per call.
    #[cfg(unix)]
    #[test]
    fn an_agent_without_streaming_is_asked_per_call() {
        let slot = new_slot("t".into(), Arc::new(|| stand_in("echo '{\"spm_serve\":1,\"root\":false}'; cat >/dev/null")));
        assert_eq!(stream_on(&slot, vec!["logs-follow".into()]).err(), Some(Fail::PerCall));
    }

    /// A dropped connection: the request on it fails (it is not sent
    /// again), and the next request finds it made again.
    #[cfg(unix)]
    #[test]
    fn a_dropped_connection_is_made_again() {
        let opens = Arc::new(AtomicU64::new(0));
        let o = opens.clone();
        let cut = Arc::new(Mutex::new(Vec::new()));
        let inner = in_process_cut("/bin/sh", cut.clone());
        let opener: Opener = Arc::new(move || {
            o.fetch_add(1, Ordering::SeqCst);
            inner()
        });
        let slot = new_slot("t".into(), opener);
        assert_eq!(call_on(&slot, sh("echo one"), None).unwrap().1.out, "one\n");
        // Cut it while a request is on it.
        let s1 = slot.clone();
        let on_it = std::thread::spawn(move || call_on(&s1, sh("sleep 0.3; echo late"), None));
        std::thread::sleep(Duration::from_millis(100));
        for w in cut.lock().unwrap().iter() {
            w.lock().unwrap().take();
        }
        match on_it.join().unwrap() {
            Err(Fail::Err(e)) => assert!(e.contains("dropping"), "{e}"),
            other => panic!("{other:?}"),
        }
        let (_, r) = call_on(&slot, sh("echo two"), None).unwrap();
        assert_eq!(r.out, "two\n");
        assert_eq!(opens.load(Ordering::SeqCst), 2);
    }

    /// ssh failing (exit 255) is the server being down: the request says
    /// so; once the server answers again, the next request goes through.
    #[cfg(unix)]
    #[test]
    fn a_server_down_then_back() {
        let up = Arc::new(AtomicBool::new(false));
        let u = up.clone();
        let inner = in_process("/bin/sh");
        let opener: Opener = Arc::new(move || if u.load(Ordering::SeqCst) { inner() } else { stand_in("echo 'ssh: connect to host x port 22: Connection refused' >&2; exit 255") });
        let slot = new_slot("t".into(), opener);
        match call_on(&slot, sh("true"), None) {
            Err(Fail::Err(e)) => assert!(e.contains("refused the connection"), "{e}"),
            other => panic!("{other:?}"),
        }
        up.store(true, Ordering::SeqCst);
        // An attempt just ended: within 2 s the same answer, then a new try.
        std::thread::sleep(Duration::from_millis(2100));
        assert_eq!(call_on(&slot, sh("echo back"), None).unwrap().1.out, "back\n");
    }

    /// An agent without serve mode answers with its usage text: requests
    /// go on calls of their own.
    #[cfg(unix)]
    #[test]
    fn an_old_agent_means_one_call_per_request() {
        let slot = new_slot("t".into(), Arc::new(|| stand_in("echo 'usage: --job install <plan.json> | …' >&2; echo 'usage: --job install'; exit 2")));
        assert_eq!(call_on(&slot, sh("true"), None).unwrap_err(), Fail::PerCall);
        // Not set up there (the shell's exit 3) is the same.
        let slot = new_slot("t".into(), Arc::new(|| stand_in("echo 'The server agent is not set up on this server.' >&2; exit 3")));
        assert_eq!(call_on(&slot, sh("true"), None).unwrap_err(), Fail::PerCall);
    }

    #[test]
    fn closed_means_per_call() {
        let slot = new_slot("t".into(), Arc::new(|| Err(std::io::Error::other("no"))));
        shut(&slot);
        assert_eq!(call_on(&slot, Work::Job(vec![]), None).unwrap_err(), Fail::PerCall);
    }

    /// Timing on a real server (not run by default): the Status page's
    /// reads at once (ready, status, services, database) and a live
    /// reading, opened and opened again, one ssh call per request
    /// (SPM_NO_SERVE) and over the one connection. SPM_TIMING_IDS names the
    /// instances (XDG_CONFIG_HOME's profile); SPM_TIMING_SUDO a sudo password
    /// kept for this run only; SPM_TIMING_DEPLOY sets the agent up first.
    #[test]
    #[ignore]
    fn timing_live() {
        let ids = std::env::var("SPM_TIMING_IDS").expect("SPM_TIMING_IDS");
        std::env::set_var("SPM_TEST_SERVE", "1");
        let list = crate::remote::load();
        for id in ids.split(',') {
            let inst = list.iter().find(|i| i.id == id).expect("instance").clone();
            if let Ok(pw) = std::env::var("SPM_TIMING_SUDO") {
                crate::remote::SESSION.lock().unwrap().get_or_insert_with(HashMap::new).insert(crate::remote::sudo_key(id), pw);
            }
            if std::env::var_os("SPM_TIMING_DEPLOY").is_some() {
                let sink: crate::remote::ProgressFn = Arc::new(|p| eprintln!("  deploy: {p:?}"));
                crate::remote::deploy(&inst, &sink).expect("deploy");
            }
            for mode in ["per call", "one connection"] {
                if mode == "per call" {
                    std::env::set_var("SPM_NO_SERVE", "1");
                } else {
                    std::env::remove_var("SPM_NO_SERVE");
                }
                close(&inst.id);
                for open in ["first open", "reopen"] {
                    let t = Instant::now();
                    let page: Vec<_> = (0..4)
                        .map(|k| {
                            let i = inst.clone();
                            std::thread::spawn(move || {
                                let t = Instant::now();
                                let ok = match k {
                                    0 => crate::remote::ready(&i).map(|_| ()),
                                    1 => crate::remote::status(&i).map(|_| ()),
                                    2 => crate::remote::services(&i).map(|_| ()),
                                    _ => crate::remote::dbstatus(&i).map(|_| ()),
                                };
                                (t.elapsed(), ok)
                            })
                        })
                        .collect::<Vec<_>>()
                        .into_iter()
                        .map(|h| h.join().unwrap())
                        .collect();
                    let whole = t.elapsed();
                    let t = Instant::now();
                    let st = crate::remote::status(&inst).map(|_| ());
                    let status = t.elapsed();
                    let t = Instant::now();
                    let live = crate::remote::stats_live_part(&inst, crate::stats::Part::System).map(|_| ());
                    let stats = t.elapsed();
                    if std::env::var_os("SPM_TIMING_LICENCE").is_some() {
                        // The Licence page's opening, request for request: status, a token
                        // renewed, the three reads at once. No sign-in is used: SENTIENT
                        // answers the renew and the reads with 401, the same round trips.
                        let target = crate::licence::Target::Remote(inst.clone());
                        let t = Instant::now();
                        let serving = target.serving();
                        let renew = target.renew("not-a-refresh-token").err();
                        let read = target.read("not-a-token").err();
                        eprintln!("TIMING {id} {mode:15} {open:10} Licence page {:>5} ms (serving {:?}; renew {:?}; read {:?})", t.elapsed().as_millis(), serving, renew, read);
                    }
                    eprintln!(
                        "TIMING {id} {mode:15} {open:10} Status page {:>5} ms (ready {} / status {} / services {} / db {} ms; {:?})  status alone {:>5} ms {:?}  stats-live {:>5} ms {:?}",
                        whole.as_millis(),
                        page[0].0.as_millis(),
                        page[1].0.as_millis(),
                        page[2].0.as_millis(),
                        page[3].0.as_millis(),
                        page.iter().map(|p| p.1.as_ref().err().map(|e| e.chars().take(48).collect::<String>())).collect::<Vec<_>>(),
                        status.as_millis(),
                        st.is_ok(),
                        stats.as_millis(),
                        live.err()
                    );
                }
            }
        }
    }

    /// Live (not run by default): replacing the agent (Set up / Update
    /// agent) restarts the connection on the new one. SPM_TIMING_IDS names
    /// one instance; SPM_AGENT_DIR holds an agent that differs from the one there.
    #[test]
    #[ignore]
    fn deploy_restarts_the_connection_live() {
        std::env::set_var("SPM_TEST_SERVE", "1");
        let id = std::env::var("SPM_TIMING_IDS").expect("SPM_TIMING_IDS");
        let inst = crate::remote::load().into_iter().find(|i| i.id == id).expect("instance");
        if let Ok(pw) = std::env::var("SPM_TIMING_SUDO") {
            crate::remote::SESSION.lock().unwrap().get_or_insert_with(HashMap::new).insert(crate::remote::sudo_key(&id), pw);
        }
        // The serve agent's process: the parent of a root shell request.
        let agent_pid = || crate::remote::sudo_run(&inst, "ps -o ppid= -p $$; readlink /proc/$PPID/exe", None).expect("ask");
        let before = agent_pid();
        assert_eq!(before, agent_pid(), "one connection");
        let sink: crate::remote::ProgressFn = Arc::new(|p| eprintln!("  deploy: {p:?}"));
        crate::remote::deploy(&inst, &sink).expect("deploy");
        let after = agent_pid();
        eprintln!("before: {before:?} after: {after:?}");
        assert_ne!(before.lines().next(), after.lines().next(), "a new agent process");
    }

    /// `sh -c` standing in for ssh and the agent behind it.
    #[cfg(unix)]
    pub(crate) fn stand_in(script: &str) -> std::io::Result<Pipe> {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", script]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = c.spawn()?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let mut err = child.stderr.take().unwrap();
        let stderr = Arc::new(Mutex::new(String::new()));
        let s = stderr.clone();
        let t = std::thread::spawn(move || {
            let mut b = String::new();
            let _ = err.read_to_string(&mut b);
            s.lock().unwrap().push_str(&b);
        });
        let t = Arc::new(Mutex::new(Some(t)));
        let child = Arc::new(Mutex::new(child));
        let (c1, c2) = (child.clone(), child);
        Ok(Pipe {
            input: Box::new(input),
            output: Box::new(output),
            wait: Box::new(move || {
                let code = loop {
                    match c1.lock().unwrap().try_wait() {
                        Ok(Some(s)) => break s.code(),
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                        Err(_) => break None,
                    }
                };
                if let Some(t) = t.lock().unwrap().take() {
                    let _ = t.join();
                }
                code
            }),
            kill: Box::new(move || {
                let _ = c2.lock().unwrap().kill();
            }),
            stderr,
        })
    }
}
