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
pub fn show_error_with(title: impl Into<SharedString>, text: impl Into<SharedString>, retry: Option<std::rc::Rc<dyn Fn(&mut App)>>, cx: &mut App) {
    struct ErrorBody(SharedString, Option<std::rc::Rc<dyn Fn(&mut App)>>);
    impl Render for ErrorBody {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_kit::component::ActiveTheme as _;
            use gpui_kit::component::button::ButtonVariants as _;
            let theme = cx.theme().clone();
            let (head, detail) = crate::remote::split_failure(&self.0);
            let copy = self.0.to_string();
            gpui_kit::component::v_flex()
                .gap_3()
                .child(div().text_sm().child(head))
                .when_some(detail.filter(|d| !d.trim().is_empty()), |c, d| {
                    c.child(div().id("error-detail").max_h(px(300.)).overflow_y_scroll().p_2().rounded(theme.radius).bg(theme.muted).font_family(theme.mono_font_family.clone()).text_size(px(12.5)).child(d.trim().to_string()))
                })
                .child(
                    gpui_kit::component::h_flex()
                        .gap_2()
                        .justify_end()
                        .child(button("error-copy").outline().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                        .when_some(self.1.clone(), |r, retry| {
                            r.child(button("error-retry").outline().label("Retry").on_click(move |_, _, cx| {
                                crate::modal::close(cx);
                                retry(cx);
                            }))
                        })
                        .child(button("error-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))),
                )
        }
    }
    let text = text.into();
    let view = cx.new(|_| ErrorBody(text, retry));
    crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: 560., on_close: None }, cx);
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
\"SENTIENT is starting\" meanwhile. Change it later under Control.";

/// "Start delay [ 15 ] seconds (i)": the installer's and Control's field.
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
pub fn panel(theme: &Theme) -> Div {
    v_flex()
        .gap_3()
        .p_4()
        .rounded(px(PANEL_RADIUS))
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
}

/// The scrolling page area to the right of the navigation.
pub fn page(theme: &Theme, title: &'static str, summary: &'static str) -> Stateful<Div> {
    v_flex()
        .id("page")
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_y_scroll()
        // The mockup's page: 16 px padding, 14 px between blocks, the title
        // and its line of text as one block.
        .p_4()
        .gap(px(14.))
        .bg(theme.muted)
        .child(
            v_flex()
                .gap_0p5()
                .child(div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_sm().text_color(theme.muted_foreground).child(summary)),
        )
}

/// A page whose title row ends with an action (the remote pages' Read
/// again), in the same place on every page.
pub fn page_with(theme: &Theme, title: &'static str, summary: &'static str, action: impl IntoElement) -> Stateful<Div> {
    v_flex().id("page").flex_1().min_w_0().h_full().overflow_y_scroll().p_4().gap(px(14.)).bg(theme.muted).child(
        h_flex()
            .gap_3()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(summary)),
            )
            .child(action),
    )
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
        .child(div().min_w_0().child(value.into()))
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
    use gpui_kit::component::button::ButtonVariants as _;
    use gpui_kit::component::Sizable as _;
    button(id).ghost().xsmall().icon(gpui_kit::component::IconName::Info).tooltip(help)
}

/// Shown on every manager screen when this computer has no installation.
pub fn not_installed(theme: &Theme) -> Div {
    panel(theme)
        .child(div().font_weight(FontWeight::MEDIUM).child("SENTIENT isn't installed on this computer"))
        .child(div().text_sm().text_color(theme.muted_foreground).child(
            "Download and install SENTIENT: Setup downloads the version you choose from GitHub. Without internet, install from local files: the unpacked SENTIENT package folder (with SENTIENT-Setup and payload).",
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    crate::ui::button("install-sentient")
                        .primary()
                        .label("Download and install SENTIENT")
                        .on_click(|_, window, cx| crate::standalone::install_sentient(window, cx)),
                )
                .child(
                    crate::ui::button("install-sentient-package")
                        .outline()
                        .label("Install from local files…")
                        .on_click(|_, window, cx| crate::standalone::install_sentient_from_package(window, cx)),
                )
                .child(
                    crate::ui::button("remove-manager")
                        .outline()
                        .label("Remove the Platform Manager")
                        .on_click(|_, window, cx| crate::standalone::remove_manager(window, cx)),
                ),
        )
}

/// The result of a job: like a pill in colour, but a block that wraps, one
/// row per line, for messages longer than a word or two.
/// A page's own notice, directly in the page (never inside a card): an
/// icon, a headline, the rest of the text, and an optional action.
pub fn notice(theme: &Theme, tone: Tone, text: impl Into<SharedString>) -> Div {
    notice_with(theme, tone, text, None)
}

pub fn notice_with(theme: &Theme, tone: Tone, text: impl Into<SharedString>, action: Option<AnyElement>) -> Div {
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
                .children(rest.into_iter().map(|l| div().child(l))),
        )
        .children(action)
}

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
    gpui_kit::component::h_flex()
        .id(id)
        .gap_3()
        .p_3()
        .items_start()
        .rounded(px(PANEL_RADIUS))
        .border_1()
        .border_color(if selected { theme.primary } else { theme.border })
        .when(selected, |c| c.bg(theme.list_active))
        .cursor_pointer()
        .hover(|c| c.border_color(theme.primary))
        .child(
            div()
                .mt(px(2.))
                .size_4()
                .flex_shrink_0()
                .rounded_full()
                .border_2()
                .border_color(if selected { theme.primary } else { theme.muted_foreground })
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |r| r.child(div().size_2().rounded_full().bg(theme.primary))),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().font_weight(FontWeight::MEDIUM).child(title.into()))
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

/// Per-row actions on the Control page: (service name, action) and
/// (service name) for its log.
pub type ServiceAct = std::rc::Rc<dyn Fn(&str, &'static str, &mut Window, &mut App)>;
pub type ServiceLog = std::rc::Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The Services panel's rows: state, plain name with the raw name muted, the
/// detail faint; on Control also Start, Stop, Restart and Logs per row.
/// The same on this computer and on a remote server.
pub fn service_rows(
    theme: &Theme,
    services: impl IntoIterator<Item = (SharedString, bool, bool, SharedString)>,
    actions: Option<(ServiceAct, ServiceLog, Option<String>)>,
) -> Vec<Div> {
    // One order on every page: SENTIENT, Database, ClickHouse, Gateway.
    let rank = |n: &str| match service_label(n) {
        Some("SENTIENT") => 0,
        Some("Database (PostgreSQL)") => 1,
        Some("ClickHouse") => 2,
        Some("Gateway") => 3,
        _ => 4,
    };
    let mut services: Vec<_> = services.into_iter().collect();
    services.sort_by_key(|(n, ..)| rank(n));
    services
        .into_iter()
        .map(|(name, running, starting, detail)| {
            let (tone, label) = if running { (Tone::Good, "Running") } else if starting { (Tone::Warn, "Starting") } else { (Tone::Bad, "Stopped") };
            let plain = service_label(&name);
            let busy = actions.as_ref().and_then(|a| a.2.as_ref()).is_some_and(|b| *b == *name);
            let any_busy = actions.as_ref().is_some_and(|a| a.2.is_some());
            let narrow = narrow();
            let state_pill = if busy { pill(theme, Tone::Warn, "Working…") } else { pill(theme, tone, label) }.min_w(px(86.)).justify_center();
            let raw = plain.is_some().then(|| div().min_w_0().overflow_hidden().text_ellipsis().text_size(px(12.)).font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(name.clone()));
            let uptime = (!detail.is_empty()).then(|| div().min_w_0().overflow_hidden().text_ellipsis().text_color(theme.muted_foreground).child(SharedString::from(if narrow { detail.to_string() } else { format!("· {detail}") })));
            // Wide: the names on the row; narrow: on the second line.
            let (raw_in, raw, uptime_in, uptime) = if narrow { (None, raw, None, uptime) } else { (raw, None, uptime, None) };
            let row = gpui_kit::component::h_flex()
                .gap_4()
                .text_sm()
                .items_center()
                // The state column lines up with the fact labels; the names
                // are cut with an ellipsis, never overlapped.
                .child(if narrow { gpui_kit::component::h_flex().flex_shrink_0().child(state_pill) } else { gpui_kit::component::h_flex().w(px(168.)).flex_shrink_0().child(state_pill) })
                .child(
                    gpui_kit::component::h_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .gap_2()
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().child(SharedString::from(plain.unwrap_or(&name).to_string())))
                        .children(raw_in)
                        .children(uptime_in),
                );
            let row = row.when_some(actions.clone(), |r, (act, log, _)| {
                    let icon = |id: &str, icon: gpui_kit::assets::IconName, tip: &'static str, enabled: bool, action: &'static str| {
                        let (act, n) = (act.clone(), name.to_string());
                        button(SharedString::from(format!("svc-{action}-{id}")))
                            .ghost()
                            .icon(gpui_kit::component::Icon::new(icon))
                            .tooltip(tip)
                            .when(!enabled || any_busy, |b| gpui_kit::component::Disableable::disabled(b, true))
                            .on_click(move |_, window, cx| act(&n, action, window, cx))
                    };
                    let n = name.to_string();
                    r.child(
                        gpui_kit::component::h_flex()
                            .gap_1()
                            .flex_shrink_0()
                            .child(icon(&name, gpui_kit::assets::IconName::Play, "Start", !running, "start"))
                            .child(icon(&name, gpui_kit::assets::IconName::Square, "Stop", running, "stop"))
                            .child(icon(&name, gpui_kit::assets::IconName::RotateCw, "Restart", running, "restart"))
                            .child(button(SharedString::from(format!("svc-log-{name}"))).ghost().label("Logs").on_click(move |_, window, cx| log(&n, window, cx))),
                    )
                });
            // Narrow: the unit or container name and its uptime on a second line.
            if narrow && (raw.is_some() || uptime.is_some()) {
                gpui_kit::component::v_flex()
                    .gap_0p5()
                    .child(row)
                    .child(gpui_kit::component::h_flex().gap_2().text_sm().whitespace_nowrap().overflow_hidden().children(raw).children(uptime))
            } else {
                gpui_kit::component::v_flex().child(row)
            }
        })
        .collect()
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
pub fn popover_menu(theme: &Theme, id: &'static str, width: f32, rows: Vec<MenuRow>, up: bool, pointer_x: Pixels, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Div {
    use gpui_kit::component::{h_flex, v_flex};
    let rows = rows.into_iter().map(|r| match r {
        MenuRow::Divider => div().my_1().mx_1().h(px(1.)).bg(theme.border).into_any_element(),
        MenuRow::Item { id, icon, label, secondary, on, click } => h_flex()
            .id(id)
            .gap_2()
            .items_center()
            .px_2()
            .py(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_sm()
            .when(on, |r| r.bg(theme.primary.opacity(0.10)).text_color(theme.primary).font_weight(FontWeight::MEDIUM))
            .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
            .children(icon)
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
            .when(on, |r| r.child(gpui_kit::component::Icon::new(gpui_kit::component::IconName::Check).size_4()))
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
}

/// Every button: the toolkit's, with the mockup's medium-weight label.
pub fn button(id: impl Into<gpui_kit::ElementId>) -> gpui_kit::component::button::Button {
    gpui_kit::component::button::Button::new(id).font_weight(FontWeight::MEDIUM)
}

/// `/home/mz/…/sentient-platform-manager`: a long text cut in the middle to
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

/// Screens and layout tests: an invisible child that reports its parent's
/// bounds on stderr as `LAYOUT <name> left right` (SPM_DEBUG_LAYOUT set).
pub fn layout_probe(name: &'static str) -> impl IntoElement {
    let on = cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some();
    div().absolute().inset_0().when(on, move |d| {
        d.child(
            canvas(
                move |bounds, _, _| {
                    eprintln!("LAYOUT {name} {:.0} {:.0}", f32::from(bounds.origin.x), f32::from(bounds.origin.x + bounds.size.width));
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    })
}
