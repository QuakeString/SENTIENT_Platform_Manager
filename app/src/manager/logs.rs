// Copyright © 2016-2026 The SENTIENT Authors

//! Logs: the tail of the platform's log, refreshed while "Follow" is on,
//! filtered by a search term, and exportable to a file.

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, h_flex,
    button::Button,
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

use crate::installation::InstallationRecord;
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
        let rx = cx.prompt_for_new_path(&home, Some("sentient.log"));
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
        let shown = lines.len();
        page.child(
            h_flex()
                .gap_3()
                .items_center()
                .child(div().w_80().child(Input::new(&self.filter)))
                .child(
                    Checkbox::new("follow")
                        .label("Follow")
                        .checked(self.follow)
                        .on_change(cx.listener(|this, value: &bool, _, cx| {
                            this.follow = *value;
                            if this.follow {
                                this.start_polling(cx);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("refresh")
                        .outline()
                        .label("Refresh")
                        .on_click(cx.listener(|this, _, _, cx| this.start_polling(cx))),
                )
                .child(
                    Button::new("export")
                        .outline()
                        .label("Export…")
                        .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
                )
                .child(div().flex_1())
                .child(div().text_xs().text_color(theme.muted_foreground).child(if shown == 1 { "1 line".to_string() } else { format!("{shown} lines") })),
        )
        .children(self.status.clone().map(|s| div().text_sm().child(s)))
        .child(
            ui::panel(&theme)
                .id("log-lines")
                .h(px(460.))
                .track_scroll(&self.scroll)
                .overflow_y_scroll()
                .gap_0()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .children(lines.into_iter().map(|line| div().child(line))),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_colour_codes() {
        let line = "\u{1b}[2m2026-10-16\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m started";
        assert_eq!(crate::ui::strip_ansi(line), "2026-10-16  INFO started");
    }
}
