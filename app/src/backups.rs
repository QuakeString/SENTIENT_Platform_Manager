// Copyright © 2016-2026 The SENTIENT Authors

//! Backups the Platform Manager makes and keeps: what to include (presets),
//! where they go, the automatic schedule, the list of backups in the folder
//! with each one's last check, and one backup run from start to finish
//! (write, read back and compare every part, a `.sha256` beside the file,
//! a second copy, the oldest automatic ones removed).
//!
//! The settings live beside the installation record (`backup.json`), where
//! the scheduled task, which runs as the system, finds them too. Database
//! passwords are the ones the Platform Manager keeps; nothing is typed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sentient_backup_core::backup::{self, BackupOptions, FileStoreSpec, Selection};
use sentient_backup_core::progress as bk;
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use crate::installation::InstallationRecord;
use crate::migration::Setup;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// Every part, file stores too.
    #[default]
    Everything,
    /// Devices, dashboards, users and the rest of the configuration only.
    Configuration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "mode", content = "days")]
pub enum History {
    #[default]
    All,
    LastDays(u32),
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupPrefs {
    pub folder: PathBuf,
    #[serde(default)]
    pub preset: Preset,
    #[serde(default)]
    pub history: History,
    /// Split files larger than this many GB into parts (0: never split).
    #[serde(default = "four")]
    pub part_gb: u64,
    /// Back up every day at this time ("23:30"); None: no automatic backup.
    #[serde(default)]
    pub daily_at: Option<String>,
    /// Automatic backups kept (the oldest beyond go).
    #[serde(default = "seven")]
    pub keep: u32,
    /// A second copy of each backup here (another disk, a network folder).
    #[serde(default)]
    pub copy_to: Option<PathBuf>,
    /// Folders used before, whose backups were left there: still listed,
    /// restorable, and counted by Keep.
    #[serde(default)]
    pub old_folders: Vec<PathBuf>,
}

fn four() -> u64 {
    4
}

fn seven() -> u32 {
    7
}

pub fn default_folder() -> PathBuf {
    #[cfg(windows)]
    return PathBuf::from(r"C:\SENTIENT-backups");
    #[cfg(not(windows))]
    PathBuf::from("/var/backups/sentient")
}

impl Default for BackupPrefs {
    fn default() -> Self {
        BackupPrefs { folder: default_folder(), preset: Preset::Everything, history: History::All, part_gb: 4, daily_at: None, keep: 7, copy_to: None, old_folders: Vec::new() }
    }
}

fn prefs_path() -> PathBuf {
    crate::installation::record_path().with_file_name("backup.json")
}

impl BackupPrefs {
    pub fn load() -> BackupPrefs {
        std::fs::read_to_string(prefs_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::self_install::write_record(&prefs_path(), &json)
    }

    /// The parsed daily time, (hour, minute).
    pub fn time(&self) -> Option<(u32, u32)> {
        parse_time(self.daily_at.as_deref()?)
    }
}

pub fn parse_time(text: &str) -> Option<(u32, u32)> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

// ---------------------------------------------------------------------------
// Checks: the last result for each backup
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckRecord {
    /// RFC 3339.
    pub at: String,
    pub ok: bool,
    /// "All 14 parts match." or what does not.
    pub summary: String,
    /// Each part's result, by its archive path. Empty in checks made
    /// before parts were recorded.
    #[serde(default)]
    pub parts: Vec<(String, bool)>,
}

/// Where checks are remembered: beside the installation record when this
/// runs as the administrator (the server agent's jobs, which have no home
/// folder), else in the user's settings folder.
fn checks_path() -> PathBuf {
    if as_root() { shared_checks_path() } else { crate::updates::prefs_dir().join("backup-checks.json") }
}

fn as_root() -> bool {
    static ROOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ROOT.get_or_init(crate::rootjob::is_root)
}

fn shared_checks_path() -> PathBuf {
    crate::installation::record_path().with_file_name("backup-checks.json")
}

/// Both places, the one written by this account last.
fn load_checks() -> std::collections::HashMap<String, CheckRecord> {
    let read = |p: PathBuf| -> std::collections::HashMap<String, CheckRecord> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() };
    let mut all = read(shared_checks_path());
    all.extend(read(crate::updates::prefs_dir().join("backup-checks.json")).into_iter().filter(|_| !as_root()));
    all.extend(read(checks_path()));
    all
}

pub fn check_of(file: &Path) -> Option<CheckRecord> {
    load_checks().remove(&file.display().to_string())
}

fn store_check(file: &Path, rec: CheckRecord) {
    let mut all = load_checks();
    all.insert(file.display().to_string(), rec);
    if let Some(dir) = checks_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(checks_path(), json);
    }
}

/// Check a backup now and remember the result. Blocking.
pub fn check(file: &Path, passphrase: Option<&str>) -> Result<sentient_backup_core::restore::CheckReport, String> {
    // A file that cannot even be read through is damaged too: recorded as
    // a failed check (red in the list), not only as the job's error.
    let report = match sentient_backup_core::restore::check(file, passphrase) {
        Ok(r) => r,
        // Without the passphrase an encrypted backup cannot be read either:
        // that says nothing about the file.
        Err(e) if e.to_string().to_lowercase().contains("passphrase") => return Err(e.to_string()),
        Err(e) => {
            let summary = format!("Damaged: the file cannot be read through ({e}).");
            store_check(file, CheckRecord { at: chrono::Local::now().to_rfc3339(), ok: false, summary: summary.clone(), parts: Vec::new() });
            return Err(summary);
        }
    };
    let n = report.members.len() + report.missing.len();
    let summary = if report.ok() {
        format!("All {n} parts match.")
    } else {
        let bad: Vec<String> = report
            .members
            .iter()
            .filter(|m| !m.ok)
            .map(|m| m.label.clone())
            .chain(report.missing.iter().map(|p| format!("{} (missing)", sentient_backup_core::restore::member_label(p))))
            .chain(report.files.iter().flatten().filter(|(_, ok)| !ok).map(|(f, _)| format!("{f} (does not match its .sha256)")))
            .collect();
        format!("Damaged: {}.", bad.join(", "))
    };
    let parts = report
        .members
        .iter()
        .map(|m| (m.path.clone(), m.ok))
        .chain(report.missing.iter().map(|p| (p.clone(), false)))
        .collect();
    store_check(file, CheckRecord { at: chrono::Local::now().to_rfc3339(), ok: report.ok(), summary, parts });
    Ok(report)
}

// ---------------------------------------------------------------------------
// Details: what one backup holds (its manifest)
// ---------------------------------------------------------------------------

/// One part of a backup (an archive member), in plain words, and its size.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Content {
    /// The archive member (`db/telemetry/2026-09.copy.zst`).
    pub path: String,
    pub label: String,
    pub bytes: u64,
}

/// A part of the backup in the Details dialog's words: "Database (all but
/// telemetry)", "Telemetry 2026-09", "ClickHouse telemetry 2026-09",
/// "File store: Reports", "Manifest".
pub fn part_name(path: &str) -> String {
    use sentient_backup_core::backup::{DUMP_MEMBER, TELEMETRY_MEMBER, TELEMETRY_MONTH_PREFIX};
    // "2026-09" from ".../2026-09.copy.zst" or ".../202609.native.zst";
    // a month before 2000 is data that came without a real time (ts 0, a
    // device that sent none), named so rather than "1970-01".
    let month = |p: &str| -> Option<String> {
        let stem = p.rsplit('/').next()?.split('.').next()?.replace('-', "");
        if stem.len() < 6 || !stem[..6].bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if stem[..4].parse::<u32>().ok()? < 2000 {
            return Some("with no valid time".into());
        }
        Some(format!("{}-{}", &stem[..4], &stem[4..6]))
    };
    if path == "manifest.json" {
        "Manifest".into()
    } else if path == DUMP_MEMBER {
        "Database (all but telemetry)".into()
    } else if path == TELEMETRY_MEMBER {
        "Telemetry".into()
    } else if let Some(rest) = path.strip_prefix(TELEMETRY_MONTH_PREFIX) {
        month(rest).map(|m| format!("Telemetry {m}")).unwrap_or_else(|| "Telemetry".into())
    } else if let Some(rest) = path.strip_prefix("clickhouse/") {
        match rest.split_once('/') {
            Some((_, part)) => month(part).map(|m| format!("ClickHouse telemetry {m}")).unwrap_or_else(|| "ClickHouse telemetry".into()),
            None => "ClickHouse latest values".into(),
        }
    } else if let Some(store) = path.strip_prefix("files/") {
        match store.split('.').next().unwrap_or("") {
            "reports" => "File store: Reports".into(),
            "vc-repos" => "File store: Version control".into(),
            other => format!("File store: {other}"),
        }
    } else {
        path.to_string()
    }
}

/// One kind of data the backup selected or left out, as the manifest's
/// components record it (rows and size at backup time).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DataKind {
    pub name: String,
    pub included: bool,
    pub rows: i64,
    pub bytes: i64,
}

/// The platform as the manifest recorded it (never a secret).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PlatformFacts {
    #[serde(default)]
    pub address: Option<String>,
    /// "st-dev.local, expires 12 Mar 2027".
    #[serde(default)]
    pub certificate: Option<String>,
    /// "Professional, bound to st-dev".
    #[serde(default)]
    pub licence: Option<String>,
    #[serde(default)]
    pub gateways: Vec<String>,
}

/// What the Details dialog shows for a backup: the file, and what its
/// manifest records (nothing more). An encrypted backup's manifest needs
/// its passphrase: then only the file's own facts are known.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Details {
    pub name: String,
    pub folder: String,
    pub bytes: u64,
    /// Files it is split into (1: one file).
    pub files: usize,
    /// "2026-11-17 04:25", local.
    pub made: String,
    pub automatic: bool,
    pub encrypted: bool,
    /// Everything, Last N days, Configuration only, or the parts chosen.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub sentient_version: Option<String>,
    #[serde(default)]
    pub database_type: Option<String>,
    /// The computer it was made on.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub schema_version: Option<i64>,
    #[serde(default)]
    pub postgres_version: Option<String>,
    /// The Platform Manager that made it.
    #[serde(default)]
    pub made_by: Option<String>,
    #[serde(default)]
    pub data: Vec<DataKind>,
    /// Absent in backups made before it was recorded.
    #[serde(default)]
    pub platform: Option<PlatformFacts>,
    /// Its parts, in the archive's order. Empty when the manifest could not
    /// be read (`unreadable` says why).
    #[serde(default)]
    pub contents: Vec<Content>,
    #[serde(default)]
    pub unreadable: Option<String>,
    #[serde(default)]
    pub check: Option<CheckRecord>,
}

/// What was backed up, from the manifest: the telemetry window and whether
/// every part was taken.
pub fn scope_of(m: &sentient_backup_core::manifest::Manifest) -> String {
    use sentient_backup_core::manifest::TelemetrySelection;
    let all_parts = m.components.iter().all(|c| c.selected);
    match (&m.telemetry, all_parts) {
        (TelemetrySelection::None, _) => "Configuration only".into(),
        (TelemetrySelection::Range { days }, true) => format!("Last {days} days"),
        (TelemetrySelection::All, true) => "Everything".into(),
        // A choice of parts (Choose…): name them.
        _ => {
            let chosen: Vec<&str> = m.components.iter().filter(|c| c.selected).map(|c| c.name.as_str()).collect();
            format!("Chosen parts: {}", chosen.join(", "))
        }
    }
}

/// The Details of `e` (blocking: reads the manifest at the file's start).
pub fn details(e: &Entry) -> Details {
    let encrypted = sentient_backup_core::restore::is_encrypted(&e.path).unwrap_or(false);
    let mut d = Details {
        name: e.name.clone(),
        folder: e.path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
        bytes: e.bytes,
        files: e.parts,
        made: e.made.clone(),
        automatic: e.automatic,
        encrypted,
        scope: None,
        sentient_version: None,
        database_type: None,
        host: None,
        schema_version: None,
        postgres_version: None,
        made_by: None,
        data: Vec::new(),
        platform: None,
        contents: Vec::new(),
        unreadable: None,
        // Read now: a check just run from the dialog is newer than the list's.
        check: check_of(&e.path).or_else(|| e.check.clone()),
    };
    if encrypted {
        d.unreadable = Some("The backup is encrypted: what it holds is shown when it is restored with its passphrase.".into());
        return d;
    }
    match sentient_backup_core::restore::inspect(&e.path, None) {
        Ok(m) => {
            d.scope = Some(scope_of(&m));
            d.sentient_version = m.source.sentient_version.clone();
            d.database_type = m.source.database_type.clone();
            d.host = m.source.host.clone();
            d.schema_version = m.source.schema_version;
            d.postgres_version = Some(m.source.postgres_version.clone()).filter(|v| !v.is_empty());
            d.made_by = Some(m.tool_version.clone()).filter(|v| !v.is_empty());
            d.data = m.components.iter().map(|c| DataKind { name: c.name.clone(), included: c.selected, rows: c.rows, bytes: c.bytes }).collect();
            d.platform = m.platform.as_ref().map(|p| PlatformFacts {
                address: p.public_url.clone(),
                certificate: p.tls.as_ref().map(|t| match t.expires.as_deref().and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok()) {
                    Some(at) => format!("{}, expires {}", t.name, at.format("%-d %b %Y")),
                    None => t.name.clone(),
                }),
                licence: p.licence.as_ref().map(|l| match &l.bound_to {
                    Some(b) => format!("{}, bound to {b}", l.kind),
                    None => l.kind.clone(),
                }),
                gateways: p.gateways.clone(),
            });
            d.contents = m.files.iter().map(|f| Content { path: f.path.clone(), label: part_name(&f.path), bytes: f.bytes }).collect();
        }
        Err(err) => d.unreadable = Some(format!("The backup's list of contents cannot be read: {err}")),
    }
    d
}

/// The Details of the backup at `path` in its folder's list.
pub fn details_of(path: &Path) -> Result<Details, String> {
    let folder = path.parent().ok_or("Not a backup file.")?;
    let e = list(folder).into_iter().find(|e| e.path == path).ok_or_else(|| format!("{} is not a backup in {}.", path.display(), folder.display()))?;
    Ok(details(&e))
}

// ---------------------------------------------------------------------------
// The backups in the folder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// The file, or the base name of its parts.
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub parts: usize,
    /// When it was written (file time), local.
    pub made: String,
    pub automatic: bool,
    pub check: Option<CheckRecord>,
    /// In a folder used before (B5's "Old folder").
    #[serde(default)]
    pub old_folder: bool,
}

/// The backups in `folder`, newest first.
pub fn list(folder: &Path) -> Vec<Entry> {
    let Ok(dir) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut out: Vec<(std::time::SystemTime, Entry)> = Vec::new();
    for e in dir.flatten() {
        let p = e.path();
        let s = p.to_string_lossy().to_string();
        let base = if s.ends_with(".sentient-backup") {
            p.clone()
        } else if s.ends_with(".sentient-backup.001") {
            PathBuf::from(&s[..s.len() - 4])
        } else {
            continue;
        };
        let Ok((_, files)) = sentient_backup_core::parts::resolve(&base) else { continue };
        let modified = files.last().and_then(|f| std::fs::metadata(f).ok()).and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
        let made = chrono::DateTime::<chrono::Local>::from(modified).format("%Y-%m-%d %H:%M").to_string();
        let name = base.file_name().map(|n| n.to_string_lossy().trim_end_matches(".sentient-backup").to_string()).unwrap_or_default();
        out.push((
            modified,
            Entry {
                bytes: sentient_backup_core::parts::total_size(&base),
                parts: files.len(),
                made,
                automatic: name.starts_with("auto-"),
                check: check_of(&base),
                name,
                path: base,
                old_folder: false,
            },
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, e)| e).collect()
}

/// "16 Nov, 02:00" from the list's "2026-11-16 02:00".
pub fn short_made_label(made: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(made, "%Y-%m-%d %H:%M").map(|t| t.format("%-d %b, %H:%M").to_string()).unwrap_or_else(|_| made.to_string())
}

/// Every backup: the folder's and those left in folders used before,
/// newest first.
pub fn list_all(prefs: &BackupPrefs) -> Vec<Entry> {
    let mut all = list(&prefs.folder);
    for old in prefs.old_folders.iter().filter(|o| **o != prefs.folder) {
        all.extend(list(old).into_iter().map(|mut e| {
            e.old_folder = true;
            e
        }));
    }
    all.sort_by(|a, b| b.made.cmp(&a.made));
    all
}

/// Change the backup folder. `move_old`: the backups in the folder used
/// until now move with it (files, parts, checksums and their checks);
/// else they stay where they are, still listed and restorable.
pub fn set_folder(new: &Path, move_old: bool, sink: &ProgressFn) -> Result<String, String> {
    let mut prefs = BackupPrefs::load();
    let old = prefs.folder.clone();
    if old == new {
        return Ok("The backup folder is unchanged.".into());
    }
    std::fs::create_dir_all(new).map_err(|e| format!("{} cannot be made: {e}", new.display()))?;
    check_writable(new).map_err(|why| format!("{}: {why}", new.display()))?;
    let here = list(&old);
    if move_old {
        for e in &here {
            sink(Progress::Step { name: format!("Moving {}", e.name) });
            move_backup(&e.path, new)?;
        }
    } else if !here.is_empty() && !prefs.old_folders.contains(&old) {
        prefs.old_folders.push(old.clone());
    }
    prefs.old_folders.retain(|f| f != new);
    prefs.folder = new.to_path_buf();
    prefs.save()?;
    Ok(match (move_old, here.len()) {
        (_, 0) => format!("Backups now go to {}.", new.display()),
        (true, n) => format!("Backups now go to {}; {n} moved there.", new.display()),
        (false, n) => format!("Backups now go to {}; {n} stay in {}.", new.display(), old.display()),
    })
}

/// One backup's files (the file or its parts, its .sha256) into `folder`,
/// renamed where they can be, else copied and removed; its check moves too.
fn move_backup(base: &Path, folder: &Path) -> Result<(), String> {
    let (_, mut files) = sentient_backup_core::parts::resolve(base).map_err(|e| e.to_string())?;
    let sidecar = PathBuf::from(format!("{}.sha256", base.display()));
    if sidecar.is_file() {
        files.push(sidecar);
    }
    for f in &files {
        let to = folder.join(f.file_name().ok_or("no file name")?);
        if std::fs::rename(f, &to).is_err() {
            std::fs::copy(f, &to).map_err(|e| format!("{} could not be copied: {e}", f.display()))?;
            std::fs::remove_file(f).map_err(|e| format!("{} could not be removed after copying: {e}", f.display()))?;
        }
    }
    let new_base = folder.join(base.file_name().ok_or("no file name")?);
    if let Some(c) = check_of(base) {
        store_check(&new_base, c);
    }
    Ok(())
}

/// Whether the backup job can write in `dir` (a file is made and removed);
/// why not, in plain words.
pub fn check_writable(dir: &Path) -> Result<(), String> {
    probe_writable(dir).map_err(|(why, _)| why)
}

/// [`check_writable`], and whether giving the folder to the account would
/// fix it (permissions: yes; a read-only disk: no).
fn probe_writable(dir: &Path) -> Result<(), (String, bool)> {
    let probe = dir.join(format!(".spm-write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err((owner_why(dir), true)),
        Err(e) if e.raw_os_error() == Some(30) => Err(("the disk is read-only.".into(), false)),
        Err(e) => Err((e.to_string(), false)),
    }
}

fn owner_why(dir: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata(dir) {
            let who = if m.uid() == 0 { "root".to_string() } else { format!("user {}", m.uid()) };
            return format!("the folder belongs to {who}.");
        }
    }
    let _ = dir;
    "the folder does not allow it.".into()
}

/// Make `dir` writable for the account `uid` (or the one that asked for
/// administrator rights: pkexec's or sudo's caller): it becomes that
/// account's, folders above it untouched. Run as root.
pub fn make_writable(dir: &Path, uid: Option<u32>) -> Result<(), String> {
    #[cfg(unix)]
    {
        let uid = uid
            .or_else(|| std::env::var("PKEXEC_UID").ok().and_then(|u| u.parse().ok()))
            .or_else(|| std::env::var("SUDO_UID").ok().and_then(|u| u.parse().ok()));
        let Some(uid) = uid else {
            // Root itself asked: root writes everywhere already.
            return check_writable(dir);
        };
        let out = sentient_installer_core::host::host_command("chown").arg(uid.to_string()).arg(dir).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("{} could not be given to user {uid}: {}", dir.display(), String::from_utf8_lossy(&out.stderr).trim()));
        }
        let _ = sentient_installer_core::host::host_command("chmod").arg("u+rwx").arg(dir).status();
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        check_writable(dir)
    }
}

/// Free space on the disk that holds `p`.
pub fn free_space(p: &Path) -> Option<u64> {
    free_space_on(&disks(), p)
}

thread_local!(static DISK_LISTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

/// The mounted disks, listed afresh: every mount point, each asked for its
/// sizes (tens of milliseconds with many mounts; a slow network mount
/// holds it), so a listing lists them once for all its folders.
fn disks() -> sysinfo::Disks {
    DISK_LISTS.with(|n| n.set(n.get() + 1));
    sysinfo::Disks::new_with_refreshed_list()
}

/// The free space of the disk `p` is on, from a list of the disks.
fn free_space_on(disks: &sysinfo::Disks, p: &Path) -> Option<u64> {
    disks.list().iter().filter(|d| p.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len()).map(|d| d.available_space())
}

/// A folder as the chooser shows it (B2): its folders with their free space,
/// whether backups can be written there, and where to jump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirList {
    pub path: PathBuf,
    pub parent: Option<PathBuf>,
    /// Sub-folders: name and the free space where they are.
    pub dirs: Vec<(String, u64)>,
    /// Ok, or why not (B3).
    pub writable: Result<(), String>,
    /// Why not is a permission that "Make it writable…" fixes (not a
    /// read-only disk).
    #[serde(default)]
    pub fixable: bool,
    pub free: u64,
    /// Home, the current backup folder, and mounted disks: (label, path).
    pub shortcuts: Vec<(String, PathBuf)>,
}

pub fn dir_list(path: &Path) -> Result<DirList, String> {
    let rd = std::fs::read_dir(path).map_err(|e| format!("{} cannot be opened: {e}", path.display()))?;
    let disks = disks();
    let free = free_space_on(&disks, path).unwrap_or(0);
    let mut dirs: Vec<(String, u64)> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.'))
        .map(|n| {
            let f = free_space_on(&disks, &path.join(&n)).unwrap_or(free);
            (n, f)
        })
        .collect();
    dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    let prefs = BackupPrefs::load();
    let mut shortcuts: Vec<(String, PathBuf)> = Vec::new();
    if let Some(h) = home_dir() {
        shortcuts.push(("Home".into(), h));
    }
    shortcuts.push(("Current backup folder".into(), prefs.folder.clone()));
    for d in disks.list() {
        let m = d.mount_point().to_path_buf();
        let s = m.to_string_lossy().to_string();
        let usb = d.is_removable() || s.starts_with("/media/") || s.starts_with("/run/media/");
        let interesting = cfg!(windows) || usb || s.starts_with("/mnt/") || s.starts_with("/srv") || s.starts_with("/data");
        if interesting && !shortcuts.iter().any(|(_, p)| *p == m) {
            shortcuts.push((if usb { "USB disk".into() } else { s.clone() }, m));
        }
    }
    let (writable, fixable) = match probe_writable(path) {
        Ok(()) => (Ok(()), false),
        Err((why, fix)) => (Err(why), fix),
    };
    Ok(DirList { path: path.to_path_buf(), parent: path.parent().map(Path::to_path_buf), dirs, writable, fixable, free, shortcuts })
}

/// Home: the signed-in account's, also when the agent runs through sudo on
/// a server (sudo sets HOME to root's).
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(user) = std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty() && u != "root") {
        let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
        if let Some(home) = passwd.lines().map(|l| l.split(':').collect::<Vec<_>>()).find(|f| f.len() > 5 && f[0] == user).map(|f| PathBuf::from(f[5])) {
            return Some(home);
        }
    }
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Delete a backup: its file or parts and its `.sha256`.
pub fn delete(path: &Path) -> Result<(), String> {
    let (base, files) = sentient_backup_core::parts::resolve(path).map_err(|e| e.to_string())?;
    for f in files {
        std::fs::remove_file(&f).map_err(|e| format!("delete {}: {e}", f.display()))?;
    }
    let _ = std::fs::remove_file(sentient_backup_core::parts::sidecar_path(&base));
    Ok(())
}

/// Bring a backup from elsewhere into the folder (its parts and `.sha256` too).
pub fn add(path: &Path, folder: &Path) -> Result<PathBuf, String> {
    let (base, files) = sentient_backup_core::parts::resolve(path).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(folder).map_err(|e| format!("create {}: {e}", folder.display()))?;
    for f in files.iter().chain(std::iter::once(&sentient_backup_core::parts::sidecar_path(&base))) {
        if f.is_file() {
            let to = folder.join(f.file_name().unwrap_or_default());
            std::fs::copy(f, &to).map_err(|e| format!("copy {}: {e}", f.display()))?;
        }
    }
    Ok(folder.join(base.file_name().unwrap_or_default()))
}

// ---------------------------------------------------------------------------
// One backup
// ---------------------------------------------------------------------------

/// What one run produced.
#[derive(Debug, Clone)]
pub struct Made {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: Vec<(PathBuf, String, u64)>,
    pub sidecar: PathBuf,
    pub check: CheckRecord,
    pub members: usize,
    pub copied_to: Option<PathBuf>,
}

fn bridge(sink: ProgressFn) -> bk::ProgressFn {
    std::sync::Arc::new(move |p| {
        sink(match p {
            // The engine's steps: the job's plan names them
            // (crate::jobplan::backup).
            bk::Progress::Step { name, .. } => Progress::Step { name },
            bk::Progress::Log { line } => Progress::Log { line },
            bk::Progress::Percent { value } => Progress::Percent { value },
            bk::Progress::Done { message } => Progress::Log { line: message },
        })
    })
}

pub const STEP_WRITE: &str = "Writing the backup";
pub const STEP_CHECK: &str = "Reading the file back to check it";
pub const STEP_COPY: &str = "Copying it to the second location";
pub const STEP_PRUNE: &str = "Removing automatic backups beyond the ones kept";

/// Back up with `prefs` (and `passphrase` for a backup by hand). Blocking.
pub fn run(record: &InstallationRecord, prefs: &BackupPrefs, passphrase: Option<String>, automatic: bool, sink: ProgressFn) -> Result<Made, String> {
    run_as(record, prefs, passphrase, automatic, None, sink)
}

/// [`run`] into a file named `<name>.sentient-backup` (a restore's safety
/// backup, `sentient-before-restore-<stamp>`).
pub fn run_as(record: &InstallationRecord, prefs: &BackupPrefs, passphrase: Option<String>, automatic: bool, name: Option<&str>, sink: ProgressFn) -> Result<Made, String> {
    let setup = Setup::current(record)?;
    std::fs::create_dir_all(&prefs.folder).map_err(|e| format!("create {}: {e}", prefs.folder.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H%M");
    let output = match name {
        Some(n) => prefs.folder.join(format!("{n}.sentient-backup")),
        None => prefs.folder.join(format!("{}sentient-{stamp}.sentient-backup", if automatic { "auto-" } else { "" })),
    };
    let mut selection = match prefs.preset {
        Preset::Everything => Selection::full(),
        Preset::Configuration => Selection { include: ["configuration".to_string()].into_iter().collect(), telemetry_days: None },
    };
    match prefs.history {
        History::All => {
            selection.include.insert("telemetry_historical".into());
        }
        History::LastDays(d) => {
            selection.include.insert("telemetry_historical".into());
            selection.telemetry_days = Some(d);
        }
        History::None => {
            selection.include.remove("telemetry_historical");
        }
    }
    let file_stores = if prefs.preset == Preset::Everything {
        [("reports", "reports"), ("vc-repos", "version_control")]
            .into_iter()
            .map(|(id, cat)| FileStoreSpec { id: id.into(), category_id: cat.into(), path: record.state_dir.join(id) })
            .filter(|f| f.path.is_dir())
            .collect()
    } else {
        Vec::new()
    };
    let pg = setup.pg();
    let opts = BackupOptions {
        output: output.clone(),
        selection,
        file_stores,
        zstd_level: 10,
        passphrase: passphrase.clone(),
        clickhouse: (setup.flavor() == Flavor::Clickhouse && prefs.history != History::None).then(|| setup.ch()),
        origin: crate::backup_jobs::origin(record, &pg),
        part_size: (prefs.part_gb > 0).then(|| prefs.part_gb * 1_000_000_000),
    };
    sink(Progress::Step { name: STEP_WRITE.into() });
    let summary = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(backup::run(&pg, &opts, bridge(sink.clone())))
        .map_err(|e| e.to_string())?;
    crate::secrets_key::save_beside(record, &summary.output, opts.passphrase.as_deref(), &sink);
    crate::dbstatus::record_backup(&summary.output);

    sink(Progress::Step { name: STEP_CHECK.into() });
    let report = check(&summary.output, passphrase.as_deref())?;
    let rec = check_of(&summary.output).unwrap_or(CheckRecord { at: String::new(), ok: report.ok(), summary: String::new(), parts: Vec::new() });
    if !report.ok() {
        return Err(format!("The backup was written but does not read back correctly: {}", rec.summary));
    }

    let mut copied_to = None;
    if let Some(dest) = &prefs.copy_to {
        sink(Progress::Step { name: STEP_COPY.into() });
        match add(&summary.output, dest) {
            Ok(p) => copied_to = Some(p),
            Err(e) => sink(Progress::Log { line: format!("note: the second copy was not made: {e}") }),
        }
    }
    if automatic {
        sink(Progress::Step { name: STEP_PRUNE.into() });
        // Keep counts the backups in folders used before too.
        for old in list_all(prefs).into_iter().filter(|e| e.automatic).skip(prefs.keep.max(1) as usize) {
            let _ = delete(&old.path);
        }
    }
    sink(Progress::Done { message: format!("Backup finished and checked: {}", summary.output.display()) });
    Ok(Made {
        path: summary.output,
        bytes: summary.archive_bytes,
        files: summary.files,
        sidecar: summary.sidecar,
        check: rec,
        members: report.members.len(),
        copied_to,
    })
}

// ---------------------------------------------------------------------------
// The schedule
// ---------------------------------------------------------------------------

const TASK: &str = "SENTIENT backup";

/// Register (or remove) the daily backup: a scheduled task on Windows, a
/// systemd timer on Linux. It runs `<manager> --job scheduled-backup`.
pub fn apply_schedule(prefs: &BackupPrefs) -> Result<(), String> {
    let exe = crate::self_install::manager_exe();
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("schtasks.exe").args(["/Delete", "/TN", TASK, "/F"]).output();
        if let Some((h, m)) = prefs.time() {
            let tr = format!("\"{}\" --job scheduled-backup", exe.display());
            let st = format!("{h:02}:{m:02}");
            let out = std::process::Command::new("schtasks.exe")
                .args(["/Create", "/TN", TASK, "/SC", "DAILY", "/ST", &st, "/RU", "SYSTEM", "/RL", "HIGHEST", "/TR", &tr, "/F"])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!("The daily backup could not be scheduled: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let unit = "/etc/systemd/system/sentient-backup";
        match prefs.time() {
            Some((h, m)) => {
                let service = format!("[Unit]\nDescription={TASK}\n\n[Service]\nType=oneshot\nExecStart={} --job scheduled-backup\n", exe.display());
                let timer = format!("[Unit]\nDescription={TASK} (daily)\n\n[Timer]\nOnCalendar=*-*-* {h:02}:{m:02}:00\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n");
                std::fs::write(format!("{unit}.service"), service).map_err(|e| e.to_string())?;
                std::fs::write(format!("{unit}.timer"), timer).map_err(|e| e.to_string())?;
                let _ = sentient_installer_core::host::host_command("systemctl").arg("daemon-reload").status();
                let _ = sentient_installer_core::host::host_command("systemctl").args(["enable", "--now", "sentient-backup.timer"]).status();
            }
            None => {
                let _ = sentient_installer_core::host::host_command("systemctl").args(["disable", "--now", "sentient-backup.timer"]).status();
                let _ = std::fs::remove_file(format!("{unit}.service"));
                let _ = std::fs::remove_file(format!("{unit}.timer"));
                let _ = sentient_installer_core::host::host_command("systemctl").arg("daemon-reload").status();
            }
        }
        Ok(())
    }
}

/// The next automatic backup, as "today 23:30" / "tomorrow 23:30".
pub fn next_run(prefs: &BackupPrefs) -> Option<String> {
    let (h, m) = prefs.time()?;
    let now = chrono::Local::now();
    let today = now.date_naive().and_hms_opt(h, m, 0)?;
    Some(if today > now.naive_local() { format!("today {h:02}:{m:02}") } else { format!("tomorrow {h:02}:{m:02}") })
}

/// About how big a backup with `prefs` gets and how long it takes:
/// (bytes, seconds), from the database's own sizes.
pub fn estimate(prefs: &BackupPrefs, pg_bytes: u64, ch_bytes: u64, history_bytes: u64) -> (u64, u64) {
    let config = pg_bytes.saturating_sub(history_bytes);
    let mut raw = match prefs.preset {
        Preset::Everything => config,
        Preset::Configuration => config / 3,
    };
    let history = history_bytes + ch_bytes;
    raw += match prefs.history {
        History::All => history,
        History::LastDays(d) => history.saturating_mul(d as u64) / 365,
        History::None => 0,
    };
    // zstd on database dumps: about a third; ClickHouse parts are already compressed.
    let bytes = raw / 3 + ch_bytes.min(raw) / 3;
    (bytes, (raw / 40_000_000).max(10))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder's listing (the folder chooser, here and over SSH) lists the
    /// disks once, not once per folder in it: with 81 mounts a listing took
    /// one enumeration per folder (perf audit, docs/analysis/SPM_PERFORMANCE_AUDIT.md).
    #[test]
    fn a_folder_listing_lists_the_disks_once() {
        let dir = std::env::temp_dir().join(format!("spm-dirlist-{}", std::process::id()));
        for i in 0..40 {
            std::fs::create_dir_all(dir.join(format!("f{i:02}"))).unwrap();
        }
        let t = std::time::Instant::now();
        let one = disks();
        let one_ms = t.elapsed().as_secs_f64() * 1000.;
        drop(one);
        DISK_LISTS.with(|n| n.set(0));
        let t = std::time::Instant::now();
        let l = dir_list(&dir).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1000.;
        let lists = DISK_LISTS.with(|n| n.get());
        eprintln!("dir_list of 40 folders: {ms:.1} ms, {lists} disk list(s), one list {one_ms:.1} ms");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(l.dirs.len(), 40);
        assert_eq!(lists, 1, "the disks are listed once per folder listing");
    }

    #[test]
    fn times_parse_and_old_settings_load() {
        assert_eq!(parse_time("23:30"), Some((23, 30)));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("x"), None);
        let p: BackupPrefs = serde_json::from_str(r#"{"folder":"D:/b"}"#).unwrap();
        assert_eq!((p.part_gb, p.keep, p.preset, p.history), (4, 7, Preset::Everything, History::All));
        let p: BackupPrefs = serde_json::from_str(r#"{"folder":"D:/b","history":{"mode":"last_days","days":90}}"#).unwrap();
        assert_eq!(p.history, History::LastDays(90));
    }

    #[test]
    fn the_estimate_follows_the_choices() {
        let mut p = BackupPrefs::default();
        let all = estimate(&p, 3 << 30, 0, 2 << 30).0;
        p.history = History::None;
        let none = estimate(&p, 3 << 30, 0, 2 << 30).0;
        assert!(all > none && none > 0);
    }
}

#[cfg(test)]
mod details_tests {
    use super::part_name;

    #[test]
    fn parts_have_plain_names() {
        assert_eq!(part_name("manifest.json"), "Manifest");
        assert_eq!(part_name("db/dump.pgc.zst"), "Database (all but telemetry)");
        assert_eq!(part_name("db/telemetry.copy.zst"), "Telemetry");
        assert_eq!(part_name("db/telemetry/2026-09.copy.zst"), "Telemetry 2026-09");
        assert_eq!(part_name("clickhouse/ts_kv/202609.native.zst"), "ClickHouse telemetry 2026-09");
        assert_eq!(part_name("clickhouse/ts_kv_latest.native.zst"), "ClickHouse latest values");
        assert_eq!(part_name("files/reports.tar.zst"), "File store: Reports");
        assert_eq!(part_name("files/vc-repos.tar.zst"), "File store: Version control");
        assert_eq!(part_name("files/extra.tar.zst"), "File store: extra");
        assert_eq!(part_name("clickhouse/ts_kv/197001.native.zst"), "ClickHouse telemetry with no valid time");
        assert_eq!(part_name("db/telemetry/1970-01.copy.zst"), "Telemetry with no valid time");
    }
}

/// Screens: canned Details (SPM_DEBUG_BACKUP_DETAILS=new|old|part|long|encrypted).
#[cfg(debug_assertions)]
pub fn canned_details(kind: &str) -> Details {
    let content = |path: &str, mb: u64| Content { path: path.into(), label: part_name(path), bytes: mb * 1_000_000 };
    let kindrow = |name: &str, included: bool, rows: i64, mb: i64| DataKind { name: name.into(), included, rows, bytes: mb * 1_000_000 };
    let check = |at: &str, parts: &[Content], bad: &[&str]| CheckRecord {
        at: at.into(),
        ok: bad.is_empty(),
        summary: if bad.is_empty() { format!("All {} parts match.", parts.len()) } else { "Damaged.".into() },
        parts: parts.iter().map(|c| (c.path.clone(), !bad.contains(&c.path.as_str()))).collect(),
    };
    let mut d = Details {
        name: "sentient-2026-11-17_0425".into(),
        folder: "/var/backups/sentient".into(),
        bytes: 536_000_000,
        files: 1,
        made: "2026-11-17 04:25".into(),
        automatic: false,
        encrypted: false,
        scope: Some("Everything".into()),
        sentient_version: Some("4.4.1.0".into()),
        database_type: Some("PostgreSQL + ClickHouse".into()),
        host: Some("st-dev".into()),
        schema_version: Some(37),
        postgres_version: Some("16.4".into()),
        made_by: Some("0.9.5".into()),
        data: vec![
            kindrow("Configuration (entities, dashboards, users, …)", true, 48_210, 38),
            kindrow("Telemetry (historical)", true, 12_904_331, 412),
            kindrow("Telemetry (latest)", true, 6_402, 2),
            kindrow("Attributes", true, 21_877, 6),
            kindrow("Alarms", true, 3_114, 4),
            kindrow("Audit & event logs", true, 188_420, 52),
            kindrow("Reports", true, 0, 9),
            kindrow("Version control", true, 0, 3),
        ],
        platform: Some(PlatformFacts {
            address: Some("https://st-dev.local".into()),
            certificate: Some("st-dev.local, expires 12 Mar 2027".into()),
            licence: Some("Professional, bound to st-dev".into()),
            gateways: vec!["st-gateway → 11.11.0.3:1883".into()],
        }),
        contents: Vec::new(),
        unreadable: None,
        check: None,
    };
    let months = |from: u32, n: u32, start_year: i32| -> Vec<Content> {
        (0..n).map(|i| {
            let m0 = from - 1 + i;
            let (y, m) = (start_year + (m0 / 12) as i32, m0 % 12 + 1);
            content(&format!("db/telemetry/{y}-{m:02}.copy.zst"), 40 + (i as u64 * 7) % 80)
        }).collect()
    };
    match kind {
        "old" => {
            d.name = "sentient-2026-10-02_2210".into();
            d.made = "2026-10-02 22:10".into();
            d.automatic = true;
            d.bytes = 488_000_000;
            d.host = None;
            d.sentient_version = None;
            d.schema_version = None;
            d.database_type = None;
            d.postgres_version = Some("16.2".into());
            d.made_by = Some("0.8.0".into());
            d.platform = None;
            d.data.truncate(6);
            d.contents = std::iter::once(content("db/dump.pgc.zst", 92)).chain(months(7, 4, 2026)).collect();
            d.check = Some(check("2026-10-02T22:11:00+05:30", &d.contents, &[]));
        }
        "part" => {
            d.name = "sentient-2026-11-20_0200".into();
            d.made = "2026-11-20 02:00".into();
            d.automatic = true;
            d.bytes = 301_000_000;
            d.folder = r"D:\SENTIENT\backups".into();
            d.scope = Some("Last 90 days".into());
            d.host = Some("saf-server".into());
            d.sentient_version = Some("4.4.1.2".into());
            d.database_type = Some("PostgreSQL + TimescaleDB".into());
            d.data[5].included = false;
            d.contents = std::iter::once(content("db/dump.pgc.zst", 112)).chain(months(8, 4, 2026)).chain(std::iter::once(content("files/reports.tar.zst", 12))).collect();
            d.check = Some(check("2026-11-21T02:05:00+05:30", &d.contents, &["db/telemetry/2026-10.copy.zst"]));
        }
        "encrypted" => {
            d.encrypted = true;
            d.scope = None;
            d.data.clear();
            d.platform = None;
            d.unreadable = Some("The backup is encrypted: what it holds is shown when it is restored with its passphrase.".into());
        }
        // "new", "long" (every month since 2023, and ClickHouse rows with
        // no valid time), "noparts" (a check from before per-part results).
        _ => {
            let tele = if kind == "long" { months(1, 36, 2023) } else { months(8, 4, 2026) };
            let no_time: Vec<Content> = if kind == "long" { vec![Content { path: "clickhouse/ts_kv/197001.native.zst".into(), label: part_name("clickhouse/ts_kv/197001.native.zst"), bytes: 271 }] } else { Vec::new() };
            d.contents = std::iter::once(content("db/dump.pgc.zst", 104))
                .chain(tele)
                .chain(no_time)
                .chain([content("clickhouse/ts_kv/202611.native.zst", 71), content("clickhouse/ts_kv_latest.native.zst", 1), content("files/reports.tar.zst", 9), content("files/vc-repos.tar.zst", 3)])
                .collect();
            d.check = Some(check("2026-11-17T04:25:00+05:30", &d.contents, &[]));
            if kind == "noparts" {
                if let Some(c) = d.check.as_mut() {
                    c.parts.clear();
                }
            }
        }
    }
    d
}
