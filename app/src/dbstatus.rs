// Copyright © 2016-2026 The SENTIENT Authors

//! What the Status screen shows about the databases: PostgreSQL's version,
//! size, largest tables (telemetry history first), connections, TimescaleDB,
//! the free space on its disk and the last backup; on hybrid installs also
//! ClickHouse's version, disk use, stored history and free space.
//!
//! Every read is cheap (catalog sizes, system.parts) and runs off the UI
//! thread under a time limit; whatever cannot be read says so instead of
//! holding the screen up.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use sentient_backup_core::clickhouse::{ChConn, HISTORY_TABLE};
use sentient_backup_core::db::{ConnConfig, DbInspector};
use sentient_installer_core::stack::Flavor;

use crate::installation::{Engine, InstallationRecord};

/// The longest a whole read may take.
const LIMIT: Duration = Duration::from_secs(25);

/// Free space below this is called low.
pub const LOW_DISK: u64 = 5 << 30;
/// And below this, critical.
pub const CRITICAL_DISK: u64 = 1 << 30;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PgInfo {
    pub version: String,
    pub db_bytes: u64,
    pub connections: u32,
    pub max_connections: u32,
    /// TimescaleDB's version when the extension is in use.
    pub timescaledb: Option<String>,
    /// Largest first, telemetry history and latest values leading.
    pub tables: Vec<Table>,
    pub free_bytes: Option<u64>,
    /// The size of the disk holding the database.
    #[serde(default)]
    pub disk_total: Option<u64>,
    /// Tables in the database (for "Everything else: N tables").
    #[serde(default)]
    pub table_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChInfo {
    pub version: String,
    pub disk_bytes: u64,
    /// Rows of telemetry history.
    pub rows: u64,
    /// The oldest and newest stored history, ms since the epoch.
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub free_bytes: Option<u64>,
    /// The history's size before compression.
    #[serde(default)]
    pub uncompressed_bytes: u64,
    /// Parts ClickHouse set aside as broken (None: could not be read).
    #[serde(default)]
    pub broken_parts: Option<u64>,
    /// History per month, newest first: (`202611`, bytes on disk).
    #[serde(default)]
    pub months: Vec<(String, u64)>,
    /// How long history is kept, in days (None: forever).
    #[serde(default)]
    pub ttl_days: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DbStatus {
    pub pg: Option<PgInfo>,
    pub pg_error: Option<String>,
    /// Only on ClickHouse (hybrid) installs.
    pub ch: Option<ChInfo>,
    pub ch_error: Option<String>,
    pub hybrid: bool,
    /// Database copies earlier updates, migrations and restores left
    /// (crate::kept), when they could be listed.
    #[serde(default)]
    pub kept_bytes: Option<u64>,
}

/// The size query: one row of JSON. Partitioned tables (telemetry history on
/// stock PostgreSQL) count their partitions.
const PG_SQL: &str = "SELECT json_build_object(\
 'version', current_setting('server_version'),\
 'db_bytes', pg_database_size(current_database()),\
 'connections', (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()),\
 'max_connections', current_setting('max_connections')::int,\
 'timescaledb', (SELECT extversion FROM pg_extension WHERE extname = 'timescaledb'),\
 'data_directory', current_setting('data_directory', true),\
 'table_count', (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE c.relkind IN ('r', 'p') AND NOT c.relispartition AND n.nspname = 'public'),\
 'tables', COALESCE((SELECT json_agg(json_build_object('name', name, 'bytes', bytes)) FROM (\
   SELECT c.relname AS name, CASE WHEN c.relkind = 'p' THEN \
     (SELECT COALESCE(sum(pg_total_relation_size(relid)), 0) FROM pg_partition_tree(c.oid)) \
     ELSE pg_total_relation_size(c.oid) END AS bytes \
   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE c.relkind IN ('r', 'p') AND NOT c.relispartition AND n.nspname = 'public' \
   ORDER BY 2 DESC LIMIT 8) t), '[]'::json))::text";

/// TimescaleDB keeps a hypertable's rows in chunks elsewhere: its size.
const HYPERTABLE_SQL: &str = "SELECT COALESCE(json_object_agg(hypertable_name, \
 hypertable_size(format('%I.%I', hypertable_schema, hypertable_name)::regclass)), '{}'::json)::text \
 FROM timescaledb_information.hypertables WHERE hypertable_schema = 'public'";

#[derive(Deserialize)]
struct PgRow {
    version: String,
    db_bytes: u64,
    connections: u32,
    max_connections: u32,
    timescaledb: Option<String>,
    data_directory: Option<String>,
    tables: Vec<Table>,
    #[serde(default)]
    table_count: u32,
}

/// The size query's answer, with hypertable sizes (`hyper`, JSON object of
/// name → bytes) in place of their empty parents. Returns the data directory.
pub fn parse_pg(json: &str, hyper: Option<&str>) -> Result<(PgInfo, Option<String>), String> {
    let row: PgRow = serde_json::from_str(json).map_err(|e| format!("unexpected answer: {e}"))?;
    let mut tables = row.tables;
    if let Some(h) = hyper.and_then(|h| serde_json::from_str::<std::collections::HashMap<String, u64>>(h).ok()) {
        for t in &mut tables {
            if let Some(bytes) = h.get(&t.name) {
                t.bytes = *bytes;
            }
        }
        for (name, bytes) in h {
            if !tables.iter().any(|t| t.name == name) {
                tables.push(Table { name, bytes });
            }
        }
    }
    order_tables(&mut tables);
    tables.truncate(8);
    Ok((
        PgInfo {
            version: row.version,
            db_bytes: row.db_bytes,
            connections: row.connections,
            max_connections: row.max_connections,
            timescaledb: row.timescaledb,
            tables,
            free_bytes: None,
            disk_total: None,
            table_count: row.table_count,
        },
        row.data_directory,
    ))
}

/// Telemetry history, then latest values, then the rest by size.
pub fn order_tables(tables: &mut [Table]) {
    let rank = |t: &Table| match t.name.as_str() {
        "ts_kv" => 0,
        "ts_kv_latest" => 1,
        _ => 2,
    };
    tables.sort_by(|a, b| rank(a).cmp(&rank(b)).then(b.bytes.cmp(&a.bytes)));
}

/// What a table holds, for the ones an operator recognises.
pub fn table_label(name: &str) -> Option<&'static str> {
    plain_table(name).map(|(n, _)| n)
}

/// A table's plain name and what is in it: `("Audit log", "who did what")`.
pub fn plain_table(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "ts_kv" => ("Telemetry history", "values over time"),
        "ts_kv_latest" => ("Latest values", "newest of each key"),
        "attribute_kv" => ("Attributes", "device settings"),
        "audit_log" => ("Audit log", "who did what"),
        "event" => ("Events", "debug, lifecycle"),
        "alarm" => ("Alarms", ""),
        "resource" => ("Resources", "images, files, scripts"),
        "widget_type" => ("Widgets", "the widget library"),
        "dashboard" => ("Dashboards", ""),
        "notification" => ("Notifications", ""),
        "report" | "report_file" => ("Reports", ""),
        "rule_node" | "rule_node_state" => ("Rule chains", ""),
        "device" => ("Devices", ""),
        _ => return None,
    })
}

/// `df -PB1 <dir>`: the total ("1-blocks") column of its last line.
pub fn parse_df_total(text: &str) -> Option<u64> {
    text.lines().filter(|l| !l.trim().is_empty()).last()?.split_whitespace().nth(1)?.parse().ok()
}

/// What the history per month reads as: `202611` → `Nov 2026`.
pub fn month_label(partition: &str) -> String {
    let p = partition.trim().trim_matches('\'');
    match (p.get(..4).and_then(|y| y.parse::<i32>().ok()), p.get(4..6).and_then(|m| m.parse::<u32>().ok())) {
        (Some(y), Some(m)) => chrono::NaiveDate::from_ymd_opt(y, m, 1).map(|d| d.format("%b %Y").to_string()).unwrap_or_else(|| p.to_string()),
        _ => p.to_string(),
    }
}

/// The retention days in a ClickHouse table definition (`… TTL
/// toDateTime(ts / 1000) + toIntervalDay(365) …`), None without a TTL.
pub fn parse_ttl_days(ddl: &str) -> Option<u32> {
    let after = ddl.split(" TTL ").nth(1)?;
    let n = after.split("toIntervalDay(").nth(1)?.split(')').next()?;
    n.trim().parse().ok()
}

/// `df -PB1 <dir>`: the "Available" column of its last line.
pub fn parse_df(text: &str) -> Option<u64> {
    text.lines().filter(|l| !l.trim().is_empty()).last()?.split_whitespace().nth(3)?.parse().ok()
}

/// `1.2 GB`, `340 MB`, `12 KB`, `512 B`.
pub fn bytes_label(b: u64) -> String {
    const K: f64 = 1024.0;
    let f = b as f64;
    if f >= K * K * K * K {
        format!("{:.1} TB", f / (K * K * K * K))
    } else if f >= K * K * K {
        format!("{:.1} GB", f / (K * K * K))
    } else if f >= K * K {
        format!("{:.0} MB", f / (K * K))
    } else if f >= K {
        format!("{:.0} KB", f / K)
    } else {
        format!("{b} B")
    }
}

/// `1 234 567` rows as `1.2 million`.
pub fn count_label(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.0} thousand", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1} million", n as f64 / 1e6),
        _ => format!("{:.1} billion", n as f64 / 1e9),
    }
}

/// A day from ms since the epoch, in local time: `2026-11-14`.
pub fn day_label(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// ClickHouse's `SELECT … FORMAT TSV` answer: disk bytes, rows, first, last.
pub fn parse_ch_history(tsv: &str) -> Option<(u64, u64, Option<i64>, Option<i64>)> {
    let mut f = tsv.trim().split('\t');
    let disk = f.next()?.parse().ok()?;
    let rows = f.next()?.parse().ok()?;
    let ms = |s: Option<&str>| s.and_then(|s| s.parse::<i64>().ok()).filter(|v| *v > 0);
    let first = ms(f.next());
    let last = ms(f.next());
    Some((disk, rows, first, last))
}

/// Free and total bytes at `path`: `df` on Linux (inside the container for
/// Docker).
fn disk_linux(path: &str, container: Option<&str>) -> Option<(u64, Option<u64>)> {
    let out = match container {
        Some(c) => std::process::Command::new("docker").args(["exec", c, "df", "-PB1", path]).output().ok()?,
        None => std::process::Command::new("df").args(["-PB1", path]).output().ok()?,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    out.status.success().then(|| parse_df(&text).map(|free| (free, parse_df_total(&text)))).flatten()
}

/// Free and total bytes on the Windows drive holding `path`.
#[cfg(windows)]
fn disk_windows(path: &std::path::Path) -> Option<(u64, Option<u64>)> {
    let drive = path.to_string_lossy().chars().next()?;
    let script = format!("$d = Get-PSDrive -Name '{drive}'; \"$($d.Free) $($d.Used + $d.Free)\"");
    let mut c = std::process::Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.split_whitespace().map(|n| n.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten()))
}

/// The passwords to sign in with: SENTIENT's database user and, on hybrid
/// installs, its ClickHouse user. On native Linux they are in a file only
/// root reads, so this asks for the administrator password.
fn passwords(record: &InstallationRecord) -> Result<(String, String), String> {
    match record.engine {
        Engine::Docker => {
            let s = sentient_installer_core::distro::docker_ops::read_secrets(&record.stack)?;
            Ok((s.db_password, s.ch_password))
        }
        Engine::Native => {
            #[cfg(target_os = "linux")]
            {
                let s = sentient_installer_core::native_linux::read_secrets(&record.native_config())?;
                Ok((s.db_password, s.ch_password))
            }
            #[cfg(not(target_os = "linux"))]
            {
                use crate::secrets::{Secret, get};
                let db = get(Secret::Database).ok_or("The database password is not in the secret store.")?;
                Ok((db, get(Secret::Clickhouse).unwrap_or_default()))
            }
        }
    }
}

/// Whether reading asks for the administrator password (native Linux): the
/// screen then reads only when asked, not on a timer.
pub fn needs_admin(record: &InstallationRecord) -> bool {
    cfg!(target_os = "linux") && record.engine == Engine::Native
}

/// Read everything, within [`LIMIT`]. Blocking: run it in the background.
pub fn read(record: &InstallationRecord) -> DbStatus {
    // Development aid: a reading from a file (screens).
    #[cfg(debug_assertions)]
    if let Some(s) = std::env::var_os("SPM_DEBUG_DBSTATUS").and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()) {
        return s;
    }
    let record = record.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_now(&record));
    });
    rx.recv_timeout(LIMIT).unwrap_or_else(|_| DbStatus {
        pg_error: Some(format!("No answer within {} seconds.", LIMIT.as_secs())),
        ..Default::default()
    })
}

fn read_now(record: &InstallationRecord) -> DbStatus {
    let st = &record.stack;
    let hybrid = record.flavor() == Flavor::Clickhouse;
    let mut status = DbStatus { hybrid, ..Default::default() };
    let (db_password, ch_password) = match passwords(record) {
        Ok(p) => p,
        Err(e) => {
            status.pg_error = Some(e);
            return status;
        }
    };
    let docker = record.engine == Engine::Docker;
    let names = st.names();

    let conn = ConnConfig {
        host: "127.0.0.1".into(),
        port: st.pg_port,
        dbname: st.db_name.clone(),
        user: st.db_user.clone(),
        password: db_password,
    };
    let pg = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string()).and_then(|rt| {
        rt.block_on(async {
            let db = tokio::time::timeout(Duration::from_secs(8), DbInspector::connect(&conn))
                .await
                .map_err(|_| "PostgreSQL did not answer.".to_string())?
                .map_err(|e| format!("Could not sign in to PostgreSQL: {e}"))?;
            let main = db.text(PG_SQL).await.map_err(|e| e.to_string())?.unwrap_or_default();
            let hyper = if main.contains("\"timescaledb\" : null") || main.contains("\"timescaledb\":null") {
                None
            } else {
                db.text(HYPERTABLE_SQL).await.ok().flatten()
            };
            parse_pg(&main, hyper.as_deref())
        })
    });
    match pg {
        Ok((mut info, data_dir)) => {
            let disk = if docker {
                disk_linux("/var/lib/postgresql/data", Some(&names.pg))
            } else {
                #[cfg(windows)]
                {
                    let _ = &data_dir;
                    disk_windows(&record.pg_dir)
                }
                #[cfg(not(windows))]
                {
                    data_dir.as_deref().and_then(|d| disk_linux(d, None))
                }
            };
            info.free_bytes = disk.map(|d| d.0);
            info.disk_total = disk.and_then(|d| d.1);
            status.pg = Some(info);
        }
        Err(e) => status.pg_error = Some(e),
    }

    if hybrid {
        let ch = ChConn {
            url: format!("http://127.0.0.1:{}", st.ch_http_port),
            database: st.ch_db.clone(),
            user: st.ch_user.clone(),
            password: ch_password,
        };
        let q = format!(
            "SELECT (SELECT sum(bytes_on_disk) FROM system.parts WHERE active AND database = currentDatabase()), \
             (SELECT sum(rows) FROM system.parts WHERE active AND database = currentDatabase() AND table = '{HISTORY_TABLE}'), \
             (SELECT min(ts) FROM {HISTORY_TABLE}), (SELECT max(ts) FROM {HISTORY_TABLE}) \
             SETTINGS max_execution_time = 10 FORMAT TSV"
        );
        match ch.version().and_then(|v| ch.text(&q).map(|t| (v, t))) {
            Ok((version, tsv)) => match parse_ch_history(&tsv) {
                Some((disk, rows, first, last)) => {
                    let free_bytes = if docker {
                        disk_linux("/var/lib/clickhouse", Some(&names.ch)).map(|d| d.0)
                    } else if cfg!(target_os = "linux") {
                        disk_linux("/var/lib/clickhouse", None).map(|d| d.0)
                    } else {
                        None
                    };
                    // The rest is a bonus: what cannot be read stays empty.
                    let uncompressed = ch
                        .text("SELECT sum(data_uncompressed_bytes) FROM system.parts WHERE active AND database = currentDatabase() FORMAT TSV")
                        .ok()
                        .and_then(|t| t.trim().parse().ok())
                        .unwrap_or(0);
                    let broken_parts = ch
                        .text("SELECT count() FROM system.detached_parts WHERE database = currentDatabase() AND (startsWith(reason, 'broken') OR startsWith(reason, 'unexpected')) FORMAT TSV")
                        .ok()
                        .and_then(|t| t.trim().parse().ok());
                    let months = ch
                        .text(&format!(
                            "SELECT partition, sum(bytes_on_disk) FROM system.parts WHERE active AND database = currentDatabase() AND table = '{HISTORY_TABLE}' \
                             GROUP BY partition ORDER BY partition DESC FORMAT TSV"
                        ))
                        .map(|t| parse_months(&t))
                        .unwrap_or_default();
                    let ttl_days = ch
                        .text(&format!("SELECT engine_full FROM system.tables WHERE database = currentDatabase() AND name = '{HISTORY_TABLE}' FORMAT TSVRaw"))
                        .ok()
                        .and_then(|t| parse_ttl_days(&t));
                    status.ch = Some(ChInfo {
                        version,
                        disk_bytes: disk,
                        rows,
                        first_ms: first,
                        last_ms: last,
                        free_bytes,
                        uncompressed_bytes: uncompressed,
                        broken_parts,
                        months,
                        ttl_days,
                    });
                }
                None => status.ch_error = Some(format!("unexpected answer: {}", tsv.trim())),
            },
            Err(e) => status.ch_error = Some(e.to_string()),
        }
    }
    if let Some(pg) = &status.pg {
        record_size(pg.db_bytes + status.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0));
    }
    status.kept_bytes = crate::kept::list(record).ok().map(|k| k.iter().filter_map(|x| x.bytes).map(|b| b.max(0) as u64).sum());
    status
}

/// `202611\t2500000000` lines.
pub fn parse_months(tsv: &str) -> Vec<(String, u64)> {
    tsv.lines()
        .filter_map(|l| {
            let (p, b) = l.split_once('\t')?;
            Some((p.trim().to_string(), b.trim().parse().ok()?))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Growth: one size sample a day, to say how fast the databases grow
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Sample {
    /// `2026-11-15`
    day: String,
    bytes: u64,
}

fn samples_path() -> PathBuf {
    crate::updates::prefs_dir().join("database-size.json")
}

fn load_samples() -> Vec<Sample> {
    std::fs::read_to_string(samples_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Keep today's size (PostgreSQL plus ClickHouse); 60 days are kept.
pub fn record_size(bytes: u64) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut s = load_samples();
    s.retain(|x| x.day != today);
    s.push(Sample { day: today, bytes });
    s.sort_by(|a, b| a.day.cmp(&b.day));
    let keep = s.len().saturating_sub(60);
    s.drain(..keep);
    if let Some(dir) = samples_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(&s) {
        let _ = std::fs::write(samples_path(), json);
    }
}

/// Bytes a day over the last 7 days (or since the first sample, at least
/// one day back), and over how many days that was measured.
pub fn growth_per_day() -> Option<(i64, i64)> {
    growth_of(&load_samples(), chrono::Local::now().date_naive())
}

fn growth_of(samples: &[Sample], today: chrono::NaiveDate) -> Option<(i64, i64)> {
    let parse = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok();
    let last = samples.iter().filter_map(|s| parse(&s.day).map(|d| (d, s.bytes))).max_by_key(|(d, _)| *d)?;
    let week_ago = today - chrono::Duration::days(7);
    // The oldest sample within the week, else the newest before it.
    let base = samples
        .iter()
        .filter_map(|s| parse(&s.day).map(|d| (d, s.bytes)))
        .filter(|(d, _)| *d < last.0)
        .min_by_key(|(d, _)| (*d - week_ago).num_days().abs())?;
    let days = (last.0 - base.0).num_days();
    (days >= 1).then(|| ((last.1 as i64 - base.1 as i64) / days, days))
}

/// "about 3 years", "about 9 days" until `free` is used at `per_day`.
pub fn lasts_label(free: u64, per_day: i64) -> Option<String> {
    if per_day <= 0 {
        return None;
    }
    let days = free as i64 / per_day;
    Some(match days {
        0 => "less than a day".into(),
        1 => "about 1 day".into(),
        2..=60 => format!("about {days} days"),
        61..=729 => format!("about {} months", days / 30),
        _ => format!("about {} years", days / 365),
    })
}

// ---------------------------------------------------------------------------
// The last backup made from the Platform Manager
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastBackup {
    /// RFC 3339.
    pub at: String,
    pub file: String,
    pub bytes: Option<u64>,
}

fn last_backup_path() -> PathBuf {
    crate::updates::prefs_dir().join("last-backup.json")
}

/// Note a finished backup, for the Status screen.
pub fn record_backup(file: &std::path::Path) {
    let b = LastBackup {
        at: chrono::Local::now().to_rfc3339(),
        file: file.display().to_string(),
        bytes: std::fs::metadata(file).ok().map(|m| m.len()),
    };
    if let Ok(json) = serde_json::to_string_pretty(&b) {
        let path = last_backup_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, json);
    }
}

pub fn last_backup() -> Option<LastBackup> {
    serde_json::from_str(&std::fs::read_to_string(last_backup_path()).ok()?).ok()
}

/// "3 days ago", "5 hours ago", "just now".
pub fn ago_label(rfc3339: &str, now: chrono::DateTime<chrono::Local>) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(rfc3339) else { return String::new() };
    let mins = (now - t.with_timezone(&chrono::Local)).num_minutes().max(0);
    match mins {
        0..=1 => "just now".into(),
        2..=89 => format!("{mins} minutes ago"),
        90..=2879 => format!("{} hours ago", (mins + 30) / 60),
        _ => format!("{} days ago", (mins + 720) / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgresql_answer_puts_telemetry_first_and_uses_hypertable_sizes() {
        let json = r#"{"version":"18.1","db_bytes":5368709120,"connections":12,"max_connections":100,
            "timescaledb":"2.22.0","data_directory":"/var/lib/postgresql/data",
            "tables":[{"name":"audit_log","bytes":900},{"name":"ts_kv","bytes":0},{"name":"ts_kv_latest","bytes":300},{"name":"event","bytes":2000}]}"#;
        let (info, dir) = parse_pg(json, Some(r#"{"ts_kv": 4000000000}"#)).unwrap();
        assert_eq!(dir.as_deref(), Some("/var/lib/postgresql/data"));
        assert_eq!(info.version, "18.1");
        assert_eq!(info.timescaledb.as_deref(), Some("2.22.0"));
        let names: Vec<&str> = info.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["ts_kv", "ts_kv_latest", "event", "audit_log"]);
        assert_eq!(info.tables[0].bytes, 4_000_000_000);
    }

    #[test]
    fn postgresql_answer_without_timescaledb() {
        let json = r#"{"version":"18.1","db_bytes":1,"connections":1,"max_connections":100,"timescaledb":null,"data_directory":null,"tables":[]}"#;
        let (info, dir) = parse_pg(json, None).unwrap();
        assert_eq!(info.timescaledb, None);
        assert_eq!(dir, None);
        assert!(parse_pg("not json", None).is_err());
    }

    #[test]
    fn df_available_column() {
        let out = "Filesystem      1-blocks        Used   Available Capacity Mounted on\n/dev/sda1  491018342400 164567040000 301449703424      36% /\n";
        assert_eq!(parse_df(out), Some(301_449_703_424));
        assert_eq!(parse_df(""), None);
        assert_eq!(parse_df_total(out), Some(491_018_342_400));
    }

    #[test]
    fn clickhouse_history_answer() {
        assert_eq!(
            parse_ch_history("123456789\t42000000\t1700000000000\t1763100000000\n"),
            Some((123_456_789, 42_000_000, Some(1_700_000_000_000), Some(1_763_100_000_000)))
        );
        // An empty history: min/max are 0.
        assert_eq!(parse_ch_history("0\t0\t0\t0"), Some((0, 0, None, None)));
        assert_eq!(parse_ch_history("oops"), None);
    }

    #[test]
    fn labels() {
        assert_eq!(bytes_label(512), "512 B");
        assert_eq!(bytes_label(12 * 1024), "12 KB");
        assert_eq!(bytes_label(340 * 1024 * 1024), "340 MB");
        assert_eq!(bytes_label(5 * 1024 * 1024 * 1024 + 300 * 1024 * 1024), "5.3 GB");
        assert_eq!(count_label(950), "950");
        assert_eq!(count_label(42_000), "42 thousand");
        assert_eq!(count_label(42_000_000), "42.0 million");
        assert_eq!(count_label(3_200_000_000), "3.2 billion");
        let now = chrono::Local::now();
        assert_eq!(ago_label(&now.to_rfc3339(), now), "just now");
        assert_eq!(ago_label(&(now - chrono::Duration::hours(5)).to_rfc3339(), now), "5 hours ago");
        assert_eq!(ago_label(&(now - chrono::Duration::days(3)).to_rfc3339(), now), "3 days ago");
        assert_eq!(table_label("ts_kv"), Some("Telemetry history"));
        assert_eq!(month_label("202611"), "Nov 2026");
        assert_eq!(parse_ttl_days("MergeTree PARTITION BY toYYYYMM(x) ORDER BY k TTL toDateTime(ts / 1000) + toIntervalDay(365) SETTINGS a = 1"), Some(365));
        assert_eq!(parse_ttl_days("MergeTree ORDER BY k SETTINGS a = 1"), None);
        assert_eq!(parse_months("202611\t2500\n202610\t5200\n"), vec![("202611".into(), 2500), ("202610".into(), 5200)]);
        assert_eq!(lasts_label(9 * 100, 100).as_deref(), Some("about 9 days"));
        assert_eq!(lasts_label(1000 * 100, 100).as_deref(), Some("about 2 years"));
        assert_eq!(lasts_label(100, 0), None);
    }

    #[test]
    fn growth_is_measured_over_about_a_week() {
        let s = |d: &str, b: u64| Sample { day: d.into(), bytes: b };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let samples = vec![s("2026-11-01", 0), s("2026-11-08", 1000), s("2026-11-12", 1500), s("2026-11-15", 1700)];
        assert_eq!(growth_of(&samples, today), Some((100, 7)));
        assert_eq!(growth_of(&[s("2026-11-15", 1)], today), None);
        assert_eq!(growth_of(&[s("2026-11-14", 100), s("2026-11-15", 300)], today), Some((200, 1)));
    }
}
