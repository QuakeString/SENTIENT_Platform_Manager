// Copyright © 2016-2026 The SENTIENT Authors

//! Copies between the Files page's sides, in the background (C3): one queue
//! for the whole window, followed by the strip under the top bar and its
//! chip, whose panel lists every copy with Cancel. A file goes to
//! `<name>.spm-part` first: a copy that broke resumes from what arrived
//! (also a later copy of the same file), and the part becomes the file
//! only when its size is the source's. A name already there asks (C5):
//! Replace, Keep both or Skip, for the rest of that copy too if ticked.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable as _, button::ButtonVariants as _, checkbox::Checkbox, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::progress::{Progress, ProgressFn};

use super::fs::{self, Fs, Place};
/// Sizes as the Files mockup writes them: "38.2 MB", "412 KB", "2 KB".
pub fn size_label(b: u64) -> String {
    const K: f64 = 1024.0;
    let f = b as f64;
    let (v, unit) = if f >= K * K * K * K { (f / (K * K * K * K), "TB") } else if f >= K * K * K { (f / (K * K * K), "GB") } else if f >= K * K { (f / (K * K), "MB") } else if f >= K { (f / K, "KB") } else { return format!("{b} B") };
    if v >= 100. || unit == "KB" { format!("{v:.0} {unit}") } else { format!("{v:.1} {unit}") }
}

use size_label as bytes_label;
use crate::jobs::Job;

#[derive(Debug, Clone, PartialEq)]
pub enum St {
    Waiting,
    Copying,
    /// When it finished (seconds since 1970).
    Done(i64),
    Failed(String),
    Cancelled,
    Skipped,
}

pub struct Item {
    pub id: u64,
    pub batch: u64,
    pub name: String,
    pub from: Place,
    pub from_admin: bool,
    pub src: String,
    pub to: Place,
    pub to_admin: bool,
    pub dest_dir: String,
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub state: Mutex<St>,
    pub cancel: AtomicBool,
    started: Mutex<Option<Instant>>,
}

impl Item {
    pub fn st(&self) -> St {
        self.state.lock().map(|s| s.clone()).unwrap_or(St::Waiting)
    }
    fn set(&self, st: St) {
        if let Ok(mut s) = self.state.lock() {
            *s = st;
        }
    }
    /// Bytes a second since it started.
    pub fn rate(&self) -> Option<f64> {
        let t = (*self.started.lock().ok()?)?.elapsed().as_secs_f64();
        (t > 0.5).then(|| self.done.load(Ordering::Relaxed) as f64 / t)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Choice {
    Replace,
    KeepBoth,
    Skip,
}

/// C5: a name already there.
pub struct Ask {
    pub name: String,
    pub dir: bool,
    /// "on the server", "on this computer".
    pub where_: String,
    pub there: (u64, Option<i64>),
    pub copying: (u64, Option<i64>),
    /// How many more in this copy the choice could apply to.
    pub rest: usize,
    answer: std::sync::mpsc::Sender<Option<(Choice, bool)>>,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<VecDeque<Arc<Item>>>,
    /// Every item since the strip's job started (its percentage).
    run: Mutex<Vec<Arc<Item>>>,
    ask: Mutex<Option<Ask>>,
    /// A batch's choice for the rest ("Do the same…").
    batch_choice: Mutex<HashMap<u64, Choice>>,
    /// Folders that changed (place key, folder): the panes read them again.
    changed: Mutex<Vec<(String, String)>>,
}

pub struct Transfers {
    pub items: Vec<Arc<Item>>,
    shared: Arc<Shared>,
    job: Entity<Job>,
    asking: bool,
    next_id: u64,
    next_batch: u64,
    _poll: Option<Task<()>>,
    _observe: Subscription,
}

/// Folders a copy changed: the Files page reads them again.
pub struct Changed(pub Vec<(String, String)>);
impl EventEmitter<Changed> for Transfers {}

struct Slot(Entity<Transfers>);
impl Global for Slot {}

/// The window's one queue.
pub fn get(cx: &mut App) -> Entity<Transfers> {
    if let Some(s) = cx.try_global::<Slot>() {
        return s.0.clone();
    }
    let job = cx.new(|_| Job::new());
    let t = cx.new(|cx| {
        let observe = cx.observe(&job, |t: &mut Transfers, job, cx| {
            // The worker ended with copies queued after it looked: go on.
            if !job.read(cx).is_running() && t.shared.queue.lock().map(|q| !q.is_empty()).unwrap_or(false) {
                t.start(cx);
            }
            cx.notify();
        });
        Transfers { items: Vec::new(), shared: Arc::new(Shared::default()), job: job.clone(), asking: false, next_id: 1, next_batch: 1, _poll: None, _observe: observe }
    });
    let (t1, panel) = (t.clone(), cx.new(|cx| Panel { t: t.clone(), _o: cx.observe(&t, |_, _, cx| cx.notify()) }));
    let p2 = panel.clone();
    job.update(cx, |j, _| {
        j.set_chip(std::rc::Rc::new(move |cx| t1.read(cx).chip_text()), panel.into());
        // Once it ended, the chip's Details shows the same list.
        j.set_details(std::rc::Rc::new(move |cx| {
            crate::modal::open(crate::modal::Modal { title: "Transfers".into(), body: p2.clone().into(), width: 440., icon: None, on_close: None }, cx);
        }));
    });
    cx.set_global(Slot(t.clone()));
    t
}

/// What a copy asks for: these names in `from_dir` on `from`, into `to_dir`
/// on `to`.
pub struct Request {
    pub from: Place,
    pub from_admin: bool,
    pub from_dir: String,
    pub names: Vec<String>,
    pub to: Place,
    pub to_admin: bool,
    pub to_dir: String,
}

impl Transfers {
    pub fn add(&mut self, r: Request, cx: &mut Context<Self>) {
        let batch = self.next_batch;
        self.next_batch += 1;
        for name in r.names {
            let item = Arc::new(Item {
                id: self.next_id,
                batch,
                src: r.from.join(&r.from_dir, &name),
                name,
                from: r.from.clone(),
                from_admin: r.from_admin,
                to: r.to.clone(),
                to_admin: r.to_admin,
                dest_dir: r.to_dir.clone(),
                total: AtomicU64::new(0),
                done: AtomicU64::new(0),
                state: Mutex::new(St::Waiting),
                cancel: AtomicBool::new(false),
                started: Mutex::new(None),
            });
            self.next_id += 1;
            self.items.insert(0, item.clone());
            if let Ok(mut q) = self.shared.queue.lock() {
                q.push_back(item.clone());
            }
            if let Ok(mut run) = self.shared.run.lock() {
                run.push(item);
            }
        }
        if !self.job.read(cx).is_running() {
            self.start(cx);
        }
        cx.notify();
    }

    /// Screens: items in each state, as C3 shows them.
    #[cfg(debug_assertions)]
    pub fn debug_fill(&mut self, remote: Place, cx: &mut Context<Self>) {
        let mk = |id: u64, name: &str, total: u64, done: u64, st: St, dir: &str| Arc::new(Item {
            id,
            batch: 1,
            name: name.into(),
            from: Place::Local,
            from_admin: false,
            src: format!("/home/mz/INVENIA/{name}"),
            to: remote.clone(),
            to_admin: false,
            dest_dir: dir.into(),
            total: AtomicU64::new(total),
            done: AtomicU64::new(done),
            state: Mutex::new(st),
            cancel: AtomicBool::new(false),
            started: Mutex::new(Some(Instant::now() - std::time::Duration::from_secs_f64(done as f64 / 5_033_000.))),
        });
        let now = chrono::Local::now().timestamp() - 60;
        self.items = vec![
            mk(1, "gateway-4.16.2-linux-x86_64.tar.gz", 40_055_603, 16_882_000, St::Copying, "/opt/sentient"),
            mk(2, "plant-dashboard.json", 421_888, 0, St::Waiting, "/opt/sentient"),
            mk(3, "sunoil-tags.csv", 90_112, 90_112, St::Done(now), "/opt/sentient/conf"),
        ];
        if let Ok(mut run) = self.shared.run.lock() {
            *run = self.items.clone();
        }
        self.job.update(cx, |j, cx| {
            j.set_label("Copying");
            j.start(|_| {
                std::thread::sleep(std::time::Duration::from_secs(3600));
                Ok(())
            }, cx)
        });
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if let Ok(mut run) = self.shared.run.lock() {
            run.retain(|i| matches!(i.st(), St::Waiting | St::Copying));
        }
        let shared = self.shared.clone();
        self.job.update(cx, |j, cx| {
            j.set_label("Copying");
            j.start(move |sink| work(shared, sink), cx);
        });
        // Asks and changed folders reach the page while it runs.
        let shared = self.shared.clone();
        self._poll = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
            let go = this
                .update(cx, |t, cx| {
                    let changed: Vec<(String, String)> = shared.changed.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
                    if !changed.is_empty() {
                        cx.emit(Changed(changed));
                    }
                    if !t.asking && shared.ask.lock().map(|a| a.is_some()).unwrap_or(false) {
                        t.asking = true;
                        let me = cx.entity();
                        cx.defer(move |cx| open_ask(me, cx));
                    }
                    cx.notify();
                    t.job.read(cx).is_running() || t.asking
                })
                .unwrap_or(false);
            if !go {
                break;
            }
        }));
    }

    /// The chip: "Copying 2 items · 42%".
    pub fn chip_text(&self) -> String {
        let run = self.shared.run.lock().map(|r| r.clone()).unwrap_or_default();
        let open: Vec<&Arc<Item>> = run.iter().filter(|i| !matches!(i.st(), St::Cancelled | St::Skipped)).collect();
        let total: u64 = open.iter().map(|i| i.total.load(Ordering::Relaxed)).sum();
        let done: u64 = open.iter().map(|i| i.done.load(Ordering::Relaxed)).sum();
        let n = open.len();
        let pct = if total > 0 { format!(" · {}%", done * 100 / total) } else { String::new() };
        format!("Copying {n} item{}{pct}", if n == 1 { "" } else { "s" })
    }

    pub fn cancel(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(i) = self.items.iter().find(|i| i.id == id) {
            i.cancel.store(true, Ordering::Relaxed);
            if i.st() == St::Waiting {
                i.set(St::Cancelled);
            }
        }
        cx.notify();
    }

    pub fn clear_finished(&mut self, cx: &mut Context<Self>) {
        self.items.retain(|i| matches!(i.st(), St::Waiting | St::Copying));
        cx.notify();
    }

    /// Copying into `dir` on `place` now (the pane's "copying…" rows).
    pub fn copying_into(&self, place: &str, dir: &str) -> Vec<(String, u64)> {
        self.items
            .iter()
            .filter(|i| i.st() == St::Copying && i.to.key() == place && i.dest_dir == dir)
            .map(|i| (i.name.clone(), i.done.load(Ordering::Relaxed)))
            .collect()
    }

    fn answer(&mut self, a: Option<(Choice, bool)>, cx: &mut Context<Self>) {
        if let Some(ask) = self.shared.ask.lock().ok().and_then(|mut a| a.take()) {
            let _ = ask.answer.send(a);
        }
        self.asking = false;
        cx.notify();
    }
}

// -- The worker (a background thread) ---------------------------------------

type Conns = HashMap<(String, bool), Box<dyn Fs>>;

fn take(conns: &mut Conns, place: &Place, admin: bool) -> Result<Box<dyn Fs>, String> {
    match conns.remove(&(place.key(), admin)) {
        Some(f) => Ok(f),
        None => fs::open(place, admin),
    }
}

fn work(shared: Arc<Shared>, sink: ProgressFn) -> Result<(), String> {
    let mut conns: Conns = HashMap::new();
    let (mut ok, mut failed, mut stopped, mut last_err) = (0usize, 0usize, 0usize, String::new());
    let last_pct = Mutex::new(Instant::now());
    let report = |shared: &Shared| {
        let run = shared.run.lock().map(|r| r.clone()).unwrap_or_default();
        let total: u64 = run.iter().map(|i| i.total.load(Ordering::Relaxed)).sum();
        let done: u64 = run.iter().map(|i| i.done.load(Ordering::Relaxed)).sum();
        if total > 0 {
            sink(Progress::Percent { value: done as f32 / total as f32 });
        }
    };
    loop {
        let Some(item) = shared.queue.lock().ok().and_then(|mut q| q.pop_front()) else { break };
        if item.cancel.load(Ordering::Relaxed) {
            stopped += 1;
            item.set(St::Cancelled);
            continue;
        }
        item.set(St::Copying);
        if let Ok(mut s) = item.started.lock() {
            *s = Some(Instant::now());
        }
        let tick = || {
            let mut l = last_pct.lock().unwrap_or_else(|e| e.into_inner());
            if l.elapsed().as_millis() > 200 {
                *l = Instant::now();
                report(&shared);
            }
        };
        match copy_item(&item, &shared, &mut conns, &tick) {
            Ok(Some(st)) => {
                stopped += 1;
                item.set(st)
            }
            Ok(None) => {
                ok += 1;
                item.set(St::Done(chrono::Local::now().timestamp()));
            }
            Err(_) if item.cancel.load(Ordering::Relaxed) => {
                stopped += 1;
                item.set(St::Cancelled);
            }
            Err(e) => {
                failed += 1;
                last_err = format!("{}: {e}", item.name);
                sink(Progress::Log { line: last_err.clone() });
                item.set(St::Failed(e));
            }
        }
        if let Ok(mut c) = shared.changed.lock() {
            c.push((item.to.key(), item.dest_dir.clone()));
        }
        report(&shared);
    }
    if failed > 0 {
        return Err(if failed == 1 { last_err } else { format!("{failed} copies failed; the last: {last_err}") });
    }
    let skipped = if stopped > 0 { format!(" {stopped} cancelled or skipped.") } else { String::new() };
    sink(Progress::Done { message: format!("Copied {ok} item{}.{skipped}", if ok == 1 { "" } else { "s" }) });
    Ok(())
}

/// "name (2).ext" when `name` is taken.
fn free_name(dst: &mut dyn Fs, to: &Place, dir: &str, name: &str) -> Result<String, String> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..1000 {
        let cand = format!("{stem} ({n}){ext}");
        if dst.meta(&to.join(dir, &cand))?.is_none() {
            return Ok(cand);
        }
    }
    Err("No free name for a second copy.".into())
}

/// Ok(None): copied; Ok(Some(state)): skipped or cancelled.
fn copy_item(item: &Item, shared: &Shared, conns: &mut Conns, tick: &dyn Fn()) -> Result<Option<St>, String> {
    let mut src = take(conns, &item.from, item.from_admin)?;
    // The same side both ends (a copy within a pane): two connections.
    let same = item.from.key() == item.to.key() && item.from_admin == item.to_admin;
    let mut dst = if same { fs::open(&item.to, item.to_admin)? } else { take(conns, &item.to, item.to_admin)? };
    let r = copy_item_with(item, shared, &mut src, &mut dst, tick);
    conns.insert((item.from.key(), item.from_admin), src);
    if !same {
        conns.insert((item.to.key(), item.to_admin), dst);
    }
    r
}

fn copy_item_with(item: &Item, shared: &Shared, src: &mut Box<dyn Fs>, dst: &mut Box<dyn Fs>, tick: &dyn Fn()) -> Result<Option<St>, String> {
    let (is_dir, size, mtime) = src.meta(&item.src)?.ok_or_else(|| format!("{} is not there any more.", item.src))?;
    let mut name = item.name.clone();
    let mut dest = item.to.join(&item.dest_dir, &name);
    let mut replace = false;
    if let Some((there_dir, there_size, there_time)) = dst.meta(&dest)? {
        let remembered = shared.batch_choice.lock().ok().and_then(|c| c.get(&item.batch).copied());
        let choice = match remembered {
            Some(c) => c,
            None => {
                let rest = shared.queue.lock().map(|q| q.iter().filter(|i| i.batch == item.batch).count()).unwrap_or(0);
                let (tx, rx) = std::sync::mpsc::channel();
                if let Ok(mut a) = shared.ask.lock() {
                    *a = Some(Ask {
                        name: name.clone(),
                        dir: there_dir,
                        where_: if item.to.is_remote() { "on the server".into() } else { "on this computer".into() },
                        there: (there_size, there_time),
                        copying: (size, mtime),
                        rest,
                        answer: tx,
                    });
                }
                match rx.recv().ok().flatten() {
                    None => {
                        // Cancel copy: this one and the rest of it.
                        if let Ok(q) = shared.queue.lock() {
                            q.iter().filter(|i| i.batch == item.batch).for_each(|i| i.cancel.store(true, Ordering::Relaxed));
                        }
                        return Ok(Some(St::Cancelled));
                    }
                    Some((c, rest)) => {
                        if rest {
                            if let Ok(mut m) = shared.batch_choice.lock() {
                                m.insert(item.batch, c);
                            }
                        }
                        c
                    }
                }
            }
        };
        match choice {
            Choice::Skip => return Ok(Some(St::Skipped)),
            Choice::KeepBoth => {
                name = free_name(&mut **dst, &item.to, &item.dest_dir, &name)?;
                dest = item.to.join(&item.dest_dir, &name);
            }
            Choice::Replace => {
                // A folder over a file (or the other way): the old one goes.
                if there_dir != is_dir {
                    dst.remove_tree(&dest)?;
                } else {
                    replace = true;
                }
            }
        }
    }
    if !is_dir {
        item.total.store(size, Ordering::Relaxed);
        return copy_file(item, src, dst, &item.src, &dest, size, replace, tick).map(|_| None);
    }
    // A folder: everything under it, folders first.
    let mut files: Vec<(String, String, u64)> = Vec::new();
    let mut dirs: Vec<String> = vec![dest.clone()];
    let mut stack = vec![(item.src.clone(), dest.clone())];
    while let Some((s, d)) = stack.pop() {
        if item.cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        for e in src.list(&s)?.entries {
            let (sp, dp) = (item.from.join(&s, &e.name), item.to.join(&d, &e.name));
            if e.dir {
                dirs.push(dp.clone());
                stack.push((sp, dp));
            } else {
                files.push((sp, dp, e.size));
            }
        }
    }
    item.total.store(files.iter().map(|f| f.2).sum(), Ordering::Relaxed);
    for d in &dirs {
        if dst.meta(d)?.is_none() {
            dst.mkdir(d)?;
        }
    }
    for (s, d, size) in files {
        let there = dst.meta(&d)?.is_some();
        copy_file(item, src, dst, &s, &d, size, there, tick)?;
    }
    Ok(None)
}

/// One file through `<dest>.spm-part`: resumed from what is there, and
/// after a broken connection on new ones (three times), its size checked,
/// then renamed into place.
#[allow(clippy::too_many_arguments)]
fn copy_file(item: &Item, src: &mut Box<dyn Fs>, dst: &mut Box<dyn Fs>, from: &str, dest: &str, size: u64, replace: bool, tick: &dyn Fn()) -> Result<(), String> {
    let part = format!("{dest}.spm-part");
    let mut attempts = 0;
    let base = item.done.load(Ordering::Relaxed);
    loop {
        let offset = match dst.meta(&part)? {
            Some((false, n, _)) if n <= size => n,
            Some(_) => {
                dst.remove_tree(&part)?;
                0
            }
            None => 0,
        };
        item.done.store(base + offset, Ordering::Relaxed);
        match pipe(item, &mut **src, &mut **dst, from, &part, offset, size, tick) {
            Ok(()) => break,
            Err(e) if item.cancel.load(Ordering::Relaxed) => {
                // Cancelled: the part goes (one that broke stays, to resume).
                let _ = dst.remove_tree(&part);
                return Err(e);
            }
            Err(e) if (src.broken(&e) || dst.broken(&e)) && attempts < 3 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_secs(2 * attempts));
                if src.broken(&e) {
                    *src = fs::open(&item.from, item.from_admin)?;
                }
                if dst.broken(&e) {
                    *dst = fs::open(&item.to, item.to_admin)?;
                }
            }
            Err(e) => return Err(e),
        }
    }
    let got = dst.meta(&part)?.map(|m| m.1).unwrap_or(0);
    if got != size {
        return Err(format!("The size check failed: {} of {} arrived. Copy it again to resume.", bytes_label(got), bytes_label(size)));
    }
    if replace {
        dst.remove_tree(dest)?;
    }
    dst.rename(&part, dest)
}

#[allow(clippy::too_many_arguments)]
fn pipe(item: &Item, src: &mut dyn Fs, dst: &mut dyn Fs, from: &str, to: &str, offset: u64, size: u64, tick: &dyn Fn()) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let cancel = &item.cancel;
    std::thread::scope(|s| {
        let reader = s.spawn(move || src.read(from, offset, size, &mut |d| !cancel.load(Ordering::Relaxed) && tx.send(d.to_vec()).is_ok()));
        let w = dst.write(to, offset, &mut || {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled".into());
            }
            match rx.recv() {
                Ok(d) => {
                    item.done.fetch_add(d.len() as u64, Ordering::Relaxed);
                    tick();
                    Ok(Some(d))
                }
                Err(_) => Ok(None),
            }
        });
        // The writer stopped early: the reader's channel closes with it.
        drop(rx);
        let r = reader.join().map_err(|_| "The copy stopped unexpectedly.".to_string())?;
        w?;
        r
    })
}

// -- The chip's panel (C3) ---------------------------------------------------

/// "16.1" of "38.2 MB": the unit once when both share it.
fn same_unit(done: u64, total: u64) -> String {
    let (d, t) = (bytes_label(done), bytes_label(total));
    match (d.rsplit_once(' '), t.rsplit_once(' ')) {
        (Some((n, u1)), Some((_, u2))) if u1 == u2 => n.to_string(),
        _ => d,
    }
}

struct Panel {
    t: Entity<Transfers>,
    _o: Subscription,
}

fn time_of(t: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_opt(t, 0).single().map(|d| d.format("%H:%M").to_string()).unwrap_or_default()
}

impl Render for Panel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let items = self.t.read(cx).items.clone();
        let t = self.t.clone();
        let header = h_flex()
            .h(px(52.))
            .px(px(16.))
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().font_weight(FontWeight::MEDIUM).child("Transfers"))
            .child(crate::ui::button("tr-clear").ghost().small().text_color(theme.primary).label("Clear finished").on_click({
                let t = t.clone();
                move |_, _, cx| t.update(cx, |t, cx| t.clear_finished(cx))
            }));
        let mut list = v_flex().id("tr-list").max_h(px(420.)).overflow_y_scroll();
        if items.is_empty() {
            list = list.child(div().px(px(16.)).py_4().text_sm().text_color(theme.muted_foreground).child("No copies yet."));
        }
        for (n, i) in items.iter().enumerate() {
            let st = i.st();
            let (total, done) = (i.total.load(Ordering::Relaxed), i.done.load(Ordering::Relaxed));
            let icon = match &st {
                St::Done(_) => Icon::new(IconName::CircleCheck).text_color(theme.success_foreground),
                St::Failed(_) => Icon::new(IconName::CircleX).text_color(theme.danger),
                _ => Icon::new(IconName::ArrowRight).text_color(theme.primary),
            };
            let right: AnyElement = match &st {
                St::Copying | St::Waiting if !i.cancel.load(Ordering::Relaxed) && st == St::Copying => {
                    let (t2, id) = (t.clone(), i.id);
                    crate::ui::button(SharedString::from(format!("tr-cancel-{}", i.id))).outline().small().label("Cancel").on_click(move |_, _, cx| t2.update(cx, |t, cx| t.cancel(id, cx))).into_any_element()
                }
                _ => {
                    let w = match &st {
                        St::Waiting => "Waiting",
                        St::Copying => "Stopping…",
                        St::Done(_) => "Done",
                        St::Failed(_) => "Failed",
                        St::Cancelled => "Cancelled",
                        St::Skipped => "Skipped",
                    };
                    div().text_sm().text_color(if matches!(st, St::Failed(_)) { theme.danger } else { theme.muted_foreground }).child(w).into_any_element()
                }
            };
            let dest = &i.dest_dir;
            let line = match &st {
                St::Copying => None,
                St::Waiting => Some(format!("To {dest}")),
                St::Done(at) => Some(format!("{} to {dest} · {}", bytes_label(total), time_of(*at))),
                St::Failed(e) => Some(e.clone()),
                St::Cancelled | St::Skipped => Some(format!("To {dest}")),
            };
            let mut row = v_flex()
                .px(px(16.))
                .py(px(12.))
                .gap(px(6.))
                .when(n > 0, |r| r.border_t_1().border_color(theme.border))
                .child(
                    h_flex()
                        .gap(px(10.))
                        .items_center()
                        .child(icon.size_4().flex_shrink_0())
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(FontWeight::MEDIUM).text_sm().child(i.name.clone()))
                        .child(right),
                );
            if st == St::Copying {
                let frac = if total > 0 { (done as f32 / total as f32).clamp(0., 1.) } else { 0. };
                let rate = i.rate();
                let left = rate.filter(|r| *r > 0.).map(|r| ((total.saturating_sub(done)) as f64 / r).ceil() as u64);
                row = row
                    .child(div().h(px(4.)).rounded_full().bg(theme.muted).child(div().h_full().rounded_full().w(relative(frac)).bg(theme.primary)))
                    .child(
                        h_flex()
                            .text_size(px(12.5))
                            .text_color(theme.muted_foreground)
                            .child(div().flex_1().child(format!("{} of {}{}", same_unit(done, total), bytes_label(total), rate.map(|r| format!(" · {}/s", bytes_label(r as u64))).unwrap_or_default())))
                            .when_some(left, |r, s| r.child(if s < 60 { format!("about {s} s left") } else { format!("about {} min left", s.div_ceil(60)) })),
                    );
            }
            if let Some(l) = line {
                row = row.child(div().text_size(px(12.5)).text_color(if matches!(st, St::Failed(_)) { theme.danger } else { theme.muted_foreground }).child(l));
            }
            list = list.child(row);
        }
        v_flex().child(header).child(list)
    }
}

// -- C5: a name already there ------------------------------------------------

struct AskView {
    t: Entity<Transfers>,
    choice: Choice,
    rest: bool,
}

fn when_label(bytes: u64, t: Option<i64>) -> String {
    use chrono::TimeZone as _;
    let at = t.and_then(|t| chrono::Local.timestamp_opt(t, 0).single()).map(|d| format!(" · {}", d.format("%-d %b %H:%M"))).unwrap_or_default();
    format!("{}{at}", bytes_label(bytes))
}

fn open_ask(t: Entity<Transfers>, cx: &mut App) {
    let title = {
        let s = t.read(cx).shared.clone();
        let a = s.ask.lock().ok();
        a.as_ref().and_then(|a| a.as_ref()).map(|a| format!("{} is already {}", a.name, a.where_)).unwrap_or_default()
    };
    let dir = t.read(cx).shared.ask.lock().ok().and_then(|a| a.as_ref().map(|a| a.dir)).unwrap_or(false);
    let view = cx.new(|_| AskView { t: t.clone(), choice: Choice::Replace, rest: true });
    let footer = cx.new(|cx| AskFooter { v: view.clone(), _o: cx.observe(&view, |_, _, cx| cx.notify()) });
    let t2 = t.clone();
    crate::modal::open(
        crate::modal::Modal {
            title: title.into(),
            body: view.into(),
            width: 520.,
            icon: Some(if dir { Icon::new(IconName::Folder) } else { Icon::new(IconName::File) }),
            on_close: Some(std::rc::Rc::new(move |_, cx| t2.update(cx, |t, cx| t.answer(None, cx)))),
        },
        cx,
    );
    crate::modal::set_footer(footer.into(), cx);
}

impl Render for AskView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let shared = self.t.read(cx).shared.clone();
        let guard = shared.ask.lock().ok();
        let Some(a) = guard.as_ref().and_then(|g| g.as_ref()) else { return div().into_any_element() };
        let where_ = if a.where_ == "on the server" { "On the server" } else { "On this computer" };
        let row = |label: &'static str, value: String, line: bool| {
            h_flex()
                .gap(px(16.))
                .child(
                    h_flex().flex_1().min_w_0().min_h(px(53.)).items_center().when(line, |r| r.border_b_1().border_color(theme.border))
                        .child(div().w(px(170.)).flex_shrink_0().text_sm().text_color(theme.muted_foreground).child(label))
                        .child(div().text_sm().child(value)),
                )
        };
        let seg = crate::ui::seg_track(&theme)
            .child(crate::ui::seg_item(&theme, "ask-replace", "Replace", self.choice == Choice::Replace).on_click(cx.listener(|v, _, _, cx| {
                v.choice = Choice::Replace;
                cx.notify();
            })))
            .child(crate::ui::seg_item(&theme, "ask-both", "Keep both", self.choice == Choice::KeepBoth).on_click(cx.listener(|v, _, _, cx| {
                v.choice = Choice::KeepBoth;
                cx.notify();
            })))
            .child(crate::ui::seg_item(&theme, "ask-skip", "Skip", self.choice == Choice::Skip).on_click(cx.listener(|v, _, _, cx| {
                v.choice = Choice::Skip;
                cx.notify();
            })));
        let rest = a.rest;
        v_flex()
            .gap(px(10.))
            .child(row(where_, when_label(a.there.0, a.there.1), true))
            .child(row("Copying", when_label(a.copying.0, a.copying.1), false))
            .child(h_flex().pt_1().child(seg))
            .when(rest > 0, |c| {
                c.child(Checkbox::new("ask-rest").label(format!("Do the same for the other {rest} item{}", if rest == 1 { "" } else { "s" })).checked(self.rest).on_change(cx.listener(|v, val: &bool, _, cx| {
                    v.rest = *val;
                    cx.notify();
                })))
            })
            .into_any_element()
    }
}

struct AskFooter {
    v: Entity<AskView>,
    _o: Subscription,
}

impl Render for AskFooter {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (v1, v2) = (self.v.clone(), self.v.clone());
        h_flex()
            .w_full()
            .gap_2()
            .justify_end()
            .child(crate::ui::button("ask-cancel").outline().label("Cancel copy").on_click(move |_, _, cx| {
                let t = v1.read(cx).t.clone();
                crate::modal::close(cx);
                t.update(cx, |t, cx| t.answer(None, cx));
            }))
            .child(crate::ui::button("ask-go").primary().label("Continue").on_click(move |_, _, cx| {
                let (t, c, rest) = {
                    let v = v2.read(cx);
                    (v.t.clone(), v.choice, v.rest)
                };
                crate::modal::close(cx);
                t.update(cx, |t, cx| t.answer(Some((c, rest)), cx));
            }))
    }
}

/// Screens: C5 with a canned question.
#[cfg(debug_assertions)]
pub fn debug_ask(cx: &mut App) {
    let t = get(cx);
    let (tx, _rx) = std::sync::mpsc::channel();
    std::mem::forget(_rx);
    let now = chrono::Local::now().timestamp();
    t.update(cx, |t, cx| {
        if let Ok(mut a) = t.shared.ask.lock() {
            *a = Some(Ask { name: "sentient.yml".into(), dir: false, where_: "on the server".into(), there: (24_576, Some(now - 3600 * 13)), copying: (26_624, Some(now - 600)), rest: 3, answer: tx });
        }
        t.asking = true;
        cx.notify();
    });
    open_ask(t, cx);
}
