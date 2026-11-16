//! Backup engine. Streams `pg_dump` (custom format) through zstd + SHA-256 into
//! a `.sentient-backup` tar, plus optional file-store members (`vc-repos`,
//! `reports`). The full schema is always dumped; deselected categories only lose
//! their DATA. See `docs/RESEARCH_AND_PLAN.md`.

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;

use crate::categories::{catalog, CategoryKind};
use crate::db::{build_report, human_bytes, ConnConfig, DbInspector};
use crate::error::{Error, Result};
use crate::files;
use crate::manifest::{
    ComponentEntry, EncryptionInfo, FileEntry, FileStoreEntry, Manifest, SourceInfo,
    TelemetrySelection, FORMAT_VERSION,
};
use crate::pg_tools::PgTools;
use crate::progress::{Progress, ProgressFn, Steps};
use crate::util::HashingWriter;

pub const DUMP_MEMBER: &str = "db/dump.pgc.zst";
/// Format 1's one telemetry member (still read by restore).
pub const TELEMETRY_MEMBER: &str = "db/telemetry.copy.zst";
/// Format 2: one telemetry member per month, `db/telemetry/2026-11.copy.zst`.
pub const TELEMETRY_MONTH_PREFIX: &str = "db/telemetry/";

/// How telemetry (`ts_kv`) is captured. Always via COPY (never the pg_dump).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryMode {
    None,
    All,
    LastDays(u32),
}

/// A file store to include: its category/store id and the directory to archive.
#[derive(Debug, Clone)]
pub struct FileStoreSpec {
    pub id: String,
    pub category_id: String,
    pub path: PathBuf,
}

/// Which components' DATA to include. Full schema is always dumped, so a restore
/// always has every table; only deselected data is omitted. `configuration` is
/// always included.
#[derive(Debug, Clone)]
pub struct Selection {
    pub include: HashSet<String>,
    // Phase 2b: telemetry last-N-days via COPY (reserved).
    pub telemetry_days: Option<u32>,
}

impl Selection {
    pub fn full() -> Self {
        Self {
            include: catalog().iter().map(|c| c.id.to_string()).collect(),
            telemetry_days: None,
        }
    }
    pub fn skipping(skip: &[String]) -> Self {
        let mut include: HashSet<String> = catalog().iter().map(|c| c.id.to_string()).collect();
        for s in skip {
            include.remove(s);
        }
        include.insert("configuration".into());
        Self {
            include,
            telemetry_days: None,
        }
    }
    pub fn is_included(&self, id: &str) -> bool {
        id == "configuration" || self.include.contains(id)
    }

    pub fn telemetry_mode(&self) -> TelemetryMode {
        if !self.include.contains("telemetry_historical") {
            TelemetryMode::None
        } else if let Some(d) = self.telemetry_days {
            TelemetryMode::LastDays(d)
        } else {
            TelemetryMode::All
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupOptions {
    pub output: PathBuf,
    pub selection: Selection,
    /// File stores to include (only reachable ones should be passed).
    pub file_stores: Vec<FileStoreSpec>,
    pub zstd_level: i32,
    /// If set, the whole archive is passphrase-encrypted with age (scrypt).
    pub passphrase: Option<String>,
    /// ClickHouse holding the telemetry, when the install uses it.
    pub clickhouse: Option<crate::clickhouse::ChConn>,
    /// Where the backup comes from (recorded in the manifest).
    pub origin: Origin,
    /// Split the file into numbered parts of this size (None: one file).
    pub part_size: Option<u64>,
}

/// What a backup records about the installation it was made on.
#[derive(Debug, Clone, Default)]
pub struct Origin {
    pub sentient_version: Option<String>,
    pub schema_version: Option<i64>,
    pub database_type: Option<String>,
    pub host: Option<String>,
    /// The platform's settings (`manifest.platform`).
    pub platform: Option<crate::manifest::PlatformInfo>,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            output: PathBuf::from("sentient.sentient-backup"),
            selection: Selection::full(),
            file_stores: Vec::new(),
            zstd_level: 10,
            passphrase: None,
            clickhouse: None,
            origin: Origin::default(),
            part_size: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupSummary {
    pub output: PathBuf,
    pub archive_bytes: u64,
    pub dump_sha256: String,
    pub file_stores: usize,
    /// The files written (one, or the numbered parts) with their SHA-256.
    pub files: Vec<(PathBuf, String, u64)>,
    /// `<output>.sha256`, listing them.
    pub sidecar: PathBuf,
}

/// A member staged in a temp file, to be written into the archive.
struct StagedMember {
    archive_path: String,
    temp: PathBuf,
    sha256: String,
    bytes: u64,
}

fn exclude_data_args(sel: &Selection, ts_kv_partitions: &[String]) -> Vec<String> {
    // Telemetry is ALWAYS handled via COPY, never included in the pg_dump:
    // ts_kv, a hypertable's chunks, and on plain PostgreSQL its monthly
    // partitions (tables of their own, which excluding ts_kv does not reach).
    let mut args = vec![
        "--exclude-table-data=public.ts_kv".into(),
        "--exclude-table-data=_timescaledb_internal.*".into(),
    ];
    for p in ts_kv_partitions {
        args.push(format!("--exclude-table-data={p}"));
    }
    // The server's own licence state never travels (categories::NOT_CARRIED).
    for m in crate::categories::NOT_CARRIED {
        args.push(format!("--exclude-table-data={}", m.pg_pattern()));
    }
    for c in catalog() {
        if c.kind == CategoryKind::Configuration
            || c.kind == CategoryKind::TelemetryHistorical
            || sel.is_included(c.id)
        {
            continue;
        }
        for pat in c.pg_patterns() {
            args.push(format!("--exclude-table-data={pat}"));
        }
    }
    args
}

pub async fn run(cfg: &ConnConfig, opts: &BackupOptions, sink: ProgressFn) -> Result<BackupSummary> {
    let tmode = opts.selection.telemetry_mode();
    let telem_extra = if tmode == TelemetryMode::None { 0 } else { 1 };
    let ch_extra = u32::from(opts.clickhouse.is_some() && tmode != TelemetryMode::None);
    let total_steps = 3 + telem_extra + ch_extra + opts.file_stores.len() as u32;
    let mut steps = Steps::new(sink.clone(), total_steps);

    steps.step("Connecting and inspecting");
    let db = DbInspector::connect(cfg).await?; // kept alive for the telemetry COPY
    let server = db.server_info().await?;
    let tables = db.tables_with_true_sizes().await?;
    let report = build_report(&tables);

    let ts_kv_partitions = db.partitions_of("public.ts_kv").await?;

    let tools = PgTools::resolve()?;
    steps.log(tools.dump_version().unwrap_or_else(|_| "pg_dump: unknown version".into()));

    let n_skipped = catalog()
        .iter()
        .filter(|c| c.kind != CategoryKind::Configuration && !opts.selection.is_included(c.id))
        .count();
    steps.step(if n_skipped == 0 {
        "Dumping database (full)".to_string()
    } else {
        format!("Dumping database ({n_skipped} component(s)' data excluded)")
    });
    let cfg2 = cfg.clone();
    let opts2 = opts.clone();
    let sink2 = sink.clone();
    let (tmp_dump, dump_sha, dump_bytes) =
        tokio::task::spawn_blocking(move || dump_compressed(&tools, &cfg2, &opts2, &ts_kv_partitions, sink2))
            .await
            .map_err(|e| Error::msg(format!("dump task panicked: {e}")))??;

    let mut members = vec![StagedMember {
        archive_path: DUMP_MEMBER.into(),
        temp: tmp_dump,
        sha256: dump_sha.clone(),
        bytes: dump_bytes,
    }];
    let mut file_store_entries = Vec::new();

    // Telemetry via COPY (pg_dump excluded ts_kv data; schema is present).
    if tmode != TelemetryMode::None {
        steps.step(match tmode {
            TelemetryMode::All => "Exporting telemetry (all)".to_string(),
            TelemetryMode::LastDays(d) => format!("Exporting telemetry (last {d} days)"),
            TelemetryMode::None => unreachable!(),
        });
        // Month by month, so a damaged part costs one month.
        let since_ms: Option<i64> = match tmode {
            TelemetryMode::LastDays(d) => Some(chrono::Utc::now().timestamp_millis() - d as i64 * 86_400_000),
            _ => None,
        };
        // An empty ts_kv (a ClickHouse install keeps its history there) has
        // no months: no member, rather than a "1970-01" one.
        let range = db.text("SELECT min(ts)::text || ' ' || max(ts)::text FROM ts_kv").await?.unwrap_or_default();
        let mut it = range.split_whitespace().filter_map(|v| v.parse::<i64>().ok());
        let months = match (it.next(), it.next()) {
            (Some(min_ts), Some(max_ts)) => months_between(since_ms.map_or(min_ts, |s| s.max(min_ts)), max_ts),
            _ => Vec::new(),
        };
        for (label, from, to) in months {
            let lower = since_ms.map_or(from, |s| s.max(from));
            let sql = format!("COPY (SELECT * FROM ts_kv WHERE ts >= {lower} AND ts < {to}) TO STDOUT WITH (FORMAT binary)");
            let temp = opts.output.with_extension(format!("telemetry-{label}.tmp"));
            let (sha, bytes, raw) = db.copy_out_compressed(&sql, &temp, opts.zstd_level).await?;
            steps.log(format!("  telemetry {label}: {} raw → {} compressed", human_bytes(raw as i64), human_bytes(bytes as i64)));
            members.push(StagedMember {
                archive_path: format!("{TELEMETRY_MONTH_PREFIX}{label}.copy.zst"),
                temp,
                sha256: sha,
                bytes,
            });
        }
    }

    // ClickHouse telemetry: history by monthly partition, then latest values.
    let mut ch_section = None;
    if let (Some(ch), true) = (&opts.clickhouse, tmode != TelemetryMode::None) {
        steps.step("Exporting ClickHouse telemetry");
        let since = match tmode {
            TelemetryMode::LastDays(d) => Some(crate::clickhouse::partition_days_ago(d)),
            _ => None,
        };
        let ch = ch.clone();
        let base = opts.output.clone();
        let level = opts.zstd_level;
        let sink_ch = sink.clone();
        let (version, exported) = tokio::task::spawn_blocking(move || {
            crate::clickhouse::export_all(&ch, since.as_deref(), &base, level, &|line| {
                sink_ch(crate::progress::Progress::Log { line })
            })
        })
        .await
        .map_err(|e| Error::msg(format!("ClickHouse export panicked: {e}")))??;
        let mut entries = Vec::new();
        for e in exported {
            members.push(StagedMember {
                archive_path: e.entry.member.clone(),
                temp: e.temp,
                sha256: e.sha256,
                bytes: e.bytes,
            });
            entries.push(e.entry);
        }
        ch_section = Some(crate::clickhouse::ChSection {
            database: opts.clickhouse.as_ref().map(|c| c.database.clone()).unwrap_or_default(),
            version,
            members: entries,
        });
    }

    // File stores (only ones passed in — caller filters by selected+reachable).
    for fs in &opts.file_stores {
        steps.step(format!("Archiving file store '{}'", fs.id));
        if !files::reachable(&fs.path) {
            steps.log(format!("  skipping '{}': {} not readable", fs.id, fs.path.display()));
            continue;
        }
        let member = format!("files/{}.tar.zst", fs.id);
        let temp = opts.output.with_extension(format!("{}.fs.tmp", fs.id));
        let (sha, bytes) = files::archive_dir(&fs.path, &temp, opts.zstd_level)?;
        members.push(StagedMember {
            archive_path: member.clone(),
            temp,
            sha256: sha,
            bytes,
        });
        file_store_entries.push(FileStoreEntry {
            id: fs.id.clone(),
            category_id: fs.category_id.clone(),
            source_path: fs.path.display().to_string(),
            member,
        });
    }

    steps.step("Writing manifest and archive");
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        tool_version: crate::VERSION.to_string(),
        created_at: chrono::Utc::now(),
        source: SourceInfo {
            database: server.database,
            postgres_version: server.postgres_version,
            timescaledb_version: server.timescaledb_version,
            sentient_version: opts.origin.sentient_version.clone(),
            schema_version: opts.origin.schema_version,
            database_type: opts.origin.database_type.clone(),
            host: opts.origin.host.clone(),
        },
        components: report
            .iter()
            .map(|c| ComponentEntry {
                id: c.id.clone(),
                name: c.name.clone(),
                selected: opts.selection.is_included(&c.id),
                tables: c.tables.clone(),
                bytes: c.bytes,
                rows: c.rows,
            })
            .collect(),
        telemetry: match tmode {
            TelemetryMode::None => TelemetrySelection::None,
            TelemetryMode::All => TelemetrySelection::All,
            TelemetryMode::LastDays(d) => TelemetrySelection::Range { days: d },
        },
        files: members
            .iter()
            .map(|m| FileEntry {
                path: m.archive_path.clone(),
                bytes: m.bytes,
                sha256: m.sha256.clone(),
            })
            .collect(),
        file_stores: file_store_entries.clone(),
        encryption: if opts.passphrase.is_some() {
            EncryptionInfo { scheme: "age".into() }
        } else {
            EncryptionInfo::none()
        },
        clickhouse: ch_section,
        platform: opts.origin.platform.clone(),
    };
    let files = write_archive(&opts.output, &manifest, &members, opts.passphrase.as_deref(), opts.part_size)?;
    for m in &members {
        let _ = std::fs::remove_file(&m.temp);
    }
    let sidecar = crate::parts::write_sidecar(&opts.output, &files)?;

    let archive_bytes = files.iter().map(|(_, _, b)| *b).sum();
    steps.done(format!("Backup written: {}", opts.output.display()));
    Ok(BackupSummary {
        output: opts.output.clone(),
        archive_bytes,
        dump_sha256: dump_sha,
        file_stores: file_store_entries.len(),
        files,
        sidecar,
    })
}

/// pg_dump (custom format) → stdout → zstd → temp file, hashing the compressed
/// bytes. Returns (temp path, sha256-hex, byte length).
fn dump_compressed(
    tools: &PgTools,
    cfg: &ConnConfig,
    opts: &BackupOptions,
    ts_kv_partitions: &[String],
    sink: ProgressFn,
) -> Result<(PathBuf, String, u64)> {
    let mut args: Vec<String> = vec![
        "--format=custom".into(),
        "--verbose".into(),
        "--no-password".into(),
        "-d".into(),
        crate::pg_tools::conninfo(cfg),
    ];
    for a in exclude_data_args(&opts.selection, ts_kv_partitions) {
        args.push(a);
    }

    let mut child = crate::pg_tools::command(&tools.pg_dump)
        .args(&args)
        .env("PGPASSWORD", &cfg.password)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("spawning pg_dump: {e}")))?;

    let mut stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let sink_err = sink.clone();
    let err_thread = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in io::BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
            sink_err(Progress::Log { line });
        }
    });

    let tmp = opts.output.with_extension("dump.tmp");
    let file = File::create(&tmp).map_err(|e| Error::msg(format!("creating {}: {e}", tmp.display())))?;
    let mut hw = HashingWriter::new(file);
    {
        let mut enc = zstd::stream::Encoder::new(&mut hw, opts.zstd_level)
            .map_err(|e| Error::msg(format!("zstd: {e}")))?;
        io::copy(&mut stdout, &mut enc).map_err(|e| Error::msg(format!("streaming dump: {e}")))?;
        enc.finish().map_err(|e| Error::msg(format!("zstd finish: {e}")))?;
    }
    let status = child.wait().map_err(|e| Error::msg(e.to_string()))?;
    let _ = err_thread.join();
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::msg(format!("pg_dump failed ({status})")));
    }
    let (sha, bytes) = hw.finish();
    Ok((tmp, sha, bytes))
}

fn write_archive(
    output: &PathBuf,
    manifest: &Manifest,
    members: &[StagedMember],
    passphrase: Option<&str>,
    part_size: Option<u64>,
) -> Result<Vec<(PathBuf, String, u64)>> {
    let f = crate::parts::PartWriter::new(output, part_size.unwrap_or(u64::MAX));
    match passphrase {
        // Passphrase → wrap the tar stream in an age (scrypt + ChaCha20-Poly1305)
        // encryptor so the whole .sentient-backup file is locked.
        Some(pw) => {
            let enc = age::Encryptor::with_user_passphrase(age::secrecy::Secret::new(pw.to_owned()));
            let writer = enc
                .wrap_output(f)
                .map_err(|e| Error::msg(format!("encrypting: {e}")))?;
            let writer = write_tar(writer, manifest, members)?;
            let parts = writer.finish().map_err(|e| Error::msg(format!("finalizing encryption: {e}")))?;
            parts.finish().map_err(|e| Error::msg(format!("closing the archive: {e}")))
        }
        None => {
            let parts = write_tar(f, manifest, members)?;
            parts.finish().map_err(|e| Error::msg(format!("closing the archive: {e}")))
        }
    }
}

/// Calendar months (UTC) from `from_ms` to `to_ms` inclusive:
/// ("2026-11", first ms of the month, first ms of the next).
pub fn months_between(from_ms: i64, to_ms: i64) -> Vec<(String, i64, i64)> {
    use chrono::{Datelike, TimeZone, Utc};
    let (Some(a), Some(b)) = (Utc.timestamp_millis_opt(from_ms).single(), Utc.timestamp_millis_opt(to_ms).single()) else { return Vec::new() };
    if to_ms < from_ms {
        return Vec::new();
    }
    let mut out = Vec::new();
    let (mut y, mut m) = (a.year(), a.month());
    loop {
        let start = Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).single().map(|t| t.timestamp_millis()).unwrap_or(0);
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        let end = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).single().map(|t| t.timestamp_millis()).unwrap_or(i64::MAX);
        out.push((format!("{y:04}-{m:02}"), start, end));
        if (y, m) >= (b.year(), b.month()) {
            break;
        }
        (y, m) = (ny, nm);
    }
    out
}

#[cfg(test)]
mod month_tests {
    #[test]
    fn months_cover_the_range() {
        // 2025-12-15 to 2026-02-01
        let m = super::months_between(1_765_800_000_000, 1_769_904_000_000);
        let labels: Vec<&str> = m.iter().map(|(l, _, _)| l.as_str()).collect();
        assert_eq!(labels, ["2025-12", "2026-01", "2026-02"]);
        assert_eq!(m[0].2, m[1].1);
        assert!(super::months_between(10, 5).is_empty());
    }
}

/// Stream the manifest + members into a tar over any writer; returns the writer
/// (tar trailer written) so an encryption layer can be finalized after.
fn write_tar<W: std::io::Write>(w: W, manifest: &Manifest, members: &[StagedMember]) -> Result<W> {
    let mut tar = tar::Builder::new(w);
    let mj = serde_json::to_vec_pretty(manifest).map_err(|e| Error::msg(e.to_string()))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(mj.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", &mj[..])
        .map_err(|e| Error::msg(e.to_string()))?;
    for m in members {
        let mut df = File::open(&m.temp).map_err(|e| Error::msg(e.to_string()))?;
        tar.append_file(&m.archive_path, &mut df)
            .map_err(|e| Error::msg(e.to_string()))?;
    }
    tar.into_inner().map_err(|e| Error::msg(e.to_string()))
}

#[cfg(test)]
mod exclude_tests {
    use super::*;

    /// A full backup still carries the licence rows, never the server's own
    /// licence state; without the licensing category neither travels.
    #[test]
    fn the_licence_state_rows_never_go_in_a_backup() {
        let full = exclude_data_args(&Selection::full(), &[]);
        assert!(full.contains(&"--exclude-table-data=public.license_state".to_string()));
        assert!(!full.iter().any(|a| a == "--exclude-table-data=public.license*"));
        let without = exclude_data_args(&Selection::skipping(&["licensing".into()]), &[]);
        assert!(without.contains(&"--exclude-table-data=public.license*".to_string()));
    }
}
