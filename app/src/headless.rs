// Copyright © 2016-2026 The SENTIENT Authors

//! `--job …`: the maintenance work without a window, for servers without a
//! desktop and for scripts. Progress goes to standard output, one line per
//! step or log line; the exit code is 0 on success.
//!
//! ```text
//! --job migrate <stock|timescaledb|clickhouse>
//! --job resume-migration
//! --job update [<package folder>] [--accept-breaking]
//! --job kept
//! --job remove-kept <name>
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use crate::installation::InstallationRecord;

const USAGE: &str = "usage: --job migrate <stock|timescaledb|clickhouse> | resume-migration | \
update [<package folder>] [--accept-breaking] | kept | remove-kept <name>";

/// A console for the output: release builds on Windows have none of their
/// own, so use the one the job was started from.
fn attach_console() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        // SAFETY: ATTACH_PARENT_PROCESS; fails harmlessly with no parent console.
        unsafe {
            AttachConsole(u32::MAX);
        }
    }
}

fn printer() -> ProgressFn {
    Arc::new(|p| match p {
        Progress::Step { name } => println!("== {name}"),
        Progress::Log { line } => println!("   {}", crate::ui::strip_ansi(&line)),
        Progress::Percent { .. } => {}
        Progress::Done { message } => println!("✓ {message}"),
        Progress::Error { message } => eprintln!("✗ {message}"),
    })
}

pub fn run(args: &[String]) -> i32 {
    attach_console();
    if !crate::elevate::is_admin() {
        eprintln!("Run this from an elevated prompt (Run as administrator).");
        return 2;
    }
    let Some(record) = InstallationRecord::load() else {
        eprintln!("SENTIENT is not installed here (no installation record at {}).", crate::installation::record_path().display());
        return 2;
    };
    record.use_pg_tools();
    let sink = printer();
    let job = args.first().map(String::as_str).unwrap_or("");
    let result: Result<(), String> = match job {
        "migrate" => match args.get(1).and_then(|f| Flavor::parse(f)) {
            Some(target) => crate::migration::run(record, target, sink).map(|_| ()),
            None => Err(USAGE.into()),
        },
        "resume-migration" => crate::migration::resume(record, sink),
        "update" => {
            let accept = args.iter().any(|a| a == "--accept-breaking");
            let package = args.iter().skip(1).find(|a| !a.starts_with("--")).map(PathBuf::from);
            crate::updating::run(record, package.as_deref(), accept, sink).map(|_| ())
        }
        "kept" => crate::kept::list(&record).map(|items| {
            if items.is_empty() {
                println!("Nothing kept.");
            }
            for k in items {
                println!("{}\t{}\t{}", k.name, crate::kept::human(k.bytes), k.detail);
            }
        }),
        "remove-kept" => match args.get(1) {
            Some(name) => crate::kept::list(&record).and_then(|items| {
                let item = items.into_iter().find(|k| &k.name == name).ok_or(format!("Nothing kept is called {name}."))?;
                crate::kept::remove(&record, &item, sink)
            }),
            None => Err(USAGE.into()),
        },
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("✗ {e}");
            1
        }
    }
}
