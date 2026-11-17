// Copyright © 2016-2026 The SENTIENT Authors

//! The popup gate: every popup opens with the shared motion (ui::appear for
//! dialogs and panels, ui::menu_appear for menus and tooltips, the toolkit's
//! popup menu patched to the same numbers). Pages switch instantly; popups
//! do not (the owner's rule, 2026-11-17).
//!
//! A source scan, not a render probe: the layout tests run with reduce
//! motion on (SPM_REDUCED_MOTION=1, so the frames are still), and catching a
//! popup mid-motion on a timed capture under Xvfb would be flaky. A popup
//! opens through few doors in this code (a `deferred` layer, the dialog
//! layer, a toolkit menu, a tooltip), so each door is checked here, and
//! a new one fails `cargo test` until it uses the helper or is listed in
//! ALLOW with its reason.

use std::path::{Path, PathBuf};

/// Sites that open something over the page without the helpers, each with
/// why: (file under src/, a text unique to the site, the reason).
const ALLOW: &[(&str, &str, &str)] = &[
    // The (i) help's own tooltip view: InfoTip's render calls menu_appear
    // (checked in `tooltips_open_with_the_menu_motion`).
    ("ui.rs", "cx.new(|_| InfoTip(", "the (i) tooltip view animates in its own render"),
];

/// How far after a `deferred(` the helper must appear (the layer's opening
/// lines: its placement div and the helper call).
const DEFERRED_WINDOW: usize = 3;

fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read src") {
            let p = e.expect("dir entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|p| !p.ends_with("popup_gate.rs"))
        .map(|p| (p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), lf(&std::fs::read_to_string(&p).unwrap())))
        .collect()
}

/// Lines that are code (a `//` comment line is skipped).
fn code_lines(text: &str) -> Vec<(usize, &str)> {
    text.lines().enumerate().filter(|(_, l)| !l.trim_start().starts_with("//")).map(|(i, l)| (i + 1, l)).collect()
}

fn allowed(file: &str, near: &str) -> bool {
    ALLOW.iter().any(|(f, m, _)| *f == file && near.contains(m))
}

/// Every `deferred(` layer (the bell, the switcher, the theme menu, the
/// job panel, the licence menu) wraps its popup in a helper.
/// Source text with LF line ends: a Windows checkout has CRLF, and the
/// searches below look for "\n}\n".
fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[test]
fn every_deferred_popup_uses_the_helper() {
    let mut bad = Vec::new();
    for (file, text) in sources() {
        let lines = code_lines(&text);
        for (ix, (n, l)) in lines.iter().enumerate() {
            if !l.contains("deferred(") {
                continue;
            }
            let near: String = lines[ix..(ix + DEFERRED_WINDOW + 1).min(lines.len())].iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n");
            let ok = ["appear(", "menu_appear(", "popover_menu("].iter().any(|h| near.contains(h));
            if !ok && !allowed(&file, &near) {
                bad.push(format!("{file}:{n}: a deferred popup without ui::appear / ui::menu_appear / ui::popover_menu"));
            }
        }
    }
    assert!(bad.is_empty(), "popups that open with no motion (wrap them, or list them in popup_gate::ALLOW with the reason):\n{}", bad.join("\n"));
}

/// Toolkit popups that bring their own motion (or none): only through the
/// doors checked here.
#[test]
fn no_toolkit_popup_bypasses_the_motion() {
    // Popover draws its content with no motion; Select, Combobox and
    // DatePicker use the toolkit's 150 ms / 8 px drop; the toolkit's
    // dialogs and sheets are not the app's dialog (crate::modal).
    let doors = [
        ("Popover::new(", "a raw toolkit Popover opens with no motion: use ui::popover_menu or a deferred layer with ui::appear"),
        ("PopupMenu::build(", "build menus through .dropdown_menu / .context_menu (the patched PopupMenu)"),
        ("anchored()", "an anchored layer: place it in a deferred layer with ui::appear / ui::menu_appear"),
        ("Select::new(", "the toolkit Select moves 150 ms / 8 px: align it with ui::menu_appear first"),
        ("Combobox::new(", "the toolkit Combobox moves 150 ms / 8 px: align it with ui::menu_appear first"),
        ("DatePicker::new(", "the toolkit DatePicker moves 150 ms / 8 px: align it with ui::menu_appear first"),
        ("open_dialog(", "use crate::modal (its layer appears with ui::appear)"),
        ("open_sheet(", "use crate::modal (its layer appears with ui::appear)"),
        ("HoverCard::new(", "a hover card opens with no motion: wrap it in ui::menu_appear"),
    ];
    let mut bad = Vec::new();
    for (file, text) in sources() {
        for (n, l) in code_lines(&text) {
            for (door, why) in doors {
                if l.contains(door) && !allowed(&file, l) {
                    bad.push(format!("{file}:{n}: {why}"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A div's `.tooltip(...)` closure goes through ui::tooltip (the menu
/// motion); a button's `.tooltip("text")` opens through the toolkit's
/// tooltip layer, which moves the same way.
#[test]
fn tooltips_open_with_the_menu_motion() {
    let mut bad = Vec::new();
    for (file, text) in sources() {
        for (n, l) in code_lines(&text) {
            if file == "ui.rs" && l.contains("Tooltip::new(text.clone()).build(") {
                continue; // the helper itself
            }
            if l.contains("Tooltip::new(") {
                bad.push(format!("{file}:{n}: a toolkit Tooltip built by hand: use .tooltip(crate::ui::tooltip(...))"));
            }
            let Some(at) = l.find(".tooltip(") else { continue };
            let arg = l[at + ".tooltip(".len()..].trim_start();
            let closure = arg.starts_with('|') || arg.starts_with("move |");
            if closure && !allowed(&file, l) {
                bad.push(format!("{file}:{n}: a tooltip view of its own: use crate::ui::tooltip, or animate its render and list it in ALLOW"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let ui = lf(include_str!("ui.rs"));
    let info = &ui[ui.find("impl Render for InfoTip").expect("InfoTip")..];
    let info = &info[..info.find("\n}\n").expect("InfoTip's end")];
    assert!(info.contains("menu_appear("), "the (i) tooltip (InfoTip) opens with ui::menu_appear");
}

/// The dialog layer and the app's popover menu call the helpers.
#[test]
fn dialogs_and_popover_menus_use_the_helpers() {
    let modal = lf(include_str!("modal.rs"));
    let layer = &modal[modal.find("pub fn layer(").expect("modal::layer")..];
    let layer = &layer[..layer.find("\n}\n").expect("layer's end")];
    assert!(layer.contains("crate::ui::appear("), "the dialog (modal::layer) opens with ui::appear");
    let ui = lf(include_str!("ui.rs"));
    let menu = &ui[ui.find("pub fn popover_menu(").expect("popover_menu")..];
    let menu = &menu[..menu.find("\n}\n").expect("popover_menu's end")];
    assert!(menu.contains("menu_appear("), "ui::popover_menu opens with ui::menu_appear");
}

/// The toolkit's popup menu (dropdown and context menus) moves as ours do:
/// the vendored patch is in place, with the same numbers.
#[test]
fn toolkit_menus_move_like_ours() {
    let src = lf(include_str!("../vendor/gpui-component/src/menu/popup_menu.rs"));
    let num = |name: &str| -> f64 {
        let at = src.find(&format!("const {name}:")).unwrap_or_else(|| panic!("{name} in the vendored popup_menu.rs"));
        let rest = &src[at..];
        let v = rest[rest.find('=').unwrap() + 1..rest.find(';').unwrap()].trim();
        v.trim_end_matches('.').parse().unwrap_or_else(|_| panic!("{name} = {v}"))
    };
    assert_eq!(num("SENTIENT_OPEN_MS"), crate::ui::MENU_APPEAR_MS as f64, "the vendored menu's duration is ui::MENU_APPEAR_MS");
    assert_eq!(num("SENTIENT_OPEN_RISE"), crate::ui::MENU_APPEAR_RISE as f64, "the vendored menu's rise is ui::MENU_APPEAR_RISE");
    assert!(src.contains(".with_animation(") && src.contains("\"sentient-menu-open\""), "the vendored PopupMenu render plays the open motion");
    // The menus the app opens are PopupMenus.
    for (f, s) in [("dropdown_menu.rs", lf(include_str!("../vendor/gpui-component/src/menu/dropdown_menu.rs"))), ("context_menu.rs", lf(include_str!("../vendor/gpui-component/src/menu/context_menu.rs")))] {
        assert!(s.contains("PopupMenu"), "{f} draws a PopupMenu");
    }
}

/// The allow-list names sites that exist (a stale entry hides nothing).
#[test]
fn allow_list_is_current() {
    let src = sources();
    for (f, m, why) in ALLOW {
        assert!(!why.is_empty());
        assert!(src.iter().any(|(file, text)| file == f && text.contains(m)), "ALLOW entry {f} {m:?} matches nothing");
    }
}
