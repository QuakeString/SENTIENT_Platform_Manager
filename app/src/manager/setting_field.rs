// Copyright © 2016-2026 The SENTIENT Authors
//
//! One setting's field, from its kind in the catalog: a switch for on/off, a
//! file path with its button, a short number field with its unit, or text.
//! Shared by Platform settings on this computer and on a remote server.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme, h_flex,
    input::{Input, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::catalog::{Setting, ValueKind};
use crate::ui;

pub type SetFn = Rc<dyn Fn(&str, &mut Window, &mut App)>;
pub type ActFn = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the file button does: choose a file here, or check a path on the server.
pub enum FileButton {
    Choose(ActFn),
    Check(ActFn),
}

/// Settings offered as a few named values: (label, value). An empty value
/// is SENTIENT's own.
pub fn choices_for(env: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match env {
        "RUST_LOG" => Some(&[("Errors only", "error"), ("Normal", "info"), ("Detailed", "debug")]),
        "AUDIT_LOG_TTL_DAYS" | "NOTIFICATIONS_TTL_DAYS" => Some(&[("Forever", "0"), ("30 days", "30"), ("90 days", "90"), ("1 year", "365")]),
        "SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS" => Some(&[("Built-in", ""), ("7 days", "604800"), ("30 days", "2592000"), ("90 days", "7776000")]),
        "SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS" => Some(&[("Built-in", ""), ("1 day", "86400"), ("7 days", "604800"), ("30 days", "2592000")]),
        _ => None,
    }
}

pub fn field(setting: &Setting, input: &Entity<InputState>, set: SetFn, file: FileButton, cx: &App) -> AnyElement {
    match field_parts(setting, input, set, file, false, cx) {
        (f, Some(button)) => h_flex().flex_1().min_w_0().gap_2().items_center().child(f).child(button).into_any_element(),
        (f, None) => f,
    }
}

/// The field and, for a file, its button apart: the All variables table puts
/// every input at one width and the buttons in a column of their own
/// (`table`: inputs fill their cell).
pub fn field_parts(setting: &Setting, input: &Entity<InputState>, set: SetFn, file: FileButton, table: bool, cx: &App) -> (AnyElement, Option<AnyElement>) {
    let theme = cx.theme().clone();
    let env = setting.env.clone();
    if let Some(choices) = choices_for(&env) {
        let now = input.read(cx).value().trim().to_string();
        let effective = if now.is_empty() { setting.default.clone() } else { now.clone() };
        // A log filter ("info,sqlx=warn") counts by its overall level.
        let level = effective.split(',').next().unwrap_or("").trim().to_string();
        let at = choices.iter().position(|(_, v)| !v.is_empty() && (*v == effective || *v == level)).or_else(|| if now.is_empty() { choices.iter().position(|(_, v)| v.is_empty()) } else { None });
        return (
            ui::seg_track(&theme)
                .children(choices.iter().enumerate().map(|(i, (label, value))| {
                    let set = set.clone();
                    ui::seg_item(&theme, SharedString::from(format!("{env}-c{i}")), *label, at == Some(i)).on_click(move |_, window, cx| set(value, window, cx))
                }))
                .into_any_element(),
            None,
        );
    }
    let el = match setting.kind() {
        ValueKind::Switch => {
            let now = input.read(cx).value().trim().to_string();
            let builtin = setting.default.clone();
            let effective = if now.is_empty() { builtin.clone() } else { now };
            // Yes/no is a switch; "default" is said once, quietly.
            let on = effective == "true";
            let builtin_on = builtin == "true";
            h_flex()
                .gap_3()
                .items_center()
                .when(!builtin.is_empty(), |r| r.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(if builtin_on { "Default: on" } else { "Default: off" })))
                .child(gpui_kit::component::switch::Switch::new(SharedString::from(format!("sw-{env}"))).checked(on).on_click(move |v: &bool, window, cx| {
                    let want = if *v { "true" } else { "false" };
                    // The built-in value is written as "not set".
                    set(if builtin == want { "" } else { want }, window, cx);
                }))
                .into_any_element()
        }
        ValueKind::Number(unit) => h_flex()
            .gap_2()
            .items_center()
            .when(table, |r| r.flex_1().min_w_0())
            .child(if table { div().flex_1().min_w_0().child(Input::new(input)) } else { div().w(px(130.)).child(Input::new(input)) })
            .when_some(unit, |r, u| r.child(div().text_sm().text_color(theme.muted_foreground).child(u)))
            .into_any_element(),
        ValueKind::File => {
            let (label, act) = match file {
                FileButton::Choose(f) => ("Choose…", f),
                FileButton::Check(f) => ("Check", f),
            };
            let button = crate::ui::button(SharedString::from(format!("file-{env}"))).outline().label(label).on_click(move |_, window, cx| act(window, cx)).into_any_element();
            let input_el = if table { div().flex_1().min_w_0().child(Input::new(input)) } else { div().flex_1().min_w(px(160.)).max_w(px(420.)).child(Input::new(input)) };
            return (input_el.into_any_element(), Some(button));
        }
        ValueKind::Text => if table { div().flex_1().min_w_0().child(Input::new(input)) } else { div().flex_1().min_w(px(180.)).max_w(px(420.)).child(Input::new(input)) }.into_any_element(),
    };
    (el, None)
}
