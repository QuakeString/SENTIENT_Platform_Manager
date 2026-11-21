// Copyright © 2016-2026 The SENTIENT Authors

//! Small presentation pieces shared by the screens: the white panel the
//! design system puts all content on, page headings, and status pills.

use gpui_kit::component::scroll::ScrollableElement as _;
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
                    c.child(scroll_y(div().id("error-detail").max_h(px(300.)).p_2().rounded(theme.radius).bg(theme.muted).font_family(theme.mono_font_family.clone()).text_size(px(12.5)).child(d.trim().to_string())))
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
    open_web_app(&format!("http://localhost:{port}"), cx);
}

/// Open SENTIENT's address `url` in a browser that runs its UI. Windows:
/// the default browser, unless that is Internet Explorer (Windows 10
/// LTSC's) or none is set: then Edge, Chrome or Firefox, else a message
/// saying one is needed (sentient_installer_core::kiosk::choose_web_launch).
/// Elsewhere: [`open_url`].
pub fn open_web_app(url: &str, cx: &mut App) {
    #[cfg(windows)]
    {
        use sentient_installer_core::kiosk::{self, WebLaunch};
        let url = url.to_string();
        cx.spawn(async move |cx| {
            let launch = cx.background_executor().spawn(async move { kiosk::web_launch() }).await;
            let _ = cx.update(|cx| match launch {
                WebLaunch::Default => cx.open_url(&url),
                WebLaunch::With(browser, exe) => {
                    if let Err(e) = std::process::Command::new(&exe).args(browser.open_args(&url, false, None)).spawn() {
                        show_error("Could not open it", format!("{} could not be started to open {url}.\n{e}", browser.label()), cx);
                    }
                }
                WebLaunch::Missing => show_error("No browser for SENTIENT", kiosk::no_browser_message(&url), cx),
            });
        })
        .detach();
    }
    #[cfg(not(windows))]
    open_url(url, cx);
}

/// What the start delay does, for its (i).
pub const START_DELAY_HELP: &str = "Seconds SENTIENT waits after Windows starts before it starts itself, its database first. \
0 starts it together with Windows. A few seconds let other programs and the network settle first; an operator screen shows \
\"SENTIENT is starting\" meanwhile. Change it later in Platform settings, under Startup.";

/// "Start delay [ 15 ] seconds (i)": the installer's and Startup's field.
pub fn start_delay_row(theme: &Theme, state: &Entity<gpui_kit::component::input::InputState>) -> Div {
    start_delay_row_checked(theme, state, None)
}

/// The start delay with its message when the value cannot be used (a red
/// border and one line under the row).
pub fn start_delay_row_checked(theme: &Theme, state: &Entity<gpui_kit::component::input::InputState>, problem: Option<String>) -> Div {
    use gpui_kit::component::{button::ButtonVariants as _, IconName};
    v_flex()
        .gap_1()
        .child(
            gpui_kit::component::h_flex()
                .gap_2()
                .items_center()
                .child(div().text_sm().child("Start delay"))
                .child(div().w_20().child({
                    let i = gpui_kit::component::input::Input::new(state);
                    if problem.is_some() { i.border_color(theme.danger).focus_bordered(false) } else { i }
                }))
                .child(div().text_sm().text_color(theme.muted_foreground).child("seconds"))
                .child(crate::ui::button("start-delay-help").ghost().icon(IconName::Info).tooltip(START_DELAY_HELP)),
        )
        .when_some(problem, |d, p| d.child(div().text_xs().text_color(theme.danger).child(p)))
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
/// A page: one scroll area with the shared scrollbar (ScrollPage).
pub type ScrollPage = gpui_kit::component::scroll::Scrollable<Stateful<Div>>;

pub fn page(theme: &Theme, title: &'static str, summary: &'static str) -> ScrollPage {
    v_flex()
        .id("page")
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_y_scrollbar()
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
pub fn page_with(theme: &Theme, title: &'static str, summary: &'static str, action: impl IntoElement) -> ScrollPage {
    v_flex().id("page").flex_1().min_w_0().h_full().overflow_y_scrollbar().p(px(PAGE_PAD)).gap(px(GAP)).bg(theme.muted).child(
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

/// The title's line on a sub-page with a back button (back_title): the
/// 22 px title's line box.
pub const BACK_TITLE_LINE: f32 = 30.;

/// A sub-page's header (Move data to another disk, Restore): an icon back
/// button on the page's content edge, centred on the title's line, then
/// the 22 px title, and `sub` (the instance, the backup) under the title
/// text, never under the button.
pub fn back_title(
    theme: &Theme,
    id: &'static str,
    title: impl Into<SharedString>,
    sub: impl Into<SharedString>,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let sub: SharedString = sub.into();
    let back = button(id)
        .outline()
        .icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ChevronLeft))
        .tooltip("Back")
        .when(disabled, |b| gpui_kit::component::Disableable::disabled(b, true))
        .on_click(on_click);
    // The button's gap to the title: the app's one gap between a button
    // and the text beside it (button_gap).
    h_flex()
        .gap(button_gap())
        .items_start()
        .child(
            h_flex()
                .flex_shrink_0()
                .h(px(BACK_TITLE_LINE))
                .items_center()
                .child(div().relative().child(back).child(layout_probe("back-button"))),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .relative()
                        .text_size(px(22.))
                        .line_height(px(BACK_TITLE_LINE))
                        .font_weight(FontWeight::MEDIUM)
                        .child(title.into())
                        .child(layout_probe("page-title-line")),
                )
                .when(!sub.is_empty(), |c| {
                    c.child(div().relative().text_sm().text_color(theme.muted_foreground).child(sub).child(layout_probe("page-subtitle")))
                }),
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

/// Warning-coloured text that stays readable on its background: a light
/// amber on dark surfaces, a dark amber on light ones (4.5:1 or better).
pub fn warn_text(on_light: bool) -> Hsla {
    if on_light { rgb(0x9a3412).into() } else { rgb(0xfcd34d).into() }
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

/// A card's action buttons in its foot: the divider above, the buttons at
/// the right end (SPM's rule: actions on the right; secondary first, the
/// primary rightmost). check.py holds every such row to the right edge
/// (card-actions probe).
pub fn card_actions(theme: &Theme, buttons: impl IntoElement) -> [Div; 2] {
    [
        div().flex_1(),
        div()
            .relative()
            .pt_3()
            .border_t_1()
            .border_color(theme.border)
            .child(gpui_kit::component::h_flex().w_full().justify_end().child(buttons))
            .child(layout_probe("card-actions")),
    ]
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
        .relative()
        .child(InfoIcon)
        .child(layout_probe("info-icon"))
        .tooltip(move |_, cx| cx.new(|_| InfoTip(help.clone())).into())
}

/// A lock where a value cannot be changed here, its tooltip saying why
/// and where to change it (the gateway's SENTIENT-owned settings).
pub fn lock_tip(id: SharedString, why: SharedString) -> impl IntoElement {
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
        .relative()
        .child(LockIcon)
        .child(layout_probe("lock-icon"))
        .tooltip(move |_, cx| cx.new(|_| InfoTip(why.clone())).into())
}

#[derive(IntoElement)]
struct LockIcon;

impl RenderOnce for LockIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Lock).size(px(14.)).text_color(cx.theme().muted_foreground)
    }
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
        // The toolkit tooltip's tokens (the popover surface, its text, the
        // border), so every hover help in SPM looks alike in both themes
        // (the (i) was inverted before: a light box on the dark theme).
        // tests/layout/tooltips.sh holds this block free of literal colours.
        let (bg, fg, border) = (cx.theme().tokens.popover, cx.theme().popover_foreground, cx.theme().border);
        // Laid out at its smallest size, so the width is set: the longest
        // line's length, at most 320 px.
        let longest = self.0.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
        let w = (longest * 6.9 + 22.).clamp(60., 320.);
        let shadow = BoxShadow { color: black().opacity(if dark { 0.45 } else { 0.25 }), offset: point(px(0.), px(4.)), blur_radius: px(12.), spread_radius: px(0.), inset: false };
        // Opening: the menu motion (it hangs below the pointer).
        let tip = div().pt(px(8.)).pl(px(4.)).child(
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
        );
        menu_appear(tip, "info-tip-open", true)
    }
}

/// The Flatpak build manages SENTIENT on other computers only: its sandbox
/// cannot reach SENTIENT's files and services here. Shown instead of every
/// "this computer" page (not_installed, Setup), so they do not look empty.
pub fn flatpak_local_notice(theme: &Theme) -> Div {
    panel(theme)
        .child(div().font_weight(FontWeight::MEDIUM).child("This is the Flatpak build of the Platform Manager: it manages SENTIENT on other computers"))
        .child(
            gpui_kit::component::h_flex()
                .gap_1()
                .items_center()
                .child(div().text_sm().text_color(theme.muted_foreground).child("To install or manage SENTIENT on this computer, use the .deb, .rpm, AppImage or Arch package."))
                .child(info(
                    "flatpak-help".into(),
                    "The Flatpak runs in a sandbox that cannot reach SENTIENT's files and services on this computer, so it manages SENTIENT on other computers over SSH.\n\nAdd a server from the instance switcher.".into(),
                )),
        )
}

/// Shown on every manager screen when this computer has no installation.
/// The Flatpak build says why instead (flatpak_local_notice).
pub fn not_installed(theme: &Theme) -> Div {
    if sentient_installer_core::host::in_flatpak() {
        return flatpak_local_notice(theme);
    }
    // An install that stopped for the WSL restart comes first.
    match crate::installer::plan::pending_offer_now() {
        crate::installer::plan::PendingOffer::Button | crate::installer::plan::PendingOffer::Resume => {
            return panel(theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("The SENTIENT install has not finished"))
                .child(div().text_sm().text_color(theme.muted_foreground).child("It stopped for the Windows restart. Setup continues where it stopped, with the choices you made."))
                .child(button_row(vec![
                    crate::ui::button("resume-install").primary().label("Continue the SENTIENT install").on_click(|_, window, cx| crate::standalone::resume_install(window, cx)).into_any_element(),
                    crate::ui::button("install-sentient").outline().label("Start a new install").on_click(|_, window, cx| crate::standalone::install_sentient(window, cx)).into_any_element(),
                ]));
        }
        crate::installer::plan::PendingOffer::AfterRestart => {
            return panel(theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Restart Windows to finish installing SENTIENT"))
                .child(div().text_sm().text_color(theme.muted_foreground).child("WSL 2 is turned on after the restart. Setup then continues by itself after you sign in."));
        }
        crate::installer::plan::PendingOffer::None => {}
    }
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
                crate::ui::button("install-sentient-package").outline().label("Install from local files").on_click(|_, window, cx| crate::standalone::install_sentient_from_package(window, cx)).into_any_element()
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
/// The status dot's colours and size, everywhere a dot says whether
/// something runs (the instance switcher, the terminal's status bar).
pub const STATUS_DOT: f32 = 10.;
pub const STATUS_GREEN: u32 = 0x3ddc84;
pub const STATUS_GREY: u32 = 0x94a3b8;

pub fn probed(b: impl IntoElement) -> Div {
    div().relative().flex_shrink_0().child(b).child(layout_probe("button"))
}

/// An input reported to the layout test (`LAYOUT field`): its height and
/// its centre beside the controls in its row (tests/layout/rows.py).
pub fn probed_input(input: impl IntoElement) -> Div {
    div().relative().child(input).child(layout_probe("field"))
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

pub fn product_logo(p: Product, size: f32) -> AnyElement {
    let path = match p {
        Product::Manager if size <= 24. => "logos/spm-small.png",
        Product::Manager => "logos/spm.png",
        // SENTIENT's tile: drawn here, not in the artwork. The artwork's own
        // frame was a faint stroke on the viewBox's edge, and at most sizes
        // and scales its right and bottom sides were cut (his rc12-24 note).
        Product::Sentient => {
            let line = hsla(0., 0., 0.44, 0.33);
            return div()
                .relative()
                .flex_shrink_0()
                .size(px(size))
                // The tile, painted on whole device pixels (as the menu's
                // selection, manager::shell::selection): its box snapped,
                // its side a whole number of device pixels and its border
                // one device pixel per logical one at least. A styled
                // border_1 sat where layout put it; at fractional scales or
                // positions its top and bottom lines fell across two
                // device pixels at part strength.
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let sc = window.scale_factor();
                            let x0 = (f32::from(bounds.origin.x) * sc).round();
                            let y0 = (f32::from(bounds.origin.y) * sc).round();
                            let side = (size * sc).round();
                            let w = sc.round().max(1.);
                            // GPUI floors left and top edges and ceils right
                            // and bottom ones: a quarter pixel in from each.
                            let lo = |d: f32| px((d + 0.25) / sc);
                            let hi = |d: f32| px((d - 0.25) / sc);
                            let lg = |d: f32| px(d / sc);
                            // VERIFY: render at 1, 1.5 and 2 (tests/layout/check.py).
                            window.paint_quad(gpui::quad(
                                Bounds::from_corners(point(lo(x0), lo(y0)), point(hi(x0 + side), hi(y0 + side))),
                                Corners::all(lg((size * 0.12 * sc).round())),
                                gpui_kit::white(),
                                Edges::all(lg(w)),
                                line,
                                BorderStyle::Solid,
                            ));
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(div().absolute().inset_0().flex().items_center().justify_center().child(img("logos/sentient-glyph.svg").size(px(size - 2.))))
                // On the borderless box: its bounds are the tile's outside
                // (a probe inside a bordered div reports the box inside the
                // border, where tests/layout/check.py then looked for it).
                .child(layout_probe("logo-tile"))
                .into_any_element();
        }
        Product::Gateway => "logos/gateway.svg",
    };
    img(path).size(px(size)).flex_shrink_0().into_any_element()
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
/// with the primary colour. The standard control height ([`control_h`]),
/// border included, so it lines up with the buttons beside it. Reports
/// `segmented`.
pub fn seg_track(theme: &Theme) -> Div {
    // The mockup's segmented control: one bordered strip, segments edge to
    // edge, the chosen one tinted with primary text.
    input_box()
        .relative()
        .child(layout_probe("segmented"))
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
        .h_full()
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

/// A segment that can't be picked: greyed on a faint fill, nothing reacts,
/// and its (i) (`why`, a [`info`]) says why.
pub fn seg_item_off(theme: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, why: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id)
        .h_full()
        .pl(px(11.))
        .pr(px(5.))
        .flex()
        .items_center()
        .gap(px(2.))
        .text_sm()
        .whitespace_nowrap()
        .bg(theme.foreground.opacity(0.05))
        .child(div().text_color(theme.muted_foreground).opacity(OFF_OPACITY).child(label.into()))
        .child(why)
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

/// Set once a window draws without a GPU ([`note_renderer`]).
static SOFTWARE_RENDERER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A window that draws on a software renderer (a virtual machine without a
/// GPU: Microsoft Basic Render Driver, llvmpipe) draws without motion: GPUI
/// draws the whole window again for every frame of an animation, and a
/// job's moving bar and spinner then kept the processor busy for as long as
/// the job ran, slowing SENTIENT's own start in the same machine (the
/// 2026-11-18 rc3 VM pass: 99 % while Start ran, its window open).
pub fn note_renderer(window: &gpui_kit::Window, cx: &mut gpui_kit::App) {
    if window.gpu_specs().is_some_and(|g| software_renderer(g.is_software_emulated, &g.device_name, &g.driver_name)) {
        SOFTWARE_RENDERER.store(true, std::sync::atomic::Ordering::Relaxed);
        cx.set_reduce_motion(true);
    }
}

/// Whether a GPU description is a renderer on the processor. GPUI's own
/// flag is not enough on Windows: DXGI's Microsoft Basic Render Driver
/// (WARP, the adapter of a virtual machine without a GPU, vendor 0x1414)
/// does not carry DXGI_ADAPTER_FLAG_SOFTWARE (the 2026-11-18 rc4 VM pass:
/// the job line still moved and the Platform Manager took 2.7 processors
/// while Start ran).
pub fn software_renderer(flagged: bool, device: &str, driver: &str) -> bool {
    let d = device.to_ascii_lowercase();
    flagged
        || d.contains("basic render driver")
        || d.contains("llvmpipe")
        || d.contains("softpipe")
        || d.contains("swiftshader")
        || d.contains("warp")
        || driver.contains("0x1414")
}

/// Whether the desktop asks for less motion (KDE's animation speed at 0,
/// GNOME's animations off, or SPM_REDUCE_MOTION=1; read once), or a window
/// draws on a software renderer ([`note_renderer`]).
pub fn reduce_motion() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    SOFTWARE_RENDERER.load(std::sync::atomic::Ordering::Relaxed) || *R.get_or_init(|| {
        // SPM_REDUCED_MOTION too: the layout tests set that one (and the
        // placeholders' reduced_motion reads it), so both switch it off.
        if std::env::var("SPM_REDUCE_MOTION").is_ok_and(|v| v == "1") || std::env::var_os("SPM_REDUCED_MOTION").is_some() {
            return true;
        }
        // Windows: "Show animations in Windows" off.
        #[cfg(windows)]
        if !windows_animations_on() {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            let out = |cmd: &str, args: &[&str]| sentient_installer_core::host::host_command(cmd).args(args).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
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

/// Windows' "Show animations in Windows" (SPI_GETCLIENTAREAANIMATION);
/// on when it cannot be read.
#[cfg(windows)]
fn windows_animations_on() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION};
    let mut on: windows_sys::core::BOOL = 1;
    // SAFETY: this action writes one BOOL to the pointer given.
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut on as *mut windows_sys::core::BOOL).cast(), 0) };
    ok == 0 || on != 0
}

// Popups open with one motion per kind; pages switch instantly (the
// owner's rule, 2026-11-17). Dialogs and panels (the modal, the bell, the
// job panel): 180 ms, rising 8 px. Menus and tooltips: 140 ms, 4 px toward
// their anchor; the toolkit's popup menu (vendor/gpui-component, menu/
// popup_menu.rs) and its tooltip layer move the same way. Reduce motion
// draws them in place (here, and through GPUI's flag, set in main).
// src/popup_gate.rs fails a popup that opens without these.
pub const APPEAR_MS: u64 = 180;
pub const APPEAR_RISE: f32 = 8.;
pub const MENU_APPEAR_MS: u64 = 140;
pub const MENU_APPEAR_RISE: f32 = 4.;

/// A dialog or panel opening: it fades in, rising 8 px into place. It
/// plays when the popup is drawn after a frame without it, and again when
/// `key` changes.
pub fn appear<E: IntoElement + Styled + 'static>(el: E, key: impl Into<ElementId>) -> AnyElement {
    motion_in(el, key.into(), APPEAR_MS, APPEAR_RISE)
}

/// A menu or tooltip opening: a quick fade, moving 4 px toward its anchor.
/// `below`: it hangs below its anchor (it rises); else above (it drops).
pub fn menu_appear<E: IntoElement + Styled + 'static>(el: E, key: impl Into<ElementId>, below: bool) -> AnyElement {
    motion_in(el, key.into(), MENU_APPEAR_MS, if below { MENU_APPEAR_RISE } else { -MENU_APPEAR_RISE })
}

fn motion_in<E: IntoElement + Styled + 'static>(el: E, key: ElementId, ms: u64, rise: f32) -> AnyElement {
    if reduce_motion() {
        return el.into_any_element();
    }
    el.with_animation(key, Animation::new(std::time::Duration::from_millis(ms)).with_easing(ease_out), move |el, t| el.opacity(t).top(px(rise * (1. - t))))
        .into_any_element()
}

/// A plain tooltip for a div's `.tooltip(...)`: the toolkit's tooltip,
/// opening with the menu motion (a button's own `.tooltip("...")` opens
/// through the toolkit's tooltip layer, which moves the same way).
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |window, cx| {
        let inner = gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx);
        cx.new(|_| AppearTip(inner)).into()
    }
}

struct AppearTip(AnyView);

impl Render for AppearTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        menu_appear(div().child(self.0.clone()), "tip-open", true)
    }
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
    radio_card_inner(theme, id, title, desc, selected, interactive, 1.)
}

/// How faded a choice that can't be made is drawn (its (i) is not).
pub const OFF_OPACITY: f32 = 0.55;

/// A `radio_card` that can't be picked: its mark and line faded, nothing
/// reacts. The caller fades the title's text and keeps its (i) with the
/// reason at full strength.
pub fn radio_card_off(theme: &Theme, id: impl Into<ElementId>, title: AnyElement, desc: impl Into<SharedString>) -> Stateful<Div> {
    radio_card_inner(theme, id, title, desc, false, false, OFF_OPACITY)
}

fn radio_card_inner(theme: &Theme, id: impl Into<ElementId>, title: AnyElement, desc: impl Into<SharedString>, selected: bool, interactive: bool, fade: f32) -> Stateful<Div> {
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
                .when(selected, |r| r.child(div().size(px(8.)).rounded_full().bg(theme.primary)))
                .when(fade < 1., |r| r.opacity(fade)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(title)
                .child(div().text_sm().text_color(theme.muted_foreground).when(fade < 1., |d| d.opacity(fade)).child(desc.into())),
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

/// A service row's outline icon button (the one icon-button square) with its
/// name as the tooltip. It hovers teal, or red for Stop.
pub fn svc_icon_button(theme: &Theme, id: SharedString, icon: gpui_kit::component::Icon, tip: &'static str, enabled: bool, stop: bool) -> gpui_kit::component::button::Button {
    use gpui_kit::component::{Disableable as _};
    let (bg, border, fg) = if stop { stop_hover(theme) } else { (theme.primary.opacity(0.12), theme.primary, theme.primary) };
    button(id)
        .outline()
        .icon(icon)
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
                        icon("stop", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Square), "Stop", running).into_any_element(),
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
        // Opening: the menu motion (140 ms, 4 px toward its anchor).
        .map(|d| menu_appear(d, SharedString::from(format!("{id}-open")), up))
}

// ---------------------------------------------------------------------------
// Long dropdown menus (the version pickers): open on the side with room and
// never taller than that room, scrolling the rest
// ---------------------------------------------------------------------------

/// The tallest a long dropdown menu gets.
pub const MENU_MAX_H: f32 = 320.;
/// Kept free between a menu and the window's edge.
const MENU_MARGIN: f32 = 16.;
/// The shortest it gets (a window too small for more: the toolkit moves it).
const MENU_MIN_H: f32 = 120.;

thread_local! {
    /// Where each fitted dropdown's trigger was last painted: top, bottom
    /// and the window's height (logical pixels).
    static MENU_ROOM: std::cell::RefCell<std::collections::HashMap<&'static str, (f32, f32, f32)>> = Default::default();
}

/// Whether a menu under a trigger at `top`..`bottom` of a `window_h` high
/// window opens upwards, and its height: below unless it does not fit there
/// and there is more room above; at most [`MENU_MAX_H`], at most the room.
pub fn menu_fit(top: f32, bottom: f32, window_h: f32) -> (bool, f32) {
    let below = window_h - bottom - MENU_MARGIN;
    let above = top - MENU_MARGIN;
    let up = below < MENU_MAX_H && above > below;
    let room = if up { above } else { below };
    (up, room.min(MENU_MAX_H).max(MENU_MIN_H))
}

/// `trigger` with a long dropdown menu (`items` adds its entries): it opens
/// where there is room and scrolls past [`menu_fit`]'s height. `key` names
/// the trigger (its place is remembered from the last frame).
pub fn fitted_dropdown(
    key: &'static str,
    trigger: gpui_kit::component::button::Button,
    items: impl Fn(gpui_kit::component::menu::PopupMenu, &mut Window, &mut Context<gpui_kit::component::menu::PopupMenu>) -> gpui_kit::component::menu::PopupMenu + 'static,
) -> AnyElement {
    use gpui_kit::base::ElementExt as _;
    use gpui_kit::component::menu::DropdownMenu as _;
    let fit = |k: &str| MENU_ROOM.with(|m| m.borrow().get(k).copied()).map(|(t, b, h)| menu_fit(t, b, h));
    let up = fit(key).is_some_and(|(up, _)| up);
    let anchor = if up { Anchor::BottomLeft } else { Anchor::TopLeft };
    let menu = trigger.dropdown_menu_with_anchor(anchor, move |menu, window, cx| {
        let h = fit(key).map(|(_, h)| h).unwrap_or(MENU_MAX_H);
        items(menu.max_h(px(h)).scrollable(true), window, cx)
    });
    div()
        .flex_shrink_0()
        .relative()
        .child(menu)
        .on_prepaint(move |b, window, _| {
            let room = (b.top().as_f32(), b.bottom().as_f32(), window.viewport_size().height.as_f32());
            MENU_ROOM.with(|m| m.borrow_mut().insert(key, room));
            // Drawn for the other side (first frame, the trigger moved): draw
            // again, so the menu opens where there is room.
            if up != menu_fit(room.0, room.1, room.2).0 {
                window.refresh();
            }
        })
        .into_any_element()
}

/// Every button: the toolkit's at the one button size (crate::theme::BUTTON_H
/// high, BUTTON_PAD_X either side, at least BUTTON_MIN_W wide with a label,
/// an ICON_BUTTON square with only an icon, BUTTON_RADIUS corners, the
/// toolkit's text_sm label at medium weight). Call sites never change its
/// size, padding, corners or font (tests/layout/buttons.py --source).
pub fn button(id: impl Into<gpui_kit::ElementId>) -> gpui_kit::component::button::Button {
    use crate::theme::{BUTTON_MIN_W, BUTTON_PAD_X, BUTTON_RADIUS};
    use gpui_kit::component::Sizable as _;
    gpui_kit::component::button::Button::new(id)
        .with_size(gpui_kit::component::Size::Medium)
        .label_box(px(BUTTON_PAD_X), px(BUTTON_MIN_W))
        .rounded(px(BUTTON_RADIUS))
        .font_weight(FontWeight::MEDIUM)
}

/// A button in a fill of its own (a dialog's red destructive action, an
/// instance's Open in the instance colour), which the toolkit's variants do
/// not give: the one button box of crate::ui::button (BUTTON_H, BUTTON_PAD_X,
/// BUTTON_MIN_W, BUTTON_RADIUS, text_sm at medium weight), reported to the
/// layout test as a labelled button (`LAYOUT btn label …`).
pub fn solid_button(id: impl Into<gpui_kit::ElementId>, label: SharedString, bg: Hsla, fg: Hsla, on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    use crate::theme::{BUTTON_H, BUTTON_MIN_W, BUTTON_PAD_X, BUTTON_RADIUS};
    let id = id.into();
    let probe = cfg!(debug_assertions) && gpui_kit::component::button::LAYOUT_PROBE.load(std::sync::atomic::Ordering::Relaxed);
    let name = format!("{id:?}");
    h_flex()
        .id(id)
        .relative()
        .flex_shrink_0()
        .h(px(BUTTON_H))
        .min_w(px(BUTTON_MIN_W))
        .px(px(BUTTON_PAD_X))
        .justify_center()
        .items_center()
        .rounded(px(BUTTON_RADIUS))
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|d| d.opacity(0.9))
        .child(label)
        .when(probe, move |d| {
            d.child(
                div().absolute().inset_0().child(
                    canvas(
                        move |b, window, _| {
                            let (l, r) = (f32::from(b.origin.x), f32::from(b.origin.x + b.size.width));
                            let (t, bt) = (f32::from(b.origin.y), f32::from(b.origin.y + b.size.height));
                            let fs = 0.875 * f32::from(window.rem_size());
                            eprintln!("LAYOUT btn label {l:.2} {r:.2} {t:.2} {bt:.2} {fs:.2} {name}");
                        },
                        |_, _, _, _| {},
                    )
                    .size_full(),
                ),
            )
        })
        .on_click(on_click)
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
    // Reduce motion: upright (and not clickable) while it reads, no frames
    // asked for, and clickable again as soon as the reading ended.
    let motion = !reduce_motion();
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
            None if !motion => {
                s.remove(id);
                return None;
            }
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
    let angle = start.filter(|_| motion).map(|t| (now.duration_since(t).as_secs_f32() % REFRESH_TURN) / REFRESH_TURN * std::f32::consts::TAU).unwrap_or(0.);
    if start.is_some() && motion {
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
            .tooltip(crate::ui::tooltip("SENTIENT is stopped. Start it first."))
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
                    if matches!(name, "button" | "menu-item-icon" | "svc-button" | "back-button" | "page-title-line" | "page-subtitle" | "logo-tile") {
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

/// Motion off: static placeholders. The same answer as [`reduce_motion`]
/// (the desktop's choice, SPM_REDUCED_MOTION, or a software renderer):
/// the placeholders' shimmer once asked for every frame on its own, also
/// in a virtual machine without a GPU, where each frame is drawn on the
/// processor (the 2026-11-18 rc5 VM pass: Statistics took 3 of 4
/// processors while its Server card loaded).
pub(crate) fn reduced_motion() -> bool {
    reduce_motion()
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
    // The band, lighter than the bone at its middle. In dark 16 % white
    // (7 % read as no shimmer at all on the dark cards, his rc12-24 note);
    // tint.py holds the band's contrast in dark to at least light's.
    let light = if dark { gpui_kit::white().opacity(0.16) } else { gpui_kit::white().opacity(0.6) };
    if cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some() {
        let c = |h: Hsla| {
            let r = h.to_rgb();
            format!("{:.0} {:.0} {:.0} {:.3}", r.r * 255., r.g * 255., r.b * 255., r.a)
        };
        eprintln!("LAYOUT skeleton-colors {} {} {}", c(theme.background), c(base), c(light));
    }
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

/// One entry of a row's [`row_menu`].
pub enum RowMenuItem {
    Item { label: SharedString, icon: Option<gpui_kit::component::IconName>, action: std::rc::Rc<dyn Fn(&mut Window, &mut App)> },
    /// A destructive action (Delete…): its icon and label in the danger
    /// colour; it still asks first.
    Danger { label: SharedString, icon: gpui_kit::component::IconName, action: std::rc::Rc<dyn Fn(&mut Window, &mut App)> },
    Divider,
}

/// A row's actions behind the three dots: the outlined 28 px button with
/// the button radius that every list row in SPM uses (Instances, Backup…),
/// never a bare chevron.
pub fn row_menu(id: impl Into<SharedString>, tip: &'static str, disabled: bool, items: Vec<RowMenuItem>) -> impl IntoElement {
    use gpui_kit::component::{Disableable as _, menu::{DropdownMenu as _, PopupMenuItem}};
    let items = std::rc::Rc::new(items);
    button(gpui_kit::ElementId::Name(id.into()))
        .outline()
        .icon(gpui_kit::component::Icon::new(gpui_kit::component::IconName::Ellipsis).size(px(18.)))
        .tooltip(tip)
        .disabled(disabled)
        .dropdown_menu(move |mut menu, _, _| {
            for it in items.iter() {
                menu = match it {
                    RowMenuItem::Divider => menu.separator(),
                    RowMenuItem::Item { label, icon, action } => {
                        let a = action.clone();
                        let mut item = PopupMenuItem::new(label.clone()).on_click(move |_, window, cx| a(window, cx));
                        if let Some(i) = icon {
                            item = item.icon(gpui_kit::component::Icon::new(i.clone()));
                        }
                        menu.item(item)
                    }
                    RowMenuItem::Danger { label, icon, action } => {
                        let a = action.clone();
                        let (label, icon) = (label.clone(), icon.clone());
                        let item = PopupMenuItem::element(move |_, cx| {
                            let red = cx.theme().danger;
                            gpui_kit::component::h_flex()
                                .gap_2()
                                .items_center()
                                .text_color(red)
                                .child(gpui_kit::component::Icon::new(icon.clone()).text_color(red))
                                .child(label.clone())
                        })
                        .on_click(move |_, window, cx| a(window, cx));
                        menu.item(item)
                    }
                };
            }
            menu
        })
}

// ---------------------------------------------------------------------------
// Settings forms: rows, the value column, the card and its Apply bar
// ---------------------------------------------------------------------------

/// Every field 260 px wide (its unit slot inside it), the current value
/// 170 px, 18 px between the columns; every control (field, chips,
/// switch) the standard control height ([`control_h`]).
pub const FIELD_W: f32 = 260.;
pub const UNIT_W: f32 = 64.;
pub const CURRENT_W: f32 = 170.;
pub const COL_GAP: f32 = 18.;
/// The one control height in SPM: the toolkit's own input and button at
/// their default size (`Input::new`, `button(..)`: `Size::Medium`, 2 rem,
/// 28 px on screen), which every form and page header uses (Add server,
/// Setup, the database password, Statistics' range and Refresh). Taken
/// from the toolkit, never a number of our own; the settings field,
/// chips, switch and every segmented control are [`input_box`]es.
/// tests/layout/rows.py holds every control sharing a row to it, and
/// tests/layout/inputs.sh keeps size overrides off inputs and buttons.
pub fn control_h() -> Length {
    use gpui_kit::component::StyleSized as _;
    let mut d = div().input_h(gpui_kit::component::Size::Medium);
    d.style().size.height.expect("the toolkit's input sets a height")
}

#[cfg(test)]
mod control_h_tests {

    #[test]
    fn a_renderer_on_the_processor_is_known_also_without_dxgi_flag() {
        // The rc4 VM: DXGI's Basic Render Driver, flag not set, vendor 0x1414.
        assert!(super::software_renderer(false, "Microsoft Basic Render Driver", "Unknown Vendor (ID: 0x1414)"));
        assert!(super::software_renderer(false, "llvmpipe (LLVM 17.0.6, 256 bits)", "llvmpipe"));
        assert!(super::software_renderer(true, "anything", ""));
        assert!(!super::software_renderer(false, "NVIDIA GeForce RTX 4070", "NVIDIA Corporation"));
        assert!(!super::software_renderer(false, "Intel(R) UHD Graphics 770", "Intel Corporation"));
        assert!(!super::software_renderer(false, "Red Hat VirtIO GPU DOD controller", "Unknown Vendor (ID: 0x1AF4)"));
    }

    use super::control_h;
    use gpui_kit::{rems, Length};

    /// The standard control height is the toolkit's input and button at
    /// their default size, 2 rem (28 px on screen; tests/layout/rows.py and
    /// inputs.py hold every control to that number, so a change here
    /// changes them too).
    #[test]
    fn control_height_is_the_toolkits_default_input_height() {
        assert_eq!(control_h(), Length::from(rems(2.)));
    }
}

/// A box the standard control height ([`control_h`]), for a control drawn
/// here rather than by the toolkit: the settings field around its Input
/// and unit slot, the chips, the switch, a segmented choice.
pub fn input_box() -> Div {
    h_flex().h(control_h())
}
/// Below this window width a row stacks: label, field, current value.
pub const ROW_STACK_W: f32 = 820.;

/// What sits in a field's unit slot: its unit, or a button (a file's
/// Choose… or Check).
pub enum Slot {
    Unit(SharedString),
    Button(SharedString, SharedString, std::rc::Rc<dyn Fn(&mut Window, &mut App)>),
}

/// A unit as its slot names it (seconds, not s).
pub fn unit_word(unit: &str) -> &'static str {
    match unit {
        "s" => "seconds",
        "ms" => "ms",
        "min" => "minutes",
        "days" => "days",
        "MB" => "MB",
        "port" => "port",
        "threads" => "threads",
        _ => "",
    }
}

/// A field: the input, 260 px with its 64 px unit slot inside the box (a
/// line before it, the raised surface, muted 12 px text), the standard control height
/// with the theme's input radius; a
/// 1 px red border when `problem` is set, with the message in one 12 px
/// red line under it. Reports `field-input` (the box) and `field-slot`.
pub fn field_input(theme: &Theme, state: &Entity<gpui_kit::component::input::InputState>, slot: Option<Slot>, problem: Option<String>, mask: bool, window: &Window, cx: &App) -> Div {
    use gpui_kit::component::input::Input;
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    let border = if problem.is_some() { theme.danger } else if focused { theme.primary } else { theme.input };
    let mut input = Input::new(state).appearance(false);
    if mask {
        input = input.mask_toggle();
    }
    let slot_el = slot.map(|s| {
        let base = h_flex().relative().id("field-slot").w(px(UNIT_W)).h_full().flex_shrink_0().justify_center().items_center().border_l_1().border_color(theme.border).bg(theme.accent).text_size(px(12.)).text_color(theme.muted_foreground).child(layout_probe("field-slot"));
        match s {
            Slot::Unit(u) => base.child(u).into_any_element(),
            Slot::Button(id, label, f) => base.id(id).cursor_pointer().text_color(theme.primary).font_weight(FontWeight::MEDIUM).hover(|d| d.bg(theme.secondary)).child(label).on_click(move |_, window, cx| f(window, cx)).into_any_element(),
        }
    });
    v_flex()
        .gap(px(6.))
        .w(px(FIELD_W))
        .flex_shrink_0()
        .child(
            input_box()
                .relative()
                .w_full()
                .rounded(theme.radius)
                .border_1()
                .border_color(border)
                .bg(theme.background)
                .overflow_hidden()
                .child(div().flex_1().min_w_0().h_full().flex().items_center().child(input))
                .children(slot_el)
                .child(layout_probe("field-input")),
        )
        .when_some(problem, |d, p| d.child(div().text_size(px(12.)).text_color(theme.danger).whitespace_nowrap().overflow_hidden().text_ellipsis().child(p)))
}

/// A choice of a few values filling the field column, the standard control height.
pub fn chips(theme: &Theme, id: &str, items: &[&str], selected: Option<usize>, pick: std::rc::Rc<dyn Fn(usize, &mut Window, &mut App)>) -> Div {
    input_box()
        .relative()
        .w(px(FIELD_W))
        .flex_shrink_0()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.input)
        .overflow_hidden()
        .children(items.iter().enumerate().map(|(i, label)| {
            let on = selected == Some(i);
            let pick = pick.clone();
            div()
                .id(SharedString::from(format!("{id}-c{i}")))
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .px_1()
                .when(i > 0, |d| d.border_l_1().border_color(theme.input))
                .text_sm()
                .whitespace_nowrap()
                .overflow_hidden()
                .cursor_pointer()
                .when(on, |d| d.bg(theme.list_active).text_color(theme.primary).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(theme.muted_foreground).hover(|d| d.bg(theme.list_hover)))
                .child(label.to_string())
                .on_click(move |_, window, cx| pick(i, window, cx))
        }))
        .child(layout_probe("field-chips"))
}

/// On or off in the field column: the switch and its word, the standard control height.
pub fn toggle(theme: &Theme, id: impl Into<ElementId>, on: bool, note: Option<&'static str>, flip: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Div {
    input_box()
        .relative()
        .w(px(FIELD_W))
        .flex_shrink_0()
        .gap(px(10.))
        .items_center()
        .child(gpui_kit::component::switch::Switch::new(id).checked(on).on_click(flip))
        .child(div().text_sm().child(if on { "On" } else { "Off" }))
        .when_some(note, |r, n| r.child(div().text_size(px(12.)).text_color(theme.muted_foreground).child(n)))
        .child(layout_probe("field-control"))
}

/// The current value column, one line on the field's centre line: a muted
/// "Current" caption and the value in the body text, cut with an
/// ellipsis and whole in its tooltip. the standard control height beside the field, its
/// own height when the row is stacked. Reports `current-cell`.
pub fn current_cell(theme: &Theme, id: impl Into<SharedString>, text: Option<String>) -> Div {
    let id: SharedString = id.into();
    h_flex()
        .relative()
        .w(px(CURRENT_W))
        .flex_shrink_0()
        .min_w_0()
        .when(!stacked(), |d| d.h(control_h()))
        .gap(px(6.))
        .items_center()
        .when_some(text, |d, t| {
            let tip: SharedString = t.clone().into();
            d.child(div().flex_shrink_0().text_size(px(12.)).text_color(theme.muted_foreground).child("Current")).child(
                div()
                    .id(SharedString::from(format!("cur-{id}")))
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(t)
                    .tooltip(crate::ui::tooltip(tip.clone())),
            )
        })
        .child(layout_probe("current-cell"))
}

/// The "Restart" pill beside a setting's name.
pub fn restart_pill(cx: &App) -> Div {
    let (bg, fg) = if crate::theme::is_dark(cx) { (hsla(38. / 360., 0.92, 0.50, 0.14), rgb(0xfbbf24).into()) } else { (rgb(0xfef3c7).into(), rgb(0xb45309).into()) };
    let (bg, fg): (Hsla, Hsla) = (bg, fg);
    h_flex().flex_shrink_0().h(px(22.)).px(px(8.)).rounded(px(11.)).bg(bg).text_color(fg).text_size(px(11.)).font_weight(FontWeight::MEDIUM).child("Restart")
}

/// A group heading inside the settings card (12 px, uppercase, muted).
pub fn group_header(theme: &Theme, title: &str) -> Div {
    div().px(px(18.)).pt(px(18.)).pb(px(4.)).text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(title.to_uppercase())
}

/// One setting row of the card: [name, (i), Restart, description] [the
/// field, 260] [current value, 170], 18 px apart, padding 12 × 18, a line
/// above (not under a group heading). A changed row has the selection's
/// tint fading out from the left. `wide`: the control takes the field and
/// current columns. `stacked`: one column (a window under 820 px).
pub struct SettingRow {
    pub label: SharedString,
    pub help: Option<String>,
    pub desc: Option<String>,
    pub restart: bool,
    pub changed: bool,
    pub control: AnyElement,
    pub wide: bool,
    pub current: Option<String>,
    pub first: bool,
}

pub fn setting_row(theme: &Theme, r: SettingRow, cx: &App) -> Div {
    let stacked = stacked();
    let tint = theme.primary.opacity(if crate::theme::is_dark(cx) { 0.18 } else { 0.12 });
    let label = r.label.clone();
    // The name's line is a control tall beside the field, so the name, its
    // (i), the field and the current value share one centre line; the
    // description runs on under it.
    let lab = v_flex()
        .flex_1()
        .min_w_0()
        .gap(px(2.))
        .child(
            h_flex()
                .relative()
                .when(!stacked, |d| d.min_h(control_h()))
                .gap(px(6.))
                .items_center()
                .min_w_0()
                .child(div().min_w_0().text_sm().font_weight(FontWeight::MEDIUM).when(r.changed, |d| d.text_color(theme.primary)).child(label.clone()))
                .when_some(r.help, |d, h| d.child(info(SharedString::from(format!("help-{label}")), h.into())))
                .when(r.restart, |d| d.child(restart_pill(cx)))
                .child(layout_probe("setting-label")),
        )
        .when_some(r.desc, |d, t| d.child(div().text_size(px(12.)).text_color(theme.muted_foreground).child(t)));
    let current = (!r.wide).then(|| current_cell(theme, label.clone(), r.current));
    div()
        .flex()
        .when(stacked, |d| d.flex_col().gap(px(8.)))
        .when(!stacked, |d| d.flex_row().gap(px(COL_GAP)).items_start())
        .px(px(18.))
        .py(px(12.))
        .when(!r.first, |d| d.border_t_1().border_color(theme.border))
        .when(r.changed, |d| d.bg(linear_gradient(90., linear_color_stop(tint, 0.), linear_color_stop(tint.opacity(0.), 0.6))))
        .child(lab)
        .child(if r.wide && !stacked { h_flex().flex_shrink_0().w(px(FIELD_W + COL_GAP + CURRENT_W)).child(r.control) } else { h_flex().flex_shrink_0().child(r.control) })
        .children(current)
}

/// A plain value in a row's control column: body text on the name's line
/// (a line the standard control height, the text centred in it), its own
/// height when the row is stacked.
pub fn row_text(text: impl Into<SharedString>) -> Div {
    div().when(!stacked(), |d| d.min_h(control_h()).flex().items_center()).text_sm().child(text.into())
}

/// Something smaller than a control beside one in a row (a Reset link):
/// centred on the control's line.
pub fn on_control_line(el: impl IntoElement) -> Div {
    div().h(control_h()).flex().items_center().child(el)
}

impl SettingRow {
    /// A row with a control across the field and current columns.
    pub fn wide(label: impl Into<SharedString>, help: Option<String>, control: impl IntoElement) -> Self {
        SettingRow { label: label.into(), help, desc: None, restart: false, changed: false, control: control.into_any_element(), wide: true, current: None, first: false }
    }
}

/// A group of rows under its heading: the first row has no line above it;
/// `extra` (notices, a form) follows the rows inside the group's padding.
pub fn group(theme: &Theme, title: &str, rows: Vec<SettingRow>, extra: Vec<AnyElement>, cx: &App) -> Div {
    v_flex()
        .child(group_header(theme, title))
        .children(rows.into_iter().enumerate().map(|(i, mut r)| {
            r.first = i == 0;
            setting_row(theme, r, cx)
        }))
        .when(!extra.is_empty(), |d| d.child(v_flex().gap_3().px(px(18.)).pb(px(12.)).children(extra)))
}

static STACKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Setting rows stack (label, field, current value) on a window under
/// ROW_STACK_W; the settings pages set it as they draw.
pub fn set_stacked(on: bool) {
    STACKED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn stacked() -> bool {
    STACKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The settings card's tabs: on top, each with its count of changes (red
/// when it holds a field that is not valid); they wrap on a narrow window.
pub fn card_tabs(theme: &Theme, id: &str, titles: &[&'static str], active: usize, badges: &[(usize, bool)], pick: std::rc::Rc<dyn Fn(usize, &mut App)>) -> Div {
    card_tabs_locked(theme, id, titles, active, badges, &[], pick)
}

/// [`card_tabs`], a small lock after the title of each tab in `locked`
/// (its values are set elsewhere: the gateway's SENTIENT-owned tabs).
pub fn card_tabs_locked(theme: &Theme, id: &str, titles: &[&'static str], active: usize, badges: &[(usize, bool)], locked: &[bool], pick: std::rc::Rc<dyn Fn(usize, &mut App)>) -> Div {
    h_flex()
        .flex_wrap()
        .gap(px(4.))
        .px(px(12.))
        .border_b_1()
        .border_color(theme.border)
        .children(titles.iter().enumerate().map(|(i, title)| {
            let on = i == active;
            let pick = pick.clone();
            let (n, bad) = badges.get(i).copied().unwrap_or((0, false));
            h_flex()
                .id(SharedString::from(format!("{id}tab-{i}")))
                .h(px(44.))
                .px(px(14.))
                .gap(px(8.))
                .items_center()
                .border_b_2()
                .border_color(if on { theme.primary } else { transparent_black() })
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .cursor_pointer()
                .text_color(if on { theme.primary } else { theme.muted_foreground })
                .when(!on, |d| d.hover(|d| d.text_color(theme.foreground)))
                .child(*title)
                .when(locked.get(i).copied().unwrap_or(false), |d| {
                    d.child(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Lock).size(px(13.)).text_color(theme.muted_foreground))
                })
                .when(n > 0 || bad, |d| {
                    d.child(
                        div()
                            .min_w(px(18.))
                            .h(px(18.))
                            .px(px(5.))
                            .rounded(px(9.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .bg(if bad { theme.danger } else { theme.primary })
                            .text_color(if bad { theme.background } else { theme.primary_foreground })
                            .child(n.max(1).to_string()),
                    )
                })
                .on_click(move |_, _, cx| pick(i, cx))
        }))
}

/// The settings card: the tabs on top, the open tab's rows scrolling under
/// them, and the Apply bar pinned to the card's bottom edge while anything
/// is not applied. Reports `panel` and `settings-card`, and `page-area`
/// (the scrolling part, which ends where the bar starts).
pub fn settings_card(theme: &Theme, scroll_id: &'static str, tabs: Div, body: impl IntoElement, bar: Option<AnyElement>) -> Div {
    v_flex()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .rounded(px(PANEL_RADIUS))
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .child(layout_probe("panel"))
        .child(layout_probe("settings-card"))
        .child(tabs)
        .child(div().id(scroll_id).relative().flex_1().min_h_0().overflow_y_scrollbar().child(layout_probe("page-area")).child(body))
        .children(bar)
}

/// What the Apply bar says: how many changes, or how many fields to fix.
pub fn pending_text(changes: usize, invalid: usize) -> String {
    if invalid > 0 {
        format!("Fix {invalid} field{} first", if invalid == 1 { "" } else { "s" })
    } else {
        format!("{changes} change{} not applied", if changes == 1 { "" } else { "s" })
    }
}

/// The Apply bar's small line: the changed settings' names and the
/// restart, or what is wrong and how many changes wait.
pub fn pending_line(changes: usize, names: &[String], restart: bool, first_invalid: Option<(&str, bool)>) -> String {
    if let Some((label, number)) = first_invalid {
        let what = if number { "needs a number" } else { "is out of range" };
        return format!("{label} {what}. {changes} change{} waiting.", if changes == 1 { "" } else { "s" });
    }
    let mut line = names.join(", ");
    if !line.is_empty() {
        line.push('.');
    }
    if restart {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(if changes == 1 { "SENTIENT restarts once to apply it." } else { "SENTIENT restarts once to apply them." });
    }
    line
}

/// The Apply bar, at the settings card's bottom edge (raised surface, a
/// line above, 12 × 18 padding, the card's bottom corners): the count in
/// bold (red: "Fix 1 field first") with its small line, then Discard and
/// Apply ("Apply and restart" when a change restarts SENTIENT), 40 px
/// tall, 12 px apart. None when nothing is pending. It slides up as it
/// appears (not with reduced motion).
#[allow(clippy::too_many_arguments)]
pub fn pending_bar(
    theme: &Theme,
    id: &'static str,
    changes: usize,
    invalid: usize,
    line: String,
    restart: bool,
    can_apply: bool,
    on_discard: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_apply: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Option<AnyElement> {
    use gpui_kit::component::Disableable as _;
    if changes == 0 && invalid == 0 {
        return None;
    }
    let tip: SharedString = line.clone().into();
    let bar = h_flex()
        .id(id)
        .relative()
        .flex_shrink_0()
        .w_full()
        .px(px(18.))
        .py(px(12.))
        .gap(button_gap())
        .items_center()
        .bg(theme.accent)
        .border_t_1()
        .border_color(theme.border)
        .rounded_b(px(PANEL_RADIUS))
        .child(layout_probe("pending-bar"))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).when(invalid > 0, |d| d.text_color(theme.danger)).child(pending_text(changes, invalid)))
                .when(!line.is_empty(), |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("{id}-line")))
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(line)
                            .tooltip(crate::ui::tooltip(tip.clone())),
                    )
                }),
        )
        .child(probed(button(SharedString::from(format!("{id}-discard"))).outline().label("Discard").on_click(on_discard)))
        .child(probed(
            button(SharedString::from(format!("{id}-apply")))
                .primary()
                .label(if restart { "Apply and restart" } else { "Apply" })
                .disabled(invalid > 0 || !can_apply)
                .on_click(on_apply),
        ));
    Some(if reduce_motion() {
        bar.into_any_element()
    } else {
        bar.with_animation(SharedString::from(format!("{id}-in")), Animation::new(std::time::Duration::from_millis(200)).with_easing(ease_out), |el, t| el.opacity(t).top(px(16. * (1. - t))))
            .into_any_element()
    })
}

/// A page's changes not applied, asked about before leaving it (a menu
/// click, another instance): how many and how many fields are not valid,
/// each change as "Name: old to new", and how to apply or discard them.
/// Registered by the page under its scope (None: this computer; Some(id):
/// that server).
#[derive(Clone)]
pub struct Unsaved {
    pub pending: std::rc::Rc<dyn Fn(&App) -> (usize, usize)>,
    pub list: std::rc::Rc<dyn Fn(&App) -> Vec<String>>,
    pub apply: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    pub discard: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
}

thread_local! {
    static UNSAVED: std::cell::RefCell<std::collections::HashMap<Option<String>, Unsaved>> = Default::default();
}

pub fn set_unsaved(scope: Option<String>, u: Unsaved) {
    UNSAVED.with(|m| m.borrow_mut().insert(scope, u));
}

/// The scope's changes not applied: (changes, fields not valid).
pub fn unsaved(scope: &Option<String>, cx: &App) -> (usize, usize) {
    UNSAVED.with(|m| m.borrow().get(scope).cloned()).map(|u| (u.pending)(cx)).unwrap_or((0, 0))
}

/// Leave a page with changes not applied: asks once ("Apply your changes
/// first?", the changes listed), Stay, Discard or Apply and leave (not
/// while a field is not valid); Discard and Apply then go on (`go`).
/// Returns false when nothing is pending: the caller goes on itself.
pub fn ask_before_leaving(scope: Option<String>, window: &mut Window, cx: &mut App, go: std::rc::Rc<dyn Fn(&mut Window, &mut App)>) -> bool {
    let Some(u) = UNSAVED.with(|m| m.borrow().get(&scope).cloned()) else { return false };
    let (changes, invalid) = (u.pending)(cx);
    if changes == 0 && invalid == 0 {
        return false;
    }
    let list = (u.list)(cx);
    let text = if invalid > 0 {
        format!("These settings are changed but not applied, and {invalid} field{} not valid (stay to fix {}):", if invalid == 1 { " is" } else { "s are" }, if invalid == 1 { "it" } else { "them" })
    } else {
        "These settings are changed but not applied:".to_string()
    };
    let (go2, discard) = (go.clone(), u.discard.clone());
    crate::modal::alert(window, cx, move |dialog, _, _| {
        let (go, go2, apply, discard) = (go.clone(), go2.clone(), u.apply.clone(), discard.clone());
        let d = dialog
            .title("Apply your changes first?")
            .description(text.clone())
            .list(list.clone())
            .cancel_text("Stay")
            .secondary("Discard", move |window, cx| {
                discard(window, cx);
                go2(window, cx);
            });
        if invalid > 0 {
            d.no_ok()
        } else {
            d.ok_text("Apply and leave").on_ok(move |_, window, cx| {
                apply(window, cx);
                go(window, cx);
                true
            })
        }
    });
    true
}

/// Screens: the leave prompt with a canned list (SPM_DEBUG_SETTINGS_PENDING=leave).
pub fn debug_leave_prompt(scope: Option<String>, window: &mut Window, cx: &mut App) {
    ask_before_leaving(scope, window, cx, std::rc::Rc::new(|_, _| {}));
}

#[cfg(test)]
mod menu_fit_tests {
    use super::{MENU_MAX_H, menu_fit};

    #[test]
    fn a_long_menu_opens_where_there_is_room_and_fits_there() {
        // The rc1 VM: 1920x1080 at 150 % (720 logical), the version picker
        // near the bottom of the Update page. It opens upwards, within the window.
        let (up, h) = menu_fit(497., 525., 720.);
        assert!(up);
        assert!(h <= MENU_MAX_H && h <= 497. - 16.);
        // Plenty of room below: below, at the full height.
        assert_eq!(menu_fit(100., 128., 900.), (false, MENU_MAX_H));
        // Little room either side: the larger side, no taller than its room.
        let (up, h) = menu_fit(180., 208., 420.);
        assert!(!up);
        assert_eq!(h, 420. - 208. - 16.);
        let (up, h) = menu_fit(250., 278., 420.);
        assert!(up);
        assert_eq!(h, 250. - 16.);
    }
}

/// A scroll area that keeps its own size and scroll (a box sized by its
/// content, a list scrolled from code) with the shared bar drawn over its
/// edge: the bar paints where its handle's area is (gpui_base::Scrollbar
/// reads the handle's bounds), so it sits inside the area, last, and takes
/// no room. Shown near the edge and while scrolling (crate::theme). Every
/// scroll area that is not overflow_y_scrollbar is one of these.
#[derive(IntoElement)]
pub struct ScrollY {
    el: Stateful<Div>,
    handle: Option<ScrollHandle>,
}

/// [`ScrollY`] with a handle of its own, kept by the element's id.
pub fn scroll_y(el: Stateful<Div>) -> ScrollY {
    ScrollY { el, handle: None }
}

/// [`ScrollY`] on `handle` (code scrolls it: Follow, a selected row).
pub fn scroll_y_with(el: Stateful<Div>, handle: &ScrollHandle) -> ScrollY {
    ScrollY { el, handle: Some(handle.clone()) }
}

impl RenderOnce for ScrollY {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let h = match self.handle.take() {
            Some(h) => h,
            None => {
                let id = self.el.interactivity().element_id.clone().unwrap_or_else(|| ElementId::Name("scroll-y".into()));
                window.use_keyed_state((id, "scroll-y"), cx, |_, _| ScrollHandle::new()).read(cx).clone()
            }
        };
        self.el
            .track_scroll(&h)
            .overflow_y_scroll() /* scroll-own: this is the shared bar's own area */
            .child(div().absolute().top_0().left_0().size_0().child(gpui_kit::component::scroll::Scrollbar::vertical(&h)))
    }
}
