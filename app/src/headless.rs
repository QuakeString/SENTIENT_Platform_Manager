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
//! --job update [<package folder> | --latest | --version <release>] [--accept-breaking]
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
update [<package folder> | --latest | --version <release>] [--accept-breaking] | rollback | rollback-info | backup <file> | restore <file> <database> [<key file>] | \
use-restored <database> | find-existing | add-existing [<n> | --native] | take-over-compose | open-at-signin <kiosk|window|off> | auto-signin <account|off> | repair | control <start|stop|restart> | uninstall [--delete-data] | apply-settings <file.json> | apply-changes | scheduled-backup | backup-schedule | admin <sentient-admin args> | replace <backup> [--delete-old] [--passphrase-stdin] | status-json | monitor-json | services-json | dbstatus-json | backups-json | settings-json | logs [<lines>] | kept | remove-kept <name> | \
gateway-status | gateway-install <host> <port> | gateway-update | gateway-control <start|stop|restart>";

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
        Progress::Log { line } => println!("   {}", crate::maintenance_ops::strip_ansi(&line)),
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
    // The gateway on this computer (crate::manager::gateway's actions):
    // gateway-status | gateway-install <host> <port> (token in
    // SPM_GATEWAY_TOKEN, page password in SPM_GATEWAY_PAGE_PASSWORD) |
    // gateway-update | gateway-control <start|stop|restart>. Installs and
    // updates to the newest gateway release on the update channel.
    if job.starts_with("gateway-") {
        use sentient_installer_core::gateway;
        let sink = if json { json_printer() } else { printer() };
        let channel = crate::updates::Prefs::load().channel;
        let newest = || gateway::newest(channel).and_then(|o| o.ok_or_else(|| "No gateway release on this channel.".to_string()));
        let result = match job {
            "gateway-status" => {
                let s = gateway::status();
                println!(
                    "installed={} version={} running={} host={} port={} page={} container={} managed={}",
                    s.installed,
                    s.version.unwrap_or_default(),
                    s.running,
                    s.host.unwrap_or_default(),
                    s.port.map(|p| p.to_string()).unwrap_or_default(),
                    s.page_url.unwrap_or_else(|| "off".into()),
                    s.container.unwrap_or_default(),
                    s.managed
                );
                Ok(())
            }
            "gateway-install" => match (args.get(1), args.get(2).and_then(|p| p.parse().ok())) {
                (Some(host), Some(port)) => newest().and_then(|offer| {
                    let settings = gateway::Settings {
                        host: host.clone(),
                        port,
                        token: std::env::var("SPM_GATEWAY_TOKEN").unwrap_or_default(),
                        ui_password: std::env::var("SPM_GATEWAY_PAGE_PASSWORD").unwrap_or_default(),
                    };
                    gateway::install(&sink, &offer, &settings)
                }),
                _ => Err(USAGE.into()),
            },
            "gateway-update" => newest().and_then(|offer| gateway::update(&sink, &offer)),
            "gateway-control" => match args.get(1) {
                Some(action) => gateway::control(&sink, action),
                None => Err(USAGE.into()),
            },
            _ => Err(USAGE.into()),
        };
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
                crate::maintenance_ops::run_remove_manager(sink)
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
                start_delay: 0,
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
    // After Set up: add the SENTIENT found here when there is exactly one
    // and adding changes nothing on it (only the Platform Manager's own
    // record is written). Otherwise leave the choice to the operator.
    // Always ends with one Done line saying what happened.
    if job == "add-if-one" {
        let sink = if json { json_printer() } else { printer() };
        let found = crate::adopt::find();
        let message = match found.as_slice() {
            [] => "No SENTIENT found on this server.".to_string(),
            [c] if c.problem.is_none() && !c.sets_admin_password => match crate::adopt::adopt_as_needed(c, None, sink.clone()) {
                Ok(()) => format!(
                    "SENTIENT {}{} was found and added. Nothing on it was changed.",
                    c.record.sentient_version,
                    if c.kind.is_empty() { String::new() } else { format!(" ({})", c.kind) }
                ),
                Err(e) => {
                    sink(Progress::Error { message: format!("SENTIENT was found but could not be added: {e}") });
                    return 1;
                }
            },
            [_] => "SENTIENT was found on this server. Adding it needs your choice: open Status to see it.".to_string(),
            many => format!("{} SENTIENT instances were found on this server. Open Status to choose one.", many.len()),
        };
        sink(Progress::Done { message });
        return 0;
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
    // What a remote instance's card shows (crate::remote::status): one JSON line.
    if job == "status-json" {
        println!("{}", serde_json::to_string(&status_json(InstallationRecord::load().as_ref())).unwrap_or_default());
        return 0;
    }
    // The pages of a remote instance read this computer through these
    // (crate::remote): services, database, backups. One JSON line each.
    if job == "services-json" || job == "dbstatus-json" || job == "backups-json" || job == "settings-json" {
        let Some(record) = InstallationRecord::load() else {
            println!("null");
            return 0;
        };
        let line = match job {
            "services-json" => {
                let snap = record.platform().snapshot();
                let list: Vec<crate::remote::RemoteService> = snap
                    .services
                    .into_iter()
                    .map(|s| crate::remote::RemoteService { name: s.name, running: s.running, starting: s.starting, detail: s.detail })
                    .collect();
                serde_json::to_string(&list)
            }
            "dbstatus-json" => serde_json::to_string(&crate::dbstatus::read(&record)),
            "settings-json" => serde_json::to_string(&crate::settings::current(&record)),
            _ => {
                let prefs = crate::backups::BackupPrefs::load();
                serde_json::to_string(&crate::remote::RemoteBackups { folder: prefs.folder.clone(), daily_at: prefs.daily_at.clone(), keep: prefs.keep, list: crate::backups::list(&prefs.folder) })
            }
        };
        println!("{}", line.unwrap_or_default());
        return 0;
    }
    // The top bar's reading of this computer (crate::remote::monitor).
    if job == "monitor-json" {
        let dir = InstallationRecord::load()
            .map(|r| if r.pg_dir.as_os_str().is_empty() { r.install_dir } else { r.pg_dir })
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }));
        println!("{}", serde_json::to_string(&crate::sysmon::sample(&dir)).unwrap_or_default());
        return 0;
    }
    // The last lines of SENTIENT's log: logs [<lines>].
    if job == "logs" {
        let Some(record) = InstallationRecord::load() else {
            eprintln!("SENTIENT is not installed here.");
            return 2;
        };
        let n = args.get(1).and_then(|n| n.parse().ok()).unwrap_or(300);
        // --sources: which logs this install has (JSON); --source <key>:
        // one of them, by its key from that list only; --today: since midnight.
        if args.iter().any(|a| a == "--sources") {
            println!("{}", serde_json::to_string(&crate::logsource::available(&record)).unwrap_or_default());
            return 0;
        }
        let today = args.iter().any(|a| a == "--today");
        match args.windows(2).find(|w| w[0] == "--source").map(|w| w[1].as_str()) {
            Some(key) => match crate::logsource::Source::from_key(key).filter(|s| crate::logsource::available(&record).contains(s)) {
                Some(src) => println!("{}", crate::logsource::read(&record, src, n, today)),
                None => {
                    eprintln!("No such log here: {key}");
                    return 2;
                }
            },
            None => println!("{}", record.platform().logs(n)),
        }
        return 0;
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
            // --version <v>: that published release (a package downloaded and
            // checked, or that image tag). Older than installed goes through
            // the same dry run, which refuses a database it cannot run on.
            let version = args.windows(2).find(|w| w[0] == "--version").map(|w| w[1].clone());
            if args.iter().any(|a| a == "--version") && version.as_deref().is_none_or(|v| v.starts_with("--")) {
                eprintln!("✗ --version needs a release number, e.g. --version 4.4.0.1");
                return 1;
            }
            let mut record = record;
            let mut package = args
                .iter()
                .skip(1)
                .enumerate()
                .find(|(i, a)| !a.starts_with("--") && args.get(*i).is_none_or(|prev| prev != "--version"))
                .map(|(_, a)| PathBuf::from(a));
            if let Some(v) = &version {
                if let Some(installed) = Some(record.sentient_version.as_str()).filter(|s| !s.is_empty()) {
                    if sentient_installer_core::releases::movement(v, installed) == sentient_installer_core::releases::Move::Same {
                        eprintln!("✗ SENTIENT {v} is already installed.");
                        return 1;
                    }
                }
                if record.platform().needs_package() {
                    match crate::online::download(Some(v), &sink) {
                        Ok((_, dir)) => package = Some(dir),
                        Err(e) => {
                            eprintln!("✗ {e}");
                            return 1;
                        }
                    }
                } else {
                    record.stack.image = crate::maintenance_ops::retag(&record.stack.image, v);
                }
            }
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
            Some("kiosk") => crate::self_install::set_open_at_signin(&sink, Some(true)),
            Some("window") => crate::self_install::set_open_at_signin(&sink, Some(false)),
            Some("off") => crate::self_install::set_open_at_signin(&sink, None),
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
        "repair" => crate::maintenance_ops::run_repair(record, sink.clone()),
        // Start, stop or restart SENTIENT (a remote instance's Control).
        "control" => match args.get(1).map(String::as_str) {
            Some(a @ ("start" | "stop" | "restart")) => record.platform().control(sink.clone(), a),
            _ => Err(USAGE.into()),
        },
        "uninstall" => {
            let keep = !args.iter().any(|a| a == "--delete-data");
            crate::maintenance_ops::run_uninstall(record, keep, sink.clone())
        }
        "apply-settings" => match args.get(1) {
            Some(file) => std::fs::read_to_string(file)
                .map_err(|e| format!("read {file}: {e}"))
                .and_then(|t| serde_json::from_str::<std::collections::HashMap<String, String>>(&t).map_err(|e| format!("{file}: {e}")))
                .and_then(|values| {
                    let (values, custom) = crate::settings::split_custom(values);
                    crate::settings::apply_with(record, &values, custom.as_deref(), sink.clone()).map(|_| ())
                }),
            None => Err(USAGE.into()),
        },
        "replace" => match args.get(1) {
            Some(file) => crate::replace::run(
                record,
                std::path::PathBuf::from(file),
                if args.iter().any(|a| a == "--passphrase-stdin") {
                    let mut line = String::new();
                    let _ = std::io::stdin().read_line(&mut line);
                    Some(line.trim_end_matches(['\r', '\n']).to_string()).filter(|p| !p.is_empty())
                } else {
                    std::env::var("SBR_BACKUP_PASSWORD").ok().filter(|p| !p.is_empty())
                },
                args.iter().any(|a| a == "--delete-old"),
                sink.clone(),
            ),
            None => Err(USAGE.into()),
        },
        // sentient-admin as root (native Linux keeps the database address
        // root-only); a password comes on standard input.
        "admin" => {
            let rest: Vec<String> = args[1..].to_vec();
            let input = rest.iter().any(|a| a == "--password-stdin").then(|| {
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                line.trim_end_matches(['\r', '\n']).to_string()
            });
            crate::sentient_admin::run(&record, &rest, input.as_deref()).map(|out| {
                for line in out.lines() {
                    sink(Progress::Log { line: line.to_string() });
                }
            })
        }
        // The daily backup (scheduled task or systemd timer).
        "scheduled-backup" => {
            let prefs = crate::backups::BackupPrefs::load();
            crate::backups::run(&record, &prefs, None, true, sink.clone()).map(|_| ())
        }
        // A backup by hand with the saved settings (native Linux, as root).
        "backup-now" => {
            let pass = args.iter().any(|a| a == "--passphrase-stdin").then(|| {
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                line.trim_end_matches(['\r', '\n']).to_string()
            });
            crate::backups::run(&record, &crate::backups::BackupPrefs::load(), pass.filter(|p| !p.is_empty()), false, sink.clone()).map(|_| ())
        }
        // Register or remove the daily backup from the saved settings.
        "backup-schedule" => crate::backups::apply_schedule(&crate::backups::BackupPrefs::load()),
        "apply-changes" => crate::changes::apply(record, &sink).map(|_| ()),
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

/// The SENTIENT version to show. A stack taken over from its own compose file
/// may record an image tag such as "latest": then the running container's
/// version label says which SENTIENT it is.
fn shown_version(record: &InstallationRecord) -> String {
    let recorded = record.sentient_version.clone();
    let numbered = recorded.split('.').next().is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if numbered || record.engine != crate::installation::Engine::Docker {
        return recorded;
    }
    let label = std::process::Command::new("docker")
        .args(["inspect", "-f", "{{index .Config.Labels \"org.opencontainers.image.version\"}}", &record.stack.names().app])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|v| !v.is_empty() && v != "<no value>" && v != "dev");
    label.unwrap_or_else(|| format!("version unknown (image tag {recorded})"))
}

/// This computer's state for the Platform Manager on another computer.
pub(crate) fn status_json(record: Option<&InstallationRecord>) -> crate::remote::RemoteStatus {
    // The full version, as the window shows it (0.8.0-rc12).
    let manager_version = crate::updates::manager_version().to_string();
    let Some(record) = record else { return crate::remote::RemoteStatus { manager_version, ..Default::default() } };
    let snap = record.platform().snapshot();
    // Without administrator rights the database's size needs a password
    // prompt (native Linux): leave it out rather than ask.
    let db = if crate::dbstatus::needs_admin(record) && !crate::rootjob::is_root() { crate::dbstatus::DbStatus::default() } else { crate::dbstatus::read(record) };
    let database_bytes = db.pg.as_ref().map(|p| p.db_bytes + db.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0));
    let mon = crate::sysmon::sample(&record.state_dir);
    let prefs = crate::backups::BackupPrefs::load();
    let last_backup = crate::backups::list(&prefs.folder).first().map(|e| e.made.clone());
    let uptime = sysinfo::System::uptime();
    crate::remote::RemoteStatus {
        installed: snap.installed,
        serving: snap.serving,
        sentient_version: shown_version(record),
        database: crate::installation::flavor_label(record.flavor()).to_string(),
        database_bytes,
        disk_free: (mon.disk_total > 0).then_some(mon.disk_free),
        last_backup,
        uptime_secs: Some(uptime),
        manager_version,
        running_job: None,
        http_port: Some(record.stack.http_port),
    }
}
