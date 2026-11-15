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
        Choice::System => Theme::sync_system_appearance(window, cx),
    }
}
