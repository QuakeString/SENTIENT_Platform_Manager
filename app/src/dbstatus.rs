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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DbStatus {
    pub pg: Option<PgInfo>,
    pub pg_error: Option<String>,
    /// Only on ClickHouse (hybrid) installs.
    pub ch: Option<ChInfo>,
    pub ch_error: Option<String>,
    pub hybrid: bool,
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
    match name {
        "ts_kv" => Some("telemetry history"),
        "ts_kv_latest" => Some("latest values"),
        "audit_log" => Some("audit log"),
        "event" => Some("events"),
        "alarm" => Some("alarms"),
        _ => None,
    }
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

/// Free bytes at `path`: `df` on Linux (inside the container for Docker).
fn free_bytes_linux(path: &str, container: Option<&str>) -> Option<u64> {
    let out = match container {
        Some(c) => std::process::Command::new("docker").args(["exec", c, "df", "-PB1", path]).output().ok()?,
        None => std::process::Command::new("df").args(["-PB1", path]).output().ok()?,
    };
    out.status.success().then(|| parse_df(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// Free bytes on the Windows drive holding `path`.
#[cfg(windows)]
fn free_bytes_windows(path: &std::path::Path) -> Option<u64> {
    let drive = path.to_string_lossy().chars().next()?;
    let script = format!("(Get-PSDrive -Name '{drive}').Free");
    let mut c = std::process::Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c.output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
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
            info.free_bytes = if docker {
                free_bytes_linux("/var/lib/postgresql/data", Some(&names.pg))
            } else {
                #[cfg(windows)]
                {
                    let _ = &data_dir;
                    free_bytes_windows(&record.pg_dir)
                }
                #[cfg(not(windows))]
                {
                    data_dir.as_deref().and_then(|d| free_bytes_linux(d, None))
                }
            };
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
                        free_bytes_linux("/var/lib/clickhouse", Some(&names.ch))
                    } else if cfg!(target_os = "linux") {
                        free_bytes_linux("/var/lib/clickhouse", None)
                    } else {
                        None
                    };
                    status.ch = Some(ChInfo { version, disk_bytes: disk, rows, first_ms: first, last_ms: last, free_bytes });
                }
                None => status.ch_error = Some(format!("unexpected answer: {}", tsv.trim())),
            },
            Err(e) => status.ch_error = Some(e.to_string()),
        }
    }
    status
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
        assert_eq!(table_label("ts_kv"), Some("telemetry history"));
    }
}
