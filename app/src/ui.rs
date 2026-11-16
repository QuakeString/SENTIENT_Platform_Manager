// Copyright © 2016-2026 The SENTIENT Authors

//! Small presentation pieces shared by the screens: the white panel the
//! design system puts all content on, page headings, and status pills.

use gpui_kit::component::{
    Theme, h_flex, v_flex,
    button::ButtonVariants as _,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

/// Where GNOME runs on Wayland it does not draw window frames for
/// applications (no xdg-decoration), and this program's window came out
/// square, without shadow or title bar. Through XWayland GNOME draws its own
/// frame: title bar, rounded corners, shadow. `SPM_WAYLAND=1` keeps Wayland.
/// The Wayland display is kept for the programs this one starts ([`open_url`]).
pub fn prefer_native_frame() {
    #[cfg(target_os = "linux")]
    {
        let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        let gnome = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("GNOME"));
        if gnome && set("WAYLAND_DISPLAY") && set("DISPLAY") && !set("SPM_WAYLAND") {
            let wayland = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_default();
            std::env::set_var(ORIGINAL_WAYLAND, wayland);
            std::env::remove_var("WAYLAND_DISPLAY");
        }
    }
}

#[cfg(target_os = "linux")]
const ORIGINAL_WAYLAND: &str = "SPM_ORIGINAL_WAYLAND_DISPLAY";

/// Open `url` in the browser, on the desktop's own display server.
pub fn open_url(url: &str, cx: &mut App) {
    // Linux: xdg-open, and its answer is checked: a link or folder that
    // cannot be opened says so instead of doing nothing.
    #[cfg(target_os = "linux")]
    {
        let url = url.to_string();
        cx.spawn(async move |cx| {
            let u = url.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut c = std::process::Command::new("xdg-open");
                    c.arg(&u);
                    if let Some(wayland) = std::env::var_os(ORIGINAL_WAYLAND) {
                        c.env("WAYLAND_DISPLAY", wayland);
                    }
                    match c.output() {
                        Ok(o) if o.status.success() => Ok(()),
                        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                        Err(e) => Err(format!("xdg-open: {e}")),
                    }
                })
                .await;
            if let Err(e) = result {
                cx.update(|cx| show_error("Could not open it", format!("{url} could not be opened.{}", if e.is_empty() { String::new() } else { format!("\n{e}") }), cx));
            }
        })
        .detach();
    }
    #[cfg(not(target_os = "linux"))]
    cx.open_url(url);
}

/// A short error in the shared dialog, with Close (no window needed).
pub fn show_error(title: impl Into<SharedString>, text: impl Into<SharedString>, cx: &mut App) {
    show_error_with(title, text, None, cx)
}

/// An error in the shared dialog: the headline, the details (the step that
/// failed and what it printed) to read and Copy, and Retry when given.
/// What a failure's retry button says: "Ask again" when the failure is a
/// closed password prompt (sentient_installer_core::elevation), else `usual`.
pub fn retry_label(failure: &str, usual: &'static str) -> &'static str {
    if sentient_installer_core::elevation::ElevationError::is_cancelled(failure) { "Ask again" } else { usual }
}

pub fn show_error_with(title: impl Into<SharedString>, text: impl Into<SharedString>, retry: Option<std::rc::Rc<dyn Fn(&mut App)>>, cx: &mut App) {
    struct ErrorBody(SharedString, Option<std::rc::Rc<dyn Fn(&mut App)>>);
    impl Render for ErrorBody {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_kit::component::ActiveTheme as _;
            use gpui_kit::component::button::ButtonVariants as _;
            let theme = cx.theme().clone();
            let (head, detail) = crate::remote::split_failure(&self.0);
            let copy = self.0.to_string();
            let again = retry_label(&self.0, "Retry");
            gpui_kit::component::v_flex()
                .gap_3()
                .child(div().text_sm().child(head))
                .when_some(detail.filter(|d| !d.trim().is_empty()), |c, d| {
                    c.child(div().id("error-detail").max_h(px(300.)).overflow_y_scroll().p_2().rounded(theme.radius).bg(theme.muted).font_family(theme.mono_font_family.clone()).text_size(px(12.5)).child(d.trim().to_string()))
                })
                .child(
                    gpui_kit::component::h_flex()
                        .gap(crate::ui::button_gap())
                        .justify_end()
                        .child(crate::ui::probed(button("error-copy").outline().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone())))))
                        .when_some(self.1.clone(), |r, retry| {
                            r.child(crate::ui::probed(button("error-retry").outline().label(again).on_click(move |_, _, cx| {
                                crate::modal::close(cx);
                                retry(cx);
                            })))
                        })
                        .child(crate::ui::probed(button("error-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx)))),
                )
        }
    }
    let text = text.into();
    let view = cx.new(|_| ErrorBody(text, retry));
    crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: 560., icon: None, on_close: None }, cx);
}

/// What finished, in the shared dialog: the message as plain lines (a
/// success is never read as an error, however long it is), and Close.
pub fn show_info(title: impl Into<SharedString>, text: impl Into<SharedString>, cx: &mut App) {
    struct InfoBody(SharedString);
    impl Render for InfoBody {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_kit::component::button::ButtonVariants as _;
            let theme = cx.theme().clone();
            gpui_kit::component::v_flex()
                .gap_3()
                .child(outcome(&theme, Tone::Good, info_lines(&self.0).0))
                .children(info_lines(&self.0).1.map(|rest| div().text_sm().child(rest)))
                .child(gpui_kit::component::h_flex().justify_end().child(button("info-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))))
        }
    }
    let view = cx.new(|_| InfoBody(text.into()));
    crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: 560., icon: None, on_close: None }, cx);
}

/// A success message as a first sentence and the rest (a note to read),
/// never split as an error's headline and raw details.
pub fn info_lines(text: &str) -> (String, Option<String>) {
    let t = text.trim();
    match t.find(". ").filter(|i| *i + 2 < t.len()) {
        Some(i) => (t[..=i].to_string(), Some(t[i + 2..].trim().to_string())),
        None => (t.to_string(), None),
    }
}

/// Open SENTIENT on `port`: full screen like the kiosk shortcut when the
/// install chose it (`kiosk`: the same `--open-sentient --kiosk` command,
/// starting page included), else in the default browser.
pub fn open_sentient(port: u16, kiosk: bool, cx: &mut App) {
    if kiosk {
        if let Ok(me) = std::env::current_exe() {
            let mut command = std::process::Command::new(me);
            command.args(sentient_installer_core::kiosk::KIOSK_LAUNCH_ARGS.split(' '));
            #[cfg(target_os = "linux")]
            if let Some(wayland) = std::env::var_os(ORIGINAL_WAYLAND) {
                command.env("WAYLAND_DISPLAY", wayland);
            }
            if command.spawn().is_ok() {
                return;
            }
        }
    }
    open_url(&format!("http://localhost:{port}"), cx);
}

/// What the start delay does, for its (i).
pub const START_DELAY_HELP: &str = "Seconds SENTIENT waits after Windows starts before it starts itself, its database first. \
0 starts it together with Windows. A few seconds let other programs and the network settle first; an operator screen shows \
\"SENTIENT is starting\" meanwhile. Change it later in Platform settings, under Startup.";

/// "Start delay [ 15 ] seconds (i)": the installer's and Startup's field.
pub fn start_delay_row(theme: &Theme, state: &Entity<gpui_kit::component::input::InputState>) -> Div {
    use gpui_kit::component::{Sizable as _, button::ButtonVariants as _, input::Input, IconName};
    gpui_kit::component::h_flex()
        .gap_2()
        .items_center()
        .child(div().text_sm().child("Start delay"))
        .child(div().w_20().child(Input::new(state)))
        .child(div().text_sm().text_color(theme.muted_foreground).child("seconds"))
        .child(crate::ui::button("start-delay-help").ghost().xsmall().icon(IconName::Info).tooltip(START_DELAY_HELP))
}

/// The corner radius of panels and cards (buttons and inputs keep the
/// theme's smaller one).
pub const PANEL_RADIUS: f32 = 12.;

/// A content panel: surface colour, hairline border, panel radius.
/// The one spacing scale (his rc12-19 ask: everything a little tighter):
/// the page's padding (left equal to top), the space between cards, and a
/// card's padding.
pub const PAGE_PAD: f32 = crate::theme::PAGE_GAP;
pub const GAP: f32 = crate::theme::PAGE_GAP;
pub const CARD_PY: f32 = 14.;
pub const CARD_PX: f32 = 16.;

pub fn panel(theme: &Theme) -> Div {
    v_flex()
        .relative()
        .gap_3()
        .py(px(CARD_PY))
        .px(px(CARD_PX))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        // Layout tests: every card reports its bounds (debug builds only).
        .child(layout_probe("panel"))
}

/// The scrolling page area to the right of the navigation.
pub fn page(theme: &Theme, title: &'static str, summary: &'static str) -> Stateful<Div> {
    v_flex()
        .id("page")
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_y_scroll()
        // The page: PAGE_PAD all round, GAP between blocks, a 22 px title
        // with its line of text as one block.
        .p(px(PAGE_PAD))
        .gap(px(GAP))
        .bg(theme.muted)
        .child(
            v_flex()
                .gap_0p5()
                .min_h(px(32.))
                .justify_center()
                .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child(title))
                .when(!summary.is_empty(), |c| c.child(div().text_sm().text_color(theme.muted_foreground).child(summary))),
        )
}

/// A page whose title row ends with an action (the remote pages' Read
/// again), in the same place on every page.
pub fn page_with(theme: &Theme, title: &'static str, summary: &'static str, action: impl IntoElement) -> Stateful<Div> {
    v_flex().id("page").flex_1().min_w_0().h_full().overflow_y_scroll().p(px(PAGE_PAD)).gap(px(GAP)).bg(theme.muted).child(
        h_flex()
            .gap_3()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child(title))
                    .when(!summary.is_empty(), |c| c.child(div().text_sm().text_color(theme.muted_foreground).child(summary))),
            )
            .child(action),
    )
}

/// A refresh that failed while the page keeps what it read before: a muted
/// "Couldn't refresh · last updated HH:MM" beside the title's Refresh, the
/// reason in its (i). One line, cut rather than wrapped.
/// Screens: every page with data shows its stale note (SPM_DEBUG_STALE=<why>).
pub fn debug_stale() -> Option<String> {
    if cfg!(debug_assertions) { std::env::var("SPM_DEBUG_STALE").ok() } else { None }
}

pub fn stale_note(theme: &Theme, id: &'static str, why: &str, at: Option<&str>) -> AnyElement {
    let text = match at {
        Some(t) => format!("Couldn't refresh · last updated {t}"),
        None => "Couldn't refresh".to_string(),
    };
    h_flex()
        .gap_1()
        .items_center()
        .min_w_0()
        .flex_shrink(1.)
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text))
        .child(info(SharedString::from(id), SharedString::from(why.to_string())))
        .into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Warn,
    Bad,
    Neutral,
}

/// A rounded status label. Colour is never the only signal: the text says it.
pub fn pill(theme: &Theme, tone: Tone, text: impl Into<SharedString>) -> Div {
    let (bg, fg) = match tone {
        Tone::Good => (theme.success, theme.success_foreground),
        Tone::Warn => (theme.warning, theme.warning_foreground),
        // Soft like the others: a red tint with red text.
        Tone::Bad => (theme.danger.opacity(0.14), theme.danger),
        Tone::Neutral => (theme.secondary, theme.secondary_foreground),
    };
    h_flex()
        .px_2p5()
        .py_0p5()
        .rounded_full()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .bg(bg)
        .text_color(fg)
        .child(text.into())
}

/// A two-column label / value row.
pub fn fact(theme: &Theme, label: &'static str, value: impl Into<SharedString>) -> Div {
    h_flex()
        .gap_4()
        .text_sm()
        .child(div().w(px(168.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
        // The value takes the rest and wraps (never clipped at the card's edge).
        .child(div().flex_1().min_w_0().child(value.into()))
}

/// The label column of a card's facts where two cards share a row (Status).
pub const CARD_LABEL_W: f32 = 130.;

/// A label / value row in a half-width card: the 130 px label column.
pub fn card_fact(theme: &Theme, label: &'static str, value: impl Into<SharedString>) -> Div {
    h_flex()
        .gap_4()
        .text_sm()
        .child(div().w(px(CARD_LABEL_W)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
        .child(div().flex_1().min_w_0().child(value.into()))
}

/// [`card_fact`] while its value loads.
pub fn card_fact_loading(theme: &Theme, label: &'static str, w: f32) -> Div {
    h_flex()
        .gap_4()
        .text_sm()
        .child(div().w(px(CARD_LABEL_W)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
        .child(skeleton_line(theme, label, w))
}

/// Status's top row: the SENTIENT card and Services side by side at equal
/// height, a page gap apart; one above the other in a window below
/// SIDE_BY_SIDE_W.
pub fn card_pair(left: Div, right: Div) -> Div {
    if side_by_side() {
        h_flex()
            .w_full()
            .items_stretch()
            .gap(px(GAP))
            .child(left.flex_1().min_w_0())
            .child(right.flex_1().min_w_0())
    } else {
        v_flex().w_full().gap(px(GAP)).child(left).child(right)
    }
}

/// A card's last block (its actions, its totals) under a hairline, at the
/// card's foot when its neighbour is taller.
pub fn card_foot(theme: &Theme, content: impl IntoElement) -> [Div; 2] {
    [div().flex_1(), div().pt_3().border_t_1().border_color(theme.border).child(content)]
}

/// A fact with its explanation behind an (i), never as a line of text.
pub fn fact_help(theme: &Theme, label: &'static str, value: impl Into<SharedString>, help: impl Into<SharedString>) -> Div {
    let help: SharedString = help.into();
    h_flex()
        .gap_4()
        .text_sm()
        .items_center()
        .child(div().w(px(168.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
        .child(div().min_w_0().child(value.into()))
        .child(info(SharedString::from(format!("fact-help-{label}")), help))
}

/// An (i) whose tooltip holds the help text (the help rule: never inline).
pub fn info(id: SharedString, help: SharedString) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(22.))
        .flex_shrink_0()
        .rounded(px(5.))
        .cursor_default()
        .hover(|d| d.bg(black().opacity(0.06)))
        .child(InfoIcon)
        .tooltip(move |_, cx| cx.new(|_| InfoTip(help.clone())).into())
}

/// The (i) itself, in the muted text colour of its theme.
#[derive(IntoElement)]
struct InfoIcon;

use gpui_kit::component::ActiveTheme as _;

impl RenderOnce for InfoIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        gpui_kit::component::Icon::new(gpui_kit::component::IconName::Info).size(px(14.)).text_color(cx.theme().muted_foreground)
    }
}

/// The one tooltip style: an inverted surface (dark on the light theme, a
/// lighter raised one on the dark theme), at most 320 px wide, wrapping, a
/// little below the pointer so it never sits on the (i) itself.
struct InfoTip(SharedString);

impl Render for InfoTip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().mode.is_dark();
        // Inverted against the window, so it never reads as one more panel:
        // light on the dark theme, dark on the light one.
        let (bg, fg, border) = if dark { (rgb(0xeef3f5), rgb(0x16211f), rgb(0xffffff)) } else { (rgb(0x1f2a30), rgb(0xffffff), rgb(0x1f2a30)) };
        // Laid out at its smallest size, so the width is set: the longest
        // line's length, at most 320 px.
        let longest = self.0.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
        let w = (longest * 6.9 + 22.).clamp(60., 320.);
        let shadow = BoxShadow { color: black().opacity(if dark { 0.45 } else { 0.25 }), offset: point(px(0.), px(4.)), blur_radius: px(12.), spread_radius: px(0.), inset: false };
        div().pt(px(8.)).pl(px(4.)).child(
            gpui_kit::component::v_flex()
                .w(px(w))
                .gap(px(6.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .bg(bg)
                .border_1()
                .border_color(border)
                .shadow(vec![shadow])
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(fg)
                .children(self.0.split("\n\n").map(|p| {
                    // A paragraph of "Name: what it is" lines (the choices
                    // of a setting): one line each, the name in medium.
                    v_flex().gap(px(4.)).children(p.trim().lines().map(|line| {
                        let line = line.trim().to_string();
                        match line.split_once(": ").filter(|(name, _)| name.chars().count() <= 24 && !name.contains('.')) {
                            Some((name, _)) if p.trim().lines().count() > 1 => {
                                let n = name.len() + 1;
                                div().child(StyledText::new(line.clone()).with_highlights([(0..n, HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() })])).into_any_element()
                            }
                            _ => div().child(line).into_any_element(),
                        }
                    }))
                })),
        )
    }
}

/// Shown on every manager screen when this computer has no installation.
pub fn not_installed(theme: &Theme) -> Div {
    // One short line; the how-to behind the (i). The buttons wrap on a
    // narrow window. Removing the Platform Manager lives in Repair.
    panel(theme)
        .child(div().font_weight(FontWeight::MEDIUM).child("SENTIENT isn't installed on this computer"))
        .child(
            gpui_kit::component::h_flex()
                .gap_1()
                .items_center()
                .child(div().text_sm().text_color(theme.muted_foreground).child("Install it from the internet or from a SENTIENT package folder."))
                .child(info(
                    "install-help".into(),
                    "Download and install: Setup downloads the version you choose from GitHub.\n\nInstall from local files: without internet, choose the unpacked SENTIENT package folder (the one with SENTIENT-Setup and payload).".into(),
                )),
        )
        .child(button_row(vec![
            {
                crate::ui::button("install-sentient").primary().label("Download and install SENTIENT").on_click(|_, window, cx| crate::standalone::install_sentient(window, cx)).into_any_element()
            },
            {
                crate::ui::button("install-sentient-package").outline().label("Install from local files…").on_click(|_, window, cx| crate::standalone::install_sentient_from_package(window, cx)).into_any_element()
            },
        ]))
}

/// A row of buttons that wraps on a narrow window: a button never shrinks
/// below its label or runs out of its card (each reports its bounds to the
/// layout probe).
pub fn button_row(buttons: Vec<AnyElement>) -> Div {
    button_group(buttons).flex_wrap()
}

/// Buttons side by side on one line, theme::BUTTON_GAP apart, centred on
/// one line (each reports its bounds to the layout probe).
pub fn button_group(buttons: Vec<AnyElement>) -> Div {
    gpui_kit::component::h_flex().items_center().gap(button_gap()).children(buttons.into_iter().map(probed))
}

/// The one gap between buttons, and between a button group and the text
/// or pill beside it (theme::BUTTON_GAP).
pub fn button_gap() -> Pixels {
    px(crate::theme::BUTTON_GAP)
}

/// A button that reports its bounds to the layout test (the gap check):
/// for a button placed outside button_group/button_row.
pub fn probed(b: impl IntoElement) -> Div {
    div().relative().flex_shrink_0().child(b).child(layout_probe("button"))
}

/// A page's own notice, directly in the page (never inside a card): an
/// icon, a headline, the rest of the text, and an optional action.
pub fn notice(theme: &Theme, tone: Tone, text: impl Into<SharedString>) -> Div {
    notice_with(theme, tone, text, None)
}

pub fn notice_with(theme: &Theme, tone: Tone, text: impl Into<SharedString>, mut action: Option<AnyElement>) -> Div {
    let (bg, fg, icon) = match tone {
        Tone::Good => (theme.success, theme.success_foreground, gpui_kit::component::IconName::CircleCheck),
        Tone::Warn => (theme.warning, theme.warning_foreground, gpui_kit::component::IconName::TriangleAlert),
        Tone::Bad => (theme.danger.opacity(0.12), theme.danger, gpui_kit::component::IconName::CircleX),
        Tone::Neutral => (theme.secondary, theme.secondary_foreground, gpui_kit::component::IconName::Info),
    };
    let text: SharedString = text.into();
    let mut lines = text.lines().map(String::from);
    let head = lines.next().unwrap_or_default();
    let rest: Vec<String> = lines.collect();
    gpui_kit::component::h_flex()
        .gap_3()
        .items_start()
        .px_4()
        .py_3()
        .rounded(px(PANEL_RADIUS))
        .bg(bg)
        .text_color(fg)
        .child(gpui_kit::component::Icon::new(icon).text_color(fg).mt(px(2.)))
        .child(
            gpui_kit::component::v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .text_sm()
                .child(div().font_weight(FontWeight::MEDIUM).child(head))
                .children(rest.into_iter().map(|l| div().child(l)))
                // Narrow: the action goes under the text, not beside it.
                .when(narrow() && action.is_some(), |c| c.child(gpui_kit::component::h_flex().mt_2().children(action.take()))),
        )
        .children(action)
}

/// The result of a job: like a pill in colour, but a block that wraps, one
/// row per line, for messages longer than a word or two.
pub fn outcome(theme: &Theme, tone: Tone, text: impl Into<SharedString>) -> Div {
    let (bg, fg) = match tone {
        Tone::Good => (theme.success, theme.success_foreground),
        Tone::Warn => (theme.warning, theme.warning_foreground),
        // Soft like the others: a red tint with red text.
        Tone::Bad => (theme.danger.opacity(0.14), theme.danger),
        Tone::Neutral => (theme.secondary, theme.secondary_foreground),
    };
    let text: SharedString = text.into();
    let lines: Vec<String> = text.lines().map(String::from).collect();
    v_flex()
        .gap_1()
        .px_3()
        .py_2()
        .rounded(theme.radius)
        .text_sm()
        .bg(bg)
        .text_color(fg)
        .children(lines.into_iter().enumerate().map(|(i, line)| {
            div().when(i == 0, |d| d.font_weight(FontWeight::MEDIUM)).child(line)
        }))
}

pub use crate::maintenance_ops::strip_ansi;

/// How far a planned step got: done, running, failed or not yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepMark {
    Done,
    Running,
    Failed,
    Todo,
}

impl StepMark {
    /// ✓ for a match, ✕ for a mismatch.
    pub fn from_ok(ok: bool) -> StepMark {
        if ok { StepMark::Done } else { StepMark::Failed }
    }
}

/// Mark `planned` against the steps a job started: every started step but
/// the last is done; the last is running, done (the job finished) or failed.
/// A step the job started that is not planned still counts (a job may say
/// more than the plan shows) without being drawn.
pub fn mark_steps(planned: &[String], started: &[SharedString], finished: Option<bool>) -> Vec<(String, StepMark)> {
    let last = started.last().map(|s| s.to_string());
    let seen = |name: &str| started.iter().any(|s| s.as_ref() == name);
    planned
        .iter()
        .map(|p| {
            let mark = if !seen(p) {
                StepMark::Todo
            } else if last.as_deref() != Some(p.as_str()) {
                StepMark::Done
            } else {
                match finished {
                    None => StepMark::Running,
                    Some(true) => StepMark::Done,
                    Some(false) => StepMark::Failed,
                }
            };
            (p.clone(), mark)
        })
        .collect()
}

/// A checklist of steps: ✓ done, ● running, ✕ failed, ○ not yet.
pub fn steps(theme: &Theme, marked: &[(String, StepMark)]) -> Div {
    v_flex().gap_1().text_sm().children(marked.iter().map(|(name, mark)| {
        let (sign, color, strong) = match mark {
            StepMark::Done => ("✓", theme.success_foreground, false),
            StepMark::Running => ("●", theme.primary, true),
            StepMark::Failed => ("✕", theme.danger, true),
            StepMark::Todo => ("○", theme.muted_foreground, false),
        };
        gpui_kit::component::h_flex()
            .gap_2()
            .child(div().w_4().flex_shrink_0().text_color(color).child(sign))
            .child(
                div()
                    .when(*mark == StepMark::Todo, |d| d.text_color(theme.muted_foreground))
                    .when(*mark == StepMark::Failed, |d| d.text_color(theme.danger))
                    .when(strong, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(name.clone()),
            )
    }))
}

/// Release notes, newest first: version and date, the summary, then the
/// Added / Changed / Fixed lists.
pub fn release_notes(theme: &Theme, notes: &[crate::notes::Notes]) -> Div {
    v_flex().gap_3().children(notes.iter().map(|n| {
        v_flex()
            .gap_1()
            .child(
                gpui_kit::component::h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(div().font_weight(FontWeight::MEDIUM).child(n.version.clone()))
                    .when(!n.date.is_empty(), |r| r.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(n.date.clone()))),
            )
            .when(!n.summary.is_empty(), |c| c.child(div().text_sm().text_color(theme.muted_foreground).child(n.summary.clone())))
            .children(n.sections.iter().map(|(name, entries)| {
                v_flex()
                    .gap_0p5()
                    .pt_1()
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.primary).child(name.to_uppercase()))
                    .children(entries.iter().map(|e| {
                        gpui_kit::component::h_flex()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .child(div().flex_shrink_0().child("•"))
                            .child(div().min_w_0().child(e.clone()))
                    }))
            }))
    }))
}

#[cfg(test)]
mod step_tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{mark_steps, StepMark};
    use gpui_kit::SharedString;

    #[test]
    fn steps_are_marked_by_what_the_job_started() {
        let planned: Vec<String> = ["A", "B", "C"].map(String::from).to_vec();
        let started: Vec<SharedString> = vec!["A".into(), "B".into()];
        let m: Vec<StepMark> = mark_steps(&planned, &started, None).into_iter().map(|(_, m)| m).collect();
        assert_eq!(m, [StepMark::Done, StepMark::Running, StepMark::Todo]);
        let m: Vec<StepMark> = mark_steps(&planned, &started, Some(false)).into_iter().map(|(_, m)| m).collect();
        assert_eq!(m, [StepMark::Done, StepMark::Failed, StepMark::Todo]);
    }
}

/// A product's own logo, in colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Product {
    Manager,
    Sentient,
    Gateway,
}

pub fn product_logo(p: Product, size: f32) -> impl IntoElement {
    let path = match p {
        Product::Manager if size <= 24. => "logos/spm-small.png",
        Product::Manager => "logos/spm.png",
        Product::Sentient => "logos/sentient-mark.svg",
        Product::Gateway => "logos/gateway.svg",
    };
    img(path).size(px(size)).flex_shrink_0()
}

/// A brand mark (one colour) by its file under `logos/`.
pub fn brand_mark(name: &str, color: Hsla, size: f32) -> impl IntoElement {
    svg().path(SharedString::from(format!("logos/{name}.svg"))).size(px(size)).flex_shrink_0().text_color(color)
}

/// A database's logo in its brand colour; ClickHouse's yellow sits on a
/// dark tile so it reads on light backgrounds too.
pub fn database_logo(name: &str, size: f32) -> AnyElement {
    match name {
        "clickhouse" => div()
            .flex_shrink_0()
            .size(px(size + 4.))
            .rounded(px(3.))
            .bg(rgb(0x1f1f1f))
            .flex()
            .items_center()
            .justify_center()
            .child(brand_mark("clickhouse", rgb(0xffcc01).into(), size - 2.))
            .into_any_element(),
        "timescale" => brand_mark("timescale", rgb(0xfdb515).into(), size).into_any_element(),
        _ => brand_mark("postgresql", rgb(0x4169e1).into(), size).into_any_element(),
    }
}

/// The logos of an installation's databases, by its type.
pub fn flavor_logos(flavor: sentient_installer_core::stack::Flavor, size: f32) -> Div {
    use sentient_installer_core::stack::Flavor;
    let mut row = gpui_kit::component::h_flex().gap_1().items_center().child(database_logo("postgresql", size));
    match flavor {
        Flavor::Clickhouse => row = row.child(database_logo("clickhouse", size)),
        Flavor::Timescaledb => row = row.child(database_logo("timescale", size)),
        Flavor::Stock => {}
    }
    row
}

/// A segmented choice: one track with the theme's radius and a 1px border,
/// and segments inside it with equal padding. The chosen segment is filled
/// with the primary colour. The same height as a standard button.
pub fn seg_track(theme: &Theme) -> Div {
    // The mockup's segmented control: one bordered strip, segments edge to
    // edge, the chosen one tinted with primary text.
    gpui_kit::component::h_flex()
        .flex_none()
        .items_center()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .overflow_hidden()
}

/// One segment of a [`seg_track`]; give it `.on_click(...)`.
pub fn seg_item(theme: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(30.))
        .px(px(11.))
        .flex()
        .items_center()
        .text_sm()
        .whitespace_nowrap()
        .cursor_pointer()
        .when(on, |d| d.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground).font_weight(FontWeight::MEDIUM))
        .when(!on, |d| d.text_color(theme.muted_foreground).hover(|d| d.bg(theme.list_hover).text_color(theme.foreground)))
        .child(label.into())
}

/// `seg_item` with a 15 px icon before its label.
pub fn seg_item_with(theme: &Theme, id: impl Into<ElementId>, icon: gpui_kit::component::Icon, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    let label = label.into();
    let colour = if on { theme.sidebar_accent_foreground } else { theme.muted_foreground };
    seg_item(theme, id, "", on).gap(px(6.)).child(icon.size(px(15.)).text_color(colour)).child(label)
}

/// A row of sibling boxes or cards: all stretch to the tallest, tops
/// aligned, wrapping on narrow windows. Use it for every row of boxes.
pub fn box_row() -> Div {
    gpui_kit::component::h_flex().flex_wrap().gap_3().items_stretch()
}

/// Whether the desktop asks for less motion (KDE's animation speed at 0,
/// GNOME's animations off, or SPM_REDUCE_MOTION=1). Read once.
pub fn reduce_motion() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        if std::env::var("SPM_REDUCE_MOTION").is_ok_and(|v| v == "1") {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            let out = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            if out("kreadconfig6", &["--file", "kdeglobals", "--group", "KDE", "--key", "AnimationDurationFactor"]).is_some_and(|v| v == "0") {
                return true;
            }
            if out("gsettings", &["get", "org.gnome.desktop.interface", "enable-animations"]).is_some_and(|v| v == "false") {
                return true;
            }
        }
        false
    })
}

/// Fade (and rise a little) when shown; `key` restarts it when it changes.
pub fn appear<E: IntoElement + Styled + 'static>(el: E, key: impl Into<ElementId>, ms: u64) -> AnyElement {
    if reduce_motion() {
        return el.into_any_element();
    }
    el.with_animation(key, Animation::new(std::time::Duration::from_millis(ms)).with_easing(ease_out_quint()), |el, d| el.opacity(d))
        .into_any_element()
}

/// One choice of several, with a real radio mark, a title and a line about
/// it. Give it `.on_click(...)`.
pub fn radio_card(theme: &Theme, id: impl Into<ElementId>, title: impl Into<SharedString>, desc: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    radio_card_el(theme, id, div().font_weight(FontWeight::MEDIUM).child(title.into()).into_any_element(), desc, selected, true)
}

/// `radio_card` with its title as an element (a title with a pill).
/// `interactive`: it can be picked (a pointer and the hover border); off,
/// it is drawn faded by the caller and reacts to nothing.
pub fn radio_card_el(theme: &Theme, id: impl Into<ElementId>, title: AnyElement, desc: impl Into<SharedString>, selected: bool, interactive: bool) -> Stateful<Div> {
    gpui_kit::component::h_flex()
        .id(id)
        .gap_3()
        .py(px(14.))
        .px(px(16.))
        .items_start()
        .rounded(px(12.))
        .border_1()
        .border_color(if selected { theme.primary } else { theme.border })
        .when(selected, |c| c.bg(theme.sidebar_accent))
        .when(interactive, |c| c.cursor_pointer().hover(|c| c.border_color(theme.primary)))
        .child(
            div()
                .mt(px(2.))
                .size(px(18.))
                .flex_shrink_0()
                .rounded_full()
                .border_2()
                .border_color(if selected { theme.primary } else { theme.muted_foreground })
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |r| r.child(div().size(px(8.)).rounded_full().bg(theme.primary))),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(title)
                .child(div().text_sm().text_color(theme.muted_foreground).child(desc.into())),
        )
}

/// A service's plain name: SENTIENT, Database (PostgreSQL), ClickHouse,
/// Gateway; None for one not recognised.
pub fn service_label(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    if n.contains("postgres") {
        Some("Database (PostgreSQL)")
    } else if n.contains("clickhouse") {
        Some("ClickHouse")
    } else if n.contains("gateway") {
        Some("Gateway")
    } else if n.starts_with("sentient") || n.contains("sentientserver") {
        Some("SENTIENT")
    } else {
        None
    }
}

/// Per-row actions on Status: (service name, action) and (service name)
/// for its log.
pub type ServiceAct = std::rc::Rc<dyn Fn(&str, &'static str, &mut Window, &mut App)>;
pub type ServiceLog = std::rc::Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The hover of a Stop button: the danger tint, border and text.
pub fn stop_hover(theme: &Theme) -> (Hsla, Hsla, Hsla) {
    (theme.danger.opacity(0.14), theme.danger, theme.danger)
}

/// A service row's 32 px outline icon button with an 18 px icon and its
/// name as the tooltip. It hovers teal, or red for Stop.
pub fn svc_icon_button(theme: &Theme, id: SharedString, icon: gpui_kit::component::Icon, tip: &'static str, enabled: bool, stop: bool) -> gpui_kit::component::button::Button {
    use gpui_kit::component::{Disableable as _, Sizable as _};
    let (bg, border, fg) = if stop { stop_hover(theme) } else { (theme.primary.opacity(0.12), theme.primary, theme.primary) };
    button(id)
        .outline()
        .with_size(px(32.))
        .icon(icon)
        .icon_size(px(18.))
        .tooltip(tip)
        .hover_colors(bg, border, fg)
        .disabled(!enabled)
}

/// A Services row's text line, and its height (two lines and their gap).
const SVC_LINE_H: f32 = 20.;
const SVC_ROW_H: f32 = 2. * SVC_LINE_H + 2.;

/// The Services card's rows: the state, the plain name, the unit or
/// container name and its uptime (wrapping in a narrow card), then Start,
/// Stop, Restart and Logs, always shown, those that do not apply off. The
/// same on this computer and on a remote server.
pub fn service_rows(
    theme: &Theme,
    services: impl IntoIterator<Item = (SharedString, bool, bool, SharedString)>,
    actions: (ServiceAct, ServiceLog, Option<String>),
) -> Vec<Div> {
    // One order on every page: SENTIENT, Database, ClickHouse, Gateway.
    let rank = |n: &str| match service_label(n) {
        Some("SENTIENT") => 0,
        Some("Database (PostgreSQL)") => 1,
        Some("ClickHouse") => 2,
        Some("Gateway") => 3,
        _ => 4,
    };
    let (act, log, busy_name) = actions;
    let any_busy = busy_name.is_some();
    let mut services: Vec<_> = services.into_iter().collect();
    services.sort_by_key(|(n, ..)| rank(n));
    services
        .into_iter()
        .map(|(name, running, starting, detail)| {
            let (tone, label) = if running { (Tone::Good, "Running") } else if starting { (Tone::Warn, "Starting") } else { (Tone::Bad, "Stopped") };
            let plain = service_label(&name);
            let busy = busy_name.as_ref().is_some_and(|b| *b == *name);
            // The pill hugs its word; the uptime goes on its own line under
            // the name, so the name and the unit keep the row's room.
            let state_pill = if busy { pill(theme, Tone::Warn, "Working…") } else { pill(theme, tone, label) };
            let piece = |d: Div| d.min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis();
            let names = gpui_kit::component::v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    gpui_kit::component::h_flex()
                        .min_w_0()
                        .flex_wrap()
                        .gap_x_2()
                        .gap_y_0p5()
                        // The plain name wraps rather than being cut.
                        .child(div().min_w_0().child(SharedString::from(plain.unwrap_or(&name).to_string())))
                        .when(plain.is_some(), |r| {
                            r.child(piece(div()).text_size(px(12.)).font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(name.clone()))
                        }),
                )
                .when(!detail.is_empty(), |r| r.child(piece(div()).text_color(theme.muted_foreground).child(detail.clone())));
            let icon = |action: &'static str, icon: gpui_kit::component::Icon, tip: &'static str, enabled: bool| {
                let (act, n) = (act.clone(), name.to_string());
                svc_icon_button(theme, SharedString::from(format!("svc-{action}-{name}")), icon, tip, enabled && !any_busy, action == "stop")
                    .on_click(move |_, window, cx| act(&n, action, window, cx))
            };
            let (log, n) = (log.clone(), name.to_string());
            // One height for every row (two lines of names), whether the
            // names wrap or not; the pill on the names' first line, the
            // pill and names centred together, the icons centred on the row.
            gpui_kit::component::h_flex()
                .gap(button_gap())
                .text_sm()
                .line_height(px(SVC_LINE_H))
                .items_center()
                .min_h(px(SVC_ROW_H))
                .child(
                    gpui_kit::component::h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_start()
                        .gap_3()
                        .child(gpui_kit::component::h_flex().flex_shrink_0().h(px(SVC_LINE_H)).items_center().child(state_pill))
                        .child(names),
                )
                .child(
                    button_group(vec![
                        icon("start", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Play), "Start", !running).into_any_element(),
                        icon("stop", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Square), "Stop…", running).into_any_element(),
                        icon("restart", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::RotateCw), "Restart", running).into_any_element(),
                        svc_icon_button(theme, SharedString::from(format!("svc-log-{name}")), gpui_kit::component::Icon::empty().path("icons/file-text.svg"), "Logs", true, false)
                            .on_click(move |_, window, cx| log(&n, window, cx))
                            .into_any_element(),
                    ])
                    .flex_shrink_0(),
                )
        })
        .collect()
}

/// The Services card's foot: "2 of 3 running".
pub fn services_foot(theme: &Theme, running: usize, total: usize) -> Div {
    gpui_kit::component::h_flex()
        .items_center()
        .text_size(px(13.))
        .text_color(theme.muted_foreground)
        .child(format!("{running} of {total} running"))
}

/// A popover's pointer under it, at `x` from its left: a 12 px wide
/// triangle in the popover's colour, outlined in its border colour.
pub fn pointer_down(fill: Hsla, border: Hsla, x: Pixels) -> impl IntoElement {
    div().absolute().left(x).bottom(px(-7.)).w(px(14.)).h(px(8.)).child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let o = bounds.origin;
                let tri = |inset: f32| {
                    let mut p = gpui_kit::Path::new(point(o.x + px(inset), o.y));
                    p.line_to(point(o.x + px(14. - inset), o.y));
                    p.line_to(point(o.x + px(7.), o.y + px(8. - inset)));
                    p
                };
                window.paint_path(tri(0.), border);
                window.paint_path(tri(1.4), fill);
            },
        )
        .size_full(),
    )
}

/// A popover's pointer above it (the popover hangs below its anchor).
pub fn pointer_up(fill: Hsla, border: Hsla, x: Pixels) -> impl IntoElement {
    div().absolute().left(x).top(px(-7.)).w(px(14.)).h(px(8.)).child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let o = bounds.origin;
                let tri = |inset: f32| {
                    let mut p = gpui_kit::Path::new(point(o.x + px(inset), o.y + px(8.)));
                    p.line_to(point(o.x + px(14. - inset), o.y + px(8.)));
                    p.line_to(point(o.x + px(7.), o.y + px(inset)));
                    p
                };
                window.paint_path(tri(0.), border);
                window.paint_path(tri(1.4), fill);
            },
        )
        .size_full(),
    )
}

/// One row of a popover menu.
pub enum MenuRow {
    Item {
        id: SharedString,
        icon: Option<AnyElement>,
        label: SharedString,
        /// A second, quieter text after the label (an address).
        secondary: Option<SharedString>,
        /// The chosen one: tinted, with a check at the right.
        on: bool,
        click: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    },
    Divider,
}

/// The app's popover menu (the theme menu, the instance switcher): a
/// 12 px panel with tinted chosen row and a check at the right, and its
/// pointer toward the anchor at `pointer_x`, above (`up`) or below it. The
/// caller places it (deferred, absolute) and closes it on `on_close`.
pub fn popover_menu(theme: &Theme, id: &'static str, width: f32, rows: Vec<MenuRow>, up: bool, pointer_x: Pixels, on_close: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    use gpui_kit::component::{h_flex, v_flex};
    let rows = rows.into_iter().map(|r| match r {
        MenuRow::Divider => div().my_1().mx_1().h(px(1.)).bg(theme.border).into_any_element(),
        // The same rows as the toolkit's menus: 32 px, an 18 px icon slot,
        // 10 px to the text.
        MenuRow::Item { id, icon, label, secondary, on, click } => h_flex()
            .id(id)
            .gap(px(10.))
            .items_center()
            .px_2()
            .h(px(32.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_sm()
            .when(on, |r| r.bg(theme.primary.opacity(0.10)).text_color(theme.primary).font_weight(FontWeight::MEDIUM))
            .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
            .children(icon.map(|i| div().relative().flex_shrink_0().size(px(18.)).flex().items_center().justify_center().child(i).child(layout_probe("menu-item-icon"))))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().flex_shrink_0().child(label))
                    .when_some(secondary, |r, s| r.child(div().min_w_0().overflow_hidden().text_ellipsis().font_weight(FontWeight::NORMAL).text_color(theme.muted_foreground).child(s))),
            )
            .when(on, |r| r.child(gpui_kit::component::Icon::new(gpui_kit::component::IconName::Check).size(px(18.))))
            .on_click(move |_, window, cx| click(window, cx))
            .into_any_element(),
    });
    div()
        .relative()
        .w(px(width))
        .child(
            v_flex()
                .id(id)
                .occlude()
                .p_1()
                .gap_px()
                .rounded(px(PANEL_RADIUS))
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .children(rows)
                .on_mouse_down_out(move |_, window, cx| on_close(window, cx)),
        )
        .map(|d| if up { d.child(pointer_up(theme.popover, theme.border, pointer_x)) } else { d.child(pointer_down(theme.popover, theme.border, pointer_x)) })
        // Opening: a quick fade, moving 4 px from its anchor (140 ms).
        .map(|d| {
            if reduce_motion() {
                d.into_any_element()
            } else {
                d.with_animation(SharedString::from(format!("{id}-open")), Animation::new(std::time::Duration::from_millis(140)).with_easing(ease_out), move |el, t| {
                    let off = 4. * (1. - t);
                    el.opacity(t).top(px(if up { off } else { -off }))
                })
                .into_any_element()
            }
        })
}

/// Every button: the toolkit's, with the mockup's medium-weight label.
pub fn button(id: impl Into<gpui_kit::ElementId>) -> gpui_kit::component::button::Button {
    gpui_kit::component::button::Button::new(id).font_weight(FontWeight::MEDIUM)
}

/// A bar of coloured parts in a rounded track (the disk's PostgreSQL,
/// ClickHouse, other files; the rest is the track). Both ends are rounded
/// alike whatever the widths: the parts are drawn as layers that all start
/// at the left end with the track's own curve (the widest underneath), each
/// at least as wide as the bar is tall, so a sliver still shows the curve;
/// the right end is rounded only when the parts fill the bar.
pub fn segment_bar(track: Hsla, h: f32, parts: &[(Hsla, f32)]) -> Div {
    let r = px(h / 2.);
    let mut cum = 0.;
    let mut layers: Vec<(Hsla, f32)> = Vec::new();
    for (c, f) in parts {
        if *f <= 0. {
            continue;
        }
        cum = (cum + f).min(1.);
        layers.push((*c, cum));
    }
    div()
        .relative()
        .w_full()
        .h(px(h))
        .rounded(r)
        .bg(track)
        .children(layers.into_iter().rev().map(move |(c, end)| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(end))
                .min_w(px(h))
                .rounded_l(r)
                .when(end >= 0.999, |d| d.rounded_r(r))
                .bg(c)
        }))
}

// ---------------------------------------------------------------------------
// Motion: values that glide to a new target (the motion plan): ease-out,
// only position, size, opacity or colour; off with reduce motion
// ---------------------------------------------------------------------------

struct Tween {
    from: [f32; 4],
    to: [f32; 4],
    since: std::time::Instant,
    ms: f32,
}

thread_local! {
    static TWEENS: std::cell::RefCell<std::collections::HashMap<SharedString, Tween>> = Default::default();
}

pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

fn tween_n(key: SharedString, target: [f32; 4], ms: u64, window: &mut Window) -> [f32; 4] {
    let now = std::time::Instant::now();
    let motion = !reduce_motion();
    TWEENS.with(|t| {
        let mut t = t.borrow_mut();
        let ms = ms as f32;
        let at = |w: &Tween| {
            let k = ease_out(now.duration_since(w.since).as_secs_f32() * 1000. / w.ms);
            let mut v = [0.; 4];
            for i in 0..4 {
                v[i] = w.from[i] + (w.to[i] - w.from[i]) * k;
            }
            v
        };
        let w = t.entry(key).or_insert(Tween { from: target, to: target, since: now, ms });
        if w.to != target {
            // A new target: from where it is now.
            w.from = if motion { at(w) } else { target };
            w.to = target;
            w.since = now;
            w.ms = ms;
        }
        let done = now.duration_since(w.since).as_secs_f32() * 1000. >= w.ms;
        if done || !motion {
            w.from = w.to;
            w.to
        } else {
            window.request_animation_frame();
            at(w)
        }
    })
}

/// A number that glides to `target` over `ms` (ease-out) whenever it changes.
pub fn tween(key: impl Into<SharedString>, target: f32, ms: u64, window: &mut Window) -> f32 {
    tween_n(key.into(), [target, 0., 0., 0.], ms, window)[0]
}

/// A colour that cross-fades to `target` over `ms` whenever it changes.
pub fn tween_color(key: impl Into<SharedString>, target: Hsla, ms: u64, window: &mut Window) -> Hsla {
    let r = target.to_rgb();
    let v = tween_n(key.into(), [r.r, r.g, r.b, r.a], ms, window);
    Rgba { r: v[0], g: v[1], b: v[2], a: v[3] }.into()
}

/// One full turn of the Refresh icon.
const REFRESH_TURN: f32 = 0.9;

thread_local! {
    /// Refresh buttons spinning now: when each started, and when it is to
    /// stop (set once the reading ended: the end of that turn).
    static SPINS: std::cell::RefCell<std::collections::HashMap<&'static str, (std::time::Instant, Option<std::time::Instant>)>> = Default::default();
}

/// A page's Refresh: a standard-size outline icon button (as tall as the
/// other buttons), its icon at 16 px, "Refresh" as its tooltip. One for
/// every page, so they all look and sit alike. On a click the icon turns at
/// a steady speed while `busy` (at least one full turn, so a quick read
/// still shows), then stops upright; it cannot be clicked meanwhile.
pub fn refresh_button(id: &'static str, busy: bool, window: &mut Window, on: impl Fn(&mut Window, &mut App) + 'static) -> gpui_kit::component::button::Button {
    use gpui_kit::component::Disableable as _;
    let now = std::time::Instant::now();
    let turn = std::time::Duration::from_secs_f32(REFRESH_TURN);
    let start = SPINS.with(|s| {
        let mut s = s.borrow_mut();
        if busy {
            let e = s.entry(id).or_insert((now, None));
            e.1 = None;
        }
        let (start, stop) = s.get(id).copied()?;
        let stop = match stop {
            Some(t) => t,
            None if busy => return Some(start),
            None => {
                // Busy no more: on at least one turn, to the end of this one.
                let e = now.duration_since(start).as_secs_f32().max(REFRESH_TURN);
                let t = start + turn.mul_f32((e / REFRESH_TURN).ceil());
                s.insert(id, (start, Some(t)));
                t
            }
        };
        if now >= stop {
            s.remove(id);
            None
        } else {
            Some(start)
        }
    });
    let angle = start.map(|t| (now.duration_since(t).as_secs_f32() % REFRESH_TURN) / REFRESH_TURN * std::f32::consts::TAU).unwrap_or(0.);
    if start.is_some() {
        window.request_animation_frame();
    }
    button(id)
        .outline()
        .icon(gpui_kit::component::Icon::new(gpui_kit::component::IconName::RefreshCw).rotate(radians(angle)))
        .tooltip("Refresh")
        .disabled(start.is_some())
        .on_click(move |_, window, cx| {
            SPINS.with(|s| {
                s.borrow_mut().entry(id).or_insert((std::time::Instant::now(), None));
            });
            on(window, cx);
        })
}

/// `/home/ops/…/sentient-platform-manager`: a long text cut in the middle to
/// `max` characters, keeping both ends.
pub fn middle_ellipsis(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 8 {
        return s.to_string();
    }
    let keep = max - 1;
    let head = keep / 2;
    let tail = keep - head;
    format!("{}…{}", chars[..head].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>())
}

/// "Open SENTIENT": the primary action while SENTIENT serves; disabled with
/// the reason in a tooltip while it is stopped (Start is the action then).
pub fn open_sentient_button(id: &'static str, serving: bool, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
    use gpui_kit::component::button::ButtonVariants as _;
    use gpui_kit::component::Disableable as _;
    if serving {
        button(id).primary().label("Open SENTIENT").on_click(on_click).into_any_element()
    } else {
        div()
            .id(SharedString::from(format!("{id}-why")))
            .child(button(id).outline().label("Open SENTIENT").disabled(true))
            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("SENTIENT is stopped. Start it first.").build(w, cx))
            .into_any_element()
    }
}

/// Tabular figures: every digit the same width, so a number that changes
/// does not change its width.
pub fn tabular() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

/// The width a text of `widest`'s length takes at `size` px with tabular
/// figures (about 0.6 em a character in the UI font, plus a little room).
pub fn slot_width(widest: &str, size: f32) -> Pixels {
    px((widest.chars().count() as f32 * size * 0.6 + 2.).ceil())
}

/// A value that updates live in a fixed slot sized for its widest text,
/// right-aligned with tabular figures: nothing beside it moves when it
/// changes (the top bar's monitors, the strip's time, Statistics figures).
/// The same, its text at the slot's left: the value follows its label or bar
/// with no gap, the slot's spare room trails it.
pub fn live_value_left(widest: &str, size: f32, value: impl Into<SharedString>) -> Div {
    div().w(slot_width(widest, size)).flex_shrink_0().text_size(px(size)).whitespace_nowrap().font_features(tabular()).child(value.into())
}

pub fn live_value(widest: &str, size: f32, value: impl Into<SharedString>) -> Div {
    div()
        .w(slot_width(widest, size))
        .flex_shrink_0()
        .text_size(px(size))
        .text_right()
        .whitespace_nowrap()
        .font_features(tabular())
        .child(value.into())
}

static NARROW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The window is narrow (below 760 px): rows that hold several columns lay
/// out on two lines. The manager sets it each frame.
pub fn set_narrow(narrow: bool) {
    NARROW.store(narrow, std::sync::atomic::Ordering::Relaxed);
}

pub fn narrow() -> bool {
    NARROW.load(std::sync::atomic::Ordering::Relaxed)
}

/// The window width from which Status's two top cards share a row.
pub const SIDE_BY_SIDE_W: f32 = 1100.;

static SIDE_BY_SIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The window is SIDE_BY_SIDE_W or wider (the manager sets it each frame).
pub fn set_side_by_side(on: bool) {
    SIDE_BY_SIDE.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn side_by_side() -> bool {
    SIDE_BY_SIDE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Screens and layout tests: an invisible child that reports its parent's
/// bounds on stderr as `LAYOUT <name> left right` (SPM_DEBUG_LAYOUT set).
pub fn layout_probe(name: &'static str) -> impl IntoElement {
    let on = cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some();
    div().absolute().inset_0().when(on, move |d| {
        d.child(
            canvas(
                move |bounds, _, _| {
                    let (l, r) = (f32::from(bounds.origin.x), f32::from(bounds.origin.x + bounds.size.width));
                    let (t, b) = (f32::from(bounds.origin.y), f32::from(bounds.origin.y + bounds.size.height));
                    // Buttons to a hundredth: the gap check holds them to
                    // half a pixel at x1.
                    if name == "button" || name == "menu-item-icon" || name == "svc-button" {
                        eprintln!("LAYOUT {name} {l:.2} {r:.2} {t:.2} {b:.2}");
                    } else {
                        eprintln!("LAYOUT {name} {l:.0} {r:.0} {t:.0} {b:.0}");
                    }
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    })
}

// ---------------------------------------------------------------------------
// Skeletons: a page's final layout while its data loads
// ---------------------------------------------------------------------------

/// Motion off: static placeholders (SPM_REDUCED_MOTION, or GNOME/KDE's
/// "reduce animations" when it can be read).
fn reduced_motion() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        std::env::var_os("SPM_REDUCED_MOTION").is_some()
            || std::process::Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", "enable-animations"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "false")
    })
}

/// A placeholder block of `w` × `h` in the muted surface tint, with the
/// shimmer: one soft, slanted band of light sweeping across the whole page
/// (see [`paint_shimmer`]). `key` names it among its siblings.
pub fn skeleton_block(theme: &Theme, key: impl Into<SharedString>, w: Length, h: f32) -> Div {
    skeleton_block_r(theme, key, w, h, (h / 2.).min(6.))
}

/// [`skeleton_block`] with its own corner radius (the licence card's 22 px).
/// The shimmer is painted inside the same rounded shape.
pub fn skeleton_block_r(theme: &Theme, key: impl Into<SharedString>, w: Length, h: f32, radius: f32) -> Div {
    let _key: SharedString = key.into();
    let dark = theme.mode.is_dark();
    let base = if dark { theme.foreground.opacity(0.08) } else { theme.foreground.opacity(0.07) };
    // A few per cent lighter than the bone, at the band's middle only.
    let light = if dark { gpui_kit::white().opacity(0.07) } else { gpui_kit::white().opacity(0.6) };
    // Its width is the most it takes: it shrinks with its row and never
    // runs past its parent (a narrow window, a narrow card).
    div().relative().overflow_hidden().w(w).max_w_full().min_w_0().h(px(h)).flex_shrink(1.).rounded(px(radius)).bg(base).when(!reduced_motion(), |d| {
        d.child(canvas(|_, _, _| {}, move |bounds, _, window, _| paint_shimmer(bounds, radius, light, window)).absolute().inset_0().size_full())
    })
}

/// How long one sweep of the shimmer takes.
const SHIMMER_PERIOD: f32 = 1.5;
/// The band's width (each soft side half of it) and the stagger between
/// rows (a diagonal sweep down the page, 20°).
const SHIMMER_BAND: f32 = 260.;
const SHIMMER_TAN: f32 = 0.364;

/// The shimmer in one bone: every bone reads the same clock and places the
/// band by its own position on the page (x minus its height times the
/// slant), so one light passes over the page diagonally. The band is soft
/// all the way (light rising to its middle, then falling): it is painted as
/// strips cut at the band's corners, each a left-to-right gradient between
/// the band's light at its two ends, so no strip edge shows; the strips at
/// the bone's ends take its rounded corners. Ease-in-out; the next frame is
/// asked for.
fn paint_shimmer(bounds: Bounds<Pixels>, radius: f32, light: Hsla, window: &mut Window) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(std::time::Instant::now);
    let page_w = f32::from(window.viewport_size().width);
    let p = (start.elapsed().as_secs_f32() / SHIMMER_PERIOD).fract();
    let eased = if p < 0.5 { 2. * p * p } else { 1. - (-2. * p + 2.).powi(2) / 2. };
    // From fully left of the page to fully right, including the stagger.
    let travel = page_w + 2. * SHIMMER_BAND + 400. * SHIMMER_TAN;
    let centre_at_top = -SHIMMER_BAND + eased * travel;
    let y_mid = f32::from(bounds.origin.y + bounds.size.height / 2.);
    let c = centre_at_top - y_mid * SHIMMER_TAN;
    let half = SHIMMER_BAND / 2.;
    let (l, r) = (f32::from(bounds.origin.x), f32::from(bounds.origin.x + bounds.size.width));
    // The light at x: 0 outside the band, 1 at its middle.
    let at = |x: f32| (1. - (x - c).abs() / half).max(0.);
    // Cut the bone at the band's three corners that fall inside it.
    let mut cuts = vec![l];
    for k in [c - half, c, c + half] {
        if k > l && k < r {
            cuts.push(k);
        }
    }
    cuts.push(r);
    let rad = radius.min(f32::from(bounds.size.height) / 2.).min((r - l) / 2.);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for seg in cuts.windows(2) {
            let (x0, x1) = (seg[0], seg[1]);
            let (a0, a1) = (at(x0), at(x1));
            if x1 - x0 < 0.5 || (a0 <= 0.001 && a1 <= 0.001) {
                continue;
            }
            // Corners: the bone's own where the strip reaches its ends; a
            // strip starting within the corner takes what is left of it.
            let left = (rad - (x0 - l)).max(0.);
            let right = (rad - (r - x1)).max(0.);
            let q = fill(
                Bounds::new(point(px(x0), bounds.origin.y), size(px(x1 - x0), bounds.size.height)),
                linear_gradient(90., linear_color_stop(light.opacity(light.a * a0), 0.), linear_color_stop(light.opacity(light.a * a1), 1.)),
            )
            .corner_radii(Corners { top_left: px(left), bottom_left: px(left), top_right: px(right), bottom_right: px(right) });
            window.paint_quad(q);
        }
    });
    window.request_animation_frame();
}

/// A text-line placeholder (one line of 14 px text: 12 px high in a 20 px row).
pub fn skeleton_line(theme: &Theme, key: impl Into<SharedString>, w: f32) -> Div {
    // A 14 px line's own height (its line-height), so a loaded row is as tall.
    div().h(rems(0.875 * 1.4)).flex_1().min_w_0().flex().items_center().child(skeleton_block(theme, key, px(w).into(), 12.))
}

/// A fact row whose value is still loading: the label as it will be, the
/// value a placeholder (same height as the loaded row).
pub fn fact_loading(theme: &Theme, label: &'static str, w: f32) -> Div {
    gpui_kit::component::h_flex()
        .gap_4()
        .text_sm()
        .child(div().w(px(168.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
        .child(skeleton_line(theme, label, w))
}

/// A card while it loads: its title and `rows` placeholder rows.
pub fn skeleton_card(theme: &Theme, key: &'static str, title: Option<&'static str>, labels: &[&'static str]) -> Div {
    let mut p = panel(theme);
    if let Some(t) = title {
        p = p.child(div().font_weight(FontWeight::MEDIUM).child(t));
    }
    for (i, l) in labels.iter().enumerate() {
        let w = [180., 140., 220., 120., 160.][i % 5];
        p = p.child(if l.is_empty() { skeleton_line(theme, format!("{key}-{i}"), w + 120.) } else { fact_loading(theme, l, w) });
    }
    p
}

/// Service rows while they load: the state pill, the name and the four
/// buttons as placeholders.
pub fn skeleton_service_rows(theme: &Theme, key: &'static str, n: usize) -> Vec<Div> {
    (0..n)
        .map(|i| {
            gpui_kit::component::h_flex()
                .gap_3()
                .items_center()
                .min_h(px(SVC_ROW_H))
                .child(skeleton_block(theme, format!("{key}-p{i}"), px(72.).into(), 25.))
                .child(skeleton_line(theme, format!("{key}-n{i}"), [200., 240., 180.][i % 3]))
                .child(gpui_kit::component::h_flex().gap(button_gap()).flex_shrink_0().children((0..4).map(|b| skeleton_block_r(theme, format!("{key}-b{i}-{b}"), px(32.).into(), 32., 6.))))
        })
        .collect()
}

#[cfg(test)]
mod info_tests {
    use super::info_lines;

    /// A remote job that ends with a long note (a takeover): read as
    /// a done message, its first sentence and the rest, never as an error.
    #[test]
    fn a_long_success_note_stays_a_note() {
        let m = "SENTIENT now runs from /opt/sentient/docker-compose.yml. Start and stop it with the Platform Manager (or docker compose -f that file) from now on: the old compose file would bring back the old settings.";
        assert!(m.len() > 200);
        let (head, rest) = info_lines(m);
        assert_eq!(head, "SENTIENT now runs from /opt/sentient/docker-compose.yml.");
        assert!(rest.unwrap().starts_with("Start and stop it"));
        // The error reading of the same text is what the dialog showed before.
        assert_eq!(crate::remote::split_failure(m).0, "The action did not finish on the server.");
        assert_eq!(info_lines("Backup finished."), ("Backup finished.".to_string(), None));
    }
}

/// Enter in a form's fields: each field's Enter moves to the next field
/// shown, the last one's submits (the form's primary button). `order` lists
/// the fields shown, in their visual order, when Enter is pressed (fields
/// that come and go stay right). `submit` returns the first field that is
/// not valid, to focus (its message is the form's to show), or None when it
/// went ahead. Shift+Enter and the secondary Enter are left to the field.
pub fn enter_moves_on<T: 'static>(
    fields: &[Entity<gpui_kit::component::input::InputState>],
    window: &mut Window,
    cx: &mut Context<T>,
    order: impl Fn(&T, &App) -> Vec<Entity<gpui_kit::component::input::InputState>> + 'static,
    submit: impl Fn(&mut T, &mut Window, &mut Context<T>) -> Option<Entity<gpui_kit::component::input::InputState>> + 'static,
) -> Vec<Subscription> {
    use gpui_kit::component::input::InputEvent;
    let order = std::rc::Rc::new(order);
    let submit = std::rc::Rc::new(submit);
    fields
        .iter()
        .map(|f| {
            let (order, submit) = (order.clone(), submit.clone());
            cx.subscribe_in(f, window, move |this, me, e: &InputEvent, window, cx| {
                let InputEvent::PressEnter { secondary: false, shift: false } = e else { return };
                let list = order(this, cx);
                let next = match list.iter().position(|x| x == me) {
                    Some(i) if i + 1 < list.len() => Some(list[i + 1].clone()),
                    _ => submit(this, window, cx),
                };
                if let Some(n) = next {
                    n.update(cx, |s, cx| s.focus(window, cx));
                }
            })
        })
        .collect()
}
