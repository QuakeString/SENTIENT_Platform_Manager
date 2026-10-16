// Copyright © 2016-2026 The SENTIENT Authors

//! Which of its three jobs this run of the program does.
//!
//! One binary ships twice: as `SENTIENT-Setup.exe` (the installer) and, once
//! installed, as `SENTIENT Platform Manager.exe`. Started as setup on a machine
//! without SENTIENT it is the install wizard; started as setup where SENTIENT
//! is already installed it offers maintenance (update, repair, change database,
//! uninstall); started from the Start menu it is the manager.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Install,
    Maintenance,
    Manager,
}

impl AppMode {
    /// Decide from the command line, the executable's name and whether an
    /// installation record exists. `--install`, `--maintenance` and
    /// `--manager` override the detection (used by the uninstaller entry and
    /// for development).
    pub fn detect() -> Self {
        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--install" => return Self::Install,
                "--maintenance" | "--uninstall" => return Self::Maintenance,
                "--manager" => return Self::Manager,
                _ => {}
            }
        }
        let launched_as_setup = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
            .is_some_and(|name| name.contains("setup"));
        match (launched_as_setup, installation_record().exists()) {
            (true, true) => Self::Maintenance,
            (true, false) => Self::Install,
            (false, _) => Self::Manager,
        }
    }

    pub fn window_title(self) -> &'static str {
        match self {
            Self::Install | Self::Maintenance => "SENTIENT Setup",
            Self::Manager => "SENTIENT Platform Manager",
        }
    }
}

/// The record the installer writes when it finishes: what was installed,
/// where, and with which database. Its presence is what "SENTIENT is
/// installed on this machine" means to the program.
pub fn installation_record() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    #[cfg(not(windows))]
    let base = PathBuf::from("/var/lib");
    base.join("SENTIENT").join("installation.json")
}
