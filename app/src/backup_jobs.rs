// Copyright © 2016-2026 The SENTIENT Authors

//! Backup and restore without the window (`--job backup`, `--job restore`),
//! for scripts and scheduled backups: everything included, ClickHouse's
//! history too on a ClickHouse installation; a restore goes into a new
//! database (and a new ClickHouse database of the same name) beside the live
//! data, ready for `--job use-restored`. A backup from another database type
//! is converted to this installation's.

use std::path::Path;
use std::sync::Arc;

use sentient_backup_core::backup::{self, BackupOptions, Selection};
use sentient_backup_core::clickhouse::{self, ChConn};
use sentient_backup_core::db;
use sentient_backup_core::progress as bk;
use sentient_backup_core::restore::{self, RestoreOptions};
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use crate::installation::InstallationRecord;
use crate::migration::Setup;

fn bridge(sink: ProgressFn) -> bk::ProgressFn {
    Arc::new(move |p| {
        sink(match p {
            bk::Progress::Step { name, .. } => Progress::Step { name },
            bk::Progress::Log { line } => Progress::Log { line },
            bk::Progress::Percent { value } => Progress::Percent { value },
            bk::Progress::Done { message } => Progress::Log { line: message },
        })
    })
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(future))
        .map_err(|e| format!("start the runtime: {e}"))
}

fn ch_of(setup: &Setup) -> Option<ChConn> {
    (setup.flavor() == Flavor::Clickhouse).then(|| setup.ch())
}

pub fn backup(record: &InstallationRecord, output: &Path, sink: ProgressFn) -> Result<(), String> {
    let setup = Setup::current(record)?;
    let opts = BackupOptions {
        output: output.to_path_buf(),
        selection: Selection::skipping(&[]),
        file_stores: Vec::new(),
        zstd_level: 10,
        passphrase: std::env::var("SBR_BACKUP_PASSWORD").ok().filter(|p| !p.is_empty()),
        clickhouse: ch_of(&setup),
    };
    let summary = block_on(backup::run(&setup.pg(), &opts, bridge(sink.clone())))?.map_err(|e| e.to_string())?;
    sink(Progress::Done {
        message: format!(
            "Saved {} ({}), checksum {}…",
            summary.output.display(),
            db::human_bytes(summary.archive_bytes as i64),
            summary.dump_sha256.get(..12).unwrap_or_default()
        ),
    });
    Ok(())
}

pub fn restore(record: &InstallationRecord, input: &Path, target: &str, sink: ProgressFn) -> Result<(), String> {
    if target.is_empty() || !target.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("'{target}' is not a usable database name."));
    }
    let setup = Setup::current(record)?;
    let live = setup.pg();
    if target == live.dbname {
        return Err("A restore goes into a new database, not the one SENTIENT uses.".into());
    }
    let conn = db::ConnConfig { dbname: target.to_string(), ..live.clone() };
    let ch_target = ch_of(&setup).map(|c| c.with_database(target));
    let live_ch = setup.ch().database;
    let opts = RestoreOptions {
        input: input.to_path_buf(),
        allow_nonempty: false,
        file_store_paths: vec![
            ("reports".into(), record.state_dir.join("reports")),
            ("vc-repos".into(), record.state_dir.join("vc-repos")),
        ],
        passphrase: std::env::var("SBR_BACKUP_PASSWORD").ok().filter(|p| !p.is_empty()),
        clickhouse: ch_target.clone(),
        telemetry_into: Some(crate::migration::telemetry_into(record)?),
    };
    block_on(async {
        sink(Progress::Step { name: format!("Creating the empty database {target}") });
        db::create_database(&live, target).await.map_err(|e| e.to_string())?;
        if let Some(ch) = &ch_target {
            sink(Progress::Step { name: format!("Creating the ClickHouse database {target}") });
            clickhouse::clone_schema(ch, &live_ch).map_err(|e| e.to_string())?;
        }
        restore::run(&conn, &opts, bridge(sink.clone())).await.map_err(|e| e.to_string())
    })??;
    crate::restore_switch::match_schema(record, &live, &setup.ch().password, target, &sink)?;
    sink(Progress::Done {
        message: format!("Restored into {target}. Put it into service with --job use-restored {target}."),
    });
    Ok(())
}
