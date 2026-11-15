// Copyright © 2016-2026 The SENTIENT Authors

//! Logs: the tail of the platform's log, refreshed while "Follow" is on,
//! filtered by a search term, and exportable to a file.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, h_flex,
    button::Button,
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

use crate::installation::InstallationRecord;
use crate::log_level::Level;
use crate::ui;

const TAIL: u32 = 500;
const FOLLOW_EVERY: Duration = Duration::from_secs(2);

pub struct LogsPage {
    record: Option<InstallationRecord>,
    text: SharedString,
    filter: Entity<InputState>,
    follow: bool,
    status: Option<SharedString>,
    scroll: ScrollHandle,
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LogsPage {
    pub fn new(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter lines, e.g. ERROR"));
        let _subscriptions = vec![cx.subscribe_in(&filter, window, |_, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        let mut page = Self {
            record,
            text: SharedString::default(),
            filter,
            follow: true,
            status: None,
            scroll: ScrollHandle::new(),
            _poll: None,
            _subscriptions,
        };
        page.start_polling(cx);
        page
    }

    /// Reads the log now and, while following, every couple of seconds.
    fn start_polling(&mut self, cx: &mut Context<Self>) {
        let Some(record) = &self.record else { return };
        let platform = record.platform();
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let platform = platform.clone();
                let text = cx.background_executor().spawn(async move { platform.logs(TAIL) }).await;
                let follow = this
                    .update(cx, |page, cx| {
                        page.text = ui::strip_ansi(&text).into();
                        if page.follow {
                            page.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                        page.follow
                    })
                    .unwrap_or(false);
                if !follow {
                    break;
                }
                cx.background_executor().timer(FOLLOW_EVERY).await;
            }
        }));
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let text = self.text.to_string();
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let rx = crate::chooser::save(cx, "Save the log as", &home, Some("sentient.log"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let result = std::fs::write(&path, text);
            this.update(cx, |page, cx| {
                page.status = Some(match result {
                    Ok(()) => format!("Saved to {}", path.display()).into(),
                    Err(e) => format!("Could not save: {e}").into(),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for LogsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(&theme, "Logs", "The most recent lines of the SENTIENT log.");
        if self.record.is_none() {
            return page.child(ui::not_installed(&theme));
        }
        let needle = self.filter.read(cx).value().to_lowercase();
        let lines: Vec<SharedString> = self
            .text
            .lines()
            .filter(|line| needle.is_empty() || line.to_lowercase().contains(&needle))
            .map(|line| SharedString::from(line.to_string()))
            .collect();
        let this = cx.entity().downgrade();
        let (t1, t2, t3) = (this.clone(), this.clone(), this);
        page.child(toolbar(
            &theme,
            "",
            &self.filter,
            self.follow,
            lines.len(),
            Rc::new(move |on, _, cx| {
                t1.update(cx, |this, cx| {
                    this.follow = on;
                    if on {
                        this.start_polling(cx);
                    }
                    cx.notify();
                })
                .ok();
            }),
            Rc::new(move |_, cx| {
                t2.update(cx, |this, cx| this.start_polling(cx)).ok();
            }),
            Rc::new(move |_, cx| {
                t3.update(cx, |this, cx| this.export(cx)).ok();
            }),
        ))
        .children(self.status.clone().map(|s| div().text_sm().child(s)))
        .child(log_box(&theme, "log-lines", &self.scroll, lines))
    }
}

/// The log toolbar: filter, Follow, Refresh, Export and the line count.
/// The same on this computer and on a remote server (`id` keeps their ids apart).
#[allow(clippy::too_many_arguments)]
pub(crate) fn toolbar(
    theme: &gpui_kit::component::Theme,
    id: &str,
    filter: &Entity<InputState>,
    follow: bool,
    shown: usize,
    on_follow: Rc<dyn Fn(bool, &mut Window, &mut App)>,
    on_refresh: Rc<dyn Fn(&mut Window, &mut App)>,
    on_export: Rc<dyn Fn(&mut Window, &mut App)>,
) -> Div {
    h_flex()
        .gap_3()
        .items_center()
        .flex_wrap()
        .child(div().w_80().child(Input::new(filter)))
        .child(Checkbox::new(SharedString::from(format!("{id}follow"))).label("Follow").checked(follow).on_change(move |value: &bool, window, cx| on_follow(*value, window, cx)))
        .child(Button::new(SharedString::from(format!("{id}refresh"))).outline().label("Refresh").on_click(move |_, window, cx| on_refresh(window, cx)))
        .child(Button::new(SharedString::from(format!("{id}export"))).outline().label("Export…").on_click(move |_, window, cx| on_export(window, cx)))
        .child(div().flex_1())
        .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(if shown == 1 { "1 line".to_string() } else { format!("{shown} lines") }))
}

/// The log lines in a scrolling panel, each coloured by its level.
pub(crate) fn log_box(theme: &gpui_kit::component::Theme, id: &'static str, scroll: &ScrollHandle, lines: Vec<SharedString>) -> Stateful<Div> {
    ui::panel(theme)
        .id(id)
        .h(px(460.))
        .track_scroll(scroll)
        .overflow_y_scroll()
        .gap_0()
        .font_family(theme.mono_font_family.clone())
        .text_size(px(13.))
        // The page holds the last TAIL lines at most, and only the start of
        // each is read for its level, so every line can be coloured without
        // a virtual list (which would also stop long lines from wrapping).
        // Export saves the plain text.
        .children(lines.into_iter().map(|line| coloured_line(theme, line)))
}

/// Save log text to a file the user picks; `done` gets what happened.
pub(crate) fn export_text(text: String, cx: &mut App, done: impl FnOnce(String, &mut App) + 'static) {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(std::path::PathBuf::from).unwrap_or_default();
    let rx = crate::chooser::save(cx, "Save the log as", &home, Some("sentient.log"));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = rx.await else { return };
        let message = match std::fs::write(&path, text) {
            Ok(()) => format!("Saved to {}", path.display()),
            Err(e) => format!("Could not save: {e}"),
        };
        cx.update(|cx| done(message, cx));
    })
    .detach();
}

/// One log line: the timestamp muted, the rest by its level (ERROR red, WARN
/// amber, DEBUG and TRACE muted, others as normal text).
fn coloured_line(theme: &gpui_kit::component::Theme, line: SharedString) -> Div {
    let (level, ts) = crate::log_level::classify(&line);
    // Highlight colours are blended over the text colour: a see-through
    // muted would vanish into it, so mix it with the panel first.
    let muted = theme.background.blend(theme.muted_foreground);
    let colour = match level {
        Level::Error => Some(theme.danger),
        Level::Warn => Some(theme.warning_foreground),
        Level::Debug => Some(muted),
        Level::Info | Level::Plain => None,
    };
    let mut highlights = Vec::new();
    if ts > 0 {
        highlights.push((0..ts, HighlightStyle { color: Some(muted), ..Default::default() }));
    }
    if let Some(colour) = colour {
        if ts < line.len() {
            highlights.push((ts..line.len(), HighlightStyle { color: Some(colour), ..Default::default() }));
        }
    }
    let text = StyledText::new(line).with_highlights(highlights);
    div().when(level == Level::Error, |d| d.font_weight(FontWeight::MEDIUM)).child(text)
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_colour_codes() {
        let line = "\u{1b}[2m2026-10-16\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m started";
        assert_eq!(crate::ui::strip_ansi(line), "2026-10-16  INFO started");
    }
}
