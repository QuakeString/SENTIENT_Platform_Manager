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

/// Linux on OpenGL (no Vulkan driver): the window cannot be translucent, so
/// the menu is solid (and About says why). Every window with the menu
/// (the Platform Manager, Setup) calls it once it is open.
pub(crate) fn detect_gl(window: &mut Window) {
    if cfg!(target_os = "linux") {
        let gl = window.gpu_specs().is_some_and(|g| g.driver_info.contains("Core Profile") || g.driver_info.contains("OpenGL"));
        set_gl_only(gl);
        if gl {
            window.set_background_appearance(WindowBackgroundAppearance::Opaque);
        }
    }
}

/// The selected row's name: the primary colour; in dark lifted 40 % toward
/// white (see `shell_menu`). The Platform Manager's menu and Setup's steps.
pub(crate) fn selected_label(theme: &gpui_kit::component::Theme) -> Hsla {
    if theme.mode.is_dark() { theme.primary.blend(gpui_kit::white().opacity(0.4)) } else { theme.primary }
}

/// The menu's surface: solid, or translucent over the blurred desktop
/// (`see_through`); open over the page (`overlay`, its progress
/// `overlay_t`) nearly solid, to keep the names readable. The Platform
/// Manager's menu and Setup's steps.
pub(crate) fn menu_bg(theme: &gpui_kit::component::Theme, overlay: bool, overlay_t: f32) -> Hsla {
    if !see_through() {
        theme.sidebar
    } else if overlay {
        theme.sidebar.opacity(0.72 + 0.24 * overlay_t)
    } else {
        theme.sidebar.opacity(0.72)
    }
}

/// The menu's lines (its edge, the selected row's outline and curves, the
/// footer's and the separator's): the border token over the solid menu
/// surface, one opaque colour. The token has alpha, so drawn as it is it
/// came out lighter over the page-coloured selection than over the menu,
/// and different again over the see-through menu (his 0.9.0 dark-mode note).
pub(crate) fn menu_line(theme: &gpui_kit::component::Theme) -> Hsla {
    theme.sidebar.opacity(1.).blend(theme.sidebar_border)
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
/// The selected row's left end: a half circle (the pill he chose in
/// rc12-24); hovered rows take the same shape.
const SEL_RADIUS: f32 = ROW_H / 2.;
/// A row's height (the selection's too), and the gap between rows: a
/// 44 px pitch. Folded, unfolded and over the page alike.
pub const ROW_H: f32 = 40.;
pub(crate) const ROW_GAP: f32 = 4.;
/// Clear space between the brand row and the first row's selection.
pub(crate) const NAV_TOP: f32 = 16.;
/// The top bar's height, and the brand row's beside it (their lines meet).
pub const BAR_H: f32 = 60.;
/// Where every menu icon's centre is, folded or not: the middle of the
/// folded menu. Folding only hides the labels; nothing moves.
pub const ICON_X: f32 = 32.;
/// The menu's left padding (the selection's left end), the same folded or not.
pub(crate) const MENU_PAD: f32 = 12.;
/// Unfolded, the selection keeps a wider margin from the window's edge (his
/// D1 note: too little room on its left); the icons stay on ICON_X.
const MENU_PAD_OPEN: f32 = 12.;
/// A row's icon size and its left padding inside the row (icon centre at ICON_X).
pub(crate) const ICON: f32 = 18.;
pub(crate) fn row_pl(pad: f32) -> f32 {
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
pub(crate) fn row_probe(id: &'static str) -> impl IntoElement {
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

/// Layout tests: the row drawn selected, by its page id (`LAYOUT menu-on
/// <id> l r t b`; check.py holds it to the route's `LAYOUT-NAV selected=`).
fn selected_probe(id: &'static str) -> impl IntoElement {
    let on = cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some();
    div().absolute().inset_0().when(on, move |d| {
        d.child(
            canvas(
                move |b, _, _| {
                    let (l, r) = (f32::from(b.origin.x), f32::from(b.origin.x + b.size.width));
                    let (t, bt) = (f32::from(b.origin.y), f32::from(b.origin.y + b.size.height));
                    eprintln!("LAYOUT menu-on {id} {l:.0} {r:.0} {t:.0} {bt:.0}");
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    })
}

/// Notes the menu's edges.
/// Open over the page, the page's edge is the rail slot's (`slot`).
pub(crate) fn menu_probe(left_pad: f32, slot: Option<f32>) -> impl IntoElement {
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
pub(crate) fn selection(active: &'static str, page: Hsla, line: Hsla, motion: bool) -> impl IntoElement {
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
            // Everything is placed in device pixels (whole ones where an
            // edge must meet another), then given to GPUI in logical units:
            // GPUI snaps quad edges to the nearest device pixel and a
            // shadow's to the outer one, so each value is nudged a quarter
            // pixel to land where it is meant to.
            let sc = window.scale_factor();
            let lg = |d: f32| px(d / sc);
            // For edges GPUI floors (left, top) or ceils (right, bottom).
            let lo = |d: f32| px((d + 0.25) / sc);
            let hi = |d: f32| px((d - 0.25) / sc);
            let e = (right * sc).round();
            let w = sc.round().max(1.);
            let (y0, y1) = ((top_y * sc).round(), (bottom_y * sc).round());
            let dev = |x0: f32, t0: f32, x1: f32, t1: f32| Bounds::from_corners(point(lg(x0), lg(t0)), point(lg(x1), lg(t1)));
            let Some(top) = y else {
                // No row selected (About): the plain edge.
                window.paint_quad(gpui::fill(dev(e - w, y0, e, y1), line));
                return;
            };
            // The row's outline: its top line on [t, t + w], its bottom line
            // on [b - w, b], its left end a half circle; the outline's centre
            // line runs at SEL_RADIUS (in device pixels `rd`) round the end.
            let hd = (ROW_H * sc).round();
            let t = (top * sc - w / 2.).round();
            let b = t + hd + w;
            let l0 = (left * sc - w / 2.).round();
            let rd = hd / 2.;
            // The layout test checks the opening (tests/layout/check.py).
            if cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some() {
                eprintln!("LAYOUT menu-sel {:.2} {:.2} {:.2} {:.2}", left, right, top, top + ROW_H);
            }
            // The concave curves join the row to the edge with the row's
            // own radius (his 0.9.0 note: they looked deeper than the
            // pill). Centre-line circle centres: above the row's top line
            // and below its bottom line, `rd` left of the edge line.
            let cx = e - w / 2. - rd;
            let (cy_top, cy_bot) = (t + w / 2. - rd, b - w / 2. + rd);
            // The edge above and below the opening (ending where its
            // curve leaves it).
            window.paint_quad(gpui::fill(dev(e - w, y0, e, cy_top.ceil()), line));
            window.paint_quad(gpui::fill(dev(e - w, cy_bot.floor(), e, y1), line));
            // The row: page-coloured from its left end out to the edge, its
            // outline on the top, left and bottom. GPUI antialiases a
            // quad's rounded corners exactly at every scale, where paths
            // get at most 4x MSAA on Linux (his rc12-24 note).
            let end = (b - t) / 2.;
            window.paint_quad(gpui::quad(
                dev(l0, t, e, b),
                Corners { top_left: lg(end), bottom_left: lg(end), top_right: px(0.), bottom_right: px(0.) },
                page,
                Edges { top: lg(w), left: lg(w), bottom: lg(w), right: px(0.) },
                line,
                BorderStyle::Solid,
            ));
            // The concave curves, each as two inset shadows over its corner
            // square: an inset shadow paints the complement of a rounded
            // "hole" inside its element, its curved edge antialiased by the
            // same distance field as a quad's corner, with no content mask
            // (GPUI's masks are hard-edged). The hole is a circle about the
            // curve's centre: first the line colour outside r - w/2, then the
            // page colour outside r + w/2, leaving the line between. Inside
            // the curve nothing is painted, so the menu shows through as it
            // is (translucent when see-through). Only the hole's corner that
            // faces the element matters; its other edges lie far outside.
            // `corner` is that corner's device position and `up` whether
            // the hole lies above it (the top curve) or below (the bottom).
            let mut curve = |el: Bounds<Pixels>, corner: (f32, f32), up: bool, r: f32, color: Hsla| {
                // The hole: the element moved by `offset` and grown by r on
                // every side (spread -r), all its corners of radius r.
                let (ex1, ey0, ey1) = (f32::from(el.origin.x + el.size.width), f32::from(el.origin.y), f32::from(el.origin.y + el.size.height));
                let hx = f32::from(hi(corner.0)) - r / sc - ex1;
                let hy = if up { f32::from(hi(corner.1)) - r / sc - ey1 } else { f32::from(lo(corner.1)) + r / sc - ey0 };
                let shadow = BoxShadow {
                    color,
                    offset: point(px(hx), px(hy)),
                    blur_radius: px(0.),
                    spread_radius: px(-r / sc),
                    inset: true,
                };
                window.paint_inset_shadows(el, Corners::default(), &[shadow]);
            };
            let top_sq = Bounds::from_corners(point(lo(cx.floor()), lo(cy_top.floor())), point(hi(e), hi(t + w)));
            let bot_sq = Bounds::from_corners(point(lo(cx.floor()), lo(b - w)), point(hi(e), hi(cy_bot.ceil())));
            curve(top_sq, (e - w, t), true, rd - w / 2., line);
            curve(bot_sq, (e - w, b), false, rd - w / 2., line);
            curve(top_sq, (e, t + w), true, rd + w / 2., page);
            curve(bot_sq, (e, b - w), false, rd + w / 2., page);
        },
    )
    .absolute()
    .inset_0()
}

// ---------------------------------------------------------------------------
// The menu
// ---------------------------------------------------------------------------

/// The product's name beside its logo on top of the menu: "SENTIENT" and
/// the app under it (the Platform Manager's menu, Setup's steps).
pub(crate) fn brand_name(theme: &gpui_kit::component::Theme, app: impl Into<SharedString>) -> Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .line_height(px(17.))
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("SENTIENT"))
        .child(div().text_size(px(10.)).font_weight(FontWeight::SEMIBOLD).text_color(theme.primary).whitespace_nowrap().text_ellipsis().overflow_hidden().child(app.into()))
}

/// The brand logo, the footer's logo and the preferences button.
pub(crate) const LOGO: f32 = 32.;
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
        // The page whose row is selected: a sub-flow's parent while it
        // shows (Manager::menu_page).
        let current = self.menu_page();
        let active: &'static str = current.id();
        let left_pad = if rail { MENU_PAD } else { MENU_PAD_OPEN };
        // The selected row's name: the primary colour; in dark lifted 40 %
        // toward white. Teal on the dark selection was 5:1 against 9.6:1
        // for the other names, and its fainter edges made it read a size
        // smaller (his rc12-22 note); lifted it is 7.9:1. The icon keeps
        // the primary colour.
        let sel_label = selected_label(&theme);
        // The names (text_sm, 0.875 rem) on whole device pixels: at ×1.25
        // they fell between pixels, and the hinting then drew the semibold
        // selected name 1-2 px taller than the medium ones (rc12-23 gate,
        // labels.py).
        let sf = window.scale_factor();
        let name_size = px((0.875 * f32::from(window.rem_size()) * sf).round() / sf);
        let row = |id: &'static str, icon: Icon, title: SharedString, on: bool| {
            h_flex()
                .id(id)
                .relative()
                .h(px(ROW_H))
                .flex_shrink_0()
                .gap(px(12.))
                .pl(px(row_pl(left_pad)))
                .when(!rail, |r| r.pr(px(12.)).mr(px(10.)))
                // The pill, like the selection's left end.
                .rounded(px(SEL_RADIUS))
                .cursor_pointer()
                .text_size(name_size)
                .text_color(if on { sel_label } else { theme.foreground })
                .font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
                .child(row_probe(id))
                .child(ui::layout_probe("menu-row"))
                .when(on, |r| r.child(selected_probe(id)))
                .child(div().relative().flex_shrink_0().child(icon.size(px(ICON)).text_color(if on { theme.primary } else { theme.muted_foreground })).child(ui::layout_probe("menu-icon")))
                .when(!rail, |r| r.child(div().relative().whitespace_nowrap().opacity(names).child(title.clone()).child(ui::layout_probe(if on { "menu-label-on" } else { "menu-label" }))))
                .when(rail, move |r| r.tooltip(crate::ui::tooltip(title.clone())))
        };
        let items = Page::menu(self.open_gateway_only()).into_iter().map(|page| {
            row(page.id(), page.icon(), page.title().into(), page == current)
                // Files is new in this release (the mockup's pill).
                .when(page == Page::Files && !rail, |r| {
                    r.child(div().flex_1()).child(div().px(px(7.)).rounded_full().bg(theme.muted).text_size(px(11.)).text_color(theme.muted_foreground).child("New"))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                // Platform settings with changes not applied asks first
                // (not on its own row).
                if page == this.page {
                    this.menu_overlay = false;
                    cx.notify();
                    return;
                }
                this.leave_then(window, cx, move |this, cx| {
                    this.page = page;
                    this.menu_overlay = false;
                    cx.notify();
                });
            }))
        });
        let instances = row("instances", Page::Instances.icon(), Page::Instances.title().into(), current == Page::Instances)
            .when(!self.remotes.is_empty() && !rail, |r| {
                r.child(div().flex_1()).child(div().relative().text_size(px(13.)).text_color(theme.muted_foreground).child((self.remotes.len() + 1).to_string()))
            })
            .on_click(cx.listener(|this, _, window, cx| {
                this.leave_then(window, cx, |this, cx| {
                    this.page = Page::Instances;
                    this.menu_overlay = false;
                    cx.notify();
                });
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
                        // In front of the window's drag area (crate::win_caption).
                        .when(crate::win_caption::enabled(), |d| d.occlude())
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
                        .tooltip(crate::ui::tooltip("Unfold menu"))
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
                // Windows: the brand row drags the window too (crate::win_caption).
                .child(crate::win_caption::drag_layer())
                // The logo's centre on ICON_X, folded or not.
                .pl(px(ICON_X - LOGO / 2.))
                .when(!rail, |r| r.pr(px(10.)))
                // No line under it (his rc12-20 note): the menu starts below.
                .child(logo)
                .when(!rail, |r| {
                    r.child(brand_name(&theme, "PLATFORM MANAGER"))
                    .child(
                        div()
                            .id("fold")
                            .when(crate::win_caption::enabled(), |d| d.occlude())
                            .size(px(crate::theme::ICON_BUTTON))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.list_hover))
                            .child(menu_icon("panel-left-close").size(px(ICON)))
                            .tooltip(crate::ui::tooltip("Fold menu"))
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
            let on = current == Page::About;
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
                .tooltip(crate::ui::tooltip("About and preferences"))
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
                .border_color(menu_line(&theme))
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
        let menu_bg = menu_bg(&theme, overlay, overlay_t);
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
            .child(selection(active, theme.muted, menu_line(&theme), motion))
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
                    .child(div().mt(px(12.)).mb(px(12.)).ml(px(2.)).mr(px(10.)).h(px(1.)).flex_shrink_0().bg(menu_line(&theme)))
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
