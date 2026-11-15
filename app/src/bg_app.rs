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

struct Bg {
    prefs: Prefs,
    tray: Option<crate::tray::Tray>,
    tx: UnboundedSender<Command>,
    manager: Option<WeakEntity<Manager>>,
    window: Option<AnyWindowHandle>,
    options: fn(&mut App) -> WindowOptions,
}

impl Global for Bg {}

/// The background part runs (the tray is up and closing does not quit).
fn keeps_running(cx: &App) -> bool {
    cx.try_global::<Bg>().is_some_and(|b| b.tray.is_some() && !b.prefs.close_quits)
}

/// Start the manager: the background part when the tray is on, and the
/// window unless started with `--background`.
pub fn start(options: fn(&mut App) -> WindowOptions, open_window: bool, request: String, cx: &mut App) {
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

    cx.spawn(async move |cx| {
        while let Some(cmd) = rx.next().await {
            cx.update(|cx| handle(cmd, cx));
        }
    })
    .detach();

    if open_window || !running {
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
            let model = cx.background_executor().spawn(async { crate::tray::Model::read(local_state()) }).await;
            let alive = cx.update(|cx| match cx.try_global::<Bg>().and_then(|b| b.tray.as_ref()) {
                Some(t) => {
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
        cx.background_executor().timer(Duration::from_secs(60)).await;
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
                for (inst, seen) in found {
                    let key = inst.as_ref().map(|i| i.0.clone());
                    let before = last.insert(key, seen);
                    let Some(before) = before.filter(|b| *b != seen) else { continue };
                    let name = inst.as_ref().map(|i| i.1.clone()).unwrap_or_else(|| "This computer".into());
                    let (failed, title, detail) = match seen {
                        Seen::Stopped => (true, "SENTIENT stopped".to_string(), format!("SENTIENT on {name} does not answer on its web port.")),
                        Seen::NoAnswer => (true, "The server does not answer".to_string(), format!("{name} could not be reached over SSH.")),
                        Seen::Running if before == Seen::NoAnswer => (false, "The server answers again".to_string(), format!("{name} is reachable again; SENTIENT runs.")),
                        Seen::Running => (false, "SENTIENT runs again".to_string(), format!("SENTIENT on {name} answers again.")),
                    };
                    if cx.global::<Bg>().window.is_none() {
                        desktop_notification(&format!("{name}: {title}"), &detail);
                    }
                    crate::manager::strip::push_condition(inst, failed, title, detail, cx);
                }
                true
            });
            if !alive {
                break;
            }
            cx.background_executor().timer(CHECK_EVERY).await;
        }
    })
    .detach();
}

fn desktop_notification(summary: &str, body: &str) {
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
