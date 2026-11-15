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
        BackupPrefs { folder: default_folder(), preset: Preset::Everything, history: History::All, part_gb: 4, daily_at: None, keep: 7, copy_to: None }
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
}

fn checks_path() -> PathBuf {
    crate::updates::prefs_dir().join("backup-checks.json")
}

fn load_checks() -> std::collections::HashMap<String, CheckRecord> {
    std::fs::read_to_string(checks_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
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
    let report = sentient_backup_core::restore::check(file, passphrase).map_err(|e| e.to_string())?;
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
    store_check(file, CheckRecord { at: chrono::Local::now().to_rfc3339(), ok: report.ok(), summary });
    Ok(report)
}

// ---------------------------------------------------------------------------
// The backups in the folder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
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
            },
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, e)| e).collect()
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
            bk::Progress::Step { name, .. } => Progress::Log { line: format!(">> {name}") },
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
    let setup = Setup::current(record)?;
    std::fs::create_dir_all(&prefs.folder).map_err(|e| format!("create {}: {e}", prefs.folder.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H%M");
    let output = prefs.folder.join(format!("{}sentient-{stamp}.sentient-backup", if automatic { "auto-" } else { "" }));
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
    crate::secrets_key::save_beside(record, &summary.output, &sink);
    crate::dbstatus::record_backup(&summary.output);

    sink(Progress::Step { name: STEP_CHECK.into() });
    let report = check(&summary.output, passphrase.as_deref())?;
    let rec = check_of(&summary.output).unwrap_or(CheckRecord { at: String::new(), ok: report.ok(), summary: String::new() });
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
        for old in list(&prefs.folder).into_iter().filter(|e| e.automatic).skip(prefs.keep.max(1) as usize) {
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

/// The steps a run shows.
pub fn planned_steps(prefs: &BackupPrefs, automatic: bool) -> Vec<String> {
    let mut v = vec![STEP_WRITE.to_string(), STEP_CHECK.to_string()];
    if prefs.copy_to.is_some() {
        v.push(STEP_COPY.into());
    }
    if automatic {
        v.push(STEP_PRUNE.into());
    }
    v
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
                let _ = std::process::Command::new("systemctl").arg("daemon-reload").status();
                let _ = std::process::Command::new("systemctl").args(["enable", "--now", "sentient-backup.timer"]).status();
            }
            None => {
                let _ = std::process::Command::new("systemctl").args(["disable", "--now", "sentient-backup.timer"]).status();
                let _ = std::fs::remove_file(format!("{unit}.service"));
                let _ = std::fs::remove_file(format!("{unit}.timer"));
                let _ = std::process::Command::new("systemctl").arg("daemon-reload").status();
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
