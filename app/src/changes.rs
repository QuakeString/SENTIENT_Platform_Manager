// Copyright © 2016-2026 The SENTIENT Authors

//! Changes a newer Platform Manager makes to an installation set up by an
//! earlier one: listed before they run, applied on the operator's click,
//! and only these steps run (never the whole Repair).
//!
//! Each change has an id that is stored in the installation record once it
//! is done (`InstallationRecord::changes_done`). An install, an adopt and a
//! Repair made by this version start with every id done: they already set
//! things up this way. A change that fails stays listed until it succeeds,
//! and SENTIENT is always started again afterwards.

use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::InstallationRecord;
#[cfg(windows)]
use crate::installation::Engine;

pub struct Change {
    pub id: &'static str,
    /// What it does, in the operator's words.
    pub text: &'static str,
    /// The step's name while it runs.
    pub step: &'static str,
    /// SENTIENT is stopped while it runs.
    pub restarts: bool,
    applies: fn(&InstallationRecord) -> bool,
    run: fn(&InstallationRecord, &ProgressFn) -> Result<(), String>,
}

/// Every change this version knows, oldest first.
pub fn all() -> Vec<Change> {
    vec![
        Change {
            id: "clickhouse-clean-stop",
            text: "ClickHouse is stopped cleanly before Windows shuts down.",
            step: "Setting up the clean ClickHouse stop",
            restarts: true,
            applies: clickhouse_on_windows,
            run: clickhouse_clean_stop,
        },
        Change {
            id: "start-without-delay",
            text: "SENTIENT starts without the 2-minute Windows delay.",
            step: "Registering the services to start without delay",
            restarts: false,
            applies: native_windows,
            run: start_without_delay,
        },
        Change {
            id: "windows-firewall",
            text: "Other computers can reach SENTIENT: its ports are opened in Windows Firewall.",
            step: "Opening SENTIENT's ports in Windows Firewall",
            restarts: false,
            applies: native_windows,
            run: open_firewall,
        },
    ]
}

/// The ids a new install, adopt or Repair records as done.
pub fn all_ids() -> Vec<String> {
    all().into_iter().map(|c| c.id.to_string()).collect()
}

/// What is waiting for this installation.
pub fn pending(record: &InstallationRecord) -> Vec<Change> {
    // Development aid: list every change not done, wherever this runs.
    #[cfg(debug_assertions)]
    let every = std::env::var_os("SPM_DEBUG_ALL_CHANGES").is_some();
    #[cfg(not(debug_assertions))]
    let every = false;
    all().into_iter().filter(|c| !record.changes_done.iter().any(|d| d == c.id) && (every || (c.applies)(record))).collect()
}

/// The steps a run shows, in order (also its Progress::Step names).
pub fn planned_steps(changes: &[Change]) -> Vec<String> {
    let restarts = changes.iter().any(|c| c.restarts);
    let mut steps = Vec::new();
    if restarts {
        steps.push(STOP.to_string());
    }
    steps.extend(changes.iter().map(|c| c.step.to_string()));
    if restarts {
        steps.push(START.to_string());
    }
    steps
}

pub const STOP: &str = "Stopping SENTIENT";
pub const START: &str = "Starting SENTIENT";

/// "SENTIENT restarts once (about 1 minute). Data and settings are kept."
pub fn effect(changes: &[Change]) -> &'static str {
    if changes.iter().any(|c| c.restarts) {
        "SENTIENT restarts once (about 1 minute). Data and settings are kept."
    } else {
        "SENTIENT keeps running. Data and settings are kept."
    }
}

/// Apply what is pending and save the record with each change that
/// succeeded. Errors name the first change that failed; the others still
/// ran, and SENTIENT is started again either way.
pub fn apply(mut record: InstallationRecord, sink: &ProgressFn) -> Result<InstallationRecord, String> {
    let changes = pending(&record);
    if changes.is_empty() {
        return Ok(record);
    }
    let restarts = changes.iter().any(|c| c.restarts);
    if restarts {
        sink(Progress::Step { name: STOP.into() });
        if let Err(e) = record.platform().control(sink.clone(), "stop") {
            sink(Progress::Log { line: format!("note: {e}") });
        }
    }
    let mut failed: Option<String> = None;
    for change in &changes {
        sink(Progress::Step { name: change.step.into() });
        match (change.run)(&record, sink) {
            Ok(()) => record.changes_done.push(change.id.to_string()),
            Err(e) => {
                sink(Progress::Log { line: format!("{}: {e}", change.step) });
                failed.get_or_insert(format!("{} could not be done: {e}", change.text.trim_end_matches('.')));
            }
        }
    }
    if restarts {
        sink(Progress::Step { name: START.into() });
        record.platform().control(sink.clone(), "start")?;
    }
    save(&record)?;
    match failed {
        Some(e) => Err(format!("{e}. SENTIENT is running as before.")),
        None => Ok(record),
    }
}

fn save(record: &InstallationRecord) -> Result<(), String> {
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)
}

#[cfg(windows)]
fn native_windows(record: &InstallationRecord) -> bool {
    record.engine == Engine::Native
}

#[cfg(not(windows))]
fn native_windows(_: &InstallationRecord) -> bool {
    false
}

#[cfg(windows)]
fn clickhouse_on_windows(record: &InstallationRecord) -> bool {
    record.engine == Engine::Native
        && record.flavor() == sentient_installer_core::stack::Flavor::Clickhouse
        && crate::secrets::get(crate::secrets::Secret::ClickhouseAccount).is_some()
}

#[cfg(not(windows))]
fn clickhouse_on_windows(_: &InstallationRecord) -> bool {
    false
}

/// The settings that keep ClickHouse starting after an unclean stop, the
/// service that stops it before Windows shuts down, and the keep-alive
/// that answers that stop (also part of Repair).
#[cfg(windows)]
pub fn clickhouse_clean_stop(record: &InstallationRecord, sink: &ProgressFn) -> Result<(), String> {
    use sentient_installer_core::clickhouse_wsl;
    let password = crate::secrets::get(crate::secrets::Secret::ClickhouseAccount)
        .ok_or_else(|| "the ClickHouse account's password is not in the secret store".to_string())?;
    // The work runs as the account with this version of the program.
    let manager = crate::self_install::install_manager()?;
    if let Err(e) = clickhouse_wsl::run_as_account(sink, &password, &manager, "harden", Some(std::time::Duration::from_secs(240))) {
        sink(Progress::Log { line: format!("note: {e}") });
    }
    clickhouse_wsl::register_keepalive(&password)?;
    clickhouse_wsl::install_guard(&manager, record.stack.ch_http_port)?;
    // The running keep-alive is the old one, which does not answer a stop.
    let _ = std::process::Command::new("schtasks.exe").args(["/end", "/tn", clickhouse_wsl::KEEPALIVE_TASK]).output();
    let _ = std::process::Command::new("schtasks.exe").args(["/run", "/tn", clickhouse_wsl::KEEPALIVE_TASK]).output();
    Ok(())
}

#[cfg(not(windows))]
pub fn clickhouse_clean_stop(_: &InstallationRecord, _: &ProgressFn) -> Result<(), String> {
    Ok(())
}

/// Windows "in the system": SENTIENT's ports open in Windows Firewall, in
/// the group SENTIENT (native::ensure_firewall), added when a rule is
/// missing: installs from before 0.9.2-rc3 have none. Also part of Repair
/// and of an update. Elsewhere nothing.
pub fn open_firewall(record: &InstallationRecord, sink: &ProgressFn) -> Result<(), String> {
    #[cfg(windows)]
    if record.engine == Engine::Native {
        let added = sentient_installer_core::native::ensure_firewall(&record.native_config())?;
        sink(Progress::Log {
            line: if added {
                "SENTIENT's ports were opened in Windows Firewall (rule group SENTIENT).".into()
            } else {
                "SENTIENT's Windows Firewall rules are in place.".into()
            },
        });
    }
    #[cfg(not(windows))]
    let _ = (record, sink);
    Ok(())
}

fn start_without_delay(_: &InstallationRecord, _: &ProgressFn) -> Result<(), String> {
    #[cfg(windows)]
    crate::self_install::undelay_autostart();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_a_new_record_has_nothing_waiting() {
        let ids = all_ids();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
        let record = InstallationRecord { changes_done: ids, ..crate::installation::tests::sample() };
        assert!(pending(&record).is_empty());
    }

    #[test]
    fn steps_stop_and_start_sentient_around_a_change_that_restarts() {
        let changes: Vec<Change> = all().into_iter().filter(|c| c.restarts).collect();
        let steps = planned_steps(&changes);
        assert_eq!(steps.first().map(String::as_str), Some(STOP));
        assert_eq!(steps.last().map(String::as_str), Some(START));
        let quiet: Vec<Change> = all().into_iter().filter(|c| !c.restarts).collect();
        assert!(!planned_steps(&quiet).contains(&STOP.to_string()));
    }
}
