// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT from github.com/QuakeString/sentient-release instead of an
//! offline package: the latest release's manifest, and downloading what this
//! computer needs into a folder that then works as a package
//! (manifest::Manifest::download_package).

use std::path::PathBuf;

use sentient_installer_core::manifest::{self, Manifest};
use sentient_installer_core::progress::ProgressFn;
use sentient_installer_core::releases;

/// The newest release, when it has what this computer needs (a release
/// may carry only some systems' files). Blocking: call from a job or a
/// background task.
pub fn latest() -> Result<Manifest, String> {
    let m = manifest::fetch(manifest::LATEST)?;
    m.wanted()?;
    Ok(m)
}

/// One SENTIENT version that can be picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// The release number, which is also its tag (`4.4.0.1`).
    pub version: String,
    /// Published, `2026-11-13`.
    pub date: String,
    /// The notes' one-line summary.
    pub summary: String,
}

impl Version {
    /// Short, for a list: `4.4.0.1 · 2026-11-13`.
    pub fn label(&self) -> String {
        if self.date.is_empty() { self.version.clone() } else { format!("{} · {}", self.version, self.date) }
    }
}

/// The published SENTIENT versions this computer can install, newest first
/// (releases::numbered). `native`: only releases that carry this computer's
/// files (a Docker install can use any, by image tag). Blocking. An error
/// means the list could not be read: the caller falls back to the latest
/// release alone.
pub fn versions(native: bool) -> Result<Vec<Version>, String> {
    let all = releases::fetch(releases::SENTIENT_REPO)?;
    let required = sentient_installer_core::native::required_bundle_files();
    Ok(pickable(&all, native.then_some(required.as_slice())))
}

/// The releases a picker lists: numbered full releases holding `required`
/// (and a manifest to check the download against).
fn pickable(all: &[releases::Release], required: Option<&[String]>) -> Vec<Version> {
    releases::numbered(all)
        .into_iter()
        .filter(|r| r.has_asset("manifest.json"))
        .filter(|r| required.is_none_or(|files| files.iter().all(|f| r.has_asset(f))))
        .map(|r| Version { version: r.tag_name.clone(), date: r.date(), summary: r.summary() })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool, files: &[&str]) -> releases::Release {
        releases::Release {
            tag_name: tag.into(),
            name: None,
            body: Some(format!("SENTIENT {tag}.\n\n### Fixed\n- x")),
            html_url: String::new(),
            prerelease: pre,
            draft: false,
            published_at: Some("2026-11-13T10:42:00Z".into()),
            assets: files
                .iter()
                .map(|f| releases::Asset { name: f.to_string(), browser_download_url: String::new(), size: 1 })
                .collect(),
        }
    }

    #[test]
    fn a_native_picker_lists_only_releases_with_this_computers_files() {
        let win = ["manifest.json", "sentient-payload.tar", "pg.zip"];
        let all = vec![
            rel("4.4.0.1", false, &win),
            rel("4.3.0.58", false, &["manifest.json", "sentient-payload-linux-x86_64.tar"]),
            rel("4.3.0.57", false, &win),
            rel("4.3.0.40", false, &["sentient-payload.tar", "pg.zip"]),
            rel("4.4.0.2-rc1", true, &win),
            rel("gateway-4.16.1", false, &["manifest.json"]),
        ];
        let required = vec!["sentient-payload.tar".to_string(), "pg.zip".to_string()];
        let native: Vec<String> = pickable(&all, Some(&required)).into_iter().map(|v| v.version).collect();
        assert_eq!(native, ["4.4.0.1", "4.3.0.57"]);
        let docker: Vec<String> = pickable(&all, None).into_iter().map(|v| v.version).collect();
        assert_eq!(docker, ["4.4.0.1", "4.3.0.58", "4.3.0.57"]);
    }

    #[test]
    fn version_labels_are_short() {
        let v = &pickable(&[rel("4.4.0.1", false, &["manifest.json"])], None)[0];
        assert_eq!(v.label(), "4.4.0.1 · 2026-11-13");
        assert_eq!(v.summary, "SENTIENT 4.4.0.1.");
    }
}
