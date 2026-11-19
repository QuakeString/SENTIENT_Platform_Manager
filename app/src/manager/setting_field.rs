// Copyright © 2016-2026 The SENTIENT Authors
//
//! One setting's field, from its kind in the catalog: a switch for on/off, a
//! file path with its button, a short number field with its unit, or text.
//! Shared by Platform settings on this computer and on a remote server.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme,
    input::InputState,
};
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
        "RUST_LOG" => Some(&[(n!("Errors only"), "error"), (n!("Normal"), "info"), (n!("Detailed"), "debug")]),
        "AUDIT_LOG_TTL_DAYS" | "NOTIFICATIONS_TTL_DAYS" => Some(&[(n!("Forever"), "0"), (n!("30 days"), "30"), (n!("90 days"), "90"), (n!("1 year"), "365")]),
        "SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS" => Some(&[(n!("Built-in"), ""), (n!("7 days"), "604800"), (n!("30 days"), "2592000"), (n!("90 days"), "7776000")]),
        "SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS" => Some(&[(n!("Built-in"), ""), (n!("1 day"), "86400"), (n!("7 days"), "604800"), (n!("30 days"), "2592000")]),
        _ => None,
    }
}

/// A setting's control in the field column (crate::ui): a choice of a
/// few values, a switch, or the input with its unit (or the file's button)
/// in its slot and, when the value cannot be used, its message under it.
pub fn control(setting: &Setting, input: &Entity<InputState>, set: SetFn, file: FileButton, window: &Window, cx: &App) -> AnyElement {
    let theme = cx.theme().clone();
    let env = setting.env.clone();
    if let Some(choices) = choices_for(&env) {
        let now = input.read(cx).value().trim().to_string();
        let effective = if now.is_empty() { setting.default.clone() } else { now.clone() };
        // A log filter ("info,sqlx=warn") counts by its overall level.
        let level = effective.split(',').next().unwrap_or("").trim().to_string();
        let at = choices.iter().position(|(_, v)| !v.is_empty() && (*v == effective || *v == level)).or_else(|| if now.is_empty() { choices.iter().position(|(_, v)| v.is_empty()) } else { None });
        let labels: Vec<&str> = choices.iter().map(|(l, _)| crate::i18n::t(l)).collect();
        return ui::chips(&theme, &env, &labels, at, Rc::new(move |i, window, cx| set(choices[i].1, window, cx))).into_any_element();
    }
    match setting.kind() {
        ValueKind::Switch => {
            let now = input.read(cx).value().trim().to_string();
            let builtin = setting.default.clone();
            let effective = if now.is_empty() { builtin.clone() } else { now };
            let on = effective == "true";
            ui::toggle(&theme, SharedString::from(format!("sw-{env}")), on, None, move |v: &bool, window, cx| {
                let want = if *v { "true" } else { "false" };
                // The built-in value is written as "not set".
                set(if builtin == want { "" } else { want }, window, cx);
            })
            .into_any_element()
        }
        kind => {
            let slot = match (kind, file) {
                (ValueKind::File, FileButton::Choose(f)) => Some(ui::Slot::Button(SharedString::from(format!("file-{env}")), t!("Choose").into(), f)),
                (ValueKind::File, FileButton::Check(f)) => Some(ui::Slot::Button(SharedString::from(format!("file-{env}")), t!("Check").into(), f)),
                _ => unit_of(setting).map(|u| ui::Slot::Unit(u.into())),
            };
            ui::field_input(&theme, input, slot, setting.problem(&input.read(cx).value()), false, window, cx).into_any_element()
        }
    }
}

/// The word in a number field's unit slot: seconds, days, MB, port,
/// threads; None for a plain count.
pub fn unit_of(setting: &Setting) -> Option<&'static str> {
    match setting.kind() {
        ValueKind::Number(Some(u)) => Some(ui::unit_word(u)),
        ValueKind::Number(None) if setting.env.ends_with("_PORT") => Some("port"),
        ValueKind::Number(None) if setting.env.ends_with("_THREADS") => Some("threads"),
        _ => None,
    }
}

/// A number of seconds in plain words: 86400 is "24 hours", 604800 "7
/// days"; None when it is not a whole number of minutes.
fn seconds_words(n: u64) -> Option<String> {
    let plural = |n: u64, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
    if n == 0 || n % 60 != 0 {
        return None;
    }
    // A day reads as 24 hours; two and more as days.
    Some(if n % 86_400 == 0 && n / 86_400 >= 2 {
        plural(n / 86_400, "day")
    } else if n % 3600 == 0 {
        plural(n / 3600, "hour")
    } else {
        plural(n / 60, "minute")
    })
}

/// The words a setting's value is shown with: a choice's own label
/// (Forever, Built-in, 30 days), On or Off, a duration in plain words
/// ("Every 24 hours" for an interval), the number with its unit, or the
/// value as written. An empty value is SENTIENT's own.
pub fn value_words(setting: &Setting, value: &str) -> String {
    let v = value.trim();
    if let Some(choices) = choices_for(&setting.env) {
        // A log filter ("info,sqlx=warn") is its own words: the level alone
        // would hide the rest.
        if let Some((label, _)) = choices.iter().find(|(_, c)| *c == v) {
            return crate::i18n::t(label).to_string();
        }
    }
    if v.is_empty() {
        return t!("SENTIENT's own").to_string();
    }
    match setting.kind() {
        ValueKind::Switch if v == "true" => t!("On").to_string(),
        ValueKind::Switch if v == "false" => t!("Off").to_string(),
        ValueKind::Number(Some(u)) => {
            let n = v.parse::<u64>().ok();
            let words = match (u, n) {
                ("s", Some(n)) => seconds_words(n),
                ("min", Some(n)) if n > 0 && n % 60 == 0 => seconds_words(n * 60),
                _ => None,
            };
            match words {
                Some(w) if setting.env.contains("_INTERVAL") => tf!("Every {}", w.strip_prefix("1 ").unwrap_or(&w)),
                Some(w) => w,
                None => format!("{v} {}", ui::unit_word(u)),
            }
        }
        _ => v.to_string(),
    }
}

/// The current value column's words: the value in effect, the built-in one
/// when nothing is set.
pub fn current_words(setting: &Setting, applied: &str) -> String {
    if applied.trim().is_empty() {
        // A choice of "Built-in" says so; otherwise the built-in value.
        if choices_for(&setting.env).is_some_and(|c| c.iter().any(|(_, v)| v.is_empty())) || setting.default.is_empty() {
            return value_words(setting, "");
        }
        return value_words(setting, &setting.default);
    }
    value_words(setting, applied)
}

#[cfg(test)]
mod tests {
    use super::value_words;

    /// The value column says what the choice says: Forever, not "0 days";
    /// Built-in, not "0 s"; numbers with their unit.
    #[test]
    fn value_words_match_the_choices() {
        let all: Vec<crate::catalog::Setting> = crate::catalog::groups().into_iter().flat_map(|g| g.settings).collect();
        let s = |env: &str| all.iter().find(|s| s.env == env).unwrap().clone();
        assert_eq!(value_words(&s("NOTIFICATIONS_TTL_DAYS"), "0"), "Forever");
        assert_eq!(value_words(&s("AUDIT_LOG_TTL_DAYS"), "365"), "1 year");
        assert_eq!(value_words(&s("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS"), ""), "Built-in");
        assert_eq!(value_words(&s("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS"), "604800"), "7 days");
        assert_eq!(value_words(&s("SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS"), "86400"), "1 day");
        // A value no choice has: the number and its unit.
        assert_eq!(value_words(&s("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS"), "1000"), "1000 seconds");
        assert_eq!(value_words(&s("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS"), "1209600"), "14 days");
        assert_eq!(value_words(&s("NOTIFICATIONS_TTL_DAYS"), "12"), "12 days");
        assert_eq!(value_words(&s("RUST_LOG"), "info"), "Normal");
        assert_eq!(value_words(&s("RUST_LOG"), "info,sqlx=warn"), "info,sqlx=warn");
        assert_eq!(value_words(&s("LICENSE_HEARTBEAT_INTERVAL_SECS"), "3600"), "Every hour");
        assert_eq!(value_words(&s("LICENSE_BINDING_CHECK_INTERVAL_SECS"), "86400"), "Every 24 hours");
        assert_eq!(value_words(&s("LICENSE_BINDING_CHECK_INTERVAL_SECS"), "90"), "90 seconds");
        // The current column: nothing set is the built-in value in words.
        use super::current_words;
        assert_eq!(current_words(&s("NOTIFICATIONS_TTL_DAYS"), ""), "Forever");
        assert_eq!(current_words(&s("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS"), ""), "Built-in");
        assert_eq!(current_words(&s("LICENSE_BINDING_CHECK_INTERVAL_SECS"), ""), "Every 24 hours");
        assert_eq!(current_words(&s("LICENSE_SERVER_URL"), ""), "https://license.invenia.in");
        assert_eq!(value_words(&s("AUDIT_LOG_CLEANUP_ENABLED"), "true"), "On");
        assert_eq!(value_words(&s("LICENSE_SERVER_URL"), ""), "SENTIENT's own");
    }
}
