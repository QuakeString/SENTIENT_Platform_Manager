// Copyright © 2016-2026 The SENTIENT Authors

//! The window's shell (the approved shell-layout mockup): the full-height
//! menu with the product on top and its fold button, the selected row that
//! joins the page (D1) and slides between rows, and the footer with the
//! version and the preferences button. The bar over the page is
//! `Manager::top_bar`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::*;

// ---------------------------------------------------------------------------
// Appearance preferences: the folded menu, the translucent menu and bar
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Appearance {
    /// The menu folded to its icons by choice (a narrow window folds it
    /// anyway).
    #[serde(default)]
    pub folded: bool,
    /// The menu shows the desktop behind the window, blurred, where the
    /// desktop can (`see_through_supported`).
    #[serde(default = "yes")]
    pub see_through: bool,
    /// The Platform Manager's language (a code from LANGUAGES).
    #[serde(default = "english")]
    pub language: String,
}

fn english() -> String {
    "en_US".into()
}

/// The languages the Platform Manager can show: code and the language's
/// own name. Only English is translated so far; SENTIENT's set (ui-ngx
/// locales) is added here as data when its translations come.
pub const LANGUAGES: &[(&str, &str)] = &[("en_US", "English")];

fn yes() -> bool {
    true
}

impl Default for Appearance {
    fn default() -> Self {
        Self { folded: false, see_through: true, language: english() }
    }
}

fn appearance_path() -> std::path::PathBuf {
    crate::updates::prefs_dir().join("appearance.json")
}

thread_local! {
    static APPEARANCE: RefCell<Option<Appearance>> = const { RefCell::new(None) };
}

impl Appearance {
    pub fn get() -> Appearance {
        APPEARANCE.with(|a| {
            a.borrow_mut()
                .get_or_insert_with(|| std::fs::read_to_string(appearance_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
                .clone()
        })
    }

    pub fn set(f: impl FnOnce(&mut Appearance)) {
        let mut a = Appearance::get();
        f(&mut a);
        if let Ok(t) = serde_json::to_string_pretty(&a) {
            let _ = std::fs::create_dir_all(crate::updates::prefs_dir());
            let _ = std::fs::write(appearance_path(), t);
        }
        APPEARANCE.with(|s| *s.borrow_mut() = Some(a));
    }
}

/// Where the translucent menu cannot work, why (None: it can). Windows blurs behind
/// any window (acrylic); KDE Plasma does on Wayland (ext-background-effect,
/// or its own blur protocol) and on X11 (its blur property); Hyprland blurs
/// every translucent window by itself, and picom when it runs (with blur in
/// its settings). GNOME and the rest have no blur: solid. On Linux the
/// window also needs Vulkan graphics: on OpenGL it cannot be translucent.
pub fn see_through_why_not() -> Option<&'static str> {
    if cfg!(windows) {
        return None;
    }
    if GL_ONLY.with(|g| g.get()) {
        return Some("The menu cannot be translucent without Vulkan graphics: install your graphics card's Vulkan driver (on Arch vulkan-intel, vulkan-radeon or nvidia-utils; on Debian and Ubuntu mesa-vulkan-drivers), then start the Platform Manager again.");
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    let blurs = desktop.contains("KDE") || desktop.contains("HYPRLAND") || picom_runs();
    if blurs { None } else { Some("Needs a desktop that blurs what is behind a window: KDE Plasma, Hyprland, picom, or Windows. Here the menu is solid.") }
}

pub fn see_through_supported() -> bool {
    see_through_why_not().is_none()
}

/// picom (an X11 compositor that can blur) is running.
fn picom_runs() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        std::env::var_os("WAYLAND_DISPLAY").is_none()
            && std::fs::read_dir("/proc").is_ok_and(|d| d.flatten().any(|e| std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "picom")))
    })
}

thread_local! {
    /// The window draws with OpenGL (no Vulkan driver): it cannot be
    /// translucent (crate::manager::Manager::new sets it).
    static GL_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set_gl_only(gl: bool) {
    GL_ONLY.with(|g| g.set(gl));
}

/// The translucent menu on: chosen and possible.
pub fn see_through() -> bool {
    see_through_supported() && Appearance::get().see_through
}

// ---------------------------------------------------------------------------
// The menu's selection: where each row is, and the slide between rows
// ---------------------------------------------------------------------------

/// One full slide of the selection between rows.
const SLIDE: Duration = Duration::from_millis(220);
/// The concave curves above and below the selected row.
const FILLET: f32 = 14.;
/// The selected row's own corner radius (its left end).
const SEL_RADIUS: f32 = 10.;
/// A row's height (the selection's too), and the gap between rows: a
/// 44 px pitch. Folded, unfolded and over the page alike.
pub const ROW_H: f32 = 40.;
const ROW_GAP: f32 = 4.;
/// Clear space between the brand row and the first row's selection.
const NAV_TOP: f32 = 16.;
/// The top bar's height, and the brand row's beside it (their lines meet).
pub const BAR_H: f32 = 60.;
/// Where every menu icon's centre is, folded or not: the middle of the
/// folded menu. Folding only hides the labels; nothing moves.
pub const ICON_X: f32 = 32.;
/// The menu's left padding (the selection's left end), the same folded or not.
const MENU_PAD: f32 = 12.;
/// Unfolded, the selection keeps a wider margin from the window's edge (his
/// D1 note: too little room on its left); the icons stay on ICON_X.
const MENU_PAD_OPEN: f32 = 12.;
/// A row's icon size and its left padding inside the row (icon centre at ICON_X).
const ICON: f32 = 18.;
fn row_pl(pad: f32) -> f32 {
    ICON_X - pad - ICON / 2.
}

/// A menu icon from the heavier set (assets/icons/menu).
pub fn menu_icon(name: &str) -> Icon {
    Icon::empty().path(SharedString::from(format!("icons/menu/{name}.svg")))
}

#[derive(Default)]
struct Rows {
    /// Each row's top in window coordinates (from its probe).
    top: HashMap<&'static str, f32>,
    /// The menu's right edge (its border included) and its left content edge.
    right: f32,
    left: f32,
    /// The menu's top and bottom (the edge line runs between them).
    top_y: f32,
    bottom_y: f32,
    /// The slide: from, to, since; and the row it is for.
    from: f32,
    to: f32,
    since: Option<Instant>,
    row: Option<&'static str>,
}

thread_local! {
    static ROWS: RefCell<Rows> = RefCell::new(Rows::default());
}

/// An invisible child of a row that notes where the row is.
fn row_probe(id: &'static str) -> impl IntoElement {
    canvas(
        move |b, _, _| {
            ROWS.with(|r| {
                r.borrow_mut().top.insert(id, f32::from(b.origin.y));
            })
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// Notes the menu's edges.
/// Open over the page, the page's edge is the rail slot's (`slot`).
fn menu_probe(left_pad: f32, slot: Option<f32>) -> impl IntoElement {
    canvas(
        move |b, _, _| {
            ROWS.with(|r| {
                let mut r = r.borrow_mut();
                r.right = f32::from(b.origin.x + b.size.width);
                r.left = f32::from(b.origin.x) + left_pad;
                r.top_y = f32::from(b.origin.y);
                r.bottom_y = f32::from(b.origin.y + b.size.height);
            });
            // The layout test measures the page's gaps from here.
            if cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some() {
                eprintln!("LAYOUT menu-edge {:.2}", slot.unwrap_or(f32::from(b.origin.x + b.size.width)));
                if slot.is_some() {
                    eprintln!("LAYOUT menu-overlay {:.2}", f32::from(b.origin.x + b.size.width));
                }
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

/// The menu's right edge and the selected row joined to the page, drawn as
/// ONE shape in device pixels (his rc12-19 HiDPI crops: a border plus an
/// overlay never met at fractional scales). The edge is a line one device
/// pixel (rounded scale) wide on the menu's last pixels; at the selected
/// row it opens: a concave curve leaves the line tangent, runs along the
/// row's top, round its left end and bottom, and back into the line. Inside
/// the opening the page's colour, so the row and the page are one. It
/// slides to a newly selected row.
fn selection(active: &'static str, page: Hsla, line: Hsla, motion: bool) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let (y, moving, left, right, top_y, bottom_y) = ROWS.with(|r| {
                let mut r = r.borrow_mut();
                let (left, right, top_y, bottom_y) = (r.left, r.right, r.top_y, r.bottom_y);
                let Some(target) = r.top.get(active).copied() else { return (None, false, left, right, top_y, bottom_y) };
                let now = Instant::now();
                let current = match r.since {
                    Some(s) => r.from + (r.to - r.from) * ease_out(now.duration_since(s).as_secs_f32() / SLIDE.as_secs_f32()),
                    None => r.to,
                };
                if r.row != Some(active) {
                    // Another row: slide there (or jump, the first time or
                    // with motion off).
                    let first = r.row.is_none();
                    r.from = if first { target } else { current };
                    r.to = target;
                    r.since = (!first && motion).then_some(now);
                    r.row = Some(active);
                } else if (r.to - target).abs() > 0.5 && r.since.is_none() {
                    // The same row moved (a resize, the menu scrolled).
                    r.from = target;
                    r.to = target;
                }
                let y = match r.since {
                    Some(s) => {
                        let t = now.duration_since(s).as_secs_f32() / SLIDE.as_secs_f32();
                        if t >= 1. {
                            r.since = None;
                            r.to
                        } else {
                            r.from + (r.to - r.from) * ease_out(t)
                        }
                    }
                    None => r.to,
                };
                (Some(y), r.since.is_some(), left, right, top_y, bottom_y)
            });
            if moving {
                window.request_animation_frame();
            }
            if right <= 0. {
                return;
            }
            // Device pixels: the edge on whole pixels, the line a whole
            // number of them wide; everything then in logical units again.
            let sc = window.scale_factor();
            let edge_dev = (right * sc).round();
            let w_dev = sc.round().max(1.);
            let edge = edge_dev / sc;
            let lw = w_dev / sc;
            let xc = (edge_dev - w_dev / 2.) / sc;
            let p = |x: f32, y: f32| point(px(x), px(y));
            let Some(top) = y else {
                // No row selected (About): the plain edge.
                let mut stroke = PathBuilder::stroke(px(lw));
                stroke.move_to(p(xc, top_y));
                stroke.line_to(p(xc, bottom_y));
                if let Ok(path) = stroke.build() {
                    window.paint_path(path, line);
                }
                return;
            };
            let (l, t, b) = (left, top, top + ROW_H);
            // The layout test checks the opening (tests/layout/check.py).
            if cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some() {
                eprintln!("LAYOUT menu-sel {:.2} {:.2} {:.2} {:.2}", l, edge, t, b);
            }
            let f = FILLET;
            let rad = SEL_RADIUS;
            // The page-coloured opening: out to the menu's edge (over the
            // line there), the curves tangent to it.
            let mut fill = PathBuilder::fill();
            fill.move_to(p(edge, t - f));
            fill.line_to(p(xc, t - f));
            fill.arc_to(p(f, f), px(0.), false, true, p(xc - f, t));
            fill.line_to(p(l + rad, t));
            fill.arc_to(p(rad, rad), px(0.), false, false, p(l, t + rad));
            fill.line_to(p(l, b - rad));
            fill.arc_to(p(rad, rad), px(0.), false, false, p(l + rad, b));
            fill.line_to(p(xc - f, b));
            fill.arc_to(p(f, f), px(0.), false, true, p(xc, b + f));
            fill.line_to(p(edge, b + f));
            fill.close();
            if let Ok(path) = fill.build() {
                window.paint_path(path, page);
            }
            // The edge and the opening's outline: one line, top to bottom.
            let h = lw / 2.;
            let mut stroke = PathBuilder::stroke(px(lw));
            stroke.move_to(p(xc, top_y));
            stroke.line_to(p(xc, t - f));
            stroke.arc_to(p(f - h, f - h), px(0.), false, true, p(xc - f + h, t + h));
            stroke.line_to(p(l + rad, t + h));
            stroke.arc_to(p(rad - h, rad - h), px(0.), false, false, p(l + h, t + rad));
            stroke.line_to(p(l + h, b - rad));
            stroke.arc_to(p(rad - h, rad - h), px(0.), false, false, p(l + rad, b - h));
            stroke.line_to(p(xc - f + h, b - h));
            stroke.arc_to(p(f - h, f - h), px(0.), false, true, p(xc, b + f));
            stroke.line_to(p(xc, bottom_y));
            if let Ok(path) = stroke.build() {
                window.paint_path(path, line);
            }
        },
    )
    .absolute()
    .inset_0()
}

// ---------------------------------------------------------------------------
// The menu
// ---------------------------------------------------------------------------

/// The brand logo, the footer's logo and the preferences button.
const LOGO: f32 = 32.;
const FOOT_LOGO: f32 = 26.;
const PREFS: f32 = 32.;

/// The menu's width, unfolded and folded.
pub const MENU_W: f32 = 232.;
pub const MENU_FOLDED_W: f32 = 64.;
/// Below this window width the menu folds by itself (its unfold button
/// then opens it over the page instead).
pub const FOLD_BELOW: f32 = 1000.;

impl Manager {
    /// Folded: by choice, or because the window is narrow.
    pub(super) fn folded(width: f32) -> bool {
        width < FOLD_BELOW || Appearance::get().folded
    }

    pub(super) fn shell_menu(&self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        // Folding glides the width (the names fade with it); the layout is
        // the folded one once the menu is narrow.
        let forced = width < FOLD_BELOW;
        let target_w = if forced && self.menu_overlay { MENU_W } else if Self::folded(width) { MENU_FOLDED_W } else { MENU_W };
        let menu_w = ui::tween("menu-w", target_w, 200, window);
        let rail = menu_w < 150.;
        let names = ((menu_w - 150.) / (MENU_W - 150.)).clamp(0., 1.);
        // Narrow window: the menu opens over the page (and stays over it
        // while it folds back), a rail-wide slot keeping the page still.
        let overlay = forced && (self.menu_overlay || menu_w > MENU_FOLDED_W + 0.5);
        let overlay_t = if overlay { ((menu_w - MENU_FOLDED_W) / (MENU_W - MENU_FOLDED_W)).clamp(0., 1.) } else { 0. };
        self.overlay_t.set(overlay_t);
        let motion = !ui::reduce_motion();
        let active: &'static str = if self.page == Page::Instances { "instances" } else if self.page == Page::About { "about" } else { self.page.id() };
        let left_pad = if rail { MENU_PAD } else { MENU_PAD_OPEN };
        // The selected row's name: the primary colour; in dark lifted 40 %
        // toward white. Teal on the dark selection was 5:1 against 9.6:1
        // for the other names, and its fainter edges made it read a size
        // smaller (his rc12-22 note); lifted it is 7.9:1. The icon keeps
        // the primary colour.
        let sel_label: Hsla = if theme.mode.is_dark() { theme.primary.blend(gpui_kit::white().opacity(0.4)) } else { theme.primary };
        let row = |id: &'static str, icon: Icon, title: SharedString, on: bool| {
            h_flex()
                .id(id)
                .relative()
                .h(px(ROW_H))
                .flex_shrink_0()
                .gap(px(12.))
                .pl(px(row_pl(left_pad)))
                .when(!rail, |r| r.pr(px(12.)).mr(px(10.)))
                .rounded(px(8.))
                .cursor_pointer()
                .text_sm()
                .text_color(if on { sel_label } else { theme.foreground })
                .font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
                .child(row_probe(id))
                .child(ui::layout_probe("menu-row"))
                .child(div().relative().flex_shrink_0().child(icon.size(px(ICON)).text_color(if on { theme.primary } else { theme.muted_foreground })).child(ui::layout_probe("menu-icon")))
                .when(!rail, |r| r.child(div().relative().whitespace_nowrap().opacity(names).child(title.clone()).child(ui::layout_probe(if on { "menu-label-on" } else { "menu-label" }))))
                .when(rail, move |r| r.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(title.clone()).build(w, cx)))
        };
        let items = Page::ALL.into_iter().map(|page| {
            row(page.id(), page.icon(), page.title().into(), page == self.page)
                // Files is new in this release (the mockup's pill).
                .when(page == Page::Files && !rail, |r| {
                    r.child(div().flex_1()).child(div().px(px(7.)).rounded_full().bg(theme.muted).text_size(px(11.)).text_color(theme.muted_foreground).child("New"))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                this.page = page;
                this.menu_overlay = false;
                cx.notify();
            }))
        });
        let instances = row("instances", Page::Instances.icon(), Page::Instances.title().into(), self.page == Page::Instances)
            .when(!self.remotes.is_empty() && !rail, |r| {
                r.child(div().flex_1()).child(div().relative().text_size(px(13.)).text_color(theme.muted_foreground).child((self.remotes.len() + 1).to_string()))
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.page = Page::Instances;
                this.menu_overlay = false;
                cx.notify();
            }));

        // The product on top: its logo and name, and the fold button; folded,
        // the logo turns into the unfold button on hover.
        let brand = {
            let logo_hover = self.logo_hover;
            let logo = div()
                .id("brand-logo")
                .relative()
                .size(px(LOGO))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .child(ui::layout_probe("menu-icon"))
                .when(rail, |d| {
                    d.cursor_pointer()
                        .on_hover(cx.listener(|this, h: &bool, _, cx| {
                            this.logo_hover = *h;
                            cx.notify();
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if forced {
                                // Narrow: open it over the page; the
                                // preference for wide windows stays.
                                this.menu_overlay = true;
                                window.focus(&this.modal_focus, cx);
                            } else {
                                Appearance::set(|a| a.folded = false);
                            }
                            this.logo_hover = false;
                            cx.notify();
                        }))
                        .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Unfold menu").build(w, cx))
                })
                // The swap cross-fades (150 ms, the app's ease-out): the logo
                // fades and shrinks a little as the unfold button fades in,
                // and back on leaving. Reduced motion: at once.
                .child({
                    let t = ui::tween("logo-swap", if rail && logo_hover { 1. } else { 0. }, 150, window);
                    let logo_size = LOGO * (1. - 0.08 * t);
                    div()
                        .relative()
                        .size_full()
                        .child(div().absolute().inset_0().flex().items_center().justify_center().opacity(1. - t).child(ui::product_logo(ui::Product::Manager, logo_size)))
                        .when(t > 0.001, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.list_hover)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .opacity(t)
                                    .child(menu_icon("panel-left-open").size(px(ICON)).text_color(theme.muted_foreground)),
                            )
                        })
                });
            h_flex()
                .relative()
                .h(px(BAR_H))
                .flex_shrink_0()
                .items_center()
                .gap(px(10.))
                .child(ui::layout_probe("menu-brand"))
                // The logo's centre on ICON_X, folded or not.
                .pl(px(ICON_X - LOGO / 2.))
                .when(!rail, |r| r.pr(px(10.)))
                // No line under it (his rc12-20 note): the menu starts below.
                .child(logo)
                .when(!rail, |r| {
                    r.child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .line_height(px(17.))
                            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("SENTIENT"))
                            .child(div().text_size(px(10.)).font_weight(FontWeight::SEMIBOLD).text_color(theme.primary).whitespace_nowrap().child("PLATFORM MANAGER")),
                    )
                    .child(
                        div()
                            .id("fold")
                            .size(px(32.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.list_hover))
                            .child(menu_icon("panel-left-close").size(px(ICON)))
                            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Fold menu").build(w, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if forced {
                                    this.menu_overlay = false;
                                } else {
                                    Appearance::set(|a| a.folded = true);
                                }
                                cx.notify();
                            })),
                    )
                })
        };

        // The footer: the Platform Manager, its version and an Update pill
        // when one is waiting; the preferences button (About and
        // preferences).
        let footer = {
            let on = self.page == Page::About;
            let update = self.updates.read(cx).notices().1.is_some();
            let prefs = div()
                .id("prefs")
                .relative()
                .size(px(PREFS))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                // Folded it sits on ICON_X; unfolded it is at the right end.
                .when(rail, |d| d.child(ui::layout_probe("menu-icon")))
                .border_1()
                .border_color(if on { theme.primary } else { theme.border })
                .bg(if on { theme.sidebar_accent } else { theme.background })
                .text_color(if on { theme.primary } else { theme.foreground })
                .cursor_pointer()
                .when(!on, |d| d.hover(|d| d.bg(theme.list_hover)))
                .child(menu_icon("sliders-horizontal").size(px(ICON)))
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("About and preferences").build(w, cx))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.page = Page::About;
                    this.menu_overlay = false;
                    cx.notify();
                }));
            h_flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(4.))
                .border_t_1()
                .border_color(theme.sidebar_border)
                // Folded: the preferences button on ICON_X; unfolded: the
                // product logo there.
                .when(rail, |r| r.flex_col().items_start().py(px(8.)).pl(px(ICON_X - PREFS / 2.)))
                .when(!rail, |r| r.py(px(9.)).pl(px(ICON_X - FOOT_LOGO / 2.)).pr(px(10.)))
                .when(!rail, |r| {
                    r.child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(8.))
                            .items_center()
                            .child(div().relative().child(ui::product_logo(ui::Product::Manager, FOOT_LOGO)).child(ui::layout_probe("menu-icon")))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .line_height(px(16.))
                                    .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).whitespace_nowrap().child("Platform Manager"))
                                    .child(
                                        h_flex()
                                            .gap(px(6.))
                                            .items_center()
                                            .text_size(px(12.))
                                            .text_color(theme.muted_foreground)
                                            .child(crate::updates::manager_version().to_string())
                                            .when(update, |r| {
                                                r.child(
                                                    div()
                                                        .id("footer-update")
                                                        .px(px(6.))
                                                        .rounded(px(8.))
                                                        .bg(theme.primary.opacity(0.10))
                                                        .text_color(theme.primary)
                                                        .text_size(px(11.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .cursor_pointer()
                                                        .child("Update")
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.page = Page::Update;
                                                            this.menu_overlay = false;
                                                            cx.notify();
                                                        })),
                                                )
                                            }),
                                    ),
                            ),
                    )
                })
                .child(prefs)
        };

        // Over the page the window's blur has only the page under it (GPUI
        // cannot blur inside the window), so the open overlay is nearly
        // solid to keep the names readable.
        let menu_bg = if !see_through() {
            theme.sidebar
        } else if overlay {
            theme.sidebar.opacity(0.72 + 0.24 * overlay_t)
        } else {
            theme.sidebar.opacity(0.72)
        };
        let dark = theme.mode.is_dark();
        let menu = v_flex()
            .w(px(menu_w))
            .overflow_hidden()
            .h_full()
            .flex_shrink_0()
            .relative()
            .bg(menu_bg)
            // No border: the selection canvas draws the edge (one shape
            // with the selected row's opening).
            .child(menu_probe(left_pad, overlay.then_some(MENU_FOLDED_W)))
            // Under the rows: the selection, joined to the page.
            .child(selection(active, theme.muted, theme.sidebar_border, motion))
            .child(brand)
            .child(
                // The rows scroll when the window is short; the selection is
                // drawn under them, out to the menu's edge.
                v_flex()
                    .id("nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(px(ROW_GAP))
                    .pt(px(NAV_TOP))
                    .pl(px(left_pad))
                    .when(rail, |m| m.pr(px(MENU_PAD)))
                    .children(items)
                    .child(div().mt(px(12.)).mb(px(12.)).ml(px(2.)).mr(px(10.)).h(px(1.)).flex_shrink_0().bg(theme.sidebar_border))
                    .child(instances),
            )
            .child(footer);
        if !overlay {
            return menu.into_any_element();
        }
        // Over the page: a soft shadow on its right edge; the slot keeps the
        // rail's width so nothing beside it moves.
        let shadow = BoxShadow {
            color: black().opacity(overlay_t * if dark { 0.45 } else { 0.18 }),
            offset: point(px(4.), px(0.)),
            blur_radius: px(18.),
            spread_radius: px(0.),
            inset: false,
        };
        div()
            .relative()
            .h_full()
            .w(px(MENU_FOLDED_W))
            .flex_shrink_0()
            .child(menu.absolute().top_0().left_0().shadow(vec![shadow]))
            .into_any_element()
    }
}

/// The theme icons: sun (Light), moon (Dark), half-filled circle (Follow
/// system).
pub fn theme_icon(c: theme::Choice) -> Icon {
    match c {
        theme::Choice::Light => Icon::new(IconName::Sun),
        theme::Choice::Dark => Icon::new(IconName::Moon),
        theme::Choice::System => Icon::new(Lucide::Contrast),
    }
}
