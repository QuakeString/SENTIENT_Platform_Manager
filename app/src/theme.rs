// Copyright © 2016-2026 The SENTIENT Authors

//! The SENTIENT look for GPUI Kit: the design system's teal brand, surfaces
//! and radii, in light and dark. Colour values live in
//! `assets/sentient-theme.json` (GPUI Kit's theme format) so views only ever
//! read semantic roles from `cx.theme()`.

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::App;

const THEMES: &str = include_str!("../assets/sentient-theme.json");
const LIGHT: &str = "SENTIENT Light";
const DARK: &str = "SENTIENT Dark";

/// Register the SENTIENT themes and make them the light and dark themes.
/// Call once, right after `gpui_kit::init`.
pub fn init(mode: ThemeMode, cx: &mut App) {
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(THEMES)
        .expect("sentient-theme.json is valid");
    let themes = ThemeRegistry::global(cx).themes();
    let light = themes.get(LIGHT).cloned().expect("light theme registered");
    let dark = themes.get(DARK).cloned().expect("dark theme registered");
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    Theme::change(mode, None, cx);
}

/// Flip between light and dark.
pub fn toggle(cx: &mut App) {
    let next = if Theme::global(cx).mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(next, None, cx);
}

pub fn is_dark(cx: &App) -> bool {
    Theme::global(cx).mode.is_dark()
}
