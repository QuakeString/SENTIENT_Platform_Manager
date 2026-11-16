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
    let name = match path.strip_prefix(crate::backup::TELEMETRY_MONTH_PREFIX) {
        Some(r) => r,
        None => path.strip_prefix(&format!("clickhouse/{}/", crate::clickhouse::HISTORY_TABLE))?,
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

/// A forced failure for tests (`SBR_FAIL_AT=<point>`): the rollback paths
/// are run by naming the point where the restore should fail.
fn fail_point(at: &str) -> Result<()> {
    if std::env::var("SBR_FAIL_AT").ok().as_deref() == Some(at) {
        return Err(Error::msg(format!("forced failure at {at} (SBR_FAIL_AT)")));
    }
    Ok(())
}

/// The rows of `public.<table>` in pg_restore's plain output: column name →
/// value (None for NULL), one map per row.
pub fn copy_rows(text: &str, table: &str) -> Vec<std::collections::HashMap<String, Option<String>>> {
    let head = format!("COPY public.{table} (");
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix(&head) else { continue };
        let Some(cols) = rest.split_once(") FROM stdin;").map(|(c, _)| c) else { continue };
        let cols: Vec<String> = cols.split(", ").map(|c| c.trim_matches('"').to_string()).collect();
        for row in lines.by_ref() {
            if row == "\\." {
                break;
            }
            let vals = row.split('\t').map(|v| if v == "\\N" { None } else { Some(unescape_copy(v)) });
            out.push(cols.iter().cloned().zip(vals).collect());
        }
    }
    out
}

/// COPY text escapes: `\\`, `\t`, `\n`, `\r`, `\b`, `\f`, `\v`.
fn unescape_copy(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('v') => out.push('\u{b}'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// The data of some tables in a dump, as pg_restore prints it.
fn dump_data(tools: &crate::pg_tools::PgTools, dump: &Path, tables: &[&str]) -> Result<String> {
    let mut c = crate::pg_tools::command(&tools.pg_restore);
    c.args(["--data-only", "-f", "-"]);
    for t in tables {
        c.args(["-t", t]);
    }
    let out = c.arg(dump).output().map_err(|e| Error::msg(format!("pg_restore: {e}")))?;
    if !out.status.success() {
        return Err(Error::msg(format!("reading the dump failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The backup's key dictionary (name → id), read from its dump without
/// restoring it. `dump` is the decompressed dump (pg_restore's custom format).
pub fn backup_key_dictionary(tools: &crate::pg_tools::PgTools, dump: &Path) -> Result<Vec<(String, i32)>> {
    let text = dump_data(tools, dump, &["key_dictionary"])?;
    let mut dict = Vec::new();
    for row in copy_rows(&text, "key_dictionary") {
        let (Some(Some(name)), Some(Some(id))) = (row.get("key"), row.get("key_id")) else { continue };
        let id: i32 = id.parse().map_err(|_| Error::msg(format!("key_dictionary: bad id {id}")))?;
        dict.push((name.clone(), id));
    }
    Ok(dict)
}

/// Rows `select` (a `COPY … TO STDOUT` query) on `src` into `copy_in` on `dst`.
async fn pg_copy_between(src: &tokio_postgres::Client, copy_out: &str, dst: &tokio_postgres::Client, copy_in: &str) -> Result<u64> {
    use futures_util::{SinkExt, StreamExt};
    let stream = src.copy_out(copy_out).await?;
    futures_util::pin_mut!(stream);
    let sink = dst.copy_in::<_, bytes::Bytes>(copy_in).await?;
    futures_util::pin_mut!(sink);
    while let Some(chunk) = stream.next().await {
        sink.send(chunk?).await?;
    }
    Ok(sink.finish().await?)
}

/// Cancel what `pg` runs once the guard trips (a long INSERT in a
/// transaction cannot otherwise be stopped between rows). Dropping the
/// returned handle's task stops watching.
fn cancel_on_trip(pg: &tokio_postgres::Client, guard: &crate::space::WatchGuard) -> tokio::task::JoinHandle<()> {
    let token = pg.cancel_token();
    let guard = guard.clone();
    tokio::spawn(async move {
        loop {
            if guard.tripped().is_some() {
                let _ = token.cancel_query(tokio_postgres::NoTls).await;
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    })
}

async fn min_max(pg: &tokio_postgres::Client, table: &str) -> Result<Option<(i64, i64)>> {
    let row = pg.query_one(&format!("SELECT min(ts), max(ts) FROM {table}"), &[]).await?;
    Ok(row.get::<_, Option<i64>>(0).zip(row.get::<_, Option<i64>>(1)))
}

async fn table_exists(pg: &tokio_postgres::Client, table: &str) -> Result<bool> {
    Ok(pg.query_one("SELECT to_regclass($1) IS NOT NULL", &[&format!("public.{table}")]).await?.get(0))
}

/// Telemetry only and a date range: the backup's telemetry into the running
/// database `cfg` (and the running ClickHouse, `opts.clickhouse`). Returns
/// the rows the backup brought.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn into_live(
    cfg: &ConnConfig,
    opts: &crate::restore::RestoreOptions,
    manifest: &Manifest,
    members: &std::collections::HashMap<String, std::path::PathBuf>,
    tools: &crate::pg_tools::PgTools,
    dump: &Path,
    guard: &crate::space::WatchGuard,
    sink: &ProgressFn,
    steps: &mut crate::progress::Steps,
) -> Result<u64> {
    use crate::restore::STAGING;
    use crate::telemetry_move as mv;
    let _ = manifest;
    steps.step("Matching the backup's key names to this database");
    let dict = {
        let (t, d) = (tools.clone(), dump.to_path_buf());
        tokio::task::spawn_blocking(move || backup_key_dictionary(&t, &d))
            .await
            .map_err(|e| Error::msg(format!("reading the key names panicked: {e}")))??
    };
    let pg = mv::connect(cfg).await?;
    let names: Vec<String> = dict.iter().map(|(n, _)| n.clone()).collect();
    let added = pg
        .execute("INSERT INTO key_dictionary (key) SELECT unnest($1::varchar[]) ON CONFLICT DO NOTHING", &[&names])
        .await?;
    let live: std::collections::HashMap<String, i32> = pg
        .query("SELECT key::text, key_id FROM key_dictionary WHERE key = ANY($1::varchar[])", &[&names])
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let (olds, news): (Vec<i32>, Vec<i32>) = dict.iter().filter_map(|(n, old)| live.get(n).map(|new| (*old, *new))).unzip();
    sink(crate::progress::Progress::Log { line: format!("  {} key names, {added} new here", dict.len()) });

    steps.step("Reading the backup's telemetry");
    pg.batch_execute(&format!(
        "DROP TABLE IF EXISTS {STAGING}; CREATE UNLOGGED TABLE {STAGING} (entity_id uuid, key int, ts bigint, \
         bool_v boolean, str_v varchar, long_v bigint, dbl_v double precision, json_v json)"
    ))
    .await?;
    let result = async {
        let db = crate::db::DbInspector::connect(cfg).await?;
        let mut loaded = 0u64;
        for tmp in crate::restore::telemetry_members(members) {
            guard.check()?;
            loaded += db.copy_in_compressed(&format!("COPY {STAGING} FROM STDIN WITH (FORMAT binary)"), tmp).await?;
        }
        if members.keys().any(|m| m.starts_with(&format!("clickhouse/{}/", crate::clickhouse::HISTORY_TABLE))) {
            loaded = crate::restore::load_clickhouse_history(&pg, members, sink).await?;
        }
        guard.check()?;
        if let Scope::Range { from_ms, to_ms } = opts.scope {
            pg.execute(&format!("DELETE FROM {STAGING} WHERE ts < $1 OR ts >= $2"), &[&from_ms, &to_ms]).await?;
        }
        pg.batch_execute("DROP TABLE IF EXISTS spm_keymap; CREATE TEMP TABLE spm_keymap (old int PRIMARY KEY, new int)").await?;
        pg.execute("INSERT INTO spm_keymap SELECT unnest($1::int[]), unnest($2::int[])", &[&olds, &news]).await?;
        let unknown = pg
            .execute(&format!("DELETE FROM {STAGING} s WHERE NOT EXISTS (SELECT 1 FROM spm_keymap m WHERE m.old = s.key)"), &[])
            .await?;
        if unknown > 0 {
            sink(crate::progress::Progress::Log { line: format!("  {unknown} rows left out: their key is not in the backup's key names") });
        }
        pg.execute(&format!("UPDATE {STAGING} s SET key = m.new FROM spm_keymap m WHERE s.key = m.old AND m.old <> m.new"), &[]).await?;
        let rows: i64 = pg.query_one(&format!("SELECT count(*) FROM {STAGING}"), &[]).await?.get(0);
        sink(crate::progress::Progress::Log { line: format!("  {loaded} rows read, {rows} to restore") });
        match &opts.clickhouse {
            None => into_live_pg(&pg, &opts.scope, guard, sink, steps).await,
            Some(ch) => into_live_ch(&pg, ch, &opts.scope, guard, sink, steps).await,
        }
    }
    .await;
    let _ = pg.batch_execute(&format!("DROP TABLE IF EXISTS {STAGING}; DROP TABLE IF EXISTS spm_keymap")).await;
    result
}

/// PostgreSQL: one transaction, so a failure leaves the database as it was.
async fn into_live_pg(pg: &tokio_postgres::Client, scope: &Scope, guard: &crate::space::WatchGuard, sink: &ProgressFn, steps: &mut crate::progress::Steps) -> Result<u64> {
    use crate::restore::STAGING;
    use crate::telemetry_move::{self as mv, COLS};
    let layout = mv::layout(pg, "ts_kv").await?;
    if layout == mv::Layout::Missing {
        return Err(Error::msg("this database has no ts_kv to restore telemetry into"));
    }
    if layout == mv::Layout::Partitioned {
        if let Some((min, max)) = min_max(pg, STAGING).await? {
            for w in mv::month_windows(min, max) {
                mv::ensure_partition(pg, &w).await?;
            }
        }
    }
    steps.step(match scope {
        Scope::Range { .. } => "Replacing the telemetry of those dates",
        _ => "Adding the telemetry (rows already here are kept)",
    });
    guard.check()?;
    fail_point("pg_before_begin")?;
    let canceller = cancel_on_trip(pg, guard);
    pg.batch_execute("BEGIN").await?;
    let work = async {
        let mut deleted = 0;
        if let Scope::Range { from_ms, to_ms } = scope {
            // The series the range touches: those it deletes from and those it brings.
            pg.execute(
                &format!(
                    "CREATE TEMP TABLE spm_affected ON COMMIT DROP AS SELECT DISTINCT entity_id, key FROM ts_kv WHERE ts >= $1 AND ts < $2 \
                     UNION SELECT DISTINCT entity_id, key FROM {STAGING}"
                ),
                &[from_ms, to_ms],
            )
            .await?;
            deleted = pg.execute("DELETE FROM ts_kv WHERE ts >= $1 AND ts < $2", &[from_ms, to_ms]).await?;
        }
        let inserted = pg
            .execute(&format!("INSERT INTO ts_kv ({COLS}) SELECT {COLS} FROM {STAGING} ON CONFLICT DO NOTHING"), &[])
            .await?;
        if table_exists(pg, "ts_kv_latest").await? {
            if matches!(scope, Scope::Range { .. }) {
                recompute_latest_pg(pg, "spm_affected", sink).await?;
            } else {
                // Rows only added: the newer one wins, as SENTIENT writes latest values.
                mv::latest_from_table(pg, STAGING, sink).await?;
            }
        }
        fail_point("pg_before_commit")?;
        guard.check()?;
        Ok::<_, Error>((deleted, inserted))
    }
    .await;
    canceller.abort();
    match work {
        Ok((deleted, inserted)) => {
            pg.batch_execute("COMMIT").await?;
            if deleted > 0 {
                sink(crate::progress::Progress::Log { line: format!("  {deleted} rows of those dates replaced") });
            }
            sink(crate::progress::Progress::Log { line: format!("  {inserted} rows added") });
            Ok(inserted)
        }
        Err(e) => {
            let _ = pg.batch_execute("ROLLBACK").await;
            sink(crate::progress::Progress::Log { line: "  rolled back: the database is as it was".into() });
            Err(guard.tripped().map(Error::SpaceLow).unwrap_or(e))
        }
    }
}

/// PostgreSQL's latest values of the series in `affected` (entity_id,
/// key) made again from the history, as SENTIENT does when history is
/// deleted (`delete_ts_range`): the newest row of each series becomes its
/// latest value, and a series with no history left loses its latest value.
pub(crate) async fn recompute_latest_pg(pg: &tokio_postgres::Client, affected: &str, sink: &ProgressFn) -> Result<u64> {
    use crate::telemetry_move::COLS;
    let n = pg
        .execute(
            &format!(
                "INSERT INTO ts_kv_latest ({COLS}) SELECT a.entity_id, a.key, h.ts, h.bool_v, h.str_v, h.long_v, h.dbl_v, h.json_v \
                 FROM {affected} a CROSS JOIN LATERAL (SELECT ts, bool_v, str_v, long_v, dbl_v, json_v FROM ts_kv t \
                 WHERE t.entity_id = a.entity_id AND t.key = a.key ORDER BY t.ts DESC LIMIT 1) h \
                 ON CONFLICT (entity_id, key) DO UPDATE SET ts = EXCLUDED.ts, bool_v = EXCLUDED.bool_v, str_v = EXCLUDED.str_v, \
                 long_v = EXCLUDED.long_v, dbl_v = EXCLUDED.dbl_v, json_v = EXCLUDED.json_v"
            ),
            &[],
        )
        .await?;
    let gone = pg
        .execute(
            &format!(
                "DELETE FROM ts_kv_latest l USING {affected} a WHERE l.entity_id = a.entity_id AND l.key = a.key \
                 AND NOT EXISTS (SELECT 1 FROM ts_kv t WHERE t.entity_id = a.entity_id AND t.key = a.key)"
            ),
            &[],
        )
        .await?;
    sink(crate::progress::Progress::Log { line: format!("  latest values made again from the history: {n}, {gone} without history removed") });
    Ok(n)
}

/// ClickHouse's latest values of the series in `spm_affected` (a table in
/// the same ClickHouse database) made again from its history, as SENTIENT's
/// view makes them (the newest `ts`, then the newest write); and, on a
/// ClickHouse install whose PostgreSQL still holds `ts_kv_latest` rows for
/// those series, those rows made the same.
async fn recompute_latest_ch(pg: &tokio_postgres::Client, ch: &crate::clickhouse::ChConn, sink: &ProgressFn) -> Result<()> {
    let c = ch.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let s = "SETTINGS mutations_sync = 2, allow_nondeterministic_mutations = 1";
        c.text(&format!("ALTER TABLE ts_kv_latest_agg DELETE WHERE (entity_id, key) IN (SELECT entity_id, key FROM spm_affected) {s}"))?;
        c.text(
            "INSERT INTO ts_kv_latest_agg (entity_id, key, ts_max, bool_v, str_v, long_v, dbl_v, json_v) \
             SELECT entity_id, key, maxState(ts), \
             argMaxState(bool_v, (ts, toDateTime64(inserted_at, 9))), argMaxState(str_v, (ts, toDateTime64(inserted_at, 9))), \
             argMaxState(long_v, (ts, toDateTime64(inserted_at, 9))), argMaxState(dbl_v, (ts, toDateTime64(inserted_at, 9))), \
             argMaxState(json_v, (ts, toDateTime64(inserted_at, 9))) \
             FROM ts_kv WHERE (entity_id, key) IN (SELECT entity_id, key FROM spm_affected) GROUP BY entity_id, key",
        )?;
        Ok(())
    })
    .await
    .map_err(|e| Error::msg(e.to_string()))??;
    sink(crate::progress::Progress::Log { line: "  ClickHouse's latest values made again from its history".into() });
    if table_exists(pg, "ts_kv_latest").await? && pg.query_one("SELECT EXISTS (SELECT 1 FROM ts_kv_latest)", &[]).await?.get::<_, bool>(0) {
        use crate::telemetry_move::{COLS, ch_to_pg_stream};
        pg.batch_execute(
            "DROP TABLE IF EXISTS spm_ch_affected; CREATE TEMP TABLE spm_ch_affected (entity_id uuid, key int); \
             DROP TABLE IF EXISTS spm_ch_latest; CREATE TEMP TABLE spm_ch_latest (entity_id uuid, key int, ts bigint, bool_v boolean, \
             str_v varchar, long_v bigint, dbl_v double precision, json_v json)",
        )
        .await?;
        ch_to_pg_stream(ch, "SELECT entity_id, key FROM spm_affected", pg, "COPY spm_ch_affected FROM STDIN").await?;
        ch_to_pg_stream(
            ch,
            "SELECT entity_id, key, max(ts), argMax(bool_v, (ts, inserted_at)), argMax(str_v, (ts, inserted_at)), argMax(long_v, (ts, inserted_at)), \
             argMax(dbl_v, (ts, inserted_at)), argMax(json_v, (ts, inserted_at)) FROM ts_kv \
             WHERE (entity_id, key) IN (SELECT entity_id, key FROM spm_affected) GROUP BY entity_id, key",
            pg,
            &format!("COPY spm_ch_latest ({COLS}) FROM STDIN"),
        )
        .await?;
        // Only the series PostgreSQL holds: SENTIENT keeps latest values in ClickHouse here.
        let n = pg
            .execute(
                "UPDATE ts_kv_latest l SET ts = n.ts, bool_v = n.bool_v, str_v = n.str_v, long_v = n.long_v, dbl_v = n.dbl_v, json_v = n.json_v \
                 FROM spm_ch_latest n WHERE l.entity_id = n.entity_id AND l.key = n.key",
                &[],
            )
            .await?;
        let gone = pg
            .execute(
                "DELETE FROM ts_kv_latest l USING spm_ch_affected a WHERE l.entity_id = a.entity_id AND l.key = a.key \
                 AND NOT EXISTS (SELECT 1 FROM spm_ch_latest n WHERE n.entity_id = a.entity_id AND n.key = a.key)",
                &[],
            )
            .await?;
        pg.batch_execute("DROP TABLE spm_ch_affected; DROP TABLE spm_ch_latest").await?;
        sink(crate::progress::Progress::Log { line: format!("  PostgreSQL's latest values: {n} made again, {gone} removed") });
    }
    Ok(())
}

fn now_nanos() -> i64 {
    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
}

/// ClickHouse. Telemetry only: rows go in with an early `inserted_at` (one
/// of their own), so a row already here for the same entity, key and time
/// wins when the table merges; a failure removes exactly those rows. A range:
/// rows go to a table of their own first; only when all arrived are the
/// live rows of the range deleted and the new ones moved in.
async fn into_live_ch(
    pg: &tokio_postgres::Client,
    ch: &crate::clickhouse::ChConn,
    scope: &Scope,
    guard: &crate::space::WatchGuard,
    sink: &ProgressFn,
    steps: &mut crate::progress::Steps,
) -> Result<u64> {
    use crate::restore::STAGING;
    use crate::telemetry_move as mv;
    let Some((min, max)) = min_max(pg, STAGING).await? else {
        sink(crate::progress::Progress::Log { line: "  nothing to restore".into() });
        return Ok(0);
    };
    let range = matches!(scope, Scope::Range { .. });
    // 1970-01-01 00:00:01.xxx: older than anything SENTIENT writes, unique to this run.
    let marker = 1_000_000_000 + now_nanos().rem_euclid(1_000_000_000);
    let table = if range { "spm_restore_range" } else { crate::clickhouse::HISTORY_TABLE };
    let ch_blocking = |sql: String| {
        let c = ch.clone();
        async move { tokio::task::spawn_blocking(move || c.text(&sql)).await.map_err(|e| Error::msg(e.to_string()))? }
    };
    if range {
        ch_blocking("DROP TABLE IF EXISTS spm_restore_range".into()).await?;
        ch_blocking("CREATE TABLE spm_restore_range AS ts_kv ENGINE = MergeTree ORDER BY (entity_id, key, ts)".into()).await?;
    }
    // The series this run touches, for their latest values after it (done or undone).
    ch_blocking("DROP TABLE IF EXISTS spm_affected".into()).await?;
    ch_blocking("CREATE TABLE spm_affected (entity_id UUID, key Int32) ENGINE = Memory".into()).await?;
    mv::pg_to_ch_stream(pg, &format!("SELECT DISTINCT entity_id, key FROM {STAGING}"), ch, "INSERT INTO spm_affected (entity_id, key) FORMAT TabSeparated").await?;
    if let Scope::Range { from_ms, to_ms } = scope {
        ch_blocking(format!("INSERT INTO spm_affected SELECT DISTINCT entity_id, key FROM ts_kv WHERE ts >= {from_ms} AND ts < {to_ms}")).await?;
    }
    let mut touched = false;
    let insert = format!(
        "INSERT INTO {table} (entity_id, key, ts, bool_v, str_v, long_v, dbl_v, json_v, inserted_at) \
         SELECT entity_id, key, ts, bool_v, str_v, long_v, toFloat64(dbl_v), json_v, fromUnixTimestamp64Nano(toInt64({marker})) \
         FROM input('entity_id UUID, key Int32, ts Int64, bool_v Nullable(UInt8), str_v Nullable(String), \
         long_v Nullable(Int64), dbl_v Nullable(String), json_v Nullable(String)') \
         SETTINGS precise_float_parsing = 1 FORMAT TabSeparated"
    );
    steps.step(if range { "Replacing the telemetry of those dates in ClickHouse" } else { "Adding the telemetry to ClickHouse (rows already here are kept)" });
    let mut inserted_any = false;
    let mut rows = 0u64;
    let work = async {
        let windows = mv::month_windows(min, max);
        for (i, w) in windows.iter().enumerate() {
            guard.check()?;
            let n: i64 = pg
                .query_one(&format!("SELECT count(*) FROM {STAGING} WHERE ts >= $1 AND ts < $2"), &[&w.start, &w.end])
                .await?
                .get(0);
            if n == 0 {
                continue;
            }
            inserted_any = true;
            let select = format!(
                "SELECT entity_id, key, ts, bool_v::int, str_v, long_v, dbl_v, json_v::text FROM {STAGING} WHERE ts >= {} AND ts < {}",
                w.start, w.end
            );
            mv::pg_to_ch_stream(pg, &select, ch, &insert).await?;
            rows += n as u64;
            sink(crate::progress::Progress::Log { line: format!("  {}: {n} rows", w.label) });
            sink(crate::progress::Progress::Percent { value: (i + 1) as f32 / windows.len() as f32 });
            fail_point("ch_mid")?;
        }
        fail_point("ch_after_insert")?;
        guard.check()?;
        if let Scope::Range { from_ms, to_ms } = scope {
            touched = true;
            ch_blocking(format!("ALTER TABLE ts_kv DELETE WHERE ts >= {from_ms} AND ts < {to_ms} SETTINGS mutations_sync = 2")).await?;
            ch_blocking("INSERT INTO ts_kv SELECT * FROM spm_restore_range".into()).await?;
        }
        Ok::<_, Error>(())
    }
    .await;
    if range {
        let _ = ch_blocking("DROP TABLE IF EXISTS spm_restore_range".into()).await;
    }
    let result = match work {
        Ok(()) => Ok(rows),
        Err(e) => {
            if inserted_any && !range {
                let undo = ch_blocking(format!(
                    "ALTER TABLE ts_kv DELETE WHERE inserted_at = fromUnixTimestamp64Nano(toInt64({marker})) SETTINGS mutations_sync = 2"
                ))
                .await;
                sink(crate::progress::Progress::Log {
                    line: match undo {
                        Ok(_) => "  rolled back: the rows this restore added to ClickHouse were removed".into(),
                        Err(u) => format!("  the rows this restore added could not be removed ({u}); they carry inserted_at {marker} ns"),
                    },
                });
            } else {
                sink(crate::progress::Progress::Log { line: "  stopped before ClickHouse's telemetry was changed".into() });
            }
            Err(guard.tripped().map(Error::SpaceLow).unwrap_or(e))
        }
    };
    // Latest values follow the history whatever happened: rows added (their
    // view already fed the aggregate), a range replaced, or rows taken out
    // again after a failure (the aggregate kept their states).
    // Telemetry only that went through needs nothing more in ClickHouse: the
    // view kept the newest per series. PostgreSQL's leftover rows on such an
    // install take a newer value as SENTIENT's own writes would.
    if result.is_ok() && !touched && inserted_any && table_exists(pg, "ts_kv_latest").await.unwrap_or(false) {
        let n = pg
            .execute(
                &format!(
                    "UPDATE ts_kv_latest l SET ts = n.ts, bool_v = n.bool_v, str_v = n.str_v, long_v = n.long_v, dbl_v = n.dbl_v, json_v = n.json_v \
                     FROM (SELECT DISTINCT ON (entity_id, key) * FROM {STAGING} ORDER BY entity_id, key, ts DESC) n \
                     WHERE l.entity_id = n.entity_id AND l.key = n.key AND n.ts > l.ts"
                ),
                &[],
            )
            .await;
        if let Ok(n) = n {
            sink(crate::progress::Progress::Log { line: format!("  PostgreSQL's latest values: {n} newer from the backup") });
        }
    } else if (inserted_any && result.is_err()) || touched {
        if let Err(e) = recompute_latest_ch(pg, ch, sink).await {
            sink(crate::progress::Progress::Log { line: format!("  the latest values could not be made again ({e}); they follow with the next value of each series") });
            if result.is_ok() {
                let _ = ch_blocking("DROP TABLE IF EXISTS spm_affected".into()).await;
                return Err(e);
            }
        }
    }
    let _ = ch_blocking("DROP TABLE IF EXISTS spm_affected".into()).await;
    result
}

/// Configuration only, Keep: renumber the restored database's key
/// dictionary (and `attribute_kv`, `ts_kv_latest`) to the live one's ids,
/// adding the live names it lacks, then copy the live `ts_kv` (month by
/// month, checked) and `ts_kv_latest` into it. Returns the history rows
/// copied. A ClickHouse installation keeps its history where it is (the
/// switch does not exchange it), so only the dictionary and the latest
/// values are carried.
pub async fn carry_telemetry(live: &ConnConfig, restored: &ConnConfig, watch: Option<&crate::space::WatchGuard>, sink: &ProgressFn) -> Result<u64> {
    use crate::telemetry_move::{self as mv, COLS};
    let none = crate::space::WatchGuard::none();
    let guard = watch.unwrap_or(&none);
    let log = |line: String| sink(crate::progress::Progress::Log { line });
    let lp = mv::connect(live).await?;
    let rp = mv::connect(restored).await?;
    let dict = |rows: Vec<tokio_postgres::Row>| rows.iter().map(|r| (r.get::<_, String>(0), r.get::<_, i32>(1))).collect::<Vec<_>>();
    let live_dict = dict(lp.query("SELECT key::text, key_id FROM key_dictionary", &[]).await?);
    let rest_dict = dict(rp.query("SELECT key::text, key_id FROM key_dictionary", &[]).await?);
    let (olds, news, add_names, add_ids) = renumbering(&live_dict, &rest_dict);
    rp.batch_execute("BEGIN").await?;
    let renumber = async {
        rp.batch_execute("CREATE TEMP TABLE spm_km (old int PRIMARY KEY, new int) ON COMMIT DROP").await?;
        rp.execute("INSERT INTO spm_km SELECT unnest($1::int[]), unnest($2::int[])", &[&olds, &news]).await?;
        for (t, c) in [("key_dictionary", "key_id"), ("attribute_kv", "attribute_key"), ("ts_kv_latest", "key"), ("ts_kv", "key")] {
            // An empty table is left alone: a restored hypertable may hold
            // empty compressed chunks, which refuse updates of `key`.
            if !table_exists(&rp, t).await? || !rp.query_one(&format!("SELECT EXISTS (SELECT 1 FROM {t})"), &[]).await?.get::<_, bool>(0) {
                continue;
            }
            rp.execute(&format!("UPDATE {t} SET {c} = -m.new FROM spm_km m WHERE {t}.{c} = m.old"), &[]).await?;
            rp.execute(&format!("UPDATE {t} SET {c} = -{c} WHERE {c} < 0"), &[]).await?;
        }
        rp.execute("INSERT INTO key_dictionary (key, key_id) SELECT unnest($1::varchar[]), unnest($2::int[])", &[&add_names, &add_ids]).await?;
        rp.batch_execute("SELECT setval(pg_get_serial_sequence('key_dictionary', 'key_id'), GREATEST((SELECT max(key_id) FROM key_dictionary), 1))").await?;
        fail_point("carry_renumber")?;
        Ok::<_, Error>(())
    }
    .await;
    match renumber {
        Ok(()) => rp.batch_execute("COMMIT").await?,
        Err(e) => {
            let _ = rp.batch_execute("ROLLBACK").await;
            return Err(e);
        }
    }
    log(format!("  key names matched to this server's: {} renumbered, {} added", olds.len(), add_names.len()));

    // History, month by month; a month already there is skipped.
    let mut copied = 0u64;
    let restored_layout = mv::layout(&rp, "ts_kv").await?;
    if let Some((min, max)) = min_max(&lp, "ts_kv").await? {
        rp.batch_execute(
            "CREATE TEMP TABLE IF NOT EXISTS spm_carry (entity_id uuid, key int, ts bigint, bool_v boolean, \
             str_v varchar, long_v bigint, dbl_v double precision, json_v json)",
        )
        .await?;
        for w in mv::month_windows(min, max) {
            guard.check()?;
            let filter = format!("ts >= {} AND ts < {}", w.start, w.end);
            let want = mv::pg_fingerprint(&lp, "ts_kv", &filter).await?;
            if want.0 == 0 {
                continue;
            }
            if mv::pg_fingerprint(&rp, "ts_kv", &filter).await? == want {
                log(format!("  {} already copied ({} rows)", w.label, want.0));
                continue;
            }
            if restored_layout == mv::Layout::Partitioned {
                mv::ensure_partition(&rp, &w).await?;
            }
            rp.batch_execute("TRUNCATE spm_carry").await?;
            pg_copy_between(
                &lp,
                &format!("COPY (SELECT {COLS} FROM ts_kv WHERE {filter}) TO STDOUT WITH (FORMAT binary)"),
                &rp,
                &format!("COPY spm_carry ({COLS}) FROM STDIN WITH (FORMAT binary)"),
            )
            .await?;
            rp.execute(&format!("INSERT INTO ts_kv ({COLS}) SELECT {COLS} FROM spm_carry ON CONFLICT DO NOTHING"), &[]).await?;
            let got = mv::pg_fingerprint(&rp, "ts_kv", &filter).await?;
            if got.0 < want.0 {
                return Err(Error::msg(format!("{}: the restored database holds {} rows after the copy, this server {}", w.label, got.0, want.0)));
            }
            copied += want.0 as u64;
            log(format!("  {} copied: {} rows", w.label, want.0));
        }
    }
    guard.check()?;
    // The latest values: this server's, as they go with its history.
    if table_exists(&lp, "ts_kv_latest").await? && table_exists(&rp, "ts_kv_latest").await? {
        rp.batch_execute(
            "CREATE TEMP TABLE IF NOT EXISTS spm_carry_latest (entity_id uuid, key int, ts bigint, bool_v boolean, \
             str_v varchar, long_v bigint, dbl_v double precision, json_v json); TRUNCATE spm_carry_latest",
        )
        .await?;
        let n = pg_copy_between(
            &lp,
            &format!("COPY (SELECT {COLS} FROM ts_kv_latest) TO STDOUT WITH (FORMAT binary)"),
            &rp,
            &format!("COPY spm_carry_latest ({COLS}) FROM STDIN WITH (FORMAT binary)"),
        )
        .await?;
        rp.execute(
            &format!(
                "INSERT INTO ts_kv_latest ({COLS}) SELECT {COLS} FROM spm_carry_latest \
                 ON CONFLICT (entity_id, key) DO UPDATE SET ts = EXCLUDED.ts, bool_v = EXCLUDED.bool_v, \
                 str_v = EXCLUDED.str_v, long_v = EXCLUDED.long_v, dbl_v = EXCLUDED.dbl_v, json_v = EXCLUDED.json_v \
                 WHERE ts_kv_latest.ts <= EXCLUDED.ts"
            ),
            &[],
        )
        .await?;
        log(format!("  latest values: {n}"));
    }
    Ok(copied)
}

/// The restored dictionary renumbered to the live one: (old ids, new ids)
/// of the restored names whose id changes, and the live names (with their
/// ids) the restored dictionary lacks. Restored-only names get ids above
/// every id on either side.
fn renumbering(live: &[(String, i32)], restored: &[(String, i32)]) -> (Vec<i32>, Vec<i32>, Vec<String>, Vec<i32>) {
    let live_by: std::collections::HashMap<&str, i32> = live.iter().map(|(n, i)| (n.as_str(), *i)).collect();
    let rest_names: std::collections::HashSet<&str> = restored.iter().map(|(n, _)| n.as_str()).collect();
    let mut next = live.iter().chain(restored).map(|(_, i)| *i).max().unwrap_or(0);
    let (mut olds, mut news) = (Vec::new(), Vec::new());
    for (name, old) in restored {
        let new = match live_by.get(name.as_str()) {
            Some(id) => *id,
            None => {
                next += 1;
                next
            }
        };
        if new != *old {
            olds.push(*old);
            news.push(new);
        }
    }
    let (add_names, add_ids) = live.iter().filter(|(n, _)| !rest_names.contains(n.as_str())).map(|(n, i)| (n.clone(), *i)).unzip();
    (olds, news, add_names, add_ids)
}

/// Configuration only: the entities with telemetry on the running instance
/// that the backup does not have (their telemetry stays without a device
/// with Keep). Their names, up to `limit`, and how many in all.
pub async fn orphans(live: &ConnConfig, input: &Path, passphrase: Option<&str>, temp_dir: Option<&Path>, limit: usize) -> Result<(Vec<String>, usize)> {
    const ENTITY_TABLES: [&str; 6] = ["device", "asset", "entity_view", "customer", "tenant", "edge"];
    let (input, pw, td) = (input.to_path_buf(), passphrase.map(str::to_string), temp_dir.map(Path::to_path_buf));
    let known: std::collections::HashSet<String> = tokio::task::spawn_blocking(move || -> Result<std::collections::HashSet<String>> {
        let only = |_: &Manifest| vec![crate::backup::DUMP_MEMBER.to_string()];
        let (_, members) = crate::restore::read_archive(&input, pw.as_deref(), td.as_deref(), Some(&only), &crate::space::WatchGuard::none())?;
        let dump = members.get(crate::backup::DUMP_MEMBER).cloned().ok_or_else(|| Error::msg("backup has no database dump"))?;
        let tools = crate::pg_tools::PgTools::resolve();
        let result = tools.and_then(|tools| {
            let (archive, _) = crate::restore::unpack_dump(&tools, &dump)?;
            let text = dump_data(&tools, &archive, &ENTITY_TABLES);
            let _ = std::fs::remove_file(&archive);
            let text = text?;
            Ok(ENTITY_TABLES.iter().flat_map(|t| copy_rows(&text, t)).filter_map(|r| r.get("id").cloned().flatten()).collect())
        });
        for p in members.values() {
            let _ = std::fs::remove_file(p);
        }
        result
    })
    .await
    .map_err(|e| Error::msg(format!("reading the backup panicked: {e}")))??;
    let pg = crate::telemetry_move::connect(live).await?;
    if !table_exists(&pg, "ts_kv_latest").await? {
        return Ok((Vec::new(), 0));
    }
    let with_telemetry: Vec<String> = pg.query("SELECT DISTINCT entity_id::text FROM ts_kv_latest", &[]).await?.iter().map(|r| r.get(0)).collect();
    let mut missing: Vec<String> = with_telemetry.into_iter().filter(|id| !known.contains(id)).collect();
    missing.sort();
    let mut names: std::collections::HashMap<String, String> = Default::default();
    for t in ["device", "asset"] {
        if table_exists(&pg, t).await? {
            for r in pg.query(&format!("SELECT id::text, name::text FROM {t} WHERE id::text = ANY($1)"), &[&missing]).await? {
                names.insert(r.get(0), r.get(1));
            }
        }
    }
    // Only devices and assets: SENTIENT's own system telemetry (a node's
    // id, different on every server) is no one's to lose.
    let mut out: Vec<String> = missing.iter().filter_map(|id| names.get(id).cloned()).collect();
    out.sort();
    let total = out.len();
    out.truncate(limit);
    Ok((out, total))
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
    fn copy_text_parses_with_escapes_and_nulls() {
        let text = "SET x;\nCOPY public.key_dictionary (key, key_id) FROM stdin;\ntemp\t1\nwith\\ttab\t2\n\\.\nCOPY public.device (id, name) FROM stdin;\nabc\t\\N\n\\.\n";
        let rows = copy_rows(text, "key_dictionary");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["key"].as_deref(), Some("with\ttab"));
        assert_eq!(copy_rows(text, "device")[0]["name"], None);
    }

    #[test]
    fn renumbering_takes_live_ids_and_never_clashes() {
        let s = |v: &[(&str, i32)]| v.iter().map(|(n, i)| (n.to_string(), *i)).collect::<Vec<_>>();
        let live = s(&[("volt", 1), ("temp", 2), ("hum", 3)]);
        let rest = s(&[("temp", 1), ("hum", 2), ("press", 3)]);
        let (olds, news, add, add_ids) = renumbering(&live, &rest);
        assert_eq!(olds, [1, 2, 3]);
        assert_eq!(news, [2, 3, 4]);
        assert_eq!((add, add_ids), (vec!["volt".to_string()], vec![1]));
        let mut all: Vec<i32> = news.clone();
        all.push(1);
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 4);
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
