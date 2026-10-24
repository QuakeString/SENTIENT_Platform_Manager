// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT from github.com/QuakeString/sentient-releases instead of an
//! offline package: the latest release's manifest, and downloading what this
//! computer needs into a folder that then works as a package
//! (manifest::Manifest::download_package).

use std::path::PathBuf;

use sentient_installer_core::manifest::{self, Manifest};
use sentient_installer_core::progress::ProgressFn;

/// The newest release. Blocking: call from a job or a background task.
pub fn latest() -> Result<Manifest, String> {
    manifest::fetch(manifest::LATEST)
}

/// Where downloaded releases are kept (one folder per version, reused by a
/// retry): beside the installation record on Windows, the user's cache on
/// Linux (the download runs as the user; root jobs read it from there).
pub fn downloads_dir(version: &str) -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("SENTIENT")
        .join("downloads");
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/var/cache"))
        .join("sentient-platform-manager")
        .join("downloads");
    base.join(version)
}

/// Download `version` (or the latest) as a package folder.
pub fn download(version: Option<&str>, sink: &ProgressFn) -> Result<(String, PathBuf), String> {
    let m = match version {
        Some(v) => manifest::fetch(&manifest::release_url(v))?,
        None => latest()?,
    };
    let dir = m.download_package(sink, &downloads_dir(&m.version))?;
    Ok((m.version, dir))
}

/// "1.2 GB" / "450 MB"
pub fn size_label(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", bytes >> 20)
    }
}
