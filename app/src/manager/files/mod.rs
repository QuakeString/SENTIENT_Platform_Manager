// Copyright © 2016-2026 The SENTIENT Authors

//! Files (mockup C1–C7): this computer on the left, the open instance on the
//! right (over SFTP on its SSH sign-in). With this computer open there is one
//! pane, full width (two of the same computer made no sense, his rc12-22
//! note): copy, cut and paste work inside it. Drag between the panes, Ctrl+C / Ctrl+V (folders too), rename,
//! new folder and delete with a confirmation. Copies run in the background
//! (crate::manager::files::transfers). A folder the account may not change
//! is read only, with "Open as administrator" on a server (sudo, its
//! password asked once).
//! Back, Forward and Up beside the path bar keep a history per pane; the
//! bar turns into a text field for a typed path (Ctrl+L), checked where the
//! pane's files are before the pane goes there.

pub(crate) mod fs;
pub(crate) mod transfers;

use gpui_kit::component::scroll::ScrollableElement as _;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::fs::{Entry, Fs, Listing, Place, Shortcut};
use self::transfers::{Request, Transfers};
use self::transfers::size_label as bytes_label;
use crate::ui::{self, Tone};

const ROW_H: f32 = 34.;
/// Size, Modified and Owner: their widths until dragged.
const COLS: [f32; 3] = [84., 110., 70.];
/// The Name column never gets narrower than this; the others go first
/// (Owner, then Size, then Modified).
const NAME_MIN: f32 = 160.;
const COL_GAP: f32 = 8.;
/// A pane is never narrower than this beside the other.
const PANE_MIN: f32 = 320.;
/// The splitter between the panes.
const GUTTER: f32 = crate::theme::PAGE_GAP;
/// The panes' side padding (the layout test's card padding).
const PANE_PX: f32 = 16.;

/// What Ctrl+C took.
#[derive(Clone)]
struct Clip {
    place: Place,
    admin: bool,
    dir: String,
    names: Vec<String>,
    /// Cut: the paste moves (a rename on the same computer).
    cut: bool,
}

struct ClipSlot(Option<Clip>);
impl Global for ClipSlot {}

/// What a drag carries (and draws by the pointer, C2).
#[derive(Clone)]
struct Drag {
    side: usize,
    names: Vec<String>,
    dir: bool,
}

impl Render for Drag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_card(cx.theme(), &self.names, self.dir)
    }
}

fn drag_card(theme: &gpui_kit::component::Theme, names: &[String], dir: bool) -> Div {
    h_flex()
        .w(px(260.))
        .p(px(16.))
        .gap(px(16.))
        .items_center()
        .rounded(px(8.))
        .bg(theme.background)
        .border_1()
        .border_color(theme.primary)
        .shadow_lg()
        .child(file_icon(dir).size(px(48.)).text_color(if dir { folder_color() } else { theme.foreground }))
        .child(div().flex_1().min_w_0().text_sm().child(names.first().cloned().unwrap_or_default()))
        .when(names.len() > 1, |c| {
            c.child(div().flex_shrink_0().px(px(7.)).rounded_full().bg(theme.primary).text_color(theme.primary_foreground).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(names.len().to_string()))
        })
}

struct Pane {
    place: Place,
    admin: bool,
    /// Where "Open as administrator" was asked: leaving it drops the rights,
    /// so nothing elsewhere is made as root by accident.
    admin_root: String,
    conn: Arc<Mutex<Option<Box<dyn Fs>>>>,
    path: String,
    listing: Option<Listing>,
    loading: bool,
    error: Option<String>,
    shortcuts: Vec<Shortcut>,
    selected: BTreeSet<String>,
    anchor: Option<usize>,
    focus: FocusHandle,
    rename: Option<(String, Entity<InputState>)>,
    new_folder: Option<Entity<InputState>>,
    seq: u64,
    /// Folders listed (or read ahead) in the last CACHE_FOR, by path: shown
    /// at once when opened, while they are listed again behind.
    seen: std::collections::HashMap<String, (Listing, std::time::Instant)>,
    /// The folder last loaded, while another one loads in its place (the
    /// path bar already shows the new one): it comes back when that fails.
    back: Option<Listing>,
    /// Requests of the page waiting for the connection: reading ahead
    /// gives way to them.
    waiting: Arc<std::sync::atomic::AtomicUsize>,
    /// Reading ahead's round: a navigation moves it on, which stops it.
    ahead: Arc<std::sync::atomic::AtomicU64>,
    /// A re-list failed while the listing read before still shows.
    stale: Option<String>,
    /// When the listing last came back (HH:MM).
    updated_at: Option<String>,
    /// Size, Modified, Owner widths (dragged in the header).
    cols: [f32; 3],
    /// The pane's width as last drawn (which columns fit).
    width: Rc<std::cell::Cell<f32>>,
    /// Back and Forward: the folders left, newest last.
    hist: Vec<String>,
    fwd: Vec<String>,
    /// The path bar as a text field (a click on its empty part, Ctrl+L).
    edit: Option<Entity<InputState>>,
    /// Why the typed path did not open (the field stays, the folder too).
    edit_error: Option<String>,
    /// The typed path is being checked (on the server, for a server).
    checking: bool,
}

impl Pane {
    fn new(place: Place, cx: &mut Context<FilesPage>) -> Pane {
        Pane {
            place,
            admin: false,
            admin_root: String::new(),
            conn: Arc::new(Mutex::new(None)),
            path: String::new(),
            listing: None,
            loading: true,
            error: None,
            shortcuts: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            focus: cx.focus_handle(),
            rename: None,
            new_folder: None,
            seq: 0,
            seen: Default::default(),
            back: None,
            waiting: Default::default(),
            ahead: Default::default(),
            stale: None,
            updated_at: None,
            cols: COLS,
            width: Rc::new(std::cell::Cell::new(0.)),
            hist: Vec::new(),
            fwd: Vec::new(),
            edit: None,
            edit_error: None,
            checking: false,
        }
    }

    /// The listing's entries as shown: folders first, then by name (hidden
    /// ones last); the
    /// parts of copies under way are hidden.
    fn rows(&self) -> Vec<Entry> {
        let mut rows: Vec<Entry> = self.listing.as_ref().map(|l| l.entries.iter().filter(|e| !e.name.ends_with(".spm-part")).cloned().collect()).unwrap_or_default();
        // Hidden names (".env") after the others, in each group.
        rows.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.starts_with('.').cmp(&b.name.starts_with('.'))).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        rows
    }

    /// May change things here (nothing while a folder loads).
    fn writable(&self) -> bool {
        self.listing.as_ref().is_some_and(|l| self.admin || l.writable)
    }

    /// A folder's listing from the last CACHE_FOR.
    fn cached(&self, path: &str) -> Option<Listing> {
        self.seen.get(path).filter(|(_, at)| at.elapsed() < CACHE_FOR).map(|(l, _)| l.clone())
    }

    /// Which of Size, Modified and Owner fit beside a Name of NAME_MIN:
    /// Owner goes first, then Size, then Modified.
    fn shown(&self) -> [bool; 3] {
        // Owner: a Linux server's account names; a Windows server's OpenSSH
        // gives none (only made-up POSIX ids).
        let owners = matches!(&self.place, Place::Remote(i) if !crate::remote::is_windows(i));
        let mut on = [true, true, owners];
        let w = self.width.get();
        if w <= 0. {
            return on;
        }
        let inner = w - 2. * PANE_PX;
        for drop in [2, 0, 1] {
            let need = NAME_MIN + (0..3).filter(|&i| on[i]).map(|i| self.cols[i] + COL_GAP).sum::<f32>();
            if need <= inner {
                break;
            }
            on[drop] = false;
        }
        on
    }

    fn label(&self) -> String {
        match &self.place {
            Place::Local => "This computer".into(),
            Place::Remote(i) => i.name.clone(),
        }
    }
}

pub struct FilesPage {
    panes: [Pane; 2],
    /// This computer is the open instance: one pane (the first) only.
    single: bool,
    drag_over: Option<usize>,
    /// How many items the drag carries.
    drag_count: usize,
    transfers: Entity<Transfers>,
    /// The left pane's share of the width; the splitter is being dragged.
    split: f32,
    splitting: bool,
    /// The short note at the bottom ("Location copied"), and since when.
    toast: Option<(String, std::time::Instant)>,
    /// A typed path opened: the keyboard back to its pane.
    refocus: Option<(usize, FocusHandle)>,
    /// Screens: the path field open on a side, a path typed in it
    /// (SPM_DEBUG_FILES_EDIT=<side>[:<typed path>]).
    debug_edit: Option<(usize, String)>,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl FilesPage {
    /// `remote`: the open instance (else this computer on both sides).
    pub fn new(remote: Option<crate::remote::Instance>, cx: &mut Context<Self>) -> Self {
        let transfers = transfers::get(cx);
        let subs = vec![cx.subscribe(&transfers, |this: &mut Self, _, e: &transfers::Changed, cx| {
            for side in 0..2 {
                let (key, path) = (this.panes[side].place.key(), this.panes[side].path.clone());
                if e.0.iter().any(|(k, d)| *k == key && *d == path) {
                    this.load(side, path.clone(), None, cx);
                }
            }
        }), cx.observe(&transfers, |_, _, cx| cx.notify())];
        let single = remote.is_none();
        let right = remote.map(Place::Remote).unwrap_or(Place::Local);
        let mut me = FilesPage { panes: [Pane::new(Place::Local, cx), Pane::new(right, cx)], single, drag_over: None, drag_count: 0, transfers, split: 0.5, splitting: false, toast: None, refocus: None, debug_edit: None, _subs: subs, _tasks: Vec::new() };
        let saved = crate::manager::session::load();
        me.split = saved.files_split.unwrap_or(0.5).clamp(0.1, 0.9);
        if let Some(c) = saved.files_cols {
            for side in 0..2 {
                me.panes[side].cols = c[side].map(|w| w.clamp(48., 320.));
            }
        }
        // Screens: a split (SPM_DEBUG_FILES_SPLIT=0.3).
        #[cfg(debug_assertions)]
        if let Some(r) = std::env::var("SPM_DEBUG_FILES_SPLIT").ok().and_then(|v| v.parse::<f32>().ok()) {
            me.split = r.clamp(0.1, 0.9);
        }
        for side in 0..2 {
            // Click tests: where each side starts (SPM_DEBUG_FILES_START=<left>|<right>).
            #[cfg(debug_assertions)]
            let start = std::env::var("SPM_DEBUG_FILES_START").ok().and_then(|v| v.split('|').nth(side).map(String::from)).unwrap_or_default();
            #[cfg(not(debug_assertions))]
            let start = String::new();
            // Else the folder last open on that side's computer.
            let start = if start.is_empty() { saved.files_dirs.get(&me.panes[side].place.key()).cloned().unwrap_or_default() } else { start };
            if single && side == 1 {
                continue;
            }
            me.load(side, start, None, cx);
        }
        #[cfg(debug_assertions)]
        me.debug_state(cx);
        me
    }

    /// Screens: a selection and a state (SPM_DEBUG_FILES_SELECT=1:a+b and
    /// SPM_DEBUG_FILES_STATE=drag|copying|ask|delete).
    #[cfg(debug_assertions)]
    fn debug_state(&mut self, cx: &mut Context<Self>) {
        if let Ok(v) = std::env::var("SPM_DEBUG_FILES_EDIT") {
            let (side, typed) = v.split_once(':').unwrap_or((v.as_str(), ""));
            self.debug_edit = Some((side.parse().unwrap_or(0usize).min(1), typed.to_string()));
        }
        if let Ok(sel) = std::env::var("SPM_DEBUG_FILES_SELECT") {
            for part in sel.split(';') {
                if let Some((side, names)) = part.split_once(':') {
                    let side: usize = side.parse().unwrap_or(0).min(1);
                    self.panes[side].selected = names.split('+').map(String::from).collect();
                }
            }
        }
        match std::env::var("SPM_DEBUG_FILES_STATE").as_deref() {
            Ok("drag") => self.drag_over = Some(1),
            Ok("copying") => {
                let to = self.panes[1].place.clone();
                self.transfers.update(cx, |t, cx| t.debug_fill(to, cx));
            }
            Ok("ask") => cx.defer(transfers::debug_ask),
            Ok("delete") => {
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                    this.update(cx, |p, cx| p.confirm_delete(1, cx)).ok();
                });
                self._tasks.push(task);
            }
            _ => {}
        }
    }

    /// Run `op` on a side's connection (made when there is none), then
    /// `then` here.
    fn run<T: Send + 'static>(
        &mut self,
        side: usize,
        op: impl FnOnce(&mut dyn Fs) -> Result<T, String> + Send + 'static,
        then: impl FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.run_held(side, None, op, then, cx);
    }

    /// [`Self::run`], `hold` later (the click test's slow listing).
    fn run_held<T: Send + 'static>(
        &mut self,
        side: usize,
        hold: Option<std::time::Duration>,
        op: impl FnOnce(&mut dyn Fs) -> Result<T, String> + Send + 'static,
        then: impl FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        use std::sync::atomic::Ordering::SeqCst;
        let p = &self.panes[side];
        let (conn, place, admin, waiting) = (p.conn.clone(), p.place.clone(), p.admin, p.waiting.clone());
        waiting.fetch_add(1, SeqCst);
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    if let Some(d) = hold {
                        std::thread::sleep(d);
                    }
                    let g = conn.lock();
                    waiting.fetch_sub(1, SeqCst);
                    let mut g = g.map_err(|_| "The connection is busy.".to_string())?;
                    if g.is_none() {
                        *g = Some(fs::open(&place, admin)?);
                    }
                    let f = g.as_mut().expect("opened");
                    let r = op(f.as_mut());
                    if let Err(e) = &r {
                        if f.broken(e) {
                            *g = None;
                        }
                    }
                    r
                })
                .await;
            this.update(cx, |p, cx| {
                then(p, r, cx);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Shown again after another instance was open: both sides are listed
    /// again behind what shows.
    pub fn reshow(&mut self, cx: &mut Context<Self>) {
        for side in 0..2 {
            if !self.panes[side].loading {
                self.reload(side, cx);
            }
        }
    }

    /// List `path` (empty: home), and pick `select` in it. Another folder
    /// shows in the path bar at once: its listing from the last CACHE_FOR
    /// with it, else the skeleton rows until it comes. The newest request
    /// wins (`seq`); one that fails goes back to the folder last loaded,
    /// the reason under the bar.
    fn load(&mut self, side: usize, path: String, select: Option<String>, cx: &mut Context<Self>) {
        let pane = &mut self.panes[side];
        if pane.admin && !(path.starts_with(&pane.admin_root) && !path.is_empty()) {
            pane.admin = false;
            pane.conn = Arc::new(Mutex::new(None));
        }
        pane.seq += 1;
        let seq = pane.seq;
        pane.loading = true;
        pane.error = None;
        // Reading ahead stops: this request goes first.
        pane.ahead.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let here = pane.listing.as_ref().is_some_and(|l| l.path == path);
        if !here && !path.is_empty() {
            if let Some(l) = pane.listing.take() {
                pane.back = Some(l);
            }
            pane.selected.clear();
            pane.anchor = None;
            pane.rename = None;
            pane.path = path.clone();
            pane.listing = pane.cached(&path);
        }
        let asked = path.clone();
        let first = pane.shortcuts.is_empty();
        crate::timing::mark(&format!("files-nav side={side} path={path}"));
        let t0 = std::time::Instant::now();
        let conns0 = fs::CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed);
        self.run_held(
            side,
            debug_hold(),
            move |f| {
                let c0 = f.counters();
                let path = if path.is_empty() { f.home() } else { path };
                let shortcuts = if first { f.shortcuts() } else { Vec::new() };
                let t = std::time::Instant::now();
                let l = f.list(&path);
                let c1 = f.counters();
                Ok((l, shortcuts, [c1[0] - c0[0], c1[1] - c0[1], c1[2] - c0[2], t.elapsed().as_micros() as u64, t0.elapsed().as_micros() as u64]))
            },
            move |this, r: Result<(Result<Listing, String>, Vec<Shortcut>, [u64; 5]), String>, cx| {
                let r = r.and_then(|(l, s, c)| {
                    let conns = fs::CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed) - conns0;
                    crate::timing::mark(&format!("files-listed side={side} ok={} n={} took_ms={} list_ms={} done_ms={} rtts={} bytes_in={} bytes_out={} conns={conns} current={}", l.is_ok(), l.as_ref().map(|l| l.entries.len()).unwrap_or(0), t0.elapsed().as_millis(), c[3] / 1000, c[4] / 1000, c[0], c[1], c[2], this.panes[side].seq == seq));
                    l.map(|l| (l, s))
                });
                let pane = &mut this.panes[side];
                if pane.seq != seq {
                    // A newer request: this one is dropped (kept to show
                    // at once if its folder is opened again).
                    if let Ok((l, _)) = r {
                        pane.seen.insert(l.path.clone(), (l, std::time::Instant::now()));
                    }
                    return;
                }
                pane.loading = false;
                match r {
                    Ok((l, shortcuts)) => {
                        let first = !shortcuts.is_empty();
                        if first {
                            pane.shortcuts = shortcuts;
                        }
                        if !pane.path.is_empty() && pane.path != l.path {
                            pane.selected.clear();
                            pane.anchor = None;
                            pane.rename = None;
                        }
                        pane.back = None;
                        pane.path = l.path.clone();
                        crate::manager::session::set_files_dir(&pane.place.key(), &l.path);
                        let names: BTreeSet<String> = l.entries.iter().map(|e| e.name.clone()).collect();
                        pane.selected.retain(|n| names.contains(n));
                        if let Some(s) = select.filter(|s| names.contains(s)) {
                            pane.selected = [s].into();
                        }
                        let now = std::time::Instant::now();
                        pane.seen.insert(l.path.clone(), (l.clone(), now));
                        if !asked.is_empty() && asked != l.path {
                            pane.seen.insert(asked, (l.clone(), now));
                        }
                        pane.stale = None;
                        pane.updated_at = Some(chrono::Local::now().format("%H:%M").to_string());
                        // Read ahead: on opening, the place buttons' folders
                        // and the folders above; every time, the folders in it.
                        let mut ahead: Vec<String> = Vec::new();
                        if first {
                            ahead.extend(pane.shortcuts.iter().map(|(_, _, p)| p.clone()));
                            ahead.extend(pane.place.crumbs(&l.path).into_iter().map(|(_, p)| p).filter(|p| *p != l.path));
                        }
                        ahead.extend(l.entries.iter().filter(|e| e.dir && !e.name.ends_with(".spm-part")).take(AHEAD_MAX).map(|e| pane.place.join(&l.path, &e.name)));
                        pane.listing = Some(l);
                        this.read_ahead(side, ahead, cx);
                    }
                    // The folder shown, listed again, and no other one
                    // asked: it stays, said softly to be stale.
                    Err(e) if pane.back.is_none() && pane.listing.as_ref().is_some_and(|l| l.path == asked) => pane.stale = Some(e),
                    // Back to the folder last loaded; the reason under the bar.
                    Err(e) => {
                        pane.seen.remove(&asked);
                        if let Some(l) = pane.back.take() {
                            pane.path = l.path.clone();
                            pane.listing = Some(l);
                            // Back where it was: the step there is not one.
                            if pane.hist.last() == Some(&pane.path) {
                                pane.hist.pop();
                            }
                        } else {
                            pane.listing = None;
                        }
                        pane.error = Some(e);
                    }
                }
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }

    /// Read these folders' listings into the cache behind the page, one at
    /// a time over the side's connection (never a new one), giving way to
    /// the page's own requests; a navigation stops it. Folders cached in
    /// the last CACHE_FOR are skipped.
    fn read_ahead(&mut self, side: usize, paths: Vec<String>, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering::SeqCst;
        // Click tests: every folder opened uncached (the skeleton rows).
        if cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_FILES_NO_AHEAD").is_some() {
            return;
        }
        let pane = &self.panes[side];
        let mut todo: Vec<String> = Vec::new();
        for p in paths {
            if pane.cached(&p).is_none() && !todo.contains(&p) {
                todo.push(p);
            }
        }
        todo.truncate(AHEAD_MAX + 8);
        if todo.is_empty() {
            return;
        }
        let round = pane.ahead.fetch_add(1, SeqCst) + 1;
        let (conn, waiting, ahead) = (pane.conn.clone(), pane.waiting.clone(), pane.ahead.clone());
        let task = cx.spawn(async move |this, cx| {
            let t0 = std::time::Instant::now();
            let (mut n, mut tot) = (0usize, [0u64; 3]);
            let mut i = 0;
            while i < todo.len() {
                if ahead.load(SeqCst) != round {
                    break;
                }
                let (conn, waiting, ahead2, path) = (conn.clone(), waiting.clone(), ahead.clone(), todo[i].clone());
                let r = cx
                    .background_executor()
                    .spawn(async move {
                        // The page's own request goes first.
                        if waiting.load(SeqCst) > 0 || ahead2.load(SeqCst) != round {
                            return None;
                        }
                        let mut g = conn.lock().ok()?;
                        if ahead2.load(SeqCst) != round {
                            return None;
                        }
                        let f = g.as_mut()?;
                        let c0 = f.counters();
                        let l = f.list(&path);
                        let c1 = f.counters();
                        Some((l, [c1[0] - c0[0], c1[1] - c0[1], c1[2] - c0[2]]))
                    })
                    .await;
                let Some((l, c)) = r else {
                    if ahead.load(SeqCst) != round {
                        break;
                    }
                    cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
                    continue;
                };
                n += 1;
                for k in 0..3 {
                    tot[k] += c[k];
                }
                if let Ok(l) = l {
                    let stop = this
                        .update(cx, |p, _| {
                            let pane = &mut p.panes[side];
                            pane.seen.insert(l.path.clone(), (l, std::time::Instant::now()));
                            pane.ahead.load(SeqCst) != round
                        })
                        .unwrap_or(true);
                    if stop {
                        break;
                    }
                }
                i += 1;
            }
            crate::timing::mark(&format!("files-ahead side={side} folders={n} of={} took_ms={} rtts={} bytes_in={} bytes_out={}", todo.len(), t0.elapsed().as_millis(), tot[0], tot[1], tot[2]));
        });
        self._tasks.push(task);
    }

    fn reload(&mut self, side: usize, cx: &mut Context<Self>) {
        let path = self.panes[side].path.clone();
        self.load(side, path, None, cx);
    }

    fn open_dir(&mut self, side: usize, name: &str, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        let path = p.place.join(&p.path, name);
        self.go(side, path, None, cx);
    }

    /// Open another folder as a step Back can undo (a folder opened, a
    /// crumb, a chip, Up, a typed path); Forward's steps are dropped.
    fn go(&mut self, side: usize, path: String, select: Option<String>, cx: &mut Context<Self>) {
        let pane = &mut self.panes[side];
        if !pane.path.is_empty() && pane.path != path {
            if pane.hist.last() != Some(&pane.path) {
                pane.hist.push(pane.path.clone());
            }
            pane.fwd.clear();
        }
        self.cancel_edit(side, cx);
        self.load(side, path, select, cx);
    }

    fn go_back(&mut self, side: usize, cx: &mut Context<Self>) {
        let pane = &mut self.panes[side];
        let Some(to) = pane.hist.pop() else { return };
        if !pane.path.is_empty() {
            pane.fwd.push(pane.path.clone());
        }
        let from = Self::name_in(&pane.path, &to);
        self.cancel_edit(side, cx);
        self.load(side, to, from, cx);
    }

    fn go_forward(&mut self, side: usize, cx: &mut Context<Self>) {
        let pane = &mut self.panes[side];
        let Some(to) = pane.fwd.pop() else { return };
        if !pane.path.is_empty() {
            pane.hist.push(pane.path.clone());
        }
        self.cancel_edit(side, cx);
        self.load(side, to, None, cx);
    }

    /// Back to a folder above: the one it came from is picked there.
    fn name_in(from: &str, to: &str) -> Option<String> {
        let rest = from.strip_prefix(to)?.trim_start_matches(['/', '\\']);
        rest.split(['/', '\\']).next().filter(|n| !n.is_empty()).map(String::from)
    }

    fn up(&mut self, side: usize, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        if let Some(up) = p.place.parent(&p.path) {
            let from = p.path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().map(String::from);
            self.go(side, up, from, cx);
        }
    }

    /// The path bar as a text field with the full path, all of it picked.
    fn start_edit(&mut self, side: usize, window: &mut Window, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        if p.edit.is_some() || p.path.is_empty() {
            return;
        }
        let windows = matches!(&p.place, Place::Remote(i) if crate::remote::is_windows(i));
        let text = os_path(&p.path, windows);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
        let sub = cx.subscribe_in(&input, window, move |this: &mut Self, input, e: &InputEvent, window, cx| match e {
            InputEvent::PressEnter { .. } => {
                let typed = input.read(cx).value().to_string();
                this.submit_edit(side, typed, window, cx);
            }
            InputEvent::Change => {
                if this.panes[side].edit_error.take().is_some() {
                    cx.notify();
                }
            }
            // Clicked away: the bar again (not while the path is checked).
            InputEvent::Blur if !this.panes[side].checking => this.cancel_edit(side, cx),
            _ => {}
        });
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self._subs.push(sub);
        let pane = &mut self.panes[side];
        pane.edit = Some(input);
        pane.edit_error = None;
        cx.notify();
    }

    fn cancel_edit(&mut self, side: usize, cx: &mut Context<Self>) {
        let pane = &mut self.panes[side];
        if pane.edit.take().is_some() || pane.edit_error.take().is_some() {
            pane.checking = false;
            cx.notify();
        }
    }

    /// Enter in the path field: the typed folder is listed (on the server,
    /// for a server) before the pane goes there; one that is not there or
    /// may not be read keeps the field and the folder, the reason under it.
    fn submit_edit(&mut self, side: usize, typed: String, window: &mut Window, cx: &mut Context<Self>) {
        let p = &mut self.panes[side];
        if p.checking {
            return;
        }
        let server = match &p.place {
            Place::Local => None,
            Place::Remote(i) => Some(crate::remote::is_windows(i)),
        };
        let cur = p.path.clone();
        p.checking = true;
        p.edit_error = None;
        let focus = p.focus.clone();
        cx.notify();
        let _ = window;
        self.run(
            side,
            move |f| {
                let home = f.home();
                let path = fs::typed_path(&typed, &cur, &home, server)?;
                let l = f.list(&path)?;
                Ok(l)
            },
            move |this, r, cx| {
                let pane = &mut this.panes[side];
                pane.checking = false;
                if pane.edit.is_none() {
                    // Cancelled while it was checked.
                    return;
                }
                match r {
                    Ok(l) => {
                        let path = l.path.clone();
                        pane.seen.insert(path.clone(), (l, std::time::Instant::now()));
                        this.go(side, path, None, cx);
                        this.refocus = Some((side, focus));
                    }
                    Err(e) => pane.edit_error = Some(e),
                }
            },
            cx,
        );
    }

    /// The second press of a double click on a folder opens it, at once:
    /// the click itself is lost when the pointer moves more than GPUI's 2 px
    /// drag threshold between press and release (rows are drag sources),
    /// which a real hand often does.
    fn press_row(&mut self, side: usize, i: usize, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !opens_on_press(e.click_count, e.modifiers) {
            return;
        }
        let Some(row) = self.panes[side].rows().into_iter().nth(i) else { return };
        if row.dir {
            window.focus(&self.panes[side].focus, cx);
            self.open_dir(side, &row.name, cx);
            cx.stop_propagation();
        }
    }

    /// A row clicked: select it (Ctrl adds, Shift a range); twice opens a folder.
    fn click_row(&mut self, side: usize, i: usize, e: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.panes[side].rows();
        let Some(row) = rows.get(i) else { return };
        let pane = &mut self.panes[side];
        window.focus(&pane.focus, cx);
        if e.click_count() >= 2 {
            // Opened on the second press already (press_row): a hand that
            // moves a little between down and up starts a drag, and the
            // click then never comes.
            return;
        }
        let m = e.modifiers();
        if m.shift {
            let a = pane.anchor.unwrap_or(i);
            let (lo, hi) = (a.min(i), a.max(i));
            pane.selected = rows[lo..=hi].iter().map(|r| r.name.clone()).collect();
        } else if m.control || m.platform {
            if !pane.selected.remove(&row.name) {
                pane.selected.insert(row.name.clone());
            }
            pane.anchor = Some(i);
        } else {
            pane.selected = [row.name.clone()].into();
            pane.anchor = Some(i);
        }
        cx.notify();
    }

    /// Copy the names from one side into the other side's folder (or the
    /// clipboard's into this one).
    fn copy(&mut self, from: usize, names: Vec<String>, to: usize, cx: &mut Context<Self>) {
        if names.is_empty() {
            return;
        }
        if !self.panes[to].writable() {
            ui::show_error("This folder is read only", read_only_text(&self.panes[to]), cx);
            return;
        }
        let (f, t) = (&self.panes[from], &self.panes[to]);
        let r = Request { from: f.place.clone(), from_admin: f.admin, from_dir: f.path.clone(), names, to: t.place.clone(), to_admin: t.admin, to_dir: t.path.clone() };
        self.transfers.update(cx, |q, cx| q.add(r, cx));
    }

    fn clip_copy(&mut self, side: usize, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        if p.selected.is_empty() {
            return;
        }
        cx.set_global(ClipSlot(Some(Clip { place: p.place.clone(), admin: p.admin, dir: p.path.clone(), names: p.selected.iter().cloned().collect(), cut: false })));
        cx.notify();
    }

    /// Cut: Paste then moves the items (on the same computer; from one
    /// computer to another it copies and the originals stay).
    fn clip_cut(&mut self, side: usize, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        if p.selected.is_empty() || !p.writable() {
            return;
        }
        cx.set_global(ClipSlot(Some(Clip { place: p.place.clone(), admin: p.admin, dir: p.path.clone(), names: p.selected.iter().cloned().collect(), cut: true })));
        cx.notify();
    }

    /// Copy location: the item's full path, written the way its side's
    /// system writes paths (a Windows server's "/C:/Users/x" as
    /// "C:\\Users\\x"), to this computer's clipboard; a short note says so.
    fn copy_location(&mut self, side: usize, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        let mut names: Vec<String> = p.selected.iter().cloned().collect();
        names.sort();
        if names.is_empty() {
            return;
        }
        let windows = matches!(&p.place, Place::Remote(i) if crate::remote::is_windows(i));
        let paths: Vec<String> = names.iter().map(|n| os_path(&p.place.join(&p.path, n), windows)).collect();
        cx.write_to_clipboard(ClipboardItem::new_string(paths.join("\n")));
        let text = if paths.len() == 1 { "Location copied".to_string() } else { format!("{} locations copied", paths.len()) };
        self.show_toast(text, cx);
    }

    fn show_toast(&mut self, text: String, cx: &mut Context<Self>) {
        self.toast = Some((text, std::time::Instant::now()));
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            this.update(cx, |p, cx| {
                if p.toast.as_ref().is_some_and(|(_, at)| at.elapsed() >= TOAST_FOR) {
                    p.toast = None;
                    cx.notify();
                }
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn clip_paste(&mut self, side: usize, cx: &mut Context<Self>) {
        let Some(c) = cx.try_global::<ClipSlot>().and_then(|c| c.0.clone()) else { return };
        let t = &self.panes[side];
        if !t.writable() {
            ui::show_error("This folder is read only", read_only_text(t), cx);
            return;
        }
        // A cut on the same computer and account: move by renaming.
        if c.cut && c.place.key() == t.place.key() && c.admin == t.admin {
            if c.dir == t.path {
                return;
            }
            let (place, from_dir, to_dir, names) = (t.place.clone(), c.dir.clone(), t.path.clone(), c.names.clone());
            let back = to_dir.clone();
            cx.set_global(ClipSlot(None));
            self.run(
                side,
                move |f| {
                    for n in &names {
                        let (a, b) = (place.join(&from_dir, n), place.join(&to_dir, n));
                        if f.meta(&b)?.is_some() {
                            return Err(format!("{n} is already in this folder."));
                        }
                        f.rename(&a, &b).map_err(|e| if e.contains("cross-device") || e.contains("os error 18") { format!("{n} is on another disk: use Copy and Paste, then delete the original.") } else { e })?;
                    }
                    Ok(())
                },
                move |this, r: Result<(), String>, cx| {
                    if let Err(e) = r {
                        ui::show_error("Not moved", e, cx);
                    }
                    this.load(side, back, None, cx);
                },
                cx,
            );
            return;
        }
        let r = Request { from: c.place, from_admin: c.admin, from_dir: c.dir, names: c.names, to: t.place.clone(), to_admin: t.admin, to_dir: t.path.clone() };
        self.transfers.update(cx, |q, cx| q.add(r, cx));
        if c.cut {
            self.toast = Some(("Copied: between two computers the originals stay".into(), std::time::Instant::now()));
        }
    }

    fn start_rename(&mut self, side: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.panes[side].selected.iter().next().cloned() else { return };
        if !self.panes[side].writable() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name.clone()));
        let sub = cx.subscribe_in(&input, window, move |this: &mut Self, input, e: &InputEvent, _, cx| match e {
            InputEvent::PressEnter { .. } => {
                let to = input.read(cx).value().trim().to_string();
                this.finish_rename(side, to, cx);
            }
            InputEvent::Blur => {
                this.panes[side].rename = None;
                cx.notify();
            }
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(sub);
        self.panes[side].rename = Some((name, input));
        cx.notify();
    }

    fn finish_rename(&mut self, side: usize, to: String, cx: &mut Context<Self>) {
        let Some((from, _)) = self.panes[side].rename.take() else { return };
        if to.is_empty() || to == from || to.contains(['/', '\\']) {
            cx.notify();
            return;
        }
        let p = &self.panes[side];
        let (a, b) = (p.place.join(&p.path, &from), p.place.join(&p.path, &to));
        let path = p.path.clone();
        self.run(
            side,
            move |f| {
                if f.meta(&b)?.is_some() {
                    return Err(format!("{to} is already there."));
                }
                f.rename(&a, &b).map(|_| to)
            },
            move |this, r, cx| match r {
                Ok(to) => this.load(side, path, Some(to), cx),
                Err(e) => ui::show_error("Not renamed", e, cx),
            },
            cx,
        );
    }

    fn start_new_folder(&mut self, side: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panes[side].writable() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
        let sub = cx.subscribe_in(&input, window, move |this: &mut Self, input, e: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = e {
                let name = input.read(cx).value().trim().to_string();
                this.make_folder(side, name, cx);
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(sub);
        self.panes[side].new_folder = Some(input);
        cx.notify();
    }

    fn make_folder(&mut self, side: usize, name: String, cx: &mut Context<Self>) {
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            return;
        }
        self.panes[side].new_folder = None;
        let p = &self.panes[side];
        let (path, dir) = (p.place.join(&p.path, &name), p.path.clone());
        self.run(
            side,
            move |f| f.mkdir(&path).map(|_| name),
            move |this, r, cx| match r {
                Ok(name) => this.load(side, dir, Some(name), cx),
                Err(e) => ui::show_error("No new folder", e, cx),
            },
            cx,
        );
    }

    /// C7: what goes, asked first.
    fn confirm_delete(&mut self, side: usize, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        if p.selected.is_empty() || !p.writable() {
            return;
        }
        let rows: Vec<Entry> = p.rows().into_iter().filter(|r| p.selected.contains(&r.name)).collect();
        let n = rows.len();
        let on = if p.place.is_remote() { p.label() } else { "this computer".into() };
        let title = if n == 1 { format!("Delete {} on {on}?", rows[0].name) } else { format!("Delete {n} items on {on}?") };
        let note = if p.place.is_remote() { "The server has no recycle bin; they are gone for good." } else { "They are deleted for good, not moved to the recycle bin." };
        let me = cx.entity().downgrade();
        let body = cx.new(|_| DeleteBody { rows, note });
        let footer = cx.new(|_| DeleteFooter { me, side });
        crate::modal::open(crate::modal::Modal { title: title.into(), body: body.into(), width: 520., icon: Some(Icon::empty().path("icons/trash.svg")), on_close: None }, cx);
        crate::modal::set_footer(footer.into(), cx);
    }

    fn delete(&mut self, side: usize, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        let paths: Vec<String> = p.selected.iter().map(|n| p.place.join(&p.path, n)).collect();
        let dir = p.path.clone();
        crate::manager::strip::quiet(true, cx);
        self.run(
            side,
            move |f| {
                for path in &paths {
                    f.remove_tree(path)?;
                }
                Ok(())
            },
            move |this, r, cx| {
                crate::manager::strip::quiet(false, cx);
                if let Err(e) = r {
                    ui::show_error("Not everything was deleted", e, cx);
                }
                this.panes[side].selected.clear();
                this.load(side, dir, None, cx);
            },
            cx,
        );
    }

    /// C6: as administrator (sudo there; its password asked once).
    fn open_as_admin(&mut self, side: usize, cx: &mut Context<Self>) {
        let Place::Remote(inst) = self.panes[side].place.clone() else { return };
        let me = cx.entity().downgrade();
        let task = cx.spawn(async move |_, cx| {
            let i2 = inst.clone();
            let needs = cx.background_executor().spawn(async move { i2.user != "root" && crate::remote::sudo_needs_password(&i2) }).await;
            let go = move |cx: &mut App| {
                me.update(cx, |p, cx| {
                    let pane = &mut p.panes[side];
                    pane.admin = true;
                    pane.admin_root = pane.path.clone();
                    pane.conn = Arc::new(Mutex::new(None));
                    p.reload(side, cx);
                })
                .ok();
            };
            let _ = cx.update(|cx| if needs { crate::manager::remote_pages::ask_sudo_password(inst, Rc::new(go), cx) } else { go(cx) });
        });
        self._tasks.push(task);
    }

    fn key(&mut self, side: usize, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes[side].rename.is_some() || self.panes[side].new_folder.is_some() || self.panes[side].edit.is_some() {
            return;
        }
        let k = &e.keystroke;
        let ctrl = k.modifiers.control || k.modifiers.platform;
        match k.key.as_str() {
            "c" if ctrl => self.clip_copy(side, cx),
            "v" if ctrl => self.clip_paste(side, cx),
            "a" if ctrl => {
                let names = self.panes[side].rows().into_iter().map(|r| r.name).collect();
                self.panes[side].selected = names;
                cx.notify();
            }
            "delete" => self.confirm_delete(side, cx),
            "f2" => self.start_rename(side, window, cx),
            "backspace" => self.up(side, cx),
            "up" if k.modifiers.alt => self.up(side, cx),
            "left" if k.modifiers.alt => self.go_back(side, cx),
            "right" if k.modifiers.alt => self.go_forward(side, cx),
            "l" if ctrl => self.start_edit(side, window, cx),
            "enter" => {
                let p = &self.panes[side];
                if let Some(r) = p.rows().into_iter().find(|r| p.selected.len() == 1 && p.selected.contains(&r.name) && r.dir) {
                    self.open_dir(side, &r.name, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// A press that opens a folder: the second of a double click, without
/// Shift or Ctrl (those change the selection).
fn opens_on_press(click_count: usize, m: Modifiers) -> bool {
    click_count == 2 && !m.shift && !m.control && !m.platform
}

fn read_only_text(p: &Pane) -> String {
    let owner = p.listing.as_ref().map(|l| l.owner.clone()).filter(|o| !o.is_empty()).unwrap_or_else(|| "another account".into());
    format!("This folder belongs to {owner}. You can view, but not change anything.")
}

fn folder_color() -> Hsla {
    rgb(0xd9a23a).into()
}

fn file_icon(dir: bool) -> Icon {
    if dir { Icon::empty().path("icons/folder-fill.svg") } else { Icon::new(IconName::File) }
}

/// "16 Nov 06:40" in this computer's time.
fn when(t: Option<i64>) -> String {
    use chrono::TimeZone as _;
    t.and_then(|t| chrono::Local.timestamp_opt(t, 0).single()).map(|d| d.format("%-d %b %H:%M").to_string()).unwrap_or_default()
}

fn shortcut_icon(name: &str) -> Icon {
    match name {
        "house" => Icon::empty().path("icons/house.svg"),
        "download" => Icon::empty().path("icons/download.svg"),
        "server" => Icon::empty().path("icons/server.svg"),
        "archive" => Icon::empty().path("icons/archive.svg"),
        "scroll-text" => Icon::empty().path("icons/scroll-text.svg"),
        _ => Icon::new(IconName::HardDrive),
    }
}

/// A narrow window: the panes stack.
fn narrow(window: &Window) -> bool {
    window.viewport_size().width < px(980.)
}

impl FilesPage {
    fn pane(&self, side: usize, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let p = &self.panes[side];
        let other = 1 - side;
        let single = self.single;
        let writable = p.writable();
        let read_only = p.listing.is_some() && !writable;
        let drop_here = self.drag_over == Some(side) && writable;
        let rows = p.rows();

        // The title: where, and New folder and Read again.
        let title = h_flex()
            .h(px(52.))
            .px(px(PANE_PX))
            .gap(crate::ui::button_gap())
            .items_center()
            .child(match &p.place {
                Place::Local => Icon::empty().path("icons/monitor.svg").size_4().text_color(theme.muted_foreground).into_any_element(),
                Place::Remote(i) => div().size(px(10.)).rounded_full().bg(rgb(u32::from_str_radix(i.color.trim_start_matches('#'), 16).unwrap_or(0x1f9d55))).into_any_element(),
            })
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(FontWeight::MEDIUM).child(p.label()))
            .when(p.admin, |r| r.child(ui::pill(&theme, Tone::Warn, "Administrator").text_size(px(12.))))
            .child(crate::ui::probed(
                crate::ui::button(SharedString::from(format!("f-new-{side}")))
                    .ghost()
                    
                    .icon(Icon::empty().path("icons/folder-plus.svg"))
                    .tooltip("New folder")
                    .disabled(!writable)
                    .on_click(cx.listener(move |this, _, window, cx| this.start_new_folder(side, window, cx))),
            ))
            .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("f-reload-{side}"))).ghost().icon(IconName::RefreshCw).tooltip("Read again").on_click(cx.listener(move |this, _, _, cx| this.reload(side, cx)))));

        // Where: the path's parts, each a way back.
        let crumbs = p.place.crumbs(&p.path);
        let skeleton = p.loading && p.listing.is_none();
        trace_change(side, "bar", &format!("path={} skeleton={skeleton} rows={} error={}", p.path, rows.len(), p.error.is_some()));
        let last = crumbs.len().saturating_sub(1);
        // The bar and its text field are one box: the standard control
        // height, 8 px corners, the thin border (the focus colour while
        // typing), so switching between them moves nothing.
        let bar_box = |id: String| {
            ui::input_box()
                .id(SharedString::from(id))
                .flex_1()
                .min_w_0()
                .relative()
                .rounded(px(8.))
                .border_1()
                .overflow_hidden()
                .text_sm()
        };
        let bar = match p.edit.clone() {
            Some(input) => {
                let focus = p.focus.clone();
                bar_box(format!("f-path-edit-{side}"))
                    .border_color(if p.edit_error.is_some() { theme.danger } else { theme.ring })
                    .px(px(2.))
                    // Escape: the bar again, the folder as it was; before the
                    // window's Escape (which closes a dialog).
                    .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        if e.keystroke.key == "escape" {
                            this.cancel_edit(side, cx);
                            window.focus(&focus, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(div().flex_1().min_w_0().child(Input::new(&input).appearance(false).disabled(p.checking)))
                    .when(p.checking, |b| b.child(div().pr(px(8.)).text_color(theme.muted_foreground).child("Checking…")))
                    .child(probe(format!("files-path-edit {side}")))
                    .into_any_element()
            }
            None => bar_box(format!("f-path-{side}"))
                .px(px(10.))
                .gap_1()
                .items_center()
                .border_color(theme.border)
                .cursor_text()
                // Its empty part: the path as text, to type another.
                .on_click(cx.listener(move |this, _, window, cx| this.start_edit(side, window, cx)))
                .child(probe(format!("files-path {side}")))
                .children(crumbs.into_iter().enumerate().flat_map(|(i, (name, path))| {
                    let sep = (i > 0).then(|| div().text_color(theme.muted_foreground).child("›").into_any_element());
                    let part = div()
                        .id(SharedString::from(format!("f-crumb-{side}-{i}")))
                        .relative()
                        .flex_shrink_0()
                        .px_1()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.list_hover))
                        .when(i == last, |d| d.text_color(theme.foreground).font_weight(FontWeight::MEDIUM))
                        .when(i != last, |d| d.text_color(theme.muted_foreground))
                        .child(name)
                        .child(probe(format!("files-crumb {side} {i}")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // A crumb jumps there; not the bar's own click.
                            cx.stop_propagation();
                            this.go(side, path.clone(), None, cx);
                        }))
                        .into_any_element();
                    sep.into_iter().chain([part])
                }))
                .into_any_element(),
        };
        // Back, Forward and Up, at the bar's left: each off where it can't go.
        let nav = |id: &str, icon: IconName, tip: &'static str, on: bool, go: fn(&mut Self, usize, &mut Context<Self>)| {
            crate::ui::probed(
                crate::ui::button(SharedString::from(format!("f-{id}-{side}")))
                    .ghost()
                    .icon(icon)
                    .tooltip(tip)
                    .disabled(!on)
                    .on_click(cx.listener(move |this, _, _, cx| go(this, side, cx))),
            )
        };
        let can_up = !p.path.is_empty() && p.place.parent(&p.path).is_some();
        let bar = h_flex()
            .gap(crate::ui::button_gap())
            .items_center()
            .child(nav("back", IconName::ArrowLeft, "Back (Alt+Left)", !p.hist.is_empty(), Self::go_back))
            .child(nav("forward", IconName::ArrowRight, "Forward (Alt+Right)", !p.fwd.is_empty(), Self::go_forward))
            .child(nav("up", IconName::ArrowUp, "Up (Alt+Up)", can_up, Self::up))
            .child(bar);

        // Shortcuts; the one the folder is in is marked.
        let marked = p.shortcuts.iter().find(|(_, _, s)| s.trim_end_matches(['/', '\\']) == p.path.trim_end_matches(['/', '\\'])).map(|(_, _, s)| s.clone());
        let chips = h_flex().flex_wrap().gap_2().children(p.shortcuts.iter().enumerate().map(|(i, (label, icon, path))| {
            let on = marked.as_deref() == Some(path.as_str());
            let path = path.clone();
            h_flex()
                .id(SharedString::from(format!("f-chip-{side}-{i}")))
                .h(px(28.))
                .px(px(10.))
                .gap(px(6.))
                .items_center()
                .rounded_full()
                .border_1()
                .text_size(px(13.))
                .cursor_pointer()
                .when(on, |c| c.border_color(theme.primary).bg(theme.primary.opacity(0.08)).text_color(theme.primary))
                .when(!on, |c| c.border_color(theme.border).hover(|c| c.bg(theme.list_hover)))
                .relative()
                .child(shortcut_icon(icon).size_3p5())
                .child(label.clone())
                .child(probe(format!("files-chip {side} {}", label.replace(' ', "_"))))
                .on_click(cx.listener(move |this, _, _, cx| this.go(side, path.clone(), None, cx)))
        }));

        // C6: read only, and the way out on a server.
        let amber = crate::ui::warn_text(theme.background.l >= 0.5);
        let banner = read_only.then(|| {
            h_flex()
                .gap_3()
                .px(px(14.))
                .py(px(10.))
                .items_center()
                .rounded(px(8.))
                .bg(rgb(0xfdf3c7))
                .when(theme.background.l < 0.5, |b| b.bg(rgb(0x3d3413)))
                .child(Icon::empty().path("icons/lock.svg").size_4().flex_shrink_0().text_color(amber))
                .child(div().flex_1().min_w_0().text_sm().text_color(amber).child(read_only_text(p)))
                .when(matches!(&p.place, Place::Remote(i) if !crate::remote::is_windows(i)), |b| {
                    b.child(crate::ui::button(SharedString::from(format!("f-admin-{side}"))).outline().label("Open as administrator").on_click(cx.listener(move |this, _, _, cx| this.open_as_admin(side, cx))))
                })
        });

        let head = v_flex()
            .relative()
            .px(px(PANE_PX))
            .pb(px(10.))
            .gap(px(10.))
            // The layout test checks this part's right padding.
            .child(crate::ui::layout_probe("panel"))
            .child(bar)
            // A folder that did not open: why, under the bar (the bar is
            // back on the folder before).
            .children(p.edit_error.clone().or_else(|| p.error.clone()).map(|e| div().id(SharedString::from(format!("f-error-{side}"))).text_sm().text_color(theme.danger).child(e)))
            .child(chips)
            .children(banner);

        // The table.
        let shown = p.shown();
        let w = p.cols;
        let cols = |name: AnyElement, size: AnyElement, time: AnyElement, owner: Option<AnyElement>| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(COL_GAP))
                .items_center()
                .child(div().flex_1().min_w_0().child(name))
                .when(shown[0], |r| r.child(div().w(px(w[0])).flex_shrink_0().flex().justify_end().overflow_hidden().child(size)))
                .when(shown[1], |r| r.child(div().w(px(w[1])).flex_shrink_0().flex().justify_end().overflow_hidden().child(time)))
                .when(shown[2], |r| r.child(div().w(px(w[2])).flex_shrink_0().pl(px(8.)).overflow_hidden().children(owner)))
        };
        // A header cell with its left edge as a handle that widens it.
        let handle = |col: usize, label: &'static str, end: bool| -> AnyElement {
            div()
                .relative()
                .size_full()
                .flex()
                .items_center()
                .when(end, |d| d.justify_end())
                .child(label)
                .child(
                    div()
                        .id(SharedString::from(format!("f-colh-{side}-{col}")))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        // Inside the cell (it clips): its left edge.
                        .left_0()
                        .w(px(8.))
                        .cursor_col_resize()
                        .group(SharedString::from(format!("f-colg-{side}-{col}")))
                        .flex()
                        .items_center()
                        .child(div().w(px(2.)).h(px(14.)).rounded_full().group_hover(SharedString::from(format!("f-colg-{side}-{col}")), |d| d.bg(theme.primary)))
                        .on_drag(ColDrag { side, col }, |_, _, _, cx| cx.new(|_| NoDrag))
                        .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                            // Twice: back to its width before dragging.
                            if e.click_count() >= 2 {
                                this.panes[side].cols[col] = COLS[col];
                                this.save_layout();
                                cx.notify();
                            }
                        })),
                )
                .into_any_element()
        };
        let header = h_flex()
            .h(px(30.))
            .px(px(PANE_PX))
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.5))
            .text_color(theme.muted_foreground)
            .child(cols(div().child("Name").into_any_element(), handle(0, "Size", true), handle(1, "Modified", true), Some(handle(2, "Owner", false))))
            .on_drag_move::<ColDrag>(cx.listener(move |this, e: &DragMoveEvent<ColDrag>, _, cx| {
                let d = e.drag(cx).clone();
                if d.side != side {
                    return;
                }
                let p = &mut this.panes[side];
                let shown = p.shown();
                // The column's right edge: the header's, less padding and
                // the columns right of it.
                let right = f32::from(e.bounds.right()) - PANE_PX - ((d.col + 1)..3).filter(|&i| shown[i]).map(|i| p.cols[i] + COL_GAP).sum::<f32>();
                let inner = f32::from(e.bounds.size.width) - 2. * PANE_PX;
                let others: f32 = (0..3).filter(|&i| shown[i] && i != d.col).map(|i| p.cols[i] + COL_GAP).sum();
                let max = (inner - NAME_MIN - others - COL_GAP).clamp(48., 320.);
                let want = (right - f32::from(e.event.position.x)).clamp(48., max);
                if (want - p.cols[d.col]).abs() >= 1. {
                    p.cols[d.col] = want.round();
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this, _: &ColDrag, _, _| this.save_layout()));
        let mut list = v_flex().id(SharedString::from(format!("f-rows-{side}"))).flex_1().min_h_0().overflow_y_scrollbar();
        if let Some(input) = &p.new_folder {
            list = list.child(
                h_flex()
                    .h(px(40.))
                    .px(px(PANE_PX))
                    .gap(crate::ui::button_gap())
                    .items_center()
                    .child(file_icon(true).size_4().text_color(folder_color()))
                    .child(div().flex_1().child(Input::new(input)))
                    .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("f-make-{side}"))).primary().label("Make").on_click(cx.listener(move |this, _, _, cx| {
                        let name = this.panes[side].new_folder.as_ref().map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
                        this.make_folder(side, name, cx);
                    }))))
                    .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("f-make-no-{side}"))).ghost().label("Cancel").on_click(cx.listener(move |this, _, _, cx| {
                        this.panes[side].new_folder = None;
                        cx.notify();
                    })))),
            );
        }
        if skeleton {
            for i in 0..5 {
                list = list.child(h_flex().h(px(ROW_H)).px(px(PANE_PX)).gap_3().items_center().child(crate::ui::skeleton_line(&theme, format!("f-sk-{side}-{i}"), 180.)).child(div().flex_1()).child(crate::ui::skeleton_line(&theme, format!("f-sk2-{side}-{i}"), 60.)));
            }
        } else if p.listing.is_some() && rows.is_empty() && p.new_folder.is_none() && self.transfers.read(cx).copying_into(&p.place.key(), &p.path).is_empty() {
            list = list.child(div().px(px(PANE_PX)).py_3().text_sm().text_color(theme.muted_foreground).child("Nothing here."));
        }
        let me = cx.entity().downgrade();
        let other_label = if self.panes[other].place.is_remote() { format!("Copy to {}", self.panes[other].label()) } else { "Copy to this computer".to_string() };
        for (i, r) in rows.iter().enumerate() {
            let on = p.selected.contains(&r.name);
            let renaming = p.rename.as_ref().filter(|(n, _)| *n == r.name).map(|(_, i)| i.clone());
            let name_el = h_flex()
                .gap(px(10.))
                .items_center()
                .min_w_0()
                .child(file_icon(r.dir).size_4().flex_shrink_0().text_color(if r.dir { folder_color() } else { theme.muted_foreground }))
                .child(match renaming {
                    Some(input) => div().flex_1().child(Input::new(&input)).into_any_element(),
                    None => div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(r.name.clone()).into_any_element(),
                })
                .into_any_element();
            let size = div().text_color(theme.muted_foreground).font_features(ui::tabular()).child(if r.dir { "—".to_string() } else { bytes_label(r.size) }).into_any_element();
            let time = div().text_color(theme.muted_foreground).font_features(ui::tabular()).child(when(r.mtime)).into_any_element();
            let owner = div().text_color(theme.muted_foreground).overflow_hidden().whitespace_nowrap().child(r.owner.clone()).into_any_element();
            let drag = Drag { side, names: if on { p.selected.iter().cloned().collect() } else { vec![r.name.clone()] }, dir: r.dir };
            let (m, name, ol) = (me.clone(), r.name.clone(), other_label.clone());
            let row = h_flex()
                .id(SharedString::from(format!("f-row-{side}-{i}")))
                .h(px(ROW_H))
                .flex_shrink_0()
                .px(px(PANE_PX))
                .text_sm()
                .when(on, |r| r.bg(theme.sidebar_accent))
                .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
                .relative()
                .child(cols(name_el, size, time, Some(owner)))
                .child(probe(format!("files-row {side} {}", r.name.replace(' ', "_"))))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| this.press_row(side, i, e, window, cx)))
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| this.click_row(side, i, e, window, cx)))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, window, cx| {
                    let p = &mut this.panes[side];
                    window.focus(&p.focus, cx);
                    if !p.selected.contains(&name) {
                        p.selected = [name.clone()].into();
                        p.anchor = Some(i);
                    }
                    cx.notify();
                }))
                .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
                .context_menu(move |menu, _, _| {
                    let act = |f: fn(&mut FilesPage, usize, &mut Window, &mut Context<FilesPage>)| {
                        let m = m.clone();
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            m.update(cx, |p, cx| f(p, side, window, cx)).ok();
                        }
                    };
                    menu.when(!single, |m| {
                        m.item(PopupMenuItem::new(ol.clone()).icon(Icon::empty().path("icons/download.svg")).on_click(act(|p, side, _, cx| {
                            let names = p.panes[side].selected.iter().cloned().collect();
                            p.copy(side, names, 1 - side, cx);
                        })))
                        .separator()
                    })
                    .item(PopupMenuItem::new("Copy").icon(IconName::Copy).on_click(act(|p, side, _, cx| p.clip_copy(side, cx))))
                    .item(PopupMenuItem::new("Cut").icon(Icon::empty().path("icons/scissors.svg")).disabled(!writable).on_click(act(|p, side, _, cx| p.clip_cut(side, cx))))
                    .item(PopupMenuItem::new("Copy location").icon(Icon::empty().path("icons/link.svg")).on_click(act(|p, side, _, cx| p.copy_location(side, cx))))
                    .item(PopupMenuItem::new("Paste").icon(Icon::empty().path("icons/clipboard.svg")).disabled(!writable).on_click(act(|p, side, _, cx| p.clip_paste(side, cx))))
                    .item(PopupMenuItem::new("Rename").icon(Icon::empty().path("icons/pencil.svg")).disabled(!writable).on_click(act(|p, side, w, cx| p.start_rename(side, w, cx))))
                    .item(PopupMenuItem::new("New folder").icon(Icon::empty().path("icons/folder-plus.svg")).disabled(!writable).on_click(act(|p, side, w, cx| p.start_new_folder(side, w, cx))))
                    .separator()
                    .item(PopupMenuItem::new("Delete").icon(Icon::empty().path("icons/trash.svg")).disabled(!writable).on_click(act(|p, side, _, cx| p.confirm_delete(side, cx))))
                });
            list = list.child(row);
        }
        // C3: copies arriving here.
        for (k, (name, done)) in self.transfers.read(cx).copying_into(&p.place.key(), &p.path).into_iter().enumerate() {
            if rows.iter().any(|r| r.name == name) {
                continue;
            }
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("f-incoming-{side}-{k}")))
                    .h(px(ROW_H))
                    .flex_shrink_0()
                    .px(px(PANE_PX))
                    .text_sm()
                    .child(cols(
                        h_flex().gap(px(10.)).items_center().min_w_0().child(file_icon(false).size_4().text_color(theme.muted_foreground)).child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name)).into_any_element(),
                        div().text_color(theme.muted_foreground).child(bytes_label(done)).into_any_element(),
                        div().text_color(theme.muted_foreground).child("copying…").into_any_element(),
                        None,
                    )),
            );
        }

        // The foot: what is selected and the copy across, or what is here;
        // while a drag hovers, where it goes (C2).
        let selected_bytes: u64 = rows.iter().filter(|r| p.selected.contains(&r.name)).map(|r| r.size).sum();
        let n = rows.len();
        let foot_text = if drop_here {
            let k = if self.drag_count > 0 { self.drag_count } else { self.panes[other].selected.len().max(1) };
            format!("Drop to copy {k} item{} into {}", if k == 1 { "" } else { "s" }, p.path)
        } else if !p.selected.is_empty() {
            if selected_bytes > 0 { format!("{} selected · {}", p.selected.len(), bytes_label(selected_bytes)) } else { format!("{} selected", p.selected.len()) }
        } else {
            let free = p.listing.as_ref().and_then(|l| l.free).filter(|_| p.place.is_remote()).map(|f| format!(" · {} free", crate::manager::backup::free_label(f))).unwrap_or_default();
            format!("{n} item{}{}{}", if n == 1 { "" } else { "s" }, if read_only { " · read only" } else { &free }, "")
        };
        let foot = h_flex()
            .min_h(px(56.))
            .px(px(PANE_PX))
            .gap(crate::ui::button_gap())
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .child(div().flex_1().min_w_0().text_sm().text_color(if drop_here { theme.primary } else { theme.muted_foreground }).overflow_hidden().whitespace_nowrap().text_ellipsis().child(foot_text))
            .when(!self.single && !p.selected.is_empty() && !drop_here, |r| {
                let label = if self.panes[other].place.is_remote() { "Copy to server".to_string() } else if p.place.is_remote() { "Copy to this computer".to_string() } else { "Copy to the other side".to_string() };
                r.child(crate::ui::button(SharedString::from(format!("f-copy-{side}"))).primary().icon(IconName::ArrowRight).label(label).disabled(!self.panes[other].writable()).on_click(cx.listener(move |this, _, _, cx| {
                    let names = this.panes[side].selected.iter().cloned().collect();
                    this.copy(side, names, 1 - side, cx);
                })))
            });

        let focus = p.focus.clone();
        let measured = p.width.clone();
        v_flex()
            .id(SharedString::from(format!("f-pane-{side}")))
            .relative()
            // Its width as drawn: the columns that fit follow it.
            .child(
                canvas(
                    move |b, window, _| {
                        let w = f32::from(b.size.width);
                        if (measured.get() - w).abs() > 0.5 {
                            measured.set(w);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .track_focus(&focus)
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| this.key(side, e, window, cx)))
            .flex_1()
            .min_w_0()
            .when(narrow, |c| c.h(px(460.)).flex_none())
            .when(!narrow, |c| c.h_full())
            .overflow_hidden()
            .rounded(px(ui::PANEL_RADIUS))
            .bg(theme.background)
            .border_1()
            .border_color(if drop_here { theme.primary } else { theme.border })
            .when(drop_here, |c| c.border_2())
            .child(title)
            .child(head)
            .child(header)
            .child(list)
            .child(foot)
            .on_drag_move::<Drag>(cx.listener(move |this, e: &DragMoveEvent<Drag>, _, cx| {
                let inside = e.bounds.contains(&e.event.position);
                let (src, count) = { let d = e.drag(cx); (d.side, d.names.len()) };
                this.drag_count = count;
                let want = if inside && src != side { Some(side) } else if this.drag_over == Some(side) { None } else { this.drag_over };
                if this.drag_over != want {
                    this.drag_over = want;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(move |this, d: &Drag, _, cx| {
                this.drag_over = None;
                if d.side != side {
                    this.copy(d.side, d.names.clone(), side, cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for FilesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((_, f)) = self.refocus.take() {
            window.focus(&f, cx);
        }
        // Screens: the path field once its side has a folder.
        if let Some((side, typed)) = self.debug_edit.take() {
            if self.panes[side].listing.is_some() {
                self.start_edit(side, window, cx);
                if let (false, Some(input)) = (typed.is_empty(), self.panes[side].edit.clone()) {
                    input.update(cx, |i, cx| i.set_value(typed.clone(), window, cx));
                    self.submit_edit(side, typed, window, cx);
                }
            } else {
                self.debug_edit = Some((side, typed));
            }
        }
        let theme = cx.theme().clone();
        if !cx.has_active_drag() && self.drag_over.is_some() && std::env::var_os("SPM_DEBUG_FILES_STATE").is_none() {
            self.drag_over = None;
        }
        let narrow = narrow(window);
        let left = self.pane(0, narrow, cx);
        let single = self.single;
        let right = if single { div().into_any_element() } else { self.pane(1, narrow, cx) };
        // Screens: the drag's card over the right pane (C2).
        let fake_drag = (self.drag_over == Some(1) && !cx.has_active_drag()).then(|| {
            let names: Vec<String> = self.panes[0].selected.iter().cloned().collect();
            div().absolute().top(px(250.)).left(px(120.)).child(drag_card(&theme, &names, false))
        });
        let me = cx.entity().downgrade();
        let shown = if single { 1 } else { 2 };
        let busy = self.panes[..shown].iter().any(|p| p.loading);
        let refresh = crate::ui::refresh_button("f-reload", busy, window, move |_, cx| {
            me.update(cx, |this, cx| {
                this.reload(0, cx);
                if !this.single {
                    this.reload(1, cx);
                }
            })
            .ok();
        });
        let dbg = ui::debug_stale();
        let stale = self.panes[..shown].iter().find_map(|p| p.stale.as_deref().map(|e| (e, p.updated_at.as_deref()))).or(dbg.as_deref().map(|e| (e, Some("21:05")))).filter(|_| !busy).map(|(e, at)| ui::stale_note(&theme, "files-stale", e, at));
        let panes = if single {
            v_flex().flex_1().min_h_0().child(left).into_any_element()
        } else if narrow {
            v_flex().gap(px(crate::ui::GAP)).child(left).child(div().relative().child(right).children(fake_drag)).into_any_element()
        } else {
            let r = self.split;
            let grow = |d: Div, g: f32| {
                let mut d = d.flex_basis(px(0.)).min_w(px(PANE_MIN)).h_full().flex();
                d.style().flex_grow = Some(g);
                d.style().flex_shrink = Some(1.);
                d
            };
            let active = self.splitting && cx.has_active_drag();
            let splitter = div()
                .id("f-split")
                .w(px(GUTTER))
                .h_full()
                .flex_shrink_0()
                .flex()
                .justify_center()
                .cursor_col_resize()
                .group("f-split")
                .child(div().w(px(2.)).h_full().rounded_full().when(active, |d| d.bg(theme.primary)).group_hover("f-split", |d| d.bg(theme.primary)))
                .on_drag(SplitDrag, |_, _, _, cx| cx.new(|_| NoDrag))
                .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                    // Twice: half and half again.
                    if e.click_count() >= 2 {
                        this.split = 0.5;
                        this.save_layout();
                        cx.notify();
                    }
                }));
            h_flex()
                .id("f-panes")
                .flex_1()
                .min_h_0()
                .items_stretch()
                .child(grow(div(), r).child(left))
                .child(splitter)
                .child(grow(div().relative(), 1. - r).child(right).children(fake_drag))
                .on_drag_move::<SplitDrag>(cx.listener(|this, e: &DragMoveEvent<SplitDrag>, _, cx| {
                    let avail = f32::from(e.bounds.size.width) - GUTTER;
                    if avail <= 2. * PANE_MIN {
                        return;
                    }
                    let x = f32::from(e.event.position.x - e.bounds.left()) - GUTTER / 2.;
                    let r = (x / avail).clamp(PANE_MIN / avail, 1. - PANE_MIN / avail);
                    this.splitting = true;
                    if (r - this.split).abs() > 0.001 {
                        this.split = r;
                        cx.notify();
                    }
                }))
                .on_drop(cx.listener(|this, _: &SplitDrag, _, cx| {
                    this.splitting = false;
                    this.save_layout();
                    cx.notify();
                }))
                .into_any_element()
        };
        let page = v_flex()
            .id("page")
            .flex_1()
            .min_w_0()
            .h_full()
            .p(px(ui::PAGE_PAD))
            .gap(px(ui::GAP))
            .bg(theme.muted)
            .child(h_flex().min_h(px(32.)).items_center().child(div().flex_1().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Files")).child(h_flex().gap_3().items_center().children(stale).child(crate::ui::probed(refresh))))
            .child(panes);
        // The short note, bottom centre over the page (fades in).
        let toast = self.toast.as_ref().map(|(text, _)| {
            div().absolute().bottom(px(24.)).left_0().right_0().flex().justify_center().child(
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .bg(theme.foreground)
                    .text_color(theme.background)
                    .text_sm()
                    .shadow_md()
                    .child(text.clone())
                    .with_animation(SharedString::from(format!("toast-{text}")), Animation::new(std::time::Duration::from_millis(150)), |el, t| el.opacity(t)),
            )
        });
        // Narrow: the whole page scrolls (the panes stack), with the shared bar.
        let page = if narrow { page.overflow_y_scrollbar().into_any_element() } else { page.into_any_element() };
        div().relative().flex().flex_1().min_w_0().h_full().child(page).children(toast)
    }
}

/// What the splitter and the header's column edges drag (nothing drawn).
#[derive(Clone)]
struct SplitDrag;
#[derive(Clone)]
struct ColDrag {
    side: usize,
    col: usize,
}
struct NoDrag;
impl Render for NoDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl FilesPage {
    /// Keep the split and the columns with the window's state.
    fn save_layout(&self) {
        crate::manager::session::set_files(self.split, [self.panes[0].cols, self.panes[1].cols]);
    }
}

/// Click tests: where a crumb, chip or row is (`LAYOUT files-… l r t b`
/// with SPM_DEBUG_LAYOUT; written when it moves).
fn probe(name: String) -> AnyElement {
    if !(cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some()) {
        return div().into_any_element();
    }
    canvas(
        move |b, _, _| {
            let v = [b.origin.x, b.origin.x + b.size.width, b.origin.y, b.origin.y + b.size.height].map(|x| f32::from(x).round() as i32);
            static LAST: Mutex<Option<std::collections::HashMap<String, [i32; 4]>>> = Mutex::new(None);
            let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
            if g.get_or_insert_with(Default::default).insert(name.clone(), v) != Some(v) {
                eprintln!("LAYOUT {name} {} {} {} {}", v[0], v[1], v[2], v[3]);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// Click tests: what a pane shows, as a timing mark when it changes
/// (SPM_DEBUG_TIMING): `TIMING files-bar side=1 path=… skeleton=…`.
fn trace_change(side: usize, what: &str, now: &str) {
    if !crate::timing::on() {
        return;
    }
    static LAST: Mutex<[String; 2]> = Mutex::new([String::new(), String::new()]);
    let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if g[side] != now {
        g[side] = now.to_string();
        drop(g);
        crate::timing::mark(&format!("files-{what} side={side} {now}"));
    }
}

/// How long a folder's listing is shown at once when it is opened again
/// (then listed again behind it): 45 s, long enough to go back and forth
/// and to open what was read ahead, short enough that a cached listing is
/// rarely far from the folder.
const CACHE_FOR: std::time::Duration = std::time::Duration::from_secs(45);

/// The folders in a folder read ahead after it opens.
const AHEAD_MAX: usize = 50;

/// Click tests: the next listing held back by the milliseconds written in
/// the file SPM_DEBUG_FILES_HOLD names (the file is removed).
fn debug_hold() -> Option<std::time::Duration> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let f = std::env::var_os("SPM_DEBUG_FILES_HOLD")?;
    let ms: u64 = std::fs::read_to_string(&f).ok()?.trim().parse().ok()?;
    let _ = std::fs::remove_file(&f);
    Some(std::time::Duration::from_millis(ms))
}

/// How long the short note stays.
const TOAST_FOR: std::time::Duration = std::time::Duration::from_millis(1800);

/// A path as its system writes it: an SFTP path on a Windows server
/// ("/C:/Users/x") as Windows writes it ("C:\\Users\\x").
fn os_path(p: &str, windows: bool) -> String {
    if !windows {
        return p.to_string();
    }
    let t = p.strip_prefix('/').filter(|r| r.as_bytes().get(1) == Some(&b':')).unwrap_or(p);
    t.replace('/', "\\")
}

// -- C7: delete ----------------------------------------------------------------

struct DeleteBody {
    rows: Vec<Entry>,
    note: &'static str,
}

impl Render for DeleteBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut list = v_flex().id("del-list").max_h(px(240.)).rounded(px(8.)).border_1().border_color(theme.border).py_1();
        for r in &self.rows {
            list = list.child(
                h_flex()
                    .h(px(34.))
                    .px(px(14.))
                    .gap(px(10.))
                    .items_center()
                    .text_sm()
                    .child(file_icon(r.dir).size_4().text_color(if r.dir { folder_color() } else { theme.muted_foreground }))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(r.name.clone()))
                    .child(div().text_color(theme.muted_foreground).child(if r.dir { "folder".to_string() } else { bytes_label(r.size) })),
            );
        }
        v_flex().gap(px(14.)).child(crate::ui::scroll_y(list)).child(div().text_sm().text_color(theme.muted_foreground).child(self.note))
    }
}

struct DeleteFooter {
    me: WeakEntity<FilesPage>,
    side: usize,
}

impl Render for DeleteFooter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (me, side) = (self.me.clone(), self.side);
        let _ = cx;
        h_flex()
            .w_full()
            .gap(crate::ui::button_gap())
            .justify_end()
            .child(crate::ui::probed(crate::ui::button("del-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx))))
            // The mockup's solid red (the kit's danger is a tint).
            .child(crate::ui::probed(
                div()
                    .id("del-go")
                    .h(px(32.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(rgb(0xef4444))
                    .hover(|d| d.bg(rgb(0xdc2626)))
                    .active(|d| d.bg(rgb(0xb91c1c)))
                    .text_color(white())
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .child("Delete")
                    .on_click(move |_, _, cx| {
                        crate::modal::close(cx);
                        me.update(cx, |p, cx| p.delete(side, cx)).ok();
                    }),
            ))
    }
}

#[cfg(test)]
mod location_tests {
    #[test]
    fn the_second_press_opens_a_folder_but_not_with_shift_or_ctrl() {
        use gpui_kit::Modifiers;
        let none = Modifiers::default();
        assert!(!super::opens_on_press(1, none));
        assert!(super::opens_on_press(2, none));
        assert!(!super::opens_on_press(3, none));
        assert!(!super::opens_on_press(2, Modifiers { shift: true, ..Default::default() }));
        assert!(!super::opens_on_press(2, Modifiers { control: true, ..Default::default() }));
    }

    use super::os_path;

    #[test]
    fn locations_as_the_system_writes_them() {
        assert_eq!(os_path("/opt/sentient/backups", false), "/opt/sentient/backups");
        assert_eq!(os_path("/C:/Users/ops/file.txt", true), "C:\\Users\\ops\\file.txt");
        assert_eq!(os_path("/home/x", true), "\\home\\x");
    }
}
