// Copyright © 2016-2026 The SENTIENT Authors

//! Small presentation pieces shared by the screens: the white panel the
//! design system puts all content on, page headings, and status pills.

use gpui_kit::component::{Theme, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

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
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Run SENTIENT-Setup.exe to install it. The Platform Manager is installed with it."),
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
