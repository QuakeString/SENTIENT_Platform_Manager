// Copyright © 2016-2026 The SENTIENT Authors

//! Files (mockup C1–C7): this computer on the left, the open instance on the
//! right (over SFTP on its SSH sign-in). With this computer open there is one
//! pane, full width (two of the same computer made no sense, his rc12-22
//! note): copy, cut and paste work inside it. Drag between the panes, Ctrl+C / Ctrl+V (folders too), rename,
//! new folder and delete with a confirmation. Copies run in the background
//! (crate::manager::files::transfers). A folder the account may not change
//! is read only, with "Open as administrator" on a server (sudo, its
//! password asked once).

pub(crate) mod fs;
pub(crate) mod transfers;

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex,
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
    /// Every folder listed in this run, by path: shown at once when it is
    /// opened again, while it is listed again behind it.
    seen: std::collections::HashMap<String, Listing>,
    /// A re-list failed while the listing read before still shows.
    stale: Option<String>,
    /// When the listing last came back (HH:MM).
    updated_at: Option<String>,
    /// Size, Modified, Owner widths (dragged in the header).
    cols: [f32; 3],
    /// The pane's width as last drawn (which columns fit).
    width: Rc<std::cell::Cell<f32>>,
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
            stale: None,
            updated_at: None,
            cols: COLS,
            width: Rc::new(std::cell::Cell::new(0.)),
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

    /// May change things here.
    fn writable(&self) -> bool {
        self.admin || self.listing.as_ref().is_some_and(|l| l.writable)
    }

    /// Which of Size, Modified and Owner fit beside a Name of NAME_MIN:
    /// Owner goes first, then Size, then Modified.
    fn shown(&self) -> [bool; 3] {
        let mut on = [true, true, self.place.is_remote()];
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
        let mut me = FilesPage { panes: [Pane::new(Place::Local, cx), Pane::new(right, cx)], single, drag_over: None, drag_count: 0, transfers, split: 0.5, splitting: false, toast: None, _subs: subs, _tasks: Vec::new() };
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
        let p = &self.panes[side];
        let (conn, place, admin) = (p.conn.clone(), p.place.clone(), p.admin);
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let mut g = conn.lock().map_err(|_| "The connection is busy.".to_string())?;
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

    /// List `path` (empty: home), and pick `select` in it.
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
        // A folder listed before shows at once (its selection starts afresh).
        if let Some(l) = pane.seen.get(&path).filter(|l| pane.listing.as_ref().is_none_or(|c| c.path != l.path)).cloned() {
            pane.selected.clear();
            pane.anchor = None;
            pane.rename = None;
            pane.path = l.path.clone();
            pane.listing = Some(l);
        }
        let asked = path.clone();
        let first = pane.shortcuts.is_empty();
        self.run(
            side,
            move |f| {
                let path = if path.is_empty() { f.home() } else { path };
                let shortcuts = if first { f.shortcuts() } else { Vec::new() };
                Ok((f.list(&path)?, shortcuts))
            },
            move |this, r, cx| {
                let pane = &mut this.panes[side];
                if pane.seq != seq {
                    return;
                }
                pane.loading = false;
                match r {
                    Ok((l, shortcuts)) => {
                        if !shortcuts.is_empty() {
                            pane.shortcuts = shortcuts;
                        }
                        if !pane.path.is_empty() && pane.path != l.path {
                            pane.selected.clear();
                            pane.anchor = None;
                            pane.rename = None;
                        }
                        pane.path = l.path.clone();
                        crate::manager::session::set_files_dir(&pane.place.key(), &l.path);
                        let names: BTreeSet<String> = l.entries.iter().map(|e| e.name.clone()).collect();
                        pane.selected.retain(|n| names.contains(n));
                        if let Some(s) = select.filter(|s| names.contains(s)) {
                            pane.selected = [s].into();
                        }
                        pane.seen.insert(l.path.clone(), l.clone());
                        if !asked.is_empty() && asked != l.path {
                            pane.seen.insert(asked, l.clone());
                        }
                        pane.stale = None;
                        pane.updated_at = Some(chrono::Local::now().format("%H:%M").to_string());
                        pane.listing = Some(l);
                    }
                    // The folder shown is the one asked for: it stays,
                    // said softly to be stale.
                    Err(e) if pane.listing.as_ref().is_some_and(|l| l.path == asked) => pane.stale = Some(e),
                    Err(e) => pane.error = Some(e),
                }
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }

    fn reload(&mut self, side: usize, cx: &mut Context<Self>) {
        let path = self.panes[side].path.clone();
        self.load(side, path, None, cx);
    }

    fn open_dir(&mut self, side: usize, name: &str, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        let path = p.place.join(&p.path, name);
        self.load(side, path, None, cx);
    }

    fn up(&mut self, side: usize, cx: &mut Context<Self>) {
        let p = &self.panes[side];
        if let Some(up) = p.place.parent(&p.path) {
            let from = p.path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().map(String::from);
            self.load(side, up, from, cx);
        }
    }

    /// A row clicked: select it (Ctrl adds, Shift a range); twice opens a folder.
    fn click_row(&mut self, side: usize, i: usize, e: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.panes[side].rows();
        let Some(row) = rows.get(i) else { return };
        let pane = &mut self.panes[side];
        window.focus(&pane.focus, cx);
        if e.click_count() >= 2 {
            if row.dir {
                let name = row.name.clone();
                self.open_dir(side, &name, cx);
            }
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
        if self.panes[side].rename.is_some() || self.panes[side].new_folder.is_some() {
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

fn read_only_text(p: &Pane) -> String {
    let owner = p.listing.as_ref().map(|l| l.owner.clone()).filter(|o| !o.is_empty()).unwrap_or_else(|| "another account".into());
    format!("This folder belongs to {owner}. You can look, but not change anything.")
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
                    .small()
                    .icon(Icon::empty().path("icons/folder-plus.svg"))
                    .tooltip("New folder")
                    .disabled(!writable)
                    .on_click(cx.listener(move |this, _, window, cx| this.start_new_folder(side, window, cx))),
            ))
            .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("f-reload-{side}"))).ghost().small().icon(IconName::RefreshCw).tooltip("Read again").on_click(cx.listener(move |this, _, _, cx| this.reload(side, cx)))));

        // Where: the path's parts, each a way back.
        let crumbs = p.place.crumbs(&p.path);
        let last = crumbs.len().saturating_sub(1);
        let bar = h_flex()
            .h(px(34.))
            .px(px(10.))
            .gap_1()
            .items_center()
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .text_sm()
            .children(crumbs.into_iter().enumerate().flat_map(|(i, (name, path))| {
                let sep = (i > 0).then(|| div().text_color(theme.muted_foreground).child("›").into_any_element());
                let part = div()
                    .id(SharedString::from(format!("f-crumb-{side}-{i}")))
                    .flex_shrink_0()
                    .px_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.list_hover))
                    .when(i == last, |d| d.text_color(theme.foreground).font_weight(FontWeight::MEDIUM))
                    .when(i != last, |d| d.text_color(theme.muted_foreground))
                    .child(name)
                    .on_click(cx.listener(move |this, _, _, cx| this.load(side, path.clone(), None, cx)))
                    .into_any_element();
                sep.into_iter().chain([part])
            }));

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
                .child(shortcut_icon(icon).size_3p5())
                .child(label.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.load(side, path.clone(), None, cx)))
        }));

        // C6: read only, and the way out on a server.
        let amber: Hsla = if theme.background.l < 0.5 { rgb(0xfcd34d).into() } else { rgb(0x9a3412).into() };
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
        let mut list = v_flex().id(SharedString::from(format!("f-rows-{side}"))).flex_1().min_h_0().overflow_y_scroll();
        if let Some(input) = &p.new_folder {
            list = list.child(
                h_flex()
                    .h(px(40.))
                    .px(px(PANE_PX))
                    .gap(crate::ui::button_gap())
                    .items_center()
                    .child(file_icon(true).size_4().text_color(folder_color()))
                    .child(div().flex_1().child(Input::new(input).small()))
                    .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("f-make-{side}"))).primary().small().label("Make").on_click(cx.listener(move |this, _, _, cx| {
                        let name = this.panes[side].new_folder.as_ref().map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default();
                        this.make_folder(side, name, cx);
                    }))))
                    .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("f-make-no-{side}"))).ghost().small().label("Cancel").on_click(cx.listener(move |this, _, _, cx| {
                        this.panes[side].new_folder = None;
                        cx.notify();
                    })))),
            );
        }
        if p.loading && p.listing.is_none() {
            for i in 0..5 {
                list = list.child(h_flex().h(px(ROW_H)).px(px(PANE_PX)).gap_3().items_center().child(crate::ui::skeleton_line(&theme, format!("f-sk-{side}-{i}"), 180.)).child(div().flex_1()).child(crate::ui::skeleton_line(&theme, format!("f-sk2-{side}-{i}"), 60.)));
            }
        } else if let Some(e) = &p.error {
            list = list.child(div().px(px(PANE_PX)).py_3().text_sm().text_color(theme.danger).child(e.clone()));
        } else if rows.is_empty() && p.new_folder.is_none() && self.transfers.read(cx).copying_into(&p.place.key(), &p.path).is_empty() {
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
                    Some(input) => div().flex_1().child(Input::new(&input).small()).into_any_element(),
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
                .child(cols(name_el, size, time, Some(owner)))
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
            .when(narrow, |p| p.overflow_y_scroll())
            .child(h_flex().min_h(px(32.)).items_center().child(div().flex_1().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Files")).child(h_flex().gap_3().items_center().children(stale).child(refresh)))
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
        let mut list = v_flex().id("del-list").max_h(px(240.)).overflow_y_scroll().rounded(px(8.)).border_1().border_color(theme.border).py_1();
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
        v_flex().gap(px(14.)).child(list).child(div().text_sm().text_color(theme.muted_foreground).child(self.note))
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
    use super::os_path;

    #[test]
    fn locations_as_the_system_writes_them() {
        assert_eq!(os_path("/opt/sentient/backups", false), "/opt/sentient/backups");
        assert_eq!(os_path("/C:/Users/ops/file.txt", true), "C:\\Users\\ops\\file.txt");
        assert_eq!(os_path("/home/x", true), "\\home\\x");
    }
}
