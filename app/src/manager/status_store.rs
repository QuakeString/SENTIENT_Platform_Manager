// Copyright © 2016-2026 The SENTIENT Authors

//! Every added server's state, read from the start and kept fresh, for the
//! Instances cards and the switcher's dots (crate::remote::status_cache
//! holds it, also for the tray's checks).
//!
//! - At start: every server at once, a little apart, over the connections
//!   crate::remote::conn opens; what was kept on disk shows meanwhile.
//! - Then every [`EVERY`] while a window is open (the tray's own checks
//!   keep it warm while none is), and when the window is focused again.
//! - One request per server at a time; after the computer slept, the
//!   round is spread over its whole length instead of all at once.

use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::remote::status_cache::{self, View};
use crate::remote::Instance;
use crate::ui::Tone;

/// How often every server is read while a window is open.
pub const EVERY: Duration = Duration::from_secs(15);
/// A window focused again reads every server, unless a round started this
/// recently.
const FOCUS_AGAIN: Duration = Duration::from_secs(5);

pub struct StatusStore {
    list: Vec<Instance>,
    /// When the last round started.
    last_round: Option<Instant>,
    _tasks: Vec<Task<()>>,
}

struct Global_(Entity<StatusStore>);
impl Global for Global_ {}

/// The store, made (and its first round started) the first time.
pub fn get(cx: &mut App) -> Entity<StatusStore> {
    if let Some(g) = cx.try_global::<Global_>() {
        return g.0.clone();
    }
    let store = cx.new(StatusStore::new);
    cx.set_global(Global_(store.clone()));
    store
}

impl StatusStore {
    fn new(cx: &mut Context<Self>) -> Self {
        let list = crate::remote::load();
        // What was kept on disk shows from the first frame.
        status_cache::retain(&list.iter().map(|i| i.id.clone()).collect::<Vec<_>>());
        let mut store = Self { list, last_round: None, _tasks: Vec::new() };
        store.round(false, cx);
        let tick = cx.spawn(async move |this, cx| {
            let mut since = Instant::now();
            loop {
                cx.background_executor().timer(EVERY).await;
                let waited = since.elapsed();
                since = Instant::now();
                let alive = this
                    .update(cx, |s, cx| {
                        // No window: the tray's checks read what they need.
                        if cx.windows().is_empty() {
                            return;
                        }
                        // A round started meanwhile (focus): not again so soon.
                        if s.last_round.is_some_and(|t| t.elapsed() < EVERY / 2) {
                            return;
                        }
                        s.round(status_cache::overslept(waited, EVERY), cx);
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        store._tasks.push(tick);
        store
    }

    /// Read every server, a little apart.
    fn round(&mut self, overslept: bool, cx: &mut Context<Self>) {
        self.last_round = Some(Instant::now());
        let offsets = status_cache::offsets(self.list.len(), EVERY, overslept);
        for (inst, after) in self.list.clone().into_iter().zip(offsets) {
            self.read(inst, after, false, cx);
        }
    }

    fn read(&mut self, inst: Instance, after: Duration, again: bool, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            if !after.is_zero() {
                cx.background_executor().timer(after).await;
            }
            let id = inst.id.clone();
            let read = cx.background_executor().spawn(async move { status_cache::refresh(&inst, again) }).await;
            crate::timing::mark(&format!("store-read id={id} read={read}"));
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        // Holds the store weakly: ends with the read.
        .detach();
        cx.notify();
    }

    /// Read one server now: Check again, a job ended there. One under way
    /// is followed by one more (it may have started before the change).
    pub fn refresh(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(inst) = self.list.iter().find(|i| i.id == id).cloned() {
            self.read(inst, Duration::ZERO, true, cx);
        }
    }

    /// The window was focused: every server, unless a round just started.
    pub fn focused(&mut self, cx: &mut Context<Self>) {
        if self.last_round.is_none_or(|t| t.elapsed() >= FOCUS_AGAIN) {
            self.round(false, cx);
        }
    }

    /// The list of servers changed (added, edited, removed): read the new
    /// and changed ones, forget the removed ones.
    pub fn reload_list(&mut self, cx: &mut Context<Self>) {
        let before = std::mem::replace(&mut self.list, crate::remote::load());
        status_cache::retain(&self.list.iter().map(|i| i.id.clone()).collect::<Vec<_>>());
        let changed: Vec<Instance> = self
            .list
            .iter()
            .filter(|i| !before.iter().any(|b| serde_json::to_string(b).ok() == serde_json::to_string(i).ok()))
            .cloned()
            .collect();
        for inst in changed {
            self.read(inst, Duration::ZERO, true, cx);
        }
        cx.notify();
    }

    /// An answer a page got itself (an error using an installed agent).
    pub fn set(&mut self, id: &str, got: Result<crate::remote::RemoteStatus, String>, cx: &mut Context<Self>) {
        status_cache::set(id, &got);
        cx.notify();
    }

    pub fn view(&self, id: &str) -> View {
        status_cache::view(id)
    }

    /// The switcher's dot: the last answer's state at once (also the one
    /// kept from before the start, while it is checked again); red when
    /// the server stopped answering; None before anything is known.
    pub fn tone(&self, id: &str) -> Option<Tone> {
        let v = self.view(id);
        if v.stale.is_some() {
            return Some(Tone::Bad);
        }
        v.status.as_ref().map(|s| super::instances::state_of(Some(s), false).0)
    }
}
