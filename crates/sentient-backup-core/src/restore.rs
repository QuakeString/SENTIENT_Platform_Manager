//! Restore engine (empty-DB-only). Extracts + checksums every archive member,
//! runs the TimescaleDB-aware DB restore, then extracts any selected file stores
//! to their target paths.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::db::{ConnConfig, DbInspector};
use crate::error::{Error, Result};
use crate::files;
use crate::manifest::Manifest;
use crate::pg_tools::PgTools;
use crate::progress::{Progress, ProgressFn, Steps};
use crate::util::sha256_file;

const DUMP_MEMBER: &str = "db/dump.pgc.zst";

#[derive(Debug, Clone)]
pub struct RestoreOptions {
    pub input: PathBuf,
    /// v1 is empty-DB-only; override for advanced use / testing.
    pub allow_nonempty: bool,
    /// Where to extract each file store, keyed by store id. Missing id → skip.
    pub file_store_paths: Vec<(String, PathBuf)>,
    /// Password for an encrypted (age) archive; required iff it's encrypted.
    pub passphrase: Option<String>,
    /// Target ClickHouse, required when the backup holds ClickHouse telemetry
    /// or `telemetry_into` is [`TelemetryInto::Clickhouse`].
    pub clickhouse: Option<crate::clickhouse::ChConn>,
    /// Where the installation restored into keeps telemetry. When it differs
    /// from where the backed-up installation kept it, the telemetry is
    /// converted (TimescaleDB, partitioned PostgreSQL and ClickHouse in any
    /// direction). `None` restores it as it was backed up.
    pub telemetry_into: Option<TelemetryInto>,
}

/// How an installation keeps telemetry (SENTIENT's `ts_kv`).
#[derive(Debug, Clone)]
pub enum TelemetryInto {
    /// PostgreSQL + TimescaleDB: a hypertable made by SENTIENT's own
    /// data/sql/schema-ts-timescale.sql, given here.
    Hypertable { timescale_sql: String },
    /// Plain PostgreSQL: partitioned by month.
    Partitioned,
    /// PostgreSQL + ClickHouse: the history and latest values in ClickHouse
    /// ([`RestoreOptions::clickhouse`]); PostgreSQL's `ts_kv` stays plain and empty.
    Clickhouse,
}

/// Where a backup's telemetry came from, read from the backup itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryStore {
    /// A plain `ts_kv` (plain PostgreSQL before monthly partitions).
    Plain,
    Partitioned,
    Hypertable,
    Clickhouse,
}

impl TelemetryStore {
    pub fn label(self) -> &'static str {
        match self {
            TelemetryStore::Plain | TelemetryStore::Partitioned => "PostgreSQL",
            TelemetryStore::Hypertable => "PostgreSQL + TimescaleDB",
            TelemetryStore::Clickhouse => "PostgreSQL + ClickHouse",
        }
    }
}

impl TelemetryInto {
    fn store(&self) -> TelemetryStore {
        match self {
            TelemetryInto::Hypertable { .. } => TelemetryStore::Hypertable,
            TelemetryInto::Partitioned => TelemetryStore::Partitioned,
            TelemetryInto::Clickhouse => TelemetryStore::Clickhouse,
        }
    }
}

/// The staging table a converted restore loads telemetry into first.
const STAGING: &str = "spm_restore_ts_kv";

#[derive(Debug, Clone)]
pub struct RestoreSummary {
    pub database: String,
    pub restored_from: PathBuf,
    pub file_stores_restored: usize,
}

pub async fn run(
    cfg: &ConnConfig,
    opts: &RestoreOptions,
    sink: ProgressFn,
) -> Result<RestoreSummary> {
    let restorable_stores: Vec<_> = opts.file_store_paths.iter().cloned().collect();
    let (manifest, members) = read_archive(&opts.input, opts.passphrase.as_deref())?;
    let has_telemetry = members.contains_key(crate::backup::TELEMETRY_MEMBER);
    let mut steps = Steps::new(sink.clone(), 7 + u32::from(has_telemetry));
    steps.step("Reading backup");
    let cleanup = || {
        for p in members.values() {
            let _ = std::fs::remove_file(p);
        }
    };
    steps.log(format!(
        "Backup of '{}' ({}), created {}",
        manifest.source.database,
        manifest
            .source
            .timescaledb_version
            .as_deref()
            .map(|v| format!("TimescaleDB {v}"))
            .unwrap_or_else(|| "no TimescaleDB".into()),
        manifest.created_at.to_rfc3339()
    ));

    steps.step("Verifying integrity");
    if let Err(e) = verify_all(&manifest, &members) {
        cleanup();
        return Err(e);
    }

    steps.step("Checking target database");
    let db = DbInspector::connect(cfg).await?;
    let server = db.server_info().await?;
    let existing = db.list_public_tables().await?;
    if !existing.is_empty() && !opts.allow_nonempty {
        cleanup();
        return Err(Error::msg(format!(
            "target database '{}' is not empty ({} tables). v1 restores into an empty database only.",
            server.database,
            existing.len()
        )));
    }
    let has_timescale = server.timescaledb_version.is_some();

    steps.step("Reading the database dump");
    let dump_tmp = members
        .get(DUMP_MEMBER)
        .cloned()
        .ok_or_else(|| Error::msg("backup has no database dump"))?;
    let tools = PgTools::resolve()?;
    let (archive, toc) = {
        let tools = tools.clone();
        let dump = dump_tmp.clone();
        tokio::task::spawn_blocking(move || unpack_dump(&tools, &dump))
            .await
            .map_err(|e| Error::msg(format!("reading the dump panicked: {e}")))?
    }
    .inspect_err(|_| cleanup())?;
    let source = source_store(&manifest, &toc);
    let convert = opts.telemetry_into.as_ref().filter(|into| into.store() != source).cloned();
    if let Some(into) = &convert {
        steps.log(format!(
            "The backup is from {}; its telemetry is converted for {}.",
            source.label(),
            into.store().label()
        ));
    }

    let result = match &convert {
        None => {
            restore_as_backed_up(cfg, &db, &tools, &archive, &toc, has_timescale, &manifest, &members, opts, &mut steps, &sink)
                .await
        }
        Some(into) => {
            restore_converted(cfg, &db, &tools, &archive, &toc, source, into, &members, opts, &mut steps, &sink).await
        }
    };
    let _ = std::fs::remove_file(&archive);
    if let Err(e) = result {
        cleanup();
        return Err(e);
    }

    // File stores.
    let mut restored_stores = 0usize;
    for entry in &manifest.file_stores {
        let target = restorable_stores
            .iter()
            .find(|(id, _)| id == &entry.id)
            .map(|(_, p)| p.clone());
        match target {
            Some(dest) => {
                steps.log(format!("Restoring file store '{}' → {}", entry.id, dest.display()));
                if let Some(member_tmp) = members.get(&entry.member) {
                    files::extract_dir(member_tmp, &dest)?;
                    restored_stores += 1;
                }
            }
            None => steps.log(format!(
                "Skipping file store '{}' (no target path given; was at {})",
                entry.id, entry.source_path
            )),
        }
    }

    cleanup();
    steps.done(format!("Restored into '{}'", server.database));
    Ok(RestoreSummary {
        database: server.database,
        restored_from: opts.input.clone(),
        file_stores_restored: restored_stores,
    })
}

/// The original restore: the dump, then telemetry where it was backed up from.
#[allow(clippy::too_many_arguments)]
async fn restore_as_backed_up(
    cfg: &ConnConfig,
    db: &DbInspector,
    tools: &PgTools,
    archive: &Path,
    toc: &[String],
    has_timescale: bool,
    manifest: &Manifest,
    members: &HashMap<String, PathBuf>,
    opts: &RestoreOptions,
    steps: &mut Steps,
    sink: &ProgressFn,
) -> Result<()> {
    steps.step("Preparing (timescaledb_pre_restore)");
    if has_timescale {
        db.batch("CREATE EXTENSION IF NOT EXISTS timescaledb").await?;
        db.batch("SELECT timescaledb_pre_restore()").await?;
    }

    steps.step("Restoring database (pg_restore)");
    // TimescaleDB's own catalog metadata (uuid, install_timestamp, …) is
    // written by CREATE EXTENSION in the target database and re-written by
    // the extension while the restore runs, so restoring the source's rows on
    // top fails on metadata_pkey and --exit-on-error aborts everything. Skip
    // just that table's data: the target keeps its own identity, as a fresh
    // TimescaleDB database should.
    let list: Vec<String> =
        toc.iter().filter(|l| !l.contains("TABLE DATA _timescaledb_catalog metadata ")).cloned().collect();
    let restore_res = run_pg_restore(tools, cfg, archive, list, sink).await;

    if has_timescale {
        steps.step("Finalizing (timescaledb_post_restore)");
        db.batch("SELECT timescaledb_post_restore()").await?;
    }
    restore_res?;

    // Telemetry (after post_restore: ts_kv is a live hypertable again).
    if let Some(tmp) = members.get(crate::backup::TELEMETRY_MEMBER) {
        steps.step("Restoring telemetry (COPY)");
        let rows = db.copy_in_compressed("COPY ts_kv FROM STDIN WITH (FORMAT binary)", tmp).await?;
        steps.log(format!("  {rows} telemetry rows"));
    }

    // ClickHouse telemetry: into an empty store, every part counted.
    if let Some(section) = &manifest.clickhouse {
        steps.step("Restoring ClickHouse telemetry");
        let ch = opts.clickhouse.clone().ok_or_else(|| {
            Error::msg("this backup holds ClickHouse telemetry; restore it into a PostgreSQL + ClickHouse install")
        })?;
        let files: Vec<(crate::clickhouse::ChMember, PathBuf)> = section
            .members
            .iter()
            .filter_map(|m| members.get(&m.member).map(|p| (m.clone(), p.clone())))
            .collect();
        let sink_ch = sink.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            crate::clickhouse::ensure_empty(&ch)?;
            for (entry, file) in files {
                let rows = crate::clickhouse::import(&ch, &entry, &file)?;
                sink_ch(Progress::Log { line: format!("  ClickHouse {} {}: {rows} rows", entry.table, entry.partition) });
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::msg(format!("ClickHouse restore panicked: {e}")))??;
    }
    Ok(())
}

/// A restore into another database type: everything but telemetry as it
/// was, `ts_kv` in the shape the target needs, and the telemetry loaded into
/// a staging table and moved from there with the migration's checked,
/// month-by-month copy.
#[allow(clippy::too_many_arguments)]
async fn restore_converted(
    cfg: &ConnConfig,
    db: &DbInspector,
    tools: &PgTools,
    archive: &Path,
    toc: &[String],
    source: TelemetryStore,
    into: &TelemetryInto,
    members: &HashMap<String, PathBuf>,
    opts: &RestoreOptions,
    steps: &mut Steps,
    sink: &ProgressFn,
) -> Result<()> {
    use crate::telemetry_move as mv;
    let ch = match into {
        TelemetryInto::Clickhouse => Some(opts.clickhouse.clone().ok_or_else(|| {
            Error::msg("restoring into PostgreSQL + ClickHouse needs the ClickHouse database to restore into")
        })?),
        _ => None,
    };

    steps.step("Preparing telemetry for the new database type");
    let pg = mv::connect(cfg).await?;
    match into {
        TelemetryInto::Hypertable { timescale_sql } => mv::create_hypertable(&pg, timescale_sql).await?,
        TelemetryInto::Partitioned => mv::create_partitioned(&pg).await?,
        TelemetryInto::Clickhouse => mv::create_plain(&pg).await?,
    }

    steps.step("Restoring database (pg_restore)");
    let list: Vec<String> = toc.iter().filter(|l| !is_telemetry_store_entry(l)).cloned().collect();
    run_pg_restore(tools, cfg, archive, list, sink).await?;

    steps.step("Converting telemetry");
    pg.batch_execute(&format!(
        "DROP TABLE IF EXISTS {STAGING}; CREATE UNLOGGED TABLE {STAGING} (entity_id uuid, key int, ts bigint, \
         bool_v boolean, str_v varchar, long_v bigint, dbl_v double precision, json_v json)"
    ))
    .await?;
    let mut loaded = 0u64;
    if let Some(tmp) = members.get(crate::backup::TELEMETRY_MEMBER) {
        loaded += db.copy_in_compressed(&format!("COPY {STAGING} FROM STDIN WITH (FORMAT binary)"), tmp).await?;
    }
    if source == TelemetryStore::Clickhouse {
        loaded += load_clickhouse_history(&pg, members, sink).await?;
    }
    steps.log(format!("  {loaded} telemetry rows read"));
    let msink = sink.clone();
    match &ch {
        Some(ch) => {
            let ch2 = ch.clone();
            tokio::task::spawn_blocking(move || crate::clickhouse::ensure_empty(&ch2))
                .await
                .map_err(|e| Error::msg(e.to_string()))??;
            mv::history_pg_to_ch(&pg, STAGING, ch, i64::MAX, &msink).await?;
            mv::latest_pg_to_ch(&pg, ch, &msink).await?;
        }
        None => {
            mv::history_pg_to_pg(&pg, STAGING, i64::MAX, &msink).await?;
            // ClickHouse kept the latest values itself; PostgreSQL's copy may
            // be behind. The newest row of each key's history is its latest.
            if source == TelemetryStore::Clickhouse {
                mv::latest_from_table(&pg, STAGING, &msink).await?;
            }
        }
    }
    pg.batch_execute(&format!("DROP TABLE {STAGING}")).await?;
    Ok(())
}

/// The ClickHouse history members of a backup into the staging table, one
/// row per key and time (ClickHouse may hold a row twice until it merges; the
/// newest write wins, as it does there).
async fn load_clickhouse_history(
    pg: &tokio_postgres::Client,
    members: &HashMap<String, PathBuf>,
    sink: &ProgressFn,
) -> Result<u64> {
    use futures_util::SinkExt;
    let mut files: Vec<(&String, &PathBuf)> = members
        .iter()
        .filter(|(m, _)| m.starts_with(&format!("clickhouse/{}/", crate::clickhouse::HISTORY_TABLE)))
        .collect();
    files.sort();
    pg.batch_execute(
        "DROP TABLE IF EXISTS spm_restore_ch; CREATE UNLOGGED TABLE spm_restore_ch (entity_id uuid, key int, \
         ts bigint, bool_v boolean, str_v varchar, long_v bigint, dbl_v double precision, json_v json, inserted_at bigint)",
    )
    .await?;
    for (member, file) in files {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(16);
        let path = file.clone();
        let read = tokio::task::spawn_blocking(move || -> Result<u64> {
            let f = File::open(&path)?;
            let dec = zstd::stream::Decoder::new(f).map_err(|e| Error::msg(format!("zstd: {e}")))?;
            crate::ch_native::to_copy_text(dec, |chunk| {
                tx.blocking_send(bytes::Bytes::from(chunk)).map_err(|_| Error::msg("PostgreSQL stopped reading"))
            })
        });
        let copy = pg
            .copy_in::<_, bytes::Bytes>(&format!("COPY spm_restore_ch ({}) FROM STDIN", crate::ch_native::COLUMNS.join(", ")))
            .await?;
        futures_util::pin_mut!(copy);
        while let Some(chunk) = rx.recv().await {
            copy.send(chunk).await?;
        }
        let rows = read.await.map_err(|e| Error::msg(format!("reading ClickHouse data panicked: {e}")))??;
        copy.finish().await?;
        sink(Progress::Log { line: format!("  {member}: {rows} rows") });
    }
    pg.batch_execute(&format!(
        "INSERT INTO {STAGING} SELECT DISTINCT ON (entity_id, key, ts) entity_id, key, ts, bool_v, str_v, long_v, \
         dbl_v, json_v FROM spm_restore_ch ORDER BY entity_id, key, ts, inserted_at DESC; DROP TABLE spm_restore_ch"
    ))
    .await?;
    let rows: i64 = pg.query_one(&format!("SELECT count(*) FROM {STAGING}"), &[]).await?.get(0);
    Ok(rows as u64)
}

/// Where the backed-up installation kept telemetry: ClickHouse if the backup
/// holds its data, else the shape of `ts_kv` in the dump.
pub fn source_store(manifest: &Manifest, toc: &[String]) -> TelemetryStore {
    if manifest.clickhouse.is_some() {
        TelemetryStore::Clickhouse
    } else if toc.iter().any(|l| l.contains(" _timescaledb_catalog hypertable ")) {
        TelemetryStore::Hypertable
    } else if toc.iter().any(|l| l.contains("TABLE ATTACH public ts_kv_")) {
        TelemetryStore::Partitioned
    } else {
        TelemetryStore::Plain
    }
}

/// A dump table-of-contents entry for `ts_kv` in its backed-up shape: the
/// table, its monthly partitions, its constraints and triggers, and
/// TimescaleDB (the extension, its catalogs and the hypertable's chunks).
fn is_telemetry_store_entry(line: &str) -> bool {
    if line.trim_start().starts_with(';') {
        return false;
    }
    let Some((_, rest)) = line.split_once("; ") else { return false };
    let is_partition = |t: &str| {
        t.strip_prefix("ts_kv_").is_some_and(|r| {
            let b = r.as_bytes();
            b.len() >= 7 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'_' && b[5..7].iter().all(u8::is_ascii_digit)
        })
    };
    rest.split_whitespace().skip(2).any(|t| {
        t == "ts_kv"
            || is_partition(t)
            || t == "timescaledb"
            || t.starts_with("_timescaledb_")
            || t.starts_with("timescaledb_")
            || t.starts_with("_hyper_")
            || t.starts_with("_compressed_hypertable_")
    })
}

/// True if the archive is an age-encrypted (password-protected) file.
pub fn is_encrypted(path: &Path) -> Result<bool> {
    let mut f = File::open(path).map_err(|e| Error::msg(format!("opening backup: {e}")))?;
    let mut magic = [0u8; 22];
    let n = f.read(&mut magic).map_err(|e| Error::msg(e.to_string()))?;
    Ok(magic[..n].starts_with(b"age-encryption.org"))
}

/// Extract manifest.json + every `db/*`, `clickhouse/*` and `files/*` member to temp files.
/// Transparently decrypts an age-encrypted archive when a passphrase is given.
fn read_archive(input: &Path, passphrase: Option<&str>) -> Result<(Manifest, HashMap<String, PathBuf>)> {
    let f = File::open(input).map_err(|e| Error::msg(format!("opening backup: {e}")))?;
    let reader: Box<dyn Read> = if is_encrypted(input)? {
        let pw = passphrase.ok_or_else(|| {
            Error::msg("this backup is password-protected — a password is required to restore")
        })?;
        let decryptor = match age::Decryptor::new(io::BufReader::new(f))
            .map_err(|e| Error::msg(format!("reading encrypted backup: {e}")))?
        {
            age::Decryptor::Passphrase(d) => d,
            _ => return Err(Error::msg("unsupported encryption type")),
        };
        let r = decryptor
            .decrypt(&age::secrecy::Secret::new(pw.to_owned()), None)
            .map_err(|_| Error::msg("wrong password, or the backup is corrupted"))?;
        Box::new(r)
    } else {
        Box::new(f)
    };
    let mut ar = tar::Archive::new(reader);
    let mut manifest: Option<Manifest> = None;
    let mut members: HashMap<String, PathBuf> = HashMap::new();

    for entry in ar.entries().map_err(|e| Error::msg(e.to_string()))? {
        let mut e = entry.map_err(|e| Error::msg(e.to_string()))?;
        let path = e.path().map_err(|e| Error::msg(e.to_string()))?.to_string_lossy().into_owned();
        if path == "manifest.json" {
            let mut buf = String::new();
            e.read_to_string(&mut buf).map_err(|e| Error::msg(e.to_string()))?;
            manifest = Some(serde_json::from_str(&buf).map_err(|e| Error::msg(format!("bad manifest: {e}")))?);
        } else if path.starts_with("db/") || path.starts_with("clickhouse/") || path.starts_with("files/") {
            let safe = path.replace(['/', '\\'], "_");
            let tmp = input.with_extension(format!("{safe}.tmp"));
            let mut out = File::create(&tmp).map_err(|e| Error::msg(e.to_string()))?;
            io::copy(&mut e, &mut out).map_err(|e| Error::msg(e.to_string()))?;
            members.insert(path, tmp);
        }
    }
    let manifest = manifest.ok_or_else(|| Error::msg("backup has no manifest.json"))?;
    Ok((manifest, members))
}

fn verify_all(manifest: &Manifest, members: &HashMap<String, PathBuf>) -> Result<()> {
    for fe in &manifest.files {
        let tmp = members
            .get(&fe.path)
            .ok_or_else(|| Error::msg(format!("archive is missing member '{}'", fe.path)))?;
        let (got, _) = sha256_file(tmp)?;
        if got != fe.sha256 {
            return Err(Error::msg(format!(
                "integrity check failed for '{}' — the backup is corrupted or altered",
                fe.path
            )));
        }
    }
    Ok(())
}

/// Decompress the dump to a file pg_restore can list, and list it.
fn unpack_dump(tools: &PgTools, dump_zst: &Path) -> Result<(PathBuf, Vec<String>)> {
    let archive = dump_zst.with_extension("dump");
    {
        let zf = File::open(dump_zst).map_err(|e| Error::msg(format!("open dump: {e}")))?;
        let mut dec = zstd::stream::Decoder::new(zf).map_err(|e| Error::msg(format!("decompress dump: {e}")))?;
        let mut out = File::create(&archive).map_err(|e| Error::msg(format!("write dump: {e}")))?;
        io::copy(&mut dec, &mut out).map_err(|e| Error::msg(format!("decompress dump: {e}")))?;
    }
    let toc = crate::pg_tools::command(&tools.pg_restore)
        .arg("-l")
        .arg(&archive)
        .output()
        .map_err(|e| Error::msg(format!("listing the dump: {e}")))?;
    if !toc.status.success() {
        let _ = std::fs::remove_file(&archive);
        return Err(Error::msg(format!(
            "listing the dump failed: {}",
            String::from_utf8_lossy(&toc.stderr).trim()
        )));
    }
    let lines = String::from_utf8_lossy(&toc.stdout).lines().map(str::to_string).collect();
    Ok((archive, lines))
}

/// pg_restore the entries in `list` (a table of contents from [`unpack_dump`]).
async fn run_pg_restore(tools: &PgTools, cfg: &ConnConfig, archive: &Path, list: Vec<String>, sink: &ProgressFn) -> Result<()> {
    let (tools, cfg, archive, sink) = (tools.clone(), cfg.clone(), archive.to_path_buf(), sink.clone());
    tokio::task::spawn_blocking(move || pg_restore_stream(&tools, &cfg, &archive, &list, sink))
        .await
        .map_err(|e| Error::msg(format!("restore task panicked: {e}")))?
}

fn pg_restore_stream(tools: &PgTools, cfg: &ConnConfig, archive: &Path, list: &[String], sink: ProgressFn) -> Result<()> {
    // Pass a full conninfo (with TCP keepalives) via -d so long server-side
    // steps like index ATTACH don't get their idle connection dropped by a
    // NAT/firewall on a remote/wifi link. Password stays in PGPASSWORD.
    let conninfo = crate::pg_tools::conninfo(cfg);
    let list_file = archive.with_extension("toc");
    let text: String = list.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&list_file, text).map_err(|e| Error::msg(format!("write restore list: {e}")))?;

    let mut child = crate::pg_tools::command(&tools.pg_restore)
        .args(["--no-password", "--verbose", "--exit-on-error", "-d", &conninfo])
        .arg("-L")
        .arg(&list_file)
        .arg(archive)
        .env("PGPASSWORD", &cfg.password)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("spawning pg_restore: {e}")))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let sink_o = sink.clone();
    let out_t = std::thread::spawn(move || drain(stdout, sink_o));
    let sink_e = sink.clone();
    let err_t = std::thread::spawn(move || drain(stderr, sink_e));

    let status = child.wait().map_err(|e| Error::msg(e.to_string()))?;
    let _ = out_t.join();
    let last_err = err_t.join().unwrap_or_default();
    let _ = std::fs::remove_file(&list_file);

    if !status.success() {
        return Err(Error::msg(format!("pg_restore failed ({status}): {}", last_err.join(" | "))));
    }
    Ok(())
}

/// Forward each line to the sink; return the last few, for error messages.
fn drain<R: Read>(r: R, sink: ProgressFn) -> Vec<String> {
    use std::io::BufRead;
    let mut last: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    for line in io::BufReader::new(r).lines().map_while(std::result::Result::ok) {
        if last.len() == 3 {
            last.pop_front();
        }
        last.push_back(line.clone());
        sink(Progress::Log { line });
    }
    last.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines from real dumps of SENTIENT databases.
    const KEPT: &[&str] = &[
        "395; 1255 18375 FUNCTION public delete_asset_records_from_ts_kv(uuid, uuid, bigint) sentient",
        "751; 1255 20863 FUNCTION public ts_kv_now_ms() sentient",
        "410; 1255 18421 FUNCTION public create_future_partitions(text, integer, integer) sentient",
        "338; 1259 21300 TABLE public ts_kv_latest sentient",
        "4651; 2606 21301 CONSTRAINT public ts_kv_latest ts_kv_latest_pkey sentient",
        ";",
        "; Archive created at 2026-10-17 14:11:34 UTC",
    ];
    const DROPPED: &[&str] = &[
        "3; 3079 18021 EXTENSION - timescaledb ",
        "5763; 0 0 COMMENT - EXTENSION timescaledb ",
        "394; 1259 20846 TABLE public ts_kv sentient",
        "395; 1259 20864 TABLE _timescaledb_internal _compressed_hypertable_2 sentient",
        "444; 1259 69831 TABLE _timescaledb_internal _hyper_1_10_chunk sentient",
        "4417; 0 18046 TABLE DATA _timescaledb_catalog hypertable sentient",
        "5766; 0 0 SEQUENCE SET _timescaledb_catalog bgw_job_id_seq sentient",
        "5294; 2606 69840 CONSTRAINT _timescaledb_internal _hyper_1_10_chunk 10_10_ts_kv_pkey sentient",
        "5231; 2606 20855 CONSTRAINT public ts_kv ts_kv_pkey sentient",
        "376; 1259 32467 TABLE public ts_kv_2026_07 postgres",
        "4011; 0 0 TABLE ATTACH public ts_kv_2026_07 postgres",
        "4703; 2606 21701 CONSTRAINT public ts_kv_2026_10 ts_kv_2026_10_pkey sentient",
        "4792; 0 0 INDEX ATTACH public ts_kv_2026_10_pkey sentient",
    ];

    #[test]
    fn filters_only_the_backed_up_telemetry_store() {
        for line in KEPT {
            assert!(!is_telemetry_store_entry(line), "dropped: {line}");
        }
        for line in DROPPED {
            assert!(is_telemetry_store_entry(line), "kept: {line}");
        }
    }

    #[test]
    fn tells_the_source_store_from_the_dump() {
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "format_version": 1, "tool_version": "t", "created_at": "2026-10-17T00:00:00Z",
            "source": {"database": "sentient", "postgres_version": "16", "timescaledb_version": null},
            "components": [], "telemetry": {"mode": "none"}, "files": [], "file_stores": [],
            "encryption": {"scheme": "none"}
        }))
        .unwrap();
        let toc = |lines: &[&str]| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        assert_eq!(source_store(&manifest, &toc(&[DROPPED[5]])), TelemetryStore::Hypertable);
        assert_eq!(source_store(&manifest, &toc(&[DROPPED[10]])), TelemetryStore::Partitioned);
        assert_eq!(source_store(&manifest, &toc(&[DROPPED[2]])), TelemetryStore::Plain);
    }
}
