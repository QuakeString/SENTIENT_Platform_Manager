// Copyright © 2016-2026 The SENTIENT Authors

//! Lists read from release sites (GitHub's releases, Docker Hub's tags),
//! asked for at most once per key per [`WINDOW`], shared by every caller of
//! the process and kept on disk for the next one.
//!
//! GitHub allows 60 anonymous API requests per hour per address: one
//! screen, an install and a restart used half of that before this. Now:
//! - a key is fetched at most once per 30 minutes (a refused or failed
//!   request counts too, so a limit is not hammered);
//! - callers asking at once share one request (single-flight);
//! - the last good answer is kept in memory and in a file per key; when the
//!   site refuses (403/429) or cannot be reached, that answer is returned
//!   marked [`Stale`], with its age. Only a key never fetched is an error;
//! - "Check for updates" may skip the window ([`Cache::get`]'s `force`), at
//!   most once per [`FORCE_GAP`].
//!
//! Every call is blocking (run from a background task). The map's lock is
//! never held during a request or a file read/write: a caller marks the key
//! in flight, lets go, fetches, then takes the lock again to store.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// At most one request per key in this time.
pub const WINDOW: Duration = Duration::from_secs(30 * 60);
/// An explicit check skips the window, but never sooner than this after
/// the key's last request.
pub const FORCE_GAP: Duration = Duration::from_secs(60);
/// A request that never reached the site (no network) may be tried again
/// this soon: it cost the site nothing.
pub const UNREACHED_GAP: Duration = Duration::from_secs(60);

/// Why a fetch gave nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The site refused: rate limit (403/429).
    Limited(String),
    /// Anything else: no network, a bad answer.
    Other(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        match self {
            Failure::Limited(m) | Failure::Other(m) => m,
        }
    }
}

/// An answer older than the window, given because the site refused or could
/// not be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    /// When it was fetched (Unix seconds).
    pub fetched_at: u64,
    /// The site refused (a rate limit), rather than could not be reached.
    pub limited: bool,
    /// "GitHub", "Docker Hub".
    pub site: &'static str,
    /// "Release list", "Image list".
    pub what: &'static str,
}

impl Stale {
    pub fn age(&self) -> Duration {
        Duration::from_secs(unix_now().saturating_sub(self.fetched_at))
    }

    /// "Release list from 12 min ago: GitHub is limiting requests from this network."
    pub fn note(&self) -> String {
        let why = if self.limited {
            format!("{} is limiting requests from this network.", self.site)
        } else {
            format!("{} could not be reached.", self.site)
        };
        format!("{} from {}: {why}", self.what, ago(self.age()))
    }
}

/// "1 min ago", "45 min ago", "3 h ago", "2 days ago".
pub fn ago(age: Duration) -> String {
    let min = (age.as_secs() / 60).max(1);
    match min {
        m if m < 120 => format!("{m} min ago"),
        m if m < 48 * 60 => format!("{} h ago", m / 60),
        m => format!("{} days ago", m / (24 * 60)),
    }
}

/// A value, and whether it is an old one (see [`Stale`]).
#[derive(Debug, Clone)]
pub struct Listed<T> {
    pub value: T,
    pub stale: Option<Stale>,
}

/// One key's state.
struct Slot<T> {
    /// The last good value and when it was fetched (Unix seconds).
    good: Option<(T, u64)>,
    /// The last request (or, from disk, as long ago as it was made).
    attempt: Option<Instant>,
    /// The last request failed: how (cleared by a good one).
    failure: Option<Failure>,
    /// A caller is fetching it now; others wait for that answer.
    in_flight: bool,
    /// The file was read already (once per process).
    disk_read: bool,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot { good: None, attempt: None, failure: None, in_flight: false, disk_read: false }
    }
}

/// The file per key.
#[derive(Serialize, Deserialize)]
struct Saved<T> {
    /// The good value's fetch time (Unix seconds), with the value.
    fetched_at: u64,
    value: Option<T>,
    /// The last request (Unix seconds), and how it failed.
    #[serde(default)]
    attempted_at: u64,
    #[serde(default)]
    limited: bool,
    #[serde(default)]
    failure: Option<String>,
}

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where the files go (the app's settings folder). Unset: memory only.
pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

pub struct Cache<T> {
    site: &'static str,
    what: &'static str,
    /// The file name's prefix (`github`, `dockerhub`).
    prefix: &'static str,
    window: Duration,
    force_gap: Duration,
    /// None: the process-wide [`set_dir`].
    dir: Option<PathBuf>,
    slots: Mutex<BTreeMap<String, Slot<T>>>,
    done: Condvar,
}

/// Lets waiters go when the fetch ends, even by a panic.
struct InFlight<'a, T> {
    cache: &'a Cache<T>,
    key: &'a str,
    armed: bool,
}

impl<T> Drop for InFlight<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            if let Some(s) = self.cache.lock().get_mut(self.key) {
                s.in_flight = false;
            }
            self.cache.done.notify_all();
        }
    }
}

impl<T> Cache<T> {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Slot<T>>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<T: Clone + Serialize + DeserializeOwned> Cache<T> {
    pub const fn new(site: &'static str, what: &'static str, prefix: &'static str) -> Self {
        Cache { site, what, prefix, window: WINDOW, force_gap: FORCE_GAP, dir: None, slots: Mutex::new(BTreeMap::new()), done: Condvar::new() }
    }

    /// Other times and an own folder (tests).
    pub fn with(mut self, window: Duration, force_gap: Duration, dir: Option<PathBuf>) -> Self {
        self.window = window;
        self.force_gap = force_gap;
        self.dir = dir;
        self
    }

    fn file(&self, key: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref().or_else(|| DIR.get())?;
        let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
        Some(dir.join("release-cache").join(format!("{}-{safe}.json", self.prefix)))
    }

    /// The value for `key`: the cached one inside the window, else
    /// `fetch`'s (one caller fetches, the others wait for it). A failed
    /// fetch gives the last good value marked stale; only a key never
    /// fetched is an error. `force`: an explicit check (see [`FORCE_GAP`]).
    pub fn get(&self, key: &str, force: bool, fetch: impl FnOnce() -> Result<T, Failure>) -> Result<Listed<T>, String> {
        // The file, once, outside the lock.
        let unread = !self.lock().get(key).is_some_and(|s| s.disk_read);
        if unread {
            let saved = self.file(key).and_then(|p| read_saved::<T>(&p));
            let mut map = self.lock();
            let slot = map.entry(key.to_string()).or_default();
            if !slot.disk_read {
                slot.disk_read = true;
                if let Some(s) = saved {
                    if slot.attempt.is_none() {
                        let ago = Duration::from_secs(unix_now().saturating_sub(s.attempted_at.max(s.fetched_at)));
                        slot.attempt = Instant::now().checked_sub(ago);
                        slot.failure = s.failure.map(|m| if s.limited { Failure::Limited(m) } else { Failure::Other(m) });
                    }
                    if slot.good.is_none() {
                        slot.good = s.value.map(|v| (v, s.fetched_at));
                    }
                }
            }
        }
        {
            let mut map = self.lock();
            loop {
                let slot = map.entry(key.to_string()).or_default();
                if slot.in_flight {
                    map = self.done.wait(map).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                let gap = match &slot.failure {
                    Some(Failure::Other(_)) => UNREACHED_GAP.min(self.window),
                    _ => self.window,
                };
                let due = match slot.attempt {
                    None => true,
                    Some(t) => {
                        let since = t.elapsed();
                        since >= gap || (force && since >= self.force_gap)
                    }
                };
                if !due {
                    return self.answer(slot);
                }
                slot.in_flight = true;
                break;
            }
        }
        // The request, with no lock held.
        let mut flight = InFlight { cache: self, key, armed: true };
        let got = fetch();
        let (answer, save) = {
            let mut map = self.lock();
            let slot = map.entry(key.to_string()).or_default();
            slot.in_flight = false;
            slot.attempt = Some(Instant::now());
            match got {
                Ok(v) => {
                    slot.good = Some((v, unix_now()));
                    slot.failure = None;
                }
                Err(f) => slot.failure = Some(f),
            }
            let save = Saved {
                fetched_at: slot.good.as_ref().map(|g| g.1).unwrap_or(0),
                value: slot.good.as_ref().map(|g| g.0.clone()),
                attempted_at: unix_now(),
                limited: matches!(slot.failure, Some(Failure::Limited(_))),
                failure: slot.failure.as_ref().map(|f| f.message().to_string()),
            };
            (self.answer(slot), save)
        };
        flight.armed = false;
        drop(flight);
        self.done.notify_all();
        if let Some(p) = self.file(key) {
            write_saved(&p, &save);
        }
        answer
    }

    fn answer(&self, slot: &Slot<T>) -> Result<Listed<T>, String> {
        match (&slot.good, &slot.failure) {
            (Some((v, _)), None) => Ok(Listed { value: v.clone(), stale: None }),
            (Some((v, at)), Some(f)) => Ok(Listed {
                value: v.clone(),
                stale: Some(Stale { fetched_at: *at, limited: matches!(f, Failure::Limited(_)), site: self.site, what: self.what }),
            }),
            (None, Some(f)) => Err(f.message().to_string()),
            (None, None) => Err(format!("The {} has not been read yet.", self.what.to_lowercase())),
        }
    }

    /// The value held now (memory, else the file), without a request.
    pub fn peek(&self, key: &str) -> Option<T> {
        if let Some(v) = self.lock().get(key).and_then(|s| s.good.as_ref().map(|g| g.0.clone())) {
            return Some(v);
        }
        self.file(key).and_then(|p| read_saved::<T>(&p)).and_then(|s| s.value)
    }
}

fn read_saved<T: DeserializeOwned>(p: &Path) -> Option<Saved<T>> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn write_saved<T: Serialize>(p: &Path, s: &Saved<T>) {
    let Ok(text) = serde_json::to_string(s) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, p).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn cache(window_ms: u64, gap_ms: u64, dir: Option<PathBuf>) -> Cache<Vec<String>> {
        Cache::new("GitHub", "Release list", "test").with(Duration::from_millis(window_ms), Duration::from_millis(gap_ms), dir)
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spm-fetch-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn one_request_per_window() {
        let c = cache(150, 50, None);
        let calls = AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["a".to_string()])
        };
        for _ in 0..5 {
            assert_eq!(c.get("k", false, fetch).unwrap().value, ["a"]);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Another key is its own request.
        c.get("other", false, fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        std::thread::sleep(Duration::from_millis(170));
        c.get("k", false, fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_forced_check_skips_the_window_once_per_gap() {
        let c = cache(60_000, 100, None);
        let calls = AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        };
        c.get("k", false, fetch).unwrap();
        // Too soon after the last request: the cached answer.
        c.get("k", true, fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(120));
        c.get("k", true, fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        c.get("k", true, fetch).unwrap();
        c.get("k", false, fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn callers_at_once_share_one_request() {
        let c = Arc::new(cache(60_000, 1_000, None));
        let calls = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (c, calls) = (c.clone(), calls.clone());
                std::thread::spawn(move || {
                    c.get("k", false, || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(150));
                        Ok(vec!["x".to_string()])
                    })
                    .unwrap()
                    .value
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), ["x"]);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refusal_gives_the_last_good_list_marked_stale() {
        let c = cache(50, 10, None);
        c.get("k", false, || Ok(vec!["4.4.0.1".to_string()])).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        let got = c.get("k", false, || Err(Failure::Limited("GitHub is limiting requests from this network; try again in an hour.".into()))).unwrap();
        assert_eq!(got.value, ["4.4.0.1"]);
        let stale = got.stale.expect("stale");
        assert!(stale.limited);
        assert_eq!(stale.note(), "Release list from 1 min ago: GitHub is limiting requests from this network.");
        // Inside the window after the refusal: no request, still stale.
        let again = c.get("k", false, || panic!("no request inside the window")).unwrap();
        assert!(again.stale.is_some());
        // A good answer clears it.
        std::thread::sleep(Duration::from_millis(60));
        assert!(c.get("k", false, || Ok(vec![])).unwrap().stale.is_none());
    }

    #[test]
    fn a_429_or_no_network_is_stale_too_and_never_fetched_is_an_error() {
        let c = cache(50, 10, None);
        let e = c.get("k", false, || Err(Failure::Limited("GitHub is limiting requests from this network; try again in an hour.".into()))).unwrap_err();
        assert!(e.contains("limiting"));
        std::thread::sleep(Duration::from_millis(60));
        c.get("k", false, || Ok(vec!["a".to_string()])).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        let got = c.get("k", false, || Err(Failure::Other("the release site could not be reached".into()))).unwrap();
        let stale = got.stale.unwrap();
        assert!(!stale.limited);
        assert!(stale.note().ends_with("GitHub could not be reached."), "{}", stale.note());
    }

    #[test]
    fn the_last_good_list_survives_on_disk() {
        let dir = temp("disk");
        let c = cache(60_000, 10, Some(dir.clone()));
        c.get("QuakeString/sentient-release", false, || Ok(vec!["4.4.0.1".to_string()])).unwrap();
        assert!(dir.join("release-cache").join("test-QuakeString_sentient-release.json").exists());
        // A new process inside the window: the file, no request.
        let next = cache(60_000, 10, Some(dir.clone()));
        let got = next.get("QuakeString/sentient-release", false, || panic!("fresh on disk")).unwrap();
        assert_eq!(got.value, ["4.4.0.1"]);
        // Past the window and refused: the file's list, stale.
        let later = cache(0, 0, Some(dir.clone()));
        let got = later.get("QuakeString/sentient-release", false, || Err(Failure::Limited("limited".into()))).unwrap();
        assert_eq!(got.value, ["4.4.0.1"]);
        assert!(got.stale.unwrap().limited);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ages_read_plainly() {
        assert_eq!(ago(Duration::from_secs(5)), "1 min ago");
        assert_eq!(ago(Duration::from_secs(12 * 60)), "12 min ago");
        assert_eq!(ago(Duration::from_secs(3 * 3600)), "3 h ago");
        assert_eq!(ago(Duration::from_secs(3 * 86400)), "3 days ago");
    }
}
