// Copyright © 2016-2026 The SENTIENT Authors

//! `--job …`: the maintenance work without a window, for servers without a
//! desktop and for scripts. Progress goes to standard output, one line per
//! step or log line; the exit code is 0 on success.
//!
//! `--json` first writes progress as JSON lines instead (the manager reads
//! them when it runs a job as root on Linux, crate::rootjob).
//!
//! ```text
//! --job [--json] install <plan.json>   (unattended install; the wizard's plan)
//! --job [--json] migrate <stock|timescaledb|clickhouse>
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

const USAGE: &str = "usage: --job install <plan.json> | migrate <stock|timescaledb|clickhouse> | resume-migration | \
update [<package folder>] [--accept-breaking] | backup <file> | restore <file> <database> [<key file>] | \
use-restored <database> | repair | uninstall [--delete-data] | apply-settings <file.json> | kept | remove-kept <name>";

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
        // Data lines of a root job (crate::kept::encode) are printed decoded.
        Progress::Log { line } if line.starts_with("KEPT\t") => {}
        Progress::Log { line } => println!("   {}", crate::ui::strip_ansi(&line)),
        Progress::Percent { .. } => {}
        Progress::Done { message } => println!("✓ {message}"),
        Progress::Error { message } => eprintln!("✗ {message}"),
    })
}

fn json_printer() -> ProgressFn {
    Arc::new(|p| {
        if let Ok(line) = serde_json::to_string(&p) {
            println!("{line}");
        }
    })
}

pub fn run(args: &[String]) -> i32 {
    attach_console();
    let json = args.first().is_some_and(|a| a == "--json");
    let args = if json { &args[1..] } else { args };
    if !crate::elevate::is_admin() {
        eprintln!("Run this from an elevated prompt (Run as administrator).");
        return 2;
    }
    let job = args.first().map(String::as_str).unwrap_or("");
    if job == "install" {
        let sink = if json { json_printer() } else { printer() };
        let result = args
            .get(1)
            .ok_or_else(|| USAGE.to_string())
            .and_then(|path| std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}")))
            .and_then(|text| serde_json::from_str::<crate::installer::InstallPlan>(&text).map_err(|e| format!("the plan: {e}")))
            .and_then(|plan| crate::installer::run_install(plan, sink.clone()));
        return match result {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("✗ {e}");
                1
            }
        };
    }
    let Some(record) = InstallationRecord::load() else {
        eprintln!("SENTIENT is not installed here (no installation record at {}).", crate::installation::record_path().display());
        return 2;
    };
    let sink = if json { json_printer() } else { printer() };
    // Native Linux keeps its passwords and services root-only: hand the job
    // to a root copy of this program (one password prompt), as the window does.
    if crate::rootjob::needed(&record) {
        return match crate::rootjob::run(args, &sink) {
            Ok(lines) => {
                if args.first().is_some_and(|a| a == "kept") {
                    let items: Vec<_> = lines.iter().filter_map(|l| crate::kept::decode(l)).collect();
                    if items.is_empty() {
                        println!("Nothing kept.");
                    }
                    for k in items {
                        println!("{}\t{}\t{}", k.name, crate::kept::human(k.bytes), k.detail);
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("✗ {e}");
                1
            }
        };
    }
    record.use_pg_tools();
    let result: Result<(), String> = match job {
        "migrate" => match args.get(1).and_then(|f| Flavor::parse(f)) {
            Some(target) => crate::migration::run(record, target, sink.clone()).map(|_| ()),
            None => Err(USAGE.into()),
        },
        "resume-migration" => crate::migration::resume(record, sink.clone()),
        "update" => {
            let accept = args.iter().any(|a| a == "--accept-breaking");
            let package = args.iter().skip(1).find(|a| !a.starts_with("--")).map(PathBuf::from);
            crate::updating::run(record, package.as_deref(), accept, sink.clone()).map(|_| ())
        }
        "backup" => match args.get(1) {
            Some(file) => crate::backup_jobs::backup(&record, std::path::Path::new(file), sink.clone()),
            None => Err(USAGE.into()),
        },
        "restore" => match (args.get(1), args.get(2)) {
            (Some(file), Some(db)) => crate::backup_jobs::restore(
                &record,
                std::path::Path::new(file),
                db,
                args.get(3).map(std::path::Path::new),
                sink.clone(),
            ),
            _ => Err(USAGE.into()),
        },
        "use-restored" => match args.get(1) {
            Some(name) => crate::restore_switch::run(&record, name, sink.clone()).map(|_| ()),
            None => Err(USAGE.into()),
        },
        "repair" => crate::maintenance::run_repair(record, sink.clone()),
        "uninstall" => {
            let keep = !args.iter().any(|a| a == "--delete-data");
            crate::maintenance::run_uninstall(record, keep, sink.clone())
        }
        "apply-settings" => match args.get(1) {
            Some(file) => std::fs::read_to_string(file)
                .map_err(|e| format!("read {file}: {e}"))
                .and_then(|t| serde_json::from_str::<std::collections::HashMap<String, String>>(&t).map_err(|e| format!("{file}: {e}")))
                .and_then(|values| crate::settings::apply(record, &values, sink.clone()).map(|_| ())),
            None => Err(USAGE.into()),
        },
        "kept" => crate::kept::list(&record).map(|items| {
            if json {
                for k in items {
                    sink(Progress::Log { line: crate::kept::encode(&k) });
                }
                return;
            }
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
                crate::kept::remove(&record, &item, sink.clone())
            }),
            None => Err(USAGE.into()),
        },
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            if json {
                sink(Progress::Error { message: e });
            } else {
                eprintln!("✗ {e}");
            }
            1
        }
    }
}
