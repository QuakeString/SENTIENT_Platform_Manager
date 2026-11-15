// Copyright © 2016-2026 The SENTIENT Authors

//! The work behind Repair, Uninstall and removing the Platform Manager,
//! without any window: the maintenance screen and the server agent
//! (`--job repair`, crate::headless) both run it.

use sentient_installer_core::{distro, native};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};
use crate::self_install;

pub(crate) fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

/// The configuration with its passwords. On Linux they live only in
/// SENTIENT's environment file; when that is what went missing, the
/// database users get new ones.
pub(crate) fn native_config_for_repair(record: &InstallationRecord, sink: &ProgressFn) -> Result<native::NativeConfig, String> {
    #[cfg(target_os = "linux")]
    if !sentient_installer_core::native_linux::env_file_exists() {
        let mut cfg = record.native_config();
        sentient_installer_core::native_linux::reissue_secrets(sink, &mut cfg)?;
        return Ok(cfg);
    }
    let _ = sink;
    record.native_config_with_secrets()
}

pub(crate) fn run_repair(record: InstallationRecord, sink: ProgressFn) -> Result<(), String> {
    // ClickHouse first: a ClickHouse that refuses a table after an unclean
    // stop keeps SENTIENT from starting, so the repair below would fail.
    // Installs from before the clean ClickHouse stop get it here.
    #[cfg(windows)]
    if record.engine == Engine::Native && record.flavor() == sentient_installer_core::stack::Flavor::Clickhouse {
        if let Some(password) = crate::secrets::get(crate::secrets::Secret::ClickhouseAccount) {
            use sentient_installer_core::clickhouse_wsl;
            // This version of the program does the work as the account, so
            // it is put in place first (repair restores it anyway).
            let _ = self_install::install_manager();
            step(&sink, "Making ClickHouse safe against unclean stops");
            if let Err(e) = clickhouse_wsl::run_as_account(&sink, &password, &self_install::manager_exe(), "harden", Some(std::time::Duration::from_secs(240))) {
                sink(Progress::Log { line: format!("note: {e}") });
            }
            step(&sink, "Stopping ClickHouse cleanly when Windows shuts down");
            match clickhouse_wsl::register_keepalive(&password)
                .and_then(|()| clickhouse_wsl::install_guard(&self_install::manager_exe(), record.stack.ch_http_port))
            {
                // The running keep-alive is the old one, which does not
                // answer a stop request: start the new one now.
                Ok(()) => {
                    let _ = std::process::Command::new("schtasks.exe").args(["/end", "/tn", clickhouse_wsl::KEEPALIVE_TASK]).output();
                    let _ = std::process::Command::new("schtasks.exe").args(["/run", "/tn", clickhouse_wsl::KEEPALIVE_TASK]).output();
                }
                Err(e) => sink(Progress::Log { line: format!("note: the ClickHouse shutdown service was not set up: {e}") }),
            }
        }
    }
    // SENTIENT first: it is what the repair is for, and on Linux Docker it
    // needs no administrator, so a dismissed password prompt for the
    // Platform Manager's files below does not leave it broken.
    match record.engine {
        // Write the compose file again from the record (with the stack's
        // passwords) and bring everything up; without a readable compose
        // file, just start what is there.
        Engine::Docker => match distro::docker_ops::with_secrets(&record.stack) {
            Ok(cfg) => distro::docker_ops::reconfigure(&sink, &cfg)?,
            Err(_) => distro::control(sink.clone(), &record.stack, "start")?,
        },
        // deploy is idempotent: it keeps the database, writes the service
        // and its configuration again, and starts SENTIENT.
        Engine::Native => native::deploy(sink.clone(), &native_config_for_repair(&record, &sink)?)?,
    }
    // What a newer Platform Manager would otherwise list as changes for
    // this installation: Repair sets it all up.
    #[cfg(windows)]
    if record.engine == Engine::Native {
        self_install::undelay_autostart();
    }
    if let Some(mut saved) = InstallationRecord::load() {
        saved.changes_done = crate::changes::all_ids();
        let _ = save_record(&saved);
    }
    step(&sink, "Restoring the Platform Manager");
    let message = match self_install::install_manager() {
        Ok(manager) => {
            self_install::restore_menu_entry(&manager);
            self_install::refresh_operator_screen(&sink);
            "Repaired: SENTIENT is running.".to_string()
        }
        Err(e) => {
            sink(Progress::Log { line: format!("The Platform Manager's files were not restored: {e}") });
            "SENTIENT is running again; the Platform Manager's own files were not restored (see the log). Run Repair again to restore them.".to_string()
        }
    };
    sink(Progress::Done { message });
    Ok(())
}

pub(crate) fn run_uninstall(record: InstallationRecord, keep_data: bool, sink: ProgressFn) -> Result<(), String> {
    match (record.engine, keep_data) {
        (Engine::Docker, true) => distro::remove_keeping_data(sink.clone(), &record.stack)?,
        (Engine::Docker, false) => distro::uninstall(sink.clone(), &record.stack)?,
        (Engine::Native, true) => native::uninstall(sink.clone(), &record.native_config())?,
        (Engine::Native, false) => native::cleanup(sink.clone(), &record.native_config())?,
    }
    // Windows hybrid: ClickHouse's distro belongs to its own account. Keeping
    // the data keeps both; otherwise the account deletes its distro, then
    // the account and its boot task go.
    // Whatever the installation uses now: a migration away from ClickHouse
    // leaves it set up.
    #[cfg(windows)]
    if record.engine == Engine::Native && sentient_installer_core::clickhouse_wsl::is_installed() {
        if keep_data {
            // Stopped cleanly and not started at boot: left running, it
            // held its ports and a new install refused them.
            sentient_installer_core::clickhouse_wsl::park(&sink, record.stack.ch_http_port);
        } else {
            crate::kept::remove_clickhouse(&sink)?;
        }
    }
    if !keep_data {
        crate::secrets::forget_all();
    }
    step(&sink, "Removing shortcuts and the Platform Manager");
    self_install::remove_shortcuts();
    // The kiosk shortcuts (shared and, from earlier managers, per account),
    // and what opening SENTIENT left in each account.
    sentient_installer_core::kiosk::remove_shortcuts(&sink, &record.state_dir);
    sentient_installer_core::kiosk::remove_user_data(&sink);
    // The task that starts SENTIENT a set time after Windows.
    #[cfg(windows)]
    sentient_installer_core::native::remove_start_task();
    // The operator screen's sign-in entry would start a manager that is gone.
    if sentient_installer_core::kiosk::open_at_signin().is_some() {
        let _ = sentient_installer_core::kiosk::set_open_at_signin(&sentient_installer_core::progress::noop(), &self_install::manager_exe(), None);
    }
    // Automatic sign-in was only there for that screen.
    if sentient_installer_core::autologin::set_by_manager() {
        let _ = sentient_installer_core::autologin::set(&sink, None);
    }
    self_install::remove_manager_entry();
    self_install::remove_record(&crate::installation::record_path());
    self_install::remove_manager_later();
    sink(Progress::Done {
        message: if keep_data {
            "SENTIENT is removed. The database was kept and can be reused by a new install.".into()
        } else {
            "SENTIENT and its data are removed.".into()
        },
    });
    Ok(())
}

/// Remove a Platform Manager installed without SENTIENT: shortcuts, its Apps
/// & features entry and the program itself (once this process exits).
pub(crate) fn run_remove_manager(sink: ProgressFn) -> Result<(), String> {
    step(&sink, "Removing the Platform Manager");
    self_install::remove_shortcuts();
    self_install::remove_manager_entry();
    self_install::remove_manager_later();
    sink(Progress::Done { message: "The Platform Manager is removed.".into() });
    Ok(())
}

pub(crate) fn save_record(record: &InstallationRecord) -> Result<(), String> {
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    self_install::write_record(&crate::installation::record_path(), &json)
}


/// A Step from a nested engine call as a log line, so the screen's
/// checklist keeps showing the step this screen planned.
pub(crate) fn nested(sink: &ProgressFn) -> ProgressFn {
    let sink = sink.clone();
    std::sync::Arc::new(move |p| match p {
        Progress::Step { name } => sink(Progress::Log { line: format!(">> {name}") }),
        Progress::Done { message } => sink(Progress::Log { line: message }),
        other => sink(other),
    })
}

pub(crate) const CLOSE_MANAGER: &str = "Closing the open Platform Manager";
pub(crate) const REPLACE_MANAGER: &str = "Replacing the program";
pub(crate) const INSTALL_MANAGER: &str = "Installing the Platform Manager";
pub(crate) const SHORTCUTS: &str = "Adding the Start menu and desktop shortcuts";

pub(crate) fn updating_sentient(version: &str) -> String {
    format!("Updating SENTIENT to {version}")
}


/// Text without terminal colour codes (container logs).
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and intermediates, then one final byte in @..~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The Platform Manager's own log: what it did, with times
/// (`activity.log` beside its preferences, kept under about 1 MB).
/// A line in the Platform Manager's own log, with its time (both builds).
pub fn log_line(line: &str) {
    use std::io::Write as _;
    let path = activity_log();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), line.lines().next().unwrap_or(""));
    }
}

pub fn activity_log() -> std::path::PathBuf {
    crate::updates::prefs_dir().join("activity.log")
}

/// `image` at tag `version`: `quakestring/sentient:latest` → `…:4.3.0.50`.
pub(crate) fn retag(image: &str, version: &str) -> String {
    let repo = match image.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => repo,
        _ => image,
    };
    format!("{repo}:{version}")
}
