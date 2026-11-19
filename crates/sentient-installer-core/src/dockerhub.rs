// Copyright © 2016-2026 The SENTIENT Authors

//! The image tags on Docker Hub, for SENTIENT and the gateway in Docker:
//! a release is offered to a Docker install only once its image is
//! published, and when GitHub refuses, the newest tag stands in for the
//! release list. Cached like the release lists (crate::fetch_cache).

use crate::fetch_cache::{Cache, Failure, Listed};
use crate::releases::{self, Channel};

/// SENTIENT's image (crate::distro::DeployConfig's default, without its tag).
pub const SENTIENT_IMAGE: &str = "quakestring/sentient";
/// Pages of 100 tags read at most: the newest come first.
const MAX_PAGES: usize = 3;
const AGENT: &str = "SENTIENT-Platform-Manager";

static TAGS: Cache<Vec<String>> = Cache::new("Docker Hub", "Image list", "dockerhub");
static EXISTS: Cache<bool> = Cache::new("Docker Hub", "Image list", "dockerhub-tag");

/// `quakestring/sentient:4.4.0.1` → `quakestring/sentient`; an official
/// image (`postgres`) is `library/postgres`.
pub fn repo_of(image: &str) -> String {
    let image = image.split('@').next().unwrap_or(image);
    let repo = match image.rsplit_once(':') {
        Some((r, t)) if !t.contains('/') => r,
        _ => image,
    };
    let repo = repo.strip_prefix("docker.io/").unwrap_or(repo);
    if repo.contains('/') { repo.to_string() } else { format!("library/{repo}") }
}

/// The repository's tags, newest pushed first (up to 300). `repo`:
/// `namespace/name`.
pub fn tags(repo: &str, force: bool) -> Result<Listed<Vec<String>>, String> {
    TAGS.get(repo, force, || list_pages(repo, &get))
}

/// Whether `repo:tag` is published. Answered from the tag list when it is
/// held and names it; else one small request (404: not published).
pub fn tag_exists(repo: &str, tag: &str, force: bool) -> Result<bool, String> {
    if !force && TAGS.peek(repo).is_some_and(|t| t.iter().any(|x| x == tag)) {
        return Ok(true);
    }
    EXISTS.get(&format!("{repo}:{tag}"), force, || exists(repo, tag, &get)).map(|l| l.value)
}

/// One GET: the body, `Ok(None)` for 404.
pub(crate) fn get(url: &str) -> Result<Option<String>, Failure> {
    match crate::net::get(url).map_err(Failure::Other)?.set("Accept", "application/json").set("User-Agent", AGENT).call() {
        Ok(r) => r.into_string().map(Some).map_err(|e| Failure::Other(format!("could not read Docker Hub's answer: {e}"))),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(403 | 429, _)) => Err(Failure::Limited("Docker Hub is limiting requests from this network; try again later.".into())),
        Err(e) => Err(Failure::Other(format!("Docker Hub could not be reached: {e}"))),
    }
}

fn list_pages(repo: &str, get: &dyn Fn(&str) -> Result<Option<String>, Failure>) -> Result<Vec<String>, Failure> {
    #[derive(serde::Deserialize)]
    struct Page {
        #[serde(default)]
        next: Option<String>,
        #[serde(default)]
        results: Vec<Tag>,
    }
    #[derive(serde::Deserialize)]
    struct Tag {
        name: String,
    }
    let mut url = Some(format!("https://hub.docker.com/v2/repositories/{repo}/tags?page_size=100"));
    let mut out = Vec::new();
    for _ in 0..MAX_PAGES {
        let Some(u) = url.take() else { break };
        let body = get(&u)?.ok_or_else(|| Failure::Other(format!("There is no image {repo} on Docker Hub.")))?;
        let page: Page = serde_json::from_str(&body).map_err(|e| Failure::Other(format!("Docker Hub's tag list is not valid JSON: {e}")))?;
        out.extend(page.results.into_iter().map(|t| t.name));
        // Only Docker Hub's own next page is followed.
        url = page.next.filter(|n| n.starts_with("https://hub.docker.com/"));
    }
    Ok(out)
}

fn exists(repo: &str, tag: &str, get: &dyn Fn(&str) -> Result<Option<String>, Failure>) -> Result<bool, Failure> {
    Ok(get(&format!("https://hub.docker.com/v2/repositories/{repo}/tags/{tag}"))?.is_some())
}

/// A release candidate's tag: `4.4.0.2-rc1`.
fn is_candidate(tag: &str) -> bool {
    tag.split_once('-').is_some_and(|(n, p)| releases::is_numbered(n) && p.strip_prefix("rc").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())))
}

/// The tags a channel offers, newest version first: Stable plain numbers
/// (`4.4.0.1`), Release candidate also `-rcN`, Nightly only `nightly`.
/// `latest` and other names never count.
pub fn on_channel(tags: &[String], channel: Channel) -> Vec<String> {
    let mut out: Vec<String> = tags
        .iter()
        .filter(|t| match channel {
            Channel::Stable => releases::is_numbered(t),
            Channel::Candidate => releases::is_numbered(t) || is_candidate(t),
            Channel::Nightly => t.as_str() == releases::NIGHTLY_TAG,
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| releases::compare(b, a));
    out.dedup();
    out
}

/// The newest tag on the channel.
pub fn newest(tags: &[String], channel: Channel) -> Option<String> {
    on_channel(tags, channel).into_iter().next()
}

/// What a Docker install shows for a release whose image is missing.
pub fn not_published(version: &str) -> String {
    format!("{version} is released; its Docker image is not published yet.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn repositories_of_images() {
        assert_eq!(repo_of("quakestring/sentient:4.4.0.1"), "quakestring/sentient");
        assert_eq!(repo_of("quakestring/sentient-gateway-rs"), "quakestring/sentient-gateway-rs");
        assert_eq!(repo_of("docker.io/quakestring/sentient:latest@sha256:ab"), "quakestring/sentient");
        assert_eq!(repo_of("postgres:18-alpine"), "library/postgres");
    }

    #[test]
    fn channels_pick_their_tags() {
        let all = tags(&["latest", "4.4.0.1", "nightly", "4.4.0.2-rc1", "4.3.0.100", "4.3.0.9", "dev", "4.4.0.2-nightly.1", "4.4"]);
        assert_eq!(on_channel(&all, Channel::Stable), ["4.4.0.1", "4.3.0.100", "4.3.0.9"]);
        assert_eq!(newest(&all, Channel::Candidate).as_deref(), Some("4.4.0.2-rc1"));
        assert_eq!(on_channel(&all, Channel::Nightly), ["nightly"]);
        // Gateway tags are three parts.
        let gw = tags(&["4.16.1", "4.16.10", "4.16.9", "latest"]);
        assert_eq!(newest(&gw, Channel::Stable).as_deref(), Some("4.16.10"));
        assert_eq!(newest(&tags(&["latest"]), Channel::Stable), None);
        assert_eq!(newest(&tags(&["4.16.1"]), Channel::Nightly), None);
    }

    #[test]
    fn pages_are_followed_up_to_three() {
        let asked = RefCell::new(Vec::<String>::new());
        let get = |url: &str| -> Result<Option<String>, Failure> {
            asked.borrow_mut().push(url.to_string());
            let n = asked.borrow().len();
            Ok(Some(format!(r#"{{"next":"https://hub.docker.com/v2/repositories/q/x/tags?page={}","results":[{{"name":"4.16.{n}"}}]}}"#, n + 1)))
        };
        let got = list_pages("q/x", &get).unwrap();
        assert_eq!(got, ["4.16.1", "4.16.2", "4.16.3"]);
        assert_eq!(asked.borrow().len(), 3);
        assert_eq!(asked.borrow()[0], "https://hub.docker.com/v2/repositories/q/x/tags?page_size=100");
        // No next page: one request.
        let once = |_: &str| -> Result<Option<String>, Failure> { Ok(Some(r#"{"next":null,"results":[{"name":"latest"}]}"#.into())) };
        assert_eq!(list_pages("q/x", &once).unwrap(), ["latest"]);
    }

    #[test]
    fn a_missing_tag_is_404() {
        let get = |url: &str| -> Result<Option<String>, Failure> { Ok(url.ends_with("/4.16.1").then(|| "{}".to_string())) };
        assert!(exists("q/x", "4.16.1", &get).unwrap());
        assert!(!exists("q/x", "4.16.2", &get).unwrap());
        let limited = |_: &str| -> Result<Option<String>, Failure> { Err(Failure::Limited("x".into())) };
        assert!(matches!(exists("q/x", "4.16.2", &limited), Err(Failure::Limited(_))));
    }

    #[test]
    fn the_unpublished_text() {
        assert_eq!(not_published("4.16.3"), "4.16.3 is released; its Docker image is not published yet.");
    }
}
