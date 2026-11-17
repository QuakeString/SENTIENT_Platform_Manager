// Copyright © 2016-2026 The SENTIENT Authors

//! Statistics: one reading of the computer (processor, memory, swap, load,
//! network, disk activity, each SENTIENT service, the database numbers) and
//! its history.
//!
//! A small sampler (`--job stats-sample`, run once a minute by a systemd
//! timer or a scheduled task, installed with `--job stats-install`) appends a
//! reading to a history file of at most a week of minutes (about 2 MB). The
//! Statistics page reads one live reading every few seconds while it is open
//! (`--job stats-live`) and the history for 1 hour, 24 hours or 7 days
//! (`--job stats-history <seconds>`), on this computer or through the agent
//! on a server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::installation::{Engine, InstallationRecord};

/// A week of one-minute readings.
pub const KEEP: usize = 7 * 24 * 60;
/// The history file is rewritten once it holds this many more readings.
const SLACK: usize = 600;
/// The database is read every this many minutes by the sampler (its sizes
/// change slowly and reading them costs a query).
const DB_EVERY_MIN: i64 = 15;
/// The most points a chart gets.
pub const MAX_POINTS: usize = 360;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix seconds.
    pub t: i64,
    /// 0–100.
    pub cpu: f32,
    pub cores: u32,
    /// One-minute load average (None on Windows).
    #[serde(default)]
    pub load: Option<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub disk_free: u64,
    pub disk_total: u64,
    /// Bytes per second over the reading.
    pub rx: f64,
    pub tx: f64,
    /// Disk reads and writes, bytes per second (None where not read).
    #[serde(default)]
    pub io: Option<f64>,
    /// When the computer started, unix seconds.
    pub boot: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<Service>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<Db>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Service {
    /// The container or unit name.
    pub name: String,
    /// 0–100 of the whole computer.
    pub cpu: f32,
    pub mem: u64,
    #[serde(default)]
    pub restarts: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Db {
    pub connections: u32,
    pub max_connections: u32,
    pub pg_bytes: u64,
    #[serde(default)]
    pub ch_bytes: Option<u64>,
    #[serde(default)]
    pub timescale: bool,
    /// RFC 3339 and size of the last backup.
    #[serde(default)]
    pub last_backup: Option<(String, Option<u64>)>,
}

impl Sample {
    pub fn cpu_high(&self) -> bool {
        self.cpu >= 90.0
    }

    pub fn mem_high(&self) -> bool {
        self.mem_total > 0 && self.mem_used as f64 / self.mem_total as f64 >= 0.9
    }

    pub fn disk_low(&self) -> bool {
        self.disk_total > 0 && self.disk_free < crate::dbstatus::LOW_DISK
    }
}

// ---------------------------------------------------------------------------
// One reading
// ---------------------------------------------------------------------------

/// The quick part of a reading: processor, memory, swap, load, disk,
/// network and uptime, all from /proc and sysinfo (no service manager, no
/// database, no password). It keeps the counters of the reading before, so
/// each reading is a rate since the last one and never waits, except the
/// very first, which waits [`FIRST_WINDOW`].
pub struct Reader {
    sys: sysinfo::System,
    nets: sysinfo::Networks,
    io: Option<u64>,
    at: std::time::Instant,
    data_dir: PathBuf,
}

/// The first reading's window: long enough for a processor figure, short
/// enough that the page's first cards show within a fifth of a second.
pub const FIRST_WINDOW: Duration = Duration::from_millis(120);

impl Reader {
    pub fn new(record: Option<&InstallationRecord>) -> Self {
        use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Networks, RefreshKind, System};
        let sys = System::new_with_specifics(RefreshKind::new().with_cpu(CpuRefreshKind::new().with_cpu_usage()).with_memory(MemoryRefreshKind::everything()));
        Self { sys, nets: Networks::new_with_refreshed_list(), io: disk_io_bytes(), at: std::time::Instant::now(), data_dir: data_dir(record) }
    }

    /// A reading over the time since the last one (at least `min_window`:
    /// it waits for the rest). Blocking. No services, no database.
    pub fn read(&mut self, min_window: Duration) -> Sample {
        use sysinfo::{Disks, System};
        let since = self.at.elapsed();
        if since < min_window {
            std::thread::sleep(min_window - since);
        }
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.nets.refresh();
        let secs = self.at.elapsed().as_secs_f64().max(0.001);
        self.at = std::time::Instant::now();
        let (rx, tx) = self
            .nets
            .iter()
            .filter(|(name, _)| !is_loopback(name))
            .fold((0u64, 0u64), |(r, t), (_, n)| (r + n.received(), t + n.transmitted()));
        let io_now = disk_io_bytes();
        let io = match (self.io, io_now) {
            (Some(a), Some(b)) => Some(b.saturating_sub(a) as f64 / secs),
            _ => None,
        };
        self.io = io_now;
        let disks = Disks::new_with_refreshed_list();
        let disk = disks
            .list()
            .iter()
            .filter(|d| self.data_dir.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
            .or_else(|| disks.list().iter().max_by_key(|d| d.total_space()));
        let load = System::load_average();
        let sys = &self.sys;
        Sample {
            t: chrono::Utc::now().timestamp(),
            cpu: sys.global_cpu_usage(),
            cores: sys.cpus().len() as u32,
            load: (!cfg!(windows)).then_some(load.one as f32),
            mem_used: sys.used_memory(),
            mem_total: sys.total_memory(),
            swap_used: sys.used_swap(),
            swap_total: sys.total_swap(),
            disk_free: disk.map(|d| d.available_space()).unwrap_or(0),
            disk_total: disk.map(|d| d.total_space()).unwrap_or(0),
            rx: rx as f64 / secs,
            tx: tx as f64 / secs,
            io,
            boot: System::boot_time() as i64,
            services: Vec::new(),
            db: None,
        }
    }
}

/// This computer's reader, shared by the top bar's monitor and the
/// Statistics page: the top bar reads every few seconds, so when Statistics
/// opens its first reading is a rate over those seconds and waits for
/// nothing.
static LOCAL: std::sync::Mutex<Option<Reader>> = std::sync::Mutex::new(None);

/// A reading of this computer from the shared reader (at least
/// `min_window` since the one before it). Blocking.
pub fn local_read(record: Option<&InstallationRecord>, min_window: Duration) -> Sample {
    // The lock is held only while reading, never while waiting (the other
    // reader would wait too) and never across an await.
    let wait = {
        let mut guard = LOCAL.lock().unwrap_or_else(|e| e.into_inner());
        let reader = guard.get_or_insert_with(|| Reader::new(record));
        min_window.saturating_sub(reader.at.elapsed())
    };
    std::thread::sleep(wait);
    let mut guard = LOCAL.lock().unwrap_or_else(|e| e.into_inner());
    let reader = guard.get_or_insert_with(|| Reader::new(record));
    reader.data_dir = data_dir(record);
    reader.read(Duration::ZERO)
}

/// The folder whose disk the figures show: the data's.
fn data_dir(record: Option<&InstallationRecord>) -> PathBuf {
    record
        .map(|r| if r.pg_dir.as_os_str().is_empty() { r.install_dir.clone() } else { r.pg_dir.clone() })
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }))
}

/// One part of a reading, read on its own so a slow part never holds the
/// others back (the page and `--job stats-live <part>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// Processor, memory, disk, network, uptime ([`Reader`]).
    System,
    /// Each SENTIENT service (systemctl or docker stats: a second or two).
    Services,
    /// The database numbers (a query; on native Linux only when its
    /// password is readable without asking).
    Db,
}

impl Part {
    pub fn arg(self) -> &'static str {
        match self {
            Part::System => "system",
            Part::Services => "services",
            Part::Db => "db",
        }
    }

    pub fn from_arg(s: &str) -> Option<Part> {
        [Part::System, Part::Services, Part::Db].into_iter().find(|p| p.arg() == s)
    }
}

/// The database numbers as a page or the sampler may read them: never
/// asking for a password.
#[derive(Debug, Clone, PartialEq)]
pub enum DbRead {
    Read(Db),
    /// Its password is readable only by the administrator here (native
    /// Linux): not read, and nothing asked. The last backup needs no
    /// password.
    /// `after_sign_in`: this account was just given read access (group
    /// sentient-admin), which takes effect at its next sign-in.
    NeedsAdmin { last_backup: Option<(String, Option<u64>)>, after_sign_in: bool },
    /// No installation, or the database did not answer.
    None,
}

/// Read the database numbers unless that needs the administrator password.
pub fn db_unprompted(record: Option<&InstallationRecord>) -> DbRead {
    let Some(r) = record else { return DbRead::None };
    if crate::dbstatus::needs_admin(r) && !crate::rootjob::is_root() {
        return DbRead::NeedsAdmin { last_backup: crate::dbstatus::last_backup().map(|b| (b.at, b.bytes)), after_sign_in: crate::dbstatus::after_sign_in(r) };
    }
    db(r).map(DbRead::Read).unwrap_or(DbRead::None)
}

/// Each SENTIENT service's processor (over `window`) and memory. Blocking.
pub fn services_over(record: Option<&InstallationRecord>, window: Duration) -> Vec<Service> {
    let Some(r) = record else { return Vec::new() };
    let window = window.max(Duration::from_millis(100));
    let before = service_cpu_ns(Some(r));
    if r.engine == Engine::Native && !cfg!(windows) {
        std::thread::sleep(window);
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as u32;
    services(r, before, window.as_secs_f64(), cores)
}

/// One part only, as `--job stats-live <part>` answers it.
pub fn read_part(record: Option<&InstallationRecord>, part: Part) -> Sample {
    match part {
        Part::System => {
            let mut r = Reader::new(record);
            r.read(Duration::from_millis(300))
        }
        Part::Services => Sample { t: chrono::Utc::now().timestamp(), services: services_over(record, Duration::from_secs(1)), ..Default::default() },
        Part::Db => Sample {
            t: chrono::Utc::now().timestamp(),
            db: match db_unprompted(record) {
                DbRead::Read(d) => Some(d),
                _ => None,
            },
            ..Default::default()
        },
    }
}

/// Read the computer over `window` (the processor, network and disk figures
/// are rates over it): the three parts side by side. Blocking. `with_db`:
/// also read the database numbers (never asking for a password).
pub fn read(record: Option<&InstallationRecord>, window: Duration, with_db: bool) -> Sample {
    let window = window.max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    std::thread::scope(|scope| {
        let services = scope.spawn(|| services_over(record, window));
        let db = with_db.then(|| scope.spawn(|| db_unprompted(record)));
        let mut reader = Reader::new(record);
        let mut s = reader.read(window);
        s.services = services.join().unwrap_or_default();
        s.db = match db.map(|h| h.join()) {
            Some(Ok(DbRead::Read(d))) => Some(d),
            _ => None,
        };
        s
    })
}

fn is_loopback(name: &str) -> bool {
    name == "lo" || name.starts_with("Loopback") || name.starts_with("veth") || name.starts_with("docker") || name.starts_with("br-")
}

/// Bytes read and written by the whole disks so far (Linux /proc/diskstats).
fn disk_io_bytes() -> Option<u64> {
    if cfg!(windows) {
        return None;
    }
    std::fs::read_to_string("/proc/diskstats").ok().map(|t| parse_diskstats(&t))
}

/// Sectors read (field 6) and written (field 10) of the whole disks, in
/// bytes; partitions and virtual devices are left out.
pub fn parse_diskstats(text: &str) -> u64 {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let name = *f.get(2)?;
            if !is_whole_disk(name) {
                return None;
            }
            let read: u64 = f.get(5)?.parse().ok()?;
            let written: u64 = f.get(9)?.parse().ok()?;
            Some((read + written) * 512)
        })
        .sum()
}

fn is_whole_disk(name: &str) -> bool {
    let digits_tail = |prefix: &str| name.strip_prefix(prefix).is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase()));
    if digits_tail("sd") || digits_tail("vd") || digits_tail("xvd") || digits_tail("hd") {
        return true;
    }
    // nvme0n1 (not nvme0n1p2), mmcblk0 (not mmcblk0p1).
    if let Some(r) = name.strip_prefix("nvme") {
        return r.contains('n') && !r.contains('p');
    }
    if let Some(r) = name.strip_prefix("mmcblk") {
        return !r.is_empty() && r.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// Native Linux services: their processor time so far (ns), by unit.
fn service_cpu_ns(record: Option<&InstallationRecord>) -> Vec<(String, u64)> {
    let Some(r) = record.filter(|r| r.engine == Engine::Native && !cfg!(windows)) else { return Vec::new() };
    unit_names(r)
        .into_iter()
        .filter_map(|u| {
            let out = sentient_installer_core::host::host_command("systemctl").args(["show", "-p", "CPUUsageNSec", "--value", &u]).output().ok()?;
            let ns = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
            Some((u, ns))
        })
        .collect()
}

fn unit_names(r: &InstallationRecord) -> Vec<String> {
    r.platform().snapshot().services.into_iter().map(|s| s.name).collect()
}

/// Each SENTIENT service's processor and memory use.
fn services(r: &InstallationRecord, cpu_before: Vec<(String, u64)>, secs: f64, cores: u32) -> Vec<Service> {
    match r.engine {
        Engine::Docker => docker_services(&unit_names(r)),
        Engine::Native if cfg!(windows) => Vec::new(),
        Engine::Native => cpu_before
            .into_iter()
            .map(|(unit, before)| {
                let show = sentient_installer_core::host::host_command("systemctl")
                    .args(["show", "-p", "CPUUsageNSec", "-p", "MemoryCurrent", "-p", "NRestarts", &unit])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default();
                let get = |k: &str| show.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).and_then(|v| v.trim().parse::<u64>().ok());
                let used = get("CPUUsageNSec").map(|now| now.saturating_sub(before)).unwrap_or(0);
                Service {
                    cpu: (used as f64 / (secs * 1e9) / cores.max(1) as f64 * 100.) as f32,
                    mem: get("MemoryCurrent").unwrap_or(0),
                    restarts: get("NRestarts").map(|n| n as u32),
                    name: unit,
                }
            })
            .collect(),
    }
}

fn docker_services(names: &[String]) -> Vec<Service> {
    if names.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["stats", "--no-stream", "--format", "{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}"];
    args.extend(names.iter().map(String::as_str));
    let stats = sentient_installer_core::host::host_command("docker").args(&args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut inspect = vec!["inspect", "--format", "{{.Name}}\t{{.RestartCount}}"];
    inspect.extend(names.iter().map(String::as_str));
    let restarts = sentient_installer_core::host::host_command("docker").args(&inspect).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    parse_docker_stats(&stats, &restarts)
}

/// `docker stats` lines (name, "3.10%", "1.2GiB / 7.7GiB") and `docker
/// inspect` lines ("/name", restarts) as services. Docker's percentage is of
/// one processor; it is shared out over all of them by the caller's figure.
pub fn parse_docker_stats(stats: &str, restarts: &str) -> Vec<Service> {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
    stats
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let name = f.next()?.trim().to_string();
            let cpu: f32 = f.next()?.trim().trim_end_matches('%').parse().ok()?;
            let mem = f.next()?.split('/').next().map(|m| parse_size(m.trim())).unwrap_or(0);
            let restarts = restarts.lines().find_map(|r| {
                let (n, c) = r.split_once('\t')?;
                (n.trim().trim_start_matches('/') == name).then(|| c.trim().parse().ok()).flatten()
            });
            Some(Service { name, cpu: cpu / cores, mem, restarts })
        })
        .collect()
}

/// "1.2GiB", "512MiB", "40kB" → bytes.
pub fn parse_size(s: &str) -> u64 {
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: f64 = n.trim().parse().unwrap_or(0.);
    let mult = match unit.trim() {
        "KiB" => 1024.,
        "MiB" => 1024. * 1024.,
        "GiB" => 1024. * 1024. * 1024.,
        "TiB" => 1024f64.powi(4),
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => 1.,
    };
    (n * mult) as u64
}

fn db(record: &InstallationRecord) -> Option<Db> {
    let s = crate::dbstatus::read(record);
    let pg = s.pg?;
    Some(Db {
        connections: pg.connections,
        max_connections: pg.max_connections,
        pg_bytes: pg.db_bytes,
        ch_bytes: s.ch.map(|c| c.disk_bytes),
        timescale: pg.timescaledb.is_some(),
        last_backup: crate::dbstatus::last_backup().map(|b| (b.at, b.bytes)),
    })
}

// ---------------------------------------------------------------------------
// The history file
// ---------------------------------------------------------------------------

/// Where the history is kept: beside the installation record when this runs
/// as the administrator (the server agent, the system timer), else in the
/// user's settings folder (this computer's user timer).
pub fn history_path() -> PathBuf {
    if crate::rootjob::is_root() || cfg!(windows) {
        crate::installation::record_path().with_file_name("stats.jsonl")
    } else {
        crate::updates::prefs_dir().join("stats.jsonl")
    }
}

/// Add a reading; keep the last KEEP of them.
pub fn append(path: &Path, s: &Sample) -> Result<(), String> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let line = serde_json::to_string(s).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;
    drop(f);
    trim(path, KEEP, SLACK)
}

/// Keep the last `keep` lines once there are `keep + slack`.
pub fn trim(path: &Path, keep: usize, slack: usize) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let n = text.lines().count();
    if n <= keep + slack {
        return Ok(());
    }
    let kept: Vec<&str> = text.lines().skip(n - keep).collect();
    let tmp = path.with_extension("jsonl.new");
    std::fs::write(&tmp, kept.join("\n") + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// The readings of the last `secs` seconds, at most MAX_POINTS of them (an
/// even spread; the newest is always there).
pub fn history(path: &Path, secs: i64, now: i64) -> Vec<Sample> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let from = now - secs;
    let all: Vec<Sample> = text.lines().filter_map(|l| serde_json::from_str::<Sample>(l).ok()).filter(|s| s.t >= from).collect();
    thin(all, MAX_POINTS)
}

/// At most `max` readings, evenly spread, keeping the last; a reading with
/// the database numbers carries them on to the next kept one when dropped.
pub fn thin(all: Vec<Sample>, max: usize) -> Vec<Sample> {
    if all.len() <= max {
        return all;
    }
    let step = all.len() as f64 / max as f64;
    let mut out: Vec<Sample> = Vec::with_capacity(max);
    let mut db: Option<Db> = None;
    let mut next = 0f64;
    let last = all.len() - 1;
    for (i, mut s) in all.into_iter().enumerate() {
        if s.db.is_some() {
            db = s.db.clone();
        }
        if i as f64 >= next || i == last {
            if s.db.is_none() {
                s.db = db.take();
            } else {
                db = None;
            }
            out.push(s);
            next += step;
        }
    }
    out
}

/// One reading for the history: the database every DB_EVERY_MIN minutes.
pub fn sample_now() -> Result<(), String> {
    let record = InstallationRecord::load();
    let minute = chrono::Utc::now().timestamp() / 60;
    let s = read(record.as_ref(), Duration::from_secs(1), minute % DB_EVERY_MIN == 0);
    append(&history_path(), &s)
}

// ---------------------------------------------------------------------------
// The sampler's timer
// ---------------------------------------------------------------------------

const UNIT: &str = "sentient-pm-stats";
#[cfg(windows)]
const TASK: &str = "SENTIENT Platform Manager statistics";

/// Run the sampler once a minute: a system timer as the administrator, a user
/// timer otherwise (this computer, no password asked), a scheduled task on
/// Windows. Idempotent.
pub fn install_sampler(exe: &Path) -> Result<String, String> {
    #[cfg(windows)]
    {
        let tr = format!("\"{}\" --job stats-sample", exe.display());
        let out = std::process::Command::new("schtasks.exe")
            .args(["/Create", "/TN", TASK, "/SC", "MINUTE", "/MO", "1", "/RU", "SYSTEM", "/TR", &tr, "/F"])
            .output()
            .map_err(|e| e.to_string())?;
        return if out.status.success() { Ok("scheduled task".into()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) };
    }
    #[cfg(not(windows))]
    {
        let root = crate::rootjob::is_root();
        let dir = if root {
            PathBuf::from("/etc/systemd/system")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .ok_or("no home folder")?
                .join("systemd/user")
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let (service, timer) = unit_files(exe);
        std::fs::write(dir.join(format!("{UNIT}.service")), service).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("{UNIT}.timer")), timer).map_err(|e| e.to_string())?;
        let ctl = |args: &[&str]| {
            let mut c = sentient_installer_core::host::host_command("systemctl");
            if !root {
                c.arg("--user");
            }
            c.args(args).output()
        };
        let _ = ctl(&["daemon-reload"]);
        let out = ctl(&["enable", "--now", &format!("{UNIT}.timer")]).map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(if root { "system timer".into() } else { "user timer".into() })
    }
}

/// The sampler's systemd unit and timer.
pub fn unit_files(exe: &Path) -> (String, String) {
    (
        format!("[Unit]\nDescription=SENTIENT Platform Manager statistics sample\n\n[Service]\nType=oneshot\nNice=10\nExecStart={} --job stats-sample\n", exe.display()),
        "[Unit]\nDescription=SENTIENT Platform Manager statistics (every minute)\n\n[Timer]\nOnCalendar=minutely\nAccuracySec=5s\n\n[Install]\nWantedBy=timers.target\n".to_string(),
    )
}

/// Is the sampler's timer there (this account's or the system's)?
pub fn sampler_installed() -> bool {
    #[cfg(windows)]
    {
        return std::process::Command::new("schtasks.exe").args(["/Query", "/TN", TASK]).output().is_ok_and(|o| o.status.success());
    }
    #[cfg(not(windows))]
    {
        let user = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("systemd/user").join(format!("{UNIT}.timer")));
        Path::new(&format!("/etc/systemd/system/{UNIT}.timer")).exists() || user.is_some_and(|p| p.exists())
    }
}

/// Screens: a canned live reading (`live.json` in `dir`), moving a little
/// each call so the live charts draw.
#[cfg(debug_assertions)]
pub fn debug_live(dir: &Path) -> Result<Sample, String> {
    // Screens: an agent too old, a failed read, a reading that never comes.
    if dir.join("agent-old").exists() {
        return Err(crate::remote::AGENT_OLD.into());
    }
    if dir.join("fail").exists() {
        return Err("The server did not answer (address, port or firewall).".into());
    }
    if dir.join("slow").exists() {
        std::thread::sleep(std::time::Duration::from_secs(600));
    }
    let mut s: Sample = serde_json::from_str(&std::fs::read_to_string(dir.join("live.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    let w = (now % 60) as f32;
    s.t = now;
    s.cpu = (s.cpu + (w * 0.7).sin() * 3.).max(0.);
    s.rx *= 1. + ((w * 0.5).sin() as f64) * 0.3;
    s.tx *= 1. + ((w * 0.3).cos() as f64) * 0.3;
    s.io = s.io.map(|v| v * (1. + ((w * 0.9).sin() as f64) * 0.4));
    Ok(s)
}

/// Screens: one part of the canned reading, after the delay
/// SPM_DEBUG_STATS_DELAY gives it ("services=4,db=8": seconds per part).
#[cfg(debug_assertions)]
pub fn debug_part(dir: &Path, part: Part) -> Result<Sample, String> {
    if let Some(secs) = std::env::var("SPM_DEBUG_STATS_DELAY")
        .ok()
        .and_then(|v| v.split(',').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == part.arg()).and_then(|(_, n)| n.parse::<f64>().ok())))
    {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }
    let mut s = debug_live(dir)?;
    match part {
        Part::System => {
            s.services.clear();
            s.db = None;
        }
        Part::Services => s.db = None,
        Part::Db => s.services.clear(),
    }
    Ok(s)
}

/// Screens: a canned history made from `live.json`: a week of minutes with
/// a daily rhythm and a few spikes.
#[cfg(debug_assertions)]
pub fn debug_history(dir: &Path, secs: i64) -> Vec<Sample> {
    if dir.join("empty").exists() {
        return Vec::new();
    }
    let Ok(base) = debug_live(dir) else { return Vec::new() };
    let now = base.t;
    let n = (secs / 60).max(1);
    let all: Vec<Sample> = (0..n)
        .map(|i| {
            let t = now - (n - 1 - i) * 60;
            let day = ((t % 86_400) as f32 / 86_400. * std::f32::consts::TAU).sin();
            let wob = ((t / 60) as f32 * 0.37).sin() * 0.5 + ((t / 60) as f32 * 0.11).cos() * 0.5;
            let spike = if (t / 60) % 97 == 0 { 3.5 } else { 1. };
            let mut s = base.clone();
            s.t = t;
            s.cpu = ((base.cpu + day * 4. + wob * 3.) * spike).clamp(0., 100.);
            s.mem_used = (base.mem_used as f64 * (1. + day as f64 * 0.03)) as u64;
            s.load = base.load.map(|l| (l + day * 0.2 + wob * 0.1).max(0.) * spike);
            s.rx = base.rx * (1. + day as f64 * 0.4 + wob as f64 * 0.2);
            s.tx = base.tx * (1. + day as f64 * 0.3 + wob as f64 * 0.2);
            s.io = base.io.map(|v| v * (1. + wob as f64 * 0.5) * spike as f64);
            s.disk_free = base.disk_free + ((now - t) as u64) * 2_000;
            s.db = base.db.clone().map(|mut d| {
                d.pg_bytes = d.pg_bytes.saturating_sub(((now - t) as u64) * 40);
                d.ch_bytes = d.ch_bytes.map(|c| c.saturating_sub(((now - t) as u64) * 120));
                d
            });
            s
        })
        .collect();
    thin(all, MAX_POINTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diskstats_counts_whole_disks_only() {
        let t = "   8       0 sda 100 0 2000 0 50 0 1000 0 0 0 0\n   8       1 sda1 90 0 1800 0 40 0 900 0 0 0 0\n 259       0 nvme0n1 10 0 100 0 5 0 50 0 0 0 0\n 259       1 nvme0n1p2 9 0 90 0 4 0 40 0 0 0 0\n   7       0 loop0 5 0 10 0 0 0 0 0 0 0 0\n";
        assert_eq!(parse_diskstats(t), (2000 + 1000 + 100 + 50) * 512);
        assert!(is_whole_disk("vda") && is_whole_disk("mmcblk0") && !is_whole_disk("mmcblk0p1") && !is_whole_disk("dm-0"));
    }

    #[test]
    fn docker_stats_lines_become_services() {
        let stats = "sentient\t310.00%\t1.2GiB / 7.7GiB\nsentient-postgres\t0.90%\t812MiB / 7.7GiB\n";
        let restarts = "/sentient\t0\n/sentient-postgres\t2\n";
        let s = parse_docker_stats(stats, restarts);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].restarts, Some(2));
        assert_eq!(s[0].mem, (1.2 * 1024. * 1024. * 1024.) as u64);
        assert_eq!(parse_size("812MiB"), 812 * 1024 * 1024);
        assert_eq!(parse_size("40kB"), 40_000);
    }

    #[test]
    fn the_history_keeps_a_week_and_reads_a_range() {
        let dir = std::env::temp_dir().join(format!("spm-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("stats.jsonl");
        for i in 0..50 {
            append(&path, &Sample { t: 1000 + i * 60, cpu: i as f32, ..Default::default() }).unwrap();
        }
        // Trimming: keep 20 once there are 30.
        trim(&path, 20, 10).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 20);
        let now = 1000 + 49 * 60;
        let h = history(&path, 10 * 60, now);
        assert_eq!(h.len(), 11);
        assert_eq!(h.last().unwrap().cpu, 49.);
        // A broken line is skipped.
        std::fs::write(&path, format!("{text}not json\n")).unwrap();
        assert_eq!(history(&path, 3600, now).len(), 20);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thinning_keeps_the_last_and_carries_the_database() {
        let mut all: Vec<Sample> = (0..1000).map(|i| Sample { t: i, ..Default::default() }).collect();
        all[3].db = Some(Db { pg_bytes: 7, ..Default::default() });
        let t = thin(all, 100);
        assert!(t.len() <= 101 && t.len() >= 99, "{}", t.len());
        assert_eq!(t.last().unwrap().t, 999);
        assert!(t.iter().any(|s| s.db.as_ref().is_some_and(|d| d.pg_bytes == 7)));
    }

    #[test]
    fn parts_name_themselves_and_read_alone() {
        for p in [Part::System, Part::Services, Part::Db] {
            assert_eq!(Part::from_arg(p.arg()), Some(p));
        }
        assert_eq!(Part::from_arg("all"), None);
        // The quick part: a first reading within its window, the next one
        // at once (a rate since the first), no services, no database.
        let mut r = Reader::new(None);
        let started = std::time::Instant::now();
        let a = r.read(FIRST_WINDOW);
        let b = r.read(Duration::ZERO);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(a.mem_total > 0 && a.cores > 0 && b.boot > 0);
        assert!(b.rx >= 0. && b.tx >= 0.);
        assert!(a.services.is_empty() && a.db.is_none());
        // Nothing installed: no services and no database to read.
        assert!(services_over(None, Duration::ZERO).is_empty());
        assert_eq!(db_unprompted(None), DbRead::None);
    }

    #[test]
    fn unit_files_run_the_sampler_every_minute() {
        let (s, t) = unit_files(Path::new("/opt/x/sentient-platform-manager"));
        assert!(s.contains("ExecStart=/opt/x/sentient-platform-manager --job stats-sample"));
        assert!(t.contains("OnCalendar=minutely"));
    }
}
