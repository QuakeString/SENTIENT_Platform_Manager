// Copyright © 2016-2026 The SENTIENT Authors

//! Moving telemetry between SENTIENT's stores when an installation changes
//! database type: PostgreSQL's `ts_kv` (a plain table, or a TimescaleDB
//! hypertable) and ClickHouse's `ts_kv`, plus the latest values.
//!
//! The pattern is the same in every direction. SENTIENT is switched to the
//! new store first, so it is down only for a restart; then the history from
//! before the switch (`ts < cutover`) is copied over in calendar-month
//! windows while SENTIENT runs. Each window is skipped when the target
//! already holds it exactly (count and sums match), so an interrupted move
//! picks up where it stopped. The source is never modified: the old data
//! stays where it was until the operator removes it.
//!
//! Rows travel as tab-separated text, which PostgreSQL's `COPY` and
//! ClickHouse's `TabSeparated` agree on (`\N` for NULL, backslash escapes).

use std::io::Read;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio_postgres::{Client, NoTls};

use crate::clickhouse::ChConn;
use crate::db::ConnConfig;
use crate::error::{Error, Result};
use crate::progress::{Progress, ProgressFn};

const COLS: &str = "entity_id, key, ts, bool_v, str_v, long_v, dbl_v, json_v";

/// A ClickHouse insert of PostgreSQL's text rows into `table`. The double
/// column is read as text and converted with `precise_float_parsing`: the
/// input formats' fast parser reads "1.66" as 1.6600000000000001, and that
/// setting does not reach them (24.8), only `toFloat64`.
fn ch_insert(table: &str) -> String {
    format!(
        "INSERT INTO {table} ({COLS}) SELECT entity_id, key, ts, bool_v, str_v, long_v, toFloat64(dbl_v), json_v \
         FROM input('entity_id UUID, key Int32, ts Int64, bool_v Nullable(UInt8), str_v Nullable(String), \
         long_v Nullable(Int64), dbl_v Nullable(String), json_v Nullable(String)') \
         SETTINGS precise_float_parsing = 1 FORMAT TabSeparated"
    )
}

/// SENTIENT's plain `ts_kv` (data/sql/schema-ts-psql.sql).
const PLAIN_TS_KV: &str = "CREATE TABLE IF NOT EXISTS ts_kv (
    entity_id uuid   NOT NULL,
    key       int    NOT NULL,
    ts        bigint NOT NULL,
    bool_v    boolean,
    str_v     varchar(10000000),
    long_v    bigint,
    dbl_v     double precision,
    json_v    json,
    CONSTRAINT ts_kv_pkey PRIMARY KEY (entity_id, key, ts)
)";

/// How PostgreSQL holds a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Missing,
    Plain,
    /// `PARTITION BY RANGE (ts)`, one `ts_kv_YYYY_MM` per month: what
    /// SENTIENT on plain PostgreSQL writes into.
    Partitioned,
    Hypertable,
}

/// What a move did.
#[derive(Debug, Default, Clone)]
pub struct MoveReport {
    pub windows_copied: usize,
    pub windows_skipped: usize,
    pub rows: u64,
}

pub async fn connect(cfg: &ConnConfig) -> Result<Client> {
    let conn_str = format!(
        "host={} port={} dbname={} user={} password={} application_name=sentient-move",
        cfg.host, cfg.port, cfg.dbname, cfg.user, cfg.password
    );
    let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
        .await
        .map_err(|e| Error::Connect(e.to_string()))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!("postgres connection error: {e}");
        }
    });
    Ok(client)
}

fn log(sink: &ProgressFn, line: String) {
    sink(Progress::Log { line });
}

// ---------------------------------------------------------------------------
// PostgreSQL layout
// ---------------------------------------------------------------------------

pub async fn layout(pg: &Client, table: &str) -> Result<Layout> {
    let exists: bool = pg
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&format!("public.{table}")])
        .await?
        .get(0);
    if !exists {
        return Ok(Layout::Missing);
    }
    let partitioned: bool = pg
        .query_one("SELECT relkind = 'p' FROM pg_class WHERE oid = to_regclass($1)", &[&format!("public.{table}")])
        .await?
        .get(0);
    if partitioned {
        return Ok(Layout::Partitioned);
    }
    let timescale: bool = pg
        .query_one("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb')", &[])
        .await?
        .get(0);
    if timescale {
        let hyper: bool = pg
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM timescaledb_information.hypertables \
                 WHERE hypertable_schema = 'public' AND hypertable_name = $1)",
                &[&table],
            )
            .await?
            .get(0);
        if hyper {
            return Ok(Layout::Hypertable);
        }
    }
    Ok(Layout::Plain)
}

async fn has_rows(pg: &Client, table: &str) -> Result<bool> {
    Ok(pg.query_one(&format!("SELECT EXISTS (SELECT 1 FROM {table})"), &[]).await?.get(0))
}

/// Move `ts_kv` aside as `aside` (with its primary key, whose name a new
/// `ts_kv` needs) and stop TimescaleDB's background work on it, so the old
/// copy stays exactly as it was. Returns false when there was nothing to keep:
/// an empty `ts_kv` is dropped instead.
pub async fn set_aside(pg: &Client, aside: &str, sink: &ProgressFn) -> Result<bool> {
    let current = layout(pg, "ts_kv").await?;
    if current == Layout::Missing {
        return Ok(false);
    }
    if !has_rows(pg, "ts_kv").await? {
        pg.batch_execute("DROP TABLE ts_kv").await?;
        log(sink, "  ts_kv was empty; replaced".into());
        return Ok(false);
    }
    if current == Layout::Hypertable {
        pg.batch_execute(
            "SELECT remove_compression_policy('ts_kv', if_exists => true); \
             SELECT remove_retention_policy('ts_kv', if_exists => true);",
        )
        .await?;
    }
    if current == Layout::Partitioned {
        // The monthly partitions keep their names through a rename of the
        // parent; the new ts_kv needs those names.
        let children: Vec<String> = pg
            .query("SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid WHERE i.inhparent = 'public.ts_kv'::regclass", &[])
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        for child in children {
            let renamed = child.replacen("ts_kv", aside, 1);
            pg.batch_execute(&format!(
                "ALTER TABLE {child} RENAME TO {renamed}; ALTER INDEX IF EXISTS {child}_pkey RENAME TO {renamed}_pkey;"
            ))
            .await?;
        }
    }
    pg.batch_execute(&format!(
        "ALTER TABLE ts_kv RENAME TO {aside}; ALTER INDEX IF EXISTS ts_kv_pkey RENAME TO {aside}_pkey;"
    ))
    .await?;
    log(sink, format!("  the existing history is kept as {aside}"));
    Ok(true)
}

/// Create `ts_kv` as SENTIENT's plain table.
pub async fn create_plain(pg: &Client) -> Result<()> {
    pg.batch_execute(PLAIN_TS_KV).await?;
    Ok(())
}

/// Create `ts_kv` partitioned by month, the shape SENTIENT on plain
/// PostgreSQL writes into (it adds `ts_kv_YYYY_MM` as months begin).
pub async fn create_partitioned(pg: &Client) -> Result<()> {
    let ddl = PLAIN_TS_KV.trim_end().strip_suffix(')').unwrap_or(PLAIN_TS_KV);
    pg.batch_execute(&format!("{ddl}) PARTITION BY RANGE (ts)")).await?;
    Ok(())
}

/// The month's partition of a partitioned `ts_kv`, named and bounded as
/// SENTIENT names and bounds it (`ts_kv_YYYY_MM`, UTC month in ms).
async fn ensure_partition(pg: &Client, w: &Window) -> Result<()> {
    let (y, m) = w.label.split_at(4);
    pg.batch_execute(&format!(
        "CREATE TABLE IF NOT EXISTS ts_kv_{y}_{m} PARTITION OF ts_kv FOR VALUES FROM ({}) TO ({})",
        w.start, w.end
    ))
    .await?;
    Ok(())
}

/// Create `ts_kv` as a hypertable with SENTIENT's own TimescaleDB setup
/// (`timescale_sql`, the installed data/sql/schema-ts-timescale.sql): the
/// plain table first, which that file converts while it is still empty.
pub async fn create_hypertable(pg: &Client, timescale_sql: &str) -> Result<()> {
    pg.batch_execute("CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE").await?;
    create_plain(pg).await?;
    pg.batch_execute(timescale_sql).await?;
    if layout(pg, "ts_kv").await? != Layout::Hypertable {
        return Err(Error::msg("ts_kv did not become a hypertable"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// A calendar month (UTC) in epoch milliseconds, `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub label: String,
    pub start: i64,
    pub end: i64,
}

/// The months covering `[min, max]`.
pub fn month_windows(min: i64, max: i64) -> Vec<Window> {
    use chrono::{Datelike, TimeZone, Utc};
    let mut out = Vec::new();
    let Some(first) = Utc.timestamp_millis_opt(min).single() else { return out };
    let (mut y, mut m) = (first.year(), first.month());
    loop {
        let start = Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap().timestamp_millis();
        if start > max {
            break;
        }
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        let end = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).unwrap().timestamp_millis();
        out.push(Window { label: format!("{y}{m:02}"), start, end });
        (y, m) = (ny, nm);
    }
    out
}

fn window_filter(w: &Window, cutover: i64) -> String {
    format!("ts >= {} AND ts < {}", w.start, w.end.min(cutover))
}

/// count, sum(key), sum(ts) of a window: equal on both sides means the window
/// is already there.
type Fingerprint = (i64, i128, i128);

async fn pg_fingerprint(pg: &Client, table: &str, filter: &str) -> Result<Fingerprint> {
    let row = pg
        .query_one(
            &format!(
                "SELECT count(*), coalesce(sum(key), 0)::text, coalesce(sum(ts), 0)::text \
                 FROM {table} WHERE {filter}"
            ),
            &[],
        )
        .await?;
    let parse = |s: String| s.parse::<i128>().map_err(|e| Error::msg(format!("fingerprint: {e}")));
    Ok((row.get(0), parse(row.get(1))?, parse(row.get(2))?))
}

fn ch_fingerprint(ch: &ChConn, filter: &str) -> Result<Fingerprint> {
    let text = ch.text(&format!(
        "SELECT count(), sum(toInt128(key)), sum(toInt128(ts)) FROM ts_kv FINAL WHERE {filter}"
    ))?;
    let parts: Vec<&str> = text.trim().split('\t').collect();
    let num = |i: usize| -> Result<i128> {
        parts
            .get(i)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::msg(format!("ClickHouse fingerprint: {text}")))
    };
    Ok((num(0)? as i64, num(1)?, num(2)?))
}

async fn pg_range(pg: &Client, table: &str, cutover: i64) -> Result<Option<(i64, i64)>> {
    let row = pg
        .query_one(&format!("SELECT min(ts), max(ts) FROM {table} WHERE ts < $1"), &[&cutover])
        .await?;
    Ok(row.get::<_, Option<i64>>(0).zip(row.get::<_, Option<i64>>(1)))
}

fn ch_range(ch: &ChConn, cutover: i64) -> Result<Option<(i64, i64)>> {
    let text = ch.text(&format!("SELECT count(), min(ts), max(ts) FROM ts_kv WHERE ts < {cutover}"))?;
    let parts: Vec<i64> = text.trim().split('\t').filter_map(|s| s.parse().ok()).collect();
    match parts.as_slice() {
        [0, ..] => Ok(None),
        [_, min, max] => Ok(Some((*min, *max))),
        _ => Err(Error::msg(format!("ClickHouse range: {text}"))),
    }
}

// ---------------------------------------------------------------------------
// Streaming between the two
// ---------------------------------------------------------------------------

/// The request body of a ClickHouse insert, fed from the async side.
struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
    buf: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.buf.is_empty() {
            match self.rx.blocking_recv() {
                Some(b) => self.buf = b,
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        self.buf = self.buf.slice(n..);
        Ok(n)
    }
}

/// `COPY (select) TO STDOUT` into `INSERT INTO <table> FORMAT TabSeparated`.
async fn pg_to_ch_stream(pg: &Client, select: &str, ch: &ChConn, insert: &str) -> Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(32);
    let ch = ch.clone();
    let insert = insert.to_string();
    let post = tokio::task::spawn_blocking(move || ch.insert(&insert, ChannelReader { rx, buf: Bytes::new() }));
    let stream = pg.copy_out(&format!("COPY ({select}) TO STDOUT")).await?;
    futures_util::pin_mut!(stream);
    let mut send_failed = false;
    while let Some(chunk) = stream.next().await {
        if tx.send(chunk?).await.is_err() {
            // The insert stopped reading: its error says why.
            send_failed = true;
            break;
        }
    }
    drop(tx);
    post.await.map_err(|e| Error::msg(format!("ClickHouse insert panicked: {e}")))??;
    if send_failed {
        return Err(Error::msg("ClickHouse stopped reading the rows"));
    }
    Ok(())
}

/// `SELECT … FORMAT TabSeparated` into `COPY <table> FROM STDIN`.
async fn ch_to_pg_stream(ch: &ChConn, select: &str, pg: &Client, copy: &str) -> Result<u64> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(32);
    let ch = ch.clone();
    let select = format!("{select} FORMAT TabSeparated");
    let fetch = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut reader = ch.stream(&select)?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                return Ok(());
            }
        }
    });
    let sink = pg.copy_in::<_, Bytes>(copy).await?;
    futures_util::pin_mut!(sink);
    while let Some(chunk) = rx.recv().await {
        sink.send(chunk).await?;
    }
    fetch.await.map_err(|e| Error::msg(format!("ClickHouse read panicked: {e}")))??;
    Ok(sink.finish().await?)
}

/// A session-local staging table shaped like `ts_kv`, emptied.
async fn staging(pg: &Client) -> Result<()> {
    pg.batch_execute(
        "CREATE TEMP TABLE IF NOT EXISTS spm_move (entity_id uuid, key int, ts bigint, bool_v boolean, \
         str_v varchar, long_v bigint, dbl_v double precision, json_v json); TRUNCATE spm_move;",
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// PostgreSQL `table` (ts_kv or a set-aside copy) → ClickHouse `ts_kv`.
pub async fn history_pg_to_ch(
    pg: &Client,
    table: &str,
    ch: &ChConn,
    cutover: i64,
    sink: &ProgressFn,
) -> Result<MoveReport> {
    let mut report = MoveReport::default();
    let Some((min, max)) = pg_range(pg, table, cutover).await? else {
        log(sink, format!("  {table}: no history before the switch"));
        return Ok(report);
    };
    let windows = month_windows(min, max);
    for (i, w) in windows.iter().enumerate() {
        let filter = window_filter(w, cutover);
        let want = pg_fingerprint(pg, table, &filter).await?;
        if want.0 == 0 {
            continue;
        }
        let ch_conn = ch.clone();
        let f = filter.clone();
        let have = tokio::task::spawn_blocking(move || ch_fingerprint(&ch_conn, &f))
            .await
            .map_err(|e| Error::msg(e.to_string()))??;
        if have == want {
            report.windows_skipped += 1;
            log(sink, format!("  {} already copied ({} rows)", w.label, want.0));
            continue;
        }
        let select = format!(
            "SELECT entity_id, key, ts, bool_v::int, str_v, long_v, dbl_v, json_v::text FROM {table} WHERE {filter}"
        );
        pg_to_ch_stream(pg, &select, ch, &ch_insert("ts_kv")).await?;
        let ch_conn = ch.clone();
        let f = filter.clone();
        let got = tokio::task::spawn_blocking(move || ch_fingerprint(&ch_conn, &f))
            .await
            .map_err(|e| Error::msg(e.to_string()))??;
        if got.0 < want.0 {
            return Err(Error::msg(format!(
                "{}: ClickHouse holds {} rows after the copy, PostgreSQL {}",
                w.label, got.0, want.0
            )));
        }
        report.windows_copied += 1;
        report.rows += want.0 as u64;
        log(sink, format!("  {} copied: {} rows", w.label, want.0));
        sink(Progress::Percent { value: (i + 1) as f32 * 100.0 / windows.len() as f32 });
    }
    Ok(report)
}

/// ClickHouse `ts_kv` → PostgreSQL `ts_kv`. Rows already in PostgreSQL win.
pub async fn history_ch_to_pg(pg: &Client, ch: &ChConn, cutover: i64, sink: &ProgressFn) -> Result<MoveReport> {
    let mut report = MoveReport::default();
    let partitioned = layout(pg, "ts_kv").await? == Layout::Partitioned;
    let ch_conn = ch.clone();
    let range = tokio::task::spawn_blocking(move || ch_range(&ch_conn, cutover))
        .await
        .map_err(|e| Error::msg(e.to_string()))??;
    let Some((min, max)) = range else {
        log(sink, "  ClickHouse: no history before the switch".into());
        return Ok(report);
    };
    let windows = month_windows(min, max);
    for (i, w) in windows.iter().enumerate() {
        let filter = window_filter(w, cutover);
        let ch_conn = ch.clone();
        let f = filter.clone();
        let want = tokio::task::spawn_blocking(move || ch_fingerprint(&ch_conn, &f))
            .await
            .map_err(|e| Error::msg(e.to_string()))??;
        if want.0 == 0 {
            continue;
        }
        if pg_fingerprint(pg, "ts_kv", &filter).await? == want {
            report.windows_skipped += 1;
            log(sink, format!("  {} already copied ({} rows)", w.label, want.0));
            continue;
        }
        if partitioned {
            ensure_partition(pg, w).await?;
        }
        staging(pg).await?;
        ch_to_pg_stream(
            ch,
            &format!("SELECT {COLS} FROM ts_kv FINAL WHERE {filter}"),
            pg,
            &format!("COPY spm_move ({COLS}) FROM STDIN"),
        )
        .await?;
        pg.batch_execute(&format!(
            "INSERT INTO ts_kv ({COLS}) SELECT {COLS} FROM spm_move ON CONFLICT DO NOTHING; TRUNCATE spm_move;"
        ))
        .await?;
        let got = pg_fingerprint(pg, "ts_kv", &filter).await?;
        if got.0 < want.0 {
            return Err(Error::msg(format!(
                "{}: PostgreSQL holds {} rows after the copy, ClickHouse {}",
                w.label, got.0, want.0
            )));
        }
        report.windows_copied += 1;
        report.rows += want.0 as u64;
        log(sink, format!("  {} copied: {} rows", w.label, want.0));
        sink(Progress::Percent { value: (i + 1) as f32 * 100.0 / windows.len() as f32 });
    }
    Ok(report)
}

/// A set-aside PostgreSQL copy → the new `ts_kv`, inside PostgreSQL.
pub async fn history_pg_to_pg(pg: &Client, from: &str, cutover: i64, sink: &ProgressFn) -> Result<MoveReport> {
    let mut report = MoveReport::default();
    let partitioned = layout(pg, "ts_kv").await? == Layout::Partitioned;
    let Some((min, max)) = pg_range(pg, from, cutover).await? else {
        log(sink, format!("  {from}: no history before the switch"));
        return Ok(report);
    };
    let windows = month_windows(min, max);
    for (i, w) in windows.iter().enumerate() {
        let filter = window_filter(w, cutover);
        let want = pg_fingerprint(pg, from, &filter).await?;
        if want.0 == 0 {
            continue;
        }
        if pg_fingerprint(pg, "ts_kv", &filter).await? == want {
            report.windows_skipped += 1;
            log(sink, format!("  {} already copied ({} rows)", w.label, want.0));
            continue;
        }
        if partitioned {
            ensure_partition(pg, w).await?;
        }
        pg.batch_execute(&format!(
            "INSERT INTO ts_kv ({COLS}) SELECT {COLS} FROM {from} WHERE {filter} ON CONFLICT DO NOTHING"
        ))
        .await?;
        let got = pg_fingerprint(pg, "ts_kv", &filter).await?;
        if got.0 < want.0 {
            return Err(Error::msg(format!(
                "{}: ts_kv holds {} rows after the copy, {from} {}",
                w.label, got.0, want.0
            )));
        }
        report.windows_copied += 1;
        report.rows += want.0 as u64;
        log(sink, format!("  {} copied: {} rows", w.label, want.0));
        sink(Progress::Percent { value: (i + 1) as f32 * 100.0 / windows.len() as f32 });
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Latest values
// ---------------------------------------------------------------------------

/// PostgreSQL `ts_kv_latest` → ClickHouse, through `ts_kv_latest_writes`
/// (the table whose view feeds the latest aggregate). The aggregate keeps the
/// newest value per key, so values SENTIENT wrote since the switch stay.
pub async fn latest_pg_to_ch(pg: &Client, ch: &ChConn, sink: &ProgressFn) -> Result<u64> {
    let rows: i64 = pg.query_one("SELECT count(*) FROM ts_kv_latest", &[]).await?.get(0);
    if rows == 0 {
        return Ok(0);
    }
    pg_to_ch_stream(
        pg,
        "SELECT entity_id, key, ts, bool_v::int, str_v, long_v, dbl_v, json_v::text FROM ts_kv_latest",
        ch,
        &ch_insert("ts_kv_latest_writes"),
    )
    .await?;
    let ch_conn = ch.clone();
    let keys: i64 = tokio::task::spawn_blocking(move || {
        ch_conn.text("SELECT count() FROM (SELECT entity_id, key FROM ts_kv_latest_agg GROUP BY entity_id, key)")
    })
    .await
    .map_err(|e| Error::msg(e.to_string()))??
    .trim()
    .parse()
    .map_err(|e| Error::msg(format!("latest count: {e}")))?;
    if keys < rows {
        return Err(Error::msg(format!("ClickHouse holds {keys} latest values, PostgreSQL {rows}")));
    }
    log(sink, format!("  latest values: {rows}"));
    Ok(rows as u64)
}

/// ClickHouse's latest aggregate → PostgreSQL `ts_kv_latest`, keeping
/// whichever value is newer per key.
pub async fn latest_ch_to_pg(pg: &Client, ch: &ChConn, sink: &ProgressFn) -> Result<u64> {
    staging(pg).await?;
    let rows = ch_to_pg_stream(
        ch,
        "SELECT entity_id, key, finalizeAggregation(ts_max), finalizeAggregation(bool_v), \
         finalizeAggregation(str_v), finalizeAggregation(long_v), finalizeAggregation(dbl_v), \
         finalizeAggregation(json_v) FROM ts_kv_latest_agg FINAL",
        pg,
        &format!("COPY spm_move ({COLS}) FROM STDIN"),
    )
    .await?;
    pg.batch_execute(&format!(
        "INSERT INTO ts_kv_latest ({COLS}) SELECT {COLS} FROM spm_move \
         ON CONFLICT (entity_id, key) DO UPDATE SET ts = EXCLUDED.ts, bool_v = EXCLUDED.bool_v, \
         str_v = EXCLUDED.str_v, long_v = EXCLUDED.long_v, dbl_v = EXCLUDED.dbl_v, json_v = EXCLUDED.json_v \
         WHERE ts_kv_latest.ts < EXCLUDED.ts; TRUNCATE spm_move;"
    ))
    .await?;
    log(sink, format!("  latest values: {rows}"));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cover_the_range_by_month() {
        // 2026-08-15 .. 2026-10-02 UTC
        let w = month_windows(1_786_752_000_000, 1_790_899_200_000);
        let labels: Vec<_> = w.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["202608", "202609", "202610"]);
        assert!(w.windows(2).all(|p| p[0].end == p[1].start));
        assert!(w[0].start <= 1_786_752_000_000 && w[2].end > 1_790_899_200_000);
    }

    #[test]
    fn december_rolls_into_january() {
        let w = month_windows(1_796_083_200_000, 1_798_761_600_000); // 2026-12-01 .. 2027-01-01
        assert_eq!(w.iter().map(|w| w.label.clone()).collect::<Vec<_>>(), ["202612", "202701"]);
    }
}
