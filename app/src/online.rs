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
///
/// Docker: only versions whose image is on Docker Hub; without GitHub's
/// list, Docker Hub's numbered tags alone (no dates or notes). Native:
/// GitHub's releases that carry this system's files.
pub fn versions(native: bool) -> Result<Vec<Version>, String> {
    use sentient_installer_core::dockerhub;
    let all = match releases::fetch(releases::SENTIENT_REPO) {
        Ok(all) => all,
        Err(e) if !native => {
            let tags = dockerhub::tags(dockerhub::SENTIENT_IMAGE, false).map_err(|_| e)?;
            return Ok(from_tags(&tags.value));
        }
        Err(e) => return Err(e),
    };
    let required = sentient_installer_core::native::required_bundle_files();
    let list = pickable(&all, native.then_some(required.as_slice()));
    if native {
        return Ok(list);
    }
    // Docker Hub not reachable: the list as GitHub has it.
    Ok(match dockerhub::tags(dockerhub::SENTIENT_IMAGE, false) {
        Ok(tags) => published(list, &tags.value),
        Err(_) => list,
    })
}

/// The versions whose image tag is published.
fn published(list: Vec<Version>, tags: &[String]) -> Vec<Version> {
    list.into_iter().filter(|v| tags.iter().any(|t| *t == v.version)).collect()
}

/// Docker Hub's plain numbered tags as versions, newest first.
fn from_tags(tags: &[String]) -> Vec<Version> {
    sentient_installer_core::dockerhub::on_channel(tags, releases::Channel::Stable)
        .into_iter()
        .map(|version| Version { version, date: String::new(), summary: String::new() })
        .collect()
}

/// The releases a picker lists: numbered full releases holding `required`
/// (and a manifest to check the download against).
fn pickable(all: &[releases::Release], required: Option<&[String]>) -> Vec<Version> {
    releases::numbered(all)
        .into_iter()
        .filter(|r| r.has_asset("manifest.json"))
        // Each file under any of its names (the Windows payload's old one too).
        .filter(|r| required.is_none_or(|files| files.iter().all(|f| sentient_installer_core::native::present_name(f, |n| r.has_asset(n)).is_some())))
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

/// Download `version` (or the latest) as a package folder. The manifest's
/// signature is checked first (crate::signing's rule; an unsigned release
/// from before signing began is used with a warning), then every file by
/// the SHA-256 the manifest gives.
pub fn download(version: Option<&str>, sink: &ProgressFn) -> Result<(String, PathBuf), String> {
    let feed = releases::fetch(releases::SENTIENT_REPO).ok();
    let started = crate::updates::signing_started(feed.as_deref(), sentient_installer_core::signing::Product::Sentient);
    let url = match version {
        Some(v) => manifest::release_url(v),
        None => manifest::LATEST.to_string(),
    };
    let (m, trust) = manifest::fetch_signed(&url, started)?;
    m.wanted()?;
    // Native Linux: a server binary this glibc cannot load is not
    // downloaded (Setup and the Update page say so before; this covers
    // every other way here, the agent's install on a server too).
    if cfg!(target_os = "linux") {
        use sentient_installer_core::glibc;
        if let glibc::NativeGlibc::Blocked(why) = glibc::decide(&m.version, m.min_glibc(), &glibc::local(), &glibc::local_os_name(), if cfg!(feature = "gui") { glibc::Here::Computer } else { glibc::Here::Server }) {
            return Err(why);
        }
    }
    if let Some(w) = trust.warning(&format!("SENTIENT {}", m.version)) {
        sink(sentient_installer_core::progress::Progress::Log { line: w });
    }
    let dir = m.download_package(sink, &downloads_dir(&m.version))?;
    Ok((m.version, dir))
}

/// "1.2 GB" / "450 MB"
pub fn size_label(bytes: u64) -> String {
    sentient_installer_core::size::label(bytes)
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
                .map(|f| releases::Asset { name: f.to_string(), browser_download_url: String::new(), size: 1, digest: None })
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

    /// The Windows payload under its name from 4.4.1.3 on, or its old one.
    #[test]
    fn a_native_picker_takes_the_windows_payload_under_either_name() {
        let all = vec![
            rel("4.4.1.4", false, &["manifest.json", "sentient-payload-windows-x86_64.tar", "pg.zip"]),
            rel("4.4.1.3", false, &["manifest.json", "sentient-payload-windows-x86_64.tar", "sentient-payload.tar", "pg.zip"]),
            rel("4.4.1.2", false, &["manifest.json", "sentient-payload.tar", "pg.zip"]),
            rel("4.4.1.1", false, &["manifest.json", "pg.zip"]),
        ];
        let required = vec![sentient_installer_core::native::WINDOWS_PAYLOAD.to_string(), "pg.zip".to_string()];
        let native: Vec<String> = pickable(&all, Some(&required)).into_iter().map(|v| v.version).collect();
        assert_eq!(native, ["4.4.1.4", "4.4.1.3", "4.4.1.2"]);
    }

    #[test]
    fn a_docker_picker_lists_only_published_images() {
        let all = vec![rel("4.4.0.2", false, &["manifest.json"]), rel("4.4.0.1", false, &["manifest.json"]), rel("4.3.0.58", false, &["manifest.json"])];
        let tags: Vec<String> = ["latest", "4.4.0.1", "4.3.0.58", "nightly"].iter().map(|s| s.to_string()).collect();
        let got: Vec<String> = published(pickable(&all, None), &tags).into_iter().map(|v| v.version).collect();
        assert_eq!(got, ["4.4.0.1", "4.3.0.58"]);
        // Without GitHub: the numbered tags, newest first.
        let got: Vec<String> = from_tags(&tags).into_iter().map(|v| v.version).collect();
        assert_eq!(got, ["4.4.0.1", "4.3.0.58"]);
    }

    #[test]
    fn version_labels_are_short() {
        let v = &pickable(&[rel("4.4.0.1", false, &["manifest.json"])], None)[0];
        assert_eq!(v.label(), "4.4.0.1 · 2026-11-13");
        assert_eq!(v.summary, "SENTIENT 4.4.0.1.");
    }
}
