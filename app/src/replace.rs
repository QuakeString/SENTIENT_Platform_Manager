// Copyright © 2016-2026 The SENTIENT Authors

//! Replacing this instance's data with a backup, possibly from another
//! instance with another database setup. This installation keeps its own
//! PostgreSQL (and ClickHouse); only what is in them is replaced.
//!
//! 1. [`summary`]: what the file says about itself, before anything runs.
//! 2. [`check`]: the backup against this installation (database type,
//!    SENTIENT version, free disk) and the reasons it cannot be used, each
//!    with what solves it.
//! 3. [`run`]: one run. The backup is restored beside the live data (and
//!    converted to this installation's type, and brought up to its schema),
//!    then SENTIENT switches to it (restore_switch, which puts the previous
//!    data back when SENTIENT does not start), then, when asked, the
//!    previous data is deleted. It is deleted only after SENTIENT answered.

use std::path::{Path, PathBuf};

use sentient_backup_core::manifest::{Manifest, TelemetrySelection};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{InstallationRecord, flavor_label};

/// What a backup file says about itself.
#[derive(Debug, Clone)]
#[allow(dead_code)] // the whole summary travels; `--job replace` reads part of it
pub struct Summary {
    pub file: PathBuf,
    pub file_bytes: u64,
    /// Local time it was made: `3 Nov 2026, 11:30 PM`.
    pub made: String,
    pub host: Option<String>,
    pub sentient_version: Option<String>,
    pub schema_version: Option<i64>,
    /// Recorded, or worked out from what the file holds.
    pub database_type: String,
    /// About how much room it needs once restored.
    pub restored_bytes: u64,
    /// "platform data, telemetry history (all), report files"
    pub holds: Vec<String>,
    pub encrypted: bool,
}

/// Why a summary could not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum Unreadable {
    /// Encrypted: ask for the passphrase (or it was wrong).
    Passphrase(String),
    Other(String),
}

pub fn summary(file: &Path, passphrase: Option<&str>) -> Result<Summary, Unreadable> {
    let encrypted = sentient_backup_core::restore::is_encrypted(file).map_err(|e| Unreadable::Other(e.to_string()))?;
    let manifest = match sentient_backup_core::restore::inspect(file, passphrase) {
        Ok(m) => m,
        Err(e) if encrypted => return Err(Unreadable::Passphrase(e.to_string())),
        Err(e) => return Err(Unreadable::Other(e.to_string())),
    };
    let file_bytes = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    Ok(from_manifest(file, file_bytes, encrypted, &manifest))
}

pub(crate) fn from_manifest(file: &Path, file_bytes: u64, encrypted: bool, m: &Manifest) -> Summary {
    let database_type = m.source.database_type.clone().unwrap_or_else(|| {
        if m.clickhouse.is_some() {
            n!("PostgreSQL + ClickHouse").into()
        } else if m.source.timescaledb_version.is_some() {
            n!("PostgreSQL + TimescaleDB").into()
        } else {
            "PostgreSQL".into()
        }
    });
    let component_bytes: u64 = m.components.iter().filter(|c| c.selected).map(|c| c.bytes.max(0) as u64).sum();
    let restored_bytes = component_bytes.max(file_bytes.saturating_mul(3));
    let mut holds = vec!["platform data".to_string()];
    match m.telemetry {
        TelemetrySelection::None => {}
        TelemetrySelection::All => holds.push("telemetry history (all)".into()),
        TelemetrySelection::Range { days } => holds.push(nf!("telemetry history (last {days} days)")),
    }
    if !m.file_stores.is_empty() {
        holds.push("report files and version control".into());
    }
    Summary {
        file: file.to_path_buf(),
        file_bytes,
        made: m.created_at.with_timezone(&chrono::Local).format("%-d %b %Y, %-I:%M %p").to_string(),
        host: m.source.host.clone(),
        sentient_version: m.source.sentient_version.clone(),
        schema_version: m.source.schema_version,
        database_type,
        restored_bytes,
        holds,
        encrypted,
    }
}

/// The backup against this installation.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // `--job replace` reads the refusal; the lines are for a window
pub struct Check {
    pub type_line: String,
    pub version_line: String,
    pub disk_line: String,
    /// The data in use now (bytes), to say what deleting it frees.
    pub live_bytes: Option<u64>,
    /// Why it cannot be used, and the one thing that solves it.
    pub refusal: Option<(String, Fix)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    UpdateSentient,
    FreeDisk,
}

/// Compare. Blocking (reads the live schema version and free disk).
pub fn check(record: &InstallationRecord, s: &Summary) -> Check {
    let here = flavor_label(record.flavor());
    let type_line = if s.database_type == here {
        nf!("{} into {here}: the same type.", s.database_type)
    } else {
        nf!("{} into {here}. This installation keeps its type; the history is converted.", s.database_type)
    };
    let live_schema = crate::migration::Setup::current(record).ok().and_then(|setup| crate::restore_switch::schema_version_now(&setup.pg()).ok());
    let mut refusal = None;
    let installed = &record.sentient_version;
    let version_line = match (&s.sentient_version, s.schema_version, live_schema) {
        (v, Some(theirs), Some(ours)) if theirs > ours => {
            let made = v.clone().unwrap_or_else(|| "a newer SENTIENT".into());
            refusal = Some((
                nf!("This backup was made on SENTIENT {made}. This installation is {installed} and cannot read newer data."),
                Fix::UpdateSentient,
            ));
            nf!("{made} into {installed}: newer than this SENTIENT.")
        }
        (Some(v), Some(theirs), Some(ours)) if theirs < ours => nf!("{v} into {installed}. The data is brought up to {installed} while it is restored."),
        (Some(v), _, _) if v == installed => nf!("{v} into {installed}: the same version."),
        (Some(v), _, _) => nf!("{v} into {installed}. The data is brought up to {installed} while it is restored when it is older."),
        (None, _, _) => format!("{}", n!("Not recorded in this backup (made before versions were kept). A backup from a newer SENTIENT is found after it is restored beside the live data, which stays untouched.")),
    };
    let status = crate::dbstatus::read(record);
    let free = status.pg.as_ref().and_then(|p| p.free_bytes);
    let live_bytes = status.pg.as_ref().map(|p| p.db_bytes + status.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0));
    let need = s.restored_bytes;
    let disk_line = match free {
        Some(f) => nf!("Needs about {}, {} free.", crate::dbstatus::bytes_label(need), crate::dbstatus::bytes_label(f)),
        None => nf!("Needs about {}.", crate::dbstatus::bytes_label(need)),
    };
    if refusal.is_none() {
        if let Some(f) = free.filter(|f| *f < need) {
            refusal = Some((
                nf!("Not enough free disk: the restore needs about {}, {} is free.", crate::dbstatus::bytes_label(need), crate::dbstatus::bytes_label(f)),
                Fix::FreeDisk,
            ));
        }
    }
    Check { type_line, version_line, disk_line, live_bytes, refusal }
}

pub const STEP_CHECK: &str = n!("Checking the backup file and its passphrase");
pub const STEP_RESTORE: &str = n!("Restoring the backup beside the live data (and converting it)");
pub const STEP_SWITCH: &str = n!("Switching SENTIENT to the restored data and checking it answers");
pub const STEP_DELETE: &str = n!("Deleting the previous data");

#[cfg(test)]
pub fn planned_steps(delete_old: bool) -> Vec<String> {
    let mut v = vec![STEP_CHECK.to_string(), STEP_RESTORE.to_string(), STEP_SWITCH.to_string()];
    if delete_old {
        v.push(STEP_DELETE.to_string());
    }
    v
}

/// Steps of a nested call as log lines, so the checklist keeps its own.
pub(crate) fn nested(sink: &ProgressFn) -> ProgressFn {
    let sink = sink.clone();
    std::sync::Arc::new(move |p| match p {
        Progress::Step { name } => sink(Progress::Log { line: format!(">> {name}") }),
        Progress::Done { message } => sink(Progress::Log { line: message }),
        other => sink(other),
    })
}

/// Replace this instance's data with the backup. Returns what was done.
pub fn run(record: InstallationRecord, file: PathBuf, passphrase: Option<String>, delete_old: bool, sink: ProgressFn) -> Result<(), String> {
    sink(Progress::Step { name: STEP_CHECK.into() });
    let s = summary(&file, passphrase.as_deref()).map_err(|e| match e {
        Unreadable::Passphrase(m) | Unreadable::Other(m) => m,
    })?;
    if let Some((why, _)) = check(&record, &s).refusal {
        return Err(why);
    }
    let target = format!("sentient_restore_{}", chrono::Local::now().format("%Y%m%d%H%M"));
    sink(Progress::Step { name: STEP_RESTORE.into() });
    crate::backup_jobs::restore_with(&record, &file, &target, None, passphrase, nested(&sink))?;
    sink(Progress::Step { name: STEP_SWITCH.into() });
    let aside = crate::restore_switch::run(&record, &target, nested(&sink))?;
    let name = file.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if delete_old {
        sink(Progress::Step { name: STEP_DELETE.into() });
        // The previous PostgreSQL data is kept as `aside`; ClickHouse's is in
        // the restored database's name after the exchange.
        let kept = crate::kept::list(&record)?;
        let mut freed = 0i64;
        for item in kept.iter().filter(|k| k.name == aside || k.name == target) {
            crate::kept::remove(&record, item, nested(&sink))?;
            freed += item.bytes.unwrap_or(0);
        }
        sink(Progress::Done {
            message: nf!(
                "SENTIENT is running on the data from {name}. The previous data was deleted ({} freed).",
                crate::dbstatus::bytes_label(freed.max(0) as u64)
            ),
        });
    } else {
        sink(Progress::Done {
            message: nf!("SENTIENT is running on the data from {name}. The previous data is kept under \"Kept after earlier changes\" until you remove it."),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentient_backup_core::manifest::{EncryptionInfo, SourceInfo};

    fn manifest(ts: Option<&str>, ch: bool, recorded: Option<&str>) -> Manifest {
        Manifest {
            format_version: 1,
            tool_version: "x".into(),
            created_at: chrono::Utc::now(),
            source: SourceInfo {
                database: "sentient".into(),
                postgres_version: "18".into(),
                timescaledb_version: ts.map(String::from),
                sentient_version: Some("4.3.0.58".into()),
                schema_version: Some(30),
                database_type: recorded.map(String::from),
                host: Some("PLANT-B".into()),
            },
            components: Vec::new(),
            telemetry: TelemetrySelection::Range { days: 90 },
            files: Vec::new(),
            file_stores: Vec::new(),
            encryption: EncryptionInfo::none(),
            clickhouse: ch.then(|| sentient_backup_core::clickhouse::ChSection { database: "s".into(), version: "24".into(), members: Vec::new() }),
            platform: None,
        }
    }

    #[test]
    fn the_type_is_recorded_or_worked_out() {
        let p = Path::new("b.sentient-backup");
        assert_eq!(from_manifest(p, 10, false, &manifest(Some("2.2"), false, None)).database_type, "PostgreSQL + TimescaleDB");
        assert_eq!(from_manifest(p, 10, false, &manifest(None, true, None)).database_type, "PostgreSQL + ClickHouse");
        assert_eq!(from_manifest(p, 10, false, &manifest(None, false, Some("PostgreSQL"))).database_type, "PostgreSQL");
        let s = from_manifest(p, 1000, false, &manifest(None, false, None));
        assert_eq!(s.holds, vec!["platform data".to_string(), "telemetry history (last 90 days)".to_string()]);
        assert_eq!(s.restored_bytes, 3000);
        assert_eq!(planned_steps(true).len(), 4);
    }
}
