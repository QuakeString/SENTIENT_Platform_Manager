// Copyright © 2016-2026 The SENTIENT Authors

//! What is newer, on the update channel the operator chose.
//!
//! Two products are released on GitHub: SENTIENT on
//! github.com/QuakeString/sentient-release (beside the gateway's
//! `gateway-*` releases, which are not SENTIENT and are skipped), and the
//! Platform Manager on its own repository. Both are read from the public
//! releases API, without a token.
//!
//! A channel decides which releases count:
//! - Stable: full releases only.
//! - Release candidate: pre-releases too (the `-rc` builds).
//! - Nightly: the release tagged `nightly`, rebuilt from master.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

pub const SENTIENT_REPO: &str = "QuakeString/sentient-release";
pub const MANAGER_REPO: &str = "QuakeString/SENTIENT_Platform_Manager";
/// The tag a nightly build is published under (one release, replaced).
pub const NIGHTLY_TAG: &str = "nightly";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Candidate,
    Nightly,
}

impl Channel {
    pub const ALL: [Channel; 3] = [Channel::Stable, Channel::Candidate, Channel::Nightly];

    pub fn label(self) -> &'static str {
        match self {
            Channel::Stable => "Stable",
            Channel::Candidate => "Release candidate",
            Channel::Nightly => "Nightly",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Channel::Stable => "Full releases only.",
            Channel::Candidate => "Also release candidates: new features before they are final.",
            Channel::Nightly => "The newest build of the development branch, every night. For testing.",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

/// One GitHub release, as the API returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    /// The version the tag names: `v0.8.0-rc6` → `0.8.0-rc6`, `4.3.0.50` as is.
    /// A nightly's tag is always `nightly`: its title carries the build's
    /// version (`0.8.0-nightly.20261025`).
    pub fn version(&self) -> String {
        if self.is_nightly() {
            if let Some(name) = self.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                return name.trim_start_matches('v').to_string();
            }
        }
        self.tag_name.trim_start_matches('v').to_string()
    }

    pub fn is_nightly(&self) -> bool {
        self.tag_name == NIGHTLY_TAG
    }

    /// The release notes, without GitHub's trailing whitespace.
    pub fn notes(&self) -> String {
        self.body.clone().unwrap_or_default().trim().to_string()
    }

    /// The published date, `2026-10-24`.
    pub fn date(&self) -> String {
        self.published_at.clone().unwrap_or_default().chars().take(10).collect()
    }
}

/// The repository's releases, newest first (the API's order).
pub fn fetch(repo: &str) -> Result<Vec<Release>, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=50");
    let body = crate::net::agent()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "SENTIENT-Platform-Manager")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(403, _) | ureq::Error::Status(429, _) => {
                "GitHub is limiting requests from this network; try again in an hour.".to_string()
            }
            e => format!("the release site could not be reached: {e}"),
        })?
        .into_string()
        .map_err(|e| format!("could not read the release list: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("the release list is not valid JSON: {e}"))
}

/// The releases a channel offers, best first. `belongs` filters out another
/// product's releases sharing the repository.
pub fn on_channel<'a>(releases: &'a [Release], channel: Channel, belongs: impl Fn(&Release) -> bool) -> Vec<&'a Release> {
    let mut out: Vec<&Release> = releases
        .iter()
        .filter(|r| !r.draft && belongs(r))
        .filter(|r| match channel {
            Channel::Nightly => r.is_nightly(),
            Channel::Candidate => !r.is_nightly(),
            Channel::Stable => !r.is_nightly() && !r.prerelease,
        })
        .collect();
    out.sort_by(|a, b| compare(&b.version(), &a.version()));
    out
}

/// SENTIENT's releases in its repository: not the gateway's.
pub fn is_sentient(r: &Release) -> bool {
    !r.tag_name.starts_with("gateway-")
}

/// Whether `candidate` is newer than `installed`. A nightly is newer when it
/// differs at all: its version names the build, not an order.
pub fn is_newer(candidate: &str, installed: &str, nightly: bool) -> bool {
    if nightly {
        return candidate != installed;
    }
    compare(candidate, installed) == Ordering::Greater
}

/// Versions such as `4.3.0.50`, `0.8.0`, `0.8.0-rc6`: numbers compared part
/// by part, and a pre-release (`-rc6`) before the release it leads to.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (an, ap) = split(a);
    let (bn, bp) = split(b);
    let len = an.len().max(bn.len());
    for i in 0..len {
        let x = an.get(i).copied().unwrap_or(0);
        let y = bn.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    match (ap, bp) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let num = |s: &str| s.trim_start_matches(|c: char| !c.is_ascii_digit()).parse::<u64>().ok();
            match (num(x), num(y)) {
                (Some(p), Some(q)) if x.trim_end_matches(|c: char| c.is_ascii_digit()) == y.trim_end_matches(|c: char| c.is_ascii_digit()) => p.cmp(&q),
                _ => x.cmp(y),
            }
        }
    }
}

fn split(v: &str) -> (Vec<u64>, Option<&str>) {
    let v = v.trim().trim_start_matches('v');
    let (numbers, pre) = match v.split_once('-') {
        Some((n, p)) => (n, Some(p)),
        None => (v, None),
    };
    (numbers.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool) -> Release {
        Release {
            tag_name: tag.into(),
            name: None,
            body: None,
            html_url: String::new(),
            prerelease: pre,
            draft: false,
            published_at: None,
            assets: vec![],
        }
    }

    #[test]
    fn versions_order() {
        assert_eq!(compare("4.3.0.50", "4.3.0.49"), Ordering::Greater);
        assert_eq!(compare("4.3.0.100", "4.3.0.99"), Ordering::Greater);
        assert_eq!(compare("0.8.0-rc6", "0.8.0-rc5"), Ordering::Greater);
        assert_eq!(compare("0.8.0-rc10", "0.8.0-rc9"), Ordering::Greater);
        assert_eq!(compare("0.8.0", "0.8.0-rc6"), Ordering::Greater);
        assert_eq!(compare("0.9.0-rc1", "0.8.0"), Ordering::Greater);
        assert_eq!(compare("v0.8.0", "0.8.0"), Ordering::Equal);
        assert_eq!(compare("4.3.0", "4.3.0.0"), Ordering::Equal);
    }

    #[test]
    fn channels_pick_their_releases() {
        let all = vec![
            rel("gateway-4.12.3", false),
            rel("4.3.0.49", false),
            rel("4.3.0.51", true),
            rel("4.3.0.50", false),
            rel("nightly", true),
        ];
        let stable = on_channel(&all, Channel::Stable, is_sentient);
        assert_eq!(stable.iter().map(|r| r.tag_name.as_str()).collect::<Vec<_>>(), ["4.3.0.50", "4.3.0.49"]);
        let rc = on_channel(&all, Channel::Candidate, is_sentient);
        assert_eq!(rc[0].tag_name, "4.3.0.51");
        let nightly = on_channel(&all, Channel::Nightly, is_sentient);
        assert_eq!(nightly.len(), 1);
        assert!(nightly[0].is_nightly());
    }

    #[test]
    fn newer() {
        assert!(is_newer("4.3.0.50", "4.3.0.49", false));
        assert!(!is_newer("4.3.0.49", "4.3.0.49", false));
        assert!(!is_newer("4.3.0.48", "4.3.0.49", false));
        assert!(is_newer("4.3.0.51-nightly.20261025", "4.3.0.50", true));
        assert!(!is_newer("4.3.0.51-nightly.20261025", "4.3.0.51-nightly.20261025", true));
    }
}
