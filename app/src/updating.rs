// Copyright © 2016-2026 The SENTIENT Authors

//! Updating SENTIENT, shared by Setup's Update and the manager's Update
//! screen. Every installation goes through a safe update (native::safe_update
//! for Windows and Linux services, distro::docker_ops for Docker): dry run,
//! database copy, roll back on failure.

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

/// A package folder unpacked into `state_dir/update-staging`, or a folder
/// that is already unpacked. Windows packages hold `sentient-payload.tar`,
/// Linux packages `sentient-payload-linux-<arch>.tar`.
fn staged(sink: &ProgressFn, record: &InstallationRecord, package: &Path) -> Result<(PathBuf, bool), String> {
    let server = if cfg!(windows) { "sentient-server.exe" } else { "sentient-server" };
    if package.join("bin").join(server).exists() {
        return Ok((package.to_path_buf(), false));
    }
    let tar_name = if cfg!(windows) { "sentient-payload.tar".to_string() } else { native::linux_payload_name() };
    let tar = package.join(&tar_name);
    if !tar.exists() {
        return Err(format!("{} is not a SENTIENT package (no {tar_name})", package.display()));
    }
    let staging = record.state_dir.join("update-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
    step(sink, "Unpacking the new SENTIENT");
    // Windows' own bsdtar: a Git-for-Windows tar earlier on PATH reads
    // "C:\…" as a remote host.
    let tar_exe = if cfg!(windows) { r"C:\Windows\System32\tar.exe" } else { "tar" };
    let ok = std::process::Command::new(tar_exe)
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
        Engine::Docker => {
            let cfg = distro::docker_ops::with_secrets(&record.stack)?;
            let report = distro::docker_ops::safe_update(sink.clone(), &cfg, accept_breaking)?;
            message = format!(
                " The database from before the update is kept as {}{}, until the next update.",
                report.snapshot,
                report.previous_image.map(|t| format!(" and the previous image as {t}")).unwrap_or_default()
            );
        }
        Engine::Native => {
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
