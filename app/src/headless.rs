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
//! --job diagnose-fix <fix json>         (a failure's one-click fix, as the error panel offers it)
//! --job install-manager [--no-shortcuts] (the Platform Manager alone)
//! --job remove-manager                  (the Platform Manager installed alone)
//! --job [--json] migrate <stock|timescaledb|clickhouse>
//! --job resume-migration
//! --job update [<package folder> | --latest | --version <release>] [--accept-breaking]
//! --job rollback                        (back to what the last update replaced)
//! --job rollback-info
//! --job find-existing                   (SENTIENT instances the Platform Manager does not manage yet)
//! --job add-existing [<n> | --native] [--pg-admin-password-stdin]
//!                                       (add the n-th one found, 1 by default, or the native one;
//!                                       the PostgreSQL admin password as one line on standard input.
//!                                       Without it, an unknown one is replaced by a new one)
//! --job add-if-one [--pg-admin-password-stdin]
//!                                       (add the one SENTIENT found; when it needs the PostgreSQL
//!                                       admin password, a "@@NEEDS-PG-ADMIN <kind> | <location>"
//!                                       log line and exit 0, crate::adopt::NEEDS_PG_ADMIN)
//! --job open-at-signin <kiosk|window|off> (open SENTIENT when someone signs in)
//! --job auto-signin <account|off>       (sign in by itself after a restart; on Windows
//!                                       the password comes from SPM_AUTOSIGNIN_PASSWORD)
//! --job take-over-compose               (an added Docker instance: use SPM's compose file)
//! --job agent-info                      (version, commit and agent PROTOCOL, as JSON)
//! --job serve                           (requests on standard input, answers on standard output,
//!                                       one JSON line each: crate::remote::serve)
//! --job agents                          (the server agents this package carries, written out)
//! --job kept
//! --job remove-kept <name>
//! --job uninstall [--delete-data]       (everything Setup installed but the Platform Manager;
//!                                       the data kept unless --delete-data)
//! --job uninstall --parts sentient,postgres,clickhouse,gateway,shortcuts,manager
//!       [--keep-files] [--keep-pg-data] [--keep-ch-data] [--wsl-distro] [--wsl]
//!                                       (the parts picked; pg-database / ch-database drop only
//!                                       SENTIENT's database in one Setup did not install)
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use sentient_installer_core::native::safe_update::RollbackPoint;

use crate::installation::InstallationRecord;

const USAGE: &str = "usage: --job install <plan.json> | diagnose-fix <fix json> | install-manager [--no-shortcuts] | remove-manager | migrate <stock|timescaledb|clickhouse> | resume-migration | migrate-preflight | movedata-json | movedata-check <store> <folder> | movedata-estimate <store=folder,…> | move-data <store=folder,…> | move-data-remove-old <folder> | move-data-cancel | \
update [<package folder> | --latest | --version <release>] [--accept-breaking] | rollback | rollback-info | backup <file> | restore <file> <database> [<key file>] | \
use-restored <database> | restore-inspect <backup> [--passphrase-file <file> | --passphrase-stdin] | restore-precheck <request.json> [--passphrase-file <file>] | restore-run <request.json> [--passphrase-file <file> | --passphrase-stdin] | restore-history-json | restore-log <id> | restore-cancel | restore-remove-db <name> | backup-list <folder> | find-existing | add-if-one [--pg-admin-password-stdin] | add-existing [<n> | --native] [--pg-admin-password-stdin] | take-over-compose [--compose <file>] [--db-password-file <file>] [--ch-password-file <file>] | takeover-plan [--compose <file>] | takeover-check-password [--clickhouse] --db-password-file <file> | open-at-signin <kiosk|window|off> | auto-signin <account|off> | repair | control <start|stop|restart> [--stop-shared-pg] | service <start|stop|restart> <name> | uninstall [--delete-data] | uninstall --parts <sentient,postgres,pg-database,clickhouse,ch-database,gateway,shortcuts,wsl-distro,wsl,manager> [--keep-files] [--keep-pg-data] [--keep-ch-data] [--wsl-distro] [--wsl] | uninstall-facts-json | apply-settings <file.json> | apply-changes | scheduled-backup | backup-schedule | backup-auto <off|HH:MM> <keep> <everything|last90|configuration> | backup-verify <backup> | backup-delete <backup> | backup-folder <folder> <move|leave> | dir-list <folder> | dir-mkdir <folder> <name> | dir-make-writable <folder> [<uid>] | licence-api --request-file <file> | --request-b64 <base64> | admin <sentient-admin args> | replace <backup> [--delete-old] [--passphrase-stdin] | status-json | monitor-json | stats-live [system|services|db] | stats-sample | stats-history <seconds> | stats-install | server-info-json | server-boot-json | server-power <reboot|poweroff> | docker-restart | system-updates | services-json | dbstatus-json | backups-json | settings-json | logs [<lines>] | kept | remove-kept <name> | agent-info | agents | serve | pg-others-json | \
gateway-status | gateway-install <host> <port> | gateway-update | gateway-control <start|stop|restart> | gateway-remove [--delete-data] [--remove-image] | gateway-adopt

Administrator password (native Linux): a job typed in a terminal may ask for it, once for the
whole job. Started without a terminal (a script, a timer, a service), a job never asks: a step
that needs root then fails and says so. Run such a job from a terminal, or as root.";

/// A console for the output, only when there is nowhere else to write: a
/// windowed build started from a console with its output not redirected.
/// The agent (a console program) already has the console, and a piped
/// run (crate::remote) has its pipes: both are left as they are, so the
/// agent never takes over the console it shares with the shell that
/// started it (a windowed program attached to it wrote over the lines
/// that shell printed after it).
fn attach_console() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
            fn GetConsoleWindow() -> *mut core::ffi::c_void;
            fn GetStdHandle(which: u32) -> *mut core::ffi::c_void;
        }
        const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
        // SAFETY: plain queries; ATTACH_PARENT_PROCESS fails harmlessly with
        // no parent console.
        unsafe {
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            let no_output = out.is_null() || out as isize == -1;
            if no_output && GetConsoleWindow().is_null() {
                AttachConsole(u32::MAX);
            }
        }
    }
}

/// The job's last output lines, for the diagnosis of a failure.
static TAIL: std::sync::Mutex<std::collections::VecDeque<String>> = std::sync::Mutex::new(std::collections::VecDeque::new());

fn remember(p: &Progress) {
    if let Progress::Log { line } = p {
        if let Ok(mut t) = TAIL.lock() {
            if t.len() >= 200 {
                t.pop_front();
            }
            t.push_back(line.clone());
        }
    }
}

/// A failed job: what went wrong and the fix
/// (sentient_installer_core::diagnose), then the error. With `--json` the
/// diagnosis is one `DIAG\t<json>` line the window reads (and shows in its
/// error panel); a root copy of this program that already sent one is not
/// diagnosed twice.
fn report_failure(e: String, json: bool, sink: &ProgressFn) {
    use sentient_installer_core::diagnose::{self, Diagnosis, Fix};
    let lines: Vec<String> = TAIL.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default();
    let d = match lines.iter().rev().find_map(|l| Diagnosis::decode(l)) {
        Some(d) => Some(d).filter(|_| !json),
        None => {
            let shown: Vec<String> = lines.into_iter().filter(|l| !l.starts_with(diagnose::DIAG_PREFIX)).collect();
            let d = diagnose::diagnose_failure(&e, &shown, crate::online::downloads_dir("").parent());
            if json {
                sink(Progress::Log { line: d.encode() });
                None
            } else {
                Some(d)
            }
        }
    };
    if json {
        sink(Progress::Error { message: e });
        return;
    }
    eprintln!("✗ {e}");
    if let Some(d) = d {
        eprintln!("  Cause: {}\n  Why:   {}", d.cause, d.why);
        for f in &d.fixes {
            match f {
                Fix::Action { label, action } => eprintln!("  Fix:   {label}  (--job diagnose-fix '{}')", serde_json::to_string(action).unwrap_or_default()),
                Fix::Command { label, command } => eprintln!("  Fix:   {label}: {command}"),
                Fix::Steps { text } => eprintln!("  Fix:   {text}"),
            }
        }
        if !d.is_known() {
            for l in &d.last_lines {
                eprintln!("  | {l}");
            }
        }
    }
}

fn printer() -> ProgressFn {
    Arc::new(|p| {
        remember(&p);
        print_plain(p)
    })
}

fn print_plain(p: Progress) {
    match p {
        Progress::Plan { .. } => {}
        Progress::Step { name } => println!("== {name}"),
        // Data lines of a root job (crate::kept::encode) are printed decoded.
        Progress::Log { line } if line.starts_with("KEPT\t") || line.starts_with("ROLLBACK\t") || line.starts_with("FOUND\t") || line.starts_with(sentient_installer_core::diagnose::DIAG_PREFIX) || crate::restore_flow::is_data(&line) => {}
        // Uninstall's check of what is left (crate::uninstall).
        Progress::Log { line } if sentient_installer_core::removal::Check::decode(&line).is_some() => {
            if let Some(c) = sentient_installer_core::removal::Check::decode(&line) {
                match (c.ok, c.fix) {
                    (true, _) => println!("   ok    {}", c.what),
                    (false, Some(fix)) => println!("   LEFT  {}\n         remove it with: {fix}", c.what),
                    (false, None) => println!("   LEFT  {}", c.what),
                }
            }
        }
        Progress::Log { line } => println!("   {}", crate::maintenance_ops::strip_ansi(&line)),
        Progress::Percent { .. } => {}
        Progress::Done { message } => println!("✓ {message}"),
        Progress::Error { message } => eprintln!("✗ {message}"),
    }
}

fn json_printer() -> ProgressFn {
    Arc::new(|p| {
        remember(&p);
        if let Ok(line) = serde_json::to_string(&p) {
            println!("{line}");
        }
    })
}

pub fn run(args: &[String]) -> i32 {
    let code = run_job(args);
    // Tests count the password prompts a job causes.
    if std::env::var_os("SPM_ELEVATION_STATS").is_some() {
        use sentient_installer_core::elevation;
        eprintln!("elevation: {} password prompt(s), {} root step(s)", elevation::prompts(), elevation::steps());
    }
    code
}

fn run_job(args: &[String]) -> i32 {
    attach_console();
    let json = args.first().is_some_and(|a| a == "--json");
    let args = if json { &args[1..] } else { args };
    // Which Platform Manager this is (crate::remote::reuse): read-only, so
    // before the administrator check; any account may ask.
    if args.first().map(String::as_str) == Some("agent-info") {
        println!("{}", serde_json::to_string(&crate::remote::reuse::AgentInfo::this()).unwrap_or_default());
        return 0;
    }
    // The server agents this package carries, written out where Set up
    // copies them from (a package check: version and size of each).
    if args.first().map(String::as_str) == Some("agents") {
        for (system, file) in crate::remote::carried_agents() {
            match file {
                Ok(Some(path)) => println!("{}\t{}\t{}", system.label(), std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0), path.display()),
                Ok(None) => println!("{}\tnot carried", system.label()),
                Err(e) => println!("{}\t{e}", system.label()),
            }
        }
        return 0;
    }
    // The one connection a Platform Manager keeps to this server
    // (crate::remote::serve): requests on standard input, each answered by
    // a job run as its own process; those check the rights themselves.
    if args.first().map(String::as_str) == Some("serve") {
        return crate::remote::serve::run();
    }
    if !crate::elevate::is_admin() {
        eprintln!("Run this from an elevated prompt (Run as administrator).");
        return 2;
    }
    // Typed in a terminal, the job is the user's own action: its root steps
    // may ask for the password (once, sentient_installer_core::elevation).
    // A timer's job (stats-sample, scheduled-backup) never asks.
    {
        use std::io::IsTerminal as _;
        if std::io::stdin().is_terminal() || std::io::stderr().is_terminal() {
            sentient_installer_core::elevation::allow_in_process();
        }
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
                report_failure(e, json, &sink);
                1
            }
        };
    }
    // One of the error panel's one-click fixes, on this computer (the
    // window sends it to a server's agent the same way):
    // diagnose-fix '{"do":"upgrade-pg-data","family":"arch"}'.
    if job == "diagnose-fix" {
        use sentient_installer_core::diagnose;
        let sink = if json { json_printer() } else { printer() };
        let result = args
            .get(1)
            .ok_or_else(|| USAGE.to_string())
            .and_then(|j| serde_json::from_str::<diagnose::FixAction>(j).map_err(|e| format!("the fix: {e}")))
            .and_then(|a| diagnose::apply(&a, &sink))
            .map(|message| sink(Progress::Done { message }));
        return match result {
            Ok(()) => 0,
            Err(e) => {
                report_failure(e, json, &sink);
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
                sentient_installer_core::distro::gateway_docker::set_cache_dir(crate::updates::prefs_dir());
                let s = gateway::status();
                let kind = s.kind();
                // Values with spaces in them are %20-escaped (the line splits on spaces).
                let esc = |v: Option<String>| v.unwrap_or_default().replace('%', "%25").replace(' ', "%20");
                let recorded = crate::gateway_record::load().is_some_and(|r| r.is(&s));
                println!(
                    "installed={} version={} running={} host={} port={} page={} container={} managed={} kind={} service={} process={} image={} compose={} project={} volumes={} package={} recorded={recorded}",
                    s.installed,
                    s.version.unwrap_or_default(),
                    s.running,
                    s.host.unwrap_or_default(),
                    s.port.map(|p| p.to_string()).unwrap_or_default(),
                    s.page_url.unwrap_or_else(|| "off".into()),
                    s.container.clone().unwrap_or_default(),
                    s.managed,
                    kind,
                    s.service.clone().unwrap_or_default(),
                    s.process.clone().unwrap_or_default().replace(' ', ""),
                    esc(s.image.clone()),
                    esc(s.compose_dir.clone()),
                    esc(s.compose_project.clone()),
                    esc(Some(s.volumes.join(","))),
                    esc(s.package.clone()),
                );
                Ok(())
            }
            // Remove the gateway found here (as root: the Gateway page runs
            // it through rootjob; a server's agent runs as root).
            "gateway-remove" => {
                let keep = !args.iter().any(|a| a == "--delete-data");
                let image = args.iter().any(|a| a == "--remove-image");
                gateway::remove(&sink, keep, image).map(|msg| {
                    if let Err(e) = crate::gateway_record::save(None) {
                        sink(sentient_installer_core::progress::Progress::Log { line: format!("The gateway's record was not cleared: {e}") });
                    }
                    sink(sentient_installer_core::progress::Progress::Done { message: msg });
                })
            }
            "gateway-adopt" => crate::gateway_record::adopt().map(|msg| sink(sentient_installer_core::progress::Progress::Done { message: msg })),
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
                Err("SENTIENT is installed: uninstall it first (--job uninstall), then remove the Platform Manager.".into())
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
        let pg_admin = pg_admin_arg(args);
        let search = crate::adopt::search();
        let found = search.found.clone();
        let message = match found.as_slice() {
            [] => format!("No SENTIENT found on this server. {}", search.looked_text()).trim_end().to_string(),
            [c] if c.problem.is_none() && (!c.sets_admin_password || (c.only_needs_pg_admin() && pg_admin.is_some())) => match crate::adopt::adopt_as_needed(c, pg_admin.clone(), sink.clone()) {
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
            // Say why it was not added: the window on another computer has
            // no list to choose from, so this line is all the operator sees.
            [c] => {
                let what = if c.kind.is_empty() { String::new() } else { format!(" ({}, {})", c.kind, c.location) };
                match &c.problem {
                    Some(p) => {
                        sink(Progress::Error { message: format!("SENTIENT was found on this server{what} but cannot be added: {p}") });
                        return 1;
                    }
                    // The window that started this asks for the password
                    // (the marker line) and runs add-existing with it.
                    None if c.only_needs_pg_admin() => {
                        for n in &c.notes {
                            sink(Progress::Log { line: n.clone() });
                        }
                        sink(Progress::Log { line: crate::adopt::needs_pg_admin_line(c) });
                        format!("SENTIENT was found on this server{what}. Adding it needs the PostgreSQL admin (postgres) password.")
                    }
                    None => format!("SENTIENT was found on this server{what}. Add it with the Platform Manager on that server."),
                }
            }
            many => format!(
                "{} SENTIENT instances were found on this server: {}. Add the one you want with the Platform Manager on that server.",
                many.len(),
                many.iter().map(|c| format!("{} ({})", c.kind, c.location)).collect::<Vec<_>>().join("; ")
            ),
        };
        sink(Progress::Done { message });
        return 0;
    }
    if job == "find-existing" || job == "add-existing" {
        let sink = if json { json_printer() } else { printer() };
        let search = crate::adopt::search();
        let found = search.found.clone();
        if job == "find-existing" {
            if json {
                for c in &found {
                    sink(Progress::Log { line: crate::adopt::encode(c) });
                }
                if found.is_empty() {
                    sink(Progress::Log { line: format!("No existing SENTIENT instance was found on this computer. {}", search.looked_text()) });
                }
            } else {
                print_candidates(&found);
                if found.is_empty() {
                    println!("{}", search.looked_text());
                }
            }
            return 0;
        }
        let chosen = if args.iter().any(|a| a == "--native") {
            found.iter().find(|c| c.record.engine == crate::installation::Engine::Native).ok_or_else(|| "No SENTIENT service was found on this computer.".to_string())
        } else {
            let n: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(1);
            let n = if found.len() == 1 { 1 } else { n };
            found.get(n.saturating_sub(1)).ok_or_else(|| {
                if found.is_empty() {
                    format!("No existing SENTIENT instance was found on this computer. {}", search.looked_text()).trim_end().to_string()
                } else {
                    format!("Only {} instance(s) were found.", found.len())
                }
            })
        };
        let pg_admin = pg_admin_arg(args);
        // Asked for on standard input and none came: never fall back to
        // setting a new one.
        let chosen = if pg_admin.is_none() && args.iter().any(|a| a == "--pg-admin-password-stdin") {
            Err("No PostgreSQL admin password was given.".to_string())
        } else {
            chosen
        };
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
    // The backup folder chooser (crate::manager::backup_folder): a folder's
    // folders, free space and whether backups can go there; make one; make
    // one writable for the account that runs the manager (as root). One JSON
    // line: the folder as listed (dir-make-writable: after the change).
    if job == "dir-list" || job == "dir-mkdir" || job == "dir-make-writable" {
        let r = (|| -> Result<crate::backups::DirList, String> {
            let path = std::path::PathBuf::from(args.get(1).ok_or(USAGE)?);
            match job {
                "dir-mkdir" => {
                    let name = args.get(2).map(String::as_str).filter(|n| !n.is_empty() && !n.contains(['/', '\\']) && *n != "." && *n != "..").ok_or("Not a folder name.")?;
                    let made = path.join(name);
                    std::fs::create_dir(&made).map_err(|e| format!("{name} cannot be made in {}: {e}", path.display()))?;
                    crate::backups::dir_list(&made)
                }
                "dir-make-writable" => {
                    crate::backups::make_writable(&path, args.get(2).and_then(|u| u.parse().ok()))?;
                    crate::backups::dir_list(&path)
                }
                _ => crate::backups::dir_list(&path),
            }
        })();
        return match r {
            Ok(l) => {
                println!("{}", serde_json::to_string(&l).unwrap_or_default());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // SENTIENT's licence functions, called here for the window
    // (crate::licence): the request in a file, removed once read.
    if job == "licence-api" {
        if let Some(w) = args.windows(2).find(|w| w[0] == "--request-b64") {
            return crate::licence::agent_job_b64(&w[1]);
        }
        return match args.windows(2).find(|w| w[0] == "--request-file") {
            Some(w) => crate::licence::agent_job(&w[1]),
            None => {
                eprintln!("{USAGE}");
                2
            }
        };
    }
    // The restore history (crate::restore_flow): one JSON line, and a run's log.
    if job == "restore-history-json" {
        println!("{}", serde_json::to_string(&crate::restore_flow::history()).unwrap_or_default());
        return 0;
    }
    // Stop a running restore before its switch (crate::restore_flow).
    if job == "restore-cancel" {
        return match crate::restore_flow::request_cancel() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // The backups in a folder other than the backup folder (Restore from…).
    if job == "backup-list" {
        let Some(folder) = args.get(1) else {
            eprintln!("{USAGE}");
            return 2;
        };
        println!("{}", serde_json::to_string(&crate::backups::list(std::path::Path::new(folder))).unwrap_or_default());
        return 0;
    }
    if job == "restore-log" {
        return match args.get(1).ok_or_else(|| USAGE.to_string()).and_then(|id| crate::restore_flow::log_text(id)) {
            Ok(t) => {
                print!("{t}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // Moving data to another disk (crate::movedata): where each store is
    // and the disks, a folder checked, the downtime; one JSON line each.
    if let Some(code) = crate::movedata::data_job(job, args) {
        return code;
    }
    // What a remote instance's card shows (crate::remote::status): one JSON line.
    if job == "status-json" {
        println!("{}", serde_json::to_string(&status_json(InstallationRecord::load().as_ref())).unwrap_or_default());
        return 0;
    }
    // The pages of a remote instance read this computer through these
    // (crate::remote): services, database, backups. One JSON line each.
    // The databases that keep a shared PostgreSQL running when the stack
    // stops (Status's Stop dialog on a remote instance): one JSON line.
    if job == "pg-others-json" {
        let answer = match InstallationRecord::load() {
            Some(record) => record.platform().pg_other_databases(),
            None => Ok(Vec::new()),
        };
        let line = match answer {
            Ok(names) => serde_json::json!({ "names": names }),
            Err(e) => serde_json::json!({ "names": [], "error": e }),
        };
        println!("{line}");
        return 0;
    }
    // What a server's Uninstall dialog shows (crate::uninstall::Facts): one JSON line.
    if job == "uninstall-facts-json" {
        let line = match InstallationRecord::load() {
            Some(record) => serde_json::to_string(&crate::uninstall::facts_full(&record)).unwrap_or_else(|_| "null".into()),
            None => "null".into(),
        };
        println!("{line}");
        return 0;
    }
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
                serde_json::to_string(&crate::remote::RemoteBackups { folder: prefs.folder.clone(), daily_at: prefs.daily_at.clone(), keep: prefs.keep, list: crate::backups::list_all(&prefs), preset: prefs.preset, history: prefs.history, free: crate::backups::free_space(&prefs.folder) })
            }
        };
        println!("{}", line.unwrap_or_default());
        return 0;
    }
    // Whether a migration could start now (crate::migration::preflight):
    // "ok", or why not (exit 1). Nothing is changed.
    if job == "migrate-preflight" {
        let Some(record) = InstallationRecord::load() else {
            eprintln!("No SENTIENT is added on this server.");
            return 1;
        };
        return match crate::migration::preflight(&record) {
            Ok(()) => {
                println!("ok");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // Whether PostgreSQL takes the password in a file (removed at once):
    // "ok", or why not (exit 1).
    if job == "takeover-check-password" {
        let Some(record) = InstallationRecord::load() else {
            eprintln!("No SENTIENT is added on this server.");
            return 1;
        };
        let pw = args.windows(2).find(|w| w[0] == "--db-password-file").map(|w| {
            let p = std::fs::read_to_string(&w[1]).unwrap_or_default().trim_end_matches(['\r', '\n']).to_string();
            let _ = std::fs::remove_file(&w[1]);
            p
        });
        let ch = args.iter().any(|a| a == "--clickhouse");
        return match crate::adopt::check_takeover_password(&record, ch, pw.as_deref().unwrap_or("")) {
            Ok(()) => {
                println!("ok");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // What taking this installation over would do (one JSON line):
    // takeover-plan [--compose <file>].
    if job == "takeover-plan" {
        let Some(record) = InstallationRecord::load() else {
            println!("null");
            return 0;
        };
        let compose = args.windows(2).find(|w| w[0] == "--compose").map(|w| w[1].clone());
        return match crate::adopt::take_over_plan(&record, compose.as_deref()) {
            Ok(p) => {
                println!("{}", serde_json::to_string(&p).unwrap_or_default());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // ClickHouse parts set aside: ch-detached (JSON), ch-attach / ch-drop
    // <table> <part> [<table> <part> ...] (one line per part: ok or why not).
    if job == "ch-detached" || job == "ch-attach" || job == "ch-drop" {
        let Some(record) = InstallationRecord::load() else {
            println!("null");
            return 0;
        };
        if job == "ch-detached" {
            return match crate::dbstatus::detached_parts(&record) {
                Ok(list) => {
                    println!("{}", serde_json::to_string(&list).unwrap_or_default());
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            };
        }
        let parts: Vec<(String, String)> = args[1..].chunks(2).filter(|c| c.len() == 2).map(|c| (c[0].clone(), c[1].clone())).collect();
        let r = if job == "ch-attach" { crate::dbstatus::attach_parts(&record, &parts) } else { crate::dbstatus::drop_parts(&record, &parts) };
        return match r {
            Ok(results) => {
                let failed = results.iter().filter(|(_, r)| r.is_err()).count();
                for (p, r) in results {
                    match r {
                        Ok(()) => println!("ok {p}"),
                        Err(e) => println!("failed {p}: {}", e.lines().next().unwrap_or("")),
                    }
                }
                // Per-part results are on the lines; the job itself answered.
                let _ = failed;
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // Statistics (crate::stats): a live reading with the database numbers,
    // one reading for the history (the sampler's timer), the history of the
    // last <seconds>, and installing the sampler.
    // `stats-live <system|services|db>`: one part only, so a slow part
    // never holds the others back (an older agent ignores the word and
    // answers the whole reading, which the page takes as it is).
    if job == "stats-live" {
        let record = InstallationRecord::load();
        let s = match args.get(1).and_then(|a| crate::stats::Part::from_arg(a)) {
            Some(part) => crate::stats::read_part(record.as_ref(), part),
            None => crate::stats::read(record.as_ref(), std::time::Duration::from_secs(1), true),
        };
        println!("{}", serde_json::to_string(&s).unwrap_or_default());
        return 0;
    }
    if job == "stats-sample" {
        return match crate::stats::sample_now() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    if job == "stats-history" {
        let secs: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3600);
        let h = crate::stats::history(&crate::stats::history_path(), secs, chrono::Utc::now().timestamp());
        println!("{}", serde_json::to_string(&h).unwrap_or_default());
        return 0;
    }
    if job == "stats-install" {
        let exe = std::env::current_exe().unwrap_or_else(|_| crate::self_install::manager_exe());
        return match crate::stats::install_sampler(&exe) {
            Ok(how) => {
                println!("The statistics sampler runs every minute ({how}).");
                0
            }
            Err(e) => {
                eprintln!("The statistics sampler could not be installed: {e}");
                1
            }
        };
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
                Some(src) => return print_log(crate::logsource::read(&record, src, n, today)),
                None => {
                    eprintln!("No such log here: {key}");
                    return 2;
                }
            },
            // No source named (an older window): SENTIENT's own log, read
            // by container or service, never through the compose files.
            None => return print_log(crate::logsource::read(&record, crate::logsource::Source::Sentient, n, today)),
        }
    }
    // The Server card's reading and actions (crate::server_ops): no
    // SENTIENT needed on the machine.
    {
        let sink = if json { json_printer() } else { printer() };
        if let Some(r) = crate::server_ops::job(job, args, &sink) {
            return match r {
                Ok(()) => 0,
                Err(e) => {
                    report_failure(e, json, &sink);
                    1
                }
            };
        }
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
                report_failure(e, json, &sink);
                1
            }
        };
    }
    record.use_pg_tools();
    // The job's steps before it starts (crate::jobplan): the window shows
    // them all at once.
    if let Some(steps) = crate::jobplan::for_job(&record, args) {
        sink(Progress::Plan { steps });
    }
    let result: Result<(), String> = match job {
        "migrate" => match args.get(1).and_then(|f| Flavor::parse(f)) {
            Some(target) => crate::migration::run(record, target, sink.clone()).map(|_| ()),
            None => Err(USAGE.into()),
        },
        "resume-migration" => crate::migration::resume(record, sink.clone()),
        "move-data" => match args.get(1) {
            Some(a) => crate::movedata::parse_moves(a).and_then(|m| crate::movedata::local_run(&m, sink.clone())),
            None => Err(USAGE.into()),
        },
        "move-data-remove-old" => match args.get(1) {
            Some(path) => crate::movedata::local_remove_old(path, sink.clone()),
            None => Err(USAGE.into()),
        },
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
            let record = record;
            let mut target: Option<String> = None;
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
                    // The chosen release, run under the tag the stack's
                    // compose file names (it is not rewritten here).
                    target = Some(crate::maintenance_ops::retag(&record.stack.image, v));
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
            crate::updating::run_to(record, package.as_deref(), target.as_deref(), accept, sink.clone()).map(|_| ())
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
        // The Restore flow (crate::restore_flow). Its answers are RESTORE lines.
        "restore-inspect" => match args.get(1) {
            Some(file) => {
                let pass = passphrase_arg(args);
                match crate::restore_flow::inspect(Some(&record), std::path::Path::new(file), pass.as_deref(), &sink) {
                    Ok(i) => {
                        sink(crate::restore_flow::data_line("inspect", &i));
                        Ok(())
                    }
                    Err(u) => {
                        sink(crate::restore_flow::data_line("unreadable", &u));
                        Err(u.message())
                    }
                }
            }
            None => Err(USAGE.into()),
        },
        "restore-remove-db" => match args.get(1) {
            Some(name) => crate::restore_flow::remove_restored(&record, name, &sink).map(|_| sink(Progress::Done { message: format!("{name} is removed.") })),
            None => Err(USAGE.into()),
        },
        "restore-precheck" | "restore-run" => match args.get(1).map(|f| read_request(f)) {
            Some(Ok(req)) => {
                let pass = passphrase_arg(args);
                if job == "restore-run" {
                    sink(Progress::Plan { steps: crate::jobplan::restore(&req) });
                    crate::restore_flow::run(record, req, pass, sink.clone())
                } else {
                    crate::restore_flow::precheck(&record, &req, pass.as_deref(), &sink).map(|p| sink(crate::restore_flow::data_line("precheck", &p)))
                }
            }
            Some(Err(e)) => Err(e),
            None => Err(USAGE.into()),
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
        // take-over-compose [--compose <file on this machine>]
        "take-over-compose" => {
            let compose = args.windows(2).find(|w| w[0] == "--compose").map(|w| w[1].clone());
            // The password the operator gave, from a file only the account
            // reads (never on the command line); the file goes at once.
            let password = args.windows(2).find(|w| w[0] == "--db-password-file").map(|w| {
                let p = std::fs::read_to_string(&w[1]).unwrap_or_default().trim_end_matches(['\r', '\n']).to_string();
                let _ = std::fs::remove_file(&w[1]);
                p
            });
            let ch_password = args.windows(2).find(|w| w[0] == "--ch-password-file").map(|w| {
                let p = std::fs::read_to_string(&w[1]).unwrap_or_default().trim_end_matches(['\r', '\n']).to_string();
                let _ = std::fs::remove_file(&w[1]);
                p
            });
            crate::adopt::take_over_compose(record, compose.as_deref(), password.as_deref(), ch_password.as_deref(), sink.clone()).map(|_| ())
        }
        "repair" => crate::maintenance_ops::run_repair(record, sink.clone()),
        // Start, stop or restart SENTIENT (a remote instance's Status).
        "control" => match args.get(1).map(String::as_str) {
            // The whole stack in order; --stop-shared-pg also stops a
            // PostgreSQL that holds other databases.
            Some(a @ ("start" | "stop" | "restart")) => record.platform().control_stack(sink.clone(), a, args.iter().any(|x| x == "--stop-shared-pg")),
            _ => Err(USAGE.into()),
        },
        // service <start|stop|restart> <name>: one of the install's own
        // services, by the name its own list gives (anything else refused).
        "service" => match (args.get(1).map(String::as_str), args.get(2)) {
            (Some(a @ ("start" | "stop" | "restart")), Some(name)) => record.platform().service_control(sink.clone(), name, a),
            _ => Err(USAGE.into()),
        },
        // uninstall [--parts …] [--keep-*]: crate::uninstall. The old
        // `uninstall [--delete-data]` is everything Setup installed but the
        // Platform Manager.
        "uninstall" => crate::uninstall::from_args(&args[1..]).and_then(|choice| {
            let mut facts = crate::uninstall::facts_quick(&record);
            facts.pg_others = Some(crate::uninstall::pg_others(&record));
            let manager = choice.has(crate::uninstall::Key::Manager);
            crate::uninstall::run(record, choice, facts, sink.clone())?;
            if manager {
                crate::uninstall::remove_manager_last(&sink);
            }
            Ok(())
        }),
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
        // The automatic backup, from a remote window: off or a time, how many
        // to keep, what to include. Saved and registered at once.
        "backup-auto" => match (args.get(1), args.get(2).and_then(|k| k.parse::<u32>().ok()), args.get(3).map(String::as_str)) {
            (Some(at), Some(keep), Some(include)) if (1..=365).contains(&keep) => (|| -> Result<(), String> {
                let mut prefs = crate::backups::BackupPrefs::load();
                prefs.daily_at = match at.as_str() {
                    "off" => None,
                    t if crate::backups::parse_time(t).is_some() => Some(t.to_string()),
                    _ => return Err(format!("Not a time: {at}")),
                };
                prefs.keep = keep;
                (prefs.preset, prefs.history) = match include {
                    "everything" => (crate::backups::Preset::Everything, crate::backups::History::All),
                    "last90" => (crate::backups::Preset::Everything, crate::backups::History::LastDays(90)),
                    "configuration" => (crate::backups::Preset::Configuration, prefs.history),
                    _ => return Err(format!("Not a choice: {include}")),
                };
                prefs.save().and_then(|_| crate::backups::apply_schedule(&prefs)).map(|_| {
                    sink(Progress::Done {
                        message: match &prefs.daily_at {
                            Some(t) => format!("Automatic backup every day at {t}, keeping {keep}."),
                            None => "Automatic backup is off.".into(),
                        },
                    })
                })
            })(),
            _ => Err(USAGE.into()),
        },
        // Change the backup folder; the backups there move along or stay.
        "backup-folder" => match (args.get(1), args.get(2).map(String::as_str)) {
            (Some(folder), Some(how @ ("move" | "leave"))) => crate::backups::set_folder(std::path::Path::new(folder), how == "move", &sink).map(|message| sink(Progress::Done { message })),
            _ => Err(USAGE.into()),
        },
        // Check or delete one backup in the backup folder (only there).
        "backup-verify" | "backup-delete" => match args.get(1) {
            Some(file) => {
                let prefs = crate::backups::BackupPrefs::load();
                match crate::backups::list_all(&prefs).into_iter().find(|e| e.path == std::path::Path::new(file)) {
                    None => Err(format!("{file} is not a backup in {}.", prefs.folder.display())),
                    Some(e) if job == "backup-delete" => crate::backups::delete(&e.path).map(|_| sink(Progress::Done { message: format!("{} is deleted.", e.name) })),
                    Some(e) => crate::backups::check(&e.path, None).and_then(|r| {
                        let summary = crate::backups::check_of(&e.path).map(|c| c.summary).unwrap_or_default();
                        if r.ok() {
                            sink(Progress::Done { message: format!("{}: {summary}", e.name) });
                            Ok(())
                        } else {
                            Err(format!("{} is damaged. {summary}", e.name))
                        }
                    }),
                }
            }
            None => Err(USAGE.into()),
        },
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
            report_failure(e, json, &sink);
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
/// SENTIENT's version as shown: an image tag like "latest" with the real
/// version from the image's label, "4.4.0.2 (image tag latest)".
pub(crate) fn shown_version(record: &InstallationRecord) -> String {
    let recorded = record.sentient_version.clone();
    let numbered = recorded.split('.').next().is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if numbered || record.engine != crate::installation::Engine::Docker {
        return recorded;
    }
    let label = sentient_installer_core::host::host_command("docker")
        .args(["inspect", "-f", "{{index .Config.Labels \"org.opencontainers.image.version\"}}", &record.stack.names().app])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|v| !v.is_empty() && v != "<no value>" && v != "dev");
    match label {
        Some(v) => format!("{v} (image tag {recorded})"),
        None => format!("version unknown (image tag {recorded})"),
    }
}

/// This computer's state for the Platform Manager on another computer.
pub(crate) fn status_json(record: Option<&InstallationRecord>) -> crate::remote::RemoteStatus {
    // The full version, as the window shows it (0.8.0-rc12).
    // The same text as the window's About: version and build.
    let manager_version = format!("{} (build {})", crate::updates::manager_version(), crate::updates::build_commit());
    let Some(record) = record else { return crate::remote::RemoteStatus { manager_version, ..Default::default() } };
    let snap = record.platform().snapshot();
    // Without administrator rights the database's size needs a password
    // prompt (native Linux): leave it out rather than ask.
    let db = if crate::dbstatus::needs_admin(record) && !crate::rootjob::is_root() { crate::dbstatus::DbStatus::default() } else { crate::dbstatus::read(record) };
    // The licence's one-word state, read-only from the database (same
    // rights as the size above).
    let licence = if crate::dbstatus::needs_admin(record) && !crate::rootjob::is_root() { None } else { crate::licence::read_tile(record) };
    let database_bytes = db.pg.as_ref().map(|p| p.db_bytes + db.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0));
    let mon = crate::sysmon::sample(&record.state_dir);
    let prefs = crate::backups::BackupPrefs::load();
    let last_backup = crate::backups::list_all(&prefs).first().map(|e| e.made.clone());
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
        vanished_files: record.stack.vanished_files(),
        external_compose: record.engine == crate::installation::Engine::Docker && record.stack.external.is_some(),
        licence,
        wsl_distro: if record.engine == crate::installation::Engine::Docker { record.stack.wsl_distro.clone() } else { None },
    }
}

/// A log for the window: its lines, or (exit 1) why it could not be read, so
/// the page shows an error state instead of the reason as a log line.
fn print_log(text: String) -> i32 {
    let t = text.trim();
    if t.starts_with("Could not read") || t.starts_with("No gateway") || t.starts_with("No PostgreSQL log") {
        eprintln!("{t}");
        return 1;
    }
    println!("{text}");
    0
}

/// The PostgreSQL admin password for add-existing / add-if-one: one line on
/// standard input (`--pg-admin-password-stdin`, what a window on another
/// computer sends), or `--pg-admin-password <pw>`, kept for scripts written
/// for 0.9.0 (it shows in the process list; nothing in the Platform Manager
/// uses it). Empty: none given.
fn pg_admin_arg(args: &[String]) -> Option<String> {
    if args.iter().any(|a| a == "--pg-admin-password-stdin") {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        return Some(line.trim_end_matches(['\r', '\n']).to_string()).filter(|p| !p.is_empty());
    }
    args.windows(2).find(|w| w[0] == "--pg-admin-password").map(|w| w[1].clone()).filter(|p| !p.is_empty())
}

/// A backup's passphrase: from a file only the account reads (removed at
/// once), or from standard input; never from the command line.
fn passphrase_arg(args: &[String]) -> Option<String> {
    if let Some(w) = args.windows(2).find(|w| w[0] == "--passphrase-file") {
        let p = std::fs::read_to_string(&w[1]).unwrap_or_default().trim_end_matches(['\r', '\n']).to_string();
        let _ = std::fs::remove_file(&w[1]);
        return Some(p).filter(|p| !p.is_empty());
    }
    if args.iter().any(|a| a == "--passphrase-stdin") {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        return Some(line.trim_end_matches(['\r', '\n']).to_string()).filter(|p| !p.is_empty());
    }
    None
}

/// The Restore flow's request (crate::restore_flow::Request), from a file
/// the window wrote; removed once read when it is a sent one (`.spm-restore-`).
fn read_request(path: &str) -> Result<crate::restore_flow::Request, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    if std::path::Path::new(path).file_name().is_some_and(|n| n.to_string_lossy().starts_with(".spm-restore-")) {
        let _ = std::fs::remove_file(path);
    }
    serde_json::from_str(&text).map_err(|e| format!("the restore request: {e}"))
}
