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

static WINDOW_OPENED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Which page a new window opens on, given the page left last: after a
/// real quit (the first window of this run of the Platform Manager) it is
/// Status; a window closed and opened again while the Platform Manager
/// kept running in the background opens where it was left (his ask). The
/// instance is restored either way.
pub fn page_to_restore(saved: Option<&str>) -> Option<String> {
    let reopened = WINDOW_OPENED.swap(true, std::sync::atomic::Ordering::SeqCst);
    restore_page(saved, reopened)
}

fn restore_page(saved: Option<&str>, reopened: bool) -> Option<String> {
    if reopened { saved.map(str::to_string) } else { None }
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

/// A window of `size` (at least `min`) fitted into a screen's work area
/// `work` (x, y, width, height: the visible area, on Windows the screen
/// without the taskbar), all in logical pixels at that screen's scale.
/// Never larger than the work area (at 150 % a 1920×1080 screen is
/// 1280×693 logical without the taskbar: the 720 px default ran under the
/// taskbar, rc093-1). At `at` when it fits there whole and was not made
/// smaller; else centred. Returns x, y, width, height.
pub fn fit_to_work_area(at: Option<(f32, f32)>, size: (f32, f32), min: (f32, f32), work: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let (wx, wy, ww, wh) = work;
    // The minimum size gives way to the work area: a window that runs
    // under the taskbar is worse than a smaller one.
    let w = size.0.max(min.0).min(ww).max(1.);
    let h = size.1.max(min.1).min(wh).max(1.);
    let shrunk = w < size.0 || h < size.1;
    match at {
        Some((x, y)) if !shrunk => (x.min(wx + ww - w).max(wx), y.min(wy + wh - h).max(wy), w, h),
        _ => (wx + (ww - w) / 2., wy + (wh - h) / 2., w, h),
    }
}

fn work_area(d: &dyn PlatformDisplay) -> (f32, f32, f32, f32) {
    let b = d.visible_bounds();
    (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height))
}

fn to_bounds((x, y, w, h): (f32, f32, f32, f32)) -> Bounds<Pixels> {
    Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
}

/// A new window of `want` (at least `min`), centred in the main screen's
/// work area and no larger than it.
pub fn centred_bounds(cx: &App, want: Size<Pixels>, min: Size<Pixels>) -> WindowBounds {
    let (want, min) = ((f32::from(want.width), f32::from(want.height)), (f32::from(min.width), f32::from(min.height)));
    let work = cx.primary_display().or_else(|| cx.displays().into_iter().next()).map(|d| work_area(&*d));
    WindowBounds::Windowed(match work {
        Some(work) => to_bounds(fit_to_work_area(None, want, min, work)),
        None => to_bounds((0., 0., want.0.max(min.0), want.1.max(min.1))),
    })
}

/// The window's bounds to open with: the saved ones when they are on a
/// screen now, fitted into its work area (fit_to_work_area); else the saved
/// size centred on the main screen; none when nothing was saved (the
/// default, centred).
pub fn window_bounds(cx: &App, min: Size<Pixels>) -> Option<WindowBounds> {
    let s = load();
    let [x, y, w, h] = s.bounds?;
    let displays = cx.displays();
    let on = displays.iter().find(|d| {
        let b = d.bounds();
        let (dx, dy) = (f32::from(b.origin.x), f32::from(b.origin.y));
        let (dw, dh) = (f32::from(b.size.width), f32::from(b.size.height));
        // Its top-left area (where the title bar is) on this screen.
        x + 40. >= dx && x + 40. < dx + dw && y + 10. >= dy && y + 10. < dy + dh
    });
    let min = (f32::from(min.width), f32::from(min.height));
    let bounds = match on {
        // Kept on that screen whole: after a scaling change (100 % → 150 %)
        // the saved place could leave it running off the right or bottom,
        // or under the taskbar.
        Some(d) => to_bounds(fit_to_work_area(Some((x, y)), (w, h), min, work_area(&**d))),
        // Off every screen now: the saved size, centred.
        None => match centred_bounds(cx, size(px(w), px(h)), size(px(min.0), px(min.1))) {
            WindowBounds::Windowed(b) | WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => b,
        },
    };
    Some(if s.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) })
}

#[cfg(test)]
mod fit_tests {
    use super::fit_to_work_area;

    /// A 1920×1080 screen with a 40 px (physical) taskbar at the bottom, as
    /// GPUI gives it at `scale`: logical pixels.
    fn work(scale: f32) -> (f32, f32, f32, f32) {
        (0., 0., 1920. / scale, (1080. - 40.) / scale)
    }

    fn inside(b: (f32, f32, f32, f32), w: (f32, f32, f32, f32)) -> bool {
        b.0 >= w.0 && b.1 >= w.1 && b.0 + b.2 <= w.0 + w.2 + 0.01 && b.1 + b.3 <= w.1 + w.3 + 0.01
    }

    const MIN: (f32, f32) = (560., 480.);

    #[test]
    fn the_default_size_fits_above_the_taskbar_at_every_scale() {
        for scale in [1., 1.5, 2.] {
            let wa = work(scale);
            let b = fit_to_work_area(None, (1100., 720.), MIN, wa);
            assert!(inside(b, wa), "scale {scale}: {b:?} outside {wa:?}");
            // Centred.
            assert!((b.0 - (wa.2 - b.2) / 2.).abs() < 0.01 && (b.1 - (wa.3 - b.3) / 2.).abs() < 0.01, "scale {scale}: {b:?}");
        }
        // At 1x it is the size asked for; at 150 % (1280×693) only its height gives.
        assert_eq!(fit_to_work_area(None, (1100., 720.), MIN, work(1.)), ((1920. - 1100.) / 2., (1040. - 720.) / 2., 1100., 720.));
        let b = fit_to_work_area(None, (1100., 720.), MIN, work(1.5));
        assert_eq!((b.2, b.3), (1100., 1040. / 1.5));
    }

    #[test]
    fn a_saved_window_stays_inside_the_work_area_at_every_scale() {
        for scale in [1., 1.5, 2.] {
            let wa = work(scale);
            // Left at 100 % across the whole screen, or partly off it.
            for (at, size) in [((0., 0.), (1920., 1080.)), ((900., 500.), (1100., 720.)), ((-50., -20.), (700., 500.)), ((0., 0.), (1100., 720.))] {
                let b = fit_to_work_area(Some(at), size, MIN, wa);
                assert!(inside(b, wa), "scale {scale}, saved {at:?} {size:?}: {b:?} outside {wa:?}");
            }
        }
    }

    #[test]
    fn a_saved_window_that_fits_keeps_its_place() {
        assert_eq!(fit_to_work_area(Some((100., 50.)), (800., 600.), MIN, work(1.)), (100., 50., 800., 600.));
        // Moved in, not resized, when it only runs off the edge.
        assert_eq!(fit_to_work_area(Some((1500., 700.)), (800., 600.), MIN, work(1.)), (1120., 440., 800., 600.));
    }

    #[test]
    fn a_saved_window_made_smaller_is_centred() {
        let wa = work(1.5);
        let b = fit_to_work_area(Some((0., 0.)), (1100., 720.), MIN, wa);
        assert_eq!(b, ((1280. - 1100.) / 2., 0., 1100., 1040. / 1.5));
    }

    #[test]
    fn the_minimum_size_gives_way_to_a_small_work_area() {
        let wa = (0., 0., 500., 400.);
        assert!(inside(fit_to_work_area(None, (1100., 720.), MIN, wa), wa));
    }

    #[test]
    fn a_work_area_off_the_origin() {
        // A second screen to the left, its taskbar at the top.
        let wa = (-1280., 27., 1280., 693.);
        let b = fit_to_work_area(Some((-1300., 0.)), (600., 500.), MIN, wa);
        assert_eq!(b, (-1280., 27., 600., 500.));
    }
}

#[cfg(test)]
mod restore_tests {
    use super::restore_page;

    #[test]
    fn after_a_quit_it_starts_on_status() {
        assert_eq!(restore_page(Some("update"), false), None);
        assert_eq!(restore_page(None, false), None);
    }

    #[test]
    fn a_window_opened_again_keeps_its_page() {
        assert_eq!(restore_page(Some("update"), true).as_deref(), Some("update"));
        assert_eq!(restore_page(None, true), None);
    }
}
