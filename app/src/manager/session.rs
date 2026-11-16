// Copyright © 2016-2026 The SENTIENT Authors

//! The window as it was left (his ask): the open instance and page, the
//! window's size, position and maximized state, and the Platform settings
//! tab. Saved whenever it changes (a little after, while a window is being
//! dragged), so a crash or a kill keeps it; restored on start. The menu's
//! fold and the theme keep their own settings.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// The remote instance open (None: this computer).
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub page: Option<String>,
    /// The window's bounds when not maximized: x, y, width, height.
    #[serde(default)]
    pub bounds: Option<[f32; 4]>,
    #[serde(default)]
    pub maximized: bool,
    /// The Platform settings tab (its position).
    #[serde(default)]
    pub settings_tab: Option<usize>,
    /// Files: the left pane's share of the width (0.5: half).
    #[serde(default)]
    pub files_split: Option<f32>,
    /// Files: each pane's Size, Modified and Owner column widths.
    #[serde(default)]
    pub files_cols: Option<[[f32; 3]; 2]>,
    /// Files: the last folder open per place ("local" or an instance id).
    #[serde(default)]
    pub files_dirs: std::collections::BTreeMap<String, String>,
}

fn path() -> std::path::PathBuf {
    crate::updates::prefs_dir().join("session.json")
}

struct State {
    saved: Option<Session>,
    written: Option<Instant>,
    current: Session,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.get_or_insert_with(|| {
            let saved: Session = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            State { saved: Some(saved.clone()), written: None, current: saved }
        });
        f(st)
    })
}

/// The session saved last (empty the first time).
pub fn load() -> Session {
    with(|s| s.saved.clone().unwrap_or_default())
}

pub fn set_settings_tab(i: usize) {
    with(|s| s.current.settings_tab = Some(i));
}

/// Files' split and column widths (written with the window's state).
/// Written at once (a drag ended), not only with the next window change.
pub fn set_files(split: f32, cols: [[f32; 3]; 2]) {
    with(|s| {
        s.current.files_split = Some(split);
        s.current.files_cols = Some(cols);
        if s.saved.as_ref() == Some(&s.current) {
            return;
        }
        if let Ok(t) = serde_json::to_string_pretty(&s.current) {
            let _ = std::fs::create_dir_all(crate::updates::prefs_dir());
            let _ = std::fs::write(path(), t);
        }
        s.saved = Some(s.current.clone());
        s.written = Some(Instant::now());
    });
}

/// Files: the folder last open on `place` (written at once).
pub fn set_files_dir(place: &str, dir: &str) {
    with(|s| {
        if s.current.files_dirs.get(place).map(String::as_str) == Some(dir) {
            return;
        }
        s.current.files_dirs.insert(place.to_string(), dir.to_string());
        if let Ok(t) = serde_json::to_string_pretty(&s.current) {
            let _ = std::fs::create_dir_all(crate::updates::prefs_dir());
            let _ = std::fs::write(path(), t);
        }
        s.saved = Some(s.current.clone());
        s.written = Some(Instant::now());
    });
}

/// Note the window's state; written when it changed, at most every half a
/// second (true: a write is still due, call again shortly).
pub fn note(instance: Option<String>, page: &str, window: &Window) -> bool {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (Some(b), false),
        WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (Some(b), true),
    };
    with(|s| {
        s.current.instance = instance;
        s.current.page = Some(page.to_string());
        s.current.maximized = maximized;
        // A maximized window keeps the bounds it had before.
        if !maximized {
            if let Some(b) = bounds {
                s.current.bounds = Some([f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]);
            }
        }
        if s.saved.as_ref() == Some(&s.current) {
            return false;
        }
        if s.written.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
            return true;
        }
        if let Ok(t) = serde_json::to_string_pretty(&s.current) {
            let _ = std::fs::create_dir_all(crate::updates::prefs_dir());
            let _ = std::fs::write(path(), t);
        }
        s.saved = Some(s.current.clone());
        s.written = Some(Instant::now());
        false
    })
}

/// The window's bounds to open with: the saved ones when they are on a
/// screen now, clamped to it and to the window's minimum size; else none
/// (the default, centred).
pub fn window_bounds(cx: &App, min: Size<Pixels>) -> Option<WindowBounds> {
    let s = load();
    let [x, y, w, h] = s.bounds?;
    let displays = cx.displays();
    let on = displays.iter().map(|d| d.bounds()).find(|d| {
        let (dx, dy) = (f32::from(d.origin.x), f32::from(d.origin.y));
        let (dw, dh) = (f32::from(d.size.width), f32::from(d.size.height));
        // Its top-left area (where the title bar is) on this screen.
        x + 40. >= dx && x + 40. < dx + dw && y + 10. >= dy && y + 10. < dy + dh
    });
    let size = |d: Option<&Bounds<Pixels>>| {
        let (mw, mh) = (f32::from(min.width), f32::from(min.height));
        let (maxw, maxh) = d.map(|d| (f32::from(d.size.width), f32::from(d.size.height))).unwrap_or((f32::MAX, f32::MAX));
        size(px(w.clamp(mw, maxw.max(mw))), px(h.clamp(mh, maxh.max(mh))))
    };
    let bounds = match on {
        Some(d) => Bounds { origin: point(px(x), px(y)), size: size(Some(&d)) },
        // Off every screen now: the saved size, centred.
        None => Bounds::centered(None, size(displays.first().map(|d| d.bounds()).as_ref()), cx),
    };
    Some(if s.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) })
}
