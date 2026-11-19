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
            Source::Database => t!("Database"),
            Source::ClickHouse => "ClickHouse",
            Source::Gateway => t!("Gateway"),
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

/// The sources this installation has.
pub fn available(record: &InstallationRecord) -> Vec<Source> {
    let mut v = vec![Source::Sentient, Source::Database];
    if record.flavor().uses_clickhouse() && !cfg!(windows) {
        v.push(Source::ClickHouse);
    }
    if gateway_files().is_some() || gateway_container().is_some() || gateway_unit().is_some() {
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
/// Docker and journal lines carry their own timestamp in front.
pub fn read(record: &InstallationRecord, source: Source, tail: u32, today: bool) -> String {
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
    let names = record.stack.names();
    match (record.engine, source) {
        (Engine::Docker, Source::Sentient) => docker(&names.app),
        (Engine::Docker, Source::Database) => docker(&names.pg),
        (Engine::Docker, Source::ClickHouse) => docker(&names.ch),
        // The Windows service first (as gateway::status finds it), then a
        // container, then a systemd service.
        (_, Source::Gateway) => match (gateway_files(), gateway_container(), gateway_unit()) {
            (Some(dir), _, _) => {
                let since = today.then(|| (Utc::now() - chrono::Duration::hours(24)).format("%Y-%m-%dT%H:%M:%S").to_string());
                let tail = if today { 20_000 } else { tail as usize };
                sentient_installer_core::gateway::read_log_dir(&dir, tail, since.as_deref())
                    .unwrap_or_else(|| format!("No gateway log file yet in {}.", dir.display()))
            }
            (None, Some(c), _) => docker(&c),
            (None, None, Some(u)) => journal(&u).unwrap_or_else(|| "Could not read the gateway's journal.".into()),
            _ => "No gateway on this computer.".into(),
        },
        (Engine::Native, Source::Sentient) => record.platform().logs(tail),
        (Engine::Native, Source::Database) if cfg!(windows) => newest(&record.pg_dir.join("log"), ".log")
            .and_then(|p| tail_file(&p, tail))
            .unwrap_or_else(|| t!("No PostgreSQL log file yet.").into()),
        (Engine::Native, Source::Database) => newest(std::path::Path::new("/var/log/postgresql"), ".log")
            .and_then(|p| tail_file(&p, tail))
            .or_else(|| journal("postgresql*"))
            .unwrap_or_else(|| t!("Could not read the PostgreSQL log.").into()),
        (Engine::Native, Source::ClickHouse) => tail_file(std::path::Path::new("/var/log/clickhouse-server/clickhouse-server.log"), tail)
            .or_else(|| journal("clickhouse-server"))
            .unwrap_or_else(|| t!("Could not read the ClickHouse log.").into()),
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
    tf!("Times in UTC{sign}{:02}:{:02}", off / 3600, (off % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

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
