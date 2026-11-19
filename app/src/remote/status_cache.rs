// Copyright © 2016-2026 The SENTIENT Authors

//! Every server's last state, shared by everything that shows it: the
//! Instances cards, the switcher's dots and the tray's checks
//! (crate::manager::status_store reads it on a timer; crate::bg_app reuses
//! it).
//!
//! - One request per server at a time: a second ask while one is out waits
//!   for it ([`fetch`]) or asks for one more read when it ends ([`begin`]
//!   with `again`), never a second request beside it.
//! - What shows while a read is out is the last answer. An answer replaces
//!   it, also an answer that is an error the server gave (not set up, needs
//!   sudo, …). No answer at all (the server or the network down) keeps the
//!   last answer and marks it stale, with why.
//! - The last answer of each server is kept on disk, so a fresh start shows
//!   it at once, as "checking" until the server answers again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Instance, RemoteStatus};

/// Whether an error is the server's answer (it was reached and said why
/// it cannot report), rather than no answer at all.
pub fn is_answer(e: &str) -> bool {
    super::needs_setup(e)
        || e == super::FORGOTTEN
        || e == super::NOT_ADDED
        || e == super::AGENT_OLD
        || e.contains("administrator rights")
        || super::is_not_admin(e)
}

/// The last answer, as kept on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    Ok(RemoteStatus),
    Err(String),
}

impl Answer {
    fn of(r: &Result<RemoteStatus, String>) -> Self {
        match r {
            Ok(s) => Answer::Ok(s.clone()),
            Err(e) => Answer::Err(e.clone()),
        }
    }

    fn result(&self) -> Result<RemoteStatus, String> {
        match self {
            Answer::Ok(s) => Ok(s.clone()),
            Answer::Err(e) => Err(e.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Saved {
    answer: Answer,
    /// When it answered (Unix seconds).
    at: i64,
}

#[derive(Debug, Default)]
struct Entry {
    /// The last answer and when it came (Unix seconds).
    last: Option<(Answer, i64)>,
    /// It answered since this Platform Manager started (else `last` is
    /// from disk).
    live: bool,
    /// The last read got no answer: why (the last answer is stale).
    no_answer: Option<String>,
    /// When the last read ended.
    checked: Option<Instant>,
    in_flight: bool,
    /// Read once more when the read under way ends (a job changed the
    /// server meanwhile).
    again: bool,
}

/// What a card or a dot shows for one server.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct View {
    /// The last answer; or, when it never answered, why not.
    pub status: Option<Result<RemoteStatus, String>>,
    /// Not answered yet since the start (`status` is the one kept on disk),
    /// or never read: "Checking…".
    pub checking: bool,
    /// `status` is an earlier answer: the server did not answer since (why).
    pub stale: Option<String>,
    /// When `status` was answered (Unix seconds).
    pub answered_at: Option<i64>,
    /// A read is out now.
    pub in_flight: bool,
}

/// The cache itself (one shared instance: [`shared`]).
#[derive(Debug, Default)]
pub struct Cache {
    entries: HashMap<String, Entry>,
}

impl Cache {
    /// The answers kept on disk, as "checking" until each server answers.
    pub fn from_saved(text: &str) -> Self {
        let saved: HashMap<String, Saved> = serde_json::from_str(text).unwrap_or_default();
        let entries = saved.into_iter().map(|(id, s)| (id, Entry { last: Some((s.answer, s.at)), ..Default::default() })).collect();
        Cache { entries }
    }

    /// What goes to disk: each server's last answer.
    pub fn to_saved(&self) -> String {
        let saved: HashMap<&String, Saved> = self.entries.iter().filter_map(|(id, e)| e.last.as_ref().map(|(a, at)| (id, Saved { answer: a.clone(), at: *at }))).collect();
        serde_json::to_string_pretty(&saved).unwrap_or_default()
    }

    /// Start a read of `id`: false when one is out already (with `again`,
    /// that one is followed by one more).
    pub fn begin(&mut self, id: &str, again: bool) -> bool {
        let e = self.entries.entry(id.to_string()).or_default();
        if e.in_flight {
            e.again |= again;
            return false;
        }
        e.in_flight = true;
        true
    }

    /// A read ended with `got` at `now` (Unix seconds). Returns whether one
    /// more read was asked for meanwhile.
    pub fn finish(&mut self, id: &str, got: &Result<RemoteStatus, String>, now: i64) -> bool {
        let e = self.entries.entry(id.to_string()).or_default();
        e.in_flight = false;
        e.checked = Some(Instant::now());
        match got {
            Err(why) if !is_answer(why) => e.no_answer = Some(why.clone()),
            answer => {
                e.last = Some((Answer::of(answer), now));
                e.live = true;
                e.no_answer = None;
            }
        }
        std::mem::take(&mut e.again)
    }

    /// An answer that came another way (an error the page got itself).
    pub fn set(&mut self, id: &str, got: &Result<RemoteStatus, String>, now: i64) {
        let e = self.entries.entry(id.to_string()).or_default();
        let (in_flight, again) = (e.in_flight, e.again);
        self.finish(id, got, now);
        let e = self.entries.entry(id.to_string()).or_default();
        e.in_flight = in_flight;
        e.again = again;
    }

    pub fn view(&self, id: &str) -> View {
        let Some(e) = self.entries.get(id) else { return View { checking: true, ..Default::default() } };
        let mut v = View { in_flight: e.in_flight, ..Default::default() };
        match (&e.last, &e.no_answer) {
            (Some((a, at)), why) => {
                v.status = Some(a.result());
                v.answered_at = Some(*at);
                v.stale = why.clone();
                v.checking = !e.live && why.is_none();
            }
            // Never answered: why, as before.
            (None, Some(why)) => v.status = Some(Err(why.clone())),
            (None, None) => v.checking = true,
        }
        v
    }

    /// The servers' list changed: forget the ones removed. Returns whether
    /// anything was forgotten.
    pub fn retain(&mut self, ids: &[String]) -> bool {
        let before = self.entries.len();
        self.entries.retain(|id, _| ids.contains(id));
        before != self.entries.len()
    }

    /// The result of a read that ended less than `max_age` ago, if any.
    pub fn recent(&self, id: &str, max_age: Duration) -> Option<Result<RemoteStatus, String>> {
        let e = self.entries.get(id)?;
        if e.checked?.elapsed() >= max_age {
            return None;
        }
        match (&e.no_answer, &e.last) {
            (Some(why), _) => Some(Err(why.clone())),
            (None, Some((a, _))) => Some(a.result()),
            (None, None) => None,
        }
    }

    pub fn in_flight(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|e| e.in_flight)
    }
}

/// When each of `n` servers is read in one round, from its start: one
/// after another a little apart, never all at once; spread over the whole
/// `every` after the computer slept (`overslept`), when every server is due
/// at the same moment and the network may only just be back.
pub fn offsets(n: usize, every: Duration, overslept: bool) -> Vec<Duration> {
    if n == 0 {
        return Vec::new();
    }
    let gap = if overslept { every / n as u32 } else { (every / n as u32).min(Duration::from_millis(500)) };
    (0..n).map(|i| gap * i as u32).collect()
}

/// Whether a round's timer fired so late that the computer must have slept.
pub fn overslept(waited: Duration, every: Duration) -> bool {
    waited > every * 2
}

// ---------------------------------------------------------------------------
// The one shared cache
// ---------------------------------------------------------------------------

struct Shared {
    cache: Mutex<Option<Cache>>,
    /// Woken when a read ends ([`fetch`] waits for one under way).
    done: Condvar,
}

static SHARED: Shared = Shared { cache: Mutex::new(None), done: Condvar::new() };

fn lock() -> MutexGuard<'static, Option<Cache>> {
    SHARED.cache.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` on the shared cache (loaded from disk the first time).
pub fn with<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    let mut g = lock();
    f(g.get_or_insert_with(load))
}

fn load() -> Cache {
    Cache::from_saved(&std::fs::read_to_string(path()).unwrap_or_default())
}

/// Where the last answers are kept.
pub fn path() -> PathBuf {
    crate::updates::prefs_dir().join("remote-status.json")
}

fn save(text: &str) {
    let p = path();
    let _ = write_atomic(&p, text);
}

fn write_atomic(p: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn view(id: &str) -> View {
    with(|c| c.view(id))
}

/// An answer the caller got itself (kept, and on disk).
pub fn set(id: &str, got: &Result<RemoteStatus, String>) {
    let text = with(|c| {
        c.set(id, got, now());
        c.to_saved()
    });
    save(&text);
}

/// The list of servers changed.
pub fn retain(ids: &[String]) {
    if let Some(text) = with(|c| c.retain(ids).then(|| c.to_saved())) {
        save(&text);
    }
}

/// Read `inst` now, unless a read is out already (then, with `again`, it is
/// read once more after that one). Blocking. Returns whether it read.
pub fn refresh(inst: &Instance, again: bool) -> bool {
    if !with(|c| c.begin(&inst.id, again)) {
        return false;
    }
    loop {
        let got = super::status(inst);
        let (more, text) = with(|c| {
            let before = c.to_saved();
            let more = c.finish(&inst.id, &got, now());
            let after = c.to_saved();
            (more, (after != before).then_some(after))
        });
        SHARED.done.notify_all();
        if let Some(t) = text {
            save(&t);
        }
        if !more || !with(|c| c.begin(&inst.id, false)) {
            return true;
        }
    }
}

/// The server's state, read at most `max_age` ago: the cached one when it
/// is that recent, else the read under way, else a new read. Blocking (the
/// tray's checks).
pub fn fetch(inst: &Instance, max_age: Duration) -> Result<RemoteStatus, String> {
    loop {
        {
            let mut g = lock();
            loop {
                let c = g.get_or_insert_with(load);
                if let Some(r) = c.recent(&inst.id, max_age) {
                    return r;
                }
                if !c.in_flight(&inst.id) {
                    break;
                }
                g = SHARED.done.wait_timeout(g, Duration::from_secs(60)).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
        // Another read may start between: then wait for that one.
        if refresh(inst, false) {
            if let Some(r) = with(|c| c.recent(&inst.id, Duration::from_secs(3600))) {
                return r;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> Result<RemoteStatus, String> {
        Ok(RemoteStatus { installed: true, serving: true, sentient_version: "4.4.0.2".into(), ..Default::default() })
    }

    fn stopped() -> Result<RemoteStatus, String> {
        Ok(RemoteStatus { installed: true, serving: false, sentient_version: "4.4.0.2".into(), ..Default::default() })
    }

    const DOWN: &str = "The server did not answer (address, port or firewall).";

    #[test]
    fn a_server_never_read_is_being_checked() {
        let c = Cache::default();
        let v = c.view("a");
        assert!(v.checking);
        assert_eq!(v.status, None);
        assert_eq!(v.stale, None);
    }

    #[test]
    fn an_answer_replaces_the_last_one() {
        let mut c = Cache::default();
        assert!(c.begin("a", false));
        c.finish("a", &running(), 100);
        assert_eq!(c.view("a"), View { status: Some(running()), answered_at: Some(100), ..Default::default() });
        assert!(c.begin("a", false));
        c.finish("a", &stopped(), 115);
        let v = c.view("a");
        assert_eq!(v.status, Some(stopped()));
        assert_eq!(v.answered_at, Some(115));
        assert!(!v.checking && v.stale.is_none());
    }

    #[test]
    fn the_last_answer_shows_while_a_read_is_out() {
        let mut c = Cache::default();
        c.begin("a", false);
        c.finish("a", &running(), 100);
        c.begin("a", false);
        let v = c.view("a");
        assert_eq!(v.status, Some(running()));
        assert!(v.in_flight);
        assert!(!v.checking, "a read under way is not \"checking\" once it answered");
        assert_eq!(v.stale, None, "not stale just because a read is out");
    }

    #[test]
    fn no_answer_keeps_the_last_one_and_marks_it_stale() {
        let mut c = Cache::default();
        c.begin("a", false);
        c.finish("a", &running(), 100);
        c.begin("a", false);
        c.finish("a", &Err(DOWN.into()), 115);
        let v = c.view("a");
        assert_eq!(v.status, Some(running()));
        assert_eq!(v.answered_at, Some(100));
        assert_eq!(v.stale.as_deref(), Some(DOWN));
        // It answers again: no longer stale.
        c.begin("a", false);
        c.finish("a", &stopped(), 130);
        let v = c.view("a");
        assert_eq!(v.status, Some(stopped()));
        assert_eq!(v.stale, None);
    }

    #[test]
    fn a_server_that_never_answered_shows_why() {
        let mut c = Cache::default();
        c.begin("a", false);
        c.finish("a", &Err(DOWN.into()), 100);
        let v = c.view("a");
        assert_eq!(v.status, Some(Err(DOWN.to_string())));
        assert_eq!(v.stale, None);
        assert!(!v.checking);
    }

    #[test]
    fn an_error_the_server_gave_is_an_answer() {
        let mut c = Cache::default();
        c.begin("a", false);
        c.finish("a", &running(), 100);
        c.begin("a", false);
        c.finish("a", &Err(crate::remote::NOT_SET_UP.into()), 115);
        let v = c.view("a");
        assert_eq!(v.status, Some(Err(crate::remote::NOT_SET_UP.to_string())));
        assert_eq!(v.stale, None);
        assert!(is_answer(crate::remote::FORGOTTEN));
        assert!(is_answer(&crate::remote::not_admin_message("ops", "plant-b")));
        assert!(!is_answer(DOWN));
        assert!(!is_answer("The connection to the server keeps dropping: the network or VPN between here and the server is losing packets. Check it, then try again."));
    }

    #[test]
    fn one_read_at_a_time_per_server() {
        let mut c = Cache::default();
        assert!(c.begin("a", false));
        assert!(!c.begin("a", false), "a second read beside the one out");
        assert!(c.begin("b", false), "another server reads at the same time");
        // A poll while one is out asks for nothing more.
        assert!(!c.finish("a", &running(), 100));
        assert!(c.begin("a", false));
        // A job ended meanwhile: exactly one more read, however often asked.
        assert!(!c.begin("a", true));
        assert!(!c.begin("a", true));
        assert!(c.finish("a", &running(), 101));
        assert!(c.begin("a", false));
        assert!(!c.finish("a", &running(), 102));
    }

    #[test]
    fn the_last_answers_survive_a_restart_as_checking() {
        let mut c = Cache::default();
        c.begin("a", false);
        c.finish("a", &running(), 100);
        c.begin("b", false);
        c.finish("b", &Err(crate::remote::NOT_SET_UP.into()), 101);
        // No answer is not kept: the server's last answer is.
        c.begin("a", false);
        c.finish("a", &Err(DOWN.into()), 115);
        c.begin("c", false);
        c.finish("c", &Err(DOWN.into()), 116);
        let text = c.to_saved();
        let d = Cache::from_saved(&text);
        let a = d.view("a");
        assert_eq!(a.status, Some(running()));
        assert_eq!(a.answered_at, Some(100));
        assert!(a.checking && a.stale.is_none());
        let b = d.view("b");
        assert_eq!(b.status, Some(Err(crate::remote::NOT_SET_UP.to_string())));
        assert!(b.checking);
        assert_eq!(d.view("c"), View { checking: true, ..Default::default() });
        // And through a file.
        let p = std::env::temp_dir().join(format!("spm-status-cache-{}.json", std::process::id()));
        write_atomic(&p, &text).unwrap();
        let e = Cache::from_saved(&std::fs::read_to_string(&p).unwrap());
        assert_eq!(e.view("a"), a);
        let _ = std::fs::remove_file(&p);
        // An unreadable file is an empty cache.
        assert_eq!(Cache::from_saved("not json").view("a"), View { checking: true, ..Default::default() });
    }

    #[test]
    fn a_kept_answer_is_stale_when_the_first_read_gets_none() {
        let mut c = Cache::default();
        c.begin("a", false);
        c.finish("a", &running(), 100);
        let mut d = Cache::from_saved(&c.to_saved());
        d.begin("a", false);
        d.finish("a", &Err(DOWN.into()), 200);
        let v = d.view("a");
        assert_eq!(v.status, Some(running()));
        assert!(!v.checking);
        assert_eq!(v.stale.as_deref(), Some(DOWN));
    }

    #[test]
    fn removed_servers_are_forgotten() {
        let mut c = Cache::default();
        c.begin("a", false);
        c.finish("a", &running(), 100);
        c.begin("b", false);
        c.finish("b", &running(), 100);
        assert!(c.retain(&["b".to_string()]));
        assert!(!c.retain(&["b".to_string()]));
        assert!(c.view("a").checking);
        assert!(!c.to_saved().contains("\"a\""));
    }

    #[test]
    fn recent_reads_are_reused() {
        let mut c = Cache::default();
        assert_eq!(c.recent("a", Duration::from_secs(30)), None);
        c.begin("a", false);
        c.finish("a", &running(), 100);
        assert_eq!(c.recent("a", Duration::from_secs(30)), Some(running()));
        assert_eq!(c.recent("a", Duration::ZERO), None);
        c.begin("a", false);
        c.finish("a", &Err(DOWN.into()), 115);
        assert_eq!(c.recent("a", Duration::from_secs(30)), Some(Err(DOWN.to_string())));
        // Kept on disk only: not a recent read.
        let d = Cache::from_saved(&c.to_saved());
        assert_eq!(d.recent("a", Duration::from_secs(30)), None);
    }

    #[test]
    fn a_set_answer_leaves_the_read_under_way_alone() {
        let mut c = Cache::default();
        c.begin("a", true);
        c.set("a", &Err(crate::remote::NOT_SET_UP.into()), 100);
        assert!(c.in_flight("a"));
        assert_eq!(c.view("a").status, Some(Err(crate::remote::NOT_SET_UP.to_string())));
    }

    #[test]
    fn rounds_are_spread_out() {
        let every = Duration::from_secs(15);
        assert!(offsets(0, every, false).is_empty());
        assert_eq!(offsets(3, every, false), vec![Duration::ZERO, Duration::from_millis(500), Duration::from_millis(1000)]);
        // Many servers: still no more than one round long.
        let many = offsets(60, every, false);
        assert!(many.windows(2).all(|w| w[1] > w[0]));
        assert!(*many.last().unwrap() < every);
        // After a sleep: over the whole round.
        assert_eq!(offsets(3, every, true), vec![Duration::ZERO, Duration::from_secs(5), Duration::from_secs(10)]);
        assert!(!overslept(Duration::from_secs(16), every));
        assert!(overslept(Duration::from_secs(600), every));
    }
}
