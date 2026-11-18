// Copyright © 2016-2026 The SENTIENT Authors

//! Timing marks for the performance audit (SPM_DEBUG_TIMING=1, also in
//! release builds): `TIMING <what> ms=<since main> epoch_ms=<wall clock>`
//! on stderr. docs/analysis/SPM_PERFORMANCE_AUDIT.md and
//! tests/layout/perf.sh read them.

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static ON: OnceLock<bool> = OnceLock::new();

/// Whether marks are written.
pub fn on() -> bool {
    *ON.get_or_init(|| std::env::var_os("SPM_DEBUG_TIMING").is_some())
}

/// The clock's start: called first thing in main.
pub fn start() {
    START.get_or_init(Instant::now);
}

/// Milliseconds since main started.
pub fn since_start() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.
}

/// A mark.
pub fn mark(what: &str) {
    if on() {
        let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        eprintln!("TIMING {what} ms={:.1} epoch_ms={epoch}", since_start());
    }
}

/// A mark with how long `since` took.
pub fn took(what: &str, since: Instant) {
    if on() {
        eprintln!("TIMING {what} took_ms={:.2} ms={:.1}", since.elapsed().as_secs_f64() * 1000., since_start());
    }
}

/// A mark the first time `key` is seen (the first frame something shows).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn once(key: &str, what: &str) {
    if on() {
        static SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
        let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if g.get_or_insert_with(Default::default).insert(key.to_string()) {
            drop(g);
            mark(what);
        }
    }
}
