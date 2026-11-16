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
use crate::space::WatchGuard;
use crate::util::sha256_file;

const DUMP_MEMBER: &str = "db/dump.pgc.zst";

#[derive(Debug, Clone, Default)]
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
    /// What to restore (crate::restore_scope). Everything by default.
    /// Telemetry only and a date range go into the running database (`cfg`
    /// is the live one, `clickhouse` the live ClickHouse), deduplicated.
    pub scope: crate::restore_scope::Scope,
    /// Members to leave out (a damaged month, B5): never read or checked.
    pub skip_members: Vec<String>,
    /// Stop cleanly when a disk the restore writes to runs low.
    pub watch: Option<crate::space::SpaceWatch>,
    /// Where the archive's members are unpacked (default: beside the file).
    pub temp_dir: Option<PathBuf>,
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
pub(crate) const STAGING: &str = "spm_restore_ts_kv";

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
    let guard = opts.watch.as_ref().map(|w| w.start()).unwrap_or_else(WatchGuard::none);
    let result = run_watched(cfg, opts, &guard, sink).await;
    guard.stop();
    // The watch stopped the run: say so, whatever error the stop caused.
    match (result, guard.tripped()) {
        (Err(_), Some(why)) => Err(Error::SpaceLow(why)),
        (r, _) => r,
    }
}

async fn run_watched(
    cfg: &ConnConfig,
    opts: &RestoreOptions,
    guard: &WatchGuard,
    sink: ProgressFn,
) -> Result<RestoreSummary> {
    use crate::restore_scope::Scope;
    let restorable_stores: Vec<_> = opts.file_store_paths.iter().cloned().collect();
    let (manifest, members) = read_archive(
        &opts.input,
        opts.passphrase.as_deref(),
        opts.temp_dir.as_deref(),
        Some(&|m: &Manifest| crate::restore_scope::members_for(m, &opts.scope, &opts.skip_members)),
        guard,
    )?;
    let cleanup = || {
        for p in members.values() {
            let _ = std::fs::remove_file(p);
        }
    };
    if manifest.format_version > crate::manifest::FORMAT_VERSION {
        cleanup();
        return Err(Error::msg(format!(
            "this backup was made by a newer Platform Manager (format {}); update the Platform Manager to restore it",
            manifest.format_version
        )));
    }
    let wanted = crate::restore_scope::members_for(&manifest, &opts.scope, &opts.skip_members);
    let has_telemetry = !telemetry_members(&members).is_empty();
    let mut steps = Steps::new(sink.clone(), 7 + u32::from(has_telemetry));
    steps.step("Reading backup");
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
    if opts.scope != Scope::Everything {
        steps.log(format!("Restoring: {}", opts.scope.label()));
    }
    for skipped in &opts.skip_members {
        steps.log(format!("Left out: {}", member_label(skipped)));
    }

    steps.step("Verifying integrity");
    if let Err(e) = verify_all(&manifest, &members, &wanted).and_then(|_| guard.check()) {
        cleanup();
        return Err(e);
    }

    steps.step("Checking target database");
    let db = match DbInspector::connect(cfg).await {
        Ok(db) => db,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };
    let server = db.server_info().await.inspect_err(|_| cleanup())?;
    let existing = db.list_public_tables().await.inspect_err(|_| cleanup())?;
    if !existing.is_empty() && !opts.allow_nonempty && !opts.scope.into_live() {
        cleanup();
        return Err(Error::msg(format!(
            "target database '{}' is not empty ({} tables). v1 restores into an empty database only.",
            server.database,
            existing.len()
        )));
    }
    if opts.scope.into_live() && existing.iter().all(|t| t.name != "key_dictionary") {
        cleanup();
        return Err(Error::msg(format!(
            "telemetry goes into a running SENTIENT database; '{}' has no key_dictionary",
            server.database
        )));
    }
    let has_timescale = server.timescaledb_version.is_some();

    steps.step("Reading the database dump");
    let dump_tmp = match members.get(DUMP_MEMBER).cloned() {
        Some(d) => d,
        None => {
            cleanup();
            return Err(Error::msg("backup has no database dump"));
        }
    };
    let tools = PgTools::resolve().inspect_err(|_| cleanup())?;
    let (archive, toc) = {
        let tools = tools.clone();
        let dump = dump_tmp.clone();
        tokio::task::spawn_blocking(move || unpack_dump(&tools, &dump))
            .await
            .map_err(|e| Error::msg(format!("reading the dump panicked: {e}")))?
    }
    .inspect_err(|_| cleanup())?;

    if opts.scope.into_live() {
        let result = crate::restore_scope::into_live(cfg, opts, &manifest, &members, &tools, &archive, guard, &sink, &mut steps).await;
        let _ = std::fs::remove_file(&archive);
        cleanup();
        let rows = result?;
        steps.done(format!("{rows} telemetry rows restored into '{}'", server.database));
        return Ok(RestoreSummary { database: server.database, restored_from: opts.input.clone(), file_stores_restored: 0 });
    }

    let source = source_store(&manifest, &toc);
    let convert = opts.telemetry_into.as_ref().filter(|into| into.store() != source).cloned();
    if let Some(into) = &convert {
        steps.log(format!(
            "The backup is from {}; its telemetry is converted for {}.",
            source.label(),
            into.store().label()
        ));
    }
    // Rows that never travel, even when an older backup holds them: the
    // server's own licence state and clock anchor (categories::NOT_CARRIED).
    let toc: Vec<String> = toc.into_iter().filter(|l| !is_not_carried_data_entry(l)).collect();
    // Configuration only: the latest values are telemetry too.
    let toc: Vec<String> = if opts.scope == Scope::Configuration {
        toc.into_iter().filter(|l| !is_latest_data_entry(l)).collect()
    } else {
        toc
    };

    let result = match &convert {
        None => {
            restore_as_backed_up(cfg, &db, &tools, &archive, &toc, has_timescale, &manifest, &members, opts, guard, &mut steps, &sink)
                .await
        }
        Some(into) => {
            restore_converted(cfg, &db, &tools, &archive, &toc, source, into, &members, opts, guard, &mut steps, &sink).await
        }
    };
    let _ = std::fs::remove_file(&archive);
    if let Err(e) = result.and_then(|_| guard.check()) {
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
                if let Some(member_tmp) = members.get(&entry.member) {
                    steps.log(format!("Restoring file store '{}' → {}", entry.id, dest.display()));
                    if let Err(e) = files::extract_dir(member_tmp, &dest) {
                        cleanup();
                        return Err(e);
                    }
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

/// A table-of-contents line restoring the latest values' rows.
fn is_latest_data_entry(line: &str) -> bool {
    let Some((_, rest)) = line.split_once("; ") else { return false };
    let t: Vec<&str> = rest.split_whitespace().collect();
    // "<oid> <oid> TABLE DATA public ts_kv_latest owner"
    t.len() >= 6 && t[2] == "TABLE" && t[3] == "DATA" && t[5] == "ts_kv_latest"
}

/// A table-of-contents line restoring the rows of a table that never
/// travels (`license_state`, `license_time_anchor`).
fn is_not_carried_data_entry(line: &str) -> bool {
    let Some((_, rest)) = line.split_once("; ") else { return false };
    let t: Vec<&str> = rest.split_whitespace().collect();
    t.len() >= 6 && t[2] == "TABLE" && t[3] == "DATA" && t[4] == "public" && crate::categories::is_not_carried(t[5])
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
    guard: &WatchGuard,
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
    let restore_res = run_pg_restore(tools, cfg, archive, list, guard, sink).await;

    if has_timescale {
        steps.step("Finalizing (timescaledb_post_restore)");
        db.batch("SELECT timescaledb_post_restore()").await?;
    }
    restore_res?;

    // Telemetry (after post_restore: ts_kv is a live hypertable again).
    let telemetry = telemetry_members(&members);
    if !telemetry.is_empty() {
        steps.step("Restoring telemetry (COPY)");
        let mut rows = 0;
        for tmp in telemetry {
            guard.check()?;
            rows += db.copy_in_compressed("COPY ts_kv FROM STDIN WITH (FORMAT binary)", tmp).await?;
        }
        steps.log(format!("  {rows} telemetry rows"));
    }

    // ClickHouse telemetry: into an empty store, every part counted.
    if let Some(section) = manifest.clickhouse.as_ref().filter(|_| opts.scope.restores_telemetry()) {
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
        let g = guard.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            crate::clickhouse::ensure_empty(&ch)?;
            for (entry, file) in files {
                g.check()?;
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
    guard: &WatchGuard,
    steps: &mut Steps,
    sink: &ProgressFn,
) -> Result<()> {
    use crate::telemetry_move as mv;
    // Configuration only needs no ClickHouse: the app clones its schema empty.
    let ch = match into {
        TelemetryInto::Clickhouse if opts.scope.restores_telemetry() => Some(opts.clickhouse.clone().ok_or_else(|| {
            Error::msg("restoring into PostgreSQL + ClickHouse needs the ClickHouse database to restore into")
        })?),
        TelemetryInto::Clickhouse => opts.clickhouse.clone(),
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
    run_pg_restore(tools, cfg, archive, list, guard, sink).await?;
    guard.check()?;

    steps.step("Converting telemetry");
    pg.batch_execute(&format!(
        "DROP TABLE IF EXISTS {STAGING}; CREATE UNLOGGED TABLE {STAGING} (entity_id uuid, key int, ts bigint, \
         bool_v boolean, str_v varchar, long_v bigint, dbl_v double precision, json_v json)"
    ))
    .await?;
    let mut loaded = 0u64;
    for tmp in telemetry_members(members) {
        guard.check()?;
        loaded += db.copy_in_compressed(&format!("COPY {STAGING} FROM STDIN WITH (FORMAT binary)"), tmp).await?;
    }
    if source == TelemetryStore::Clickhouse {
        loaded += load_clickhouse_history(&pg, members, sink).await?;
    }
    steps.log(format!("  {loaded} telemetry rows read"));
    guard.check()?;
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
pub(crate) async fn load_clickhouse_history(
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
    let (_, mut f) = crate::parts::open(path)?;
    let mut magic = [0u8; 22];
    let n = f.read(&mut magic).map_err(|e| Error::msg(e.to_string()))?;
    Ok(magic[..n].starts_with(b"age-encryption.org"))
}

/// Extract manifest.json + every `db/*`, `clickhouse/*` and `files/*` member to temp files.
/// Transparently decrypts an age-encrypted archive when a passphrase is given.
/// The manifest of a backup file without restoring or unpacking it (it is
/// the archive's first member). An encrypted backup needs its passphrase.
pub fn inspect(input: &Path, passphrase: Option<&str>) -> Result<Manifest> {
    let (_, f) = crate::parts::open(input)?;
    let reader: Box<dyn Read> = if is_encrypted(input)? {
        let pw = passphrase.ok_or_else(|| Error::msg("this backup is encrypted: its passphrase is needed"))?;
        let decryptor = match age::Decryptor::new(io::BufReader::new(f)).map_err(|e| Error::msg(format!("reading encrypted backup: {e}")))? {
            age::Decryptor::Passphrase(d) => d,
            _ => return Err(Error::msg("unsupported encryption type")),
        };
        Box::new(decryptor.decrypt(&age::secrecy::Secret::new(pw.to_owned()), None).map_err(|_| Error::msg("wrong passphrase, or the backup is damaged"))?)
    } else {
        Box::new(f)
    };
    let mut ar = tar::Archive::new(reader);
    let mut entries = ar.entries().map_err(|e| Error::msg(e.to_string()))?;
    let mut e = entries
        .next()
        .ok_or_else(|| Error::msg("this file is not a SENTIENT backup (it is empty)"))?
        .map_err(|_| Error::msg("this file is not a SENTIENT backup (it is not a backup archive)"))?;
    let path = e.path().map_err(|e| Error::msg(e.to_string()))?.to_string_lossy().into_owned();
    if path != "manifest.json" {
        return Err(Error::msg("this file is not a SENTIENT backup (no manifest at its start)"));
    }
    let mut buf = String::new();
    e.read_to_string(&mut buf).map_err(|e| Error::msg(e.to_string()))?;
    serde_json::from_str(&buf).map_err(|e| Error::msg(format!("bad manifest: {e}")))
}

/// Picks the members to unpack, once the manifest is read.
pub(crate) type MemberFilter<'a> = &'a dyn Fn(&Manifest) -> Vec<String>;

/// Unpack the archive's members to temp files (in `temp_dir`, else beside
/// the file). With `only`, just the members that scope reads (the manifest is
/// the first member, so it is known before any other is written). The guard
/// is checked after each member: a disk running low stops the unpacking.
pub(crate) fn read_archive(
    input: &Path,
    passphrase: Option<&str>,
    temp_dir: Option<&Path>,
    only: Option<MemberFilter<'_>>,
    guard: &WatchGuard,
) -> Result<(Manifest, HashMap<String, PathBuf>)> {
    let (base, f) = crate::parts::open(input)?;
    let input = base.as_path();
    let reader: Box<dyn Read> = if is_encrypted(input)? {
        let pw = passphrase.ok_or_else(|| {
            Error::msg("this backup is password-protected. A password is required to restore")
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
    let stem = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "backup".into());
    let temp_path = |safe: &str| match temp_dir {
        Some(d) => d.join(format!("{stem}.{safe}.tmp")),
        None => input.with_extension(format!("{safe}.tmp")),
    };
    let mut ar = tar::Archive::new(reader);
    let mut manifest: Option<Manifest> = None;
    let mut wanted: Option<Vec<String>> = None;
    let mut members: HashMap<String, PathBuf> = HashMap::new();
    let drop_all = |m: &HashMap<String, PathBuf>| {
        for p in m.values() {
            let _ = std::fs::remove_file(p);
        }
    };

    let result = (|| -> Result<()> {
        for entry in ar.entries().map_err(|e| Error::msg(e.to_string()))? {
            let mut e = entry.map_err(|e| Error::msg(e.to_string()))?;
            let path = e.path().map_err(|e| Error::msg(e.to_string()))?.to_string_lossy().into_owned();
            if path == "manifest.json" {
                let mut buf = String::new();
                e.read_to_string(&mut buf).map_err(|e| Error::msg(e.to_string()))?;
                let m: Manifest = serde_json::from_str(&buf).map_err(|e| Error::msg(format!("bad manifest: {e}")))?;
                wanted = only.map(|f| f(&m));
                manifest = Some(m);
            } else if path.starts_with("db/") || path.starts_with("clickhouse/") || path.starts_with("files/") {
                if wanted.as_ref().is_some_and(|w| !w.contains(&path)) {
                    continue;
                }
                let safe = path.replace(['/', '\\'], "_");
                let tmp = temp_path(&safe);
                let mut out = File::create(&tmp).map_err(|e| Error::msg(format!("{}: {e}", tmp.display())))?;
                members.insert(path, tmp.clone());
                io::copy(&mut e, &mut out).map_err(|e| Error::msg(format!("unpacking to {}: {e}", tmp.display())))?;
                guard.check()?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        drop_all(&members);
        return Err(e);
    }
    let Some(manifest) = manifest else {
        drop_all(&members);
        return Err(Error::msg("backup has no manifest.json"));
    };
    Ok((manifest, members))
}

/// Every wanted member against its checksum.
fn verify_all(manifest: &Manifest, members: &HashMap<String, PathBuf>, wanted: &[String]) -> Result<()> {
    for fe in manifest.files.iter().filter(|f| wanted.contains(&f.path)) {
        let tmp = members
            .get(&fe.path)
            .ok_or_else(|| Error::msg(format!("archive is missing member '{}'", fe.path)))?;
        let (got, _) = sha256_file(tmp)?;
        if got != fe.sha256 {
            return Err(Error::msg(format!(
                "integrity check failed for '{}'. The backup is corrupted or altered",
                fe.path
            )));
        }
    }
    Ok(())
}

/// The PostgreSQL telemetry members, oldest month first (format 2), or the
/// one member of format 1.
pub(crate) fn telemetry_members(members: &HashMap<String, PathBuf>) -> Vec<&PathBuf> {
    let mut months: Vec<(&String, &PathBuf)> = members.iter().filter(|(k, _)| k.starts_with(crate::backup::TELEMETRY_MONTH_PREFIX)).collect();
    months.sort_by(|a, b| a.0.cmp(b.0));
    let mut out: Vec<&PathBuf> = months.into_iter().map(|(_, v)| v).collect();
    if let Some(one) = members.get(crate::backup::TELEMETRY_MEMBER) {
        out.push(one);
    }
    out
}

/// One part of a backup and whether it matches its checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCheck {
    pub path: String,
    /// "Platform data", "Telemetry history, Sep 2026", …
    pub label: String,
    pub bytes: u64,
    pub ok: bool,
}

/// What [`check`] found.
#[derive(Debug, Clone)]
pub struct CheckReport {
    pub manifest: Manifest,
    pub members: Vec<MemberCheck>,
    /// Parts listed in the manifest but not in the file.
    pub missing: Vec<String>,
    /// Each file against `<name>.sha256`, when there is one.
    pub files: Option<Vec<(String, bool)>>,
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.missing.is_empty() && self.members.iter().all(|m| m.ok) && self.files.as_ref().is_none_or(|f| f.iter().all(|(_, ok)| *ok))
    }
}

/// A plain name for a member.
pub fn member_label(path: &str) -> String {
    let month = |p: &str| {
        let ym = p.split('/').next_back().unwrap_or("").split('.').next().unwrap_or("");
        let ym = ym.replace('-', "");
        match (ym.get(..4).and_then(|y| y.parse::<i32>().ok()), ym.get(4..6).and_then(|m| m.parse::<u32>().ok())) {
            (Some(y), Some(m)) => chrono::NaiveDate::from_ymd_opt(y, m, 1).map(|d| d.format("%b %Y").to_string()).unwrap_or_default(),
            _ => String::new(),
        }
    };
    if path == crate::backup::DUMP_MEMBER {
        "Platform data".into()
    } else if path == crate::backup::TELEMETRY_MEMBER {
        "Telemetry history".into()
    } else if path.starts_with(crate::backup::TELEMETRY_MONTH_PREFIX) || path.starts_with("clickhouse/ts_kv/") {
        format!("Telemetry history, {}", month(path))
    } else if path.starts_with("clickhouse/") {
        "Latest values (ClickHouse)".into()
    } else if let Some(store) = path.strip_prefix("files/") {
        match store.split('.').next().unwrap_or("") {
            "reports" => "Report files".into(),
            "vc-repos" => "Version control".into(),
            other => format!("Files: {other}"),
        }
    } else {
        path.to_string()
    }
}

/// Read the whole backup and compare every part with its checksum, without
/// restoring or keeping anything. Blocking.
pub fn check(input: &Path, passphrase: Option<&str>) -> Result<CheckReport> {
    check_with(input, passphrase, |_, _| {})
}

/// [`check`], calling `each(member, bytes)` as each part has been read
/// (a check step that shows its progress part by part).
pub fn check_with(input: &Path, passphrase: Option<&str>, mut each: impl FnMut(&str, u64)) -> Result<CheckReport> {
    use sha2::{Digest, Sha256};
    let files = crate::parts::check_sidecar(input)?;
    let (base, f) = crate::parts::open(input)?;
    let reader: Box<dyn Read> = if is_encrypted(&base)? {
        let pw = passphrase.ok_or_else(|| Error::msg("this backup is encrypted: its passphrase is needed"))?;
        let decryptor = match age::Decryptor::new(io::BufReader::new(f)).map_err(|e| Error::msg(format!("reading encrypted backup: {e}")))? {
            age::Decryptor::Passphrase(d) => d,
            _ => return Err(Error::msg("unsupported encryption type")),
        };
        Box::new(decryptor.decrypt(&age::secrecy::Secret::new(pw.to_owned()), None).map_err(|_| Error::msg("wrong passphrase, or the backup is damaged"))?)
    } else {
        Box::new(f)
    };
    let mut ar = tar::Archive::new(reader);
    let mut manifest: Option<Manifest> = None;
    let mut seen: HashMap<String, (String, u64)> = HashMap::new();
    let entries = ar.entries().map_err(|e| Error::msg(e.to_string()))?;
    for entry in entries {
        // A file cut short or garbled ends the walk; what was not reached counts as missing.
        let Ok(mut e) = entry else { break };
        let Ok(path) = e.path().map(|p| p.to_string_lossy().into_owned()) else { break };
        if path == "manifest.json" {
            let mut buf = String::new();
            e.read_to_string(&mut buf).map_err(|e| Error::msg(e.to_string()))?;
            manifest = Some(serde_json::from_str(&buf).map_err(|e| Error::msg(format!("bad manifest: {e}")))?);
            continue;
        }
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut bytes = 0u64;
        let mut broken = false;
        loop {
            match e.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    h.update(&buf[..n]);
                    bytes += n as u64;
                }
                Err(_) => {
                    broken = true;
                    break;
                }
            }
        }
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        each(&path, bytes);
        seen.insert(path, (if broken { String::new() } else { hex }, bytes));
        if broken {
            break;
        }
    }
    let manifest = manifest.ok_or_else(|| Error::msg("this file is not a SENTIENT backup (no manifest)"))?;
    let mut members = Vec::new();
    let mut missing = Vec::new();
    for fe in &manifest.files {
        match seen.get(&fe.path) {
            Some((sha, bytes)) => members.push(MemberCheck { path: fe.path.clone(), label: member_label(&fe.path), bytes: *bytes, ok: *sha == fe.sha256 }),
            None => missing.push(fe.path.clone()),
        }
    }
    Ok(CheckReport { manifest, members, missing, files })
}

/// Decompress the dump to a file pg_restore can list, and list it.
pub(crate) fn unpack_dump(tools: &PgTools, dump_zst: &Path) -> Result<(PathBuf, Vec<String>)> {
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
async fn run_pg_restore(tools: &PgTools, cfg: &ConnConfig, archive: &Path, list: Vec<String>, guard: &WatchGuard, sink: &ProgressFn) -> Result<()> {
    let (tools, cfg, archive, sink, guard) = (tools.clone(), cfg.clone(), archive.to_path_buf(), sink.clone(), guard.clone());
    tokio::task::spawn_blocking(move || pg_restore_stream(&tools, &cfg, &archive, &list, &guard, sink))
        .await
        .map_err(|e| Error::msg(format!("restore task panicked: {e}")))?
}

fn pg_restore_stream(tools: &PgTools, cfg: &ConnConfig, archive: &Path, list: &[String], guard: &WatchGuard, sink: ProgressFn) -> Result<()> {
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

    // Polled, so a disk running low stops pg_restore at once.
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| Error::msg(e.to_string()))? {
            break st;
        }
        if let Some(why) = guard.tripped() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_t.join();
            let _ = err_t.join();
            let _ = std::fs::remove_file(&list_file);
            return Err(Error::SpaceLow(why));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
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

    /// An older backup may hold license_state and license_time_anchor rows:
    /// the restore leaves them out, and keeps the licence rows and the tables.
    #[test]
    fn a_restore_leaves_out_the_licence_state_rows() {
        assert!(is_not_carried_data_entry("4520; 0 20401 TABLE DATA public license_state sentient"));
        assert!(!is_not_carried_data_entry("4519; 0 20390 TABLE DATA public license sentient"));
        assert!(is_not_carried_data_entry("4521; 0 20410 TABLE DATA public license_time_anchor sentient"));
        assert!(!is_not_carried_data_entry("402; 1259 20401 TABLE public license_state sentient"));
        assert!(!is_not_carried_data_entry("5310; 2606 20402 CONSTRAINT public license_state license_state_pkey sentient"));
    }

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

#[cfg(test)]
mod check_tests {
    use super::*;

    fn backup_with(dir: &Path, name: &str, data: &[u8], recorded_sha: &str) -> PathBuf {
        let manifest = serde_json::json!({
            "format_version": 2, "tool_version": "t", "created_at": "2026-11-15T00:00:00Z",
            "source": {"database": "s", "postgres_version": "18", "timescaledb_version": null},
            "components": [], "telemetry": {"mode": "all"},
            "files": [
                {"path": "db/dump.pgc.zst", "bytes": data.len(), "sha256": recorded_sha},
                {"path": "db/telemetry/2026-09.copy.zst", "bytes": 3, "sha256": crate::util::hex(&<sha2::Sha256 as sha2::Digest>::digest(b"sep"))}
            ],
            "file_stores": [], "encryption": {"scheme": "none"}
        });
        let path = dir.join(name);
        let mut tar = tar::Builder::new(File::create(&path).unwrap());
        let mj = serde_json::to_vec(&manifest).unwrap();
        for (n, bytes) in [("manifest.json", mj.as_slice()), ("db/dump.pgc.zst", data), ("db/telemetry/2026-09.copy.zst", b"sep".as_slice())] {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, n, bytes).unwrap();
        }
        tar.finish().unwrap();
        path
    }

    #[test]
    fn a_check_names_the_damaged_part_and_a_good_file_passes() {
        let dir = std::env::temp_dir().join(format!("sbr-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let good_sha = crate::util::hex(&<sha2::Sha256 as sha2::Digest>::digest(b"dump"));
        let good = backup_with(&dir, "good.sentient-backup", b"dump", &good_sha);
        let r = check(&good, None).unwrap();
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.members[1].label, "Telemetry history, Sep 2026");
        let bad = backup_with(&dir, "bad.sentient-backup", b"dump", &"0".repeat(64));
        let r = check(&bad, None).unwrap();
        assert!(!r.ok());
        assert_eq!(r.members.iter().filter(|m| !m.ok).map(|m| m.label.as_str()).collect::<Vec<_>>(), ["Platform data"]);
        // Cut short: the last part is missing.
        let len = std::fs::metadata(&good).unwrap().len();
        let cut = dir.join("cut.sentient-backup");
        std::fs::write(&cut, &std::fs::read(&good).unwrap()[..(len as usize / 2)]).unwrap();
        let r = check(&cut, None).unwrap();
        assert!(!r.ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
