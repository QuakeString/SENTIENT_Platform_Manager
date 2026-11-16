// Copyright © 2016-2026 The SENTIENT Authors

//! Move data to another disk (mockup restore.html, lane M): the Data
//! locations card (Platform settings › Database, M1), and the flow it opens
//! (M4–M12): what to move, a disk, a folder, the plan with SENTIENT's
//! downtime, the run, and its result (the old copy kept until "Remove old
//! copy…"; a failure switched back). The same screens for this computer and
//! a server; the work is crate::movedata's.
//!
//! The flow takes Platform settings' place while it is open, for the
//! instance it was opened on: [`open`] from anywhere (Migration's fourth
//! card, Status's low-disk warning, the restore flow).

use std::collections::HashMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::distro::move_data::{self, BlockKind, Check, Disk, Ended, Estimate, Move, Outcome, Overview, Place, Store};

use crate::backups::DirList;
use crate::jobs::{Job, JobState};
use crate::manager::backup::{card_row, free_label};
use crate::movedata::Target;
use crate::ui::{self, Tone};

// ---------------------------------------------------------------------------
// Opening the flow from anywhere
// ---------------------------------------------------------------------------

/// The open flow and the instance it is for (None: this computer).
struct FlowSlot(Option<(Option<String>, Entity<MoveFlow>)>);
impl Global for FlowSlot {}

/// A page the flow asks the shell to show ("Free space in Files").
static GO: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// The page the flow asked for, once.
pub fn take_go() -> Option<&'static str> {
    GO.lock().ok().and_then(|mut g| g.take())
}

fn go(page: &'static str, cx: &mut App) {
    if let Ok(mut g) = GO.lock() {
        *g = Some(page);
    }
    cx.refresh_windows();
}

/// Open "Move data to another disk" on `target` (`on`: its name, "this
/// computer"), with `store` ticked when one was asked for. Platform settings
/// shows it until it is closed.
pub fn open(target: Target, on: impl Into<String>, store: Option<Store>, window: &mut Window, cx: &mut App) {
    let key = match &target {
        Target::Local => None,
        Target::Remote(i) => Some(i.id.clone()),
    };
    // A move still running there stays.
    if let Some((k, flow)) = cx.try_global::<FlowSlot>().and_then(|s| s.0.clone()) {
        if k == key && flow.read(cx).running(cx) {
            cx.set_global(FlowSlot(Some((k, flow))));
            return;
        }
    }
    let on = on.into();
    let flow = cx.new(|cx| MoveFlow::new(target, on, store, window, cx));
    cx.set_global(FlowSlot(Some((key, flow))));
}

/// The flow open for this instance (None: this computer), if any.
pub fn shown(instance: Option<&String>, cx: &App) -> Option<Entity<MoveFlow>> {
    let (k, f) = cx.try_global::<FlowSlot>()?.0.clone()?;
    (k.as_ref() == instance).then_some(f)
}

/// Whether a flow was opened since `seen` (the shell switches to Platform
/// settings then).
pub fn generation(cx: &App) -> usize {
    cx.try_global::<FlowSlot>().and_then(|s| s.0.as_ref().map(|(_, f)| f.entity_id().as_u64() as usize)).unwrap_or(0)
}

fn close(cx: &mut App) {
    cx.set_global(FlowSlot(None));
    cx.refresh_windows();
}

// ---------------------------------------------------------------------------
// Shared bits
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Read<T> {
    Loading,
    Got(T),
    Failed(String),
}

fn mono_value(theme: &gpui_kit::component::Theme, text: impl Into<SharedString>) -> Div {
    div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme.mono_font_family.clone()).text_size(px(13.)).child(text.into())
}

fn muted(theme: &gpui_kit::component::Theme, text: impl Into<SharedString>) -> Div {
    div().flex_shrink_0().text_size(px(12.5)).text_color(theme.muted_foreground).child(text.into())
}

fn store_label(store: Store, windows: bool) -> &'static str {
    match (store, windows) {
        (Store::Ch, true) => "ClickHouse (WSL)",
        _ => store.label(),
    }
}

/// The folder name a store gets on a new disk.
fn dir_name(store: Store) -> &'static str {
    match store {
        Store::Pg => "postgres",
        Store::Ch => "clickhouse",
        Store::Data => "files",
        Store::Backups => "backups",
    }
}

/// "/mnt/disk-a · 1.4 TB free · 184 GB".
fn place_detail(ov: &Overview, p: &Place) -> String {
    let disk = p.mount.as_ref().and_then(|m| ov.disks.iter().find(|d| &d.mount == m));
    let mut parts = Vec::new();
    if let Some(d) = disk {
        parts.push(d.mount.clone());
        parts.push(format!("{} free", free_label(d.avail)));
    }
    if let Some(b) = p.bytes {
        parts.push(size_label(b));
    }
    parts.join(" · ")
}

fn good_green(theme: &gpui_kit::component::Theme) -> Hsla {
    if theme.background.l < 0.5 { rgb(0x4ade80).into() } else { rgb(0x15803d).into() }
}

/// The mockup's `out` line: an icon and the text on a tinted block.
fn out_line(theme: &gpui_kit::component::Theme, tone: Tone, text: impl Into<SharedString>) -> Div {
    let (bg, fg, icon) = match tone {
        Tone::Good => (theme.success, theme.success_foreground, Lucide::CircleCheck),
        Tone::Warn => (theme.warning, theme.warning_foreground, Lucide::TriangleAlert),
        Tone::Bad => (theme.danger.opacity(0.12), theme.danger, Lucide::CircleX),
        Tone::Neutral => (theme.secondary, theme.secondary_foreground, Lucide::Info),
    };
    h_flex()
        .gap(px(10.))
        .items_start()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(8.))
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .child(Icon::new(icon).size_4().flex_shrink_0().mt(px(2.)))
        .child(div().flex_1().min_w_0().child(text.into()))
}

/// One step of a plan or a run: ✓ done, ● now, ✕ failed, ○ to do, with a
/// muted note at the right ("288 of 612 GB · 260 MB/s · 21 min left").
fn chk(theme: &gpui_kit::component::Theme, mark: ui::StepMark, text: impl Into<SharedString>, sub: Option<String>) -> Div {
    let (sign, color) = match mark {
        ui::StepMark::Done => ("✓", good_green(theme)),
        ui::StepMark::Running => ("●", theme.primary),
        ui::StepMark::Failed => ("✕", theme.danger),
        ui::StepMark::Todo => ("○", theme.muted_foreground),
    };
    h_flex()
        .gap(px(10.))
        .items_center()
        .min_h(px(23.))
        .text_sm()
        .when(mark == ui::StepMark::Todo, |r| r.text_color(theme.muted_foreground))
        .when(mark == ui::StepMark::Failed, |r| r.text_color(theme.danger))
        .child(div().w(px(14.)).flex_shrink_0().flex().justify_center().text_size(px(13.)).text_color(color).child(sign))
        .child(div().flex_1().min_w_0().child(text.into()))
        .when_some(sub, |r, s| r.child(div().flex_shrink_0().text_size(px(13.)).text_color(theme.muted_foreground).font_features(ui::tabular()).child(s)))
}

/// A label / value grid row (the plan's Moves, Stays, …).
fn kv(theme: &gpui_kit::component::Theme, first: bool, label: &'static str, value: impl IntoElement) -> Div {
    h_flex()
        .gap(px(16.))
        .items_center()
        .py(px(8.))
        .text_sm()
        .when(!first, |r| r.border_t_1().border_color(theme.border))
        .child(div().w(px(if ui::narrow() { 120. } else { 186. })).flex_shrink_0().text_color(theme.muted_foreground).child(label))
        .child(div().flex_1().min_w_0().child(value))
}

fn card_title(title: impl Into<SharedString>, right: Option<AnyElement>) -> Div {
    h_flex().gap_3().items_center().child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(title.into())).children(right)
}

// ---------------------------------------------------------------------------
// The Data locations card (M1)
// ---------------------------------------------------------------------------

pub struct LocationsCard {
    target: Target,
    on: String,
    windows: bool,
    ov: Read<Overview>,
    removing: Option<String>,
    _task: Option<Task<()>>,
}

impl LocationsCard {
    pub fn new(target: Target, on: String, windows: bool, cx: &mut Context<Self>) -> Self {
        let windows = windows || (cfg!(debug_assertions) && std::env::var("SPM_DEBUG_MOVEDATA").is_ok_and(|v| v == "win"));
        let mut me = Self { target, on, windows, ov: Read::Loading, removing: None, _task: None };
        me.reload(cx);
        me
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let t = self.target.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { crate::movedata::overview(&t) }).await;
            this.update(cx, |me, cx| {
                me.ov = match r {
                    Ok(o) => Read::Got(o),
                    Err(e) => Read::Failed(crate::remote::split_failure(&e).0),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn remove(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let text = format!("{path} and everything in it are deleted. This cannot be undone. The data in use now is not touched.");
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let (this, path) = (this.clone(), path.clone());
            dialog.title("Remove the old copy?").description(text.clone()).confirm().ok_text("Remove").on_ok(move |_, _, cx| {
                let path = path.clone();
                this.update(cx, |me, cx| me.run_remove(path, cx)).ok();
                true
            })
        });
    }

    fn run_remove(&mut self, path: String, cx: &mut Context<Self>) {
        self.removing = Some(path.clone());
        let t = self.target.clone();
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { remove_old(&t, &path) }).await;
            this.update(cx, |me, cx| {
                me.removing = None;
                if let Err(e) = r {
                    ui::show_error("The old copy was not removed", e, cx);
                }
                me.reload(cx);
            })
            .ok();
        });
        task.detach();
        cx.notify();
    }
}

/// Remove an old copy, here or on the server (a job there: it needs root).
fn remove_old(t: &Target, path: &str) -> Result<(), String> {
    match t {
        Target::Local => crate::movedata::local_remove_old(path, sentient_installer_core::progress::noop()),
        Target::Remote(inst) => {
            let id = crate::remote::start_job(inst, &["move-data-remove-old", path])?;
            crate::remote::follow(inst, &id, &sentient_installer_core::progress::noop())
        }
    }
}

impl Render for LocationsCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let narrow = ui::narrow();
        let mut card = ui::panel(&theme).gap_0().child(card_title("Data locations", None).pb(px(4.)));
        let ov = match &self.ov {
            Read::Loading => {
                for (i, l) in ["PostgreSQL", "ClickHouse", "SENTIENT files", "Backups"].into_iter().enumerate() {
                    card = card.child(card_row(&theme, narrow, l, None, ui::skeleton_line(&theme, format!("dl-sk-{i}"), 260.), None, i == 3));
                }
                return card;
            }
            Read::Failed(e) => return card.child(div().pt_2().text_sm().text_color(theme.danger).child(e.clone())),
            Read::Got(o) => o.clone(),
        };
        if ov.external {
            card = card.child(div().pt_2().pb_1().text_sm().text_color(theme.muted_foreground).child("SENTIENT runs from a compose file the Platform Manager did not write: take it over first (Repair and uninstall) to move its data."));
        }
        let n = ov.places.len() + ov.old_copies.len();
        let mut i = 0;
        for p in &ov.places {
            i += 1;
            let label = store_label(p.store, self.windows);
            let help: Option<&'static str> = match (p.store, self.windows) {
                (Store::Pg, false) => Some("Where each store keeps its data. Each one moves on its own."),
                (Store::Pg, true) => Some("The PostgreSQL service's data folder. Moving it on Windows is not available yet."),
                (Store::Ch, true) => Some("ClickHouse runs in a WSL distribution of the hidden account sentient-clickhouse. Its disk file is moved whole with wsl --manage sentient --move (or export and import on older WSL)."),
                _ => None,
            };
            let value = h_flex().min_w_0().gap_2().items_center().child(mono_value(&theme, p.path.clone())).child(muted(&theme, place_detail(&ov, p)));
            let can = !ov.external && (!self.windows || p.store == Store::Ch);
            let store = p.store;
            let (target, on) = (self.target.clone(), self.on.clone());
            let id = SharedString::from(format!("dl-move-{}", store.as_str()));
            let btn = if store == Store::Backups {
                ui::button(id).outline().small().icon(Icon::new(Lucide::Folder)).label("Change…")
            } else {
                ui::button(id).outline().small().icon(Icon::empty().path("icons/move-data.svg")).label("Move…")
            }
            .disabled(!can)
            .when(!can && self.windows, |b| b.tooltip("Not on Windows yet: only ClickHouse's WSL disk moves there."))
            .on_click(move |_, window, cx| open(target.clone(), on.clone(), Some(store), window, cx));
            card = card.child(card_row(&theme, narrow, label, help, value, Some(btn.into_any_element()), i == n));
        }
        for o in &ov.old_copies {
            i += 1;
            let label: &'static str = if o.partial { "Partial copy" } else { "Old copy" };
            let what = format!("{} · {}{}", o.store.label(), free_label(o.bytes), o.at.get(..10).map(|d| format!(" · since {d}")).unwrap_or_default());
            let value = h_flex().min_w_0().gap_2().items_center().child(mono_value(&theme, o.path.clone())).child(muted(&theme, what));
            let path = o.path.clone();
            let busy = self.removing.as_deref() == Some(o.path.as_str());
            let btn = ui::button(SharedString::from(format!("dl-rm-{i}")))
                .outline()
                .small()
                .icon(Icon::empty().path("icons/trash.svg"))
                .label("Remove…")
                .loading(busy)
                .disabled(self.removing.is_some())
                .on_click(cx.listener(move |me, _, window, cx| me.remove(path.clone(), window, cx)));
            card = card.child(card_row(&theme, narrow, label, None, value, Some(btn.into_any_element()), i == n));
        }
        let _ = window;
        card
    }
}

// ---------------------------------------------------------------------------
// The flow (M4–M12)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    What,
    /// The disk for the n-th chosen store.
    Disk(usize),
    Folder(usize),
    Plan,
    /// Running, or its result.
    Run,
}

/// Where one store goes.
struct Pick {
    mount: Option<String>,
    input: Entity<InputState>,
    listing: Option<Read<DirList>>,
    check: Option<Read<Check>>,
    /// The name of a folder the move makes (New folder).
    new_name: Option<Entity<InputState>>,
}

pub struct MoveFlow {
    target: Target,
    on: String,
    windows: bool,
    ov: Read<Overview>,
    stage: Stage,
    /// The stores to move, in the order chosen.
    chosen: Vec<Store>,
    picks: HashMap<Store, Pick>,
    est: Option<Read<Estimate>>,
    job: Entity<Job>,
    /// The moves the job runs.
    moves: Vec<Move>,
    show_log: bool,
    removing: bool,
    removed: bool,
    seq: u64,
    locations: Option<Entity<LocationsCard>>,
    /// A folder path to put in its field at the next draw.
    pending_path: Option<(Store, String)>,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl MoveFlow {
    fn new(target: Target, on: String, store: Option<Store>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let windows = match &target {
            Target::Local => cfg!(windows),
            Target::Remote(i) => crate::remote::is_windows(i),
        } || (cfg!(debug_assertions) && std::env::var("SPM_DEBUG_MOVEDATA").is_ok_and(|v| v == "win"));
        let job = cx.new(|_| Job::new());
        let subs = vec![cx.observe(&job, |me: &mut Self, job, cx| {
            if !job.read(cx).is_running() {
                if let Some(l) = &me.locations {
                    l.update(cx, |l, cx| l.reload(cx));
                }
            }
            cx.notify();
        })];
        let mut me = Self {
            target,
            on,
            windows,
            ov: Read::Loading,
            stage: Stage::What,
            chosen: store.into_iter().collect(),
            picks: HashMap::new(),
            est: None,
            job,
            moves: Vec::new(),
            show_log: false,
            removing: false,
            removed: false,
            seq: 0,
            locations: None,
            pending_path: None,
            _subs: subs,
            _tasks: Vec::new(),
        };
        let t = me.target.clone();
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { crate::movedata::overview(&t) }).await;
            this.update(cx, |me, cx| {
                me.ov = match r {
                    Ok(o) => Read::Got(o),
                    Err(e) => Read::Failed(crate::remote::split_failure(&e).0),
                };
                #[cfg(debug_assertions)]
                me.debug_frame(cx);
                cx.notify();
            })
            .ok();
        });
        me._tasks.push(task);
        me
    }

    fn running(&self, cx: &App) -> bool {
        self.job.read(cx).is_running()
    }

    fn overview(&self) -> Option<&Overview> {
        match &self.ov {
            Read::Got(o) => Some(o),
            _ => None,
        }
    }

    fn place(&self, s: Store) -> Option<&Place> {
        self.overview()?.places.iter().find(|p| p.store == s)
    }

    fn needed(&self, s: Store) -> u64 {
        self.place(s).and_then(|p| p.bytes).unwrap_or(0)
    }

    /// The stores this installation has, that can move here.
    fn movable(&self) -> Vec<Store> {
        let Some(ov) = self.overview() else { return Vec::new() };
        ov.places.iter().map(|p| p.store).filter(|s| !self.windows || *s == Store::Ch).collect()
    }

    fn pick_mut(&mut self, s: Store, window: &mut Window, cx: &mut Context<Self>) -> &mut Pick {
        if !self.picks.contains_key(&s) {
            let input = cx.new(|cx| InputState::new(window, cx));
            let sub = cx.subscribe(&input, move |me: &mut Self, _, e: &InputEvent, cx| match e {
                InputEvent::Change => me.check_soon(s, cx),
                InputEvent::PressEnter { .. } => me.list_typed(s, cx),
                _ => {}
            });
            self._subs.push(sub);
            self.picks.insert(s, Pick { mount: None, input, listing: None, check: None, new_name: None });
        }
        self.picks.get_mut(&s).expect("inserted")
    }

    // -- disk -------------------------------------------------------------

    /// What a disk says for a store: (selectable, text, tone).
    fn verdict(&self, d: &Disk, s: Store) -> (bool, String, Tone) {
        let Some(ov) = self.overview() else { return (false, String::new(), Tone::Neutral) };
        if self.place(s).and_then(|p| p.mount.as_ref()) == Some(&d.mount) {
            return (false, format!("Yes · {} is here now", s.label()), Tone::Neutral);
        }
        let blocks = move_data::disk_blocks(d, s, self.needed(s), s.label());
        if let Some(b) = blocks.first() {
            let t = match b.kind {
                BlockKind::Network => "Network disk: not for a database".to_string(),
                BlockKind::Fat => "FAT: no owners or permissions, 4 GB file limit".to_string(),
                BlockKind::ReadOnly => "Read-only".to_string(),
                BlockKind::TooSmall => "Too small".to_string(),
                _ => b.title.clone(),
            };
            return (true, t, Tone::Bad);
        }
        let here: Vec<&str> = ov.places.iter().filter(|p| p.store != s && p.store != Store::Backups && p.mount.as_ref() == Some(&d.mount)).map(|p| p.store.label()).collect();
        let t = if here.is_empty() { "Yes".to_string() } else { format!("Yes · {} {} here", here.join(" and "), if here.len() == 1 { "is" } else { "are" }) };
        (true, t, Tone::Good)
    }

    fn choose_disk(&mut self, s: Store, mount: String, window: &mut Window, cx: &mut Context<Self>) {
        let start = if mount == "/" { format!("/var/lib/sentient-data/{}", dir_name(s)) } else if self.windows { format!("{}\\WSL\\sentient", mount.trim_end_matches('\\')) } else { format!("{}/sentient/{}", mount.trim_end_matches('/'), dir_name(s)) };
        let p = self.pick_mut(s, window, cx);
        let changed = p.mount.as_deref() != Some(mount.as_str());
        p.mount = Some(mount);
        if changed {
            p.input.update(cx, |i, cx| i.set_value(start.clone(), window, cx));
            p.listing = None;
            p.check = None;
        }
        cx.notify();
    }

    fn disk_block(&self, s: Store) -> Option<move_data::Block> {
        let ov = self.overview()?;
        let m = self.picks.get(&s)?.mount.clone()?;
        let d = ov.disks.iter().find(|d| d.mount == m)?;
        move_data::disk_blocks(d, s, self.needed(s), s.label()).into_iter().next()
    }

    // -- folder -----------------------------------------------------------

    fn path_of(&self, s: Store, cx: &App) -> String {
        self.picks.get(&s).map(|p| p.input.read(cx).value().trim().to_string()).unwrap_or_default()
    }

    /// List the folder the path is in (or the path, when it is one), and
    /// check the path.
    fn list_typed(&mut self, s: Store, cx: &mut Context<Self>) {
        let path = self.path_of(s, cx);
        self.list(s, path, cx);
    }

    fn list(&mut self, s: Store, path: String, cx: &mut Context<Self>) {
        let t = self.target.clone();
        if let Some(p) = self.picks.get_mut(&s) {
            p.listing = Some(Read::Loading);
        }
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { list_near(&t, &path) }).await;
            this.update(cx, |me, cx| {
                if let Some(p) = me.picks.get_mut(&s) {
                    p.listing = Some(match r {
                        Ok(l) => Read::Got(l),
                        Err(e) => Read::Failed(e),
                    });
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        self.check_soon(s, cx);
    }

    fn check_soon(&mut self, s: Store, cx: &mut Context<Self>) {
        self.seq += 1;
        let seq = self.seq;
        if let Some(p) = self.picks.get_mut(&s) {
            p.check = Some(Read::Loading);
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
            let Ok((t, path)) = this.update(cx, |me, cx| (me.target.clone(), me.path_of(s, cx))) else { return };
            let r = cx.background_executor().spawn(async move { crate::movedata::check(&t, s, &path) }).await;
            this.update(cx, |me, cx| {
                if me.seq != seq {
                    return;
                }
                if let Some(p) = me.picks.get_mut(&s) {
                    p.check = Some(match r {
                        Ok(c) => Read::Got(c),
                        Err(e) => Read::Failed(crate::remote::split_failure(&e).0),
                    });
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn set_path(&mut self, s: Store, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.picks.get(&s) {
            p.input.update(cx, |i, cx| i.set_value(path, window, cx));
        }
        self.check_soon(s, cx);
    }

    fn folder_ok(&self, s: Store) -> bool {
        matches!(self.picks.get(&s).and_then(|p| p.check.as_ref()), Some(Read::Got(c)) if c.ok())
    }

    // -- steps ------------------------------------------------------------

    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage = match self.stage {
            Stage::What => Stage::Disk(0),
            Stage::Disk(i) => {
                let s = self.chosen[i];
                self.list_typed(s, cx);
                Stage::Folder(i)
            }
            Stage::Folder(i) if i + 1 < self.chosen.len() => Stage::Disk(i + 1),
            Stage::Folder(_) => {
                self.estimate(cx);
                Stage::Plan
            }
            Stage::Plan => {
                self.start(cx);
                Stage::Run
            }
            Stage::Run => Stage::Run,
        };
        let _ = window;
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.stage = match self.stage {
            Stage::What => {
                close(cx);
                return;
            }
            Stage::Disk(0) => Stage::What,
            Stage::Disk(i) => Stage::Folder(i - 1),
            Stage::Folder(i) => Stage::Disk(i),
            Stage::Plan => Stage::Folder(self.chosen.len().saturating_sub(1)),
            Stage::Run => Stage::Run,
        };
        cx.notify();
    }

    fn current_moves(&self, cx: &App) -> Vec<Move> {
        self.chosen.iter().map(|s| Move { store: *s, to: self.path_of(*s, cx) }).collect()
    }

    fn estimate(&mut self, cx: &mut Context<Self>) {
        self.est = Some(Read::Loading);
        let (t, moves) = (self.target.clone(), self.current_moves(cx));
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { crate::movedata::estimate(&t, &moves) }).await;
            this.update(cx, |me, cx| {
                me.est = Some(match r {
                    Ok(e) => Read::Got(e),
                    Err(e) => Read::Failed(crate::remote::split_failure(&e).0),
                });
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn label(&self) -> String {
        let names: Vec<&str> = self.chosen.iter().map(|s| s.label()).collect();
        format!("Moving {}", names.join(" and "))
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.moves = self.current_moves(cx);
        self.removed = false;
        self.show_log = false;
        let moves = self.moves.clone();
        let t = self.target.clone();
        let label = match &t {
            Target::Local => self.label(),
            Target::Remote(i) => format!("{} on {}", self.label(), i.name),
        };
        // Its steps from the first frame (crate::jobplan).
        let plan = crate::jobplan::move_data(&moves, self.overview(), self.windows);
        self.job.update(cx, |job, cx| {
            job.set_plan(plan);
            job.set_label(label);
            job.set_on_page(true);
            if let Target::Remote(i) = &t {
                job.set_instance(i.id.clone(), i.name.clone(), i.color.clone());
            }
            job.start(move |sink| run_moves(&t, &moves, sink), cx);
        });
    }

    fn cancel_run(&mut self, cx: &mut Context<Self>) {
        let t = self.target.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = match &t {
                    Target::Local => crate::movedata::local_cancel(),
                    Target::Remote(i) => crate::remote::movedata_cancel(i),
                };
            })
            .detach();
    }

    fn remove_copy(&mut self, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let text = format!("{} and everything in {} deleted. This cannot be undone. The data in use now is not touched.", paths.join(" and "), if paths.len() > 1 { "them are" } else { "it are" });
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let (this, paths) = (this.clone(), paths.clone());
            dialog.title("Remove the copy?").description(text.clone()).confirm().ok_text("Remove").on_ok(move |_, _, cx| {
                let paths = paths.clone();
                this.update(cx, |me, cx| {
                    me.removing = true;
                    let t = me.target.clone();
                    let task = cx.spawn(async move |this, cx| {
                        let r = cx.background_executor().spawn(async move { paths.iter().try_for_each(|p| remove_old(&t, p)) }).await;
                        this.update(cx, |me, cx| {
                            me.removing = false;
                            match r {
                                Ok(()) => me.removed = true,
                                Err(e) => ui::show_error("The copy was not removed", e, cx),
                            }
                            if let Some(l) = &me.locations {
                                l.update(cx, |l, cx| l.reload(cx));
                            }
                            cx.notify();
                        })
                        .ok();
                    });
                    me._tasks.push(task);
                    cx.notify();
                })
                .ok();
                true
            })
        });
    }

    fn try_again(&mut self, cx: &mut Context<Self>) {
        self.job.update(cx, |j, cx| j.reset(cx));
        for p in self.picks.values_mut() {
            p.mount = None;
            p.check = None;
        }
        self.stage = Stage::Disk(0);
        cx.notify();
    }
}

/// List `path`, or its nearest folder that exists.
fn list_near(t: &Target, path: &str) -> Result<DirList, String> {
    #[cfg(debug_assertions)]
    if crate::movedata::debug::on().is_some() {
        let dir = std::path::Path::new(path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        return Ok(DirList { path: dir.clone(), parent: dir.parent().map(|p| p.to_path_buf()), dirs: vec![("archive".into(), 1900 * (1u64 << 30)), ("sentient".into(), 1900 * (1u64 << 30))], writable: Ok(()), fixable: false, free: 1900 * (1u64 << 30), shortcuts: vec![] });
    }
    let mut p = std::path::PathBuf::from(path);
    loop {
        let s = p.display().to_string();
        let r = match t {
            Target::Local => crate::backups::dir_list(&p),
            Target::Remote(inst) => crate::remote::dir_list(inst, &s).map_err(|e| crate::remote::split_failure(&e).0),
        };
        match r {
            Ok(l) => return Ok(l),
            Err(e) => {
                if !p.pop() || p.as_os_str().is_empty() {
                    return Err(e);
                }
            }
        }
    }
}

/// The job: here, or on the server (the agent brought up to date first).
fn run_moves(t: &Target, moves: &[Move], sink: sentient_installer_core::progress::ProgressFn) -> Result<(), String> {
    #[cfg(debug_assertions)]
    if crate::movedata::debug::on().is_some() {
        return debug_run(moves, &sink);
    }
    match t {
        Target::Local => crate::movedata::local_run(moves, sink),
        Target::Remote(inst) => {
            if let Err(e) = crate::remote::deploy(inst, &sink) {
                sink(sentient_installer_core::progress::Progress::Log { line: format!("The agent could not be updated: {}", crate::remote::split_failure(&e).0) });
            }
            let arg = crate::movedata::moves_arg(moves);
            let id = crate::remote::start_job(inst, &["move-data", &arg])?;
            crate::remote::follow(inst, &id, &sink)
        }
    }
}

/// Screens: a scripted move (`SPM_DEBUG_MOVEDATA_RUN=running|done|failed`).
#[cfg(debug_assertions)]
fn debug_run(moves: &[Move], sink: &sentient_installer_core::progress::ProgressFn) -> Result<(), String> {
    use sentient_installer_core::progress::Progress;
    let mode = std::env::var("SPM_DEBUG_MOVEDATA_RUN").unwrap_or_else(|_| match std::env::var("SPM_DEBUG_MOVEDATA_FRAME").as_deref() {
        Ok("M10") => "running".into(),
        Ok("M12") => "failed".into(),
        _ => "done".into(),
    });
    let m = moves.first().cloned().unwrap_or(Move { store: Store::Ch, to: "/mnt/disk-c/sentient/clickhouse".into() });
    let step = |n: String| sink(Progress::Step { name: n });
    let log = |l: &str| sink(Progress::Log { line: l.into() });
    step("Stopping SENTIENT".into());
    step(format!("Copying {} data to {}", m.store.label(), m.to));
    log(&format!("rsync -aHAX --info=progress2 /mnt/disk-b/sentient/clickhouse/ {}/", m.to));
    log("288 GB of 612 GB · 260 MB/s · 21 min left");
    sink(Progress::Percent { value: 0.47 });
    if mode == "running" {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
    step(format!("Verifying {} (sizes and checksums)", m.store.label()));
    let ov = crate::movedata::debug::canned();
    let mut out = Outcome { ended: Ended::Moved, reason: None, failed_step: None, locations: Default::default(), old_copies: ov.old_copies.clone(), backups_dir: None, rows: vec!["ClickHouse holds the same 2\u{2009}812\u{2009}440\u{2009}118 rows as before.".into()], down_secs: 44 * 60 };
    if mode == "failed" {
        out.ended = Ended::SwitchedBack;
        out.failed_step = Some(format!("Verifying {} (sizes and checksums)", m.store.label()));
        out.reason = Some("The copy of ClickHouse did not match: 3 files differ (store/ts_kv/all_1_1_0/data.bin, …). /mnt/disk-c reported I/O errors (dmesg: sdd: Medium Error).".into());
        out.old_copies = vec![move_data::OldCopy { store: m.store, path: m.to.clone(), bytes: 612 * (1u64 << 30), at: "2026-11-16T09:12:00+05:30".into(), partial: true }];
        step("Pointing the volumes back at their old places".into());
        step("Starting SENTIENT on the old places".into());
        log(&format!("{}{}", move_data::OUTCOME_MARK, serde_json::to_string(&out).unwrap_or_default()));
        return Err(out.reason.clone().unwrap_or_default());
    }
    out.old_copies = vec![move_data::OldCopy { store: m.store, path: "/mnt/disk-b/sentient/clickhouse".into(), bytes: 612 * (1u64 << 30), at: "2026-11-16T09:12:00+05:30".into(), partial: false }];
    step("Pointing the st-ch-data volume at the new folder".into());
    step("Starting SENTIENT".into());
    step("Checking SENTIENT answers and the databases hold the same rows".into());
    log(&format!("{}{}", move_data::OUTCOME_MARK, serde_json::to_string(&out).unwrap_or_default()));
    sink(Progress::Done { message: "Moved ClickHouse.".into() });
    Ok(())
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

impl MoveFlow {
    fn title_row(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap_3()
            .items_start()
            .child(
                ui::button("md-back-title")
                    .outline()
                    .icon(Icon::new(Lucide::ChevronLeft))
                    .tooltip("Back")
                    .on_click(cx.listener(|me, _, _, cx| me.back(cx))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_size(px(22.)).line_height(px(30.)).child("Move data to another disk"))
                    .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(self.on.clone())),
            )
    }

    fn stepper(&self, theme: &gpui_kit::component::Theme) -> Div {
        let n = match self.stage {
            Stage::What => 0,
            Stage::Disk(_) => 1,
            Stage::Folder(_) => 2,
            _ => 3,
        };
        let mut row = h_flex().gap(px(6.)).items_center().text_size(px(13.)).text_color(theme.muted_foreground).whitespace_nowrap();
        for (i, s) in ["What", "Disk", "Folder", "Plan"].into_iter().enumerate() {
            if i > 0 {
                row = row.child(div().w(px(18.)).h(px(1.)).bg(theme.border));
            }
            let (done, on) = (i < n, i == n);
            let dot = div()
                .size(px(20.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .border_1()
                .when(done, |d| d.bg(theme.primary.opacity(0.14)).border_color(transparent_black()).text_color(theme.primary))
                .when(on, |d| d.bg(theme.primary).border_color(theme.primary).text_color(theme.primary_foreground))
                .when(!done && !on, |d| d.border_color(theme.input))
                .child(if done { "✓".to_string() } else { (i + 1).to_string() });
            row = row.child(h_flex().gap(px(6.)).items_center().when(on, |r| r.text_color(theme.foreground).font_weight(FontWeight::MEDIUM)).child(dot).child(s));
        }
        row
    }

    fn footer(&self, primary: Option<AnyElement>, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap(crate::ui::button_gap())
            .items_center()
            .mt_auto()
            .child(crate::ui::probed(ui::button("md-back").outline().label("Back").on_click(cx.listener(|me, _, _, cx| me.back(cx)))))
            .child(div().flex_1())
            .child(crate::ui::probed(ui::button("md-cancel").ghost().label("Cancel").on_click(|_, _, cx| close(cx))))
            .children(primary.map(ui::probed))
    }

    fn what(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let mut card = ui::panel(theme).child(card_title("What to move", None));
        let Some(ov) = self.overview() else { return card };
        let movable = self.movable();
        for p in &ov.places {
            let s = p.store;
            let on = self.chosen.contains(&s);
            let can = movable.contains(&s);
            let where_ = match (s, p.bind) {
                (Store::Backups, _) => format!("{} in {} (moves without stopping SENTIENT)", p.bytes.map(free_label).unwrap_or_default(), p.path),
                (_, false) => format!("{} in Docker's folder on {}", p.bytes.map(free_label).unwrap_or_default(), p.mount.clone().unwrap_or_else(|| "/".into())),
                _ => format!("{} on {}", p.bytes.map(free_label).unwrap_or_default(), p.mount.clone().unwrap_or_else(|| p.path.clone())),
            };
            card = card.child(
                h_flex()
                    .gap(px(10.))
                    .items_center()
                    .min_h(px(34.))
                    .child(
                        Checkbox::new(SharedString::from(format!("md-what-{}", s.as_str())))
                            .checked(on)
                            .disabled(!can)
                            .on_change(cx.listener(move |me, v: &bool, _, cx| {
                                me.chosen.retain(|x| *x != s);
                                if *v {
                                    me.chosen.push(s);
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().w(px(150.)).flex_shrink_0().text_sm().child(store_label(s, self.windows)))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(13.)).text_color(theme.muted_foreground).child(where_)),
            );
        }
        card
    }

    fn disks(&self, theme: &gpui_kit::component::Theme, i: usize, cx: &mut Context<Self>) -> Div {
        let s = self.chosen[i];
        let narrow = ui::narrow();
        let Some(ov) = self.overview() else { return div() };
        let picked = self.picks.get(&s).and_then(|p| p.mount.clone());
        let head = |t: &'static str| div().text_size(px(12.5)).text_color(theme.muted_foreground).child(t);
        let cols = |r: Stateful<Div>, radio: AnyElement, disk: AnyElement, mount: AnyElement, ty: AnyElement, size: AnyElement, free: AnyElement, verdict: AnyElement| {
            r.child(div().w(px(28.)).flex_shrink_0().child(radio))
                .when(!narrow, |r| r.child(div().w(px(150.)).flex_shrink_0().child(disk)))
                .child(div().w(px(if narrow { 110. } else { 150. })).flex_shrink_0().min_w_0().overflow_hidden().child(mount))
                .when(!narrow, |r| r.child(div().w(px(64.)).flex_shrink_0().child(ty)))
                .when(!narrow, |r| r.child(h_flex().w(px(76.)).flex_shrink_0().justify_end().child(size)))
                .child(h_flex().w(px(76.)).flex_shrink_0().justify_end().child(free))
                .child(div().flex_1().min_w_0().pl(px(20.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(verdict))
        };
        let mut table = v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden().child(cols(
            h_flex().id("md-disk-head").h(px(32.)).px(px(10.)).items_center().bg(theme.muted.opacity(0.5)),
            div().into_any_element(),
            head("Disk").into_any_element(),
            head("Mounted at").into_any_element(),
            head("Type").into_any_element(),
            head("Size").into_any_element(),
            head("Free").into_any_element(),
            div().text_size(px(12.5)).text_color(theme.muted_foreground).child(format!("Can hold {} ({})", s.label(), size_label(self.needed(s)))).into_any_element(),
        ));
        for (n, d) in ov.disks.iter().enumerate() {
            let (selectable, text, tone) = self.verdict(d, s);
            let on = picked.as_deref() == Some(d.mount.as_str());
            let color = match tone {
                Tone::Good => good_green(theme),
                Tone::Bad => theme.danger,
                _ => theme.muted_foreground,
            };
            let radio = div()
                .size(px(16.))
                .rounded_full()
                .flex_shrink_0()
                .when(on, |r| r.border(px(5.)).border_color(theme.primary))
                .when(!on, |r| r.border_2().border_color(theme.muted_foreground).when(tone != Tone::Good, |r| r.opacity(0.4)));
            let mount = d.mount.clone();
            let row = h_flex()
                .id(SharedString::from(format!("md-disk-{n}")))
                .min_h(px(34.))
                .px(px(10.))
                .items_center()
                .text_size(px(13.5))
                .border_t_1()
                .border_color(theme.border)
                .when(selectable, |r| r.cursor_pointer().hover(|r| r.bg(theme.list_hover)))
                .when(selectable, |r| r.on_click(cx.listener(move |me, _, window, cx| me.choose_disk(s, mount.clone(), window, cx))));
            table = table.child(cols(
                row,
                radio.into_any_element(),
                h_flex().gap(px(6.)).items_center().min_w_0().child(Icon::new(Lucide::HardDrive).size_4().text_color(theme.muted_foreground)).child(mono_value(theme, d.source.clone())).into_any_element(),
                mono_value(theme, d.mount.clone()).into_any_element(),
                div().child(d.fstype.clone()).into_any_element(),
                div().font_features(ui::tabular()).child(free_label(d.size)).into_any_element(),
                div().font_features(ui::tabular()).child(free_label(d.avail)).into_any_element(),
                div().text_size(px(13.)).text_color(color).child(text).into_any_element(),
            ));
        }
        let title = if self.windows { format!("Disk for {}", store_label(s, true)) } else { format!("Disk for {}", s.label()) };
        let mut card = ui::panel(theme).child(card_title(title, None)).child(table);
        match self.disk_block(s) {
            Some(b) => card = card.child(self.block(theme, &b, cx)),
            None => card = card.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("Read from the server (lsblk, df). A network disk or FAT cannot hold a database; each row says why.")),
        }
        card
    }

    /// A block: why, the steps that fix it, and the buttons that help.
    fn block(&self, theme: &gpui_kit::component::Theme, b: &move_data::Block, cx: &mut Context<Self>) -> Div {
        let too_small = b.kind == BlockKind::TooSmall;
        let stage = self.stage;
        v_flex()
            .gap(px(10.))
            .px(px(16.))
            .py(px(14.))
            .rounded(px(12.))
            .border_1()
            .border_color(theme.danger.opacity(0.45))
            .bg(theme.background)
            .child(h_flex().gap(px(10.)).items_center().text_size(px(15.)).font_weight(FontWeight::MEDIUM).child(Icon::new(Lucide::CircleX).size(px(18.)).text_color(theme.danger)).child(b.title.clone()))
            .when(!b.why.is_empty(), |c| c.child(div().text_sm().child(b.why.clone())))
            .when(!b.fixes.is_empty(), |c| {
                c.child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child("To fix it"))
                    .child(v_flex().gap(px(6.)).pl(px(4.)).text_sm().children(b.fixes.iter().enumerate().map(|(i, f)| h_flex().gap_2().items_start().child(div().flex_shrink_0().text_color(theme.muted_foreground).child(format!("{}.", i + 1))).child(div().flex_1().min_w_0().child(f.clone())))))
            })
            .child(
                h_flex()
                    .gap(crate::ui::button_gap())
                    .flex_wrap()
                    .child(crate::ui::probed(ui::button("md-pick-another").primary().label(if matches!(stage, Stage::Folder(_)) { "Pick another folder" } else { "Pick another disk" }).on_click(cx.listener(move |me, _, _, cx| {
                        if let Stage::Disk(i) = me.stage {
                            let s = me.chosen[i];
                            if let Some(p) = me.picks.get_mut(&s) {
                                p.mount = None;
                            }
                        }
                        cx.notify();
                    }))))
                    .when(too_small, |r| r.child(crate::ui::probed(ui::button("md-free-space").outline().icon(Icon::new(Lucide::Folder)).label("Free space in Files").on_click(|_, _, cx| go("files", cx))))),
            )
    }

    fn folder(&self, theme: &gpui_kit::component::Theme, i: usize, cx: &mut Context<Self>) -> Div {
        let s = self.chosen[i];
        let Some(p) = self.picks.get(&s) else { return div() };
        let mount = p.mount.clone().unwrap_or_default();
        let path = p.input.read(cx).value().to_string();
        let up = std::path::Path::new(&path).parent().map(|x| x.display().to_string()).filter(|x| !x.is_empty());
        let top = h_flex()
            .gap_2()
            .items_center()
            .child(ui::button("md-up").outline().icon(Icon::new(Lucide::ChevronLeft)).tooltip("Up one folder").disabled(up.is_none()).on_click(cx.listener(move |me, _, window, cx| {
                if let Some(u) = up.clone() {
                    me.set_path(s, u.clone(), window, cx);
                    me.list(s, u, cx);
                }
            })))
            .child(div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).child(Input::new(&p.input)))
            .child(ui::button("md-new").outline().icon(Icon::empty().path("icons/folder-plus.svg")).label("New folder").on_click(cx.listener(move |me, _, window, cx| {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
                let sub = cx.subscribe(&input, move |me: &mut Self, input, e: &InputEvent, cx| {
                    if let InputEvent::PressEnter { .. } = e {
                        let name = input.read(cx).value().trim().to_string();
                        me.new_folder(s, name, cx);
                    }
                });
                input.update(cx, |i, cx| i.focus(window, cx));
                me._subs.push(sub);
                if let Some(p) = me.picks.get_mut(&s) {
                    p.new_name = Some(input);
                }
                cx.notify();
            })));
        let header = h_flex()
            .h(px(30.))
            .px(px(10.))
            .items_center()
            .bg(theme.muted.opacity(0.5))
            .text_size(px(12.5))
            .text_color(theme.muted_foreground)
            .child(div().flex_1().child("Name"))
            .child(h_flex().w(px(120.)).justify_end().child("Free space"));
        let mut rows = v_flex().id("md-rows").max_h(px(5. * 34.)).overflow_y_scroll();
        if let Some(input) = &p.new_name {
            rows = rows.child(
                h_flex()
                    .h(px(40.))
                    .px(px(10.))
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Icon::empty().path("icons/folder-fill.svg").size_4().text_color(folder_color()))
                    .child(div().flex_1().child(Input::new(input).small()))
                    .child(ui::button("md-make").primary().small().label("Use").on_click(cx.listener(move |me, _, _, cx| {
                        let name = me.picks.get(&s).and_then(|p| p.new_name.as_ref()).map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
                        me.new_folder(s, name, cx);
                    }))),
            );
        }
        match &p.listing {
            None | Some(Read::Loading) => {
                for k in 0..2 {
                    rows = rows.child(h_flex().h(px(34.)).px(px(10.)).gap_3().items_center().border_t_1().border_color(theme.border).child(ui::skeleton_line(theme, format!("md-fsk-{k}"), 160.)).child(div().flex_1()).child(ui::skeleton_line(theme, format!("md-fsk2-{k}"), 50.)));
                }
            }
            Some(Read::Failed(e)) => rows = rows.child(div().px(px(10.)).py_2().border_t_1().border_color(theme.border).text_sm().text_color(theme.danger).child(e.clone())),
            Some(Read::Got(l)) => {
                if l.dirs.is_empty() {
                    rows = rows.child(div().px(px(10.)).py_2().border_t_1().border_color(theme.border).text_sm().text_color(theme.muted_foreground).child("No folders here."));
                }
                for (k, (name, free)) in l.dirs.iter().enumerate() {
                    let full = l.path.join(name).display().to_string();
                    let on = full == path || path.starts_with(&format!("{full}/"));
                    let f2 = full.clone();
                    rows = rows.child(
                        h_flex()
                            .id(("md-dir", k))
                            .h(px(34.))
                            .px(px(10.))
                            .gap(px(8.))
                            .items_center()
                            .flex_shrink_0()
                            .text_sm()
                            .border_t_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .when(on, |r| r.bg(theme.sidebar_accent))
                            .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
                            .child(Icon::empty().path("icons/folder-fill.svg").size_4().text_color(folder_color()))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name.clone()))
                            .child(h_flex().w(px(120.)).justify_end().text_color(theme.muted_foreground).font_features(ui::tabular()).child(free_label(*free)))
                            .on_click(cx.listener(move |me, e: &ClickEvent, window, cx| {
                                // One click picks a folder for the store in
                                // it; two open it.
                                if e.click_count() >= 2 {
                                    me.set_path(s, f2.clone(), window, cx);
                                    me.list(s, f2.clone(), cx);
                                } else {
                                    me.set_path(s, format!("{}/{}", f2.trim_end_matches('/'), dir_name(s)), window, cx);
                                }
                            })),
                    );
                }
            }
        }
        let table = v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden().child(header).child(rows);
        let status: AnyElement = match &p.check {
            None | Some(Read::Loading) => h_flex().gap_2().items_center().text_sm().text_color(theme.muted_foreground).child("Checking the folder…").into_any_element(),
            Some(Read::Failed(e)) => out_line(theme, Tone::Bad, e.clone()).into_any_element(),
            Some(Read::Got(c)) if c.ok() => {
                let mut parts = vec![if c.exists { "Empty".to_string() } else { "A new folder, made by the move".to_string() }, format!("writable by {}", s.label())];
                if let Some(d) = &c.disk {
                    parts.push(d.fstype.clone());
                    parts.push(format!("{} free, {} needed", free_label(d.avail), free_label(c.needed)));
                }
                out_line(theme, Tone::Good, parts.join(" · ")).into_any_element()
            }
            Some(Read::Got(c)) => self.block(theme, &c.blocks[0], cx).into_any_element(),
        };
        ui::panel(theme).child(card_title(format!("Folder on {mount}"), None)).child(top).child(table).child(status)
    }

    fn new_folder(&mut self, s: Store, name: String, cx: &mut Context<Self>) {
        if name.is_empty() || name.contains(['/', '\\', '\'', '"', ',']) {
            return;
        }
        let Some(p) = self.picks.get_mut(&s) else { return };
        let base = match &p.listing {
            Some(Read::Got(l)) => l.path.display().to_string(),
            _ => return,
        };
        p.new_name = None;
        let path = format!("{}/{}", base.trim_end_matches('/'), name);
        // The input is set at the next draw (it needs the window).
        self.pending_path = Some((s, path));
        cx.notify();
    }

    fn plan(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let Some(ov) = self.overview() else { return div() };
        let moves = self.current_moves(cx);
        let est = self.est.clone();
        let (down, rate) = match &est {
            Some(Read::Got(e)) => (Some(e.down_secs), Some(e.rate_mb_s)),
            _ => (None, None),
        };
        let stops = moves.iter().any(|m| m.store.stops());
        let down_el = |theme: &gpui_kit::component::Theme| -> AnyElement {
            match (&est, down) {
                (Some(Read::Failed(e)), _) => div().text_color(theme.danger).child(e.clone()).into_any_element(),
                (_, Some(_)) if !stops => div().child("SENTIENT keeps running").into_any_element(),
                (_, Some(secs)) => h_flex()
                    .gap_x_2()
                    .flex_wrap()
                    .items_center()
                    .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(move_data::about(secs)))
                    .when(moves.len() == 1, |r| {
                        r.child(div().min_w_0().text_size(px(13.)).text_color(theme.muted_foreground).child(format!("{} at about {:.0} MB/s, measured on these disks", size_label(self.needed(moves[0].store)), rate.unwrap_or(0.))))
                    })
                    .into_any_element(),
                _ => ui::skeleton_line(theme, "md-est", 180.).into_any_element(),
            }
        };
        if moves.len() == 1 {
            let m = &moves[0];
            let place = self.place(m.store);
            let from = place.and_then(|p| p.mount.clone()).unwrap_or_default();
            let stays: Vec<String> = ov
                .places
                .iter()
                .filter(|p| p.store != m.store)
                .map(|p| match (p.store, p.bind) {
                    (Store::Pg, true) if self.windows => format!("PostgreSQL in {}", p.path),
                    (Store::Pg, true) | (Store::Ch, true) => format!("{} on {}", p.store.label(), p.mount.clone().unwrap_or_default()),
                    (s, _) => s.label().to_string().replace("Backups", "backups"),
                })
                .collect();
            let named = place.is_some_and(|p| !p.bind);
            let old = if self.windows {
                format!("None: WSL moves the disk file itself, so it leaves {from}")
            } else if named {
                format!("Kept in Docker's folder on {from} until you remove it")
            } else {
                format!("Kept on {from} until you remove it")
            };
            let moves_text = if self.windows && m.store == Store::Ch {
                format!("The WSL disk ext4.vhdx of the account sentient-clickhouse, {} (the whole file, not only the data)", size_label(self.needed(m.store)))
            } else {
                format!("{}, {}, from {from}", store_label(m.store, self.windows), size_label(self.needed(m.store)))
            };
            let vol = place.map(|p| p.volume.clone()).unwrap_or_default();
            let steps = plan_steps(&moves, ov, self.windows);
            let other_db = [Store::Pg, Store::Ch].into_iter().find(|s| *s != m.store && m.store.is_database() && self.movable().contains(s));
            let mut card = ui::panel(theme)
                .child(card_title(format!("Move {} to {}", store_label(m.store, self.windows), m.to), None))
                .child(
                    v_flex()
                        .child(kv(theme, true, "Moves", div().child(moves_text)))
                        .child(kv(theme, false, "Stays", div().child(stays.join(", "))))
                        .child(kv(theme, false, "SENTIENT is down", down_el(theme)))
                        .child(kv(theme, false, "The old copy", div().child(old))),
                )
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child("Steps"))
                .child(v_flex().gap(px(10.)).children(steps.iter().map(|(t, _)| chk(theme, ui::StepMark::Todo, t.clone(), None))));
            let _ = vol;
            if let Some(o) = other_db {
                card = card.child(
                    h_flex()
                        .gap(px(10.))
                        .items_center()
                        .child(Checkbox::new("md-also").label(format!("Also move {} in the same stop", o.label())).checked(false).on_change(cx.listener(move |me, v: &bool, _, cx| {
                            if *v && !me.chosen.contains(&o) {
                                me.chosen.push(o);
                                me.stage = Stage::Disk(me.chosen.len() - 1);
                                cx.notify();
                            }
                        })))
                        .child(ui::info("md-also-help".into(), "Both moves share one stop of SENTIENT; the downtime adds up.".into())),
                );
            }
            return card;
        }
        // Several: a table and the total.
        let est_of = |s: Store| -> String {
            match &est {
                Some(Read::Got(e)) => e.moves.iter().find(|x| x.0 == s).map(|x| format!("{} min", x.2.div_ceil(60).max(1))).unwrap_or_default(),
                _ => "…".into(),
            }
        };
        let head = |t: &'static str| div().text_size(px(12.5)).text_color(theme.muted_foreground).child(t);
        let row = |r: Div, a: AnyElement, b: AnyElement, c: AnyElement, d: AnyElement, e: AnyElement| {
            r.child(div().w(px(30.)).flex_shrink_0().child(a))
                .child(div().w(px(130.)).flex_shrink_0().child(b))
                .child(div().flex_1().min_w_0().overflow_hidden().child(c))
                .child(h_flex().w(px(80.)).flex_shrink_0().justify_end().child(d))
                .child(h_flex().w(px(96.)).flex_shrink_0().justify_end().child(e))
        };
        let mut table = v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden().text_size(px(13.5)).child(row(
            h_flex().h(px(32.)).px(px(10.)).items_center().bg(theme.muted.opacity(0.5)),
            head("#").into_any_element(),
            head("Store").into_any_element(),
            head("To").into_any_element(),
            head("Size").into_any_element(),
            head("Copy takes").into_any_element(),
        ));
        for (k, m) in moves.iter().enumerate() {
            table = table.child(row(
                h_flex().min_h(px(34.)).px(px(10.)).items_center().border_t_1().border_color(theme.border),
                div().child((k + 1).to_string()).into_any_element(),
                div().child(store_label(m.store, self.windows)).into_any_element(),
                mono_value(theme, m.to.clone()).into_any_element(),
                div().font_features(ui::tabular()).child(size_label(self.needed(m.store))).into_any_element(),
                div().font_features(ui::tabular()).child(est_of(m.store)).into_any_element(),
            ));
        }
        let n = moves.len();
        ui::panel(theme)
            .child(card_title(format!("{} moves in one stop", if n == 2 { "Two".to_string() } else { n.to_string() }), None))
            .child(table)
            .child(
                v_flex()
                    .child(kv(theme, true, "SENTIENT is down", down_el(theme)))
                    .child(kv(theme, false, "If one fails", div().child(if n == 2 { "Both go back to where they were" } else { "All go back to where they were" }))),
            )
    }

    /// M10–M12: the run, then its result, in the shared job view
    /// (crate::manager::jobview).
    fn run_view(&self, theme: &gpui_kit::component::Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let job = self.job.read(cx);
        let state = job.state().clone();
        let log: Vec<SharedString> = job.log().iter().filter(|l| l.starts_with(move_data::OUTCOME_MARK)).cloned().collect();
        let outcome = crate::movedata::outcome_in(&log);
        let Some(ov) = self.overview().cloned() else { return div() };
        let names: Vec<&str> = self.moves.iter().map(|m| store_label(m.store, self.windows)).collect();
        let what = names.join(" and ");
        let first = self.moves.first().cloned();
        // Where it went: the disk, or the folder when the disk is / (it says
        // nothing then).
        let to_mount = |m: &Move| ov.disks.iter().filter(|d| m.to.starts_with(&d.mount)).max_by_key(|d| d.mount.len()).map(|d| d.mount.clone()).filter(|d| d != "/").unwrap_or_else(|| m.to.clone());
        let title = match (&state, &first, self.moves.len()) {
            (JobState::Done(_), Some(m), 1) if m.store == Store::Backups => format!("Backups are in {}", m.to),
            (JobState::Done(_), Some(m), 1) => format!("{} runs from {}", what, to_mount(m)),
            (JobState::Done(_), _, _) => format!("{what} moved"),
            (JobState::Failed(_), _, n) => format!("{what} {} not moved", if n == 1 { "was" } else { "were" }),
            (_, Some(m), 1) => format!("Moving {} to {}", what, to_mount(m)),
            _ => format!("Moving {what}"),
        };
        let me = cx.entity().downgrade();
        let mut view = crate::manager::jobview::JobView::new("md-run", title, &self.job)
            .hide_lines(|l| l.starts_with(move_data::OUTCOME_MARK))
            .cancel(crate::manager::jobview::Cancel::Can(std::rc::Rc::new(move |_, cx| {
                me.update(cx, |p, cx| p.cancel_run(cx)).ok();
            })));
        match &state {
            JobState::Done(_) => {
                let mut text = String::from("SENTIENT answers");
                if let Some(o) = &outcome {
                    if !o.rows.is_empty() {
                        text = format!("SENTIENT answers, and {}", o.rows.iter().map(|r| lower_first(r)).collect::<Vec<_>>().join(" "));
                    } else {
                        text.push('.');
                    }
                    if o.down_secs > 0 {
                        text = format!("{} Down for {} min.", text.trim_end(), o.down_secs.div_ceil(60));
                    }
                }
                view = view.result(out_line(theme, Tone::Good, text));
                // This run's old copies (the newest of each moved store);
                // earlier ones stay listed under Data locations.
                let mine: Vec<move_data::OldCopy> = outcome.as_ref().map(|o| o.old_copies.iter().filter(|c| !c.partial && self.moves.iter().any(|m| m.store == c.store)).cloned().collect()).unwrap_or_default();
                let newest = mine.iter().map(|c| c.at.clone()).max().unwrap_or_default();
                let old: Vec<move_data::OldCopy> = mine.into_iter().filter(|c| c.at == newest).collect();
                let mut acts: Vec<AnyElement> = Vec::new();
                if let Some(c) = old.first().filter(|_| !self.removed) {
                    let paths: Vec<String> = old.iter().map(|c| c.path.clone()).collect();
                    let at = ov.disks.iter().filter(|d| c.path.starts_with(&d.mount)).max_by_key(|d| d.mount.len()).map(|d| d.mount.clone()).unwrap_or_else(|| c.path.clone());
                    view = view.extra(div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!(
                        "{} {} {at}. Remove {} once SENTIENT has run well for a while.",
                        size_label(old.iter().map(|c| c.bytes).sum()),
                        if c.path.contains("/spm-moved/") { "in Docker's folder on" } else { "on" },
                        if old.len() > 1 { "them" } else { "it" }
                    )));
                    acts.push(ui::button("md-rm-old").outline().icon(Icon::empty().path("icons/trash.svg")).label(if old.len() > 1 { "Remove old copies…" } else { "Remove old copy…" }).loading(self.removing).on_click(cx.listener(move |me, _, window, cx| me.remove_copy(paths.clone(), window, cx))).into_any_element());
                }
                acts.push(ui::button("md-done").primary().label("Done").on_click(|_, _, cx| close(cx)).into_any_element());
                view = view.actions(acts);
            }
            JobState::Failed(msg) => {
                let broken = outcome.as_ref().is_some_and(|o| o.ended == Ended::Broken);
                view = if broken { view.pill("Needs a look", Tone::Bad) } else { view.pill("Switched back", Tone::Warn) };
                let mut text = outcome.as_ref().and_then(|o| o.reason.clone()).unwrap_or_else(|| msg.to_string());
                if !broken {
                    let olds: Vec<String> = self.moves.iter().filter_map(|m| ov.places.iter().find(|p| p.store == m.store)).map(|p| format!("{} runs from {} as before", p.store.label(), p.mount.clone().unwrap_or_else(|| p.path.clone()))).collect();
                    if !olds.is_empty() {
                        text = format!("{} {}", text.trim_end(), olds.join("; ") + ";");
                    }
                    let partial = outcome.as_ref().is_some_and(|o| o.old_copies.iter().any(|c| c.partial));
                    if partial {
                        text.push_str(" the partial copy is left for a look.");
                    } else {
                        text = text.trim_end_matches(';').to_string() + ".";
                    }
                }
                view = view.result(out_line(theme, Tone::Bad, text));
                let partial: Option<String> = outcome.as_ref().and_then(|o| o.old_copies.iter().find(|c| c.partial && self.moves.iter().any(|m| m.to.trim_end_matches('/') == c.path.trim_end_matches('/')))).map(|c| c.path.clone());
                let mut acts: Vec<AnyElement> = Vec::new();
                if let Some(p) = partial.filter(|_| !self.removed) {
                    acts.push(ui::button("md-rm-partial").outline().icon(Icon::empty().path("icons/trash.svg")).label("Remove the partial copy…").loading(self.removing).on_click(cx.listener(move |me, _, window, cx| me.remove_copy(vec![p.clone()], window, cx))).into_any_element());
                }
                acts.push(ui::button("md-try-other").primary().label("Try another disk").on_click(cx.listener(|me, _, _, cx| me.try_again(cx))).into_any_element());
                view = view.actions(acts);
            }
            _ => {}
        }
        let done = matches!(state, JobState::Done(_));
        let _ = window;
        view.render(cx).when(done, |c| c.children(self.locations.clone()))
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if s.starts_with("PostgreSQL") || s.starts_with("ClickHouse") => f.to_string() + c.as_str(),
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A size as the mockup writes it, small ones in KB (free_label says 0 B).
fn size_label(b: u64) -> String {
    if b > 0 && b < 1 << 20 { format!("{} KB", b.div_ceil(1024)) } else { free_label(b) }
}

fn folder_color() -> Hsla {
    rgb(0xd9a23a).into()
}

/// The plan's steps as shown, each with the start of the engine step that
/// stands for it.
fn plan_steps(moves: &[Move], ov: &Overview, windows: bool) -> Vec<(String, String)> {
    crate::jobplan::move_steps(moves, Some(ov), windows)
}

impl Render for MoveFlow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if let Some((s, p)) = self.pending_path.take() {
            self.set_path(s, p.clone(), window, cx);
            self.list(s, p, cx);
        }
        if self.locations.is_none() {
            let (t, on, w) = (self.target.clone(), self.on.clone(), self.windows);
            self.locations = Some(cx.new(|cx| LocationsCard::new(t, on, w, cx)));
        }
        let page = v_flex().id("page").flex_1().min_w_0().h_full().overflow_y_scroll().p(px(ui::PAGE_PAD)).gap(px(ui::GAP)).bg(theme.muted);
        if self.stage == Stage::Run {
            let title = v_flex()
                .gap_0p5()
                .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Platform settings"))
                .child(div().text_sm().text_color(theme.muted_foreground).child("Ports, addresses and data of SENTIENT on this server."));
            return page.child(title).child(self.run_view(&theme, window, cx));
        }
        let page = page.child(self.title_row(&theme, cx)).child(self.stepper(&theme));
        let body: Div = match &self.ov {
            Read::Loading => ui::panel(&theme).child(card_title("What to move", None)).children((0..3).map(|i| ui::skeleton_line(&theme, format!("md-sk-{i}"), 320.))),
            Read::Failed(e) => ui::panel(&theme).child(out_line(&theme, Tone::Bad, e.clone())),
            Read::Got(_) => match self.stage {
                Stage::What => self.what(&theme, cx),
                Stage::Disk(i) => self.disks(&theme, i, cx),
                Stage::Folder(i) => self.folder(&theme, i, cx),
                Stage::Plan => self.plan(&theme, cx),
                Stage::Run => div(),
            },
        };
        let can_next = match self.stage {
            Stage::What => !self.chosen.is_empty() && matches!(self.ov, Read::Got(_)) && self.overview().is_some_and(|o| !o.external),
            Stage::Disk(i) => self.picks.get(&self.chosen[i]).is_some_and(|p| p.mount.is_some()) && self.disk_block(self.chosen[i]).is_none(),
            Stage::Folder(i) => self.folder_ok(self.chosen[i]),
            Stage::Plan => matches!(self.est, Some(Read::Got(_))),
            Stage::Run => false,
        };
        let primary_label: String = match self.stage {
            Stage::Plan => {
                let stops = self.chosen.iter().any(|s| s.stops());
                match (stops, self.chosen.len()) {
                    (false, _) => "Move".into(),
                    (true, 1) => "Stop SENTIENT and move".into(),
                    (true, 2) => "Stop SENTIENT and move both".into(),
                    _ => "Stop SENTIENT and move them".into(),
                }
            }
            _ => "Next".into(),
        };
        let primary = ui::button("md-next").primary().label(primary_label).disabled(!can_next).on_click(cx.listener(|me, _, window, cx| me.next(window, cx)));
        page.child(body).child(div().flex_1()).child(self.footer(Some(primary.into_any_element()), cx))
    }
}

// ---------------------------------------------------------------------------
// Screens (debug builds): open the flow at a frame of the mockup
// ---------------------------------------------------------------------------

#[cfg(debug_assertions)]
impl MoveFlow {
    /// `SPM_DEBUG_MOVEDATA_FRAME=M4|M5|M6|M7b|M7c|M8|M9|M10|M11|M12|M13b`.
    fn debug_frame(&mut self, cx: &mut Context<Self>) {
        let Ok(f) = std::env::var("SPM_DEBUG_MOVEDATA_FRAME") else { return };
        let Some(window) = cx.windows().first().copied() else { return };
        let me = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                me.update(cx, |me, cx| me.debug_frame_in(&f, window, cx)).ok();
            });
        });
    }

    fn debug_frame_in(&mut self, f: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A live run on the instance (click tests): the store and the folder
        // from the environment, started at once.
        if let (Some(store), Ok(to)) = (std::env::var("SPM_DEBUG_MOVEDATA_STORE").ok().and_then(|s| Store::parse(&s)), std::env::var("SPM_DEBUG_MOVEDATA_TO")) {
            self.chosen = vec![store];
            let mount = self.overview().and_then(|o| move_data::disk_of(&o.disks, &to).map(|d| d.mount.clone())).unwrap_or_else(|| "/".into());
            self.choose_disk(store, mount, window, cx);
            self.set_path(store, to, window, cx);
            if f == "plan" {
                self.estimate(cx);
                self.stage = Stage::Plan;
            } else {
                self.stage = Stage::Run;
                self.start(cx);
            }
            cx.notify();
            return;
        }
        let ch = Store::Ch;
        let to_c = |me: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            me.chosen = vec![ch];
            me.choose_disk(ch, "/mnt/disk-c".into(), window, cx);
        };
        match f {
            "M4" => self.chosen = vec![ch],
            "M5" => {
                to_c(self, window, cx);
                self.stage = Stage::Disk(0);
            }
            "M7b" => {
                self.chosen = vec![ch];
                self.pick_mut(ch, window, cx).mount = Some("/media/usb".into());
                self.stage = Stage::Disk(0);
            }
            "M7c" => {
                self.chosen = vec![ch];
                self.pick_mut(ch, window, cx).mount = Some("/".into());
                self.stage = Stage::Disk(0);
            }
            "M6" => {
                to_c(self, window, cx);
                self.list_typed(ch, cx);
                self.stage = Stage::Folder(0);
            }
            "M8" | "M13b" => {
                if f == "M13b" {
                    self.chosen = vec![ch];
                    self.choose_disk(ch, "D:".into(), window, cx);
                } else {
                    to_c(self, window, cx);
                }
                self.estimate(cx);
                self.stage = Stage::Plan;
            }
            "M9" => {
                to_c(self, window, cx);
                self.chosen.push(Store::Pg);
                self.choose_disk(Store::Pg, "/mnt/disk-a".into(), window, cx);
                self.estimate(cx);
                self.stage = Stage::Plan;
            }
            "M10" | "M11" | "M12" => {
                to_c(self, window, cx);
                self.stage = Stage::Run;
                self.start(cx);
            }
            _ => {}
        }
        cx.notify();
    }
}
