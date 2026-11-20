// Copyright © 2016-2026 The SENTIENT Authors

//! "New" marks on menu items: a mark shows for one day from the first time
//! this computer showed it, then never again. The first time each mark was
//! shown is kept in `new-marks.json` beside the installation record.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// How long a mark shows after it first appeared.
pub const SHOW_FOR_SECS: i64 = 24 * 60 * 60;

fn path() -> PathBuf {
    crate::installation::record_path().with_file_name("new-marks.json")
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Whether a mark first shown at `first` still shows at `now`.
pub fn still_new(first: i64, now: i64) -> bool {
    now >= first && now - first < SHOW_FOR_SECS
}

fn read() -> BTreeMap<String, i64> {
    std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Whether the mark `id` shows now. The first call notes the time; a
/// computer that cannot keep the note shows the mark only this once.
pub fn show(id: &str) -> bool {
    let mut seen = read();
    if let Some(&first) = seen.get(id) {
        return still_new(first, now());
    }
    seen.insert(id.to_string(), now());
    if let Some(dir) = path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path(), serde_json::to_string(&seen).unwrap_or_default());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_shows_for_one_day_from_its_first_showing() {
        let first = 1_700_000_000;
        assert!(still_new(first, first));
        assert!(still_new(first, first + SHOW_FOR_SECS - 1));
        assert!(!still_new(first, first + SHOW_FOR_SECS));
        assert!(!still_new(first, first + 30 * SHOW_FOR_SECS));
        // A clock set back before the first showing: not shown.
        assert!(!still_new(first, first - 10));
    }
}
