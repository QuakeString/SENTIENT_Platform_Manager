// Copyright © 2016-2026 The SENTIENT Authors

//! Updating SENTIENT, shared by Setup's Update and the manager's Update
//! screen. Every installation goes through a safe update (native::safe_update
//! for Windows and Linux services, distro::docker_ops for Docker): dry run,
//! database copy, roll back on failure. What the last update replaced stays
//! until the next one, and `rollback` goes back to it on request.

use std::path::{Path, PathBuf};

use sentient_installer_core::distro;
use sentient_installer_core::native;
use sentient_installer_core::native::safe_update::RollbackPoint;
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
    let (version, message) = match record.engine {
        Engine::Docker => {
            let cfg = distro::docker_ops::with_secrets(&record.stack)?;
            let report = distro::docker_ops::safe_update(sink.clone(), &cfg, accept_breaking)?;
            let message = format!(
                " The database from before the update is kept as {}{}, until the next update.",
                report.snapshot,
                report.previous_image.map(|t| format!(" and the previous image as {t}")).unwrap_or_default()
            );
            (distro::docker_ops::image_version(&cfg.image), message)
        }
        Engine::Native => {
            let package = package.ok_or("Choose the folder of the new SENTIENT package first.")?;
            let cfg = record.native_config_with_secrets()?;
            let (dir, temporary) = staged(&sink, &record, package)?;
            let result = native::safe_update::safe_update(
                sink.clone(),
                &cfg,
                &dir,
                &native::safe_update::UpdateOptions {
                    accept_breaking,
                    current_version: Some(record.sentient_version.clone()).filter(|v| !v.is_empty()),
                },
            );
            if temporary {
                let _ = std::fs::remove_dir_all(&dir);
            }
            let report = result?;
            let message = format!(
                " The database from before the update is kept as {} and the previous program files in {}, until the next update.",
                report.snapshot,
                report.previous_files.display()
            );
            (package_version(package), message)
        }
    };
    let mut updated = record;
    if let Some(v) = version {
        updated.sentient_version = v;
    }
    let json = serde_json::to_string_pretty(&updated).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    sink(Progress::Done { message: format!("SENTIENT {} is running.{message}", updated.sentient_version) });
    Ok(updated)
}

/// What the last update replaced, if it is still there to go back to.
/// Blocking (Docker asks the containers): call from a job or a background
/// task. Native Linux reads it without root.
pub fn rollback_point(record: &InstallationRecord) -> Result<Option<RollbackPoint>, String> {
    match record.engine {
        Engine::Docker => Ok(distro::docker_ops::rollback_point(&distro::docker_ops::with_secrets(&record.stack)?)),
        Engine::Native => Ok(native::safe_update::rollback_point(&record.native_config())),
    }
}

/// Go back to the version the last update replaced. Returns the record with
/// that version, already saved.
pub fn rollback(record: InstallationRecord, sink: ProgressFn) -> Result<InstallationRecord, String> {
    let (point, set_aside) = match record.engine {
        Engine::Docker => {
            let r = distro::docker_ops::rollback(sink.clone(), &distro::docker_ops::with_secrets(&record.stack)?)?;
            (r.point, r.set_aside)
        }
        Engine::Native => {
            let r = native::safe_update::rollback(sink.clone(), &record.native_config_with_secrets()?)?;
            (r.point, r.set_aside)
        }
    };
    let mut updated = record;
    if let Some(v) = &point.version {
        updated.sentient_version = v.clone();
    }
    let json = serde_json::to_string_pretty(&updated).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    sink(Progress::Done {
        message: format!(
            "Rolled back: {} is running. The database that was in use is kept as {set_aside} (under Kept on the Migration screen) until you remove it.",
            point.version.as_deref().map(|v| format!("SENTIENT {v}")).unwrap_or_else(|| "the previous version".into())
        ),
    });
    Ok(updated)
}
