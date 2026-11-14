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

/// A content panel: surface colour, hairline border, panel radius.
pub fn panel(theme: &Theme) -> Div {
    v_flex()
        .gap_3()
        .p_4()
        .rounded(theme.radius)
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

/// SENTIENT colours its console output with ANSI escape sequences
/// (`ESC [ … m`); the app shows plain text.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and intermediates, then one final byte in @..~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
