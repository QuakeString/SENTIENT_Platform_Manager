// Copyright © 2016-2026 The SENTIENT Authors

//! What part of a backup a restore takes, and the restores that are not
//! "everything into an empty database":
//!
//! - [`Scope::Configuration`]: the dump without telemetry (`ts_kv`,
//!   `ts_kv_latest`, the telemetry members, ClickHouse). Into an empty
//!   database beside the live one, as everything does. To keep the target's
//!   own telemetry ("Keep"), [`carry_telemetry`] brings the live history and
//!   latest values into the restored database before the switch: the
//!   restored key dictionary is first renumbered to the live one's ids, so
//!   history kept in place (ClickHouse, which is not exchanged then) and
//!   history copied both stay tied to the right key names.
//! - [`Scope::TelemetryOnly`] and [`Scope::Range`]: into the running
//!   database (`cfg` the live one). The backup's key ids are mapped to the
//!   live dictionary by name (missing names are added); rows already there
//!   for the same entity, key and time are kept (telemetry only), or the
//!   live rows of the range are replaced (a range). PostgreSQL does it in one
//!   transaction; ClickHouse marks the rows it inserts and removes them again
//!   when the restore fails.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::ConnConfig;
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::progress::ProgressFn;

/// What to restore.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Everything,
    /// Devices, dashboards, users and settings; no telemetry.
    Configuration,
    /// The backup's telemetry into the running instance; its configuration stays.
    TelemetryOnly,
    /// The backup's telemetry from `from_ms` up to (not including) `to_ms`,
    /// replacing the running instance's telemetry for those times.
    Range { from_ms: i64, to_ms: i64 },
}

impl Scope {
    /// Goes into the running database rather than a new one beside it.
    pub fn into_live(&self) -> bool {
        matches!(self, Scope::TelemetryOnly | Scope::Range { .. })
    }

    /// Restores the configuration (the dump).
    pub fn restores_configuration(&self) -> bool {
        matches!(self, Scope::Everything | Scope::Configuration)
    }

    /// Reads telemetry from the backup.
    pub fn restores_telemetry(&self) -> bool {
        !matches!(self, Scope::Configuration)
    }

    /// "Everything", "Configuration only", "Telemetry only", "Telemetry from 1 Sep 2026 to 14 Nov 2026".
    pub fn label(&self) -> String {
        match self {
            Scope::Everything => "Everything".into(),
            Scope::Configuration => "Configuration only".into(),
            Scope::TelemetryOnly => "Telemetry only".into(),
            Scope::Range { from_ms, to_ms } => format!("Telemetry from {} to {}", day(*from_ms), day(to_ms.saturating_sub(1))),
        }
    }
}

fn day(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_default()
}

/// The month a telemetry member holds, as (year, month): PostgreSQL's
/// `db/telemetry/2026-09.copy.zst`, ClickHouse's `clickhouse/ts_kv/202609…`.
pub fn member_month(path: &str) -> Option<(i32, u32)> {
    let name = if let Some(r) = path.strip_prefix(crate::backup::TELEMETRY_MONTH_PREFIX) {
        r
    } else if let Some(r) = path.strip_prefix(&format!("clickhouse/{}/", crate::clickhouse::HISTORY_TABLE)) {
        r
    } else {
        return None;
    };
    let digits: String = name.chars().filter(|c| c.is_ascii_digit()).take(6).collect();
    let y = digits.get(..4)?.parse().ok()?;
    let m = digits.get(4..6)?.parse().ok().filter(|m| (1..=12).contains(m))?;
    Some((y, m))
}

/// Whether a member is telemetry (history of either store, the latest values,
/// or format 1's one member).
pub fn is_telemetry_member(path: &str) -> bool {
    path.starts_with(crate::backup::TELEMETRY_MONTH_PREFIX) || path == crate::backup::TELEMETRY_MEMBER || path.starts_with("clickhouse/")
}

/// The first and last millisecond of a month.
fn month_bounds((y, m): (i32, u32)) -> (i64, i64) {
    use chrono::TimeZone;
    let start = chrono::Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap().timestamp_millis();
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (start, chrono::Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).unwrap().timestamp_millis())
}

/// The members a restore of `scope` reads, leaving `skip` out: the dump
/// unless only telemetry is wanted (then still the dump, for its key
/// dictionary), telemetry months overlapping a range, file stores with the
/// configuration.
pub fn members_for(manifest: &Manifest, scope: &Scope, skip: &[String]) -> Vec<String> {
    manifest
        .files
        .iter()
        .map(|f| f.path.clone())
        .filter(|p| !skip.contains(p))
        .filter(|p| {
            if p == crate::backup::DUMP_MEMBER {
                return true;
            }
            if p.starts_with("files/") {
                return scope.restores_configuration();
            }
            if !is_telemetry_member(p) {
                return true;
            }
            match scope {
                Scope::Configuration => false,
                Scope::Everything | Scope::TelemetryOnly => true,
                Scope::Range { from_ms, to_ms } => match member_month(p) {
                    Some(m) => {
                        let (s, e) = month_bounds(m);
                        s < *to_ms && e > *from_ms
                    }
                    // The latest values and format 1's single member: the
                    // latest are recomputed from the range, format 1 is read whole.
                    None => p == crate::backup::TELEMETRY_MEMBER,
                },
            }
        })
        .collect()
}

/// The bytes those members take in the file.
pub fn bytes_for(manifest: &Manifest, scope: &Scope, skip: &[String]) -> u64 {
    let wanted = members_for(manifest, scope, skip);
    manifest.files.iter().filter(|f| wanted.contains(&f.path)).map(|f| f.bytes).sum()
}

/// The months of telemetry a backup holds, oldest first: (year, month, bytes).
pub fn telemetry_months(manifest: &Manifest) -> Vec<(i32, u32, u64)> {
    let mut by: std::collections::BTreeMap<(i32, u32), u64> = Default::default();
    for f in &manifest.files {
        if let Some(m) = member_month(&f.path) {
            *by.entry(m).or_default() += f.bytes;
        }
    }
    by.into_iter().map(|((y, m), b)| (y, m, b)).collect()
}

/// How long a restore takes, from its sizes (seconds), and how long SENTIENT
/// is down. Measured rates on an ordinary server disk; the result is rounded
/// up to whole minutes by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    pub total_secs: u64,
    pub downtime_secs: u64,
}

/// `file_bytes`: what is read from the file for this scope; `restored_bytes`:
/// what it becomes in the databases; `safety_bytes`: a safety backup first
/// (0: none); `carry_bytes`: telemetry carried over (Keep); `native`: a
/// native install (SENTIENT's service stops and starts, a little longer).
pub fn estimate(file_bytes: u64, restored_bytes: u64, safety_bytes: u64, carry_bytes: u64, scope: &Scope, native: bool) -> Estimate {
    const MB: f64 = 1e6;
    // Read and check (sha256 + unzstd) ~150 MB/s; restore and index ~25 MB/s
    // of restored data; a backup ~40 MB/s of data; a copy inside PostgreSQL
    // ~60 MB/s.
    let secs = file_bytes as f64 / (150.0 * MB)
        + restored_bytes as f64 / (25.0 * MB)
        + safety_bytes as f64 / (40.0 * MB)
        + carry_bytes as f64 / (60.0 * MB)
        + 20.0;
    let downtime = if scope.into_live() { 0 } else if native { 120 } else { 60 };
    Estimate { total_secs: secs.ceil() as u64 + downtime, downtime_secs: downtime }
}

/// The backup's key dictionary (name → id), read from its dump without
/// restoring it. `dump` is the decompressed dump (pg_restore's custom format).
pub fn backup_key_dictionary(tools: &crate::pg_tools::PgTools, dump: &Path) -> Result<Vec<(String, i32)>> {
    let _ = (tools, dump);
    Err(Error::msg("backup_key_dictionary: not built yet"))
}

/// Configuration only, Keep: renumber the restored database's key
/// dictionary (and `attribute_kv`, `ts_kv_latest`) to the live one's ids,
/// adding the live names it lacks, then copy the live `ts_kv` (month by
/// month, checked) and `ts_kv_latest` into it. Returns the history rows
/// copied. A ClickHouse installation keeps its history where it is (the
/// switch does not exchange it), so only the dictionary and the latest
/// values are carried.
pub async fn carry_telemetry(live: &ConnConfig, restored: &ConnConfig, watch: Option<&crate::space::WatchGuard>, sink: &ProgressFn) -> Result<u64> {
    let _ = (live, restored, watch, sink);
    Err(Error::msg("carry_telemetry: not built yet"))
}

/// Configuration only: the entities with telemetry on the running instance
/// that the backup does not have (their telemetry stays without a device
/// with Keep). Their names, up to `limit`, and how many in all.
pub async fn orphans(live: &ConnConfig, input: &Path, passphrase: Option<&str>, temp_dir: Option<&Path>, limit: usize) -> Result<(Vec<String>, usize)> {
    let _ = (live, input, passphrase, temp_dir, limit);
    Err(Error::msg("orphans: not built yet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(paths: &[(&str, u64)]) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "format_version": 2, "tool_version": "t", "created_at": "2026-11-15T00:00:00Z",
            "source": {"database": "s", "postgres_version": "18", "timescaledb_version": null},
            "components": [], "telemetry": {"mode": "all"},
            "files": paths.iter().map(|(p, b)| serde_json::json!({"path": p, "bytes": b, "sha256": ""})).collect::<Vec<_>>(),
            "file_stores": [], "encryption": {"scheme": "none"}
        }))
        .unwrap()
    }

    #[test]
    fn a_range_reads_only_its_months() {
        let m = manifest(&[
            ("db/dump.pgc.zst", 10),
            ("db/telemetry/2026-08.copy.zst", 100),
            ("db/telemetry/2026-09.copy.zst", 100),
            ("db/telemetry/2026-10.copy.zst", 100),
            ("clickhouse/ts_kv/202611.native.zst", 50),
            ("files/reports.tar.zst", 7),
        ]);
        let sep1 = 1_788_220_800_000; // 2026-09-01
        let oct15 = 1_792_022_400_000; // 2026-10-14
        let r = Scope::Range { from_ms: sep1, to_ms: oct15 };
        assert_eq!(members_for(&m, &r, &[]), ["db/dump.pgc.zst", "db/telemetry/2026-09.copy.zst", "db/telemetry/2026-10.copy.zst"]);
        assert_eq!(bytes_for(&m, &r, &[]), 210);
        assert_eq!(members_for(&m, &Scope::Configuration, &[]), ["db/dump.pgc.zst", "files/reports.tar.zst"]);
        assert_eq!(members_for(&m, &Scope::TelemetryOnly, &["db/telemetry/2026-09.copy.zst".into()]).len(), 4);
        assert_eq!(telemetry_months(&m).len(), 4);
        assert_eq!(member_month("clickhouse/ts_kv/202611.native.zst"), Some((2026, 11)));
        assert_eq!(member_month("clickhouse/ts_kv_latest_agg/all.native.zst"), None);
    }

    #[test]
    fn scope_round_trips_and_reads() {
        let r = Scope::Range { from_ms: 1_788_220_800_000, to_ms: 1_794_700_800_000 };
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Scope>(&j).unwrap(), r);
        assert_eq!(r.label(), "Telemetry from 1 Sep 2026 to 14 Nov 2026");
        assert!(r.into_live() && !Scope::Configuration.into_live());
        let e = estimate(2_800_000_000, 9_200_000_000, 2_900_000_000, 0, &Scope::Everything, false);
        assert!(e.total_secs > 400 && e.total_secs < 1200, "{e:?}");
        assert_eq!(e.downtime_secs, 60);
    }
}
