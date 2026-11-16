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
// Appearance preferences: the folded menu, the see-through menu and bar
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

/// Whether the desktop blurs what is behind a see-through window: Windows,
/// and KDE Plasma on Wayland (its blur protocol). Elsewhere the menu is
/// solid and the preference says why.
pub fn see_through_supported() -> bool {
    if cfg!(windows) {
        return true;
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let kde = std::env::var("XDG_CURRENT_DESKTOP").map(|d| d.to_uppercase().contains("KDE")).unwrap_or(false);
    wayland && kde
}

/// See-through on: chosen and possible.
pub fn see_through() -> bool {
    see_through_supported() && Appearance::get().see_through
}

/// Why see-through cannot be used here, for the preference's (i).
pub const SEE_THROUGH_WHY: &str = "Needs a desktop that blurs what is behind a window: KDE Plasma (Wayland) or Windows. Here the menu is solid.";

// ---------------------------------------------------------------------------
// The menu's selection: where each row is, and the slide between rows
// ---------------------------------------------------------------------------

/// One full slide of the selection between rows.
const SLIDE: Duration = Duration::from_millis(220);
/// The concave curves above and below the selected row.
const FILLET: f32 = 14.;
/// The selected row's own corner radius (its left end).
const SEL_RADIUS: f32 = 10.;
/// A row's height.
pub const ROW_H: f32 = 38.;

#[derive(Default)]
struct Rows {
    /// Each row's top in window coordinates (from its probe).
    top: HashMap<&'static str, f32>,
    /// The menu's right edge (its border included) and its left content edge.
    right: f32,
    left: f32,
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
fn menu_probe(left_pad: f32) -> impl IntoElement {
    canvas(
        move |b, _, _| {
            ROWS.with(|r| {
                let mut r = r.borrow_mut();
                r.right = f32::from(b.origin.x + b.size.width);
                r.left = f32::from(b.origin.x) + left_pad;
            })
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

/// The selected row joined to the page: the page's colour from the row's
/// rounded left end to the menu's edge, with concave curves above and
/// below; its outline runs unbroken from the menu's edge, along the curve,
/// the row's top, left and bottom, and the lower curve back to the edge.
/// It slides to a newly selected row.
fn selection(active: &'static str, page: Hsla, line: Hsla, motion: bool) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let (y, moving, left, right) = ROWS.with(|r| {
                let mut r = r.borrow_mut();
                let Some(target) = r.top.get(active).copied() else { return (None, false, 0., 0.) };
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
                (Some(y), r.since.is_some(), r.left, r.right)
            });
            let Some(top) = y else { return };
            if moving {
                window.request_animation_frame();
            }
            let (l, r, t, b) = (left, right, top, top + ROW_H);
            let f = FILLET;
            let rad = SEL_RADIUS;
            let p = |x: f32, y: f32| point(px(x), px(y));
            // The page-coloured shape.
            let mut fill = PathBuilder::fill();
            fill.move_to(p(r, t - f));
            fill.arc_to(p(f, f), px(0.), false, true, p(r - f, t));
            fill.line_to(p(l + rad, t));
            fill.arc_to(p(rad, rad), px(0.), false, false, p(l, t + rad));
            fill.line_to(p(l, b - rad));
            fill.arc_to(p(rad, rad), px(0.), false, false, p(l + rad, b));
            fill.line_to(p(r - f, b));
            fill.arc_to(p(f, f), px(0.), false, true, p(r, b + f));
            fill.close();
            if let Ok(path) = fill.build() {
                window.paint_path(path, page);
            }
            // Its outline: the same path without the edge it joins.
            let mut stroke = PathBuilder::stroke(px(1.));
            stroke.move_to(p(r - 0.5, t - f));
            stroke.arc_to(p(f - 0.5, f - 0.5), px(0.), false, true, p(r - f, t + 0.5));
            stroke.line_to(p(l + rad, t + 0.5));
            stroke.arc_to(p(rad - 0.5, rad - 0.5), px(0.), false, false, p(l + 0.5, t + rad));
            stroke.line_to(p(l + 0.5, b - rad));
            stroke.arc_to(p(rad - 0.5, rad - 0.5), px(0.), false, false, p(l + rad, b - 0.5));
            stroke.line_to(p(r - f, b - 0.5));
            stroke.arc_to(p(f - 0.5, f - 0.5), px(0.), false, true, p(r - 0.5, b + f));
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

/// The menu's width, unfolded and folded.
pub const MENU_W: f32 = 232.;
pub const MENU_FOLDED_W: f32 = 64.;
/// Below this window width the menu folds by itself.
pub const FOLD_BELOW: f32 = 1000.;

impl Manager {
    /// Folded: by choice, or because the window is narrow.
    pub(super) fn folded(width: f32) -> bool {
        width < FOLD_BELOW || Appearance::get().folded
    }

    pub(super) fn shell_menu(&self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Folding glides the width (the names fade with it); the layout is
        // the folded one once the menu is narrow.
        let target_w = if Self::folded(width) { MENU_FOLDED_W } else { MENU_W };
        let menu_w = ui::tween("menu-w", target_w, 200, window);
        let rail = menu_w < 150.;
        let names = ((menu_w - 150.) / (MENU_W - 150.)).clamp(0., 1.);
        let forced = width < FOLD_BELOW;
        let motion = !ui::reduce_motion();
        let active: &'static str = if self.page == Page::Instances { "instances" } else if self.page == Page::About { "about" } else { self.page.id() };
        let left_pad = if rail { 8. } else { 16. };
        let row = |id: &'static str, icon: Icon, title: SharedString, on: bool| {
            h_flex()
                .id(id)
                .relative()
                .h(px(ROW_H))
                .flex_shrink_0()
                .gap(px(12.))
                .when(rail, |r| r.justify_center())
                .when(!rail, |r| r.px(px(12.)).mr(px(10.)))
                .rounded(px(8.))
                .cursor_pointer()
                .text_sm()
                .text_color(if on { theme.primary } else { theme.foreground })
                .when(on, |r| r.font_weight(FontWeight::MEDIUM))
                .when(!on, |r| r.hover(|r| r.bg(theme.list_hover)))
                .child(row_probe(id))
                .child(icon.size(px(18.)).text_color(if on { theme.primary } else { theme.muted_foreground }))
                .when(!rail, |r| r.child(div().relative().whitespace_nowrap().opacity(names).child(title.clone())))
                .when(rail, move |r| r.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(title.clone()).build(w, cx)))
        };
        let items = Page::ALL.into_iter().map(|page| {
            row(page.id(), page.icon(), page.title().into(), page == self.page).on_click(cx.listener(move |this, _, _, cx| {
                this.page = page;
                cx.notify();
            }))
        });
        let instances = row("instances", Page::Instances.icon(), Page::Instances.title().into(), self.page == Page::Instances)
            .when(!self.remotes.is_empty() && !rail, |r| {
                r.child(div().flex_1()).child(div().relative().text_size(px(13.)).text_color(theme.muted_foreground).child((self.remotes.len() + 1).to_string()))
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.page = Page::Instances;
                cx.notify();
            }));

        // The product on top: its logo and name, and the fold button; folded,
        // the logo turns into the unfold button on hover.
        let brand = {
            let logo_hover = self.logo_hover;
            let logo = div()
                .id("brand-logo")
                .size(px(36.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .when(rail && !forced, |d| {
                    d.cursor_pointer()
                        .on_hover(cx.listener(|this, h: &bool, _, cx| {
                            this.logo_hover = *h;
                            cx.notify();
                        }))
                        .on_click(cx.listener(|this, _, _, cx| {
                            Appearance::set(|a| a.folded = false);
                            this.logo_hover = false;
                            cx.notify();
                        }))
                        .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Unfold menu").build(w, cx))
                })
                .when(rail && forced, |d| d.tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("The window is too narrow to unfold the menu").build(w, cx)))
                .child(if rail && !forced && logo_hover {
                    div()
                        .size_full()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.list_hover)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(Lucide::PanelLeftOpen).size(px(20.)).text_color(theme.muted_foreground))
                        .into_any_element()
                } else {
                    ui::product_logo(ui::Product::Manager, 36.).into_any_element()
                });
            h_flex()
                .h(px(72.))
                .flex_shrink_0()
                .items_center()
                .gap(px(10.))
                .when(rail, |r| r.justify_center())
                .when(!rail, |r| r.pl(px(16.)).pr(px(10.)))
                .border_b_1()
                .border_color(theme.sidebar_border)
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
                            .child(Icon::new(Lucide::PanelLeftClose).size(px(18.)))
                            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Fold menu").build(w, cx))
                            .on_click(cx.listener(|_, _, _, cx| {
                                Appearance::set(|a| a.folded = true);
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
                .size(px(34.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .border_1()
                .border_color(if on { theme.primary } else { theme.border })
                .bg(if on { theme.sidebar_accent } else { theme.background })
                .text_color(if on { theme.primary } else { theme.foreground })
                .cursor_pointer()
                .when(!on, |d| d.hover(|d| d.bg(theme.list_hover)))
                .child(Icon::new(Lucide::SlidersHorizontal).size(px(18.)))
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("About and preferences").build(w, cx))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.page = Page::About;
                    cx.notify();
                }));
            h_flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(4.))
                .border_t_1()
                .border_color(theme.sidebar_border)
                .when(rail, |r| r.flex_col().py(px(8.)).justify_center())
                .when(!rail, |r| r.py(px(10.)).pl(px(12.)).pr(px(10.)))
                .when(!rail, |r| {
                    r.child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(8.))
                            .items_center()
                            .child(ui::product_logo(ui::Product::Manager, 26.))
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

        let menu_bg = if see_through() { theme.sidebar.opacity(0.72) } else { theme.sidebar };
        v_flex()
            .w(px(menu_w))
            .overflow_hidden()
            .h_full()
            .flex_shrink_0()
            .relative()
            .bg(menu_bg)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(menu_probe(left_pad))
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
                    .gap(px(2.))
                    .pt(px(16.))
                    .pl(px(left_pad))
                    .when(rail, |m| m.pr(px(8.)))
                    .children(items)
                    .child(div().mt(px(14.)).mb(px(14.)).ml(px(2.)).mr(px(10.)).h(px(1.)).flex_shrink_0().bg(theme.sidebar_border))
                    .child(instances),
            )
            .child(footer)
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
