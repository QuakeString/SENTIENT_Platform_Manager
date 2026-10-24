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
//! --job install-manager [--no-shortcuts] (the Platform Manager alone)
//! --job remove-manager                  (the Platform Manager installed alone)
//! --job [--json] migrate <stock|timescaledb|clickhouse>
//! --job resume-migration
//! --job update [<package folder> | --latest] [--accept-breaking]
//! --job rollback                        (back to what the last update replaced)
//! --job rollback-info
//! --job find-existing                   (SENTIENT instances the Platform Manager does not manage yet)
//! --job add-existing [<n> | --native] [--pg-admin-password <pw>]
//!                                       (add the n-th one found, 1 by default, or the native one)
//! --job open-at-signin <kiosk|window|off> (open SENTIENT when someone signs in)
//! --job auto-signin <account|off>       (sign in by itself after a restart; on Windows
//!                                       the password comes from SPM_AUTOSIGNIN_PASSWORD)
//! --job take-over-compose               (an added Docker instance: use SPM's compose file)
//! --job kept
//! --job remove-kept <name>
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use sentient_installer_core::native::safe_update::RollbackPoint;

use crate::installation::InstallationRecord;

const USAGE: &str = "usage: --job install <plan.json> | install-manager [--no-shortcuts] | remove-manager | migrate <stock|timescaledb|clickhouse> | resume-migration | \
update [<package folder> | --latest] [--accept-breaking] | rollback | rollback-info | backup <file> | restore <file> <database> [<key file>] | \
use-restored <database> | find-existing | add-existing [<n> | --native] | take-over-compose | open-at-signin <kiosk|window|off> | auto-signin <account|off> | repair | uninstall [--delete-data] | apply-settings <file.json> | kept | remove-kept <name>";

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
        Progress::Log { line } if line.starts_with("KEPT\t") || line.starts_with("ROLLBACK\t") || line.starts_with("FOUND\t") => {}
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
    if job == "install-manager" || job == "remove-manager" {
        let sink = if json { json_printer() } else { printer() };
        let result = if job == "remove-manager" {
            if InstallationRecord::load().is_some() {
                Err("SENTIENT is installed: uninstall it instead (--job uninstall), which removes the Platform Manager too.".into())
            } else {
                crate::maintenance::run_remove_manager(sink)
            }
        } else {
            let shortcuts = !args.iter().any(|a| a == "--no-shortcuts");
            let s = crate::self_install::Shortcuts {
                autostart: false,
                desktop_sentient: false,
                desktop_manager: shortcuts,
                start_menu: shortcuts,
                kiosk: false,
                open_at_signin: false,
                auto_signin: String::new(),
            };
            crate::installer::run_manager_only(s, sink)
        };
        return match result {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("✗ {e}");
                1
            }
        };
    }
    if job == "find-existing" || job == "add-existing" {
        let sink = if json { json_printer() } else { printer() };
        let found = crate::adopt::find();
        if job == "find-existing" {
            if json {
                for c in &found {
                    sink(Progress::Log { line: crate::adopt::encode(c) });
                }
            } else {
                print_candidates(&found);
            }
            return 0;
        }
        let chosen = if args.iter().any(|a| a == "--native") {
            found.iter().find(|c| c.record.engine == crate::installation::Engine::Native).ok_or_else(|| "No SENTIENT service was found on this computer.".to_string())
        } else {
            let n: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(1);
            let n = if found.len() == 1 { 1 } else { n };
            found.get(n.saturating_sub(1)).ok_or_else(|| {
                if found.is_empty() { "No existing SENTIENT instance was found on this computer.".to_string() } else { format!("Only {} instance(s) were found.", found.len()) }
            })
        };
        let pg_admin = args.windows(2).find(|w| w[0] == "--pg-admin-password").map(|w| w[1].clone());
        let result = chosen.and_then(|c| crate::adopt::adopt_as_needed(c, pg_admin, sink.clone()));
        return match result {
            Ok(()) => 0,
            Err(e) => {
                if json {
                    sink(Progress::Error { message: e });
                } else {
                    eprintln!("✗ {e}");
                }
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
                if args.first().is_some_and(|a| a == "rollback-info") {
                    print_rollback_point(lines.iter().find_map(|l| RollbackPoint::decode(l)).as_ref());
                }
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
            let mut package = args.iter().skip(1).find(|a| !a.starts_with("--")).map(PathBuf::from);
            // --latest: the newest release from the releases repository.
            if package.is_none() && args.iter().any(|a| a == "--latest") && record.platform().needs_package() {
                match crate::online::download(None, &sink) {
                    Ok((_, dir)) => package = Some(dir),
                    Err(e) => {
                        eprintln!("✗ {e}");
                        return 1;
                    }
                }
            }
            crate::updating::run(record, package.as_deref(), accept, sink.clone()).map(|_| ())
        }
        "rollback" => crate::updating::rollback(record, sink.clone()).map(|_| ()),
        "rollback-info" => crate::updating::rollback_point(&record).map(|point| {
            if json {
                if let Some(p) = point {
                    sink(Progress::Log { line: p.encode() });
                }
                return;
            }
            print_rollback_point(point.as_ref());
        }),
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
        "open-at-signin" => match args.get(1).map(String::as_str) {
            Some("kiosk") => sentient_installer_core::kiosk::set_open_at_signin(&sink, &crate::self_install::manager_exe(), Some(true)),
            Some("window") => sentient_installer_core::kiosk::set_open_at_signin(&sink, &crate::self_install::manager_exe(), Some(false)),
            Some("off") => sentient_installer_core::kiosk::set_open_at_signin(&sink, &crate::self_install::manager_exe(), None),
            _ => Err(USAGE.into()),
        },
        "auto-signin" => match args.get(1).map(String::as_str) {
            Some("off") => sentient_installer_core::autologin::set(&sink, None),
            // The password stays off the command line, where other users see it.
            Some(user) => {
                let password = std::env::var("SPM_AUTOSIGNIN_PASSWORD").unwrap_or_default();
                sentient_installer_core::autologin::set(&sink, Some((user, &password)))
            }
            None => Err(USAGE.into()),
        },
        "take-over-compose" => crate::adopt::take_over_compose(record, sink.clone()).map(|_| ()),
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

fn print_rollback_point(point: Option<&RollbackPoint>) {
    match point {
        Some(p) => println!("Can roll back to {}.", p.describe()),
        None => println!("Nothing to roll back to."),
    }
}

fn print_candidates(found: &[crate::adopt::Candidate]) {
    if found.is_empty() {
        println!("No existing SENTIENT instance was found on this computer.");
    }
    for (i, c) in found.iter().enumerate() {
        println!("{}. SENTIENT {}", i + 1, c.record.sentient_version);
        for (label, value) in c.facts() {
            println!("   {label}: {value}");
        }
        match &c.problem {
            Some(p) => println!("   Cannot be added: {p}"),
            None if c.record.engine == crate::installation::Engine::Native => println!("   Can be added: --job add-existing --native"),
            None => println!("   Can be added: --job add-existing {}", i + 1),
        }
        for n in &c.notes {
            println!("   Note: {n}");
        }
    }
}
