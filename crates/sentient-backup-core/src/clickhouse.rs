// Copyright © 2016-2026 The SENTIENT Authors

//! ClickHouse telemetry in a backup: SENTIENT's history (`ts_kv`, one archive
//! member per monthly partition) and its latest values (`ts_kv_latest_agg`),
//! over ClickHouse's HTTP interface in its `Native` format.
//!
//! `Native` keeps every column exactly as stored, including the aggregate
//! states of the latest-values table, so a restore puts back what was there
//! rather than a re-derivation. Inserting the history into `ts_kv` also feeds
//! SENTIENT's materialized view, which rebuilds the latest values from it;
//! the exported states then add the latest values that were written without
//! history (`ts_kv_latest_writes`). Both merge by timestamp, so the order and
//! the overlap do not matter.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::HashingWriter;

pub const HISTORY_TABLE: &str = "ts_kv";
pub const LATEST_TABLE: &str = "ts_kv_latest_agg";

/// Where ClickHouse answers (its HTTP port) and how to sign in.
#[derive(Debug, Clone)]
pub struct ChConn {
    /// e.g. `http://127.0.0.1:8123`
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

/// One exported piece, recorded in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChMember {
    pub table: String,
    /// `YYYYMM` for history, empty for the latest-values table.
    pub partition: String,
    pub rows: u64,
    /// Archive member holding the zstd-compressed Native stream.
    pub member: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChSection {
    pub database: String,
    pub version: String,
    pub members: Vec<ChMember>,
}

impl ChConn {
    fn request(&self, sql: &str) -> ureq::Request {
        ureq::post(&format!("{}/", self.url.trim_end_matches('/')))
            .query("database", &self.database)
            .query("query", sql)
            .set("X-ClickHouse-User", &self.user)
            .set("X-ClickHouse-Key", &self.password)
            .timeout(std::time::Duration::from_secs(3600))
    }

    /// The same server and sign-in, another database.
    pub fn with_database(&self, database: &str) -> ChConn {
        ChConn { database: database.to_string(), ..self.clone() }
    }

    /// Run a query that returns a small text result.
    pub fn text(&self, sql: &str) -> Result<String> {
        // ClickHouse refuses a POST without a length: send an empty body.
        let resp = self.request(sql).send_string("").map_err(ch_error)?;
        resp.into_string().map_err(|e| Error::msg(format!("ClickHouse response: {e}")))
    }

    /// `INSERT … FORMAT …` with the rows streamed from `body`.
    pub fn insert(&self, sql: &str, body: impl Read) -> Result<()> {
        self.request(sql).send(body).map_err(ch_error)?;
        Ok(())
    }

    /// A query's result as a stream.
    pub fn stream(&self, sql: &str) -> Result<Box<dyn Read + Send + Sync + 'static>> {
        Ok(self.request(sql).send_string("").map_err(ch_error)?.into_reader())
    }

    pub fn version(&self) -> Result<String> {
        Ok(self.text("SELECT version()")?.trim().to_string())
    }

    fn count(&self, table: &str, filter: &str) -> Result<u64> {
        let sql = format!("SELECT count() FROM {table}{filter}");
        self.text(&sql)?
            .trim()
            .parse()
            .map_err(|e| Error::msg(format!("count {table}: {e}")))
    }

    /// The active monthly partitions of `ts_kv`, oldest first.
    pub fn history_partitions(&self) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT DISTINCT partition FROM system.parts WHERE database = currentDatabase() \
             AND table = '{HISTORY_TABLE}' AND active ORDER BY partition"
        );
        Ok(self.text(&sql)?.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
    }
}

fn ch_error(e: ureq::Error) -> Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            Error::msg(format!("ClickHouse {code}: {}", body.trim()))
        }
        other => Error::msg(format!("ClickHouse unreachable: {other}")),
    }
}

fn partition_filter(partition: &str) -> String {
    format!(" WHERE toYYYYMM(toDateTime64(ts / 1000, 3)) = {partition}")
}

/// Stream `SELECT … FORMAT Native` into a zstd file, hashing the compressed
/// bytes. Returns (sha256, compressed bytes).
fn export(conn: &ChConn, sql: &str, dest: &Path, level: i32) -> Result<(String, u64)> {
    let resp = conn.request(&format!("{sql} FORMAT Native")).send_string("").map_err(ch_error)?;
    let mut reader = resp.into_reader();
    let file = std::fs::File::create(dest)?;
    let mut hw = HashingWriter::new(file);
    {
        let mut enc =
            zstd::stream::Encoder::new(&mut hw, level).map_err(|e| Error::msg(format!("zstd: {e}")))?;
        std::io::copy(&mut reader, &mut enc)?;
        enc.finish().map_err(|e| Error::msg(format!("zstd finish: {e}")))?;
    }
    let (sha, bytes) = hw.finish();
    Ok((sha, bytes))
}

/// A staged export: the manifest entry, its temp file, checksum and size.
pub struct Exported {
    pub entry: ChMember,
    pub temp: std::path::PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

/// Export history (partitions at or after `since_partition`, all when None)
/// and the latest values. `temp_base` names the temp files.
pub fn export_all(
    conn: &ChConn,
    since_partition: Option<&str>,
    temp_base: &Path,
    level: i32,
    log: &dyn Fn(String),
) -> Result<(String, Vec<Exported>)> {
    let version = conn.version()?;
    let mut out = Vec::new();
    for partition in conn.history_partitions()? {
        if since_partition.is_some_and(|since| partition.as_str() < since) {
            continue;
        }
        let filter = partition_filter(&partition);
        let rows = conn.count(HISTORY_TABLE, &filter)?;
        let temp = temp_base.with_extension(format!("ch-{partition}.tmp"));
        let (sha256, bytes) = export(conn, &format!("SELECT * FROM {HISTORY_TABLE}{filter}"), &temp, level)?;
        log(format!("  ClickHouse {HISTORY_TABLE} {partition}: {rows} rows → {bytes} bytes"));
        out.push(Exported {
            entry: ChMember {
                table: HISTORY_TABLE.into(),
                partition: partition.clone(),
                rows,
                member: format!("clickhouse/{HISTORY_TABLE}/{partition}.native.zst"),
            },
            temp,
            sha256,
            bytes,
        });
    }
    let rows = conn.count(LATEST_TABLE, "")?;
    let temp = temp_base.with_extension("ch-latest.tmp");
    let (sha256, bytes) = export(conn, &format!("SELECT * FROM {LATEST_TABLE}"), &temp, level)?;
    log(format!("  ClickHouse {LATEST_TABLE}: {rows} rows → {bytes} bytes"));
    out.push(Exported {
        entry: ChMember {
            table: LATEST_TABLE.into(),
            partition: String::new(),
            rows,
            member: format!("clickhouse/{LATEST_TABLE}.native.zst"),
        },
        temp,
        sha256,
        bytes,
    });
    Ok((version, out))
}

/// A reader that decompresses a member file on the fly (the request body).
struct ZstdFileReader(zstd::stream::read::Decoder<'static, std::io::BufReader<std::fs::File>>);

impl Read for ZstdFileReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

/// Refuse to restore over existing telemetry: like the PostgreSQL side,
/// restore goes into an empty store.
pub fn ensure_empty(conn: &ChConn) -> Result<()> {
    let rows = conn.count(HISTORY_TABLE, "")?;
    if rows > 0 {
        return Err(Error::msg(format!(
            "ClickHouse already holds {rows} telemetry rows in {}.{HISTORY_TABLE}; restore goes into an empty store",
            conn.database
        )));
    }
    Ok(())
}

/// Create `conn.database` with the tables and views of `template` (the live
/// SENTIENT database), so a restore can go beside the live data instead of
/// over it. Plain tables first, then the materialized views that feed them.
pub fn clone_schema(conn: &ChConn, template: &str) -> Result<()> {
    let target = &conn.database;
    if !target.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || target.is_empty() {
        return Err(Error::msg(format!("'{target}' is not a usable ClickHouse database name")));
    }
    let admin = conn.with_database("default");
    let exists = admin.text(&format!("SELECT count() FROM system.databases WHERE name = '{target}'"))?;
    if exists.trim() != "0" {
        return Err(Error::msg(format!("ClickHouse already has a database {target}; choose another name")));
    }
    let tables = admin.text(&format!(
        "SELECT create_table_query FROM system.tables WHERE database = '{template}' \
         AND NOT startsWith(name, '.inner') AND NOT is_temporary \
         ORDER BY engine = 'MaterializedView', name FORMAT TSVRaw"
    ))?;
    admin.text(&format!("CREATE DATABASE {target}"))?;
    for create in tables.lines().filter(|l| !l.trim().is_empty()) {
        let create = create.replace(&format!("{template}."), &format!("{target}."));
        admin.text(&create)?;
    }
    Ok(())
}

/// Insert one member and check the rows arrived. History partitions are
/// counted exactly; the latest-values table is checked to be non-empty when
/// the source was (aggregate rows merge, so their count can shrink).
pub fn import(conn: &ChConn, entry: &ChMember, file: &Path) -> Result<u64> {
    let dec = zstd::stream::read::Decoder::new(std::fs::File::open(file)?)
        .map_err(|e| Error::msg(format!("zstd: {e}")))?;
    conn.request(&format!("INSERT INTO {} FORMAT Native", entry.table))
        .send(ZstdFileReader(dec))
        .map_err(ch_error)?;
    if entry.table == HISTORY_TABLE {
        let got = conn.count(HISTORY_TABLE, &partition_filter(&entry.partition))?;
        if got < entry.rows {
            return Err(Error::msg(format!(
                "ClickHouse {} {}: {got} rows after restore, the backup holds {}",
                entry.table, entry.partition, entry.rows
            )));
        }
        Ok(got)
    } else {
        let got = conn.count(LATEST_TABLE, " FINAL")?;
        if entry.rows > 0 && got == 0 {
            return Err(Error::msg("ClickHouse latest values: nothing arrived".to_string()));
        }
        Ok(got)
    }
}

/// `YYYYMM` of the moment `days` ago, for a "last N days" backup.
pub fn partition_days_ago(days: u32) -> String {
    let t = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    t.format("%Y%m").to_string()
}
