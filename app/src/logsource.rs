// Copyright © 2016-2026 The SENTIENT Authors

//! Where the Logs screen reads from: SENTIENT, its database, ClickHouse and
//! the gateway, each from the place that install keeps it (a Docker
//! container's log with Docker's timestamps, a systemd journal, a log file),
//! and how one line reads as columns: local time, level, message.


use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::installation::{Engine, InstallationRecord};
use crate::log_level::Level;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    Sentient,
    Database,
    ClickHouse,
    Gateway,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::Sentient, Source::Database, Source::ClickHouse, Source::Gateway];

    pub fn label(self) -> &'static str {
        match self {
            Source::Sentient => "SENTIENT",
            Source::Database => "Database",
            Source::ClickHouse => "ClickHouse",
            Source::Gateway => "Gateway",
        }
    }

    /// The short tag in the "All" view.
    pub fn tag(self) -> &'static str {
        match self {
            Source::Sentient => "app",
            Source::Database => "pg",
            Source::ClickHouse => "ch",
            Source::Gateway => "gw",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Source::Sentient => "sentient",
            Source::Database => "database",
            Source::ClickHouse => "clickhouse",
            Source::Gateway => "gateway",
        }
    }

    pub fn from_key(k: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.key() == k)
    }
}

/// The gateway's container on this computer, when it runs in Docker
/// (only a container of the gateway's image or program).
fn gateway_container() -> Option<String> {
    sentient_installer_core::distro::gateway_docker::container()
}

/// A gateway that runs as a systemd service (no Docker).
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

/// The Windows gateway service's log folder (its rotating files, one per
/// logger: service.log, connector.log, …), when the service is here.
fn gateway_files() -> Option<std::path::PathBuf> {
    sentient_installer_core::gateway::log_dir()
}

/// A gateway runs on this computer (Windows service, container or systemd
/// service), with or without SENTIENT beside it.
pub fn gateway_here() -> bool {
    gateway_files().is_some() || gateway_container().is_some() || gateway_unit().is_some()
}

/// The sources this computer has: SENTIENT's (when it is installed here)
/// and the gateway's (when one runs here). A gateway-only server has only
/// the gateway's log.
pub fn available(record: Option<&InstallationRecord>) -> Vec<Source> {
    let mut v = Vec::new();
    if let Some(record) = record {
        v.extend([Source::Sentient, Source::Database]);
        if record.flavor().uses_clickhouse() && !cfg!(windows) {
            v.push(Source::ClickHouse);
        }
    }
    if gateway_here() {
        v.push(Source::Gateway);
    }
    v
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = sentient_installer_core::host::host_command(cmd).args(args).output().ok()?;
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    // docker logs writes the container's stderr on stderr.
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(s)
}

fn tail_file(path: &std::path::Path, tail: u32) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(tail as usize);
    Some(lines[start..].join("\n"))
}

/// The newest file in `dir` whose name ends with `ext`.
fn newest(dir: &std::path::Path, ext: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(ext))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// The last `tail` lines of one source (or since midnight with `today`).
/// Docker and journal lines carry their own timestamp in front. The
/// gateway's log needs no SENTIENT installation; the other sources do.
pub fn read(record: Option<&InstallationRecord>, source: Source, tail: u32, today: bool) -> String {
    let n = tail.to_string();
    let docker = |container: &str| {
        let mut args = vec!["logs", "--timestamps", "--tail", n.as_str()];
        if today {
            args.retain(|a| *a != "--tail" && *a != n.as_str());
            args.extend(["--since", "24h"]);
        }
        args.push(container);
        // Windows: Docker runs in SENTIENT's WSL distro, not on Windows.
        let read = if cfg!(windows) {
            sentient_installer_core::distro::container_logs(container, (!today).then_some(tail))
        } else {
            run("docker", &args)
        };
        read.unwrap_or_else(|| format!("Could not read the {} log.", source.label()))
    };
    let journal = |unit: &str| {
        let mut args = vec!["-u", unit, "--no-pager", "-o", "short-iso"];
        if today {
            args.extend(["--since", "today"]);
        } else {
            args.extend(["-n", n.as_str()]);
        }
        run("journalctl", &args)
    };
    if source == Source::Gateway {
        // The Windows service first (as gateway::status finds it), then a
        // container, then a systemd service.
        return match (gateway_files(), gateway_container(), gateway_unit()) {
            (Some(dir), _, _) => {
                let since = today.then(|| (Utc::now() - chrono::Duration::hours(24)).format("%Y-%m-%dT%H:%M:%S").to_string());
                let tail = if today { 20_000 } else { tail as usize };
                sentient_installer_core::gateway::read_log_dir(&dir, tail, since.as_deref())
                    .unwrap_or_else(|| format!("No gateway log file yet in {}.", dir.display()))
            }
            (None, Some(c), _) => docker(&c),
            (None, None, Some(u)) => journal(&u).unwrap_or_else(|| "Could not read the gateway's journal.".into()),
            _ => "No gateway on this computer.".into(),
        };
    }
    let Some(record) = record else {
        return format!("SENTIENT is not installed on this computer, so it has no {} log.", source.label());
    };
    let names = record.stack.names();
    match (record.engine, source) {
        (Engine::Docker, Source::Sentient) => docker(&names.app),
        (Engine::Docker, Source::Database) => docker(&names.pg),
        (Engine::Docker, Source::ClickHouse) => docker(&names.ch),
        (_, Source::Gateway) => unreachable!("read above"),
        (Engine::Native, Source::Sentient) => record.platform().logs(tail),
        (Engine::Native, Source::Database) if cfg!(windows) => newest(&record.pg_dir.join("log"), ".log")
            .and_then(|p| tail_file(&p, tail))
            .unwrap_or_else(|| "No PostgreSQL log file yet.".into()),
        (Engine::Native, Source::Database) => newest(std::path::Path::new("/var/log/postgresql"), ".log")
            .and_then(|p| tail_file(&p, tail))
            .or_else(|| journal("postgresql*"))
            .unwrap_or_else(|| "Could not read the PostgreSQL log.".into()),
        (Engine::Native, Source::ClickHouse) => tail_file(std::path::Path::new("/var/log/clickhouse-server/clickhouse-server.log"), tail)
            .or_else(|| journal("clickhouse-server"))
            .unwrap_or_else(|| "Could not read the ClickHouse log.".into()),
    }
}

/// How often a followed file is looked at for new lines, and how often a
/// follow checks it is still wanted while nothing comes.
const FOLLOW_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// Follow one source as it grows (the Logs page's live view; on a server
/// the agent's `log-follow` job): every new line from now on goes to
/// `sink` as it is written, until `going` says stop. Lines from before
/// are [`read`]'s. Docker's `logs -f`, `journalctl -f`, or the log file
/// read as it grows. A source that cannot be followed this way (Docker in
/// Windows' WSL distro, the Windows gateway's rotating files) is an Err
/// starting with [`NO_FOLLOW`]: the page reads it again every few seconds
/// instead.
pub fn follow(record: Option<&InstallationRecord>, source: Source, going: &dyn Fn() -> bool, sink: &mut dyn FnMut(&str)) -> Result<(), String> {
    match follow_target(record, source)? {
        Target::Cmd(prog, args) => follow_cmd(&prog, &args, going, sink),
        Target::File(path) => follow_file(&path, going, sink),
    }
}

/// [`follow`]'s answer for a source it cannot follow.
pub const NO_FOLLOW: &str = "cannot be followed";

enum Target {
    Cmd(String, Vec<String>),
    File(std::path::PathBuf),
}

fn follow_target(record: Option<&InstallationRecord>, source: Source) -> Result<Target, String> {
    let no = || Err(format!("The {} log {NO_FOLLOW} here.", source.label()));
    let docker = |c: &str| -> Result<Target, String> {
        if cfg!(windows) {
            return Err(format!("The {} log {NO_FOLLOW} here.", source.label()));
        }
        Ok(Target::Cmd("docker".into(), ["logs", "--follow", "--timestamps", "--tail", "0", c].map(String::from).to_vec()))
    };
    let journal = |unit: &str| Target::Cmd("journalctl".into(), ["-f", "-n", "0", "-u", unit, "--no-pager", "-o", "short-iso"].map(String::from).to_vec());
    if source == Source::Gateway {
        return match (gateway_files(), gateway_container(), gateway_unit()) {
            (Some(_), _, _) => no(),
            (None, Some(c), _) => docker(&c),
            (None, None, Some(u)) => Ok(journal(&u)),
            _ => Err("No gateway on this computer.".into()),
        };
    }
    let Some(record) = record else {
        return Err(format!("SENTIENT is not installed on this computer, so it has no {} log.", source.label()));
    };
    let names = record.stack.names();
    match (record.engine, source) {
        (Engine::Docker, Source::Sentient) => docker(&names.app),
        (Engine::Docker, Source::Database) => docker(&names.pg),
        (Engine::Docker, Source::ClickHouse) => docker(&names.ch),
        (_, Source::Gateway) => unreachable!("above"),
        (Engine::Native, Source::Sentient) => match sentient_installer_core::native::log_follow(&record.native_config()) {
            sentient_installer_core::native::LogFollow::File(p) => Ok(Target::File(p)),
            sentient_installer_core::native::LogFollow::Journal(u) => Ok(journal(&u)),
        },
        (Engine::Native, Source::Database) if cfg!(windows) => newest(&record.pg_dir.join("log"), ".log").map(Target::File).ok_or_else(|| "No PostgreSQL log file yet.".into()),
        (Engine::Native, Source::Database) => Ok(newest(std::path::Path::new("/var/log/postgresql"), ".log").map(Target::File).unwrap_or_else(|| journal("postgresql*"))),
        (Engine::Native, Source::ClickHouse) => {
            let p = std::path::PathBuf::from("/var/log/clickhouse-server/clickhouse-server.log");
            Ok(if p.exists() { Target::File(p) } else { journal("clickhouse-server") })
        }
    }
}

/// A program that writes the log as it grows (`docker logs -f`,
/// `journalctl -f`): each line of its output and its error output to
/// `sink`; ended when `going` says stop.
fn follow_cmd(prog: &str, args: &[String], going: &dyn Fn() -> bool, sink: &mut dyn FnMut(&str)) -> Result<(), String> {
    use std::io::BufRead;
    use std::process::Stdio;
    let mut child = sentient_installer_core::host::host_command(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog} could not be started: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut readers = Vec::new();
    let out: Box<dyn std::io::Read + Send> = Box::new(child.stdout.take().ok_or("no output")?);
    let err: Box<dyn std::io::Read + Send> = Box::new(child.stderr.take().ok_or("no output")?);
    for r in [out, err] {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(r).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);
    let mut ended = false;
    while going() {
        match rx.recv_timeout(FOLLOW_TICK) {
            Ok(l) => sink(&l),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                ended = true;
                break;
            }
        }
    }
    let _ = child.kill();
    let status = child.wait();
    for r in readers {
        let _ = r.join();
    }
    if ended && going() {
        let code = status.ok().and_then(|s| s.code());
        return Err(format!("{prog} stopped following the log (exit {}).", code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())));
    }
    Ok(())
}

/// A log file read as it grows: from its end now, every [`FOLLOW_TICK`]
/// what was added; a file made again or cut shorter (rotated) is read
/// from its start.
fn follow_file(path: &std::path::Path, going: &dyn Fn() -> bool, sink: &mut dyn FnMut(&str)) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut pos = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    let mut partial = String::new();
    while going() {
        std::thread::sleep(FOLLOW_TICK);
        let Ok(meta) = std::fs::metadata(path) else { continue };
        let len = meta.len();
        if len < pos {
            pos = 0;
            partial.clear();
        }
        if len == pos {
            continue;
        }
        let Ok(mut f) = std::fs::File::open(path) else { continue };
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
            sink(line.trim_end_matches(['\r', '\n']));
        }
    }
    Ok(())
}

/// A message [`read`] gives instead of a log (the source is not there or
/// could not be read): an error for a save, never the file's content.
pub fn is_failure_text(s: &str) -> bool {
    let t = s.trim_start();
    ["Could not read", "No gateway", "SENTIENT is not installed", "No PostgreSQL log", "no log yet at", "No such log"].iter().any(|p| t.starts_with(p))
}

/// Copy `from` to `out`, counting bytes; `going` false stops it (Cancel).
fn pump(from: &mut dyn std::io::Read, out: &mut dyn std::io::Write, going: &dyn Fn(u64) -> bool) -> Result<u64, String> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut n = 0u64;
    loop {
        if !going(n) {
            return Err("Cancelled.".into());
        }
        let k = from.read(&mut buf).map_err(|e| format!("reading the log: {e}"))?;
        if k == 0 {
            return Ok(n);
        }
        out.write_all(&buf[..k]).map_err(|e| format!("writing the log: {e}"))?;
        n += k as u64;
    }
}

/// A program's output into `out` as it comes (a day of a container's log can
/// be hundreds of megabytes: never held in memory).
fn stream_cmd(prog: &str, args: &[String], out: &mut dyn std::io::Write, going: &dyn Fn(u64) -> bool) -> Result<u64, String> {
    use std::process::Stdio;
    let mut child = sentient_installer_core::host::host_command(prog)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog} could not be started: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let mut stderr = child.stderr.take().ok_or("no output")?;
    let err_reader = std::thread::spawn(move || {
        let mut e = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut e);
        e
    });
    let copied = pump(&mut stdout, out, going);
    if copied.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_reader.join().unwrap_or_default();
    let n = copied?;
    if !status.success() && n == 0 {
        let why = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("it gave no reason");
        return Err(format!("{prog} could not read the log: {why}"));
    }
    Ok(n)
}

/// The lines of a log file from the last 24 hours into `out`: a line with a
/// time is kept when that time is in the window, a line without one goes
/// with the line before it (a stack trace).
fn stream_file_today(path: &std::path::Path, out: &mut dyn std::io::Write, going: &dyn Fn(u64) -> bool) -> Result<u64, String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let since = Utc::now() - chrono::Duration::hours(24);
    let mut keep = false;
    let mut n = 0u64;
    for line in std::io::BufReader::with_capacity(256 * 1024, f).split(b'\n') {
        let line = line.map_err(|e| format!("reading {}: {e}", path.display()))?;
        if !going(n) {
            return Err("Cancelled.".into());
        }
        let text = String::from_utf8_lossy(&line);
        if let Some((t, _)) = take_time(&text) {
            keep = t >= since;
        }
        if keep {
            out.write_all(&line).and_then(|_| out.write_all(b"\n")).map_err(|e| format!("writing the log: {e}"))?;
            n += line.len() as u64 + 1;
        }
    }
    Ok(n)
}

/// [`read`] into `out` without holding the log: since midnight's 24 hours
/// (`today`) streams from Docker, the journal or the log file; a number of
/// last lines is read as [`read`] does (bounded) and written. An unreadable
/// source is an error, never written as the log. `going(bytes)` false
/// cancels. Returns the bytes written.
pub fn read_to(record: Option<&InstallationRecord>, source: Source, tail: u32, today: bool, out: &mut dyn std::io::Write, going: &dyn Fn(u64) -> bool) -> Result<u64, String> {
    let text = |out: &mut dyn std::io::Write| -> Result<u64, String> {
        let t = read(record, source, tail, today);
        if is_failure_text(&t) {
            return Err(t.trim().to_string());
        }
        out.write_all(t.as_bytes()).and_then(|_| out.write_all(b"\n")).map_err(|e| format!("writing the log: {e}"))?;
        Ok(t.len() as u64 + 1)
    };
    if !today || cfg!(windows) {
        return text(out);
    }
    let docker = |c: &str, out: &mut dyn std::io::Write| stream_cmd("sh", &["-c".into(), format!("docker logs --timestamps --since 24h '{}' 2>&1", c.replace('\'', ""))], out, going);
    let journal = |unit: &str, out: &mut dyn std::io::Write| stream_cmd("journalctl", &["-u".into(), unit.into(), "--no-pager".into(), "-o".into(), "short-iso".into(), "--since".into(), "-24h".into()], out, going);
    if source == Source::Gateway {
        return match (gateway_files(), gateway_container(), gateway_unit()) {
            (Some(_), _, _) => text(out),
            (None, Some(c), _) => docker(&c, out),
            (None, None, Some(u)) => journal(&u, out),
            _ => Err("No gateway on this computer.".into()),
        };
    }
    let Some(record) = record else {
        return Err(format!("SENTIENT is not installed on this computer, so it has no {} log.", source.label()));
    };
    let names = record.stack.names();
    let file_or = |p: &std::path::Path, unit: &str, out: &mut dyn std::io::Write| -> Result<u64, String> {
        if std::fs::File::open(p).is_ok() { stream_file_today(p, out, going) } else { journal(unit, out) }
    };
    match (record.engine, source) {
        (Engine::Docker, Source::Sentient) => docker(&names.app, out),
        (Engine::Docker, Source::Database) => docker(&names.pg, out),
        (Engine::Docker, Source::ClickHouse) => docker(&names.ch, out),
        (_, Source::Gateway) => unreachable!("read above"),
        (Engine::Native, Source::Sentient) => file_or(std::path::Path::new("/var/log/sentient/sentient.log"), "sentient", out),
        (Engine::Native, Source::Database) => match newest(std::path::Path::new("/var/log/postgresql"), ".log") {
            Some(p) => stream_file_today(&p, out, going),
            None => journal("postgresql*", out),
        },
        (Engine::Native, Source::ClickHouse) => file_or(std::path::Path::new("/var/log/clickhouse-server/clickhouse-server.log"), "clickhouse-server", out),
    }
}

/// The temporary file `logs-file` writes: `<temp>/spm-log-<pid>-<time>.log.gz`.
fn packed_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("spm-log-{}-{}.log.gz", std::process::id(), Utc::now().format("%Y%m%d%H%M%S")))
}

/// Whether `p` is a file [`save_packed`] wrote (the only kind
/// `logs-file-remove` deletes).
pub fn is_packed_path(p: &str) -> bool {
    let path = std::path::Path::new(p);
    path.parent() == Some(std::env::temp_dir().as_path())
        && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("spm-log-") && n.ends_with(".log.gz") && !n.contains('/'))
}

/// [`read_to`] into a gzipped temporary file readable by the account that
/// downloads it: (path, log bytes, file bytes).
pub fn save_packed(record: Option<&InstallationRecord>, source: Source, tail: u32, today: bool) -> Result<(std::path::PathBuf, u64, u64), String> {
    let path = packed_path();
    let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
    }
    let mut gz = flate2::write::GzEncoder::new(std::io::BufWriter::new(file), flate2::Compression::fast());
    let r = read_to(record, source, tail, today, &mut gz, &|_| true);
    let done = r.and_then(|bytes| gz.finish().and_then(|mut w| std::io::Write::flush(&mut w)).map(|_| bytes).map_err(|e| format!("writing {}: {e}", path.display())));
    match done {
        Ok(bytes) => {
            let packed = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            Ok((path, bytes, packed))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

/// One line as columns.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Local time, `2026-11-15 16:40:12`; empty when nothing gives one.
    pub time: String,
    /// For ordering the "All" view.
    pub at: Option<DateTime<Utc>>,
    pub level: Level,
    pub message: String,
    pub source: Source,
}

/// A local-time column value.
fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Read a leading timestamp from `s`: (time, rest).
fn take_time(s: &str) -> Option<(DateTime<Utc>, &str)> {
    let s = s.trim_start();
    let first = s.split_whitespace().next()?;
    // RFC 3339 / ISO with zone: Docker, SENTIENT, journal short-iso.
    if let Ok(t) = DateTime::parse_from_rfc3339(first) {
        return Some((t.with_timezone(&Utc), &s[first.len()..]));
    }
    if let Ok(t) = DateTime::parse_from_str(first, "%Y-%m-%dT%H:%M:%S%z") {
        return Some((t.with_timezone(&Utc), &s[first.len()..]));
    }
    // "2026-11-15 10:12:13.123 UTC" (PostgreSQL) or local "2026.11.15 10:12:13.123" (ClickHouse).
    let mut words = s.split_whitespace();
    let (d, t) = (words.next()?, words.next()?);
    let joined = format!("{} {}", d.replace('.', "-"), t.replace(',', "."));
    let naive = NaiveDateTime::parse_from_str(&joined, "%Y-%m-%d %H:%M:%S%.f").or_else(|_| NaiveDateTime::parse_from_str(&joined, "%Y-%m-%d %H:%M:%S")).ok()?;
    let after_t = s.find(t).map(|i| i + t.len()).unwrap_or(0);
    let rest = &s[after_t..];
    let zone = rest.split_whitespace().next().unwrap_or("");
    if zone == "UTC" || zone == "GMT" {
        let skip = rest.find(zone).map(|i| i + zone.len()).unwrap_or(0);
        return Some((Utc.from_utc_datetime(&naive), &rest[skip..]));
    }
    // No zone: this computer's local time.
    let t = Local.from_local_datetime(&naive).single()?.with_timezone(&Utc);
    Some((t, rest))
}

/// Split one raw line into columns. A line without its own time takes the
/// one Docker or the journal put in front.
pub fn parse(raw: &str, source: Source) -> Line {
    let clean = crate::maintenance_ops::strip_ansi(raw);
    let mut at = None;
    let mut rest: &str = &clean;
    // Docker's / the journal's stamp first.
    if let Some((t, r)) = take_time(rest) {
        at = Some(t);
        rest = r;
        // journal short-iso: "host unit[pid]: message".
        if let Some(i) = rest.find("]: ") {
            if rest[..i].split_whitespace().count() <= 2 {
                rest = &rest[i + 3..];
            }
        }
    }
    // The line's own stamp wins and leaves the message.
    if let Some((t, r)) = take_time(rest) {
        at = Some(t);
        rest = r;
    }
    let (level, _) = crate::log_level::classify(rest);
    let mut message = rest.trim_start().to_string();
    // The level word is a column of its own: drop it from the front.
    // Longest first: "WARN" would leave "ING:" of "WARNING:".
    for w in ["WARNING:", "WARNING", "ERROR:", "FATAL:", "PANIC:", "LOG:", "ERROR", "WARN", "INFO", "DEBUG", "TRACE", "FATAL", "PANIC"] {
        if let Some(r) = message.strip_prefix(w) {
            message = r.trim_start().to_string();
            break;
        }
    }
    Line { time: at.map(local).unwrap_or_default(), at, level, message, source }
}

/// Lines of several sources, oldest first; lines without a time keep their
/// place after the line before them.
pub fn merge(mut sets: Vec<Vec<Line>>) -> Vec<Line> {
    let mut all: Vec<(DateTime<Utc>, usize, Line)> = Vec::new();
    let mut seq = 0;
    for set in sets.drain(..) {
        let mut last = DateTime::<Utc>::MIN_UTC;
        for l in set {
            let at = l.at.unwrap_or(last);
            last = at;
            all.push((at, seq, l));
            seq += 1;
        }
    }
    all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    all.into_iter().map(|(_, _, l)| l).collect()
}

/// "Times in UTC+05:30".
pub fn zone_label() -> String {
    let off = Local::now().offset().local_minus_utc();
    let (sign, off) = if off < 0 { ('-', -off) } else { ('+', off) };
    format!("Times in UTC{sign}{:02}:{:02}", off / 3600, (off % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save log: a day of a log file streams through without the old lines,
    /// a line with no time goes with the line before it, Cancel stops it.
    #[test]
    fn a_log_file_streams_its_last_day() {
        let dir = std::env::temp_dir().join(format!("spm-logday-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("x.log");
        let old = (Utc::now() - chrono::Duration::hours(30)).to_rfc3339();
        let new = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        std::fs::write(&f, format!("{old} INFO old\n  at old trace\n{new} INFO new\n  at new trace\n")).unwrap();
        let mut out = Vec::new();
        let n = stream_file_today(&f, &mut out, &|_| true).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(n as usize, text.len());
        assert!(!text.contains("old"));
        assert!(text.contains("INFO new\n  at new trace\n"));
        assert_eq!(stream_file_today(&f, &mut Vec::new(), &|_| false), Err("Cancelled.".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A program's output streams; a failing one with no output is an error.
    #[cfg(unix)]
    #[test]
    fn a_command_streams_or_fails_plainly() {
        let mut out = Vec::new();
        assert_eq!(stream_cmd("sh", &["-c".into(), "printf 'a\\nb\\n'".into()], &mut out, &|_| true), Ok(4));
        assert_eq!(out, b"a\nb\n");
        let e = stream_cmd("sh", &["-c".into(), "echo nope >&2; exit 3".into()], &mut Vec::new(), &|_| true).unwrap_err();
        assert!(e.contains("nope"), "{e}");
    }

    /// Only a file save_packed made may be deleted by logs-file-remove; a
    /// message read() gives instead of a log is an error, never the file.
    #[test]
    fn packed_paths_and_failure_texts() {
        let t = std::env::temp_dir();
        assert!(is_packed_path(&t.join("spm-log-12-20261121.log.gz").display().to_string()));
        assert!(!is_packed_path(&t.join("other.log.gz").display().to_string()));
        assert!(!is_packed_path("/etc/spm-log-1.log.gz"));
        assert!(is_failure_text("Could not read the SENTIENT log."));
        assert!(is_failure_text("No gateway on this computer."));
        assert!(!is_failure_text("2026-11-21T10:00:00Z INFO started"));
    }

    #[test]
    fn docker_and_own_stamps() {
        let l = parse("2026-11-15T10:12:13.123456789Z 2026-11-15T10:12:13.120Z  INFO sentient_api: listening", Source::Sentient);
        assert_eq!(l.level, Level::Info);
        assert!(l.message.starts_with("sentient_api: listening"), "{}", l.message);
        assert_eq!(l.at.unwrap().timestamp(), DateTime::parse_from_rfc3339("2026-11-15T10:12:13Z").unwrap().timestamp());
        assert!(!l.time.is_empty());

        let pg = parse("2026-11-15T10:12:13.5Z 2026-11-15 10:12:13.456 UTC [42] ERROR:  relation \"x\" does not exist", Source::Database);
        assert_eq!(pg.level, Level::Error);
        assert!(pg.message.contains("relation"), "{}", pg.message);

        // A continuation line takes Docker's time.
        let cont = parse("2026-11-15T10:12:14Z     at frame 3", Source::Sentient);
        assert_eq!(cont.at.unwrap().timestamp(), DateTime::parse_from_rfc3339("2026-11-15T10:12:14Z").unwrap().timestamp());
        assert_eq!(cont.message, "at frame 3");

        // The journal's short-iso form.
        let j = parse("2026-11-15T15:42:13+0530 host postgres[811]: 2026-11-15 10:12:13.000 UTC [811] LOG:  checkpoint starting", Source::Database);
        assert!(j.message.starts_with("[811]") || j.message.contains("checkpoint"), "{}", j.message);
    }

    #[test]
    fn merging_orders_by_time() {
        let a = vec![parse("2026-11-15T10:00:02Z INFO a2", Source::Sentient)];
        let b = vec![parse("2026-11-15T10:00:01Z LOG: b1", Source::Database), parse("2026-11-15T10:00:03Z LOG: b3", Source::Database)];
        let m: Vec<String> = merge(vec![a, b]).into_iter().map(|l| l.message).collect();
        assert_eq!(m, vec!["b1", "a2", "b3"]);
    }
}

/// "Logs" on a Status service row: the source the Logs page opens on next, and
/// that the page should open.
static PICK: std::sync::Mutex<Option<Source>> = std::sync::Mutex::new(None);
static GO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request(source: Source) {
    if let Ok(mut p) = PICK.lock() {
        *p = Some(source);
    }
    GO.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The source asked for, once.
pub fn take_pick() -> Option<Source> {
    PICK.lock().ok().and_then(|mut p| p.take())
}

/// Whether the Logs page was asked for, once.
pub fn take_go() -> bool {
    GO.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// The log source of a service, from its name.
pub fn of_service(name: &str) -> Source {
    let n = name.to_lowercase();
    if n.contains("postgres") {
        Source::Database
    } else if n.contains("clickhouse") {
        Source::ClickHouse
    } else if n.contains("gateway") {
        Source::Gateway
    } else {
        Source::Sentient
    }
}
