// Copyright © 2016-2026 The SENTIENT Authors

//! What an earlier SENTIENT install left on this computer once its
//! installation record is gone (an uninstall that kept the database, a
//! failed install, a Platform Manager removed by hand), and removing all of
//! it from Setup with one button.
//!
//! Before this, a new install found the old PostgreSQL and ClickHouse still
//! holding their ports and the operator had to remove services, a hidden
//! account, a WSL machine and folders by hand.

use sentient_installer_core::progress::{Progress, ProgressFn};

/// Descriptions of what is left, empty when nothing is. Blocking.
#[cfg(windows)]
pub fn find(default_pg_dir: &std::path::Path) -> Vec<String> {
    use sentient_installer_core::{clickhouse_wsl, distro, native};
    let mut found = native::leftovers(default_pg_dir);
    if clickhouse_wsl::is_installed() {
        found.push(format!("ClickHouse in WSL2 and its hidden account ({})", clickhouse_wsl::ACCOUNT));
    }
    let disk = clickhouse_wsl::work_dir();
    if disk.join("distro").exists() {
        found.push(format!("ClickHouse's disk in {}", disk.display()));
    }
    if distro::is_registered() {
        found.push(format!("The WSL distribution “{}” of an earlier Docker install", distro::DISTRO));
    }
    found
}

#[cfg(not(windows))]
pub fn find(_: &std::path::Path) -> Vec<String> {
    Vec::new()
}

/// Remove everything [`find`] lists, with every database in it. Cannot be
/// undone. Only for Setup, where no installation record exists.
#[cfg(windows)]
pub fn remove(default_pg_dir: std::path::PathBuf, sink: ProgressFn) -> Result<(), String> {
    use sentient_installer_core::{clickhouse_wsl, distro, native};
    native::remove_leftovers(&sink, &default_pg_dir)?;
    if clickhouse_wsl::is_installed() {
        // With the account's password the account deletes its distro first;
        // without it, the account goes and its machine is stopped below.
        crate::kept::remove_clickhouse(&sink)?;
    } else {
        clickhouse_wsl::remove_guard(&sink);
    }
    let defaults = distro::DeployConfig::default();
    clickhouse_wsl::stop_account_wsl(&sink, &[defaults.ch_http_port, defaults.ch_native_port]);
    for task in [clickhouse_wsl::KEEPALIVE_TASK, "SENTIENT ClickHouse job"] {
        let _ = std::process::Command::new("schtasks.exe").args(["/delete", "/tn", task, "/f"]).output();
    }
    let disk = clickhouse_wsl::work_dir();
    if disk.exists() {
        sink(Progress::Step { name: format!("Deleting {}", disk.display()) });
        let mut gone = false;
        for attempt in 0..10u64 {
            if std::fs::remove_dir_all(&disk).is_ok() || !disk.exists() {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500 * (attempt + 1)));
        }
        if !gone {
            return Err(format!("Could not delete {} (still in use). Restart Windows and press Remove again.", disk.display()));
        }
    }
    if distro::is_registered() {
        sink(Progress::Step { name: format!("Removing the WSL distribution “{}”", distro::DISTRO) });
        let _ = std::process::Command::new("wsl.exe").args(["--terminate", distro::DISTRO]).output();
        let _ = std::process::Command::new("wsl.exe").args(["--unregister", distro::DISTRO]).output();
    }
    crate::secrets::forget_all();
    sink(Progress::Done { message: "The earlier install is removed.".into() });
    Ok(())
}

#[cfg(not(windows))]
pub fn remove(_: std::path::PathBuf, _: ProgressFn) -> Result<(), String> {
    Err("Removing an earlier install from Setup is for Windows.".into())
}
