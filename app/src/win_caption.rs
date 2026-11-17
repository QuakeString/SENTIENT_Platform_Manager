// Copyright © 2016-2026 The SENTIENT Authors

//! The window's caption buttons on Windows (minimise, maximise or restore,
//! close), drawn by the app at the right end of each window's top bar: the
//! Platform Manager's bar, Setup's and the maintenance window's top edge.
//! There is no separate caption strip: every window opens in GPUI's own
//! title-bar mode (crate::win_frame).
//!
//! The glyphs are drawn here, not taken from an icon font or SVG: Windows
//! 11's proportions (a 10 px glyph, 1 px strokes), snapped to whole device
//! pixels at every scale, in the bar's text colour passed in (dimmed while
//! the window is inactive, as Windows does). Each button is the window
//! control area Windows expects (HTMINBUTTON / HTMAXBUTTON / HTCLOSE), so
//! the system clicks them, and hovering maximise opens the snap layouts.
//! The empty bar is the drag area ([`drag_layer`]): a drag moves the
//! window, a double click maximises or restores it (Windows' own handling
//! of HTCAPTION). The bar's controls block it ([`enabled`] plus `occlude`).
//!
//! Linux keeps its own frame. A debug build draws the controls there too
//! with `SPM_DEBUG_WIN_CAPTION=1`, for screenshots (they do nothing there).

use gpui_kit::component::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// A caption button: Windows 11's 46 px, the bar's full height.
pub const BUTTON_W: f32 = 46.;

/// The caption buttons are drawn: on Windows, or (debug builds) with
/// SPM_DEBUG_WIN_CAPTION=1.
pub fn enabled() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(all(not(windows), debug_assertions))]
    {
        use std::sync::OnceLock;
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("SPM_DEBUG_WIN_CAPTION").is_some_and(|v| v == "1"))
    }
    #[cfg(all(not(windows), not(debug_assertions)))]
    {
        false
    }
}

/// The room the three buttons take at the end of a bar (0 without them).
pub fn width() -> f32 {
    if enabled() { 3. * BUTTON_W } else { 0. }
}

/// The window's drag area: put it first in a `relative` bar, behind its
/// children. The bar's buttons, switcher and the rest `occlude()` it (when
/// [`enabled`]), so they keep their clicks. Nothing without the caption.
pub fn drag_layer() -> Div {
    div().absolute().inset_0().when(enabled(), |d| d.window_control_area(WindowControlArea::Drag))
}

/// The three caption buttons, the height of their parent (a bar), in the
/// bar's text colour `fg` over its background `bar_bg`. None without the
/// caption.
pub fn controls(fg: Hsla, bar_bg: Hsla, window: &Window) -> Option<AnyElement> {
    if !enabled() {
        return None;
    }
    let active = window.is_window_active();
    let glyph = glyph_color(fg, bar_bg, active);
    // The hover and pressed washes: the active glyph colour, faint.
    let wash = glyph_color(fg, bar_bg, true);
    let max = if window.is_maximized() { Glyph::Restore } else { Glyph::Maximize };
    Some(
        h_flex()
            .id("caption-controls")
            .flex_shrink_0()
            .h_full()
            .child(button(Glyph::Minimize, glyph, wash))
            .child(button(max, glyph, wash))
            .child(button(Glyph::Close, glyph, wash))
            .into_any_element(),
    )
}

/// Windows 11's caption height, for a window without a bar of its own
/// (Setup's page, the maintenance window): [`overlay`].
pub const CAPTION_H: f32 = 32.;

/// For a window without a bar: a `CAPTION_H` strip over the top of the
/// `relative` parent that drags the window, the buttons at its right end.
/// Put it last, over the page (the page's padding is under it).
pub fn overlay(fg: Hsla, bg: Hsla, window: &Window) -> Option<AnyElement> {
    let controls = controls(fg, bg, window)?;
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(CAPTION_H))
            .child(drag_layer())
            .child(h_flex().absolute().top_0().right_0().h_full().child(controls))
            .into_any_element(),
    )
}

/// Windows 11's close button: #c42b1c on hover, a little darker pressed.
const CLOSE_HOVER: u32 = 0xc42b1c;
const CLOSE_PRESSED: u32 = 0xa82416;

fn button(kind: Glyph, glyph: Hsla, wash: Hsla) -> Stateful<Div> {
    let close = kind == Glyph::Close;
    let (hover_bg, pressed_bg): (Hsla, Hsla) = if close { (rgb(CLOSE_HOVER).into(), rgb(CLOSE_PRESSED).into()) } else { (wash.opacity(0.09), wash.opacity(0.15)) };
    let (id, area) = match kind {
        Glyph::Minimize => ("caption-min", WindowControlArea::Min),
        Glyph::Maximize | Glyph::Restore => ("caption-max", WindowControlArea::Max),
        Glyph::Close => ("caption-close", WindowControlArea::Close),
    };
    div()
        .id(id)
        .relative()
        .flex_shrink_0()
        .w(px(BUTTON_W))
        .h_full()
        // In front of the bar's drag area, and its own control area.
        .occlude()
        .window_control_area(area)
        .hover(move |d| d.bg(hover_bg))
        .active(move |d| d.bg(pressed_bg))
        .child(crate::ui::layout_probe(id))
        .child(
            canvas(
                // Its own hitbox over the button: the glyph knows the hover
                // (white on the red close button).
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds, hitbox, window, _| {
                    let hovered = hitbox.is_hovered(window);
                    let color: Hsla = if close && hovered { white() } else { glyph };
                    paint_glyph(kind, bounds, color, window);
                },
            )
            .absolute()
            .inset_0(),
        )
}

/// Which glyph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// A glyph in device pixels: filled rectangles `[x, y, w, h]` (the line,
/// the squares' edges) and, for close, two diagonal lines `[x1, y1, x2, y2]`
/// of `stroke` pixels, their ends on pixel centres.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// The glyph's box: its top-left corner and side, whole pixels.
    pub origin: (i32, i32),
    pub size: i32,
    pub stroke: i32,
    pub rects: Vec<[i32; 4]>,
    pub lines: Vec<[f32; 4]>,
}

/// The glyph centred in a button at `(x, y, w, h)` (logical pixels) at
/// `scale`: Windows 11's 10 px glyph and 1 px stroke, scaled and snapped
/// to whole device pixels (the stroke stays 1 px up to 2x, as Windows'
/// own buttons do at 125 % and 150 %).
pub fn shape(kind: Glyph, (x, y, w, h): (f32, f32, f32, f32), scale: f32) -> Shape {
    let g = (10. * scale).round() as i32;
    let st = scale.floor().max(1.) as i32;
    let x0 = (x * scale + (w * scale - g as f32) / 2.).round() as i32;
    let y0 = (y * scale + (h * scale - g as f32) / 2.).round() as i32;
    let square = |x: i32, y: i32, s: i32| -> [[i32; 4]; 4] {
        [
            [x, y, s, st],          // top
            [x, y + s - st, s, st], // bottom
            [x, y + st, st, s - 2 * st],
            [x + s - st, y + st, st, s - 2 * st],
        ]
    };
    let mut rects = Vec::new();
    let mut lines = Vec::new();
    match kind {
        Glyph::Minimize => rects.push([x0, y0 + (g - st) / 2, g, st]),
        Glyph::Maximize => rects.extend(square(x0, y0, g)),
        Glyph::Restore => {
            // The front square at the bottom left, the back one `d` up and
            // right: only its top and right edges show, and the short
            // pieces that meet the front square.
            let d = (2. * scale).round() as i32;
            let f = g - d;
            rects.extend(square(x0, y0 + d, f));
            rects.push([x0 + d, y0, f, st]); // back top
            rects.push([x0 + g - st, y0, st, f]); // back right
            rects.push([x0 + d, y0 + st, st, d - st]); // back left, down to the front
            rects.push([x0 + f, y0 + f - st, d - st, st]); // back bottom, from the front
        }
        Glyph::Close => {
            let a = st as f32 / 2.;
            let (l, t) = (x0 as f32 + a, y0 as f32 + a);
            let (r, b) = ((x0 + g) as f32 - a, (y0 + g) as f32 - a);
            lines.push([l, t, r, b]);
            lines.push([l, b, r, t]);
        }
    }
    Shape { origin: (x0, y0), size: g, stroke: st, rects, lines }
}

/// Device pixels back to logical ones (the renderer multiplies by the
/// same scale, so the edges land where `shape` put them).
fn logical(v: f32, scale: f32) -> Pixels {
    px(v / scale)
}

fn paint_glyph(kind: Glyph, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let s = window.scale_factor();
    let b = (f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height));
    let shape = shape(kind, b, s);
    for [x, y, w, h] in shape.rects {
        if w <= 0 || h <= 0 {
            continue;
        }
        let r = Bounds { origin: point(logical(x as f32, s), logical(y as f32, s)), size: size(logical(w as f32, s), logical(h as f32, s)) };
        window.paint_quad(fill(r, color));
    }
    for [x1, y1, x2, y2] in shape.lines {
        let mut p = PathBuilder::stroke(logical(shape.stroke as f32, s));
        p.move_to(point(logical(x1, s), logical(y1, s)));
        p.line_to(point(logical(x2, s), logical(y2, s)));
        if let Ok(path) = p.build() {
            window.paint_path(path, color);
        }
    }
}

/// The glyph colour: the bar's text colour, unless it would not read on the
/// bar (contrast under 3:1), then black or white by the bar's luminance.
/// Inactive: dimmed, as Windows dims an inactive window's caption.
pub fn glyph_color(fg: Hsla, bar_bg: Hsla, active: bool) -> Hsla {
    let bg_l = luminance(bar_bg);
    let base = if contrast(luminance(fg), bg_l) >= 3. {
        fg
    } else if bg_l > 0.18 {
        hsla(0., 0., 0.10, 1.)
    } else {
        hsla(0., 0., 1., 1.)
    };
    if active { base } else { base.opacity(0.42) }
}

/// Relative luminance (WCAG) of an opaque colour.
fn luminance(c: Hsla) -> f32 {
    let r = Rgba::from(c);
    let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(r.r) + 0.7152 * lin(r.g) + 0.0722 * lin(r.b)
}

fn contrast(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would come with it.
    use super::{glyph_color, logical, luminance, shape, Glyph, BUTTON_W};
    use gpui_kit::{hsla, rgb, Hsla};

    const SCALES: [f32; 4] = [1., 1.25, 1.5, 2.];
    const KINDS: [Glyph; 4] = [Glyph::Minimize, Glyph::Maximize, Glyph::Restore, Glyph::Close];

    /// A logical value the renderer multiplies by `scale` lands on a whole
    /// device pixel.
    fn whole(v: f32, scale: f32) -> bool {
        let d = f32::from(logical(v, scale)) * scale;
        (d - d.round()).abs() < 1e-3
    }

    #[test]
    fn glyphs_land_on_whole_device_pixels() {
        // Buttons at awkward logical positions too (a bar at x = 1013.3).
        for scale in SCALES {
            for origin in [(0., 0.), (1013.3, 0.), (977.7, 0.5)] {
                for kind in KINDS {
                    let s = shape(kind, (origin.0, origin.1, BUTTON_W, 60.), scale);
                    for [x, y, w, h] in &s.rects {
                        for v in [*x, *y, *x + *w, *y + *h] {
                            assert!(whole(v as f32, scale), "{kind:?} at {scale}: {v}");
                        }
                        assert!(*w >= 0 && *h >= 0);
                    }
                    // The X's ends on pixel centres: its edges on pixel lines.
                    for l in &s.lines {
                        for v in l {
                            assert!(whole(v - s.stroke as f32 / 2., scale), "{kind:?} at {scale}: {v}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn glyph_size_and_stroke_follow_windows_11() {
        let sizes: Vec<(i32, i32)> = SCALES.iter().map(|&sc| shape(Glyph::Maximize, (0., 0., BUTTON_W, 32.), sc)).map(|s| (s.size, s.stroke)).collect();
        assert_eq!(sizes, vec![(10, 1), (13, 1), (15, 1), (20, 2)]);
    }

    #[test]
    fn glyphs_are_centred_and_inside_their_box() {
        for scale in SCALES {
            for kind in KINDS {
                let (bx, by, bw, bh) = (500.25, 0., BUTTON_W, 60.);
                let s = shape(kind, (bx, by, bw, bh), scale);
                let centre = ((bx + bw / 2.) * scale, (by + bh / 2.) * scale);
                let mid = (s.origin.0 as f32 + s.size as f32 / 2., s.origin.1 as f32 + s.size as f32 / 2.);
                assert!((mid.0 - centre.0).abs() <= 0.5 && (mid.1 - centre.1).abs() <= 0.5, "{kind:?} at {scale}");
                let (x0, y0, g) = (s.origin.0, s.origin.1, s.size);
                for [x, y, w, h] in &s.rects {
                    assert!(*x >= x0 && *y >= y0 && x + w <= x0 + g && y + h <= y0 + g, "{kind:?} at {scale}");
                }
                for [x1, y1, x2, y2] in &s.lines {
                    for (x, y) in [(x1, y1), (x2, y2)] {
                        assert!(*x > x0 as f32 && *x < (x0 + g) as f32 && *y > y0 as f32 && *y < (y0 + g) as f32);
                    }
                }
            }
        }
    }

    #[test]
    fn each_glyph_has_its_parts() {
        let s = |k| shape(k, (0., 0., BUTTON_W, 32.), 1.);
        // Minimise: one line across the glyph, in its middle.
        let m = s(Glyph::Minimize);
        assert_eq!(m.rects, vec![[18, 15, 10, 1]]);
        // Maximise: the square's four edges, nothing doubled at the corners.
        let x = s(Glyph::Maximize);
        assert_eq!(x.rects.len(), 4);
        let area: i32 = x.rects.iter().map(|r| r[2] * r[3]).sum();
        assert_eq!(area, 10 * 10 - 8 * 8);
        // Restore: the front square plus four pieces of the back one.
        assert_eq!(s(Glyph::Restore).rects.len(), 8);
        // Close: two diagonals, no rectangles.
        let c = s(Glyph::Close);
        assert!(c.rects.is_empty());
        assert_eq!(c.lines.len(), 2);
    }

    #[test]
    fn colour_follows_the_bars_text_and_luminance() {
        let dark_text = hsla(0., 0., 0.09, 1.);
        let light_text = hsla(0., 0., 0.98, 1.);
        // SENTIENT teal mixed faintly into white (light) and into the dark
        // card colour (dark): the theme's text reads, so it is used.
        let light_bar: Hsla = Hsla::white().blend(Hsla::from(rgb(0x187074)).opacity(0.26));
        let dark_bar: Hsla = hsla(0., 0., 0.1, 1.).blend(Hsla::from(rgb(0x6b4fa0)).opacity(0.18));
        assert_eq!(glyph_color(dark_text, light_bar, true), dark_text);
        assert_eq!(glyph_color(light_text, dark_bar, true), light_text);
        // A text colour that would not read on the bar: black on a light
        // bar, white on a dark one.
        assert!(luminance(glyph_color(light_text, light_bar, true)) < 0.05);
        assert!(luminance(glyph_color(dark_text, dark_bar, true)) > 0.9);
        let saturated: Hsla = rgb(0x6b4fa0).into();
        assert!(luminance(glyph_color(hsla(0., 0., 0.3, 1.), saturated, true)) > 0.9);
        // Inactive: the same colour, dimmed.
        let dim = glyph_color(dark_text, light_bar, false);
        assert!(dim.a < 0.5 && dim.a > 0.3);
        assert_eq!((dim.h, dim.s, dim.l), (dark_text.h, dark_text.s, dark_text.l));
    }
}
