// Copyright © 2016-2026 The SENTIENT Authors

//! The restore system behind the Backup page's Restore flow, on this
//! computer or on a server through its agent (`--job restore-*`):
//!
//! 1. [`inspect`]: the file read through, every part against its SHA-256,
//!    with the backup's version, schema and type against the instance's
//!    (the Check step, and the blocks it can end in).
//! 2. [`precheck`]: room on each disk the restore writes to, the time it
//!    takes, the upgrade after, the platform settings that differ, the
//!    instance's own counts and the telemetry that would lose its device.
//! 3. [`run`]: a safety backup when asked, the backup restored beside the
//!    live data with the chosen scope (or, for telemetry only and a date
//!    range, into the running data), the instance's telemetry carried over
//!    for "Keep", the schema brought up, the switch (which puts the previous
//!    data back when SENTIENT does not answer), the settings taken from the
//!    backup, and the data from before deleted when asked. A free-space
//!    watch stops the run cleanly before a disk fills. Every run is written
//!    to the restore history with its log.
//!
//! A job's answers travel as log lines `RESTORE\t<kind>\t<json>` (both
//! through a root job here and through a server's agent).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sentient_backup_core::manifest::{FORMAT_VERSION, LicenceInfo, Manifest, PlatformInfo, TlsInfo};
use sentient_backup_core::restore_scope::{self as rs, Estimate, Scope};
use sentient_backup_core::space::{DEFAULT_MIN_FREE, DiskFree, SpaceWatch};
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use crate::installation::{Engine, InstallationRecord, flavor_label};
use crate::migration::Setup;

// ---------------------------------------------------------------------------
// Answers as log lines
// ---------------------------------------------------------------------------

pub const DATA: &str = "RESTORE\t";

pub fn data_line(kind: &str, value: &impl Serialize) -> Progress {
    Progress::Log { line: format!("{DATA}{kind}\t{}", serde_json::to_string(value).unwrap_or_default()) }
}

/// The last `kind` answer among a job's lines.
pub fn read_data<T: serde::de::DeserializeOwned>(lines: &[impl AsRef<str>], kind: &str) -> Option<T> {
    let prefix = format!("{DATA}{kind}\t");
    lines.iter().rev().find_map(|l| l.as_ref().strip_prefix(&prefix).and_then(|j| serde_json::from_str(j).ok()))
}

/// A line that is an answer, not for the log.
pub fn is_data(line: &str) -> bool {
    line.starts_with(DATA) || line.starts_with("PART\t")
}

/// A message as a sentence, so another can follow it.
fn sentence(e: &str) -> String {
    let e = e.trim_end();
    if e.ends_with(['.', '!', '?']) { e.to_string() } else { format!("{e}.") }
}

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map(|rt| rt.block_on(future)).map_err(|e| format!("start the runtime: {e}"))
}

// ---------------------------------------------------------------------------
// The instance a restore goes into
// ---------------------------------------------------------------------------

/// What the flow compares a backup with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Here {
    pub sentient_version: String,
    pub schema_version: Option<i64>,
    pub database_type: String,
    pub host: Option<String>,
    pub windows: bool,
    pub native: bool,
    pub http_port: u16,
    pub backup_folder: PathBuf,
}

pub fn here(record: &InstallationRecord) -> Here {
    let schema = Setup::current(record).ok().and_then(|s| crate::restore_switch::schema_version_now(&s.pg()).ok());
    Here {
        sentient_version: record.sentient_version.clone(),
        schema_version: schema,
        database_type: flavor_label(record.flavor()).to_string(),
        host: crate::backup_jobs::host_name(),
        windows: cfg!(windows),
        native: record.engine == Engine::Native,
        http_port: record.stack.http_port,
        backup_folder: crate::backups::BackupPrefs::load().folder,
    }
}

/// Names that look secret are never recorded.
fn secret_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    ["PASSWORD", "SECRET", "TOKEN", "_KEY", "PRIVATE", "CREDENTIAL"].iter().any(|w| n.contains(w)) && !n.ends_with("_PEM_KEY")
}

/// The platform's settings now (`manifest.platform`): never a secret.
pub fn platform_info(record: &InstallationRecord) -> PlatformInfo {
    let mut settings: BTreeMap<String, String> = crate::settings::current(record).into_iter().filter(|(k, _)| !secret_name(k)).collect();
    settings.insert("port:http".into(), record.stack.http_port.to_string());
    let tls = settings.get("SSL_PEM_CERT").map(|cert| tls_of(Path::new(cert)));
    let licence = crate::licence::read_tile(record).filter(|t| *t != crate::licence::TileLicence::None).map(|t| LicenceInfo { kind: t.label(), bound_to: crate::backup_jobs::host_name(), hardware_hash: None });
    let public_url = settings.get("SENTIENT_CORS_ALLOWED_ORIGINS").and_then(|o| o.split(',').next().map(|s| s.trim().to_string())).filter(|s| !s.is_empty() && s != "*");
    let file_store_paths = [("reports", "reports"), ("vc-repos", "vc-repos")].into_iter().map(|(id, dir)| (id.to_string(), record.state_dir.join(dir).display().to_string())).collect();
    PlatformInfo { settings, public_url, tls, licence, gateways: gateways(), file_store_paths }
}

/// The certificate's name and expiry, read with openssl where there is one.
fn tls_of(cert: &Path) -> TlsInfo {
    let name = cert.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let out = sentient_installer_core::host::host_command("openssl").args(["x509", "-noout", "-subject", "-issuer", "-enddate", "-in"]).arg(cert).output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else { return TlsInfo { name, issuer: None, expires: None } };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let field = |prefix: &str| text.lines().find_map(|l| l.strip_prefix(prefix)).map(|s| s.trim().to_string());
    let cn = |s: String| s.split(',').find_map(|p| p.trim().strip_prefix("CN").map(|v| v.trim_start_matches([' ', '=']).to_string())).unwrap_or(s);
    TlsInfo {
        name: field("subject=").map(cn).unwrap_or(name),
        issuer: field("issuer=").map(cn),
        expires: field("notAfter=").and_then(|d| chrono::NaiveDateTime::parse_from_str(d.trim_end_matches(" GMT"), "%b %e %H:%M:%S %Y").ok()).map(|d| d.and_utc().to_rfc3339()),
    }
}

/// The gateway on this machine and where it sends.
fn gateways() -> Vec<String> {
    let s = sentient_installer_core::gateway::status();
    if !s.installed {
        return Vec::new();
    }
    match (s.host, s.port) {
        (Some(h), Some(p)) => vec![format!("This server's gateway → {h}:{p}")],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 1. Inspect
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub path: String,
    pub label: String,
    pub bytes: u64,
    pub ok: bool,
}

/// The Check step's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inspection {
    pub file: PathBuf,
    pub name: String,
    pub file_bytes: u64,
    pub encrypted: bool,
    pub manifest: Manifest,
    pub parts: Vec<Part>,
    pub missing: Vec<String>,
    /// Each file of the backup against its `.sha256`, when there is one.
    pub sidecar: Option<Vec<(String, bool)>>,
    pub here: Here,
    /// The backup's database type, recorded or worked out.
    pub database_type: String,
    /// "14 Nov 2026, 5:12 PM"
    pub made: String,
    pub holds: Vec<String>,
    pub restored_bytes: u64,
    /// The same backup in the "Also copy to" folder (B5).
    pub other_copy: Option<PathBuf>,
}

impl Inspection {
    pub fn sound(&self) -> bool {
        self.missing.is_empty() && self.parts.iter().all(|p| p.ok) && self.sidecar.as_ref().is_none_or(|f| f.iter().all(|(_, ok)| *ok))
    }

    /// The parts that do not match, and those missing, by their labels.
    pub fn damaged(&self) -> Vec<(String, String)> {
        self.parts
            .iter()
            .filter(|p| !p.ok)
            .map(|p| (p.path.clone(), p.label.clone()))
            .chain(self.missing.iter().map(|m| (m.clone(), sentient_backup_core::restore::member_label(m))))
            .collect()
    }

    /// Only telemetry months are damaged: the restore can leave them out.
    pub fn damage_is_months(&self) -> bool {
        let d = self.damaged();
        !d.is_empty() && d.iter().all(|(p, _)| rs::member_month(p).is_some())
    }

    pub fn newer_schema(&self) -> bool {
        matches!((self.manifest.source.schema_version, self.here.schema_version), (Some(t), Some(o)) if t > o)
    }

    pub fn newer_format(&self) -> bool {
        self.manifest.format_version > FORMAT_VERSION
    }

    pub fn has_telemetry(&self) -> bool {
        self.manifest.files.iter().any(|f| rs::is_telemetry_member(&f.path) && f.path != format!("clickhouse/{}.native.zst", sentient_backup_core::clickhouse::LATEST_TABLE))
    }

    pub fn months(&self) -> Vec<(i32, u32, u64)> {
        rs::telemetry_months(&self.manifest)
    }

    /// The first and last day of telemetry in the backup (ms), by its months.
    pub fn telemetry_span(&self) -> Option<(i64, i64)> {
        use chrono::TimeZone;
        let m = self.months();
        let (fy, fm, _) = *m.first()?;
        let (ly, lm, _) = *m.last()?;
        let start = chrono::Utc.with_ymd_and_hms(fy, fm, 1, 0, 0, 0).single()?.timestamp_millis();
        let (ny, nm) = if lm == 12 { (ly + 1, 1) } else { (ly, lm + 1) };
        let end = chrono::Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).single()?.timestamp_millis().min(self.manifest.created_at.timestamp_millis());
        Some((start, end))
    }

    pub fn same_type(&self) -> bool {
        self.database_type == self.here.database_type
    }
}

/// Why a file could not be read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Unreadable {
    /// Encrypted: its passphrase is needed.
    Passphrase,
    WrongPassphrase,
    NotABackup(String),
    Other(String),
}

impl Unreadable {
    pub fn message(&self) -> String {
        match self {
            Unreadable::Passphrase => "This backup is encrypted: type its passphrase to read it.".into(),
            Unreadable::WrongPassphrase => "The passphrase is wrong, or the file is damaged.".into(),
            Unreadable::NotABackup(m) | Unreadable::Other(m) => m.clone(),
        }
    }

    fn of(e: &str, encrypted: bool) -> Unreadable {
        let l = e.to_lowercase();
        if l.contains("passphrase is needed") || l.contains("password is required") {
            Unreadable::Passphrase
        } else if encrypted && (l.contains("wrong passphrase") || l.contains("wrong password")) {
            Unreadable::WrongPassphrase
        } else if l.contains("not a sentient backup") || l.contains("no manifest") || l.contains("bad manifest") {
            Unreadable::NotABackup(e.to_string())
        } else {
            Unreadable::Other(e.to_string())
        }
    }
}

/// Read the backup through and check every part (blocking). Progress: one
/// `PART\t<label>\t<bytes>` log line per part, and the share read.
pub fn inspect(record: Option<&InstallationRecord>, file: &Path, passphrase: Option<&str>, sink: &ProgressFn) -> Result<Inspection, Unreadable> {
    let encrypted = sentient_backup_core::restore::is_encrypted(file).map_err(|e| Unreadable::of(&e.to_string(), false))?;
    if encrypted && passphrase.is_none() {
        return Err(Unreadable::Passphrase);
    }
    let manifest = sentient_backup_core::restore::inspect(file, passphrase).map_err(|e| Unreadable::of(&e.to_string(), encrypted))?;
    let file_bytes = sentient_backup_core::parts::total_size(file);
    let total: u64 = manifest.files.iter().map(|f| f.bytes).sum::<u64>().max(1);
    step(sink, "Checking the backup");
    let listed: Vec<(String, u64)> = manifest.files.iter().map(|f| (sentient_backup_core::restore::member_label(&f.path), f.bytes)).collect();
    sink(data_line("parts", &listed));
    log(sink, format!("PART\tFormat {}, made by Platform Manager {}\t0", manifest.format_version, manifest.tool_version));
    let mut read = 0u64;
    let report = sentient_backup_core::restore::check_with(file, passphrase, |path, bytes| {
        read += bytes;
        sink(Progress::Log { line: format!("PART\t{}\t{bytes}", sentient_backup_core::restore::member_label(path)) });
        sink(Progress::Percent { value: (read as f32 / total as f32).min(1.0) });
    })
    .map_err(|e| Unreadable::of(&e.to_string(), encrypted))?;
    let summary = crate::replace::from_manifest(file, file_bytes, encrypted, &manifest);
    let here = record.map(here).unwrap_or_default();
    let name = file.file_name().map(|n| n.to_string_lossy().trim_end_matches(".sentient-backup").to_string()).unwrap_or_default();
    let other_copy = crate::backups::BackupPrefs::load().copy_to.map(|d| d.join(file.file_name().unwrap_or_default())).filter(|p| p != file && p.exists());
    Ok(Inspection {
        file: file.to_path_buf(),
        name,
        file_bytes,
        encrypted,
        parts: report.members.iter().map(|m| Part { path: m.path.clone(), label: m.label.clone(), bytes: m.bytes, ok: m.ok }).collect(),
        missing: report.missing.clone(),
        sidecar: report.files.clone(),
        manifest: report.manifest,
        here,
        database_type: summary.database_type,
        made: summary.made,
        holds: summary.holds,
        restored_bytes: summary.restored_bytes,
        other_copy,
    })
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// What a run is asked to do: the flow's choices. The passphrase never goes
/// in it (standard input, or a file only the account reads, removed at once).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub file: PathBuf,
    pub scope: Scope,
    /// Configuration only: keep this instance's telemetry (true), or clear it.
    #[serde(default = "yes")]
    pub keep_telemetry: bool,
    /// Parts left out (a damaged month).
    #[serde(default)]
    pub skip_members: Vec<String>,
    /// A full backup of the current data first.
    #[serde(default)]
    pub safety_backup: bool,
    /// Where the safety backup goes (default: the backup folder).
    #[serde(default)]
    pub safety_dir: Option<PathBuf>,
    /// Where the backup is unpacked (default: beside the file).
    #[serde(default)]
    pub temp_dir: Option<PathBuf>,
    /// Delete the data from before once the restored data runs.
    #[serde(default)]
    pub delete_old: bool,
    /// Platform settings taken from the backup (by catalog name).
    #[serde(default)]
    pub use_from_backup: Vec<String>,
    /// Who runs it: "ops on OPS-LAPTOP".
    #[serde(default)]
    pub by: String,
    /// Where the file came from: "copied from Plant B, server to server".
    #[serde(default)]
    pub source: Option<String>,
    /// Go on after the last finished step of a failed run (X1).
    #[serde(default)]
    pub resume: bool,
    /// The safe level of free space (default 2 GB).
    #[serde(default)]
    pub min_free: Option<u64>,
}

fn yes() -> bool {
    true
}

impl Request {
    pub fn min_free(&self) -> u64 {
        self.min_free.unwrap_or(DEFAULT_MIN_FREE)
    }

    /// The safety backup is offered when data is deleted: the data from
    /// before (Delete once it runs) or the live telemetry a range replaces.
    pub fn safety_offered(&self) -> bool {
        self.delete_old || matches!(self.scope, Scope::Range { .. })
    }
}

// ---------------------------------------------------------------------------
// 2. Pre-check
// ---------------------------------------------------------------------------

/// One disk the restore writes to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskRow {
    /// "/", "/mnt/disk-a", "C:".
    pub disk: String,
    /// "/dev/sda1", "PostgreSQL", "ClickHouse (WSL)".
    pub detail: String,
    /// What needs the room: ("Restored database beside the live one", bytes).
    pub uses: Vec<(String, u64)>,
    pub need: u64,
    pub free: Option<u64>,
}

impl DiskRow {
    /// Room enough, and the safe level left.
    pub fn ok(&self, min_free: u64) -> bool {
        self.free.is_none_or(|f| f >= self.need.saturating_add(min_free))
    }

    pub fn short(&self, min_free: u64) -> u64 {
        self.free.map(|f| self.need.saturating_add(min_free).saturating_sub(f)).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    /// Always this instance's (its SENTIENT version).
    Fixed,
    /// Keep current or Use from backup; the default is the bool.
    Choice { backup_default: bool },
    /// The database type: Use from backup blocks (B3).
    DatabaseType,
    /// The licence: never carried over.
    Locked,
    /// Gateways: pointed here after.
    Gateway,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingRow {
    pub key: String,
    pub label: String,
    pub backup: String,
    pub here: String,
    pub kind: SettingKind,
    pub why: String,
}

/// The instance now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Live {
    pub devices: i64,
    pub dashboards: i64,
    pub users: i64,
    /// Telemetry history held now (PostgreSQL and ClickHouse).
    pub telemetry_bytes: u64,
    /// ClickHouse not answering, on a ClickHouse instance (B3).
    pub ch_problem: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Safety {
    pub name: String,
    pub folder: PathBuf,
    pub bytes: u64,
    pub secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Precheck {
    pub disks: Vec<DiskRow>,
    pub min_free: u64,
    pub estimate: Estimate,
    pub restored_bytes: u64,
    /// "Schema upgrade 30 → 31 (SENTIENT 4.3.0.58 → 4.4.0.2)".
    pub upgrade: Option<String>,
    pub settings: Vec<SettingRow>,
    /// Settings that are the same.
    pub same: usize,
    /// The backup recorded its platform settings (newer than 0.8.0).
    pub recorded: bool,
    pub live: Live,
    /// Configuration only: devices with telemetry the backup lacks (names, count).
    pub orphans: Option<(Vec<String>, usize)>,
    pub safety: Safety,
    /// The newest backup in the folder (B2s): "16 Nov, 02:00".
    pub newest_backup: Option<String>,
}

impl Precheck {
    pub fn blocked_disks(&self) -> Vec<&DiskRow> {
        self.disks.iter().filter(|d| !d.ok(self.min_free)).collect()
    }
}

/// Where a store keeps its data, to measure.
#[derive(Debug, Clone)]
enum Place {
    Host(PathBuf),
    Container(String, String),
}

/// (device, mount point, free bytes) of a place, from `df`.
fn measure(place: &Place) -> Option<(String, String, u64)> {
    #[cfg(windows)]
    {
        let path = match place {
            Place::Host(p) => p.clone(),
            Place::Container(_, p) => PathBuf::from(p),
        };
        let drive = path.to_string_lossy().chars().next().filter(|c| c.is_ascii_alphabetic()).unwrap_or('C').to_ascii_uppercase();
        let script = format!("(Get-PSDrive -Name '{drive}').Free");
        let mut c = std::process::Command::new("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        let free = String::from_utf8_lossy(&c.output().ok()?.stdout).trim().parse().ok()?;
        return Some((format!("{drive}:"), format!("{drive}:"), free));
    }
    #[cfg(not(windows))]
    {
        let out = match place {
            Place::Host(p) => {
                // A folder not made yet: its nearest existing parent.
                let mut p = p.clone();
                while !p.exists() {
                    if !p.pop() {
                        break;
                    }
                }
                sentient_installer_core::host::host_command("df").arg("-PB1").arg(&p).output().ok()?
            }
            Place::Container(c, p) => sentient_installer_core::host::host_command("docker").args(["exec", c, "df", "-PB1", p]).output().ok()?,
        };
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        parse_df_row(&text)
    }
}

/// The last row of `df -PB1`: (filesystem, mount point, available).
fn parse_df_row(text: &str) -> Option<(String, String, u64)> {
    let line = text.lines().filter(|l| !l.trim().is_empty()).last()?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 6 {
        return None;
    }
    Some((cols[0].to_string(), cols[5..].join(" "), cols[3].parse().ok()?))
}

/// The host's mount point of a device (a Docker volume's disk), from `df`.
#[cfg(not(windows))]
fn host_mounts() -> HashMap<String, String> {
    let out = sentient_installer_core::host::host_command("df").arg("-PB1").output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let mut m: HashMap<String, String> = HashMap::new();
    for line in out.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 6 {
            let mount = cols[5..].join(" ");
            let e = m.entry(cols[0].to_string()).or_insert_with(|| mount.clone());
            // The shortest mount point names the disk best.
            if mount.len() < e.len() {
                *e = mount;
            }
        }
    }
    m
}

/// The stores of an instance: (label, place).
fn store_places(record: &InstallationRecord, setup: &Setup) -> Vec<(&'static str, Place)> {
    let hybrid = setup.flavor() == Flavor::Clickhouse;
    let mut v = Vec::new();
    match record.engine {
        Engine::Docker => {
            let names = record.stack.names();
            v.push(("PostgreSQL", Place::Container(names.pg.clone(), "/var/lib/postgresql/data".into())));
            if hybrid {
                v.push(("ClickHouse", Place::Container(names.ch.clone(), "/var/lib/clickhouse".into())));
            }
        }
        Engine::Native => {
            let dir = if cfg!(windows) {
                record.pg_dir.join("data")
            } else {
                block_on(async {
                    let db = sentient_backup_core::db::DbInspector::connect(&setup.pg()).await.ok()?;
                    db.text("SELECT current_setting('data_directory')").await.ok().flatten()
                })
                .ok()
                .flatten()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/postgresql"))
            };
            v.push(("PostgreSQL", Place::Host(dir)));
            if hybrid {
                // Windows: ClickHouse's WSL disk lives in the account's profile on C:.
                v.push((if cfg!(windows) { "ClickHouse (WSL)" } else { "ClickHouse" }, Place::Host(if cfg!(windows) { PathBuf::from("C:\\Users") } else { PathBuf::from("/var/lib/clickhouse") })));
            }
        }
    }
    v
}

/// The disks of a restore and what each needs, grouped by device.
fn disk_rows(entries: Vec<(&str, Place, Vec<(String, u64)>)>) -> Vec<DiskRow> {
    #[cfg(not(windows))]
    let mounts = host_mounts();
    let mut rows: Vec<(String, DiskRow)> = Vec::new();
    for (role, place, uses) in entries {
        let Some((device, mount, free)) = measure(&place) else {
            rows.push((format!("?{role}"), DiskRow { disk: role.to_string(), detail: "free space not read".into(), uses: uses.clone(), need: uses.iter().map(|u| u.1).sum(), free: None }));
            continue;
        };
        #[cfg(not(windows))]
        let disk = match place {
            Place::Container(..) => mounts.get(&device).cloned().unwrap_or(device.clone()),
            Place::Host(_) => mount.clone(),
        };
        #[cfg(windows)]
        let disk = { let _ = &mount; device.clone() };
        let need: u64 = uses.iter().map(|u| u.1).sum();
        match rows.iter_mut().find(|(d, _)| *d == device) {
            Some((_, r)) => {
                r.uses.extend(uses);
                r.need += need;
                if !role.is_empty() && !r.detail.contains(role) {
                    r.detail = if r.detail.starts_with('/') || r.detail.is_empty() { role.to_string() } else { format!("{}, {role}", r.detail) };
                }
            }
            None => rows.push((device.clone(), DiskRow { disk, detail: if role.is_empty() { device.clone() } else { role.to_string() }, uses, need, free: Some(free) })),
        }
    }
    rows.into_iter().map(|(_, r)| r).collect()
}

/// The probe the free-space watch reads: every disk of the restore.
fn probe(places: Vec<(String, Place)>) -> impl Fn() -> Vec<DiskFree> + Send + Sync + 'static {
    // A container's df names the wrong mount point (busybox): the host's
    // mount point of the same device names the disk.
    #[cfg(not(windows))]
    let mounts = host_mounts();
    #[cfg(windows)]
    let mounts: HashMap<String, String> = HashMap::new();
    move || {
        places
            .iter()
            .filter_map(|(label, p)| {
                measure(p).map(|(dev, mount, free)| {
                    let disk = match p {
                        Place::Container(..) => mounts.get(&dev).cloned().unwrap_or(dev),
                        Place::Host(_) if !mount.is_empty() => mount,
                        Place::Host(_) => dev,
                    };
                    DiskFree { label: format!("{disk} ({label})"), free }
                })
            })
            .collect()
    }
}

/// Labels of the catalog settings, by name.
fn setting_label(key: &str) -> String {
    if let Some((_, l)) = crate::settings::PORTS.iter().find(|(k, _)| *k == key) {
        return format!("{l} port");
    }
    crate::catalog::groups().into_iter().flat_map(|g| g.settings).find(|s| s.env == key).map(|s| s.label).unwrap_or_else(|| key.to_string())
}

/// Keep current by default for what is tied to the machine; Use from backup
/// for how the plant runs. With the reason.
fn classify(key: &str) -> (bool, &'static str) {
    let k = key.to_ascii_uppercase();
    if k.starts_with("PORT:") || k.ends_with("_PORT") || k == "PORT" || k == "HOST" || k.ends_with("_HOST") {
        (false, "Tied to this machine and its firewall")
    } else if k.starts_with("PG:") {
        (false, "Sized for this machine's memory")
    } else if k.contains("SSL_PEM") {
        (false, "A certificate belongs to the host name")
    } else if k.contains("CORS") || k.contains("URL") {
        (false, "Devices and links reach this server by its own name")
    } else if k.starts_with("LICENSE") {
        (false, "Licensing belongs to this server")
    } else if k.contains("POOL") || k.contains("MAX_CONNECTIONS") || k.contains("BATCH") || k.starts_with("CACHE_") || k.contains("THREADS") || k.contains("WORKERS") || k.contains("CONSUMERS") || k == "PARTITION_COUNT" || k.contains("CACHE") {
        (false, "Sized for this machine")
    } else {
        (true, "A setting of the plant, not the machine")
    }
}

fn expiry_day(rfc: &Option<String>) -> String {
    rfc.as_deref().and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok()).map(|d| format!(", until {}", d.format("%-d %b %Y"))).unwrap_or_default()
}

/// The settings that differ between the backup and the instance (R14), and
/// how many are the same. `recorded`: the backup has `manifest.platform`.
pub fn settings_rows(m: &Manifest, backup_type: &str, here: &Here, now: &PlatformInfo) -> (Vec<SettingRow>, usize) {
    let mut rows = Vec::new();
    let mut same = 0usize;
    let row = |key: &str, label: &str, b: String, h: String, kind: SettingKind, why: &str| SettingRow { key: key.into(), label: label.into(), backup: b, here: h, kind, why: why.into() };
    let theirs = m.source.sentient_version.clone().unwrap_or_else(|| "Not recorded".into());
    if theirs != here.sentient_version {
        rows.push(row("sentient_version", "SENTIENT version", theirs, here.sentient_version.clone(), SettingKind::Fixed, "This server's; the data is upgraded"));
    } else {
        same += 1;
    }
    if backup_type != here.database_type {
        rows.push(row("database_type", "Database type", backup_type.into(), here.database_type.clone(), SettingKind::DatabaseType, "Converted to this server's type. To use the backup's, change the type in Migration first"));
    } else {
        same += 1;
    }
    let Some(p) = &m.platform else {
        // An older backup: the file store paths can still be compared.
        for fs in &m.file_stores {
            let mine = now.file_store_paths.get(&fs.id).cloned().unwrap_or_default();
            if fs.source_path != mine {
                rows.push(row(&format!("files:{}", fs.id), if fs.id == "reports" { "Report files" } else { "Version control files" }, fs.source_path.clone(), mine, SettingKind::Choice { backup_default: false }, "Paths of this machine"));
            }
        }
        return (rows, same);
    };
    // Only what the backup recorded: a setting it does not name has nothing to take.
    for key in p.settings.keys() {
        let (b, h) = (p.settings.get(key).cloned().unwrap_or_default(), now.settings.get(key).cloned().unwrap_or_default());
        if b == h {
            same += 1;
            continue;
        }
        let (backup_default, why) = classify(key);
        let shown = |v: String| if v.is_empty() { "Default".to_string() } else { v };
        rows.push(row(key, &setting_label(key), shown(b), shown(h), SettingKind::Choice { backup_default }, why));
    }
    if p.public_url != now.public_url {
        rows.push(row("public_url", "Public URL", p.public_url.clone().unwrap_or_else(|| "—".into()), now.public_url.clone().unwrap_or_else(|| "—".into()), SettingKind::Choice { backup_default: false }, "Devices and links reach this server by its own name"));
    } else {
        same += 1;
    }
    let tls = |t: &Option<TlsInfo>| t.as_ref().map(|t| format!("{}{}", t.name, expiry_day(&t.expires))).unwrap_or_else(|| "—".into());
    if p.tls != now.tls {
        rows.push(row("tls", "TLS certificate", tls(&p.tls), tls(&now.tls), SettingKind::Choice { backup_default: false }, "A certificate belongs to the host name"));
    } else {
        same += 1;
    }
    let lic = |l: &Option<LicenceInfo>, bound: bool| match l {
        Some(l) if bound => format!("{}, bound to {}", l.kind, l.bound_to.clone().unwrap_or_else(|| "its server".into())),
        Some(l) => format!("{}, bound to this server", l.kind),
        None => "None".into(),
    };
    if p.licence.is_some() || now.licence.is_some() {
        rows.push(row("licence", "Licence", lic(&p.licence, true), lic(&now.licence, false), SettingKind::Locked, "Bound to the hardware: not carried over"));
    }
    if !p.gateways.is_empty() && p.gateways != now.gateways {
        rows.push(row("gateways", "Gateway connections", p.gateways.join(", "), if now.gateways.is_empty() { "—".into() } else { now.gateways.join(", ") }, SettingKind::Gateway, "Gateways keep their own address; point them here after"));
    }
    for (id, path) in &p.file_store_paths {
        let mine = now.file_store_paths.get(id).cloned().unwrap_or_default();
        if *path != mine {
            rows.push(row(&format!("files:{id}"), if id == "reports" { "Report files" } else { "Version control files" }, path.clone(), mine, SettingKind::Choice { backup_default: false }, "Paths of this machine"));
        } else {
            same += 1;
        }
    }
    (rows, same)
}

/// Bytes of the backup's telemetry months in `scope` against all of them.
fn telemetry_share(m: &Manifest, scope: &Scope) -> f64 {
    let all: u64 = rs::telemetry_months(m).iter().map(|x| x.2).sum();
    if all == 0 {
        return 1.0;
    }
    let ins: u64 = m.files.iter().filter(|f| rs::member_month(&f.path).is_some() && rs::members_for(m, scope, &[]).contains(&f.path)).map(|f| f.bytes).sum();
    ins as f64 / all as f64
}

/// The pre-check (blocking). Reads the backup's manifest again (cheap).
pub fn precheck(record: &InstallationRecord, req: &Request, passphrase: Option<&str>, sink: &ProgressFn) -> Result<Precheck, String> {
    let m = sentient_backup_core::restore::inspect(&req.file, passphrase).map_err(|e| e.to_string())?;
    let setup = Setup::current(record)?;
    let here = here(record);
    let summary = crate::replace::from_manifest(&req.file, 0, false, &m);
    let status = crate::dbstatus::read(record);
    let hybrid = setup.flavor() == Flavor::Clickhouse;
    step(sink, "Measuring the disks");
    let file_bytes = rs::bytes_for(&m, &req.scope, &req.skip_members);
    let dump = m.files.iter().find(|f| f.path == sentient_backup_core::backup::DUMP_MEMBER).map(|f| f.bytes).unwrap_or(0);
    let unpack = file_bytes + dump.saturating_mul(3);
    let comp = |id: &str| m.components.iter().filter(|c| c.selected && c.id == id).map(|c| c.bytes.max(0) as u64).sum::<u64>();
    let config_bytes: u64 = m.components.iter().filter(|c| c.selected && c.id != "telemetry_historical").map(|c| c.bytes.max(0) as u64).sum();
    let ch_members: u64 = m.files.iter().filter(|f| f.path.starts_with("clickhouse/")).map(|f| f.bytes).sum();
    let share = telemetry_share(&m, &req.scope);
    let history = ((comp("telemetry_historical") + ch_members.saturating_mul(3)) as f64 * share) as u64;
    let live_pg_ts = status.pg.as_ref().and_then(|p| p.tables.iter().find(|t| t.name == "ts_kv").map(|t| t.bytes)).unwrap_or(0);
    let live_ch = status.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0);
    let mut pg_uses: Vec<(String, u64)> = Vec::new();
    let mut ch_uses: Vec<(String, u64)> = Vec::new();
    match &req.scope {
        Scope::Everything => {
            pg_uses.push(("Restored database beside the live one".into(), config_bytes + if hybrid { 0 } else { history }));
            if hybrid {
                ch_uses.push(("Restored history beside the live one".into(), history));
            }
        }
        Scope::Configuration => {
            pg_uses.push(("Restored database beside the live one".into(), config_bytes));
            if req.keep_telemetry && !hybrid && live_pg_ts > 0 {
                pg_uses.push(("This server's telemetry copied into it".into(), live_pg_ts));
            }
        }
        Scope::TelemetryOnly | Scope::Range { .. } => {
            pg_uses.push(("Telemetry staged, then added to the live data".into(), history.saturating_mul(if hybrid { 1 } else { 2 })));
            if hybrid {
                ch_uses.push(("Telemetry added to the live history".into(), history));
            }
        }
    }
    let prefs = crate::backups::BackupPrefs::load();
    let safety_folder = req.safety_dir.clone().unwrap_or_else(|| prefs.folder.clone());
    let pg_bytes = status.pg.as_ref().map(|p| p.db_bytes).unwrap_or(0);
    let (safety_bytes, safety_secs) = crate::backups::estimate(&crate::backups::BackupPrefs::default(), pg_bytes, live_ch, live_pg_ts);
    let temp_place = Place::Host(req.temp_dir.clone().or_else(|| req.file.parent().map(Path::to_path_buf)).unwrap_or_else(std::env::temp_dir));
    let mut entries: Vec<(&str, Place, Vec<(String, u64)>)> = vec![("", temp_place, vec![("Unpacking".into(), unpack)])];
    if req.safety_backup {
        entries.push(("", Place::Host(safety_folder.clone()), vec![("Safety backup".into(), safety_bytes)]));
    }
    for (role, place) in store_places(record, &setup) {
        let uses = if role.starts_with("ClickHouse") { std::mem::take(&mut ch_uses) } else { std::mem::take(&mut pg_uses) };
        entries.push((role, place, uses));
    }
    let disks = disk_rows(entries);

    step(sink, "Reading this instance");
    let live = live_facts(&setup, &status, hybrid);
    let orphans = if req.scope == Scope::Configuration && req.keep_telemetry {
        step(sink, "Finding telemetry the backup has no device for");
        block_on(rs::orphans(&setup.pg(), &req.file, passphrase, req.temp_dir.as_deref(), 3)).ok().and_then(|r| r.ok())
    } else {
        None
    };
    let restored = config_bytes + history;
    let carry = if req.scope == Scope::Configuration && req.keep_telemetry && !hybrid { live_pg_ts } else { 0 };
    let estimate = rs::estimate(file_bytes, restored, if req.safety_backup { safety_bytes } else { 0 }, carry, &req.scope, here.native);
    let upgrade = match (m.source.schema_version, here.schema_version) {
        (Some(t), Some(o)) if t < o && !req.scope.into_live() => Some(format!(
            "Schema upgrade {t} → {o} (SENTIENT {} → {})",
            m.source.sentient_version.clone().unwrap_or_else(|| "older".into()),
            here.sentient_version
        )),
        _ => None,
    };
    let (settings, same) = settings_rows(&m, &summary.database_type, &here, &platform_info(record));
    let newest_backup = crate::backups::list_all(&prefs).into_iter().find(|e| !e.name.starts_with("sentient-before-restore")).map(|e| crate::backups::short_made_label(&e.made));
    Ok(Precheck {
        disks,
        min_free: req.min_free(),
        estimate,
        restored_bytes: restored,
        upgrade,
        settings,
        same,
        recorded: m.platform.is_some(),
        live,
        orphans,
        safety: Safety { name: format!("sentient-before-restore-{}", chrono::Local::now().format("%Y-%m-%d_%H%M")), folder: safety_folder, bytes: safety_bytes, secs: safety_secs },
        newest_backup,
    })
}

fn live_facts(setup: &Setup, status: &crate::dbstatus::DbStatus, hybrid: bool) -> Live {
    let count = |t: &str| format!("(SELECT CASE WHEN to_regclass('public.{t}') IS NULL THEN 0 ELSE (SELECT count(*) FROM {t}) END)");
    let sql = format!("SELECT {} || ' ' || {} || ' ' || {}", count("device"), count("dashboard"), count("tb_user"));
    let counts: Vec<i64> = block_on(async {
        let db = sentient_backup_core::db::DbInspector::connect(&setup.pg()).await.ok()?;
        db.text(&sql).await.ok().flatten()
    })
    .ok()
    .flatten()
    .map(|t| t.split_whitespace().filter_map(|n| n.parse().ok()).collect())
    .unwrap_or_default();
    let pg_ts = status.pg.as_ref().and_then(|p| p.tables.iter().find(|t| t.name == "ts_kv").map(|t| t.bytes)).unwrap_or(0);
    Live {
        devices: counts.first().copied().unwrap_or(0),
        dashboards: counts.get(1).copied().unwrap_or(0),
        users: counts.get(2).copied().unwrap_or(0),
        telemetry_bytes: pg_ts + status.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0),
        ch_problem: if hybrid { status.ch_error.clone().map(|e| format!("ClickHouse did not answer: {e}")) } else { None },
    }
}

// ---------------------------------------------------------------------------
// 3. Run
// ---------------------------------------------------------------------------

/// The watch's word for a cancel.
const CANCELLED: &str = "the restore (cancelled)";

/// A cancel in plain words, for the messages that carry the watch's.
fn plain(e: String) -> String {
    if e.contains(CANCELLED) { "Cancelled.".into() } else { e }
}

pub const STEP_SAFETY: &str = "Backing up the current data";
pub const STEP_CHECK: &str = crate::replace::STEP_CHECK;
pub const STEP_RESTORE: &str = crate::replace::STEP_RESTORE;
pub const STEP_INTO_LIVE: &str = "Adding the backup's telemetry to the running data";
pub const STEP_KEEP: &str = "Carrying this server's telemetry into the restored data";
pub const STEP_SCHEMA: &str = "Bringing the data up to this SENTIENT's schema";
pub const STEP_SWITCH: &str = crate::replace::STEP_SWITCH;
pub const STEP_SETTINGS: &str = "Applying the settings taken from the backup";
pub const STEP_DELETE: &str = "Deleting the data from before";

/// The steps a run shows, in order.
pub fn planned_steps(req: &Request) -> Vec<String> {
    let mut v = Vec::new();
    if req.safety_backup {
        v.push(STEP_SAFETY);
    }
    v.push(STEP_CHECK);
    if req.scope.into_live() {
        v.push(STEP_INTO_LIVE);
    } else {
        v.push(STEP_RESTORE);
        if req.scope == Scope::Configuration && req.keep_telemetry {
            v.push(STEP_KEEP);
        }
        v.push(STEP_SCHEMA);
        v.push(STEP_SWITCH);
        if !req.use_from_backup.is_empty() {
            v.push(STEP_SETTINGS);
        }
        if req.delete_old {
            v.push(STEP_DELETE);
        }
    }
    v.into_iter().map(String::from).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Result_ {
    #[default]
    Done,
    /// Before the switch: nothing on the instance changed.
    Failed,
    /// SENTIENT did not answer on the restored data: the previous data is back.
    RolledBack,
    /// The free-space watch stopped it.
    Stopped,
}

impl Result_ {
    pub fn label(self) -> &'static str {
        match self {
            Result_::Done => "Done",
            Result_::Failed => "Failed",
            Result_::RolledBack => "Rolled back",
            Result_::Stopped => "Stopped",
        }
    }
}

/// One restore, as the history keeps it (`restore-history.jsonl`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// RFC 3339, the server's clock.
    pub at: String,
    pub backup: String,
    #[serde(default)]
    pub source: Option<String>,
    pub by: String,
    pub scope: String,
    pub result: Result_,
    pub message: String,
    pub took_secs: u64,
    #[serde(default)]
    pub down_secs: Option<u64>,
    #[serde(default)]
    pub safety: Option<String>,
    /// The data from before, kept under this name.
    #[serde(default)]
    pub aside: Option<String>,
    /// The restored database (left for a look after a rollback).
    #[serde(default)]
    pub restored_db: Option<String>,
    /// The step it stopped at (X1's resume).
    #[serde(default)]
    pub failed_step: Option<String>,
    #[serde(default)]
    pub converted: Option<String>,
    #[serde(default)]
    pub manager: String,
    /// The licence stays with this server (B7): open Licence after.
    #[serde(default)]
    pub licence_note: bool,
}

pub fn history_path() -> PathBuf {
    crate::installation::record_path().with_file_name("restore-history.jsonl")
}

pub fn logs_dir() -> PathBuf {
    crate::installation::record_path().with_file_name("restore-logs")
}

/// The history, newest first; entries older than a year are dropped.
pub fn history() -> Vec<Entry> {
    let text = std::fs::read_to_string(history_path()).unwrap_or_default();
    let mut v: Vec<Entry> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    v.reverse();
    v
}

fn append_history(e: &Entry) {
    let year_ago = (chrono::Utc::now() - chrono::Duration::days(365)).to_rfc3339();
    let mut all: Vec<Entry> = history().into_iter().rev().filter(|x| x.at >= year_ago).collect();
    all.push(e.clone());
    let text: String = all.iter().filter_map(|x| serde_json::to_string(x).ok()).map(|l| l + "\n").collect();
    if let Some(d) = history_path().parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(history_path(), text);
    // Logs of entries no longer listed go too.
    if let Ok(dir) = std::fs::read_dir(logs_dir()) {
        let keep: Vec<String> = all.iter().map(|x| format!("{}.log", x.id)).collect();
        for f in dir.flatten() {
            if !keep.contains(&f.file_name().to_string_lossy().to_string()) {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
}

/// A run's log, by its id.
pub fn log_text(id: &str) -> Result<String, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Not a restore id.".into());
    }
    std::fs::read_to_string(logs_dir().join(format!("{id}.log"))).map_err(|e| format!("The log of this restore is not there: {e}"))
}

/// What a resumed run can skip (X1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ResumeState {
    file: PathBuf,
    safety: Option<String>,
    checked: bool,
}

/// Asking a running restore to stop before the switch: this file.
pub fn cancel_path() -> PathBuf {
    crate::installation::record_path().with_file_name("restore-cancel")
}

pub fn request_cancel() -> Result<(), String> {
    std::fs::write(cancel_path(), b"").map_err(|e| e.to_string())
}

fn state_path() -> PathBuf {
    crate::installation::record_path().with_file_name("restore-state.json")
}

/// Who runs a restore from here: "ops on OPS-LAPTOP".
pub fn by_here() -> String {
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "someone".into());
    format!("{user} on {}", crate::backup_jobs::host_name().unwrap_or_else(|| "this computer".into()))
}

/// Drop a half-restored database (PostgreSQL and ClickHouse).
fn drop_restored(setup: &Setup, target: &str, sink: &ProgressFn) {
    let pg = setup.pg();
    let admin = sentient_backup_core::db::ConnConfig { dbname: "postgres".into(), ..pg };
    let r = block_on(async {
        let db = sentient_backup_core::db::DbInspector::connect(&admin).await.map_err(|e| e.to_string())?;
        db.batch(&format!("DROP DATABASE IF EXISTS \"{target}\" WITH (FORCE)")).await.map_err(|e| e.to_string())
    });
    match r {
        Ok(Ok(())) => log(sink, format!("The half-restored database {target} was removed.")),
        Ok(Err(e)) | Err(e) => log(sink, format!("The half-restored database {target} could not be removed ({e}); remove it under Kept after earlier changes.")),
    }
    if setup.flavor() == Flavor::Clickhouse {
        let _ = setup.ch().with_database("default").text(&format!("DROP DATABASE IF EXISTS `{target}`"));
    }
}

/// Remove a restored database a run left (X2's "Remove the restored
/// database…"): only a `<db>_restore_<stamp>` name, PostgreSQL and ClickHouse.
pub fn remove_restored(record: &InstallationRecord, name: &str, sink: &ProgressFn) -> Result<(), String> {
    let setup = Setup::current(record)?;
    let live = setup.pg().dbname;
    let ok = name.strip_prefix(&format!("{live}_restore_")).is_some_and(|s| (s.len() == 12 || s.len() == 14) && s.chars().all(|c| c.is_ascii_digit()));
    if !ok {
        return Err(format!("{name} is not a database a restore left."));
    }
    drop_restored(&setup, name, sink);
    let still = block_on(async {
        let admin = sentient_backup_core::db::ConnConfig { dbname: "postgres".into(), ..setup.pg() };
        let db = sentient_backup_core::db::DbInspector::connect(&admin).await.ok()?;
        db.text(&format!("SELECT count(*)::text FROM pg_database WHERE datname = '{name}'")).await.ok().flatten()
    })
    .ok()
    .flatten();
    if still.as_deref() == Some("0") { Ok(()) } else { Err(format!("{name} could not be removed.")) }
}

/// Run the restore (blocking). The history gets the run whatever happens.
pub fn run(record: InstallationRecord, req: Request, passphrase: Option<String>, sink: ProgressFn) -> Result<(), String> {
    let started = std::time::Instant::now();
    let id = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let tee: ProgressFn = {
        let (lines, sink) = (lines.clone(), sink.clone());
        Arc::new(move |p: Progress| {
            match &p {
                Progress::Step { name } => lines.lock().unwrap().push(format!(">> {name}")),
                Progress::Log { line } if !is_data(line) => lines.lock().unwrap().push(line.clone()),
                Progress::Error { message } => lines.lock().unwrap().push(message.clone()),
                _ => {}
            }
            sink(p)
        })
    };
    let mut entry = Entry {
        id: id.clone(),
        at: chrono::Utc::now().to_rfc3339(),
        backup: req.file.file_name().map(|n| n.to_string_lossy().trim_end_matches(".sentient-backup").to_string()).unwrap_or_default(),
        source: req.source.clone(),
        by: if req.by.is_empty() { by_here() } else { req.by.clone() },
        scope: match (&req.scope, req.keep_telemetry) {
            (Scope::Configuration, true) => "Configuration only, keeping this server's telemetry".into(),
            (Scope::Configuration, false) => "Configuration only, clearing this server's telemetry".into(),
            (s, _) => s.label(),
        },
        manager: env!("CARGO_PKG_VERSION").to_string(),
        ..Default::default()
    };
    let result = run_inner(&record, &req, passphrase, &tee, &mut entry);
    entry.took_secs = started.elapsed().as_secs();
    match &result {
        Ok(m) => {
            entry.result = Result_::Done;
            entry.message = m.clone();
            let _ = std::fs::remove_file(state_path());
        }
        Err(e) => entry.message = e.clone(),
    }
    let _ = std::fs::create_dir_all(logs_dir());
    let text: String = lines.lock().unwrap().iter().map(|l| format!("{l}\n")).collect();
    let _ = std::fs::write(logs_dir().join(format!("{id}.log")), text);
    append_history(&entry);
    sink(data_line("result", &entry));
    match result {
        Ok(m) => {
            sink(Progress::Done { message: m });
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn run_inner(record: &InstallationRecord, req: &Request, passphrase: Option<String>, sink: &ProgressFn, entry: &mut Entry) -> Result<String, String> {
    use crate::replace::nested;
    entry.result = Result_::Failed;
    let setup = Setup::current(record)?;
    let hybrid = setup.flavor() == Flavor::Clickhouse;
    let mut state: ResumeState = std::fs::read_to_string(state_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).filter(|s: &ResumeState| req.resume && s.file == req.file).unwrap_or_default();
    state.file = req.file.clone();
    let save_state = |s: &ResumeState| {
        let _ = std::fs::write(state_path(), serde_json::to_string(s).unwrap_or_default());
    };
    let fail_at = |entry: &mut Entry, step: &str| entry.failed_step = Some(step.to_string());

    // The safety backup.
    if req.safety_backup {
        step(sink, STEP_SAFETY);
        match state.safety.clone().filter(|p| Path::new(p).exists()) {
            Some(p) => log(sink, format!("The safety backup from the first attempt is used: {p}")),
            None => {
                let mut prefs = crate::backups::BackupPrefs::load();
                if let Some(d) = &req.safety_dir {
                    prefs.folder = d.clone();
                }
                prefs.preset = crate::backups::Preset::Everything;
                prefs.history = crate::backups::History::All;
                prefs.copy_to = None;
                let name = format!("sentient-before-restore-{}", chrono::Local::now().format("%Y-%m-%d_%H%M"));
                let made = crate::backups::run_as(record, &prefs, None, false, Some(&name), nested(sink)).inspect_err(|_| fail_at(entry, STEP_SAFETY))?;
                log(sink, format!("{} · {}, checked", made.path.display(), crate::dbstatus::bytes_label(made.bytes)));
                state.safety = Some(made.path.display().to_string());
                save_state(&state);
            }
        }
        entry.safety = state.safety.as_ref().and_then(|p| Path::new(p).file_stem().map(|n| n.to_string_lossy().to_string()));
    }

    // The file and its passphrase.
    step(sink, STEP_CHECK);
    let summary = crate::replace::summary(&req.file, passphrase.as_deref()).map_err(|e| match e {
        crate::replace::Unreadable::Passphrase(m) | crate::replace::Unreadable::Other(m) => m,
    });
    let summary = summary.inspect_err(|_| fail_at(entry, STEP_CHECK))?;
    if summary.database_type != flavor_label(record.flavor()) && req.scope.restores_telemetry() {
        entry.converted = Some(format!("history converted from {}", summary.database_type));
    }
    if !state.checked {
        let report = sentient_backup_core::restore::check(&req.file, passphrase.as_deref()).map_err(|e| e.to_string()).inspect_err(|_| fail_at(entry, STEP_CHECK))?;
        let bad: Vec<String> = report.members.iter().filter(|m| !m.ok && !req.skip_members.contains(&m.path)).map(|m| m.label.clone()).chain(report.missing.iter().filter(|p| !req.skip_members.contains(p)).map(|p| sentient_backup_core::restore::member_label(p))).collect();
        if !bad.is_empty() {
            fail_at(entry, STEP_CHECK);
            return Err(format!("The backup is damaged: {}. Nothing on this instance changed.", bad.join(", ")));
        }
        log(sink, format!("Every part matches its SHA-256 ({} parts).", report.members.len()));
        state.checked = true;
        save_state(&state);
    } else {
        log(sink, "The backup was checked by the first attempt.");
    }
    let live = setup.pg();
    let schema_ok = crate::restore_switch::schema_version_now(&live).ok();
    if let (Some(theirs), Some(ours)) = (summary.schema_version, schema_ok) {
        if theirs > ours {
            fail_at(entry, STEP_CHECK);
            return Err(format!("This backup is from a newer SENTIENT (schema {theirs}); this one has schema {ours}. Update SENTIENT first. Nothing on this instance changed."));
        }
    }

    // The watch, over every disk this run writes to.
    let mut places: Vec<(String, Place)> = store_places(record, &setup).into_iter().map(|(l, p)| (l.to_string(), p)).collect();
    places.push(("temporary files".into(), Place::Host(req.temp_dir.clone().or_else(|| req.file.parent().map(Path::to_path_buf)).unwrap_or_else(std::env::temp_dir))));
    let _ = std::fs::remove_file(cancel_path());
    let free_probe = probe(places);
    // Cancel rides on the watch: it stops pg_restore and the copies the same way.
    let mut watch = SpaceWatch::new(move || {
        if cancel_path().exists() {
            return vec![DiskFree { label: CANCELLED.into(), free: 0 }];
        }
        free_probe()
    });
    watch.min_free = req.min_free();
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("SPM_DEBUG_WATCH_EVERY_MS").ok().and_then(|s| s.parse().ok()) {
        watch.every = std::time::Duration::from_millis(secs);
    }
    let stopped = |e: &str, entry: &mut Entry| {
        if e.contains(CANCELLED) {
            entry.result = Result_::Failed;
        } else if e.starts_with("Stopped: ") {
            entry.result = Result_::Stopped;
        }
    };
    let ch_live = hybrid.then(|| setup.ch());

    if req.scope.into_live() {
        step(sink, STEP_INTO_LIVE);
        let opts = sentient_backup_core::restore::RestoreOptions {
            input: req.file.clone(),
            passphrase: passphrase.clone(),
            clickhouse: ch_live.clone(),
            telemetry_into: Some(crate::migration::telemetry_into(record)?),
            scope: req.scope.clone(),
            skip_members: req.skip_members.clone(),
            watch: Some(watch),
            temp_dir: req.temp_dir.clone(),
            allow_nonempty: true,
            ..Default::default()
        };
        let r = block_on(sentient_backup_core::restore::run(&live, &opts, crate::backup_jobs::bridge_pub(nested(sink))))?;
        return match r {
            Ok(_) => Ok(format!("The telemetry from {} is in {}; SENTIENT kept running.", entry.backup, flavor_label(record.flavor()))),
            Err(e) => {
                let e = e.to_string();
                stopped(&e, entry);
                fail_at(entry, STEP_INTO_LIVE);
                Err(format!("{} Nothing on this instance changed: the telemetry added so far was taken out again.", sentence(&plain(e))))
            }
        };
    }

    // Beside the live data.
    step(sink, STEP_RESTORE);
    // To the second: two restores in one minute must not meet (ClickHouse's
    // history from before stays under this name after the exchange).
    let target = format!("{}_restore_{}", live.dbname, chrono::Local::now().format("%Y%m%d%H%M%S"));
    let conn = sentient_backup_core::db::ConnConfig { dbname: target.clone(), ..live.clone() };
    let ch_target = ch_live.as_ref().map(|c| c.with_database(&target));
    let opts = sentient_backup_core::restore::RestoreOptions {
        input: req.file.clone(),
        file_store_paths: if req.scope.restores_configuration() {
            vec![("reports".into(), record.state_dir.join("reports")), ("vc-repos".into(), record.state_dir.join("vc-repos"))]
        } else {
            Vec::new()
        },
        passphrase: passphrase.clone(),
        clickhouse: ch_target.clone(),
        telemetry_into: Some(crate::migration::telemetry_into(record)?),
        scope: req.scope.clone(),
        skip_members: req.skip_members.clone(),
        watch: Some(watch.clone()),
        temp_dir: req.temp_dir.clone(),
        ..Default::default()
    };
    let restored = block_on(async {
        log(sink, format!(">> Creating the empty database {target}"));
        sentient_backup_core::db::create_database(&live, &target).await.map_err(|e| e.to_string())?;
        if let Some(ch) = &ch_target {
            log(sink, format!(">> Creating the ClickHouse database {target}"));
            #[cfg(target_os = "linux")]
            if let Setup::Native(c) = &setup {
                sentient_installer_core::native_linux::grant_clickhouse(c)?;
            }
            sentient_backup_core::clickhouse::clone_schema(ch, &setup.ch().database).map_err(|e| e.to_string())?;
        }
        sentient_backup_core::restore::run(&conn, &opts, crate::backup_jobs::bridge_pub(nested(sink))).await.map_err(|e| e.to_string())
    })
    .and_then(|r| r);
    if let Err(e) = restored {
        stopped(&e, entry);
        fail_at(entry, STEP_RESTORE);
        drop_restored(&setup, &target, sink);
        return Err(format!("{} Nothing on this instance changed: SENTIENT runs on its own data. The half-restored database was removed.", sentence(&plain(e))));
    }
    entry.restored_db = Some(target.clone());

    let keep = req.scope == Scope::Configuration && req.keep_telemetry;
    if keep {
        step(sink, STEP_KEEP);
        let guard = watch.start();
        let r = block_on(rs::carry_telemetry(&live, &conn, Some(&guard), &crate::backup_jobs::bridge_pub(nested(sink))));
        guard.stop();
        if let Err(e) = r.and_then(|r| r.map_err(|e| e.to_string())) {
            stopped(&e, entry);
            fail_at(entry, STEP_KEEP);
            drop_restored(&setup, &target, sink);
            return Err(format!("{} Nothing on this instance changed: SENTIENT runs on its own data. The half-restored database was removed.", sentence(&plain(e))));
        }
    }
    if cancel_path().exists() {
        fail_at(entry, STEP_SCHEMA);
        drop_restored(&setup, &target, sink);
        return Err("Cancelled before the switch. Nothing on this instance changed; the half-restored database was removed.".into());
    }

    step(sink, STEP_SCHEMA);
    if let Err(e) = crate::restore_switch::match_schema(record, &live, &setup.ch().password, &target, &nested(sink)).and_then(|_| crate::secrets_key::after_restore(&live, &target, &req.file, None, passphrase.as_deref(), &nested(sink))) {
        fail_at(entry, STEP_SCHEMA);
        drop_restored(&setup, &target, sink);
        return Err(format!("{} Nothing on this instance changed. The half-restored database was removed.", sentence(&e)));
    }

    step(sink, STEP_SWITCH);
    let down = std::time::Instant::now();
    let aside = match crate::restore_switch::run_with(record, &target, !keep, nested(sink)) {
        Ok(a) => a,
        Err(e) => {
            fail_at(entry, STEP_SWITCH);
            if e.contains("did not start on the restored data") {
                entry.result = Result_::RolledBack;
            }
            entry.down_secs = Some(down.elapsed().as_secs());
            return Err(e);
        }
    };
    entry.down_secs = Some(down.elapsed().as_secs());
    entry.aside = Some(aside.clone());
    if keep && hybrid {
        // The restored ClickHouse database stayed empty: nothing to keep.
        let _ = setup.ch().with_database("default").text(&format!("DROP DATABASE IF EXISTS `{target}`"));
    }
    entry.licence_note = sentient_backup_core::restore::inspect(&req.file, passphrase.as_deref()).ok().and_then(|m| m.platform).and_then(|p| p.licence).is_some();

    let mut notes = Vec::new();
    if !req.use_from_backup.is_empty() {
        step(sink, STEP_SETTINGS);
        let platform = sentient_backup_core::restore::inspect(&req.file, passphrase.as_deref()).ok().and_then(|m| m.platform).unwrap_or_default();
        let mut values: HashMap<String, String> = crate::settings::current(record);
        let mut taken = 0;
        for k in &req.use_from_backup {
            if let Some(v) = platform.settings.get(k) {
                values.insert(k.clone(), v.clone());
                taken += 1;
            }
        }
        if taken > 0 {
            match crate::settings::apply_with(record.clone(), &values, None, nested(sink)) {
                Ok(_) => notes.push(format!("{taken} setting{} taken from the backup.", if taken == 1 { "" } else { "s" })),
                Err(e) => notes.push(format!("The settings from the backup were not applied ({e}); this server's stay.")),
            }
        }
    }

    if req.delete_old {
        step(sink, STEP_DELETE);
        let mut freed = 0i64;
        for item in crate::kept::list(record)?.iter().filter(|k| k.name == aside || k.name == target) {
            crate::kept::remove(record, item, nested(sink))?;
            freed += item.bytes.unwrap_or(0);
        }
        // ClickHouse's history from before is in the restored database's
        // name after the exchange; the kept list does not show it.
        if hybrid && !keep {
            let _ = setup.ch().with_database("default").text(&format!("DROP DATABASE IF EXISTS `{target}`"));
        }
        notes.push(format!("The data from before was deleted ({} freed).", crate::dbstatus::bytes_label(freed.max(0) as u64)));
        entry.aside = None;
    }
    let kept = if req.delete_old {
        entry.safety.as_ref().map(|s| format!(" The safety backup {s} holds the data from before.")).unwrap_or_default()
    } else {
        format!(" The data from before is kept as {aside}{}.", entry.safety.as_ref().map(|s| format!(", and as the safety backup {s}")).unwrap_or_default())
    };
    Ok(format!("SENTIENT runs on the data from {}.{kept}{}", entry.backup, notes.iter().map(|n| format!(" {n}")).collect::<String>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_the_choices() {
        let mut r = Request { scope: Scope::Configuration, keep_telemetry: true, ..Default::default() };
        assert_eq!(planned_steps(&r), [STEP_CHECK, STEP_RESTORE, STEP_KEEP, STEP_SCHEMA, STEP_SWITCH]);
        r.safety_backup = true;
        r.delete_old = true;
        r.use_from_backup = vec!["RUST_LOG".into()];
        assert_eq!(planned_steps(&r).first().map(String::as_str), Some(STEP_SAFETY));
        assert_eq!(planned_steps(&r).last().map(String::as_str), Some(STEP_DELETE));
        let t = Request { scope: Scope::TelemetryOnly, ..Default::default() };
        assert_eq!(planned_steps(&t), [STEP_CHECK, STEP_INTO_LIVE]);
        assert!(!t.safety_offered() && r.safety_offered());
    }

    #[test]
    fn answers_travel_as_log_lines() {
        let e = Entry { id: "1".into(), backup: "b".into(), result: Result_::RolledBack, ..Default::default() };
        let Progress::Log { line } = data_line("result", &e) else { panic!() };
        assert!(is_data(&line));
        assert_eq!(read_data::<Entry>(&[line.as_str(), "other"], "result"), Some(e));
    }

    #[test]
    fn settings_say_what_differs_and_why() {
        let m: Manifest = serde_json::from_value(serde_json::json!({
            "format_version": 2, "tool_version": "t", "created_at": "2026-11-14T17:12:03Z",
            "source": {"database": "s", "postgres_version": "18", "timescaledb_version": "2.17", "sentient_version": "4.3.0.58", "schema_version": 30},
            "components": [], "telemetry": {"mode": "all"}, "files": [], "file_stores": [], "encryption": {"scheme": "none"},
            "platform": {"settings": {"port:mqtt": "1883", "AUDIT_LOG_TTL_DAYS": "365", "RUST_LOG": "info"}, "licence": {"kind": "Active", "bound_to": "PLANT-B"}}
        }))
        .unwrap();
        let here = Here { sentient_version: "4.4.0.2".into(), database_type: "PostgreSQL + ClickHouse".into(), ..Default::default() };
        let now = PlatformInfo { settings: [("port:mqtt", "21883"), ("AUDIT_LOG_TTL_DAYS", "90"), ("RUST_LOG", "info")].into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), ..Default::default() };
        let (rows, same) = settings_rows(&m, "PostgreSQL + TimescaleDB", &here, &now);
        let by = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
        assert_eq!(by("sentient_version").kind, SettingKind::Fixed);
        assert_eq!(by("database_type").kind, SettingKind::DatabaseType);
        assert_eq!(by("port:mqtt").kind, SettingKind::Choice { backup_default: false });
        assert_eq!(by("port:mqtt").label, "MQTT port");
        assert_eq!(by("AUDIT_LOG_TTL_DAYS").kind, SettingKind::Choice { backup_default: true });
        assert_eq!(by("licence").kind, SettingKind::Locked);
        assert!(same >= 1);
        assert!(!secret_name("SSL_PEM_KEY") && secret_name("SMTP_PASSWORD") && secret_name("API_TOKEN"));
    }

    #[test]
    fn disks_group_by_device_and_say_what_is_short() {
        assert_eq!(parse_df_row("Filesystem 1-blocks Used Available Capacity Mounted on\n/dev/sda1 100 50 3900000000 50% /\n"), Some(("/dev/sda1".into(), "/".into(), 3_900_000_000)));
        let row = DiskRow { disk: "/".into(), detail: "/dev/sda1".into(), uses: vec![], need: 8_800_000_000, free: Some(3_900_000_000) };
        assert!(!row.ok(2_000_000_000));
        assert_eq!(row.short(0), 4_900_000_000);
    }

    /// The watch on a real disk with less free than the safe level
    /// (SPM_SMALL_DISK=/boot/efi, say): it reads it with df and trips.
    #[test]
    #[ignore]
    fn the_watch_trips_on_a_real_small_disk() {
        let path = std::env::var("SPM_SMALL_DISK").expect("SPM_SMALL_DISK");
        let mut w = SpaceWatch::new(probe(vec![("temporary files".into(), Place::Host(PathBuf::from(&path)))]));
        w.every = std::time::Duration::from_millis(50);
        let g = w.start();
        std::thread::sleep(std::time::Duration::from_millis(800));
        let m = g.tripped().expect("tripped");
        eprintln!("{m}");
        assert!(m.starts_with(&format!("Stopped: {path} (temporary files) fell to")), "{m}");
        g.stop();
    }
}
