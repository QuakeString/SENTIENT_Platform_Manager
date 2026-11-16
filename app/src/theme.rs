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
    use_desktop_font(cx);
    thin_focus(cx);
}

/// Linux: draw the UI in the desktop's own font (KDE's general font, GNOME's
/// interface font, else fontconfig's sans-serif). GPUI maps its system font
/// to a fixed family on Linux, which most desktops do not have, so the text
/// otherwise falls back to whatever GPUI finds first. Windows and macOS
/// already use the system font.
fn use_desktop_font(cx: &mut App) {
    #[cfg(target_os = "linux")]
    {
        let installed = cx.text_system().all_font_names();
        // The desktop's choice, else what fontconfig gives for sans-serif.
        if let Some(family) = desktop_font_families().into_iter().find_map(|want| installed.iter().find(|f| f.eq_ignore_ascii_case(&want)).cloned()) {
            Theme::global_mut(cx).font_family = family.into();
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cx;
}

#[cfg(target_os = "linux")]
fn desktop_font_families() -> Vec<String> {
    let mut v: Vec<String> = desktop_font_family().into_iter().collect();
    if let Some(f) = std::process::Command::new("fc-match").args(["-f", "%{family[0]}", "sans-serif"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()) {
        v.push(f);
    }
    v
}

#[cfg(target_os = "linux")]
fn desktop_font_family() -> Option<String> {
    use std::process::Command;
    let out = |cmd: &str, args: &[&str]| {
        Command::new(cmd).args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
    };
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    // KDE: kdeglobals [General] font=Noto Sans,10,-1,5,400,…
    if desktop.contains("KDE") {
        for tool in ["kreadconfig6", "kreadconfig5"] {
            if let Some(v) = out(tool, &["--file", "kdeglobals", "--group", "General", "--key", "font"]) {
                return v.split(',').next().map(str::to_string);
            }
        }
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
        if let Ok(text) = std::fs::read_to_string(home.join(".config/kdeglobals")) {
            if let Some(v) = text.lines().find_map(|l| l.strip_prefix("font=")) {
                return v.split(',').next().map(str::to_string);
            }
        }
    }
    // GNOME and others with GSettings: 'Cantarell 11'
    if let Some(v) = out("gsettings", &["get", "org.gnome.desktop.interface", "font-name"]) {
        let v = v.trim_matches('\'');
        let name = v.rsplit_once(' ').filter(|(_, size)| size.parse::<f32>().is_ok()).map(|(n, _)| n).unwrap_or(v);
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
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

/// The top bar's controls (the instance switcher, the monitors, the theme
/// button and the bell): one height, one radius, one hover tint (the
/// instance's colour at this opacity), centred in the 60 px bar.
pub const BAR_CTRL_H: f32 = 40.;
pub const BAR_CTRL_RADIUS: f32 = 8.;
pub const BAR_CTRL_HOVER: f32 = 0.22;

/// Plain information (an update waiting): a soft blue, for the
/// notifications' tint and icon.
pub fn info(cx: &App) -> gpui_kit::Hsla {
    if is_dark(cx) { gpui_kit::rgb(0x60a5fa).into() } else { gpui_kit::rgb(0x2563eb).into() }
}

pub fn is_dark(cx: &App) -> bool {
    Theme::global(cx).mode.is_dark()
}

/// The theme choice: light, dark, or the system's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Light,
    Dark,
    System,
}

impl Choice {
    pub const ALL: [Choice; 3] = [Choice::Light, Choice::Dark, Choice::System];

    pub fn label(self) -> &'static str {
        match self {
            Choice::Light => "Light",
            Choice::Dark => "Dark",
            Choice::System => "Follow system",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Choice::Light => "light",
            Choice::Dark => "dark",
            Choice::System => "system",
        }
    }

    /// The saved choice.
    pub fn saved() -> Choice {
        match crate::updates::Prefs::load().theme.as_deref() {
            Some("light") => Choice::Light,
            Some("dark") => Choice::Dark,
            _ => Choice::System,
        }
    }
}

/// Apply and remember a theme choice.
pub fn choose(choice: Choice, window: Option<&mut gpui_kit::Window>, cx: &mut App) {
    apply(choice, window, cx);
    let mut prefs = crate::updates::Prefs::load();
    prefs.theme = Some(choice.key().into());
    prefs.save();
}

pub fn apply(choice: Choice, window: Option<&mut gpui_kit::Window>, cx: &mut App) {
    match choice {
        Choice::Light => Theme::change(ThemeMode::Light, None, cx),
        Choice::Dark => Theme::change(ThemeMode::Dark, None, cx),
        Choice::System => match system_dark() {
            Some(dark) => Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx),
            None => Theme::sync_system_appearance(window, cx),
        },
    }
    thin_focus(cx);
}

/// "Follow system": look at the desktop's scheme every few seconds and
/// switch while the window is open. Call once at start.
pub fn follow_system(cx: &mut App) {
    if !cfg!(target_os = "linux") {
        return;
    }
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
        let saved = Choice::saved();
        if saved != Choice::System {
            continue;
        }
        let Some(dark) = cx.background_executor().spawn(async { system_dark() }).await else { continue };
        let _ = cx.update(|cx| {
            if is_dark(cx) != dark {
                Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
                thin_focus(cx);
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

/// Is the desktop dark? Linux: on KDE Plasma its own colours (the window
/// background in kdeglobals), then the desktop portal's color-scheme, then
/// GNOME's setting. None: not known here (GPUI decides).
pub fn system_dark() -> Option<bool> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    if desktop.split(':').any(|d| d == "KDE") {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
        let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".config"));
        if let Some(dark) = std::fs::read_to_string(config.join("kdeglobals")).ok().and_then(|t| kde_dark(&t)) {
            return Some(dark);
        }
    }
    let portal = std::process::Command::new("busctl")
        .args(["--user", "--timeout=2", "call", "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings", "ReadOne", "ss", "org.freedesktop.appearance", "color-scheme"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| portal_dark(&String::from_utf8_lossy(&o.stdout)));
    if portal.is_some() {
        return portal;
    }
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let v = String::from_utf8_lossy(&o.stdout).to_string();
            if v.contains("prefer-dark") { Some(true) } else if v.contains("prefer-light") || v.contains("default") { Some(false) } else { None }
        })
}

/// kdeglobals: dark when [Colors:Window] BackgroundNormal is dark (works for
/// any colour scheme, custom ones too).
fn kde_dark(text: &str) -> Option<bool> {
    // The scheme's name when its colours are not written out.
    let by_name = text
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("ColorScheme="))
        .map(|n| n.to_lowercase().contains("dark"));
    kde_window_dark(text).or(by_name)
}

fn kde_window_dark(text: &str) -> Option<bool> {
    let mut in_window = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_window = l == "[Colors:Window]";
            continue;
        }
        if in_window {
            if let Some(v) = l.strip_prefix("BackgroundNormal=") {
                let rgb: Vec<f32> = v.split(',').filter_map(|c| c.trim().parse().ok()).collect();
                if rgb.len() >= 3 {
                    let lum = (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]) / 255.0;
                    return Some(lum < 0.5);
                }
            }
        }
    }
    None
}

/// The portal's color-scheme answer, `v u 1`: 1 dark, 2 light, 0 no preference.
fn portal_dark(out: &str) -> Option<bool> {
    match out.split_whitespace().last()? {
        "1" => Some(true),
        "2" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod scheme_tests {
    use super::*;

    #[test]
    fn reads_plasma_and_the_portal() {
        let breeze_light = "[General]\nColorScheme=BreezeLight\n\n[Colors:Window]\nBackgroundAlternate=227,229,231\nBackgroundNormal=239,240,241\n";
        let breeze_dark = "[Colors:View]\nBackgroundNormal=20,22,24\n[Colors:Window]\nBackgroundNormal=32,35,38\n";
        let custom_dark = "[General]\nColorScheme=MyNight\n[Colors:Window]\nBackgroundNormal=46,30,60\nForegroundNormal=230,230,230\n";
        assert_eq!(kde_dark(breeze_light), Some(false));
        assert_eq!(kde_dark(breeze_dark), Some(true));
        assert_eq!(kde_dark(custom_dark), Some(true));
        assert_eq!(kde_dark("[General]\nColorScheme=X\n"), Some(false));
        assert_eq!(kde_dark("[General]\nColorScheme=BreezeDark\n"), Some(true));
        assert_eq!(kde_dark("[KDE]\nLookAndFeelPackage=x\n"), None);
        assert_eq!(portal_dark("v u 1"), Some(true));
        assert_eq!(portal_dark("v u 2"), Some(false));
        assert_eq!(portal_dark("v u 0"), None);
    }
}

/// A focused field or button: its border turns the primary colour, 1px, with
/// no ring outside it.
fn thin_focus(cx: &mut App) {
    Theme::global_mut(cx).focus_ring = false;
}
