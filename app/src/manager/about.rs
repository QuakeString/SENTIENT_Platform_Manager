// Copyright © 2016-2026 The SENTIENT Authors

//! About and preferences of the Platform Manager itself (not of SENTIENT:
//! that is Platform settings). Opens from the (i) at the bottom of the
//! menu. One column: the program and its version with its update state,
//! then preferences and support, and where it lives as a quiet footer.

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Disableable as _, Sizable as _, h_flex, v_flex, button::ButtonVariants as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::releases::Channel;

use crate::ui::{self, Tone};
use crate::updates::UpdateState;

/// "What's new" for the Platform Manager, on the Update screen.
pub struct ShowWhatsNew;

pub struct AboutPage {
    updates: Entity<UpdateState>,
    /// The support file just made, or why it was not.
    support: Option<Result<std::path::PathBuf, String>>,
    making: bool,
    _observe: Subscription,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<ShowWhatsNew> for AboutPage {}

impl AboutPage {
    pub fn new(updates: Entity<UpdateState>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&updates, |_, _, cx| cx.notify());
        Self { updates, support: None, making: false, _observe, _tasks: Vec::new() }
    }

    fn make_support_file(&mut self, cx: &mut Context<Self>) {
        self.making = true;
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { crate::support::create() }).await;
            this.update(cx, |page, cx| {
                page.support = Some(r);
                page.making = false;
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }
}

fn tile(theme: &gpui_kit::component::Theme, id: &'static str, icon: IconName, title: &'static str, about: impl Into<SharedString>) -> Stateful<Div> {
    h_flex()
        .id(id)
        .flex_1()
        .min_w(px(200.))
        .gap_3()
        .items_start()
        .p_3()
        .rounded(theme.radius)
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|d| d.border_color(theme.primary))
        .child(Icon::new(icon).size_4().text_color(theme.primary).mt_0p5())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(about.into())),
        )
}

fn pref_row(theme: &gpui_kit::component::Theme, label: &'static str, about: Option<&'static str>, control: impl IntoElement) -> Div {
    h_flex()
        .gap_3()
        .items_center()
        .py_2p5()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(div().text_sm().child(label))
                .when_some(about, |c, a| c.child(crate::ui::button(SharedString::from(format!("ahelp-{label}"))).ghost().xsmall().icon(IconName::Info).tooltip(a))),
        )
        .child(control)
}

impl Render for AboutPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let u = self.updates.read(cx);
        let version = crate::updates::manager_version();
        let checking = u.checking;
        let channel = u.prefs.channel;
        let daily = u.prefs.daily;
        let last = u.last_checked();
        let state = match (&u.found, u.manager()) {
            (_, Some(o)) => (Tone::Warn, format!("{} is available", o.version)),
            (Some(f), None) if f.manager.is_ok() => (Tone::Good, "Up to date".to_string()),
            _ => (Tone::Neutral, "Not checked".to_string()),
        };
        // The mockup's K6: one centred column; the product line with its
        // state and buttons on the right; captioned groups; icon tiles; the
        // paths as a footer line.
        let hero = ui::panel(&theme).child(
            h_flex()
                .gap_4()
                .items_center()
                .flex_wrap()
                .child(ui::product_logo(ui::Product::Manager, 48.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(200.))
                        .child(div().text_size(px(16.)).font_weight(FontWeight::MEDIUM).child("SENTIENT Platform Manager"))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(format!("Version {version} (build {}) · checked {last}", crate::updates::build_commit()))),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .items_center()
                        .child(ui::pill(&theme, state.0, state.1))
                        .child(crate::ui::button("about-notes").outline().label("What's new").on_click(cx.listener(|_, _, _, cx| cx.emit(ShowWhatsNew))))
                        .child(
                            crate::ui::button("about-check")
                                .outline()
                                .label(if checking { "Checking…" } else { "Check for updates" })
                                .disabled(checking)
                                .on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.check(cx)))),
                        ),
                ),
        );
        let channels = ui::seg_track(&theme).children(Channel::ALL.into_iter().map(|c| {
            ui::seg_item(&theme, SharedString::from(format!("about-channel-{}", c.label())), c.label(), c == channel)
                .on_click(cx.listener(move |this, _, _, cx| this.updates.update(cx, |u, cx| u.set_channel(c, cx))))
        }));
        let checks = ui::seg_track(&theme)
            .child(ui::seg_item(&theme, "about-daily", "Once a day", daily).on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.set_daily(true, cx)))))
            .child(ui::seg_item(&theme, "about-by-hand", "Only when I ask", !daily).on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.set_daily(false, cx)))));
        let prefs = ui::panel(&theme)
            .gap_0()
            .py_1()
            .child(pref_row(&theme, "Language", None, div().text_sm().child("English")).border_t_0())
            .child(pref_row(&theme, "Update channel", Some("Release candidates arrive earlier and are less tested."), channels))
            .child(pref_row(&theme, "Check for updates", None, checks));
        let log_path = crate::jobs::activity_log();
        let support = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_stretch()
                    .child(tile(&theme, "about-log", IconName::File, "Open the log", "What the Platform Manager did, with times.").on_click(move |_, _, cx| {
                        if log_path.exists() {
                            ui::open_url(&log_path.display().to_string(), cx);
                        }
                    }))
                    .child(
                        tile(&theme, "about-support", IconName::ArrowDown, "Create a support file", if self.making { "Collecting…" } else { "Logs, versions and settings in one file, without passwords." })
                            .on_click(cx.listener(|this, _, _, cx| this.make_support_file(cx))),
                    )
                    .child(tile(&theme, "about-docs", IconName::BookOpen, "Documentation", "Opens docs.invenia.in in the browser.").on_click(|_, _, cx| ui::open_url("https://docs.invenia.in", cx))),
            )
            .children(self.support.as_ref().map(|r| match r {
                Ok(p) => ui::outcome(&theme, Tone::Good, format!("Support file saved: {}", p.display())).into_any_element(),
                Err(e) => ui::outcome(&theme, Tone::Bad, format!("The support file could not be made: {e}")).into_any_element(),
            }));
        let caption = |t: &'static str| div().px_0p5().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(t.to_uppercase());
        // The paths: one line each, the middle cut when long, and Copy.
        let place = |id: &'static str, label: &'static str, path: String| {
            let copy = path.clone();
            h_flex()
                .gap_2()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .text_color(theme.muted_foreground)
                .child(div().whitespace_nowrap().child(format!("{label} {}", ui::middle_ellipsis(&path, 64))))
                .child(crate::ui::button(id).ghost().xsmall().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
        };
        let footer = v_flex()
            .gap_0p5()
            .child(place("copy-installed", "Installed in", crate::self_install::manager_dir().display().to_string()))
            .child(place("copy-data", "Data in", crate::updates::prefs_dir().display().to_string()));
        v_flex()
            .id("about")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_4()
            .bg(theme.muted)
            .items_center()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(800.))
                    .gap(px(14.))
                    .child(hero)
                    .child(v_flex().gap_2().child(caption("Preferences")).child(prefs))
                    .child(v_flex().gap_2().child(caption("Support")).child(support))
                    .child(footer),
            )
    }
}
