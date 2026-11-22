// Copyright © 2016-2026 The SENTIENT Authors

//! Logs: one view for this computer and for a remote server. Pick a source
//! (SENTIENT, Database, ClickHouse, Gateway or All, from what the install
//! has), filter by level and text, follow it live, select lines to copy, and
//! save it. Every line reads as columns: local time, source (in All), level,
//! message.

use gpui_kit::component::scroll::ScrollableElement as _;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, h_flex, v_flex,
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::log_level::Level;
use crate::logsource::{self, Line, Source};
use crate::ui;

const TAIL: u32 = 500;
/// Read again this often while following a source that cannot be followed.
const FOLLOW_EVERY: Duration = Duration::from_secs(3);
/// Followed lines are shown in batches this often (not a redraw a line).
const STREAM_BATCH: Duration = Duration::from_millis(200);
/// A follow that failed is tried again after this long (meanwhile: read
/// every few seconds).
const STREAM_RETRY: Duration = Duration::from_secs(60);
/// While following, at most this many lines are kept.
const KEEP_MAX: usize = 2000;
/// How many of the last lines read are compared with the first followed ones.
const DEDUP_TAIL: usize = 50;

/// Reads one source: (source, lines, since midnight) → raw text, or why not.
pub type LoadFn = Arc<dyn Fn(Source, u32, bool) -> Result<String, String> + Send + Sync>;
/// Follows one source: (source, emit(line) false = stop, going) until going
/// says no; Err: it cannot be followed (or the follow ended).
pub type FollowFn = Arc<dyn Fn(Source, &mut dyn FnMut(&str) -> bool, &dyn Fn() -> bool) -> Result<(), String> + Send + Sync>;
/// The sources there are, or why they could not be read.
pub type SourcesFn = Arc<dyn Fn() -> Result<Vec<Source>, String> + Send + Sync>;
/// A longer read of one source written into a file as it comes (Save log):
/// (source, lines, today, out, progress, going(bytes) false = cancel) →
/// the log's bytes. Never holds a whole day's log in memory.
pub type SaveFn = Arc<dyn Fn(Source, u32, bool, &mut dyn std::io::Write, &sentient_installer_core::progress::ProgressFn, &dyn Fn(u64) -> bool) -> Result<u64, String> + Send + Sync>;

/// [`SaveFn`] from a [`LoadFn`] (a small log: the Platform Manager's own).
fn save_from_load(load: LoadFn) -> SaveFn {
    Arc::new(move |s, n, today, out, _sink, _going| {
        let t = load(s, n, today)?;
        if logsource::is_failure_text(&t) {
            return Err(t.trim().to_string());
        }
        out.write_all(t.as_bytes()).and_then(|_| out.write_all(b"\n")).map_err(|e| format!("writing the log: {e}"))?;
        Ok(t.len() as u64 + 1)
    })
}

/// What this computer's Logs showed, kept when the window closes while
/// the Platform Manager keeps running (the tray): a window opened again
/// draws these lines at once and reads them again behind them. Memory
/// only, this run.
struct Kept {
    /// The installation they were read for ([`record_key`]).
    key: String,
    sources: Vec<Source>,
    pick: Option<Source>,
    levels: Levels,
    lines: Vec<Line>,
    follow: bool,
}

thread_local! {
    static KEPT: std::cell::RefCell<Option<Kept>> = const { std::cell::RefCell::new(None) };
}

fn record_key(record: Option<&InstallationRecord>) -> String {
    serde_json::to_string(&record).unwrap_or_default()
}

/// What this computer's Logs draws first for `key`: the lines this run
/// showed before (a window opened again), or None on a cold start.
fn take_kept(kept: &mut Option<Kept>, key: &str) -> Option<Kept> {
    kept.take().filter(|k| k.key == key)
}

/// The window closed (the app runs on): the lines shown stay for the next
/// window ([`Kept`]).
impl Drop for LogView {
    fn drop(&mut self) {
        let Some(key) = self.kept_key.take() else { return };
        if self.lines.is_empty() {
            return;
        }
        let k = Kept { key, sources: std::mem::take(&mut self.sources), pick: self.pick, levels: self.levels, lines: std::mem::take(&mut self.lines), follow: self.follow };
        // (Not while the thread ends: nothing opens again then.)
        let _ = KEPT.try_with(|c| *c.borrow_mut() = Some(k));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Levels {
    All,
    Warnings,
    Errors,
}

pub struct LogView {
    load: LoadFn,
    save_fn: SaveFn,
    /// Save log, as a named job (the progress strip, the bell) with its own
    /// line and Cancel under the toolbar.
    save_job: Entity<crate::jobs::Job>,
    save_cancel: Arc<std::sync::atomic::AtomicBool>,
    sources_fn: SourcesFn,
    sources: Vec<Source>,
    /// None: All.
    pick: Option<Source>,
    levels: Levels,
    lines: Vec<Line>,
    filter: Entity<InputState>,
    follow: bool,
    status: Option<SharedString>,
    /// A read that failed: the page shows it with Try again, never as a line.
    error: Option<String>,
    /// One file (the Platform Manager's own log): no source choice.
    single: bool,
    scroll: ScrollHandle,
    /// Selected lines, as indices into the shown list.
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    focus: FocusHandle,
    /// Its instance is not the open one (a kept page): no reading until
    /// it is shown again.
    paused: bool,
    /// The page it is on, for a view that reads only while that page
    /// shows (this computer's Logs page); None: a dialog's, or a server's
    /// (`paused` covers that).
    shown_on: Option<super::Page>,
    /// This computer's Logs: what it showed is kept for the next window
    /// under this key ([`Kept`]); None for a dialog's or a server's view.
    kept_key: Option<String>,
    /// On a page (this computer's Logs, a server's): the log box fills the
    /// page's height, never less than [`FILL_MIN_H`]. In a dialog: [`DIALOG_H`].
    fill: bool,
    /// New lines as they are written (this computer's sources, a server's
    /// through its agent); None: read again every few seconds.
    follow_fn: Option<FollowFn>,
    /// A follow failed: not tried again before this.
    stream_again: Option<std::time::Instant>,
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// What a followed batch leads to.
enum Step {
    Go,
    /// Read again (and follow again): the source, Follow or the page changed.
    Again,
    /// The view is gone.
    Gone,
}

/// The follows of the sources shown, each on a thread of its own; dropped,
/// they all stop.
struct Streams {
    stop: Arc<std::sync::atomic::AtomicBool>,
    rx: std::sync::mpsc::Receiver<Streamed>,
}

enum Streamed {
    Line(Source, String),
    /// A follow ended (Ok: it was stopped).
    Ended(Result<(), String>),
}

impl Streams {
    fn start(f: &FollowFn, sources: &[Source]) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        for &s in sources {
            let (f, stop, tx) = (f.clone(), stop.clone(), tx.clone());
            std::thread::spawn(move || {
                let going = || !stop.load(std::sync::atomic::Ordering::SeqCst);
                let r = f(s, &mut |l| going() && tx.send(Streamed::Line(s, l.to_string())).is_ok(), &going);
                let _ = tx.send(Streamed::Ended(r));
            });
        }
        Streams { stop, rx }
    }

    /// The lines come so far, and whether a follow ended by itself.
    fn take(&self) -> (Vec<(Source, String)>, Option<String>) {
        let (mut got, mut ended) = (Vec::new(), None);
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Streamed::Line(s, l) => got.push((s, l)),
                Streamed::Ended(Err(e)) => ended = Some(e),
                Streamed::Ended(Ok(())) => {}
            }
        }
        (got, ended)
    }
}

impl Drop for Streams {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The log box in a dialog.
const DIALOG_H: f32 = 480.;
/// The log box on a page never gets lower than this; a window too low for
/// it scrolls the page instead.
const FILL_MIN_H: f32 = 240.;

impl LogView {
    /// On a page: the log box takes the page's height (see [`Self::fill`]).
    pub fn filling(mut self) -> Self {
        self.fill = true;
        self
    }

    /// A re-read that failed while the lines read before still show.
    pub fn stale(&self) -> Option<&str> {
        self.error.as_deref().filter(|_| !self.lines.is_empty())
    }

    /// Hidden behind another instance (or shown again: read at once).
    pub fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        let was = self.paused;
        self.paused = paused;
        if was && !paused && self.follow {
            self.start(cx);
        }
    }

    pub fn new(load: LoadFn, sources_fn: SourcesFn, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter lines"));
        let _subscriptions = vec![cx.subscribe_in(&filter, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.selected.clear();
                cx.notify();
            }
        })];
        let save_job = cx.new(|_| crate::jobs::Job::new());
        let mut _subscriptions = _subscriptions;
        _subscriptions.push(cx.observe(&save_job, |_, _, cx| cx.notify()));
        let mut v = Self {
            save_fn: save_from_load(load.clone()),
            save_job,
            save_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            load,
            sources_fn,
            sources: vec![Source::Sentient],
            pick: Some(Source::Sentient),
            levels: Levels::All,
            lines: Vec::new(),
            filter,
            follow: true,
            status: None,
            error: None,
            single: false,
            scroll: ScrollHandle::new(),
            selected: BTreeSet::new(),
            anchor: None,
            focus: cx.focus_handle(),
            paused: false,
            shown_on: None,
            kept_key: None,
            fill: false,
            follow_fn: None,
            stream_again: None,
            _poll: None,
            _subscriptions,
        };
        // Screens: open on a given source ("all" for All).
        #[cfg(debug_assertions)]
        if let Ok(k) = std::env::var("SPM_LOG_SOURCE") {
            v.pick = if k == "all" { None } else { Source::from_key(&k) };
        }
        // Screens: Save log under way, failed or done (SPM_DEBUG_LOG_SAVE=running|failed|done).
        #[cfg(debug_assertions)]
        if let Ok(kind) = std::env::var("SPM_DEBUG_LOG_SAVE") {
            v.save_job.update(cx, |j, cx| {
                j.start("Saving the SENTIENT log", move |sink| {
                    use sentient_installer_core::progress::Progress;
                    sink(Progress::Step { name: "Downloading".into() });
                    sink(Progress::Log { line: "Downloaded 41.2 MB of 96.0 MB".into() });
                    std::thread::sleep(Duration::from_secs(if kind == "running" { 600 } else { 1 }));
                    match kind.as_str() {
                        "failed" => Err("The log was not saved: the server's disk is full (No space left on device).".into()),
                        _ => {
                            sink(Progress::Done { message: "Saved to /home/mz/sentient-log-20261121-2020.log (812.4 MB)".into() });
                            Ok(())
                        }
                    }
                }, cx)
            });
        }
        v.read_sources(cx);
        v.start(cx);
        v
    }

    /// This computer's logs: SENTIENT's when it is installed, the
    /// gateway's when one runs here (also on a gateway-only computer).
    pub fn local(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = record_key(record.as_ref());
        let r1 = record.clone();
        let load: LoadFn = Arc::new(move |s, n, today| Ok(logsource::read(r1.as_ref(), s, n, today)));
        let r2 = record.clone();
        let save: SaveFn = Arc::new(move |s, n, today, out, sink, going| {
            sink(sentient_installer_core::progress::Progress::Step { name: "Reading the log".into() });
            logsource::read_to(r2.as_ref(), s, n, today, out, going)
        });
        let r3 = record.clone();
        let follow: FollowFn = Arc::new(move |s, emit, going| crate::logfollow::follow(r3.as_ref(), s, emit, going));
        let sources: SourcesFn = Arc::new(move || Ok(logsource::available(record.as_ref())));
        let mut v = Self::new(load, sources, window, cx);
        v.save_fn = save;
        v.follow_fn = Some(follow);
        v.shown_on = Some(super::Page::Logs);
        // A window opened again: the lines shown before, at once (the
        // first read, not run yet, reads them again behind them).
        if let Some(k) = KEPT.with(|c| take_kept(&mut c.borrow_mut(), &key)) {
            v.sources = k.sources;
            v.pick = k.pick;
            v.levels = k.levels;
            v.lines = k.lines;
            v.follow = k.follow;
            if v.follow {
                v.scroll.scroll_to_bottom();
            }
        }
        v.kept_key = Some(key);
        v
    }

    /// Reopen tests: the view shows lines (or why it could not read them),
    /// not its empty "No log lines" before the first read.
    pub fn has_data(&self) -> bool {
        !self.lines.is_empty() || self.error.is_some()
    }

    /// One log file (the Platform Manager's own log, About → Open the log).
    pub fn file(path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let load: LoadFn = Arc::new(move |_, n, today| {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let from = if today {
                let day = chrono::Local::now().format("%Y-%m-%d").to_string();
                lines.iter().position(|l| l.starts_with(&day)).unwrap_or(lines.len())
            } else {
                lines.len().saturating_sub(n as usize)
            };
            Ok(lines[from..].join("\n"))
        });
        let sources: SourcesFn = Arc::new(|| Ok(vec![Source::Sentient]));
        let mut v = Self::new(load, sources, window, cx);
        v.single = true;
        v
    }

    /// A remote server's logs, read through its agent.
    pub fn remote(inst: crate::remote::Instance, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let i1 = inst.clone();
        let load: LoadFn = Arc::new(move |s, n, today| crate::remote::logs_from(&i1, s, n, today));
        let i2 = inst.clone();
        let save: SaveFn = Arc::new(move |s, n, today, out, sink, going| remote_save(&i2, s, n, today, out, sink, going));
        let i3 = inst.clone();
        let follow: FollowFn = Arc::new(move |s, emit, going| crate::remote::logs_follow(&i3, s, emit, going));
        let sources: SourcesFn = Arc::new(move || crate::remote::log_sources(&inst));
        let mut v = Self::new(load, sources, window, cx);
        v.save_fn = save;
        // Screens read canned logs: nothing to follow.
        if !(cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_REMOTE_LOGS").is_some()) {
            v.follow_fn = Some(follow);
        }
        v
    }

    fn read_sources(&mut self, cx: &mut Context<Self>) {
        let f = self.sources_fn.clone();
        cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { f() }).await;
            this.update(cx, |v, cx| {
                match got {
                    Ok(got) if !got.is_empty() => {
                        // The chosen source may not exist there.
                        if v.pick.is_some_and(|p| !got.contains(&p)) {
                            v.pick = got.first().copied();
                        }
                        v.sources = got;
                        v.start(cx);
                    }
                    Ok(_) => {}
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The chosen source(s) as columns.
    fn read(load: &LoadFn, pick: Option<Source>, sources: &[Source], n: u32, today: bool) -> Result<Vec<Line>, String> {
        let parse = |s: Source| load(s, n, today).map(|t| t.lines().filter(|l| !l.trim().is_empty()).map(|l| logsource::parse(l, s)).collect::<Vec<_>>());
        match pick {
            // Every list in time order: one source sorts like several.
            Some(s) => Ok(logsource::merge(vec![parse(s)?])),
            None => {
                // All: the sources that answered; an error only when none did.
                let read: Vec<_> = sources.iter().map(|s| parse(*s)).collect();
                if let Some(Err(e)) = read.iter().find(|r| r.is_err()).filter(|_| read.iter().all(|r| r.is_err())) {
                    return Err(e.clone());
                }
                Ok(logsource::merge(read.into_iter().filter_map(Result::ok).collect()))
            }
        }
    }

    /// Read now and, while following: new lines as they are written where
    /// the source can be followed ([`FollowFn`]), else read again every
    /// few seconds.
    fn start(&mut self, cx: &mut Context<Self>) {
        let load = self.load.clone();
        let follow_fn = self.follow_fn.clone();
        self._poll = Some(cx.spawn(async move |this, cx| loop {
            let Ok((pick, sources, paused, on, following, stream_ok)) =
                this.update(cx, |v, _| (v.pick, v.sources.clone(), v.paused, v.shown_on, v.follow, v.stream_again.is_none_or(|t| std::time::Instant::now() >= t)))
            else {
                break;
            };
            if paused {
                cx.background_executor().timer(FOLLOW_EVERY).await;
                continue;
            }
            // This computer's Logs page hidden (another page, or a server
            // open): no reading; at once when it shows again.
            if let Some(page) = on.filter(|p| !super::shown_here(*p)) {
                super::until_shown(page, cx).await;
                continue;
            }
            // Followed: the follow started before the read, so nothing
            // written while the last lines are read is missed.
            let stream = follow_fn.as_ref().filter(|_| following && stream_ok).map(|f| {
                let want: Vec<Source> = match pick {
                    Some(s) => vec![s],
                    None => sources.clone(),
                };
                Streams::start(f, &want)
            });
            let load = load.clone();
            let lines = cx.background_executor().spawn(async move { Self::read(&load, pick, &sources, TAIL, false) }).await;
            let follow = this
                .update(cx, |v, cx| {
                    if v.pick != pick {
                        v.selected.clear();
                    }
                    match lines {
                        Ok(lines) => {
                            v.lines = lines;
                            v.error = None;
                        }
                        // A failed re-read keeps the lines read before.
                        Err(e) => {
                            if v.pick != pick {
                                v.lines.clear();
                            }
                            v.error = Some(e);
                        }
                    }
                    if v.follow {
                        v.scroll.scroll_to_bottom();
                    }
                    cx.notify();
                    v.follow
                })
                .unwrap_or(false);
            if !follow {
                break;
            }
            let Some(stream) = stream else {
                cx.background_executor().timer(FOLLOW_EVERY).await;
                continue;
            };
            // New lines as they come, a few times a second at most.
            let mut first = true;
            loop {
                cx.background_executor().timer(STREAM_BATCH).await;
                let (got, ended) = stream.take();
                let step = this
                    .update(cx, |v, cx| {
                        let hidden = v.shown_on.is_some_and(|p| !super::shown_here(p));
                        if v.pick != pick || !v.follow || v.paused || hidden {
                            return Step::Again;
                        }
                        if !got.is_empty() {
                            let mut new: Vec<Line> = got.iter().filter(|(_, l)| !l.trim().is_empty()).map(|(s, l)| logsource::parse(l, *s)).collect();
                            // Lines that came while the last ones were read: those it shows already.
                            if first {
                                let tail = &v.lines[v.lines.len().saturating_sub(DEDUP_TAIL)..];
                                new.retain(|n| !tail.contains(n));
                            }
                            if !new.is_empty() {
                                v.lines.extend(new);
                                if v.lines.len() > KEEP_MAX {
                                    // The oldest go; a selection would point elsewhere.
                                    let cut = v.lines.len() - KEEP_MAX + KEEP_MAX / 4;
                                    v.lines.drain(..cut);
                                    v.selected.clear();
                                    v.anchor = None;
                                }
                                v.error = None;
                                v.scroll.scroll_to_bottom();
                                cx.notify();
                            }
                        }
                        match ended {
                            // Not followed after all (an older agent there, the
                            // connection lost): read every few seconds, and
                            // try following again in a while.
                            Some(_) => {
                                v.stream_again = Some(std::time::Instant::now() + STREAM_RETRY);
                                Step::Again
                            }
                            None => Step::Go,
                        }
                    })
                    .unwrap_or(Step::Gone);
                first = false;
                match step {
                    Step::Go => {}
                    Step::Again => break,
                    Step::Gone => return,
                }
            }
            drop(stream);
        }));
    }

    fn shown(&self, cx: &App) -> Vec<&Line> {
        let needle = self.filter.read(cx).value().to_lowercase();
        self.lines
            .iter()
            .filter(|l| match self.levels {
                Levels::All => true,
                Levels::Warnings => matches!(l.level, Level::Warn | Level::Error),
                Levels::Errors => l.level == Level::Error,
            })
            .filter(|l| needle.is_empty() || l.message.to_lowercase().contains(&needle))
            .collect()
    }

    /// One line as saved or copied: time, source (in All), level, message.
    fn text_of(l: &Line, all: bool) -> String {
        let mut s = l.time.clone();
        if all {
            s.push('\t');
            s.push_str(l.source.tag());
        }
        s.push('\t');
        s.push_str(level_word(l.level));
        s.push('\t');
        s.push_str(&l.message);
        s
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let all = self.pick.is_none();
        let shown = self.shown(cx);
        let text: Vec<String> = if self.selected.is_empty() {
            shown.iter().map(|l| Self::text_of(l, all)).collect()
        } else {
            self.selected.iter().filter_map(|i| shown.get(*i)).map(|l| Self::text_of(l, all)).collect()
        };
        let n = text.len();
        cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
        self.status = Some(format!("{n} line{} copied.", if n == 1 { "" } else { "s" }).into());
        cx.notify();
    }

    /// Save: the shown lines, or a longer read of the chosen source(s), as
    /// a named job: the log goes into the file as it is read (never held
    /// whole), with its progress and Cancel; it ends saying where the file
    /// is, or why it could not be saved (never silently).
    fn save(&mut self, what: Option<(u32, bool)>, cx: &mut Context<Self>) {
        if self.save_job.read(cx).is_running() {
            return;
        }
        let all = self.pick.is_none();
        let shown: Vec<String> = self.shown(cx).iter().map(|l| Self::text_of(l, all)).collect();
        let (save_fn, pick, sources) = (self.save_fn.clone(), self.pick, self.sources.clone());
        let what_name = pick.map(|s| s.key()).unwrap_or("all");
        let name = format!("{what_name}-log-{}.log", chrono::Local::now().format("%Y%m%d-%H%M"));
        let label = format!("Saving the {} log", pick.map(|s| s.label()).unwrap_or("whole"));
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
        let rx = crate::chooser::save(cx, "Save the log as", &home, Some(&name));
        let job = self.save_job.clone();
        let cancel = self.save_cancel.clone();
        self.status = None;
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            cancel.store(false, std::sync::atomic::Ordering::SeqCst);
            job.update(cx, |j, cx| {
                j.start(
                    label,
                    move |sink| {
                        use sentient_installer_core::progress::Progress;
                        let going = |_: u64| !cancel.load(std::sync::atomic::Ordering::SeqCst);
                        let part = path.with_extension("log.part");
                        let file = std::fs::File::create(&part).map_err(|e| format!("{} could not be written: {e}", part.display()))?;
                        let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
                        let r = (|| -> Result<u64, String> {
                            use std::io::Write as _;
                            writeln!(out, "# {}", logsource::zone_label()).map_err(|e| e.to_string())?;
                            match what {
                                None => {
                                    for l in &shown {
                                        writeln!(out, "{l}").map_err(|e| e.to_string())?;
                                    }
                                    Ok(shown.len() as u64)
                                }
                                Some((n, today)) => {
                                    let list: Vec<Source> = match pick {
                                        Some(s) => vec![s],
                                        None => sources.clone(),
                                    };
                                    let mut total = 0;
                                    for s in list {
                                        if pick.is_none() {
                                            writeln!(out, "## {}", s.label()).map_err(|e| e.to_string())?;
                                        }
                                        total += save_fn(s, n, today, &mut out, &sink, &going)?;
                                    }
                                    Ok(total)
                                }
                            }
                        })();
                        let r = r.and_then(|n| out.into_inner().map_err(|e| e.to_string()).and_then(|f| f.sync_all().map_err(|e| e.to_string())).map(|_| n));
                        match r {
                            Ok(n) => {
                                std::fs::rename(&part, &path).map_err(|e| format!("{} could not be written: {e}", path.display()))?;
                                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(n);
                                sink(Progress::Done { message: format!("Saved to {} ({})", path.display(), mb(size)) });
                                Ok(())
                            }
                            Err(e) => {
                                let _ = std::fs::remove_file(&part);
                                Err(if e == "Cancelled." { "Cancelled: nothing was saved.".into() } else { format!("The log was not saved: {e}") })
                            }
                        }
                    },
                    cx,
                )
            });
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    /// Save log's line under the toolbar: its step and progress with
    /// Cancel while it runs; where it saved, or why not, when it ended.
    fn save_line(&self, theme: &gpui_kit::component::Theme, cx: &App) -> Option<AnyElement> {
        use crate::jobs::JobState;
        let job = self.save_job.read(cx);
        match job.state() {
            JobState::Running => {
                let detail = job.log().back().map(|l| l.to_string()).unwrap_or_default();
                let cancel = self.save_cancel.clone();
                Some(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .text_sm()
                        .child(div().text_color(theme.muted_foreground).child(format!("{}  {}", job.step_with_time(), detail)))
                        .child(crate::ui::button("log-save-cancel").outline().label("Cancel").on_click(move |_, _, _| cancel.store(true, std::sync::atomic::Ordering::SeqCst)))
                        .into_any_element(),
                )
            }
            JobState::Done(m) => Some(div().text_sm().child(m.to_string()).into_any_element()),
            JobState::Failed(m) => Some(div().text_sm().text_color(theme.danger).child(crate::remote::split_failure(m).0).into_any_element()),
            JobState::Idle => None,
        }
    }

    fn click(&mut self, i: usize, shift: bool, ctrl: bool, cx: &mut Context<Self>) {
        match (shift, self.anchor) {
            (true, Some(a)) => {
                self.selected = (a.min(i)..=a.max(i)).collect();
            }
            _ if ctrl => {
                if !self.selected.remove(&i) {
                    self.selected.insert(i);
                }
                self.anchor = Some(i);
            }
            _ => {
                if self.selected.len() == 1 && self.selected.contains(&i) {
                    self.selected.clear();
                } else {
                    self.selected = [i].into();
                }
                self.anchor = Some(i);
            }
        }
        // Reading a selection: stop the list moving under it.
        if !self.selected.is_empty() {
            self.follow = false;
        }
        cx.notify();
    }
}

/// The message as shown: without the level column, a level taken off its
/// front goes back in front of it.
fn shown_message(l: &logsource::Line, level_w: f32) -> String {
    if level_w > 0. || l.level == Level::Plain || l.message.starts_with("Failed:") || l.message.contains(" Failed: ") {
        return l.message.clone();
    }
    format!("{} {}", level_word(l.level), l.message)
}

fn level_word(l: Level) -> &'static str {
    match l {
        Level::Error => "ERROR",
        Level::Warn => "WARN",
        Level::Info => "INFO",
        Level::Debug => "DEBUG",
        Level::Plain => "",
    }
}

impl Render for LogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Opened from a service row's "Logs" on Status: that source.
        if let Some(s) = logsource::take_pick() {
            self.pick = Some(s);
            self.selected.clear();
            self.start(cx);
        }
        let theme = cx.theme().clone();
        let save_line = self.save_line(&theme, cx);
        let all = self.pick.is_none();
        let shown: Vec<Line> = self.shown(cx).into_iter().cloned().collect();
        let count = shown.len();
        let muted = theme.background.blend(theme.muted_foreground);
        let colour = |l: Level| match l {
            Level::Error => theme.danger,
            Level::Warn => theme.warning_foreground,
            Level::Debug => muted,
            _ => theme.foreground,
        };
        let tag_colour = |s: Source| -> Hsla {
            match s {
                Source::Sentient => theme.primary,
                Source::Database => rgb(0x3b82f6).into(),
                Source::ClickHouse => rgb(0xca8a04).into(),
                Source::Gateway => rgb(0x9333ea).into(),
            }
        };

        // Source: each one the install has, then All.
        let mut sources = ui::seg_track(&theme);
        for s in self.sources.clone() {
            sources = sources.child(ui::seg_item(&theme, SharedString::from(format!("log-src-{}", s.key())), s.label(), self.pick == Some(s)).on_click(cx.listener(move |v, _, _, cx| {
                v.pick = Some(s);
                v.selected.clear();
                v.start(cx);
                cx.notify();
            })));
        }
        if self.sources.len() > 1 {
            sources = sources.child(ui::seg_item(&theme, "log-src-all", "All", all).on_click(cx.listener(|v, _, _, cx| {
                v.pick = None;
                v.selected.clear();
                v.start(cx);
                cx.notify();
            })));
        }
        let levels = ui::seg_track(&theme).children([(Levels::All, "All levels"), (Levels::Warnings, "Warnings and errors"), (Levels::Errors, "Errors")].into_iter().map(|(l, label)| {
            ui::seg_item(&theme, SharedString::from(format!("log-lvl-{label}")), label, self.levels == l).on_click(cx.listener(move |v, _, _, cx| {
                v.levels = l;
                v.selected.clear();
                cx.notify();
            }))
        }));
        let this = cx.entity().downgrade();
        let save = crate::ui::button("log-save").outline().label("Save log").dropdown_menu(move |menu, _, _| {
            let opt = |label: &'static str, what: Option<(u32, bool)>| {
                let t = this.clone();
                PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    t.update(cx, |v, cx| v.save(what, cx)).ok();
                })
            };
            menu.item(opt("The lines shown", None))
                .item(opt("The last 1,000 lines", Some((1_000, false))))
                .item(opt("The last 10,000 lines", Some((10_000, false))))
                .item(opt("Today's lines", Some((1_000_000, true))))
        });
        let selected_n = self.selected.len();
        let toolbar = v_flex()
            .gap_2()
            .child(h_flex().gap_3().flex_wrap().items_center().when(!self.single, |r| r.child(sources)).child(levels))
            .child(
                h_flex()
                    .gap(crate::ui::button_gap())
                    .flex_wrap()
                    // A wrapped line of controls ends at the right too.
                    .justify_end()
                    .items_center()
                    .relative()
                    .child(ui::layout_probe("header-actions"))
                    .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!(
                        "{} · {}",
                        if count == 1 { "1 line".to_string() } else { format!("{count} lines") },
                        logsource::zone_label()
                    )))
                    .child(div().flex_1())
                    .child(div().w(px(280.)).child(ui::probed_input(Input::new(&self.filter))))
                    .child(Checkbox::new("log-follow").label("Follow").checked(self.follow).on_change(cx.listener(|v, on: &bool, _, cx| {
                        v.follow = *on;
                        if *on {
                            v.selected.clear();
                            v.start(cx);
                        }
                        cx.notify();
                    })))
                    .child(crate::ui::probed(crate::ui::button("log-refresh").outline().label("Refresh").on_click(cx.listener(|v, _, _, cx| v.start(cx)))))
                    .child(crate::ui::probed(crate::ui::button("log-copy").outline().label(if selected_n > 0 { format!("Copy {selected_n}") } else { "Copy".into() }).on_click(cx.listener(|v, _, _, cx| v.copy(cx)))))
                    .child(crate::ui::probed(save)),
            );
        let narrow = ui::narrow();
        // The level column only when the log has levels (SENTIENT's and the
        // services' do; the Platform Manager's activity log does not: its
        // failures are in the error colour instead).
        // A log is levelled when most of its lines are: the activity log
        // quotes tool output now and then ("WARNING:" from PostgreSQL), and
        // a few such lines must not open an empty column beside every other.
        let levelled = self.lines.iter().filter(|l| l.level != Level::Plain).count();
        let level_w = if levelled * 2 >= self.lines.len() && self.lines.iter().any(|l| matches!(l.level, Level::Info | Level::Warn | Level::Debug)) { 52. } else { 0. };
        let rows = shown.into_iter().enumerate().map(|(i, l)| {
            let sel = self.selected.contains(&i);
            // Narrow: time, source and level on one line, the message under
            // them at the full width.
            let head = h_flex()
                .gap_3()
                .child(div().w(px(150.)).flex_shrink_0().text_color(muted).child(l.time.clone()))
                .when(all, |r| {
                    let c = tag_colour(l.source);
                    r.child(h_flex().w(px(84.)).flex_shrink_0().child(div().px(px(6.)).rounded(px(4.)).bg(c.opacity(0.14)).text_color(c).text_size(px(11.5)).font_weight(FontWeight::MEDIUM).child(l.source.label())))
                })
                .when(level_w > 0., |r| r.child(div().w(px(level_w)).flex_shrink_0().font_weight(FontWeight::MEDIUM).text_color(colour(l.level)).child(level_word(l.level))));
            let message = div().flex_1().min_w_0().text_color(if l.level == Level::Error { theme.danger } else if l.level == Level::Debug { muted } else { theme.foreground }).child(shown_message(&l, level_w));
            if narrow {
                return v_flex()
                    .id(("log-line", i))
                    .px_2()
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(sel, |r| r.bg(theme.primary.opacity(0.14)))
                    .when(!sel, |r| r.hover(|r| r.bg(theme.list_hover)))
                    .child(head)
                    .child(message)
                    .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                        window.focus(&v.focus, cx);
                        let m = e.modifiers();
                        v.click(i, m.shift, m.control || m.platform, cx);
                    }));
            }
            let _ = (head, message);
            v_flex().child(h_flex()
                .id(("log-line", i))
                .gap_3()
                .items_start()
                .px_2()
                .py(px(1.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(sel, |r| r.bg(theme.primary.opacity(0.14)))
                .when(!sel, |r| r.hover(|r| r.bg(theme.list_hover)))
                .child(div().w(px(150.)).flex_shrink_0().text_color(muted).child(l.time.clone()))
                // In All: the source's name on its own soft colour.
                .when(all, |r| {
                    let c = tag_colour(l.source);
                    r.child(
                        h_flex().w(px(84.)).flex_shrink_0().child(
                            div().px(px(6.)).rounded(px(4.)).bg(c.opacity(0.14)).text_color(c).text_size(px(11.5)).font_weight(FontWeight::MEDIUM).child(l.source.label()),
                        ),
                    )
                })
                .when(level_w > 0., |r| r.child(div().w(px(level_w)).flex_shrink_0().font_weight(FontWeight::MEDIUM).text_color(colour(l.level)).child(level_word(l.level))))
                // Wrapped lines stay in the message column.
                .child(div().flex_1().min_w_0().text_color(if l.level == Level::Error { theme.danger } else if l.level == Level::Debug { muted } else { theme.foreground }).child(shown_message(&l, level_w)))
                .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                    window.focus(&v.focus, cx);
                    let m = e.modifiers();
                    v.click(i, m.shift, m.control || m.platform, cx);
                })))
                .id(("log-wrap", i))
        });
        let total = count;
        let fill = self.fill;
        v_flex()
            .gap_3()
            // On a page: the rest of the page's height. Its smallest height
            // is what it holds at the log box's FILL_MIN_H (the box below
            // has that as a fixed basis and grows from it).
            .when(fill, |v| v.flex_1())
            .child(toolbar)
            .children(self.status.clone().map(|s| div().text_sm().child(s)))
            .children(save_line)
            .child(
                // The box and its bar: the list scrolls itself (Follow scrolls it
                // from code), the shared bar is drawn over its edge (crate::theme:
                // shown near the edge and while it scrolls).
                div()
                    .relative()
                    .when(fill, |p| p.h(px(FILL_MIN_H)).flex_grow(1.).flex_shrink_0())
                    .when(!fill, |p| p.h(px(DIALOG_H)))
                    .child(ui::scroll_y_with(
                ui::panel(&theme)
                    .id("log-lines")
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(move |v, e: &KeyDownEvent, _, cx| {
                        let k = &e.keystroke;
                        let ctrl = k.modifiers.control || k.modifiers.platform;
                        if ctrl && k.key == "a" {
                            v.selected = (0..total).collect();
                            cx.notify();
                        } else if ctrl && k.key == "c" {
                            v.copy(cx);
                        } else if k.key == "escape" {
                            v.selected.clear();
                            cx.notify();
                        }
                    }))
                    .size_full()
                    .gap_0()
                    .p_2()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(12.5))
                    .when_some(self.error.clone().filter(|_| total == 0), |p, e| {
                        p.child(
                            v_flex().flex_1().items_center().justify_center().gap_3().font_family(theme.font_family.clone()).text_sm().child(div().text_color(theme.muted_foreground).child("The log could not be read.")).child(
                                div().max_w(px(560.)).text_color(theme.danger).child(e),
                            )
                            .child(crate::ui::button("log-retry").outline().label("Try again").on_click(cx.listener(|v, _, _, cx| {
                                v.error = None;
                                v.read_sources(cx);
                                v.start(cx);
                                cx.notify();
                            }))),
                        )
                    })
                    .when(total == 0 && self.error.is_none(), |p| {
                        p.child(
                            div().flex_1().flex().items_center().justify_center().font_family(theme.font_family.clone()).text_sm().text_color(theme.muted_foreground).child(
                                if self.lines.is_empty() { "No log lines" } else { "No lines match the filter" },
                            ),
                        )
                    })
                    .children(rows),
                    &self.scroll,
                    )),
            )
    }
}

/// This computer's Logs page.
pub struct LogsPage {
    view: Option<Entity<LogView>>,
}

impl LogsPage {
    /// Reopen tests: the page shows what it read (no installation: its
    /// "not installed" notice, nothing to read).
    pub fn has_data(&self, cx: &App) -> bool {
        self.view.as_ref().is_none_or(|v| v.read(cx).has_data())
    }

    pub fn new(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A gateway-only computer still has the gateway's log.
        let view = (record.is_some() || logsource::gateway_here()).then(|| cx.new(|cx| LogView::local(record.clone(), window, cx).filling()));
        Self { view }
    }
}

impl Render for LogsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(&theme, "Logs", "SENTIENT's log and its database's, newest at the bottom.");
        match &self.view {
            Some(v) => page.child(v.clone()),
            None => page.child(ui::not_installed(&theme)),
        }
    }
}

#[cfg(test)]
mod reopen_tests {
    use super::{take_kept, Kept, Levels, Source};

    fn kept(key: &str) -> Kept {
        Kept { key: key.into(), sources: vec![Source::Sentient], pick: Some(Source::Sentient), levels: Levels::All, lines: Vec::new(), follow: true }
    }

    /// Cold start: nothing shown in this run, the first read fills the view.
    #[test]
    fn a_cold_start_has_nothing_to_draw() {
        assert!(take_kept(&mut None, "{}").is_none());
    }

    /// A window opened again: the lines shown before, once.
    #[test]
    fn a_reopened_window_draws_the_lines_shown() {
        let mut slot = Some(kept("rec"));
        assert!(take_kept(&mut slot, "rec").is_some());
        assert!(slot.is_none());
    }

    /// The installation changed meanwhile: read afresh.
    #[test]
    fn another_installation_is_read_afresh() {
        assert!(take_kept(&mut Some(kept("rec")), "other").is_none());
    }
}

/// A server's log saved here: the server writes it gzipped to a temporary
/// file (`logs-file`, as it comes), this downloads that over SFTP with its
/// progress and unpacks it into `out`, then the file there is deleted. A
/// Windows server answers the read as before (its log is bounded).
fn remote_save(inst: &crate::remote::Instance, s: Source, n: u32, today: bool, out: &mut dyn std::io::Write, sink: &sentient_installer_core::progress::ProgressFn, going: &dyn Fn(u64) -> bool) -> Result<u64, String> {
    use crate::manager::files::fs::Fs as _;
    use sentient_installer_core::progress::Progress;
    if crate::remote::is_windows(inst) {
        sink(Progress::Step { name: format!("Reading the log on {}", inst.name) });
        return save_from_load(Arc::new({
            let inst = inst.clone();
            move |s, n, today| crate::remote::logs_from(&inst, s, n, today)
        }))(s, n, today, out, sink, going);
    }
    sink(Progress::Step { name: format!("Reading the log on {}", inst.name) });
    let (path, bytes, packed) = crate::remote::logs_file(inst, s, n, today)?;
    let result = (|| {
        if !going(0) {
            return Err("Cancelled.".to_string());
        }
        sink(Progress::Step { name: "Downloading".into() });
        sink(Progress::Log { line: format!("{} of log, {} to download", mb(bytes), mb(packed)) });
        let mut fs = crate::manager::files::fs::RemoteFs::connect(inst, false)?;
        let mut dec = flate2::write::GzDecoder::new(out);
        let mut got = 0u64;
        let mut failed: Option<String> = None;
        let mut last = std::time::Instant::now();
        fs.read(&path, 0, packed, &mut |chunk| {
            if let Err(e) = std::io::Write::write_all(&mut dec, chunk) {
                failed = Some(format!("writing the log: {e}"));
                return false;
            }
            got += chunk.len() as u64;
            if packed > 0 && last.elapsed() > Duration::from_millis(250) {
                last = std::time::Instant::now();
                sink(Progress::Percent { value: got as f32 / packed as f32 });
                sink(Progress::Log { line: format!("Downloaded {} of {}", mb(got), mb(packed)) });
            }
            going(got)
        })?;
        if let Some(e) = failed {
            return Err(e);
        }
        if !going(got) {
            return Err("Cancelled.".to_string());
        }
        dec.finish().map_err(|e| format!("the downloaded log is damaged: {e}"))?;
        Ok(bytes)
    })();
    crate::remote::logs_file_remove(inst, &path);
    result
}

/// Bytes in megabytes, for the progress line.
fn mb(b: u64) -> String {
    if b < 1_000_000 { format!("{:.0} KB", b as f64 / 1_000.) } else { format!("{:.1} MB", b as f64 / 1_000_000.) }
}
