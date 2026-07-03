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
}

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
    let mut steps = Steps::new(sink.clone(), 6 + u32::from(has_telemetry));
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

    steps.step("Preparing (timescaledb_pre_restore)");
    if has_timescale {
        db.batch("CREATE EXTENSION IF NOT EXISTS timescaledb").await?;
        db.batch("SELECT timescaledb_pre_restore()").await?;
    }

    steps.step("Restoring database (pg_restore)");
    let dump_tmp = members
        .get(DUMP_MEMBER)
        .cloned()
        .ok_or_else(|| Error::msg("backup has no database dump"))?;
    let tools = PgTools::resolve()?;
    let cfg2 = cfg.clone();
    let sink2 = sink.clone();
    let restore_res =
        tokio::task::spawn_blocking(move || pg_restore_stream(&tools, &cfg2, &dump_tmp, sink2))
            .await
            .map_err(|e| Error::msg(format!("restore task panicked: {e}")))?;

    if has_timescale {
        steps.step("Finalizing (timescaledb_post_restore)");
        db.batch("SELECT timescaledb_post_restore()").await?;
    }
    if let Err(e) = restore_res {
        cleanup();
        return Err(e);
    }

    // Telemetry (after post_restore: ts_kv is a live hypertable again).
    if has_telemetry {
        steps.step("Restoring telemetry (COPY)");
        if let Some(tmp) = members.get(crate::backup::TELEMETRY_MEMBER) {
            let rows = db
                .copy_in_compressed("COPY ts_kv FROM STDIN WITH (FORMAT binary)", tmp)
                .await?;
            steps.log(format!("  {rows} telemetry rows"));
        }
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

/// True if the archive is an age-encrypted (password-protected) file.
pub fn is_encrypted(path: &Path) -> Result<bool> {
    let mut f = File::open(path).map_err(|e| Error::msg(format!("opening backup: {e}")))?;
    let mut magic = [0u8; 22];
    let n = f.read(&mut magic).map_err(|e| Error::msg(e.to_string()))?;
    Ok(magic[..n].starts_with(b"age-encryption.org"))
}

/// Extract manifest.json + every `db/*` and `files/*` member to temp files.
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
        } else if path.starts_with("db/") || path.starts_with("files/") {
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

fn pg_restore_stream(
    tools: &PgTools,
    cfg: &ConnConfig,
    dump_zst: &Path,
    sink: ProgressFn,
) -> Result<()> {
    // Pass a full conninfo (with TCP keepalives) via -d so long server-side
    // steps like index ATTACH don't get their idle connection dropped by a
    // NAT/firewall on a remote/wifi link. Password stays in PGPASSWORD.
    let conninfo = crate::pg_tools::conninfo(cfg);
    let mut child = crate::pg_tools::command(&tools.pg_restore)
        .args(["--no-password", "--verbose", "--exit-on-error", "-d", &conninfo])
        .env("PGPASSWORD", &cfg.password)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("spawning pg_restore: {e}")))?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let dump_path = dump_zst.to_path_buf();
    let feeder = std::thread::spawn(move || -> io::Result<()> {
        let zf = File::open(&dump_path)?;
        let mut dec = zstd::stream::Decoder::new(zf)?;
        io::copy(&mut dec, &mut stdin)?;
        use std::io::Write;
        stdin.flush()
    });
    let sink_o = sink.clone();
    let out_t = std::thread::spawn(move || drain(stdout, sink_o));
    let sink_e = sink.clone();
    let err_t = std::thread::spawn(move || drain(stderr, sink_e));

    let status = child.wait().map_err(|e| Error::msg(e.to_string()))?;
    let feed = feeder.join();
    let _ = out_t.join();
    let _ = err_t.join();

    if let Ok(Err(e)) = feed {
        return Err(Error::msg(format!("feeding dump to pg_restore: {e}")));
    }
    if !status.success() {
        return Err(Error::msg(format!("pg_restore failed ({status})")));
    }
    Ok(())
}

fn drain<R: Read>(r: R, sink: ProgressFn) {
    use std::io::BufRead;
    for line in io::BufReader::new(r).lines().map_while(std::result::Result::ok) {
        sink(Progress::Log { line });
    }
}
