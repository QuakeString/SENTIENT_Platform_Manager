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
    /// Saving the background preferences failed (start at sign-in).
    bg_error: Option<String>,
    _observe: Subscription,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<ShowWhatsNew> for AboutPage {}

impl AboutPage {
    /// Start at sign-in, the tray, and what closing the window does
    /// (crate::background). Without the tray there is no background part:
    /// the other two do not apply and are off.
    fn background_rows(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use gpui_kit::component::{Disableable as _, switch::Switch};
        let p = crate::bg_app::prefs(cx);
        let set = |f: fn(&mut crate::background::Prefs, bool)| {
            cx.listener(move |this: &mut Self, v: &bool, _, cx| {
                let mut p = crate::bg_app::prefs(cx);
                f(&mut p, *v);
                this.bg_error = crate::bg_app::set_prefs(p, cx).err();
                cx.notify();
            })
        };
        let tray_help = if p.show_tray && !crate::bg_app::tray_up(cx) {
            "This desktop shows no tray icons (on GNOME, the AppIndicator extension adds them): the Platform Manager runs as without a tray."
        } else {
            "An icon by the clock: click it to open or close the window; its menu has the latest notifications, the instances and Exit. The instances are checked in the background."
        };
        let mut rows = vec![
            pref_row(theme, "Show in the system tray", Some(tray_help), Switch::new("bg-tray").checked(p.show_tray).on_click(set(|p, v| p.show_tray = v))).into_any_element(),
            pref_row(
                theme,
                "Start at sign-in",
                Some("In the background, with the tray icon; the window opens when you click it."),
                Switch::new("bg-signin").checked(p.show_tray && crate::background::autostart_on()).disabled(!p.show_tray).on_click(set(|p, v| p.start_at_signin = v)),
            )
            .into_any_element(),
            pref_row(
                theme,
                "Closing the window quits",
                Some("Off: closing the window leaves the Platform Manager in the tray. Exit from the tray menu quits."),
                Switch::new("bg-close").checked(!p.show_tray || p.close_quits).disabled(!p.show_tray).on_click(set(|p, v| p.close_quits = v)),
            )
            .into_any_element(),
        ];
        if let Some(e) = &self.bg_error {
            rows.push(ui::outcome(theme, Tone::Bad, format!("Could not change starting at sign-in: {e}")).into_any_element());
        }
        rows
    }
}

impl AboutPage {
    pub fn new(updates: Entity<UpdateState>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&updates, |_, _, cx| cx.notify());
        Self { updates, support: None, making: false, bg_error: None, _observe, _tasks: Vec::new() }
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
        // Every row as tall as one with a segmented control, so switch
        // rows and choice rows line up.
        .min_h(px(51.))
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
        // Only two pills: "Up to date" or "Update available". Before a check
        // has answered in this run there is no pill and the line says so.
        let state: Option<(Tone, &str)> = match (&u.found, u.manager()) {
            (_, Some(_)) => Some((Tone::Warn, "Update available")),
            (Some(f), None) if f.manager.is_ok() => Some((Tone::Good, "Up to date")),
            _ => None,
        };
        // The version alone until a check has answered, then what it found.
        let checked_line = match (&u.found, u.manager()) {
            _ if checking => " · checking for updates…".to_string(),
            (_, Some(o)) => format!(" · {} available", o.version),
            (Some(f), None) if f.manager.is_ok() => " · up to date".to_string(),
            _ => String::new(),
        };
        let _ = &last;
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
                        .child(div().text_sm().text_color(theme.muted_foreground).child(format!("Version {version} (build {}){checked_line}", crate::updates::build_commit()))),
                )
                .child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .flex_wrap()
                        .items_center()
                        .when_some(state, |r, (tone, text)| r.child(ui::pill(&theme, tone, text)))
                        .child(crate::ui::probed(crate::ui::button("about-notes").outline().label("What's new").on_click(cx.listener(|_, _, _, cx| cx.emit(ShowWhatsNew)))))
                        .child(crate::ui::probed(
                            crate::ui::button("about-check")
                                .outline()
                                .label(if checking { "Checking" } else { "Check for updates" })
                                .disabled(checking)
                                .on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.check(cx)))),
                        )),
                ),
        );
        let channels = ui::seg_track(&theme).children(Channel::ALL.into_iter().map(|c| {
            ui::seg_item(&theme, SharedString::from(format!("about-channel-{}", c.label())), c.label(), c == channel)
                .on_click(cx.listener(move |this, _, _, cx| this.updates.update(cx, |u, cx| u.set_channel(c, cx))))
        }));
        let checks = ui::seg_track(&theme)
            .child(ui::seg_item(&theme, "about-daily", "Daily", daily).on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.set_daily(true, cx)))))
            .child(ui::seg_item(&theme, "about-by-hand", "Manually", !daily).on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.set_daily(false, cx)))));
        // Appearance (the shell mockup's S7): the theme, and the menu showing
        // the desktop behind the window where the desktop can blur it.
        let choice = crate::theme::Choice::saved();
        let themes = ui::seg_track(&theme).children(crate::theme::Choice::ALL.into_iter().map(|c| {
            ui::seg_item_with(&theme, SharedString::from(format!("about-theme-{}", c.label())), crate::manager::shell::theme_icon(c), c.label(), c == choice).on_click(cx.listener(move |_, _, window, cx| {
                crate::theme::choose(c, Some(window), cx);
                cx.notify();
            }))
        }));
        let supported = crate::manager::shell::see_through_supported();
        let see_through = gpui_kit::component::switch::Switch::new("see-through")
            .checked(supported && crate::manager::shell::Appearance::get().see_through)
            .disabled(!supported)
            .on_click(cx.listener(|_, v: &bool, window, cx| {
                let v = *v;
                crate::manager::shell::Appearance::set(|a| a.see_through = v);
                window.set_background_appearance(if v { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Opaque });
                cx.notify();
                window.refresh();
            }));
        let appearance = ui::panel(&theme)
            .gap_0()
            .py_1()
            .child(pref_row(&theme, "Theme", None, themes).border_t_0())
            .child(pref_row(
                &theme,
                "Translucent menu",
                Some(crate::manager::shell::see_through_why_not().unwrap_or("Shows the desktop blurred behind the menu (KDE Plasma, Hyprland, picom, Windows). On X11 a change shows after the Platform Manager starts again.")),
                see_through,
            ));
        let prefs = ui::panel(&theme)
            .gap_0()
            .py_1()
            .child(pref_row(&theme, "Language", None, {
                // One list of languages (shell::LANGUAGES), each by its own name.
                let current = crate::manager::shell::Appearance::get().language;
                let name = crate::manager::shell::LANGUAGES.iter().find(|(c, _)| *c == current).map(|(_, n)| *n).unwrap_or("English");
                { use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem}; crate::ui::button("about-language")
                    .outline()
                    .h(px(32.))
                    .label(name)
                    .icon(IconName::ChevronDown)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for (code, native) in crate::manager::shell::LANGUAGES {
                            let code = code.to_string();
                            menu = menu.item(PopupMenuItem::new(*native).checked(code == current).on_click(move |_, window, cx| {
                                let code = code.clone();
                                crate::manager::shell::Appearance::set(|a| a.language = code);
                                window.refresh();
                                let _ = cx;
                            }));
                        }
                        menu
                    }) }
            }).border_t_0())
            .child(pref_row(&theme, "Update channel", Some("Release candidates arrive earlier and are less tested."), channels))
            .child(pref_row(&theme, "Check for updates", None, checks))
            .children(self.background_rows(&theme, cx));
        let log_path = crate::jobs::activity_log();
        let support = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_stretch()
                    .child(tile(&theme, "about-log", IconName::File, "Open the log", "Every action and error, with timestamps.").on_click(move |_, window, cx| {
                        // In the app, with the Logs page's tools: never an
                        // outside program that may fail silently.
                        let path = log_path.clone();
                        let view = cx.new(|cx| crate::manager::logs::LogView::file(path, window, cx));
                        crate::modal::open(crate::modal::Modal { title: "Platform Manager log".into(), body: view.into(), width: 1000., icon: None, on_close: None }, cx);
                    }))
                    .child(
                        tile(&theme, "about-support", IconName::ArrowDown, "Create a support file", if self.making { "Collecting…" } else { "Logs, versions and settings in one file. Passwords are left out." })
                            .on_click(cx.listener(|this, _, _, cx| this.make_support_file(cx))),
                    )
                    .child(tile(&theme, "about-docs", IconName::BookOpen, "Documentation", "Guides and help on docs.invenia.in.").on_click(|_, _, cx| ui::open_url("https://docs.invenia.in", cx))),
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
                .max_w_full()
                .gap(crate::ui::button_gap())
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .text_color(theme.muted_foreground)
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("{label} {}", ui::middle_ellipsis(&path, 64))))
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
            .p(px(crate::ui::PAGE_PAD))
            .bg(theme.muted)
            .items_center()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(800.))
                    .gap(px(14.))
                    .child(hero)
                    .child(v_flex().gap_2().child(caption("Appearance")).child(appearance))
                    .child(v_flex().gap_2().child(caption("Preferences")).child(prefs))
                    .child(v_flex().gap_2().child(caption("Support")).child(support))
                    .child(footer),
            )
    }
}
