// Copyright © 2016-2026 The SENTIENT Authors

//! Following a log: its new lines handed on as they are written, not the
//! last few hundred read again every few seconds.
//!
//! On the computer the log is on (this one, or a server's agent with
//! `--job logs-follow`): a source is followed the cheapest way it can be.
//! - [`How::Cmd`]: the program's own follow (`docker logs --follow`,
//!   `journalctl --follow`): lines come the moment they are written.
//! - [`How::File`]: a log file watched for growth (a quick size check a few
//!   times a second, only the new bytes read; a rotated or truncated file
//!   is read again from its start).
//! - [`How::Reread`]: a source without either (the gateway's rotating files
//!   on Windows, Docker inside SENTIENT's WSL distro): its last lines read
//!   again here every two seconds and only the new ones handed on. Still read
//!   on the computer itself, so nothing crosses the network unless it is new.
//!
//! A server's lines reach the Platform Manager over its one connection
//! (crate::remote::serve, `stream`), the same SSH as everything else.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use crate::installation::{Engine, InstallationRecord};
use crate::logsource::Source;

/// How often a file is looked at for new bytes.
const FILE_EVERY: Duration = Duration::from_millis(300);
/// How often a source without a follow of its own is read again.
const REREAD_EVERY: Duration = Duration::from_secs(2);
/// How many last lines a re-read compares.
const REREAD_TAIL: u32 = 200;
/// How often a running follow looks whether it should stop.
const STOP_CHECK: Duration = Duration::from_millis(250);

/// How one source is followed.
#[derive(Debug, Clone, PartialEq)]
pub enum How {
    /// A program that prints new lines as they come (its standard output
    /// and standard error both).
    Cmd(String, Vec<String>),
    /// A file that grows; `dir_ext` set: the newest file with that ending
    /// in the file's folder (a log that starts a new file now and then).
    File { path: PathBuf, dir_ext: Option<String> },
    /// Read the last lines again, hand on the new ones.
    Reread,
}

fn docker_follow(container: &str) -> How {
    How::Cmd("docker".into(), ["logs", "--timestamps", "--follow", "--tail", "0", container].map(String::from).to_vec())
}

fn journal_follow(unit: &str, format: &str) -> How {
    How::Cmd("journalctl".into(), ["-u", unit, "--follow", "-n", "0", "--no-pager", "-o", format].map(String::from).to_vec())
}

/// The newest file in `dir` whose name ends with `ext`.
fn newest(dir: &Path, ext: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(ext))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// How `source` is followed here: the same places [`crate::logsource::read`]
/// reads, in the same order.
pub fn how(record: Option<&InstallationRecord>, source: Source) -> How {
    // Windows: Docker runs inside SENTIENT's WSL distro, not on Windows.
    let docker = |c: &str| if cfg!(windows) { How::Reread } else { docker_follow(c) };
    if source == Source::Gateway {
        return match (sentient_installer_core::gateway::log_dir(), sentient_installer_core::distro::gateway_docker::container(), gateway_unit()) {
            // Several rotating files, one per logger: read again.
            (Some(_), _, _) => How::Reread,
            (None, Some(c), _) => docker(&c),
            (None, None, Some(u)) => journal_follow(&u, "short-iso"),
            _ => How::Reread,
        };
    }
    let Some(record) = record else { return How::Reread };
    let names = record.stack.names();
    match (record.engine, source) {
        (Engine::Docker, Source::Sentient) => docker(&names.app),
        (Engine::Docker, Source::Database) => docker(&names.pg),
        (Engine::Docker, Source::ClickHouse) => docker(&names.ch),
        (_, Source::Gateway) => How::Reread,
        (Engine::Native, _) if cfg!(windows) => How::Reread,
        (Engine::Native, Source::Sentient) => {
            let p = Path::new("/var/log/sentient/sentient.log");
            if p.exists() {
                How::File { path: p.into(), dir_ext: None }
            } else {
                How::Reread
            }
        }
        (Engine::Native, Source::Database) => match newest(Path::new("/var/log/postgresql"), ".log") {
            Some(p) => How::File { path: p, dir_ext: Some(".log".into()) },
            None => How::Reread,
        },
        (Engine::Native, Source::ClickHouse) => {
            let p = Path::new("/var/log/clickhouse-server/clickhouse-server.log");
            if p.exists() {
                How::File { path: p.into(), dir_ext: None }
            } else {
                journal_follow("clickhouse-server", "short-iso")
            }
        }
    }
}

fn gateway_unit() -> Option<String> {
    #[cfg(not(windows))]
    {
        sentient_installer_core::distro::gateway_native::unit()
    }
    #[cfg(windows)]
    {
        None
    }
}

/// Follow `source` here: every new line goes to `emit` (false from it: stop)
/// until `going` says no. Err: it could not be followed, or its follow
/// ended by itself (the container went away, the journal stopped).
pub fn follow(record: Option<&InstallationRecord>, source: Source, emit: &mut dyn FnMut(&str) -> bool, going: &dyn Fn() -> bool) -> Result<(), String> {
    match how(record, source) {
        How::Cmd(prog, args) => follow_cmd(&prog, &args, emit, going),
        How::File { path, dir_ext } => follow_file(&path, dir_ext.as_deref(), emit, going),
        How::Reread => {
            let read = |n| crate::logsource::read(record, source, n, false);
            follow_reread(&read, emit, going, REREAD_EVERY)
        }
    }
}

/// A program's own follow, ended (killed) when `going` says no.
pub(crate) fn follow_cmd(prog: &str, args: &[String], emit: &mut dyn FnMut(&str) -> bool, going: &dyn Fn() -> bool) -> Result<(), String> {
    let mut c = sentient_installer_core::host::host_command(prog);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("Could not start {prog}: {e}"))?;
    let (tx, rx) = mpsc::channel::<String>();
    // Both streams: docker logs writes the container's standard error on its own.
    for r in [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        // Not joined: each ends when its pipe closes (the program ended).
        std::thread::spawn(move || {
            for line in BufReader::new(r).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let result = loop {
        if !going() {
            break Ok(());
        }
        match rx.recv_timeout(STOP_CHECK) {
            Ok(line) => {
                if !emit(&line) {
                    break Ok(());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Both streams ended: the program ended by itself.
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err(format!("{prog} stopped following the log")),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// A growing file: only its new bytes, as whole lines.
pub(crate) fn follow_file(path: &Path, dir_ext: Option<&str>, emit: &mut dyn FnMut(&str) -> bool, going: &dyn Fn() -> bool) -> Result<(), String> {
    let mut path = path.to_path_buf();
    let mut pos = std::fs::metadata(&path).map(|m| m.len()).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut partial = String::new();
    while going() {
        std::thread::sleep(FILE_EVERY);
        // A log that starts new files: the newest one from its start.
        if let Some(ext) = dir_ext {
            if let Some(n) = path.parent().and_then(|d| newest(d, ext)).filter(|n| *n != path) {
                path = n;
                pos = 0;
                partial.clear();
            }
        }
        let Ok(len) = std::fs::metadata(&path).map(|m| m.len()) else { continue };
        if len < pos {
            // Rotated or truncated: again from its start.
            pos = 0;
            partial.clear();
        }
        if len == pos {
            continue;
        }
        let Ok(mut f) = std::fs::File::open(&path) else { continue };
        if f.seek(SeekFrom::Start(pos)).is_err() {
            continue;
        }
        let mut buf = Vec::new();
        let Ok(n) = f.take(len - pos).read_to_end(&mut buf) else { continue };
        pos += n as u64;
        partial.push_str(&String::from_utf8_lossy(&buf));
        // Whole lines only; a line still being written waits for its end.
        while let Some(i) = partial.find('\n') {
            let line: String = partial.drain(..=i).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if !emit(line) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The lines of `now` that came after `before`: `before`'s last line found
/// in `now` (its last such place, checked with the lines before it), the
/// lines after it new. Nothing in common: all of `now` is new (more came
/// than a re-read holds).
pub(crate) fn new_lines<'a>(before: &[String], now: &'a [&'a str]) -> &'a [&'a str] {
    let Some(last) = before.last() else { return now };
    // The longest tail of `before` that `now` holds as a run ending at i.
    for i in (0..now.len()).rev() {
        if now[i] != last.as_str() {
            continue;
        }
        let k = (i + 1).min(before.len());
        if now[i + 1 - k..=i].iter().zip(&before[before.len() - k..]).all(|(a, b)| *a == b.as_str()) {
            return &now[i + 1..];
        }
    }
    now
}

/// Read the last lines again every `every`, hand on the new ones.
pub(crate) fn follow_reread(read: &dyn Fn(u32) -> String, emit: &mut dyn FnMut(&str) -> bool, going: &dyn Fn() -> bool, every: Duration) -> Result<(), String> {
    let keep = |t: &str| t.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect::<Vec<_>>();
    let mut before = keep(&read(REREAD_TAIL));
    let mut waited = Duration::ZERO;
    while going() {
        std::thread::sleep(STOP_CHECK);
        waited += STOP_CHECK;
        if waited < every {
            continue;
        }
        waited = Duration::ZERO;
        let text = read(REREAD_TAIL);
        let now: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        for l in new_lines(&before, &now) {
            if !emit(l) {
                return Ok(());
            }
        }
        before = keep(&text);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn only_the_lines_after_the_last_one_seen_are_new() {
        assert_eq!(new_lines(&s(&["a", "b", "c"]), &["b", "c", "d", "e"]), &["d", "e"]);
        // Nothing new.
        assert_eq!(new_lines(&s(&["a", "b"]), &["a", "b"]), &[] as &[&str]);
        // A repeated last line: matched with the lines before it.
        assert_eq!(new_lines(&s(&["x", "ok", "y", "ok"]), &["y", "ok", "z", "ok"]), &["z", "ok"]);
        assert_eq!(new_lines(&s(&["x", "ok", "y", "ok"]), &["ok", "y", "ok", "new"]), &["new"]);
        // More came than the re-read holds: all of it.
        assert_eq!(new_lines(&s(&["a", "b"]), &["p", "q"]), &["p", "q"]);
        // First read.
        assert_eq!(new_lines(&[], &["p"]), &["p"]);
    }

    #[test]
    fn a_growing_file_hands_on_its_new_whole_lines() {
        let dir = std::env::temp_dir().join(format!("spm-follow-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("x.log");
        std::fs::write(&path, "old line\n").unwrap();
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (g, st, p) = (got.clone(), stop.clone(), path.clone());
        let t = std::thread::spawn(move || follow_file(&p, None, &mut |l| { g.lock().unwrap().push(l.to_string()); true }, &|| !st.load(Ordering::SeqCst)));
        std::thread::sleep(Duration::from_millis(100));
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "one\ntw").unwrap();
        f.flush().unwrap();
        std::thread::sleep(Duration::from_millis(700));
        write!(f, "o\n").unwrap();
        f.flush().unwrap();
        std::thread::sleep(Duration::from_millis(700));
        // Truncated (rotated): from its start.
        std::fs::write(&path, "fresh\n").unwrap();
        std::thread::sleep(Duration::from_millis(700));
        stop.store(true, Ordering::SeqCst);
        t.join().unwrap().unwrap();
        assert_eq!(*got.lock().unwrap(), s(&["one", "two", "fresh"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reread_hands_on_only_new_lines() {
        let log = Arc::new(Mutex::new(String::from("a\nb\n")));
        let l2 = log.clone();
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (g, st) = (got.clone(), stop.clone());
        let t = std::thread::spawn(move || {
            follow_reread(&|_| l2.lock().unwrap().clone(), &mut |l| { g.lock().unwrap().push(l.to_string()); true }, &|| !st.load(Ordering::SeqCst), Duration::from_millis(250))
        });
        std::thread::sleep(Duration::from_millis(400));
        log.lock().unwrap().push_str("c\nd\n");
        std::thread::sleep(Duration::from_millis(800));
        stop.store(true, Ordering::SeqCst);
        t.join().unwrap().unwrap();
        assert_eq!(*got.lock().unwrap(), s(&["c", "d"]));
    }

    #[cfg(unix)]
    #[test]
    fn a_program_follow_is_ended_when_asked() {
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (g, st) = (got.clone(), stop.clone());
        let args = s(&["-c", "echo one; echo two >&2; exec sleep 30"]);
        let t = std::thread::spawn(move || follow_cmd("sh", &args, &mut |l| { g.lock().unwrap().push(l.to_string()); true }, &|| !st.load(Ordering::SeqCst)));
        std::thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::SeqCst);
        let t0 = std::time::Instant::now();
        t.join().unwrap().unwrap();
        assert!(t0.elapsed() < Duration::from_secs(5));
        let mut v = got.lock().unwrap().clone();
        v.sort();
        assert_eq!(v, s(&["one", "two"]));
        // One that ends by itself says so.
        let r = follow_cmd("sh", &s(&["-c", "echo x"]), &mut |_| true, &|| true);
        assert!(r.is_err());
    }
}
