// Copyright © 2016-2026 The SENTIENT Authors

//! Updating SENTIENT, shared by Setup's Update and the manager's Update
//! screen. Native Windows installations go through the safe update
//! (sentient_installer_core::native::safe_update): database copy, dry run,
//! roll back on failure. Docker pulls and migrates; native Linux replaces
//! the files.

use std::path::{Path, PathBuf};

use sentient_installer_core::distro;
use sentient_installer_core::native;
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

/// The version a package folder says it holds.
pub fn package_version(package: &Path) -> Option<String> {
    std::fs::read_to_string(package.join("VERSION")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// A package folder (with `sentient-payload.tar`) unpacked into
/// `state_dir/update-staging`, or a folder that is already unpacked.
#[cfg_attr(not(windows), allow(dead_code))]
fn staged(sink: &ProgressFn, record: &InstallationRecord, package: &Path) -> Result<(PathBuf, bool), String> {
    if package.join("bin").join("sentient-server.exe").exists() {
        return Ok((package.to_path_buf(), false));
    }
    let tar = package.join("sentient-payload.tar");
    if !tar.exists() {
        return Err(format!("{} is not a SENTIENT package (no sentient-payload.tar)", package.display()));
    }
    let staging = record.state_dir.join("update-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
    step(sink, "Unpacking the new SENTIENT");
    // Windows' own bsdtar: a Git-for-Windows tar earlier on PATH reads
    // "C:\…" as a remote host.
    let ok = std::process::Command::new(r"C:\Windows\System32\tar.exe")
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(&staging)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok { Ok((staging, true)) } else { Err(format!("Could not unpack {}", tar.display())) }
}

/// Update `record` from `package` (native) or the registry (Docker). Returns
/// the record with its new version, already saved.
pub fn run(
    record: InstallationRecord,
    package: Option<&Path>,
    accept_breaking: bool,
    sink: ProgressFn,
) -> Result<InstallationRecord, String> {
    let mut version = None;
    let mut message = String::new();
    match record.engine {
        Engine::Docker => distro::update(sink.clone(), &record.stack)?,
        Engine::Native if cfg!(windows) => {
            let package = package.ok_or("Choose the folder of the new SENTIENT package first.")?;
            version = package_version(package);
            let cfg = record.native_config_with_secrets()?;
            let (dir, temporary) = staged(&sink, &record, package)?;
            let result = native::safe_update::safe_update(
                sink.clone(),
                &cfg,
                &dir,
                &native::safe_update::UpdateOptions { accept_breaking },
            );
            if temporary {
                let _ = std::fs::remove_dir_all(&dir);
            }
            let report = result?;
            message = format!(
                " The database from before the update is kept as {} and the previous program files in {}, until the next update.",
                report.snapshot,
                report.previous_files.display()
            );
        }
        Engine::Native => {
            let package = package.ok_or("Choose the folder of the new SENTIENT package first.")?;
            version = package_version(package);
            native::update(sink.clone(), &record.native_config(), &package.join(native::linux_payload_name()))?;
        }
    }
    let mut updated = record;
    if let Some(v) = version {
        updated.sentient_version = v;
    }
    let json = serde_json::to_string_pretty(&updated).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    sink(Progress::Done { message: format!("SENTIENT {} is running.{message}", updated.sentient_version) });
    Ok(updated)
}
