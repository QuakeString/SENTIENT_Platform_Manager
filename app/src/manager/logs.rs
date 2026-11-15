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
    scroll: ScrollHandle,
    /// Selected lines, as indices into the shown list.
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    focus: FocusHandle,
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LogView {
    pub fn new(load: LoadFn, sources_fn: SourcesFn, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter lines"));
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
            scroll: ScrollHandle::new(),
            selected: BTreeSet::new(),
            anchor: None,
            focus: cx.focus_handle(),
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
        Self::new(load, sources, window, cx)
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
            let Ok((pick, sources)) = this.update(cx, |v, _| (v.pick, v.sources.clone())) else { break };
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
                        Err(e) => {
                            v.lines.clear();
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
        self.status = Some(format!("{n} line{} copied.", if n == 1 { "" } else { "s" }).into());
        cx.notify();
    }

    /// Save: the shown lines, or a longer read of the chosen source(s).
    fn save(&mut self, what: Option<(u32, bool)>, cx: &mut Context<Self>) {
        let all = self.pick.is_none();
        let shown: Vec<String> = self.shown(cx).iter().map(|l| Self::text_of(l, all)).collect();
        let (load, pick, sources) = (self.load.clone(), self.pick, self.sources.clone());
        let name = format!("sentient-{}-{}.log", pick.map(|s| s.key()).unwrap_or("all"), chrono::Local::now().format("%Y%m%d-%H%M"));
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
        let rx = crate::chooser::save(cx, "Save the log as", &home, Some(&name));
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
                    Ok(()) => format!("Saved to {}", path.display()).into(),
                    Err(e) => format!("Could not save: {e}").into(),
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
        // Opened from a Control row's "Logs": that source.
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
        let save = crate::ui::button("log-save").outline().label("Save log…").dropdown_menu(move |menu, _, _| {
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
            .child(h_flex().gap_3().flex_wrap().items_center().child(sources).child(levels))
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_center()
                    .child(div().w(px(280.)).child(Input::new(&self.filter)))
                    .child(Checkbox::new("log-follow").label("Follow").checked(self.follow).on_change(cx.listener(|v, on: &bool, _, cx| {
                        v.follow = *on;
                        if *on {
                            v.selected.clear();
                            v.start(cx);
                        }
                        cx.notify();
                    })))
                    .child(crate::ui::button("log-refresh").outline().label("Refresh").on_click(cx.listener(|v, _, _, cx| v.start(cx))))
                    .child(crate::ui::button("log-copy").outline().label(if selected_n > 0 { format!("Copy {selected_n}") } else { "Copy".into() }).on_click(cx.listener(|v, _, _, cx| v.copy(cx))))
                    .child(save)
                    .child(div().flex_1())
                    .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!(
                        "{} · {}",
                        if count == 1 { "1 line".to_string() } else { format!("{count} lines") },
                        logsource::zone_label()
                    ))),
            );
        let narrow = ui::narrow();
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
                .child(div().w(px(52.)).flex_shrink_0().font_weight(FontWeight::MEDIUM).text_color(colour(l.level)).child(level_word(l.level)));
            let message = div().flex_1().min_w_0().text_color(if l.level == Level::Error { theme.danger } else if l.level == Level::Debug { muted } else { theme.foreground }).child(l.message.clone());
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
                .child(div().w(px(52.)).flex_shrink_0().font_weight(FontWeight::MEDIUM).text_color(colour(l.level)).child(level_word(l.level)))
                // Wrapped lines stay in the message column.
                .child(div().flex_1().min_w_0().text_color(if l.level == Level::Error { theme.danger } else if l.level == Level::Debug { muted } else { theme.foreground }).child(l.message.clone()))
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
        let page = ui::page(&theme, "Logs", "SENTIENT's log and its database's, newest at the bottom.");
        match (&self.record, &self.view) {
            (Some(_), Some(v)) => page.child(v.clone()),
            _ => page.child(ui::not_installed(&theme)),
        }
    }
}
