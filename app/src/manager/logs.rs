// Copyright © 2016-2026 The SENTIENT Authors

//! Logs: one view for this computer and for a remote server. Pick a source
//! (SENTIENT, Database, ClickHouse, Gateway or All, from what the install
//! has), filter by level and text, follow it live, select lines to copy, and
//! save it. Every line reads as columns: local time, source (in All), level,
//! message.

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
const FOLLOW_EVERY: Duration = Duration::from_secs(3);

/// Reads one source: (source, lines, since midnight) → raw text, or why not.
pub type LoadFn = Arc<dyn Fn(Source, u32, bool) -> Result<String, String> + Send + Sync>;
/// The sources there are, or why they could not be read.
pub type SourcesFn = Arc<dyn Fn() -> Result<Vec<Source>, String> + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Levels {
    All,
    Warnings,
    Errors,
}

pub struct LogView {
    load: LoadFn,
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
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LogView {
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
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Filter lines")));
        let _subscriptions = vec![cx.subscribe_in(&filter, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.selected.clear();
                cx.notify();
            }
        })];
        let mut v = Self {
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
            _poll: None,
            _subscriptions,
        };
        // Screens: open on a given source ("all" for All).
        #[cfg(debug_assertions)]
        if let Ok(k) = std::env::var("SPM_LOG_SOURCE") {
            v.pick = if k == "all" { None } else { Source::from_key(&k) };
        }
        v.read_sources(cx);
        v.start(cx);
        v
    }

    /// This computer's logs.
    pub fn local(record: InstallationRecord, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let r1 = record.clone();
        let load: LoadFn = Arc::new(move |s, n, today| Ok(logsource::read(&r1, s, n, today)));
        let sources: SourcesFn = Arc::new(move || Ok(logsource::available(&record)));
        let mut v = Self::new(load, sources, window, cx);
        v.shown_on = Some(super::Page::Logs);
        v
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
        let sources: SourcesFn = Arc::new(move || crate::remote::log_sources(&inst));
        Self::new(load, sources, window, cx)
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

    /// Read now and, while following, every few seconds.
    fn start(&mut self, cx: &mut Context<Self>) {
        let load = self.load.clone();
        self._poll = Some(cx.spawn(async move |this, cx| loop {
            let Ok((pick, sources, paused, on)) = this.update(cx, |v, _| (v.pick, v.sources.clone(), v.paused, v.shown_on)) else { break };
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
            cx.background_executor().timer(FOLLOW_EVERY).await;
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
        self.status = Some(if n == 1 { tf!("{n} line copied.") } else { tf!("{n} lines copied.") }.into());
        cx.notify();
    }

    /// Save: the shown lines, or a longer read of the chosen source(s).
    fn save(&mut self, what: Option<(u32, bool)>, cx: &mut Context<Self>) {
        let all = self.pick.is_none();
        let shown: Vec<String> = self.shown(cx).iter().map(|l| Self::text_of(l, all)).collect();
        let (load, pick, sources) = (self.load.clone(), self.pick, self.sources.clone());
        let name = format!("sentient-{}-{}.log", pick.map(|s| s.key()).unwrap_or("all"), chrono::Local::now().format("%Y%m%d-%H%M"));
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
        let rx = crate::chooser::save(cx, t!("Save the log as"), &home, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let text = match what {
                None => shown.join("\n"),
                Some((n, today)) => cx
                    .background_executor()
                    .spawn(async move { Self::read(&load, pick, &sources, n, today).map(|ls| ls.iter().map(|l| Self::text_of(l, all)).collect::<Vec<_>>().join("\n")).unwrap_or_else(|e| e) })
                    .await,
            };
            let header = format!("# {}\n", logsource::zone_label());
            let result = std::fs::write(&path, header + &text + "\n");
            this.update(cx, |v, cx| {
                v.status = Some(match result {
                    Ok(()) => tf!("Saved to {}", path.display()).into(),
                    Err(e) => tf!("Could not save: {e}").into(),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
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
            sources = sources.child(ui::seg_item(&theme, SharedString::from(format!("log-src-{}", s.key())), crate::i18n::t(s.label()), self.pick == Some(s)).on_click(cx.listener(move |v, _, _, cx| {
                v.pick = Some(s);
                v.selected.clear();
                v.start(cx);
                cx.notify();
            })));
        }
        if self.sources.len() > 1 {
            sources = sources.child(ui::seg_item(&theme, "log-src-all", t!("All"), all).on_click(cx.listener(|v, _, _, cx| {
                v.pick = None;
                v.selected.clear();
                v.start(cx);
                cx.notify();
            })));
        }
        let levels = ui::seg_track(&theme).children([(Levels::All, t!("All levels")), (Levels::Warnings, t!("Warnings and errors")), (Levels::Errors, t!("Errors"))].into_iter().map(|(l, label)| {
            ui::seg_item(&theme, SharedString::from(format!("log-lvl-{label}")), label, self.levels == l).on_click(cx.listener(move |v, _, _, cx| {
                v.levels = l;
                v.selected.clear();
                cx.notify();
            }))
        }));
        let this = cx.entity().downgrade();
        let save = crate::ui::button("log-save").outline().label(t!("Save log")).dropdown_menu(move |menu, _, _| {
            let opt = |label: &'static str, what: Option<(u32, bool)>| {
                let t = this.clone();
                PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    t.update(cx, |v, cx| v.save(what, cx)).ok();
                })
            };
            menu.item(opt(t!("The lines shown"), None))
                .item(opt(t!("The last 1,000 lines"), Some((1_000, false))))
                .item(opt(t!("The last 10,000 lines"), Some((10_000, false))))
                .item(opt(t!("Today's lines"), Some((1_000_000, true))))
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
                        if count == 1 { t!("1 line").to_string() } else { tf!("{count} lines") },
                        logsource::zone_label()
                    )))
                    .child(div().flex_1())
                    .child(div().w(px(280.)).child(ui::probed_input(Input::new(&self.filter))))
                    .child(Checkbox::new("log-follow").label(t!("Follow")).checked(self.follow).on_change(cx.listener(|v, on: &bool, _, cx| {
                        v.follow = *on;
                        if *on {
                            v.selected.clear();
                            v.start(cx);
                        }
                        cx.notify();
                    })))
                    .child(crate::ui::probed(crate::ui::button("log-refresh").outline().label(t!("Refresh")).on_click(cx.listener(|v, _, _, cx| v.start(cx)))))
                    .child(crate::ui::probed(crate::ui::button("log-copy").outline().label(if selected_n > 0 { tf!("Copy {selected_n}") } else { t!("Copy").into() }).on_click(cx.listener(|v, _, _, cx| v.copy(cx)))))
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
                    r.child(h_flex().w(px(84.)).flex_shrink_0().child(div().px(px(6.)).rounded(px(4.)).bg(c.opacity(0.14)).text_color(c).text_size(px(11.5)).font_weight(FontWeight::MEDIUM).child(crate::i18n::t(l.source.label()))))
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
                            div().px(px(6.)).rounded(px(4.)).bg(c.opacity(0.14)).text_color(c).text_size(px(11.5)).font_weight(FontWeight::MEDIUM).child(crate::i18n::t(l.source.label())),
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
        v_flex()
            .gap_3()
            .child(toolbar)
            .children(self.status.clone().map(|s| div().text_sm().child(s)))
            .child(
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
                    .h(px(480.))
                    .track_scroll(&self.scroll)
                    .overflow_y_scroll()
                    .gap_0()
                    .p_2()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(12.5))
                    .when_some(self.error.clone().filter(|_| total == 0), |p, e| {
                        p.child(
                            v_flex().flex_1().items_center().justify_center().gap_3().font_family(theme.font_family.clone()).text_sm().child(div().text_color(theme.muted_foreground).child(t!("The log could not be read."))).child(
                                div().max_w(px(560.)).text_color(theme.danger).child(crate::ui::shown(e)),
                            )
                            .child(crate::ui::button("log-retry").outline().label(t!("Try again")).on_click(cx.listener(|v, _, _, cx| {
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
                                if self.lines.is_empty() { t!("No log lines") } else { t!("No lines match the filter") },
                            ),
                        )
                    })
                    .children(rows),
            )
    }
}

/// This computer's Logs page.
pub struct LogsPage {
    record: Option<InstallationRecord>,
    view: Option<Entity<LogView>>,
}

impl LogsPage {
    pub fn new(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = record.clone().map(|r| cx.new(|cx| LogView::local(r, window, cx)));
        Self { record, view }
    }
}

impl Render for LogsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(&theme, t!("Logs"), t!("SENTIENT's log and its database's, newest at the bottom."));
        match (&self.record, &self.view) {
            (Some(_), Some(v)) => page.child(v.clone()),
            _ => page.child(ui::not_installed(&theme)),
        }
    }
}
