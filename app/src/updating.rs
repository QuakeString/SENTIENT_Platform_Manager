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
use sentient_installer_core::distro::DeployConfig;

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
    let ok = sentient_installer_core::host::host_command(tar_exe)
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(&staging)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok { Ok((staging, true)) } else { Err(format!("Could not unpack {}", tar.display())) }
}

/// The SENTIENT version a Docker install runs, read from its container
/// (crate::installation::running_version).
fn running_version(record: &InstallationRecord) -> Option<String> {
    crate::installation::running_version(record)
}

/// What moving a Docker install to a chosen release does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerStep {
    /// The update, under the tag the compose file names (its own compose
    /// file is the operator's, or it names that version already).
    Update,
    /// The update, then the compose file names the release's tag, so the
    /// stack stays on it (an install on `latest`, `dev`, another release).
    UpdateAndPin,
    /// It runs that release already under another tag: only the tag
    /// changes (no database copy, no schema step).
    QuickPin,
    /// It runs that release under that tag: nothing to do.
    AlreadyRunning,
}

/// `target` is the chosen release, `running` what the container runs (None:
/// it does not say), `tag` the tag the compose file names, `external`
/// whether that file is the operator's (rewritten only after the takeover).
pub fn docker_step(target: &str, running: Option<&str>, tag: &str, external: bool) -> DockerStep {
    use sentient_installer_core::releases::{Move, movement};
    let same = running.is_some_and(|r| movement(target, r) == Move::Same);
    let tagged = tag == target;
    match (same, tagged || external) {
        (true, true) => DockerStep::AlreadyRunning,
        (true, false) => DockerStep::QuickPin,
        (false, true) => DockerStep::Update,
        (false, false) => DockerStep::UpdateAndPin,
    }
}

/// The tag a Docker roll back writes into the compose file: the release
/// number the previous image carries (`back`: its SENTIENT_BUILD_VERSION,
/// else its numbered label), when the compose file is the Platform
/// Manager's and names another tag. None: the tag stays (the operator's
/// compose file, or an image that does not say its release).
pub fn rollback_tag(back: Option<&str>, tag: &str, external: bool) -> Option<String> {
    back.filter(|v| !external && sentient_installer_core::releases::is_numbered(v) && *v != tag).map(String::from)
}

/// What "Update SENTIENT" (no version chosen) does on a Docker install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlainUpdate {
    /// Pull what the compose tag names (latest, dev, the operator's file).
    Pull,
    /// Pinned to a release: update to the newest one, and pin it.
    To(String),
    /// Pinned, and the newest release is what runs.
    UpToDate,
}

/// `tag` is the compose file's tag, `running` what runs, `newest` the newest
/// published release (None: not known, e.g. offline).
pub fn plain_update(tag: &str, external: bool, running: Option<&str>, newest: Option<&str>) -> PlainUpdate {
    use sentient_installer_core::releases::{Move, is_numbered, movement};
    if external || !is_numbered(tag) {
        return PlainUpdate::Pull;
    }
    match newest {
        None => PlainUpdate::Pull,
        Some(n) if movement(n, running.unwrap_or(tag)) == Move::Newer => PlainUpdate::To(n.to_string()),
        Some(_) => PlainUpdate::UpToDate,
    }
}

/// Update `record` from `package` (native) or the registry (Docker). Returns
/// the record with its new version, already saved.
pub fn run(
    record: InstallationRecord,
    package: Option<&Path>,
    accept_breaking: bool,
    sink: ProgressFn,
) -> Result<InstallationRecord, String> {
    run_to(record, package, None, accept_breaking, sink)
}

/// [`run`] to the image `target` (Docker: a chosen release), which runs
/// under the tag the stack's compose file names.
pub fn run_to(
    record: InstallationRecord,
    package: Option<&Path>,
    target: Option<&str>,
    accept_breaking: bool,
    sink: ProgressFn,
) -> Result<InstallationRecord, String> {
    let mut pinned_image: Option<String> = None;
    let (version, message) = match record.engine {
        Engine::Docker => {
            let cfg = distro::docker_ops::with_secrets(&record.stack)?;
            let target_version = target.map(|t| distro::docker_ops::image_tag(t).to_string());
            let how = match (&target_version, target) {
                (Some(v), Some(_)) => docker_step(v, running_version(&record).as_deref(), distro::docker_ops::image_tag(&cfg.image), cfg.external.is_some()),
                _ => DockerStep::Update,
            };
            match how {
                DockerStep::AlreadyRunning => {
                    return Err(format!("SENTIENT {} is already running.", target_version.unwrap_or_default()));
                }
                DockerStep::QuickPin => {
                    let image = target.unwrap_or_default();
                    let v = target_version.clone().unwrap_or_default();
                    let pinned = distro::docker_ops::pin_tag(&sink, &cfg, image)?;
                    pinned_image = Some(pinned.image);
                    (Some(v.clone()), format!(" The stack now runs under the image tag {v}, the version it ran already: nothing was updated and the database was not copied."))
                }
                DockerStep::Update | DockerStep::UpdateAndPin => {
                    let report = distro::docker_ops::safe_update_to(sink.clone(), &cfg, target, accept_breaking)?;
                    let mut message = format!(
                        " The database from before the update is kept as {}{}, until the next update.",
                        report.snapshot,
                        report.previous_image.as_ref().map(|t| format!(" and the previous image as {t}")).unwrap_or_default()
                    );
                    if how == DockerStep::UpdateAndPin {
                        let image = target.unwrap_or_default();
                        step(&sink, &format!("Setting the image tag to {}", distro::docker_ops::image_tag(image)));
                        match distro::docker_ops::pin_tag(&sink, &cfg, image) {
                            Ok(pinned) => {
                                // The update ran the new image under the old
                                // tag: that tag names the previous image again.
                                if let Some(prev) = &report.previous_image {
                                    distro::docker_ops::retag_image(prev, &cfg.image);
                                }
                                message.push_str(&format!(" The compose file now names {}.", pinned.image));
                                pinned_image = Some(pinned.image);
                            }
                            Err(e) => message.push_str(&format!(" The image tag could not be changed ({e}); it still runs as {}.", cfg.image)),
                        }
                    }
                    let shown = DeployConfig { image: pinned_image.clone().unwrap_or_else(|| cfg.image.clone()), ..cfg.clone() };
                    let version = distro::docker_ops::running_version(&shown).or(target_version.clone()).or_else(|| distro::docker_ops::image_version(&shown.image));
                    (version, message)
                }
            }
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
    if let Some(image) = pinned_image {
        updated.stack.image = image;
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
///
/// Docker: the compose file then names the release the previous image holds
/// ([`rollback_tag`]), and the tag it named before keeps naming the image
/// that was rolled back from.
pub fn rollback(record: InstallationRecord, sink: ProgressFn) -> Result<InstallationRecord, String> {
    let mut pinned_image: Option<String> = None;
    let mut running: Option<String> = None;
    let (point, set_aside) = match record.engine {
        Engine::Docker => {
            let cfg = distro::docker_ops::with_secrets(&record.stack)?;
            // The image rolled back from keeps a tag of its own until the
            // compose file names the previous release (the roll back drops
            // its own tag for it, which would remove the image).
            let keep = format!("{}:spm-pin-keep", cfg.image.rsplit_once(':').filter(|(_, t)| !t.contains('/')).map(|(r, _)| r).unwrap_or(&cfg.image));
            let current = distro::docker_ops::running_image_id(&cfg);
            if let Some(id) = &current {
                distro::docker_ops::retag_image(id, &keep);
            }
            let r = match distro::docker_ops::rollback(sink.clone(), &cfg) {
                Ok(r) => r,
                Err(e) => {
                    distro::docker_ops::untag_image(&keep);
                    return Err(e);
                }
            };
            running = distro::docker_ops::running_version(&cfg).or_else(|| r.point.version.clone());
            let tag = distro::docker_ops::image_tag(&cfg.image);
            if let Some(v) = rollback_tag(running.as_deref(), tag, cfg.external.is_some()) {
                let image = crate::maintenance_ops::retag(&cfg.image, &v);
                step(&sink, &format!("Setting the image tag to {v}"));
                match distro::docker_ops::pin_tag(&sink, &cfg, &image) {
                    Ok(p) => {
                        // The roll back put the previous image under the
                        // newer release's tag: that tag names its own
                        // image again.
                        if current.is_some() {
                            distro::docker_ops::retag_image(&keep, &cfg.image);
                        }
                        pinned_image = Some(p.image);
                    }
                    Err(e) => sink(Progress::Log { line: format!("The image tag could not be set to {v} ({e}); it runs as {}.", cfg.image) }),
                }
            }
            distro::docker_ops::untag_image(&keep);
            (r.point, r.set_aside)
        }
        Engine::Native => {
            let r = native::safe_update::rollback(sink.clone(), &record.native_config_with_secrets()?)?;
            (r.point, r.set_aside)
        }
    };
    let mut updated = record;
    if let Some(v) = running.as_ref().filter(|v| sentient_installer_core::releases::is_numbered(v)).or(point.version.as_ref()) {
        updated.sentient_version = v.clone();
    }
    if let Some(image) = pinned_image {
        updated.stack.image = image;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_moves() {
        // On latest, running 4.4.0.2: 4.4.0.3 updates and pins.
        assert_eq!(docker_step("4.4.0.3", Some("4.4.0.2"), "latest", false), DockerStep::UpdateAndPin);
        // The version it runs: only the tag.
        assert_eq!(docker_step("4.4.0.2", Some("4.4.0.2"), "latest", false), DockerStep::QuickPin);
        // Running unknown: the full update, which pins.
        assert_eq!(docker_step("4.4.0.2", None, "latest", false), DockerStep::UpdateAndPin);
        // Pinned already.
        assert_eq!(docker_step("4.4.0.2", Some("4.4.0.2"), "4.4.0.2", false), DockerStep::AlreadyRunning);
        assert_eq!(docker_step("4.4.0.3", Some("4.4.0.2"), "4.4.0.2", false), DockerStep::UpdateAndPin);
        // A tag that names the release, running something else: update.
        assert_eq!(docker_step("4.4.0.3", Some("4.4.0.2"), "4.4.0.3", false), DockerStep::Update);
        // The operator's compose file: never rewritten here.
        assert_eq!(docker_step("4.4.0.3", Some("4.4.0.2"), "latest", true), DockerStep::Update);
        assert_eq!(docker_step("4.4.0.2", Some("4.4.0.2"), "latest", true), DockerStep::AlreadyRunning);
        // Going back is an update too.
        assert_eq!(docker_step("4.4.0.1", Some("4.4.0.2"), "dev", false), DockerStep::UpdateAndPin);
    }

    #[test]
    fn roll_back_names_the_previous_release() {
        // Pinned to 4.4.0.3, back to an image that says 4.4.0.2.
        assert_eq!(rollback_tag(Some("4.4.0.2"), "4.4.0.3", false).as_deref(), Some("4.4.0.2"));
        // An image that does not say its release (labelled "latest"): the tag stays.
        assert_eq!(rollback_tag(Some("latest"), "4.4.0.3", false), None);
        assert_eq!(rollback_tag(None, "4.4.0.3", false), None);
        // The operator's compose file is not rewritten.
        assert_eq!(rollback_tag(Some("4.4.0.2"), "latest", true), None);
        // Already named.
        assert_eq!(rollback_tag(Some("4.4.0.2"), "4.4.0.2", false), None);
    }

    #[test]
    fn a_pinned_install_updates_to_the_newest_release() {
        assert_eq!(plain_update("4.4.0.2", false, Some("4.4.0.2"), Some("4.4.0.3")), PlainUpdate::To("4.4.0.3".into()));
        assert_eq!(plain_update("4.4.0.3", false, Some("4.4.0.3"), Some("4.4.0.3")), PlainUpdate::UpToDate);
        // The container says nothing: the tag stands for it.
        assert_eq!(plain_update("4.4.0.2", false, None, Some("4.4.0.3")), PlainUpdate::To("4.4.0.3".into()));
        // Releases not known, latest, or the operator's file: pull the tag.
        assert_eq!(plain_update("4.4.0.2", false, Some("4.4.0.2"), None), PlainUpdate::Pull);
        assert_eq!(plain_update("latest", false, Some("4.4.0.2"), Some("4.4.0.3")), PlainUpdate::Pull);
        assert_eq!(plain_update("4.4.0.2", true, Some("4.4.0.2"), Some("4.4.0.3")), PlainUpdate::Pull);
    }
}
