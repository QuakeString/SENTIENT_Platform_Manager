// Copyright © 2016-2026 The SENTIENT Authors

//! Small presentation pieces shared by the screens: the white panel the
//! design system puts all content on, page headings, and status pills.

use gpui_kit::component::{
    Theme, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
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
    #[cfg(target_os = "linux")]
    if let Some(wayland) = std::env::var_os(ORIGINAL_WAYLAND) {
        let _ = std::process::Command::new("xdg-open").arg(url).env("WAYLAND_DISPLAY", wayland).spawn();
        return;
    }
    cx.open_url(url);
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
    use gpui_kit::component::{Sizable as _, button::{Button, ButtonVariants as _}, input::Input, IconName};
    gpui_kit::component::h_flex()
        .gap_2()
        .items_center()
        .child(div().text_sm().child("Start delay"))
        .child(div().w_20().child(Input::new(state).small()))
        .child(div().text_sm().text_color(theme.muted_foreground).child("seconds"))
        .child(Button::new("start-delay-help").ghost().xsmall().icon(IconName::Info).tooltip(START_DELAY_HELP))
}

/// The corner radius of panels and cards (buttons and inputs keep the
/// theme's smaller one).
pub const PANEL_RADIUS: f32 = 8.;

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
        .p_8()
        .gap_4()
        .bg(theme.muted)
        .child(div().text_2xl().child(title))
        .child(div().text_color(theme.muted_foreground).child(summary))
}

#[derive(Clone, Copy)]
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
        Tone::Bad => (theme.danger, theme.danger_foreground),
        Tone::Neutral => (theme.secondary, theme.secondary_foreground),
    };
    h_flex()
        .px_2p5()
        .py_0p5()
        .rounded_full()
        .text_xs()
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
        .child(div().w_56().flex_shrink_0().text_color(theme.muted_foreground).child(label))
        .child(div().min_w_0().child(value.into()))
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
                    Button::new("install-sentient")
                        .primary()
                        .label("Download and install SENTIENT")
                        .on_click(|_, window, cx| crate::standalone::install_sentient(window, cx)),
                )
                .child(
                    Button::new("install-sentient-package")
                        .outline()
                        .label("Install from local files…")
                        .on_click(|_, window, cx| crate::standalone::install_sentient_from_package(window, cx)),
                )
                .child(
                    Button::new("remove-manager")
                        .outline()
                        .label("Remove the Platform Manager")
                        .on_click(|_, window, cx| crate::standalone::remove_manager(window, cx)),
                ),
        )
}

/// The result of a job: like a pill in colour, but a block that wraps, one
/// row per line, for messages longer than a word or two.
pub fn outcome(theme: &Theme, tone: Tone, text: impl Into<SharedString>) -> Div {
    let (bg, fg) = match tone {
        Tone::Good => (theme.success, theme.success_foreground),
        Tone::Warn => (theme.warning, theme.warning_foreground),
        Tone::Bad => (theme.danger, theme.danger_foreground),
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
                    .when(!n.date.is_empty(), |r| r.child(div().text_xs().text_color(theme.muted_foreground).child(n.date.clone()))),
            )
            .when(!n.summary.is_empty(), |c| c.child(div().text_sm().text_color(theme.muted_foreground).child(n.summary.clone())))
            .children(n.sections.iter().map(|(name, entries)| {
                v_flex()
                    .gap_0p5()
                    .pt_1()
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.primary).child(name.to_uppercase()))
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
    gpui_kit::component::h_flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
}

/// One segment of a [`seg_track`]; give it `.on_click(...)`.
pub fn seg_item(theme: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    let radius = if theme.radius > px(3.) { theme.radius - px(2.) } else { theme.radius };
    div()
        .id(id)
        .h(px(26.))
        .px_3()
        .flex()
        .items_center()
        .rounded(radius)
        .text_sm()
        .whitespace_nowrap()
        .cursor_pointer()
        .when(on, |d| d.bg(theme.primary).text_color(theme.primary_foreground).font_weight(FontWeight::MEDIUM))
        .when(!on, |d| d.text_color(theme.foreground).hover(|d| d.bg(theme.list_hover)))
        .child(label.into())
}
