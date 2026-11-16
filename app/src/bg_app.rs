// Copyright © 2016-2026 The SENTIENT Authors

//! The Platform Manager window and its background part in the app
//! (crate::background, crate::tray).
//!
//! With the tray on, the app keeps running when the window closes: the tray
//! opens it again, and the instances are checked every few minutes (a
//! server that stops answering, SENTIENT stopping or running again) into
//! the bell, with a desktop notification while no window is open. Closing
//! the window while a job runs minimizes it instead, so the job's progress
//! is not lost (GPUI cannot hide a window). With the tray off, or "Closing
//! the window quits", closing quits as before.

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::*;

use crate::background::{Command, Prefs};
use crate::manager::Manager;

/// How often the menu's notifications, instances and state are read again.
const TRAY_EVERY: Duration = Duration::from_secs(15);
/// How often the instances are checked for the bell.
const CHECK_EVERY: Duration = Duration::from_secs(300);
/// …and while one of them has a problem, to see it end soon.
const PROBLEM_CHECK_EVERY: Duration = Duration::from_secs(60);

struct Bg {
    prefs: Prefs,
    tray: Option<crate::tray::Tray>,
    tx: UnboundedSender<Command>,
    manager: Option<WeakEntity<Manager>>,
    window: Option<AnyWindowHandle>,
    options: fn(&mut App) -> WindowOptions,
}

impl Global for Bg {}

/// What the background checks see now, per instance (None: this computer):
/// name, colour, and the problem while there is one. The bell and the tray
/// list problems as conditions (not clearable; gone when it runs again).
#[derive(Default)]
struct Health(HashMap<Option<String>, (String, String, Seen)>);

impl Global for Health {}

/// Whether SENTIENT runs on that instance (None: this computer) as the
/// last check found; None when not checked or the server did not answer.
pub fn serving(inst: Option<&str>, cx: &App) -> Option<bool> {
    let h = cx.try_global::<Health>()?;
    match h.0.get(&inst.map(str::to_string))?.2 {
        Seen::Running => Some(true),
        Seen::Stopped => Some(false),
        Seen::NoAnswer => None,
    }
}

/// A problem the checks found: instance, its name and colour, and whether
/// the server does not answer (else SENTIENT is stopped).
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub instance: Option<String>,
    pub name: String,
    pub color: String,
    pub no_answer: bool,
}

impl Problem {
    pub fn title(&self) -> &'static str {
        if self.no_answer { "The server does not answer" } else { "SENTIENT stopped" }
    }

    pub fn detail(&self) -> String {
        let on = if self.instance.is_none() { "this computer".to_string() } else { self.name.clone() };
        if self.no_answer { format!("{} could not be reached over SSH.", self.name) } else { format!("SENTIENT on {on} does not answer on its web port.") }
    }
}

/// The problems now, this computer first.
pub fn problems(cx: &App) -> Vec<Problem> {
    let Some(h) = cx.try_global::<Health>() else { return Vec::new() };
    let mut v: Vec<Problem> = h
        .0
        .iter()
        .filter(|(_, (_, _, s))| *s != Seen::Running)
        .map(|(id, (name, color, s))| Problem { instance: id.clone(), name: name.clone(), color: color.clone(), no_answer: *s == Seen::NoAnswer })
        .collect();
    v.sort_by(|a, b| a.instance.is_some().cmp(&b.instance.is_some()).then(a.name.cmp(&b.name)));
    v
}

/// The background part runs (the tray is up and closing does not quit).
fn keeps_running(cx: &App) -> bool {
    cx.try_global::<Bg>().is_some_and(|b| b.tray.is_some() && !b.prefs.close_quits)
}

/// Start the manager: the background part when the tray is on, and the
/// window unless started with `--background`.
pub fn start(options: fn(&mut App) -> WindowOptions, open_window: bool, request: String, cx: &mut App) {
    // GPUI quits by itself when the last window closes (Linux, Windows);
    // here closing the window keeps the tray, so quitting is ours alone
    // (on_window_closed below, Exit).
    cx.set_quit_mode(QuitMode::Explicit);
    let prefs = Prefs::load();
    let (tx, mut rx) = unbounded::<Command>();
    let tray = if prefs.show_tray {
        crate::background::listen(tx.clone());
        crate::tray::spawn(tx.clone(), crate::tray::Model::read(None))
    } else {
        None
    };
    let running = tray.is_some();
    cx.set_global(Bg { prefs, tray, tx, manager: None, window: None, options });
    // The bell's strip lives as long as the app (jobs survive a closed window).
    crate::manager::strip::install(cx);
    // Screens: problems as the checks would find them.
    #[cfg(debug_assertions)]
    if std::env::var_os("SPM_DEBUG_PROBLEMS").is_some() {
        let mut h = HashMap::new();
        h.insert(Some("plantb".to_string()), ("Plant B server".to_string(), "#6b4fa0".to_string(), Seen::NoAnswer));
        h.insert(None, ("This computer".to_string(), crate::remote::LOCAL_COLOR.to_string(), Seen::Stopped));
        cx.set_global(Health(h));
    }

    cx.spawn(async move |cx| {
        while let Some(cmd) = rx.next().await {
            cx.update(|cx| handle(cmd, cx));
        }
    })
    .detach();

    // Start at sign-in as set: the entry is written (or removed) on every
    // start, so a default ON, an update, or a removed file never leaves the
    // switch ON without it (rc12-19: ON by default, no entry).
    {
        let on = cx.global::<Bg>().prefs.start_at_signin && cx.global::<Bg>().prefs.show_tray;
        std::thread::spawn(move || {
            if let Err(e) = crate::background::set_autostart(on) {
                crate::maintenance_ops::log_line(&format!("Start at sign-in could not be set: {e}"));
                eprintln!("Start at sign-in could not be set: {e}");
            }
        });
    }
    // Started at sign-in before the desktop's tray is up: wait for it
    // (every 3 s for 2 minutes) without a window; only then fall back to
    // the window.
    let wait_for_tray = !open_window && !running && cx.global::<Bg>().prefs.show_tray;
    if wait_for_tray {
        crate::maintenance_ops::log_line("Started in the background; no tray yet, waiting for the desktop's");
        let tx = cx.global::<Bg>().tx.clone();
        cx.spawn(async move |cx| {
            for _ in 0..40 {
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                let up = cx.update(|cx| {
                    if cx.global::<Bg>().tray.is_some() {
                        return true;
                    }
                    let Some(tray) = crate::tray::spawn(tx.clone(), crate::tray::Model::read(None)) else { return false };
                    cx.global_mut::<Bg>().tray = Some(tray);
                    crate::maintenance_ops::log_line("The tray is up");
                    refresh_tray(cx);
                    check_instances(cx);
                    true
                });
                if up {
                    return;
                }
            }
            cx.update(|cx| {
                crate::maintenance_ops::log_line("No tray after 2 minutes: opening the window");
                show(cx);
            });
        })
        .detach();
    } else if open_window || !running {
        show(cx);
        if let Some(a) = request.strip_prefix("action ") {
            act(a, cx);
        }
    }
    if running {
        refresh_tray(cx);
        check_instances(cx);
    }
    cx.on_window_closed(|cx, _| {
        if let Some(b) = cx.try_global::<Bg>() {
            if b.window.is_some_and(|w| !cx.windows().iter().any(|x| x.window_id() == w.window_id())) {
                cx.global_mut::<Bg>().window = None;
                cx.global_mut::<Bg>().manager = None;
            }
        }
        if cx.windows().is_empty() && !keeps_running(cx) {
            crate::maintenance_ops::log_line("The window closed and there is no tray: quitting");
            crate::background::release();
            cx.quit();
        }
    })
    .detach();
}

fn handle(cmd: Command, cx: &mut App) {
    match cmd {
        Command::Show => show(cx),
        Command::Toggle => {
            if let Some(w) = cx.global::<Bg>().window {
                // Open: close it (the tray stays); a running job keeps it, minimized.
                let _ = w.update(cx, |_, window, cx| {
                    if crate::manager::strip::busy(cx) {
                        window.minimize_window();
                    } else {
                        window.remove_window();
                    }
                });
            } else {
                show(cx);
            }
        }
        Command::Action(a) => {
            show(cx);
            act(&a, cx);
        }
        Command::Exit => {
            crate::background::release();
            cx.quit();
        }
    }
}

/// Open the window, or bring it to the front.
fn show(cx: &mut App) {
    if let Some(w) = cx.global::<Bg>().window {
        if w.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let options = (cx.global::<Bg>().options)(cx);
    let Ok((handle, manager)) = crate::open_manager_window(options, cx) else { return };
    let b = cx.global_mut::<Bg>();
    b.window = Some(handle);
    b.manager = Some(manager.downgrade());
    // Closing: quits when asked to; with a job running only minimizes.
    let _ = handle.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, |window, cx| {
            if cx.try_global::<Bg>().is_some_and(|b| b.prefs.close_quits || b.tray.is_none()) {
                return true;
            }
            if crate::manager::strip::busy(cx) {
                window.minimize_window();
                return false;
            }
            true
        });
    });
}

fn act(action: &str, cx: &mut App) {
    let Some(m) = cx.global::<Bg>().manager.as_ref().and_then(|m| m.upgrade()) else { return };
    let action = action.to_string();
    m.update(cx, |m, cx| m.act(&action, cx));
}

/// The preferences changed (About): save, start or stop at sign-in, and
/// put the tray up or take it down now.
pub fn set_prefs(prefs: Prefs, cx: &mut App) -> Result<(), String> {
    prefs.save()?;
    crate::background::set_autostart(prefs.start_at_signin && prefs.show_tray)?;
    let Some(b) = cx.try_global::<Bg>() else { return Ok(()) };
    let had = b.tray.is_some();
    let tx = b.tx.clone();
    if prefs.show_tray && !had {
        crate::background::listen(tx.clone());
        let tray = crate::tray::spawn(tx, crate::tray::Model::read(None));
        let up = tray.is_some();
        cx.global_mut::<Bg>().tray = tray;
        if up {
            refresh_tray(cx);
            check_instances(cx);
        }
    } else if !prefs.show_tray && had {
        cx.global_mut::<Bg>().tray = None;
        crate::background::release();
    }
    cx.global_mut::<Bg>().prefs = prefs;
    Ok(())
}

pub fn prefs(cx: &App) -> Prefs {
    cx.try_global::<Bg>().map(|b| b.prefs.clone()).unwrap_or_else(Prefs::load)
}

/// The tray is up (About shows when the desktop has none).
pub fn tray_up(cx: &App) -> bool {
    cx.try_global::<Bg>().is_some_and(|b| b.tray.is_some())
}

/// This computer's SENTIENT for the menu: None when not installed.
fn local_state() -> Option<bool> {
    let record = crate::installation::InstallationRecord::load()?;
    Some(record.platform().snapshot().serving)
}

fn refresh_tray(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            let mut model = cx.background_executor().spawn(async { crate::tray::Model::read(local_state()) }).await;
            let alive = cx.update(|cx| match cx.try_global::<Bg>().and_then(|b| b.tray.as_ref()) {
                Some(t) => {
                    model.conditions = problems(cx).into_iter().map(|p| (p.instance.clone().unwrap_or_default(), format!("{}: {}", p.name, p.title()))).collect();
                    t.update(model);
                    true
                }
                None => false,
            });
            if !alive {
                break;
            }
            cx.background_executor().timer(TRAY_EVERY).await;
        }
    })
    .detach();
}

/// What a check found for one instance.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Seen {
    Running,
    Stopped,
    NoAnswer,
}

/// Check every instance now and then; a change goes to the bell (and the
/// desktop's notifications while no window is open). The first reading is
/// only remembered.
fn check_instances(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut last: HashMap<Option<String>, Seen> = HashMap::new();
        #[allow(unused_mut)]
        let mut first = Duration::from_secs(60);
        #[cfg(debug_assertions)]
        if let Some(s) = std::env::var("SPM_DEBUG_CHECK_EVERY").ok().and_then(|v| v.parse().ok()) {
            first = Duration::from_secs(s);
        }
        cx.background_executor().timer(first).await;
        loop {
            let found = cx
                .background_executor()
                .spawn(async {
                    let mut v: Vec<(Option<(String, String, String)>, Seen)> = Vec::new();
                    if let Some(running) = local_state() {
                        v.push((None, if running { Seen::Running } else { Seen::Stopped }));
                    }
                    for inst in crate::remote::load() {
                        let seen = match crate::remote::status(&inst) {
                            Ok(s) if !s.installed => continue,
                            Ok(s) if s.serving => Seen::Running,
                            Ok(_) => Seen::Stopped,
                            Err(e) if e == crate::remote::NOT_SET_UP || e == crate::remote::NOT_ADDED => continue,
                            Err(_) => Seen::NoAnswer,
                        };
                        v.push((Some((inst.id.clone(), inst.name.clone(), inst.color.clone())), seen));
                    }
                    v
                })
                .await;
            let alive = cx.update(|cx| {
                if !keeps_running(cx) {
                    return false;
                }
                let mut health = HashMap::new();
                for (inst, seen) in found {
                    let key = inst.as_ref().map(|i| i.0.clone());
                    let name = inst.as_ref().map(|i| i.1.clone()).unwrap_or_else(|| "This computer".into());
                    let color = inst.as_ref().map(|i| i.2.clone()).unwrap_or_else(|| crate::remote::LOCAL_COLOR.into());
                    health.insert(key.clone(), (name.clone(), color, seen));
                    let before = last.insert(key, seen);
                    // Once when a problem starts, once when it ends: plain
                    // lines in the bell (clearing one does not bring it back);
                    // the live state is the tray's and the pages'.
                    if let Some(note) = transition(before, seen, &name) {
                        if note.failed && cx.global::<Bg>().window.is_none() {
                            desktop_notification(&format!("{name}: {}", note.title), &note.detail);
                        }
                        crate::manager::strip::push_condition(inst, note.failed, note.title, note.detail, cx);
                    }
                }
                cx.set_global(Health(health));
                cx.refresh_windows();
                true
            });
            if !alive {
                break;
            }
            // Every minute while something is wrong, else every five; at once
            // when a job ended (request_check).
            let mut wait = if last.values().any(|s| *s != Seen::Running) { PROBLEM_CHECK_EVERY } else { CHECK_EVERY };
            // Screens of real checker events: a short interval.
            #[cfg(debug_assertions)]
            if let Some(s) = std::env::var("SPM_DEBUG_CHECK_EVERY").ok().and_then(|v| v.parse().ok()) {
                wait = Duration::from_secs(s);
            }
            let mut waited = Duration::ZERO;
            while waited < wait && !RECHECK.swap(false, std::sync::atomic::Ordering::Relaxed) {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                waited += Duration::from_secs(2);
            }
        }
    })
    .detach();
}

/// Check the instances again now (a job ended: a migration, an update, a
/// takeover may have changed what runs where).
pub fn request_check() {
    RECHECK.store(true, std::sync::atomic::Ordering::Relaxed);
}

static RECHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the bell says when an instance's state changes: nothing for the
/// first reading or no change.
#[derive(Debug, PartialEq)]
struct Note {
    failed: bool,
    title: String,
    detail: String,
}

fn transition(before: Option<Seen>, now: Seen, name: &str) -> Option<Note> {
    let before = before.filter(|b| *b != now)?;
    let on = if name == "This computer" { "this computer".to_string() } else { name.to_string() };
    Some(match now {
        Seen::Stopped => Note { failed: true, title: "SENTIENT stopped".into(), detail: format!("SENTIENT on {on} does not answer on its web port.") },
        Seen::NoAnswer => Note { failed: true, title: "The server does not answer".into(), detail: format!("{name} could not be reached over SSH.") },
        Seen::Running if before == Seen::NoAnswer => Note { failed: false, title: "The server answers again".into(), detail: format!("{name} is reachable again; SENTIENT runs.") },
        Seen::Running => Note { failed: false, title: "SENTIENT answers again".into(), detail: format!("SENTIENT on {on} answers again.") },
    })
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{Seen, transition};

    /// A problem is raised once and cleared once; a first reading or the
    /// same state again says nothing.
    #[test]
    fn a_problem_is_raised_and_cleared() {
        assert_eq!(transition(None, Seen::Stopped, "NORTHWIND"), None);
        let raised = transition(Some(Seen::Running), Seen::Stopped, "NORTHWIND").unwrap();
        assert!(raised.failed && raised.title == "SENTIENT stopped");
        assert_eq!(transition(Some(Seen::Stopped), Seen::Stopped, "NORTHWIND"), None);
        let cleared = transition(Some(Seen::Stopped), Seen::Running, "NORTHWIND").unwrap();
        assert!(!cleared.failed && cleared.title == "SENTIENT answers again");
        assert_eq!(transition(Some(Seen::NoAnswer), Seen::Running, "NORTHWIND").unwrap().title, "The server answers again");
        assert!(transition(Some(Seen::Running), Seen::Stopped, "This computer").unwrap().detail.contains("on this computer"));
    }
}

pub(crate) fn desktop_notification(summary: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = notify_rust::Notification::new().appname("SENTIENT Platform Manager").icon("sentient-platform-manager").summary(summary).body(body).show();
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows: the tray's tooltip carries the count; toasts follow.
        let _ = (summary, body);
    }
}
