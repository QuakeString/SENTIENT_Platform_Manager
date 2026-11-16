// Copyright © 2016-2026 The SENTIENT Authors

//! The backup folder chooser (mockup B2–B4), for this computer and for a
//! server over its agent: the folders where you are with their free space,
//! shortcuts (home, the current backup folder, mounted disks), a new folder,
//! and, for the folder picked, whether backups can be written there (B3: why
//! not, and one button that fixes it with the administrator password). When
//! the folder in use holds backups, a second step asks whether they move
//! along or stay (B4). The caller saves the choice.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::backups::DirList;
use crate::manager::backup::free_label as bytes_label;
use crate::remote::{self, Instance};

/// Where the folders are.
#[derive(Clone)]
pub enum Target {
    Local,
    Remote(Instance),
}

impl Target {
    fn list(&self, path: &str) -> Result<DirList, String> {
        match self {
            Target::Local => crate::backups::dir_list(Path::new(path)),
            Target::Remote(inst) => remote::dir_list(inst, path).map_err(|e| remote::split_failure(&e).0),
        }
    }

    fn mkdir(&self, path: &str, name: &str) -> Result<DirList, String> {
        match self {
            Target::Local => {
                let made = Path::new(path).join(name);
                std::fs::create_dir(&made).map_err(|e| format!("{name} cannot be made here: {e}"))?;
                crate::backups::dir_list(&made)
            }
            Target::Remote(inst) => remote::dir_mkdir(inst, path, name).map_err(|e| remote::split_failure(&e).0),
        }
    }

    fn make_writable(&self, path: &str) -> Result<DirList, String> {
        match self {
            Target::Local => {
                if cfg!(unix) && !crate::rootjob::is_root() {
                    // Pressed by the user: it may ask for the password, once.
                    let _action = sentient_installer_core::elevation::user_action();
                    crate::rootjob::run(&["dir-make-writable".to_string(), path.to_string()], &sentient_installer_core::progress::noop())?;
                } else {
                    crate::backups::make_writable(Path::new(path), None)?;
                }
                crate::backups::dir_list(Path::new(path))
            }
            Target::Remote(inst) => remote::dir_make_writable(inst, path).map_err(|e| remote::split_failure(&e).0),
        }
    }
}

/// What the page knows when it opens the chooser.
pub struct Opening {
    pub target: Target,
    /// "INVENIA IIoT", or "this computer".
    pub on: String,
    /// The backup folder in use.
    pub current: PathBuf,
    /// Backups in it now: how many and their size (B4 asks when any).
    pub here: (usize, u64),
    /// How many Keep holds (B4's line).
    pub keep: u32,
    /// The newest full backup's size ("a full backup is about …").
    pub full_bytes: Option<u64>,
    /// The folder chosen, and whether the backups there move along.
    pub done: Rc<dyn Fn(PathBuf, bool, &mut App)>,
    /// Another title than "Choose the backup folder on …" (a restore's
    /// folders); with it, the backups in the current folder are never asked
    /// about.
    pub title: Option<String>,
}

#[derive(Clone)]
enum Check {
    Checking,
    Got(DirList),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Choose,
    /// B4: move them (true) or leave them.
    Old(bool),
}

pub struct FolderChooser {
    o: Opening,
    path: Entity<InputState>,
    /// The folder whose folders are listed.
    listing: Option<DirList>,
    listing_error: Option<String>,
    loading: bool,
    /// The folder picked and what its check says.
    picked: PathBuf,
    check: Check,
    new_name: Option<Entity<InputState>>,
    busy: Option<&'static str>,
    stage: Stage,
    /// Answers to older requests are dropped.
    seq: u64,
    /// Text for the path field, put in at the next draw.
    show: Option<String>,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

const NEEDS_PASSWORD: &str = "@@SUDO_PASSWORD";
const TITLE_ICON: Lucide = Lucide::Folder;
pub const CHOOSE_W: f32 = 640.;
pub const OLD_W: f32 = 520.;

/// Open the chooser in the shared dialog.
pub fn open(o: Opening, window: &mut Window, cx: &mut App) {
    let title = o.title.clone().unwrap_or_else(|| format!("Choose the backup folder on {}", o.on));
    let view = cx.new(|cx| FolderChooser::new(o, window, cx));
    let footer = cx.new(|cx| ChooserFooter { _observe: cx.observe(&view, |_, _, cx| cx.notify()), chooser: view.clone() });
    crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: CHOOSE_W, icon: Some(Icon::new(TITLE_ICON)), on_close: None }, cx);
    crate::modal::set_footer(footer.into(), cx);
}

/// The chooser's buttons, in the dialog's footer: Cancel and Use this
/// folder (B2, B3), or Back and Use the new folder (B4).
struct ChooserFooter {
    chooser: Entity<FolderChooser>,
    _observe: Subscription,
}

impl Render for ChooserFooter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.chooser.read(cx);
        let (stage, usable, busy) = (c.stage, c.usable(), c.busy.is_some());
        let chooser = self.chooser.clone();
        let row = h_flex().w_full().gap_2().justify_end().items_center();
        match stage {
            Stage::Old(move_them) => {
                let (c1, c2) = (chooser.clone(), chooser);
                row.child(crate::ui::button("fc-back").outline().label("Back").on_click(move |_, _, cx| c1.update(cx, |this, cx| this.back(cx))))
                    .child(crate::ui::button("fc-use-new").primary().label("Use the new folder").on_click(move |_, _, cx| c2.update(cx, |this, cx| this.finish(move_them, cx))))
            }
            Stage::Choose => row
                .child(crate::ui::button("fc-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx)))
                .child(crate::ui::button("fc-use").primary().label("Use this folder").disabled(!usable || busy).on_click(move |_, _, cx| chooser.update(cx, |this, cx| this.use_it(cx)))),
        }
    }
}

impl FolderChooser {
    fn new(o: Opening, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let start = o.current.clone();
        let path = cx.new(|cx| InputState::new(window, cx).default_value(start.display().to_string()));
        let subs = vec![cx.subscribe(&path, |this: &mut Self, input, e: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = e {
                let typed = input.read(cx).value().trim().to_string();
                if !typed.is_empty() {
                    this.go(PathBuf::from(typed), cx);
                }
            }
        })];
        let mut me = Self {
            o,
            path,
            listing: None,
            listing_error: None,
            loading: true,
            picked: start.clone(),
            check: Check::Checking,
            new_name: None,
            busy: None,
            stage: Stage::Choose,
            seq: 0,
            show: None,
            _subs: subs,
            _tasks: Vec::new(),
        };
        // The folder in use is picked; its parent is listed, so its
        // neighbours show (the mockup).
        let mut pick = start.clone();
        // Screens: another folder picked (SPM_DEBUG_FOLDER_PICK), and B4
        // shown at once (SPM_DEBUG_FOLDER_STAGE=old).
        #[cfg(debug_assertions)]
        if let Ok(p) = std::env::var("SPM_DEBUG_FOLDER_PICK") {
            pick = PathBuf::from(p);
        }
        let parent = pick.parent().map(Path::to_path_buf).unwrap_or(pick.clone());
        me.load(parent, Some(pick), cx);
        me
    }

    /// List `dir`; `pick` is picked in it (else `dir` itself).
    fn load(&mut self, dir: PathBuf, pick: Option<PathBuf>, cx: &mut Context<Self>) {
        self.seq += 1;
        let seq = self.seq;
        self.loading = true;
        self.listing_error = None;
        self.picked = pick.clone().unwrap_or(dir.clone());
        self.check = Check::Checking;
        self.new_name = None;
        let target = self.o.target.clone();
        let (d, p) = (dir.display().to_string(), pick.as_ref().map(|p| p.display().to_string()));
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let listed = target.list(&d)?;
                    // The picked folder's own check (its writability), unless
                    // it is the listed one.
                    let checked = match p {
                        Some(p) if p != d => Some(target.list(&p)),
                        _ => None,
                    };
                    Ok::<_, String>((listed, checked))
                })
                .await;
            this.update(cx, |me, cx| {
                if me.seq != seq {
                    return;
                }
                me.loading = false;
                match r {
                    Ok((listed, checked)) => {
                        me.check = match checked {
                            Some(Ok(l)) => Check::Got(l),
                            Some(Err(e)) => Check::Failed(e),
                            None => Check::Got(listed.clone()),
                        };
                        me.listing = Some(listed);
                        #[cfg(debug_assertions)]
                        if std::env::var("SPM_DEBUG_FOLDER_STAGE").is_ok_and(|v| v == "old") && me.stage == Stage::Choose {
                            me.use_it(cx);
                        }
                    }
                    Err(e) => {
                        me.listing_error = Some(e.clone());
                        me.check = Check::Failed(e);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        self.show_path(cx);
        cx.notify();
    }

    /// The path field follows the picked folder (set at the next draw).
    fn show_path(&mut self, _cx: &mut Context<Self>) {
        self.show = Some(self.picked.display().to_string());
    }

    /// Open a folder: list it and pick it.
    fn go(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.load(dir, None, cx);
    }

    /// Pick a folder of the list (one click): check it.
    fn pick(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.seq += 1;
        let seq = self.seq;
        self.picked = dir.clone();
        self.check = Check::Checking;
        self.show_path(cx);
        let target = self.o.target.clone();
        let d = dir.display().to_string();
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { target.list(&d) }).await;
            this.update(cx, |me, cx| {
                if me.seq == seq {
                    me.check = match r {
                        Ok(l) => Check::Got(l),
                        Err(e) => Check::Failed(e),
                    };
                    cx.notify();
                }
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn make_folder(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.new_name else { return };
        let name = input.read(cx).value().trim().to_string();
        let Some(dir) = self.listing.as_ref().map(|l| l.path.clone()) else { return };
        if name.is_empty() || self.busy.is_some() {
            return;
        }
        self.busy = Some("mkdir");
        let target = self.o.target.clone();
        let d = dir.display().to_string();
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { target.mkdir(&d, &name) }).await;
            this.update(cx, |me, cx| {
                me.busy = None;
                match r {
                    Ok(made) => {
                        me.new_name = None;
                        let made_path = made.path.clone();
                        me.load(dir.clone(), Some(made_path), cx);
                    }
                    Err(e) => me.check = Check::Failed(e),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn make_writable(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("writable");
        let target = self.o.target.clone();
        let d = self.picked.display().to_string();
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    // sudo there wants a password nobody kept yet: ask first.
                    if let Target::Remote(inst) = &target {
                        if inst.user != "root" && remote::sudo_needs_password(inst) {
                            return Err(NEEDS_PASSWORD.to_string());
                        }
                    }
                    target.make_writable(&d)
                })
                .await;
            this.update(cx, |me, cx| {
                me.busy = None;
                if let (Err(NEEDS_PASSWORD), Target::Remote(inst)) = (r.as_ref().map_err(String::as_str), &me.o.target) {
                    let me2 = cx.entity().downgrade();
                    crate::manager::remote_pages::ask_sudo_password(inst.clone(), Rc::new(move |cx| {
                        me2.update(cx, |me, cx| me.make_writable(cx)).ok();
                    }), cx);
                    cx.notify();
                    return;
                }
                me.check = match r {
                    Ok(l) => Check::Got(l),
                    Err(e) => Check::Failed(e),
                };
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn usable(&self) -> bool {
        matches!(&self.check, Check::Got(l) if l.writable.is_ok() && !self.too_small(l))
    }

    fn too_small(&self, l: &DirList) -> bool {
        self.o.full_bytes.is_some_and(|b| l.free > 0 && l.free < b)
    }

    fn use_it(&mut self, cx: &mut Context<Self>) {
        if !self.usable() {
            return;
        }
        if self.picked == self.o.current {
            crate::modal::close(cx);
            return;
        }
        if self.o.here.0 > 0 {
            // B4: the backups in the old folder.
            self.stage = Stage::Old(false);
            crate::modal::set_title("Backups in the old folder", cx);
            crate::modal::set_width(OLD_W, cx);
            set_icon(Icon::new(Lucide::Archive), cx);
            cx.notify();
            return;
        }
        self.finish(false, cx);
    }

    fn finish(&mut self, move_them: bool, cx: &mut Context<Self>) {
        crate::modal::close(cx);
        (self.o.done)(self.picked.clone(), move_them, cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Choose;
        crate::modal::set_title(format!("Choose the backup folder on {}", self.o.on), cx);
        crate::modal::set_width(CHOOSE_W, cx);
        set_icon(Icon::new(TITLE_ICON), cx);
        cx.notify();
    }
}

fn set_icon(icon: Icon, cx: &mut App) {
    if let Some(m) = cx.try_global::<crate::modal::ModalSlot>().and_then(|s| s.0.clone()) {
        cx.set_global(crate::modal::ModalSlot(Some(crate::modal::Modal { icon: Some(icon), ..m })));
        cx.refresh_windows();
    }
}

impl Render for FolderChooser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if let Stage::Old(move_them) = self.stage {
            return self.old_stage(&theme, move_them, cx).into_any_element();
        }
        let mono = theme.mono_font_family.clone();
        let busy = self.busy;
        if let Some(text) = self.show.take() {
            self.path.update(cx, |s, cx| {
                if s.value() != text.as_str() {
                    s.set_value(text, window, cx);
                }
            });
        }

        // The path, up, and New folder.
        let up_to = self.listing.as_ref().and_then(|l| l.parent.clone());
        let top = h_flex()
            .gap_2()
            .items_center()
            .child(
                crate::ui::button("fc-up")
                    .outline()
                    .icon(Icon::new(Lucide::ChevronUp))
                    .disabled(up_to.is_none() || self.loading)
                    .tooltip("Up one folder")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(p) = up_to.clone() {
                            this.go(p, cx);
                        }
                    })),
            )
            .child(div().flex_1().min_w_0().font_family(mono.clone()).child(Input::new(&self.path)))
            .child(
                crate::ui::button("fc-new")
                    .outline()
                    .icon(Icon::new(Lucide::FolderPlus))
                    .label("New folder")
                    .disabled(self.listing.is_none() || self.loading)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
                        let sub = cx.subscribe(&input, |this: &mut Self, _, e: &InputEvent, cx| {
                            if let InputEvent::PressEnter { .. } = e {
                                this.make_folder(cx);
                            }
                        });
                        input.update(cx, |s, cx| s.focus(window, cx));
                        this._subs.push(sub);
                        this.new_name = Some(input);
                        cx.notify();
                    })),
            );

        // Shortcuts: the one the listed folder is in is marked.
        let listed = self.listing.as_ref().map(|l| l.path.clone());
        let shortcuts = self.listing.as_ref().map(|l| l.shortcuts.clone()).unwrap_or_default();
        let current = self.o.current.clone();
        let marked = listed.as_ref().and_then(|p| {
            shortcuts.iter().filter(|(_, s)| p.starts_with(s) && s.as_os_str().len() > 1).max_by_key(|(_, s)| s.as_os_str().len()).map(|(_, s)| s.clone())
        });
        let chips = h_flex().flex_wrap().gap_2().children(shortcuts.into_iter().enumerate().map(|(i, (label, p))| {
            let on = marked.as_ref() == Some(&p);
            let icon = match label.as_str() {
                "Home" => Lucide::House,
                "Current backup folder" => Lucide::Archive,
                _ => Lucide::HardDrive,
            };
            let to = if p == current { (p.parent().map(Path::to_path_buf).unwrap_or(p.clone()), Some(p.clone())) } else { (p.clone(), None) };
            h_flex()
                .id(("fc-chip", i))
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
                .child(Icon::new(icon).size_3p5())
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (dir, pick) = to.clone();
                    this.load(dir, pick, cx);
                }))
        }));

        // The folders.
        let header = h_flex()
            .h(px(30.))
            .px(px(14.))
            .items_center()
            .bg(theme.muted.opacity(0.5))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .text_color(theme.muted_foreground)
            .child(div().flex_1().child("Name"))
            .child("Free space");
        let mut rows = v_flex().id("fc-rows").max_h(px(5. * 34.)).overflow_y_scroll();
        if let Some(input) = &self.new_name {
            rows = rows.child(
                h_flex()
                    .h(px(40.))
                    .px(px(10.))
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::empty().path("icons/folder-fill.svg").size_4().text_color(folder_color()))
                    .child(div().flex_1().child(Input::new(input).small()))
                    .child(crate::ui::button("fc-make").primary().small().label("Make").loading(busy == Some("mkdir")).on_click(cx.listener(|this, _, _, cx| this.make_folder(cx))))
                    .child(crate::ui::button("fc-make-no").ghost().small().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.new_name = None;
                        cx.notify();
                    }))),
            );
        }
        if self.loading {
            for i in 0..3 {
                rows = rows.child(h_flex().h(px(34.)).px(px(14.)).gap_3().items_center().child(crate::ui::skeleton_line(&theme, format!("fc-sk-{i}"), 160.)).child(div().flex_1()).child(crate::ui::skeleton_line(&theme, format!("fc-sk2-{i}"), 50.)));
            }
        } else if let Some(e) = &self.listing_error {
            rows = rows.child(div().px(px(14.)).py_3().text_sm().text_color(theme.danger).child(e.clone()));
        } else if let Some(l) = &self.listing {
            if l.dirs.is_empty() && self.new_name.is_none() {
                rows = rows.child(div().px(px(14.)).py_3().text_sm().text_color(theme.muted_foreground).child("No folders here."));
            }
            for (i, (name, free)) in l.dirs.iter().enumerate() {
                let p = l.path.join(name);
                let on = self.picked == p;
                rows = rows.child(
                    h_flex()
                        .id(("fc-row", i))
                        .h(px(34.))
                        .px(px(14.))
                        .gap(px(10.))
                        .items_center()
                        .flex_shrink_0()
                        .text_sm()
                        .cursor_pointer()
                        .when(i > 0, |r| r.border_t_1().border_color(theme.border.opacity(0.6)))
                        .when(on, |r| r.bg(theme.sidebar_accent))
                        .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
                        .child(Icon::empty().path("icons/folder-fill.svg").size_4().text_color(folder_color()))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name.clone()))
                        .child(div().text_color(theme.muted_foreground).font_features(crate::ui::tabular()).child(bytes_label(*free)))
                        .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                            if e.click_count() >= 2 {
                                this.go(p.clone(), cx);
                            } else {
                                this.pick(p.clone(), cx);
                            }
                        })),
                );
            }
        }
        let table = v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden().child(header).child(rows);

        // B2/B3: what the picked folder allows.
        let full = self.o.full_bytes;
        let status = match &self.check {
            Check::Checking => h_flex().gap_2().items_center().text_sm().text_color(theme.muted_foreground).child("Checking the folder…").into_any_element(),
            Check::Failed(e) => status_line(&theme, false, e.clone()).into_any_element(),
            Check::Got(l) => match &l.writable {
                Ok(()) if self.too_small(l) => status_line(&theme, false, format!("Only {} free here; a full backup is about {}.", bytes_label(l.free), bytes_label(full.unwrap_or(0)))).into_any_element(),
                Ok(()) => status_line(
                    &theme,
                    true,
                    format!("Writable by SENTIENT · {} free{}", bytes_label(l.free), full.map(|b| format!(" · a full backup is about {}", bytes_label(b))).unwrap_or_default()),
                )
                .into_any_element(),
                Err(why) => v_flex()
                    .gap(px(10.))
                    .child(status_line(&theme, false, format!("SENTIENT cannot write here: {}", why.trim_end_matches('.').to_string() + ".")))
                    // A read-only disk: no button fixes that.
                    .when(l.fixable, |c| c.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                crate::ui::button("fc-writable")
                                    .outline()
                                    .icon(Icon::new(Lucide::Lock))
                                    .label("Make it writable…")
                                    .loading(busy == Some("writable"))
                                    .disabled(busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| this.make_writable(cx))),
                            )
                            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("Asks for the administrator password once.")),
                    ))
                    .into_any_element(),
            },
        };
        v_flex()
            .gap(px(12.))
            .child(top)
            .child(chips)
            .child(table)
            .child(status)
            .into_any_element()
    }
}

impl FolderChooser {
    /// B4: move the backups in the old folder along, or leave them.
    fn old_stage(&self, theme: &gpui_kit::component::Theme, move_them: bool, cx: &mut Context<Self>) -> Div {
        let mono = theme.mono_font_family.clone();
        let (n, bytes) = self.o.here;
        let seg = ui_seg(theme, move_them, cx);
        let explain = if move_them {
            "They are moved to the new folder; large files on another disk take a while. The list shows them there when it is done.".to_string()
        } else {
            format!("Left backups stay in the list and can still be restored. Automatic clean-up keeps the newest {} across both folders.", self.o.keep)
        };
        v_flex()
            .gap(px(12.))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_1()
                    .text_sm()
                    .child(format!("{n} backup{} ({}) {} in", if n == 1 { "" } else { "s" }, bytes_label(bytes), if n == 1 { "is" } else { "are" }))
                    .child(div().font_family(mono.clone()).text_size(px(13.)).child(format!("{}.", self.o.current.display()))),
            )
            .child(h_flex().child(seg))
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(explain))
            .child(
                h_flex()
                    .pt_2()
                    .pb(px(12.))
                    .gap_4()
                    .items_center()
                    .text_sm()
                    .child(div().w(px(170.)).flex_shrink_0().text_color(theme.muted_foreground).child("New folder"))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().font_family(mono).text_size(px(13.)).child(self.picked.display().to_string())),
            )
    }
}

fn ui_seg(theme: &gpui_kit::component::Theme, move_them: bool, cx: &mut Context<FolderChooser>) -> Div {
    crate::ui::seg_track(theme)
        .child(crate::ui::seg_item(theme, "fc-move", "Move them", move_them).on_click(cx.listener(|this, _, _, cx| {
            this.stage = Stage::Old(true);
            cx.notify();
        })))
        .child(crate::ui::seg_item(theme, "fc-leave", "Leave them", !move_them).on_click(cx.listener(|this, _, _, cx| {
            this.stage = Stage::Old(false);
            cx.notify();
        })))
}

fn status_line(theme: &gpui_kit::component::Theme, ok: bool, text: String) -> Div {
    let color = if ok { good_green(theme) } else { theme.danger };
    h_flex()
        .gap_2()
        .items_center()
        .text_sm()
        .text_color(color)
        .child(Icon::new(if ok { Lucide::CircleCheck } else { Lucide::CircleX }).size_4().flex_shrink_0())
        .child(div().min_w_0().child(text))
}

fn folder_color() -> Hsla {
    rgb(0xd9a23a).into()
}

/// The green of a good line (the toolkit's success is a fill): the
/// primary teal's neighbour, readable on both themes.
fn good_green(theme: &gpui_kit::component::Theme) -> Hsla {
    if theme.background.l < 0.5 { rgb(0x4ade80).into() } else { rgb(0x15803d).into() }
}
