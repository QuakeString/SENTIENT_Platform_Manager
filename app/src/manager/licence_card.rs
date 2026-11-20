// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT's licence card, measured from SENTIENT's Licence page
//! (ui-ngx license.component.scss, `.lic-card` and its tier palettes): a
//! 540 × 340 card, 22 px corners, padding 24 / 28 / 42, the brand row, the
//! two brand marks 58 px below it, the masked key 37 px below them and the
//! four facts at the bottom. On a page narrower than the card, everything in
//! it shrinks by the same factor.
//!
//! The painted parts (gradients, the tier's pattern and accent curves, the
//! metallic rim, the marks' foil) are SVG drawn as images; the text is the
//! app's own, so it stays sharp at every scale.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use crate::licence::Status;

pub const W: f32 = 540.;
pub const H: f32 = 340.;
/// The mockup's line height.
const LH: f32 = 1.45;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Tier {
    Demo,
    Basic,
    Advance,
    Professional,
    Enterprise,
}

impl Tier {
    /// SENTIENT's getTierClass: an unknown type looks Professional.
    fn of(t: Option<&str>) -> Tier {
        match t.unwrap_or("").to_ascii_lowercase().as_str() {
            "demo" => Tier::Demo,
            "basic" => Tier::Basic,
            "advance" => Tier::Advance,
            "enterprise" => Tier::Enterprise,
            _ => Tier::Professional,
        }
    }

    /// (radial highlight: cx%, cy%, colour, circle), the 135° gradient
    /// stops, the rim colour.
    fn palette(self) -> (f32, f32, &'static str, bool, [(&'static str, f32); 3], &'static str) {
        match self {
            Tier::Demo => (22., 15., "rgba(255,255,255,.18)", false, [("#3a4453", 0.), ("#222a36", 0.55), ("#0f141c", 1.)], "rgba(207,214,223,.35)"),
            Tier::Basic => (18., 10., "rgba(124,197,255,.22)", true, [("#103a6e", 0.), ("#0a234a", 0.55), ("#040e22", 1.)], "rgba(188,196,207,.40)"),
            Tier::Advance => (82., 10., "rgba(52,211,153,.22)", false, [("#0f4a3d", 0.), ("#082e25", 0.55), ("#02120e", 1.)], "rgba(232,199,154,.45)"),
            Tier::Professional => (50., 0., "rgba(232,180,163,.18)", false, [("#0f1832", 0.), ("#0a1126", 0.55), ("#040814", 1.)], "rgba(243,197,182,.45)"),
            Tier::Enterprise => (50., 0., "rgba(230,230,230,.10)", false, [("#161616", 0.), ("#080808", 0.5), ("#000000", 1.)], "rgba(227,227,227,.50)"),
        }
    }

    /// The marks' foil (lic-chip-<tier>).
    fn foil(self) -> [&'static str; 3] {
        match self {
            Tier::Demo => ["#9aa3ad", "#cfd6df", "#6b7585"],
            Tier::Basic => ["#bcc4cf", "#ffffff", "#7d8693"],
            Tier::Advance => ["#c79567", "#e8c79a", "#8d5a30"],
            Tier::Professional => ["#dca18d", "#f4cdb8", "#9a6151"],
            Tier::Enterprise => ["#e3e3e3", "#ffffff", "#7a7a7a"],
        }
    }

    /// The tier chip: its 90° gradient, text colour and corner radius.
    fn chip(self) -> (&'static [(&'static str, f32)], u32, f32) {
        match self {
            Tier::Demo => (&[("#9aa3ad", 0.), ("#cfd6df", 0.5), ("#7d8693", 1.)], 0x1a1f24, 4.),
            Tier::Basic => (&[("#bcc4cf", 0.), ("#ffffff", 0.5), ("#8d97a3", 1.)], 0x0a234a, 6.),
            Tier::Advance => (&[("#c79567", 0.), ("#e8c79a", 0.5), ("#a06d40", 1.)], 0x3a2515, 6.),
            Tier::Professional => (&[("#e6a191", 0.), ("#f3c5b6", 0.5), ("#b67864", 1.)], 0x2c1814, 4.),
            Tier::Enterprise => (&[("#e3e3e3", 0.), ("#ffffff", 0.35), ("#cdcdcd", 0.65), ("#9b9b9b", 1.)], 0x0a0a0a, 4.),
        }
    }
}

fn stops(list: &[(&str, f32)]) -> String {
    list.iter().map(|(c, o)| format!(r#"<stop offset="{o}" stop-color="{c}"/>"#)).collect()
}

/// The card's painted layer, at 540 × 340 (drawn at twice that).
fn background_svg(t: Tier) -> String {
    let (hx, hy, hc, circle, lin, rim) = t.palette();
    // CSS "ellipse/circle at x y" to the farthest corner.
    let (cx, cy) = (W * hx / 100., H * hy / 100.);
    let (fx, fy) = (cx.max(W - cx), cy.max(H - cy));
    let (rx, ry) = if circle { let r = (fx * fx + fy * fy).sqrt(); (r, r) } else { (fx * std::f32::consts::SQRT_2, fy * std::f32::consts::SQRT_2) };
    let pattern = match t {
        Tier::Demo => r##"<pattern id="p" width="20" height="20" patternUnits="userSpaceOnUse"><circle cx="10" cy="10" r=".7" fill="#ffffff" fill-opacity=".06"/></pattern>"##,
        Tier::Basic => r##"<pattern id="p" width="6" height="6" patternUnits="userSpaceOnUse"><path d="M3 0 V6" stroke="#ffffff" stroke-opacity=".045" stroke-width=".5"/></pattern>"##,
        Tier::Advance => r##"<pattern id="p" width="40" height="40" patternUnits="userSpaceOnUse"><path d="M20 2 L38 20 L20 38 L2 20 Z" fill="none" stroke="#ffffff" stroke-opacity=".055" stroke-width="1"/><path d="M20 12 L28 20 L20 28 L12 20 Z" fill="none" stroke="#ffffff" stroke-opacity=".035" stroke-width=".7"/></pattern>"##,
        Tier::Professional => r##"<pattern id="p" width="120" height="60" patternUnits="userSpaceOnUse"><path d="M0 30 Q30 0 60 30 T120 30" fill="none" stroke="#ffffff" stroke-opacity=".045" stroke-width=".75"/></pattern>"##,
        // Enterprise: the carbon-fibre weave (its ::after), at its .55.
        Tier::Enterprise => r##"<pattern id="p" width="14" height="14" patternUnits="userSpaceOnUse"><path d="M0 14 L14 0 M0 0 L14 14" stroke="#ffffff" stroke-opacity=".012" stroke-width=".4"/></pattern>"##,
    };
    let accents = match t {
        Tier::Professional => r##"<path d="M540 340 Q420 220 540 100" fill="none" stroke="#f3c5b6" stroke-width="3" opacity=".6"/><path d="M540 340 Q480 240 540 160" fill="none" stroke="#f3c5b6" stroke-width="2" opacity=".4"/>"##,
        Tier::Advance => r##"<path d="M540 340 Q470 260 540 180" fill="none" stroke="#e8c79a" stroke-width="2" opacity=".65"/><rect width="540" height="340" fill="url(#adv)"/>"##,
        _ => "",
    };
    // 135° and (advance's sheen) 225° across the 540 × 340 box.
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="540" height="340" viewBox="0 0 540 340"><defs>
<clipPath id="c"><rect width="540" height="340" rx="22"/></clipPath>
<linearGradient id="l" gradientUnits="userSpaceOnUse" x1="50" y1="-50" x2="490" y2="390">{lin}</linearGradient>
<radialGradient id="r" gradientUnits="userSpaceOnUse" cx="{cx}" cy="{cy}" r="{rx}" gradientTransform="translate({cx} {cy}) scale(1 {sy}) translate({ncx} {ncy})"><stop offset="0" stop-color="{hc}" stop-opacity="{ha}"/><stop offset=".7" stop-color="{hc}" stop-opacity="0"/></radialGradient>
<linearGradient id="adv" gradientUnits="userSpaceOnUse" x1="490" y1="-50" x2="50" y2="390"><stop offset="0" stop-color="rgb(232,199,154)" stop-opacity=".10"/><stop offset=".55" stop-color="rgb(232,199,154)" stop-opacity="0"/></linearGradient>
{pattern}</defs>
<g clip-path="url(#c)"><rect width="540" height="340" fill="url(#l)"/><rect width="540" height="340" fill="url(#r)"/><rect width="540" height="340" fill="url(#p)"/>{accents}</g>
<rect x=".5" y=".5" width="539" height="339" rx="21.5" fill="none" stroke="{rc}" stroke-opacity="{ra}" stroke-width="1"/></svg>"##,
        lin = stops(&lin),
        sy = ry / rx,
        ncx = -cx,
        ncy = -cy,
        hc = rgba_to_rgb(hc),
        ha = rgba_alpha(hc),
        rc = rgba_to_rgb(rim),
        ra = rgba_alpha(rim),
    )
}

/// "rgba(1,2,3,.4)" → "rgb(1,2,3)".
fn rgba_to_rgb(c: &str) -> String {
    let inner = c.trim_start_matches("rgba(").trim_end_matches(')');
    let parts: Vec<&str> = inner.split(',').collect();
    tf!("rgb({},{},{})", parts[0], parts[1], parts[2])
}

fn rgba_alpha(c: &str) -> String {
    c.trim_end_matches(')').rsplit(',').next().unwrap_or("1").to_string()
}

const SENTIENT_MARK: &str = r#"<path d="M 7.7914277,1.0348821 C 7.7153998,1.6069896 6.8573008,2.7562433 5.1657725,2.7562433 H 1.1478738 v 0.3129756 h 5.6802166 v 4.2251615 c 0.7093826,0 1.28445,-0.5604969 1.28445,-1.2519036 V 1.0348821 Z"/><path d="M 6.3464966,1.0348765 H 2.3287052 a 1.1807798,1.2519341 0 0 0 -1.1808199,1.251892 h 4.017877 A 1.1807334,1.2518849 0 0 0 6.3464966,1.0348828 Z"/><path d="M 1.1478765,3.5386825 H 5.0946208 A 1.2518863,1.2518863 0 0 1 6.3465069,4.7905706 V 5.181801 H 2.3997607 A 1.2518847,1.2518847 0 0 1 1.1478765,3.9299149 Z"/><path d="m 1.1478736,7.2943805 3.9467472,1e-6 A 1.251886,1.251886 0 0 0 6.3465069,6.0424934 V 5.651263 l -3.9467492,-9e-7 a 1.2518844,1.2518844 0 0 0 -1.2518841,1.251886 z"/>"#;
const N_MARK: &str = r#"<path transform="translate(2 0)" d="M 21.093685,36.04892 64.760524,68.802188 80.931399,80.929302 V 0.06650616 L 64.760524,12.197822 V 48.587543 L 21.093685,15.834284 Z"/><path transform="translate(-2 0)" d="M 16.24368,12.197822 0.06860324,0.06650616 V 80.933495 L 16.24368,68.802188 c 0,-18.868122 0,-37.736244 0,-56.604366 z"/>"#;

/// A brand mark in the tier's foil, 56 × 50 (drawn at eight times that).
fn mark_svg(t: Tier, n: bool) -> String {
    let [a, b, c] = t.foil();
    let grad = format!(r#"<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{a}"/><stop offset=".5" stop-color="{b}"/><stop offset="1" stop-color="{c}"/></linearGradient></defs>"#);
    if n {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="224" height="200" viewBox="-12 -12 105 105">{grad}<g fill="url(#g)" stroke="url(#g)" stroke-width="1.5" stroke-linejoin="round">{N_MARK}</g></svg>"#)
    } else {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="224" height="200" viewBox="0 0 9.2604168 8.329258">{grad}<g fill="url(#g)">{SENTIENT_MARK}</g></svg>"#)
    }
}

/// The tier chip's gradient, stretched to the chip.
fn chip_svg(t: Tier) -> String {
    let (list, _, _) = t.chip();
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="60" viewBox="0 0 100 10" preserveAspectRatio="none"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="0">{}</linearGradient></defs><rect width="100" height="10" fill="url(#g)"/></svg>"#, stops(list))
}

thread_local! {
    static IMAGES: RefCell<HashMap<(Tier, u8), Arc<Image>>> = RefCell::new(HashMap::new());
}

/// The painted part `which` (0 card, 1 SENTIENT mark, 2 N, 3 chip), made once.
fn image(t: Tier, which: u8) -> Arc<Image> {
    IMAGES.with(|m| {
        m.borrow_mut()
            .entry((t, which))
            .or_insert_with(|| {
                let svg = match which {
                    0 => background_svg(t),
                    1 => mark_svg(t, false),
                    2 => mark_svg(t, true),
                    _ => chip_svg(t),
                };
                Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
            })
            .clone()
    })
}

/// Text with CSS letter-spacing: each character followed by `ls`. Runs of
/// spaces collapse to one, as HTML shows them (the key's "INVE  PROF").
fn spaced(text: &str, size: f32, ls: f32, weight: FontWeight) -> Div {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    h_flex().flex_shrink_0().children(text.chars().collect::<Vec<_>>().into_iter().map(move |c| {
        div()
            .text_size(px(size))
            .line_height(px(size * LH))
            .font_weight(weight)
            .mr(px(ls))
            .child(if c == ' ' { "\u{a0}".to_string() } else { c.to_string() })
    }))
}

/// The card for `status`, `w` wide (at most 540).
pub fn card(status: &Status, w: f32, mono: SharedString) -> Div {
    let f = (w / W).min(1.);
    let s = |v: f32| v * f;
    let tier = Tier::of(status.license_type.as_deref());
    let (_, chip_fg, chip_r) = tier.chip();
    let white = gpui_kit::white();
    let typ = status.license_type.as_deref();

    let top = h_flex()
        .items_start()
        .justify_between()
        .gap(px(s(16.)))
        .child(
            v_flex()
                .child(spaced("INVENIA SYSTEMS", s(18.), s(3.), FontWeight::BOLD))
                .child(div().mt(px(s(2.))).opacity(0.7).child(spaced(t!("SENTIENT LICENSE"), s(9.), s(3.), FontWeight::MEDIUM))),
        )
        .child(
            div()
                .relative()
                .flex_shrink_0()
                .rounded(px(s(chip_r)))
                .child(img(image(tier, 3)).absolute().top_0().left_0().size_full().rounded(px(s(chip_r))).object_fit(ObjectFit::Fill))
                .child(
                    div()
                        .relative()
                        .py(px(s(6.)))
                        .pl(px(s(14.)))
                        .pr(px(s(14.)))
                        .text_color(rgb(chip_fg))
                        .child(spaced(&crate::licence::tier_label(typ).to_uppercase(), s(12.), s(2.), FontWeight::BOLD)),
                ),
        );

    let marks = h_flex()
        .gap(px(s(14.)))
        .mt(px(s(58.)))
        .ml(px(s(19.)))
        .child(img(image(tier, 1)).w(px(s(56.))).h(px(s(50.))).flex_shrink_0())
        .child(img(image(tier, 2)).w(px(s(56.))).h(px(s(50.))).flex_shrink_0());

    let key = div().mt(px(s(37.))).font_family(mono).child(spaced(&crate::licence::card_key(status.license_key.as_deref(), typ), s(21.), s(4.), FontWeight::SEMIBOLD));

    let label = |t: &str| div().opacity(0.6).child(spaced(&t.to_uppercase(), s(9.), s(2.), FontWeight::MEDIUM));
    let value = |t: String| div().mt(px(s(4.))).text_size(px(s(15.))).line_height(px(s(15.) * LH)).font_weight(FontWeight::SEMIBOLD).whitespace_nowrap().child(t);
    let company = status.customer_details.as_ref().map(|c| c.company.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| "—".into());
    let versions = status.license_allowed_versions.clone().unwrap_or_default();
    let valid = status.valid;
    let bottom = h_flex()
        .items_end()
        .gap(px(s(24.)))
        .mt_auto()
        .pt(px(s(18.)))
        .child(
            v_flex().flex_1().min_w_0().child(label(t!("Licensed to"))).child(
                div().mt(px(s(4.))).text_size(px(s(15.))).line_height(px(s(15.) * 1.2)).font_weight(FontWeight::SEMIBOLD).child(company),
            ),
        )
        .when(!versions.is_empty(), |r| r.child(v_flex().flex_shrink_0().child(label(t!("Versions"))).child(value(format!("v{}", versions.join("  v"))))))
        .child(v_flex().flex_shrink_0().child(label(t!("Valid through"))).child(value(crate::licence::valid_through(status.expiration_date.as_deref(), typ))))
        .child(
            v_flex().flex_shrink_0().child(label(t!("Status"))).child(
                h_flex()
                    .gap(px(s(6.)))
                    .items_center()
                    .mt(px(s(4.)))
                    .child(div().size(px(s(8.))).rounded_full().bg(rgb(if valid { 0x34d399 } else { 0xef4444 })))
                    .child(div().text_size(px(s(15.))).line_height(px(s(15.) * LH)).font_weight(FontWeight::SEMIBOLD).whitespace_nowrap().child(if valid { t!("Active") } else { t!("Expired") })),
            ),
        );

    div()
        .relative()
        .flex_shrink_0()
        .w(px(w))
        .h(px(H * f))
        .rounded(px(s(22.)))
        .text_color(white)
        .shadow(vec![
            BoxShadow { color: black().opacity(0.22), offset: point(px(0.), px(s(16.))), blur_radius: px(s(40.)), spread_radius: px(0.), inset: false },
            BoxShadow { color: black().opacity(0.12), offset: point(px(0.), px(s(6.))), blur_radius: px(s(14.)), spread_radius: px(0.), inset: false },
        ])
        .child(img(image(tier, 0)).absolute().top_0().left_0().size_full().object_fit(ObjectFit::Fill))
        .child(
            v_flex()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .pt(px(s(24.)))
                .px(px(s(28.)))
                .pb(px(s(42.)))
                .child(top)
                .child(marks)
                .child(key)
                .child(bottom),
        )
}

use gpui_kit::prelude::FluentBuilder as _;

#[cfg(test)]
mod tests {
    use super::{Tier, background_svg, chip_svg, mark_svg};

    #[test]
    fn every_tier_paints_valid_svg() {
        for t in [Tier::Demo, Tier::Basic, Tier::Advance, Tier::Professional, Tier::Enterprise] {
            for svg in [background_svg(t), mark_svg(t, false), mark_svg(t, true), chip_svg(t)] {
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"), "{svg}");
                assert!(!svg.contains("rgba("), "{svg}");
            }
        }
        assert!(background_svg(Tier::Professional).contains(r#"stop-color="rgb(232,180,163)" stop-opacity=".18""#));
    }
}
