// Copyright © 2016-2026 The SENTIENT Authors

//! Settings, in five tabs instead of one list of environment variables:
//!
//! - Platform: log detail, the licence server, analytics, how SENTIENT
//!   starts.
//! - Database: PostgreSQL sized by a preset worked out from this computer's
//!   memory (Custom opens the exact numbers), SENTIENT's connection pools,
//!   and the time-series store's own settings.
//! - Network and certificates: SENTIENT's ports (checked free), the
//!   certificate files for HTTPS, MQTT over TLS and CoAP over DTLS, browser
//!   origins and device claiming.
//! - Data retention: how long audit log, notifications and events are kept,
//!   as plain choices.
//! - Advanced: every variable, grouped and searchable, with its built-in
//!   value and a reset; variables the list does not know can be added.
//!
//! Every tab edits the same values; the bar under the tabs says how many
//! changed and what applying restarts (crate::settings applies them and goes
//! back if SENTIENT does not start).

use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::catalog::{self, Group, Setting};
use crate::installation::{Engine, InstallationRecord};
use crate::jobs::{Job, JobState};
use crate::manager::migration::RecordChanged;
use crate::settings::PORTS;
use crate::ui::{self, Tone};
use sentient_installer_core::stack::Flavor;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Platform,
    Database,
    Network,
    Retention,
    Advanced,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Platform, Tab::Database, Tab::Network, Tab::Retention, Tab::Advanced];
}

/// PostgreSQL sized for the computer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Preset {
    Small,
    Medium,
    Large,
}

impl Preset {
    const ALL: [Preset; 3] = [Preset::Small, Preset::Medium, Preset::Large];

    fn label(self) -> &'static str {
        match self {
            Preset::Small => "Small (up to 50 devices)",
            Preset::Medium => "Medium (recommended)",
            Preset::Large => "Large",
        }
    }
}

/// The settings a preset writes; an empty value is SENTIENT's default.
const PRESET_KEYS: [&str; 7] = [
    "pg:shared_buffers",
    "pg:effective_cache_size",
    "pg:work_mem",
    "pg:max_connections",
    "DATABASE_POOL_MAX",
    "DB_POOL_PRIMARY_MAX",
    "DB_MAX_CONNECTIONS",
];

fn preset_values(p: Preset, groups: &[Group], ram_mb: u64) -> HashMap<&'static str, String> {
    let ram = ram_mb.max(2048);
    let v = |a: String, b: String, c: &str, d: &str, e: &str, f: &str, g: &str| {
        PRESET_KEYS.iter().copied().zip([a, b, c.to_string(), d.to_string(), e.to_string(), f.to_string(), g.to_string()]).collect()
    };
    match p {
        Preset::Small => v(format!("{}MB", ram / 8), format!("{}MB", ram / 2), "8MB", "100", "60", "16", "80"),
        Preset::Medium => {
            let rec = catalog::recommended(groups, ram);
            let get = |k: &str| rec.get(k).cloned().unwrap_or_default();
            v(get("pg:shared_buffers"), get("pg:effective_cache_size"), &get("pg:work_mem"), &get("pg:max_connections"), "", "", "")
        }
        Preset::Large => v(format!("{}MB", ram / 4), format!("{}MB", ram * 3 / 4), "32MB", "400", "300", "64", "300"),
    }
}

/// A choice offered as buttons: (label, value). An empty value is the
/// built-in one.
type Choices = &'static [(&'static str, &'static str)];

/// The Value column of Advanced: every field starts and ends at the same place.
pub(crate) const VALUE_W: f32 = 340.;

const LOG_LEVELS: Choices = &[("Errors only", "error"), ("Normal", "info"), ("Detailed", "debug")];
const KEEP_DAYS: Choices = &[("Forever", "0"), ("30 days", "30"), ("90 days", "90"), ("1 year", "365")];
const KEEP_EVENTS: Choices = &[("Built-in", ""), ("7 days", "604800"), ("30 days", "2592000"), ("90 days", "7776000")];
const KEEP_DEBUG: Choices = &[("Built-in", ""), ("1 day", "86400"), ("7 days", "604800"), ("30 days", "2592000")];

const CERTS: [(&str, &str); 7] = [
    ("SSL_PEM_CERT", "HTTPS certificate"),
    ("SSL_PEM_KEY", "HTTPS private key"),
    ("MQTT_SSL_PEM_CERT", "MQTT over TLS certificate"),
    ("MQTT_SSL_PEM_KEY", "MQTT over TLS key"),
    ("MQTT_SSL_PEM_CA", "MQTT over TLS CA"),
    ("COAP_SSL_PEM_CERT", "CoAP over DTLS certificate"),
    ("COAP_SSL_PEM_KEY", "CoAP over DTLS key"),
];

pub struct SettingsPage {
    record: Option<InstallationRecord>,
    groups: Vec<Group>,
    /// The values in effect, by catalog name (and `port:*`).
    applied: HashMap<String, String>,
    inputs: HashMap<String, Entity<InputState>>,
    /// Variables the catalog does not know: (name, value) inputs.
    custom: Vec<(Entity<InputState>, Entity<InputState>)>,
    custom_applied: Vec<(String, String)>,
    new_name: Entity<InputState>,
    new_value: Entity<InputState>,
    search: Entity<InputState>,
    tab: Tab,
    /// Advanced: the group shown (None: all), and only the ones set here.
    group: Option<String>,
    changed_only: bool,
    show_names: bool,
    /// Database: the exact numbers are open.
    custom_db: bool,
    ram_mb: u64,
    job: Entity<Job>,
    /// System administrators and the default retention, read through
    /// SENTIENT's own command (crate::sentient_admin).
    admins: Option<Result<Vec<crate::sentient_admin::Admin>, String>>,
    retention_now: Option<Result<crate::sentient_admin::Retention, String>>,
    retention_edit: crate::sentient_admin::Retention,
    reading_admin: bool,
    /// The account whose password form is open.
    reset_for: Option<String>,
    new_pw: Entity<InputState>,
    repeat_pw: Entity<InputState>,
    /// The last administration result.
    admin_result: Option<(Tone, String)>,
    admin_busy: bool,
    _subscriptions: Vec<Subscription>,
    _admin_tasks: Vec<Task<()>>,
}

impl EventEmitter<RecordChanged> for SettingsPage {}

impl SettingsPage {
    pub fn new(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let groups = catalog::groups();
        let applied = record.as_ref().map(crate::settings::current).unwrap_or_default();
        let mut subscriptions = Vec::new();
        let mut inputs = HashMap::new();
        let keys: Vec<(String, String)> = groups
            .iter()
            .flat_map(|g| g.settings.iter())
            .map(|s| (s.env.clone(), s.default.clone()))
            .chain(PORTS.iter().map(|(k, _)| (k.to_string(), String::new())))
            .collect();
        for (key, placeholder) in keys {
            let value = applied.get(&key).cloned().unwrap_or_default();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
            subscriptions.push(cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()));
            inputs.insert(key, input);
        }
        let custom_applied = record.as_ref().map(crate::settings::custom).unwrap_or_default();
        let custom = custom_applied
            .iter()
            .map(|(n, v)| {
                let (n, v) = (n.clone(), v.clone());
                let name = cx.new(|cx| InputState::new(window, cx).default_value(n));
                let value = cx.new(|cx| InputState::new(window, cx).default_value(v));
                subscriptions.push(cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()));
                subscriptions.push(cx.subscribe(&value, |_, _, _: &InputEvent, cx| cx.notify()));
                (name, value)
            })
            .collect();
        let new_name = cx.new(|cx| InputState::new(window, cx).placeholder("NAME_OF_THE_VARIABLE"));
        let new_value = cx.new(|cx| InputState::new(window, cx).placeholder("value"));
        subscriptions.push(cx.subscribe(&new_name, |_, _, _: &InputEvent, cx| cx.notify()));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings and variables"));
        subscriptions.push(cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()));
        let job = cx.new(|_| Job::new());
        subscriptions.push(cx.observe(&job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_)) {
                page.record = InstallationRecord::load();
                page.applied = page.record.as_ref().map(crate::settings::current).unwrap_or_default();
                page.custom_applied = page.record.as_ref().map(crate::settings::custom).unwrap_or_default();
                cx.emit(RecordChanged);
            }
            cx.notify();
        }));
        let new_pw = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("At least 8 characters"));
        let repeat_pw = cx.new(|cx| InputState::new(window, cx).masked(true));
        subscriptions.push(cx.subscribe(&new_pw, |_, _, _: &InputEvent, cx| cx.notify()));
        subscriptions.push(cx.subscribe(&repeat_pw, |_, _, _: &InputEvent, cx| cx.notify()));
        let mut page = Self {
            admins: None,
            retention_now: None,
            retention_edit: Default::default(),
            reading_admin: false,
            reset_for: None,
            new_pw,
            repeat_pw,
            admin_result: None,
            admin_busy: false,
            _admin_tasks: Vec::new(),
            record,
            groups,
            applied,
            inputs,
            custom,
            custom_applied,
            new_name,
            new_value,
            search,
            tab: Tab::Platform,
            group: None,
            changed_only: false,
            show_names: false,
            custom_db: false,
            ram_mb: catalog::total_ram_mb(),
            job,
            _subscriptions: subscriptions,
        };
        // Development aid: open on a tab (screens).
        #[cfg(debug_assertions)]
        if let Ok(t) = std::env::var("SPM_SETTINGS_TAB") {
            page.tab = Tab::ALL.into_iter().find(|x| format!("{x:?}").eq_ignore_ascii_case(&t)).unwrap_or(Tab::Platform);
        }
        page.custom_db = page.preset(cx).is_none();
        // Read without asking where that needs no administrator password.
        if page.record.as_ref().is_some_and(|r| !crate::rootjob::needed(r)) {
            page.read_admin(cx);
        }
        page
    }

    /// Read the administrators and the default retention (background).
    fn read_admin(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.reading_admin = true;
        let task = cx.spawn(async move |this, cx| {
            let (admins, retention) = cx
                .background_executor()
                .spawn(async move { (crate::sentient_admin::admins(&record), crate::sentient_admin::retention(&record)) })
                .await;
            this.update(cx, |page, cx| {
                if let Ok(r) = &retention {
                    page.retention_edit = *r;
                }
                page.admins = Some(admins);
                page.retention_now = Some(retention);
                page.reading_admin = false;
                cx.notify();
            })
            .ok();
        });
        self._admin_tasks.push(task);
        cx.notify();
    }

    /// Run one administration action (background), then read again.
    fn admin_action(&mut self, work: impl FnOnce(&InstallationRecord) -> Result<String, String> + Send + 'static, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.admin_busy = true;
        self.admin_result = None;
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { work(&record) }).await;
            this.update(cx, |page, cx| {
                page.admin_busy = false;
                page.admin_result = Some(match r {
                    Ok(m) => (Tone::Good, m.lines().last().unwrap_or("Done.").to_string()),
                    Err(e) => (Tone::Bad, e),
                });
                if page.admin_result.as_ref().is_some_and(|(t, _)| matches!(t, Tone::Good)) {
                    page.reset_for = None;
                }
                page.read_admin(cx);
            })
            .ok();
        });
        self._admin_tasks.push(task);
        cx.notify();
    }

    fn reset_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(email) = self.reset_for.clone() else { return };
        let pw = self.new_pw.read(cx).value().to_string();
        self.new_pw.update(cx, |s, cx| s.set_value("", window, cx));
        self.repeat_pw.update(cx, |s, cx| s.set_value("", window, cx));
        self.admin_action(move |r| crate::sentient_admin::reset_password(r, &email, &pw), cx);
    }

    fn confirm_sign_out_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title("Sign everyone out?")
                .description("Every open session, on every device, has to sign in again within a few seconds. SENTIENT keeps running. It is written to SENTIENT's audit log.")
                .confirm()
                .ok_text("Sign everyone out")
                .on_ok(move |_, _, cx| {
                    this.update(cx, |page, cx| page.admin_action(crate::sentient_admin::sign_out_all, cx)).ok();
                    true
                })
        });
    }

    fn value(&self, key: &str, cx: &App) -> String {
        self.inputs.get(key).map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default()
    }

    fn set(&self, key: &str, value: &str, window: &mut Window, cx: &mut App) {
        if let Some(input) = self.inputs.get(key) {
            let value = value.to_string();
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    /// The values in the fields (empty ones left out).
    fn values(&self, cx: &App) -> HashMap<String, String> {
        self.inputs
            .iter()
            .filter_map(|(name, input)| {
                let v = input.read(cx).value().trim().to_string();
                (!v.is_empty()).then(|| (name.clone(), v))
            })
            .collect()
    }

    fn custom_values(&self, cx: &App) -> Vec<(String, String)> {
        self.custom
            .iter()
            .map(|(n, v)| (n.read(cx).value().trim().to_string(), v.read(cx).value().to_string()))
            .filter(|(n, _)| !n.is_empty())
            .collect()
    }

    fn is_changed(&self, key: &str, cx: &App) -> bool {
        self.value(key, cx) != self.applied.get(key).cloned().unwrap_or_default()
    }

    fn changed(&self, cx: &App) -> Vec<String> {
        let mut names: Vec<String> = self.inputs.keys().filter(|k| self.is_changed(k, cx)).cloned().collect();
        names.sort();
        if self.custom_values(cx) != self.custom_applied {
            names.push("added variables".into());
        }
        names
    }

    /// The preset the current values match, if any.
    fn preset(&self, cx: &App) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| {
            let want = preset_values(*p, &self.groups, self.ram_mb);
            PRESET_KEYS.iter().all(|k| self.value(k, cx) == want.get(k).cloned().unwrap_or_default())
        })
    }

    fn apply_preset(&mut self, p: Preset, window: &mut Window, cx: &mut Context<Self>) {
        for (k, v) in preset_values(p, &self.groups, self.ram_mb) {
            self.set(k, &v, window, cx);
        }
        self.custom_db = false;
        cx.notify();
    }

    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (name, input) in &self.inputs {
            let value = self.applied.get(name).cloned().unwrap_or_default();
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.custom = self
            .custom_applied
            .clone()
            .into_iter()
            .map(|(n, v)| {
                let name = cx.new(|cx| InputState::new(window, cx).default_value(n));
                let value = cx.new(|cx| InputState::new(window, cx).default_value(v));
                (name, value)
            })
            .collect();
        cx.notify();
    }

    fn add_variable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.new_name.read(cx).value().trim().to_string();
        let value = self.new_value.read(cx).value().to_string();
        if !crate::settings::valid_name(&name) {
            return;
        }
        // A variable the list knows is edited where it is.
        if self.inputs.contains_key(&name) {
            self.set(&name, &value, window, cx);
        } else {
            let n = cx.new(|cx| InputState::new(window, cx).default_value(name));
            let v = cx.new(|cx| InputState::new(window, cx).default_value(value));
            self._subscriptions.push(cx.subscribe(&v, |_, _, _: &InputEvent, cx| cx.notify()));
            self.custom.push((n, v));
        }
        self.new_name.update(cx, |s, cx| s.set_value("", window, cx));
        self.new_value.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn choose_file(&mut self, key: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) {
        let key: String = key.into();
        let start = crate::chooser::start_for(&self.value(&key, cx), None);
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose the file", start: start.as_deref(), directory: false },
            PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use this file".into()) },
        );
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let text = path.display().to_string();
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |page, cx| {
                    page.set(&key, &text, window, cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn confirm_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let values = self.values(cx);
        let custom = self.custom_values(cx);
        let new_stack = crate::settings::with_custom(&crate::settings::with_values(&record.stack, &values), &custom);
        let Some(restarts) = crate::settings::restarts(&record.stack, &new_stack) else { return };
        let changed = self.changed(cx).len();
        let this = cx.entity().downgrade();
        let text = format!(
            "{changed} setting(s) change. {restarts}, so SENTIENT is unavailable for a minute or so. If it does not start with the new settings, the previous ones are put back."
        );
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            let record = record.clone();
            let values = values.clone();
            let custom = custom.clone();
            dialog.title("Apply the new settings?").description(text.clone()).confirm().ok_text("Apply").on_ok(move |_, _, cx| {
                let (record, values, custom) = (record.clone(), values.clone(), custom.clone());
                this.update(cx, |page, cx| {
                    page.job.update(cx, |job, cx| {
                        if crate::rootjob::needed(&record) {
                            // The root job reads the values from a file;
                            // added variables travel as `custom:<NAME>`.
                            let path = std::env::temp_dir().join(format!("spm-settings-{}.json", std::process::id()));
                            let mut all = values.clone();
                            for (n, v) in &custom {
                                all.insert(format!("{}{n}", crate::settings::CUSTOM_PREFIX), v.clone());
                            }
                            let written = serde_json::to_string(&all)
                                .map_err(|e| e.to_string())
                                .and_then(|j| std::fs::write(&path, j).map_err(|e| e.to_string()));
                            let args = vec!["apply-settings".to_string(), path.display().to_string()];
                            job.start(
                                move |sink| {
                                    written?;
                                    let result = crate::rootjob::run(&args, &sink).map(|_| ());
                                    let _ = std::fs::remove_file(&path);
                                    result
                                },
                                cx,
                            )
                        } else {
                            job.start(move |sink| crate::settings::apply_with(record, &values, Some(&custom), sink).map(|_| ()), cx)
                        }
                    });
                })
                .ok();
                true
            })
        });
    }
}

// ---------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------

/// A row: label (and its help), then the control.
pub(crate) fn row(theme: &gpui_kit::component::Theme, label: impl Into<SharedString>, help: Option<String>, changed: bool, control: impl IntoElement) -> Div {
    // The mockup's setting row: the label on the left (primary when
    // changed), the control right-aligned in a fixed column.
    let label: SharedString = label.into();
    // On a narrow window the control moves under the label, right-aligned.
    h_flex()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .items_center()
        .py_2()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .flex_1()
                .min_w(px(160.))
                .gap_1()
                .items_center()
                .child(div().text_sm().when(changed, |d| d.font_weight(FontWeight::MEDIUM).text_color(theme.primary)).child(label.clone()))
                .when_some(help, |r, h| {
                    r.child(crate::ui::button(SharedString::from(format!("help-{label}"))).ghost().xsmall().icon(IconName::Info).tooltip(h))
                }),
        )
        .child(h_flex().flex_shrink_0().ml_auto().max_w(px(460.)).gap_2().items_center().justify_end().child(control))
}

/// A setting row with its variable name muted beside the plain label (the
/// exact numbers under Custom).
pub(crate) fn row_with_var(theme: &gpui_kit::component::Theme, label: impl Into<SharedString>, var: &str, help: Option<String>, changed: bool, control: impl IntoElement) -> Div {
    let label: SharedString = label.into();
    let var = var.trim_start_matches("pg:").to_string();
    // On a narrow window the control moves under the label, right-aligned.
    h_flex()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .items_center()
        .py_2()
        .border_t_1()
        .border_color(theme.border)
        // The label with its (i), the variable name on a muted line under it.
        .child(
            v_flex()
                .flex_1()
                .min_w(px(160.))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().text_sm().when(changed, |d| d.font_weight(FontWeight::MEDIUM).text_color(theme.primary)).child(label.clone()))
                        .when_some(help, |r, h| r.child(crate::ui::button(SharedString::from(format!("help-{label}"))).ghost().xsmall().icon(IconName::Info).tooltip(h))),
                )
                .child(div().text_size(px(12.)).font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(var)),
        )
        .child(h_flex().flex_shrink_0().ml_auto().max_w(px(460.)).gap_2().items_center().justify_end().child(control))
}

/// A row of the All variables table: label column, then value and built-in
/// columns (crate::manager::settings::table_header).
pub(crate) fn table_row(theme: &gpui_kit::component::Theme, label: impl Into<SharedString>, help: Option<String>, changed: bool, control: impl IntoElement) -> Div {
    let label: SharedString = label.into();
    h_flex()
        .gap_3()
        .items_center()
        .py_1p5()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .w(px(LABEL_W))
                .min_w(px(120.))
                .flex_shrink(1.)
                .gap_1()
                .items_center()
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().when(changed, |d| d.font_weight(FontWeight::MEDIUM).text_color(theme.primary)).child(label.clone()))
                .when_some(help, |r, h| {
                    r.child(crate::ui::button(SharedString::from(format!("help-{label}"))).ghost().xsmall().icon(IconName::Info).tooltip(h))
                }),
        )
        .child(h_flex().flex_1().min_w_0().gap_2().items_center().child(control))
}

pub(crate) fn section(theme: &gpui_kit::component::Theme, title: &'static str, about: Option<&'static str>) -> Div {
    ui::panel(theme)
        .gap_1()
        .child(div().font_weight(FontWeight::MEDIUM).child(title))
        .when_some(about, |p, a| p.child(div().pb_1().text_sm().text_color(theme.muted_foreground).child(a)))
}

impl SettingsPage {
    fn find(&self, key: &str) -> Option<&Setting> {
        self.groups.iter().flat_map(|g| g.settings.iter()).find(|s| s.env == key)
    }

    /// A catalog setting's help with its variable name, for the (i).
    fn help(&self, key: &str) -> Option<String> {
        let s = self.find(key)?;
        let default = if s.default.is_empty() { "SENTIENT's own".to_string() } else { s.default.clone() };
        Some(format!("{}\n\nVariable: {}. Built-in value: {default}.", s.help, s.env))
    }

    /// Buttons for a choice; the one matching the field is selected. A value
    /// that matches none shows as "Custom: …".
    /// A yes/no setting as a switch (crate::manager::setting_field).
    fn switch(&self, key: &'static str, cx: &mut Context<Self>) -> AnyElement {
        match self.find(key).cloned() {
            Some(st) => self.field(&st, cx),
            None => div().into_any_element(),
        }
    }

    fn choice(&self, key: &'static str, choices: Choices, cx: &mut Context<Self>) -> Div {
        let now = self.value(key, cx);
        let builtin = self.find(key).map(|s| s.default.clone()).unwrap_or_default();
        // The value in effect: the field, else the built-in one. A log filter
        // ("info,sqlx=warn") counts by its overall level.
        let effective = if now.is_empty() { builtin.clone() } else { now.clone() };
        let level = |v: &str| v.split(',').next().unwrap_or("").trim().to_string();
        let exact = choices.iter().position(|(_, v)| !v.is_empty() && (*v == effective || (key == "RUST_LOG" && *v == level(&effective))));
        let selected_at = exact.or_else(|| if now.is_empty() { choices.iter().position(|(_, v)| v.is_empty()) } else { None });
        let selected = |v: &str| selected_at.is_some_and(|i| choices[i].1 == v);
        let any = selected_at.is_some();
        let now = if now.is_empty() { builtin } else { now };
        let muted = cx.theme().muted_foreground;
        let theme = cx.theme().clone();
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(ui::seg_track(&theme).children(choices.iter().map(|(label, value)| {
                ui::seg_item(&theme, SharedString::from(format!("{key}-{value}")), *label, selected(value)).on_click(cx.listener(move |this, _, window, cx| {
                    this.set(key, value, window, cx);
                    cx.notify();
                }))
            })))
            .when(!any, |r| r.child(div().text_sm().text_color(muted).child(format!("Custom: {now}"))))
    }

    fn input(&self, key: &str, width: f32) -> Div {
        div().w(px(width)).child(Input::new(&self.inputs[key]))
    }

    fn catalog_row(&self, theme: &gpui_kit::component::Theme, key: &'static str, width: f32, cx: &mut Context<Self>) -> Div {
        let Some(setting) = self.find(key).cloned() else {
            return row(theme, key, None, false, self.input(key, width));
        };
        let changed = self.is_changed(key, cx);
        row(theme, setting.short_label(), self.help(key), changed, self.field(&setting, cx))
    }

    /// A catalog setting's field, by its kind (crate::manager::setting_field).
    fn field(&self, setting: &crate::catalog::Setting, cx: &mut Context<Self>) -> AnyElement {
        match self.field_parts(setting, false, cx) {
            (f, Some(b)) => h_flex().flex_1().min_w_0().gap_2().items_center().child(f).child(b).into_any_element(),
            (f, None) => f,
        }
    }

    /// The field and its file button apart (the All variables table).
    fn field_parts(&self, setting: &crate::catalog::Setting, table: bool, cx: &mut Context<Self>) -> (AnyElement, Option<AnyElement>) {
        use crate::manager::setting_field::{self, FileButton};
        let Some(input) = self.inputs.get(&setting.env) else { return (div().into_any_element(), None) };
        let this = cx.entity().downgrade();
        let key = setting.env.clone();
        let set: setting_field::SetFn = std::rc::Rc::new(move |v, window, cx| {
            this.update(cx, |page, cx| {
                page.set(&key, v, window, cx);
                cx.notify();
            })
            .ok();
        });
        let this = cx.entity().downgrade();
        let key = setting.env.clone();
        let choose: setting_field::ActFn = std::rc::Rc::new(move |window, cx| {
            this.update(cx, |page, cx| page.choose_file(key.clone(), window, cx)).ok();
        });
        setting_field::field_parts(setting, input, set, FileButton::Choose(choose), table, cx)
    }

    // ---- tabs --------------------------------------------------------------

    fn platform(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let start = crate::self_install::autostart()
            .map(|(on, delay)| if on { format!("With Windows, after {delay} s") } else { "Manual".into() })
            .unwrap_or_else(|| "With the system".into());
        vec![
            self.admin_section(theme, cx).into_any_element(),
            section(theme, "Platform", Some("How SENTIENT runs on this computer."))
                .child(row(theme, "Address users open", Some("Where browsers open SENTIENT. The web port is under Network and certificates.".into()), false, div().text_sm().child(record.url())))
                .child(row(theme, "Log detail", self.help("RUST_LOG"), self.is_changed("RUST_LOG", cx), self.choice("RUST_LOG", LOG_LEVELS, cx)))
                .child(row(theme, "Analytics engine", self.help("ANALYTICS_ENABLED"), self.is_changed("ANALYTICS_ENABLED", cx), self.switch("ANALYTICS_ENABLED", cx)))
                .child(row(theme, "Starts", Some("Start with Windows, the start delay and the operator screen are under Control.".into()), false, div().text_sm().child(start)))
                .into_any_element(),
            section(theme, "Licence", None)
                .child(self.catalog_row(theme, "LICENSE_SERVER_URL", 320., cx))
                .child(self.catalog_row(theme, "LICENSE_HEARTBEAT_INTERVAL_SECS", 120., cx))
                .into_any_element(),
        ]
    }

    /// The system administrators, their password and lockout, and signing
    /// everyone out: all through SENTIENT's own command.
    fn admin_section(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let mut sec = section(
            theme,
            "System administrator",
            Some("For when nobody can sign in. Changes go through SENTIENT's own administration command and are written to its audit log; SENTIENT keeps running."),
        );
        match &self.admins {
            None => {
                sec = sec.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(if self.reading_admin { "Reading the accounts…" } else { "Not read. Reading asks for the administrator password." }))
                        .when(!self.reading_admin, |r| r.child(crate::ui::button("read-admins").outline().label("Read").on_click(cx.listener(|this, _, _, cx| this.read_admin(cx))))),
                );
            }
            Some(Err(e)) => sec = sec.child(ui::outcome(theme, Tone::Warn, e.clone())),
            Some(Ok(list)) => {
                for (i, a) in list.iter().enumerate() {
                    let email = a.email.clone().unwrap_or_default();
                    let locked = a.locked_since.is_some();
                    let state = if locked {
                        format!("Locked after {} failed sign-ins", a.failed_sign_ins)
                    } else if a.failed_sign_ins > 0 {
                        format!("Not locked · {} failed sign-ins", a.failed_sign_ins)
                    } else {
                        "Not locked".to_string()
                    };
                    let last = a
                        .last_sign_in
                        .and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|t| format!(" · last sign-in {}", t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")))
                        .unwrap_or_default();
                    let (e1, e2) = (email.clone(), email.clone());
                    sec = sec.child(row(
                        theme,
                        email.clone(),
                        None,
                        false,
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().text_sm().text_color(if locked { theme.danger } else { theme.muted_foreground }).child(format!("{state}{last}")))
                            .when(locked, |r| {
                                r.child(crate::ui::button(SharedString::from(format!("unlock-{email}"))).outline().label("Unlock").disabled(self.admin_busy).on_click(cx.listener(move |this, _, _, cx| {
                                    let e = e1.clone();
                                    this.admin_action(move |r| crate::sentient_admin::unlock(r, &e), cx);
                                })))
                            })
                            .child(crate::ui::button(SharedString::from(format!("reset-{email}"))).outline().label("Reset password…").disabled(self.admin_busy).on_click(cx.listener(move |this, _, _, cx| {
                                this.reset_for = Some(e2.clone());
                                this.admin_result = None;
                                cx.notify();
                            }))),
                    ).when(i == 0, |r| r));
                }
                sec = sec.child(row(
                    theme,
                    "Everyone signed in",
                    Some("Makes every open session sign in again, on every device.".into()),
                    false,
                    h_flex().child(crate::ui::button("sign-out-all").outline().label("Sign everyone out…").disabled(self.admin_busy).on_click(cx.listener(|this, _, window, cx| this.confirm_sign_out_all(window, cx)))),
                ).when(list.is_empty(), |r| r));
            }
        }
        if let Some(email) = &self.reset_for {
            let pw = self.new_pw.read(cx).value().to_string();
            let again = self.repeat_pw.read(cx).value().to_string();
            let problem = if pw.chars().count() < 8 {
                Some("At least 8 characters.")
            } else if pw != again {
                Some("The two passwords differ.")
            } else {
                None
            };
            sec = sec.child(
                v_flex()
                    .gap_2()
                    .mt_2()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.primary)
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Reset the password of {email}")))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("The new password works at once; the account's open sessions are signed out and it is unlocked."))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(v_flex().gap_1().w(px(240.)).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("New password")).child(Input::new(&self.new_pw).mask_toggle()))
                            .child(v_flex().gap_1().w(px(240.)).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("Repeat it")).child(Input::new(&self.repeat_pw).mask_toggle())),
                    )
                    .when_some(problem.filter(|_| !pw.is_empty()), |c, p| c.child(div().text_sm().text_color(theme.danger).child(p)))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(crate::ui::button("do-reset").primary().label("Reset password").disabled(problem.is_some() || self.admin_busy).on_click(cx.listener(|this, _, window, cx| this.reset_password(window, cx))))
                            .child(crate::ui::button("cancel-reset").outline().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                                this.reset_for = None;
                                cx.notify();
                            }))),
                    ),
            );
        }
        if self.admin_busy {
            sec = sec.child(div().text_sm().text_color(theme.muted_foreground).child("Working…"));
        }
        if let Some((tone, text)) = &self.admin_result {
            sec = sec.child(ui::outcome(theme, *tone, text.clone()));
        }
        sec
    }

    fn database(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let current = self.preset(cx);
        let open_custom = self.custom_db || current.is_none();
        let presets = ui::seg_track(theme)
            .children(Preset::ALL.into_iter().map(|p| {
                ui::seg_item(theme, SharedString::from(format!("preset-{p:?}")), p.label(), current == Some(p) && !self.custom_db)
                    .on_click(cx.listener(move |this, _, window, cx| this.apply_preset(p, window, cx)))
            }))
            .child(ui::seg_item(theme, "preset-custom", "Custom", open_custom).on_click(cx.listener(|this, _, _, cx| {
                this.custom_db = true;
                cx.notify();
            })));
        let shown = |key: &str| {
            let v = self.value(key, cx);
            if v.is_empty() {
                self.find(key).map(|s| if s.default.is_empty() { "SENTIENT's default".to_string() } else { s.default.clone() }).unwrap_or_default()
            } else {
                v
            }
        };
        let pg_changed = PRESET_KEYS.iter().filter(|k| k.starts_with("pg:")).any(|k| self.is_changed(k, cx));
        let mut pg = section(theme, "PostgreSQL memory and connections", None)
            .child(row(theme, "This computer", None, false, div().text_sm().child(format!("{} of memory", crate::dbstatus::bytes_label(self.ram_mb << 20)))))
            .child(row(theme, "Size for", Some("Sets PostgreSQL's memory and connections and SENTIENT's connection pools together. Custom shows the exact numbers.".into()), false, presets));
        if open_custom {
            for k in [
                "pg:shared_buffers",
                "pg:effective_cache_size",
                "pg:work_mem",
                "pg:max_connections",
                "DATABASE_POOL_MAX",
                "DATABASE_POOL_MIN",
                "DB_POOL_PRIMARY_MAX",
                "DB_POOL_PRIMARY_MIN",
                "DB_POOL_REPORT_MAX",
                "DB_POOL_REPORT_MIN",
                "DB_MAX_CONNECTIONS",
            ] {
                pg = pg.child(match self.find(k).cloned() {
                    Some(st) => {
                        let changed = self.is_changed(k, cx);
                        // Every exact field one width.
                        let field = h_flex().w(px(200.)).justify_end().child(self.field(&st, cx));
                        row_with_var(theme, st.short_label(), k, self.help(k), changed, field)
                    }
                    None => self.catalog_row(theme, k, 160., cx),
                });
            }
        } else {
            pg = pg
                .child(row(theme, "Memory for PostgreSQL", self.help("pg:shared_buffers"), self.is_changed("pg:shared_buffers", cx), div().text_sm().child(shown("pg:shared_buffers"))))
                .child(row(theme, "Memory per query", self.help("pg:work_mem"), self.is_changed("pg:work_mem", cx), div().text_sm().child(shown("pg:work_mem"))))
                .child(row(theme, "Connections allowed", self.help("pg:max_connections"), self.is_changed("pg:max_connections", cx), div().text_sm().child(shown("pg:max_connections"))))
                .child(row(theme, "SENTIENT's connection pool", self.help("DATABASE_POOL_MAX"), self.is_changed("DATABASE_POOL_MAX", cx), div().text_sm().child(shown("DATABASE_POOL_MAX"))));
        }
        if pg_changed {
            pg = pg.child(ui::outcome(theme, Tone::Warn, "Changing PostgreSQL's memory or connections restarts PostgreSQL: SENTIENT is offline for about half a minute."));
        }
        let mut out = vec![pg.into_any_element()];
        let ts: Option<(&'static str, Vec<&'static str>)> = match record.flavor() {
            Flavor::Clickhouse => Some(("Telemetry history (ClickHouse)", vec!["CLICKHOUSE_TS_BATCH_SIZE", "CLICKHOUSE_TS_BATCH_MAX_DELAY_MS", "CLICKHOUSE_TS_BATCH_THREADS"])),
            Flavor::Timescaledb => Some(("Telemetry history (TimescaleDB)", vec!["SENTIENT_TS_KV_CHUNK_DAYS", "SENTIENT_TS_KV_COMPRESS_AFTER_DAYS"])),
            _ => None,
        };
        if let Some((title, keys)) = ts {
            let mut s = section(theme, title, None);
            for (i, k) in keys.into_iter().enumerate() {
                s = s.child(self.catalog_row(theme, k, 160., cx).when(i == 0, |r| r));
            }
            out.push(s.into_any_element());
        }
        out.push(
            section(theme, "Where the data is", Some("Sizes, free disk and growth are on Status."))
                .child(row(theme, "PostgreSQL", None, false, div().text_sm().child(if record.engine == Engine::Docker {
                    "In its Docker volume".to_string()
                } else {
                    format!("{}, port {}", record.pg_dir.display(), record.stack.pg_port)
                })))
                .into_any_element(),
        );
        out
    }

    fn network(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut ports = section(theme, "Ports", Some("SENTIENT's own ports. A new port is checked free before it is used."));
        for (i, (key, label)) in PORTS.into_iter().enumerate() {
            let now = self.value(key, cx);
            let changed = self.is_changed(key, cx);
            let busy = changed && now.parse::<u16>().is_ok_and(sentient_installer_core::checks::port_busy);
            ports = ports.child(row(
                theme,
                label,
                None,
                changed,
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(self.input(key, 110.))
                    .when(busy, |r| r.child(ui::pill(theme, Tone::Warn, "in use by another program"))),
            ).when(i == 0, |r| r));
        }
        ports = ports.child(self.catalog_row(theme, "EDGE_GRPC_PORT", 110., cx));
        let mut certs = section(
            theme,
            "Certificates",
            Some("PEM files on this computer. Without the HTTPS pair SENTIENT is served over HTTP; without the MQTT pair, MQTT over TLS is off."),
        );
        for (i, (key, label)) in CERTS.into_iter().enumerate() {
            certs = certs.child(row(
                theme,
                label,
                self.help(key),
                self.is_changed(key, cx),
                h_flex().gap_2().child(self.input(key, 360.)).child(
                    crate::ui::button(SharedString::from(format!("choose-{key}")))
                        .outline()
                        .label("Choose…")
                        .on_click(cx.listener(move |this, _, window, cx| this.choose_file(key, window, cx))),
                ),
            ).when(i == 0, |r| r));
        }
        vec![
            ports.into_any_element(),
            certs.into_any_element(),
            section(theme, "Access", None)
                .child(self.catalog_row(theme, "SENTIENT_CORS_ALLOWED_ORIGINS", 360., cx))
                .child(row(
                    theme,
                    "Device claiming",
                    self.help("SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT"),
                    self.is_changed("SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT", cx),
                    self.switch("SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT", cx),
                ))
                .into_any_element(),
        ]
    }

    fn retention(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let keep = section(theme, "How long data is kept", Some("Older records are deleted by SENTIENT's nightly cleanup."))
            .child(row(theme, "Audit log", self.help("AUDIT_LOG_TTL_DAYS"), self.is_changed("AUDIT_LOG_TTL_DAYS", cx), self.choice("AUDIT_LOG_TTL_DAYS", KEEP_DAYS, cx)))
            .child(row(theme, "Notifications", self.help("NOTIFICATIONS_TTL_DAYS"), self.is_changed("NOTIFICATIONS_TTL_DAYS", cx), self.choice("NOTIFICATIONS_TTL_DAYS", KEEP_DAYS, cx)))
            .child(row(
                theme,
                "Events",
                self.help("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS"),
                self.is_changed("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS", cx),
                self.choice("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS", KEEP_EVENTS, cx),
            ))
            .child(row(
                theme,
                "Debug events",
                self.help("SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS"),
                self.is_changed("SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS", cx),
                self.choice("SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS", KEEP_DEBUG, cx),
            ))
;
        let keep = keep.child(self.history_rows(theme, cx));
        let cleanup = section(theme, "Cleanup", None)
            .child(row(theme, "Audit log cleanup", self.help("AUDIT_LOG_CLEANUP_ENABLED"), self.is_changed("AUDIT_LOG_CLEANUP_ENABLED", cx), self.switch("AUDIT_LOG_CLEANUP_ENABLED", cx)))
            .child(row(theme, "Event cleanup", self.help("SENTIENT__EVENT_CLEANUP__ENABLED"), self.is_changed("SENTIENT__EVENT_CLEANUP__ENABLED", cx), self.switch("SENTIENT__EVENT_CLEANUP__ENABLED", cx)));
        vec![keep.into_any_element(), cleanup.into_any_element()]
    }

    /// Telemetry history and alarms: the default tenant profile's retention,
    /// through SENTIENT's own command, saved on its own.
    fn history_rows(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        const DAYS: [(&str, i64); 5] = [("Forever", 0), ("30 days", 30), ("90 days", 90), ("1 year", 365), ("3 years", 1095)];
        let help = "For tenants that use the default tenant profile (a tenant with its own profile keeps its own). Older values are deleted by SENTIENT's nightly cleanup.";
        match &self.retention_now {
            None => v_flex().child(row(
                theme,
                "Telemetry history and alarms",
                Some(help.into()),
                false,
                h_flex()
                    .gap_2()
                    .child(div().text_sm().text_color(theme.muted_foreground).child(if self.reading_admin { "Reading…" } else { "Not read." }))
                    .when(!self.reading_admin, |r| r.child(crate::ui::button("read-retention").outline().label("Read").on_click(cx.listener(|this, _, _, cx| this.read_admin(cx))))),
            )),
            Some(Err(e)) => v_flex().child(row(theme, "Telemetry history and alarms", Some(help.into()), false, div().text_sm().text_color(theme.muted_foreground).child(e.clone()))),
            Some(Ok(now)) => {
                let now = *now;
                let edit = self.retention_edit;
                let pick = |id: &'static str, current: i64, telemetry: bool, cx: &mut Context<Self>| {
                    h_flex().gap_1().flex_wrap().children(DAYS.iter().map(move |(label, d)| {
                        let d = *d;
                        let b = crate::ui::button(SharedString::from(format!("{id}-{d}"))).label(*label).on_click(cx.listener(move |this, _, _, cx| {
                            if telemetry { this.retention_edit.telemetry_days = d } else { this.retention_edit.alarms_days = d }
                            cx.notify();
                        }));
                        if current == d { b.primary() } else { b.outline() }
                    }))
                };
                let changed = edit != now;
                v_flex()
                    .child(row(theme, "Telemetry history", Some(help.into()), edit.telemetry_days != now.telemetry_days, pick("ret-tel", edit.telemetry_days, true, cx)))
                    .child(row(theme, "Alarms", Some(help.into()), edit.alarms_days != now.alarms_days, pick("ret-alarm", edit.alarms_days, false, cx)))
                    .when(changed, |c| {
                        c.child(
                            h_flex()
                                .gap_2()
                                .pt_1()
                                .child(crate::ui::button("save-retention").primary().label("Save").disabled(self.admin_busy).on_click(cx.listener(move |this, _, _, cx| {
                                    let r = this.retention_edit;
                                    this.admin_action(move |rec| crate::sentient_admin::set_retention(rec, r), cx);
                                })))
                                .child(crate::ui::button("undo-retention").outline().label("Discard").on_click(cx.listener(move |this, _, _, cx| {
                                    this.retention_edit = now;
                                    cx.notify();
                                }))),
                        )
                    })
            }
        }
    }

    fn advanced(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let needle = self.search.read(cx).value().to_lowercase();
        let set_here = self.groups.iter().flat_map(|g| g.settings.iter()).filter(|s| !self.value(&s.env, cx).is_empty() || self.is_changed(&s.env, cx)).count();
        let total: usize = self.groups.iter().map(|g| g.settings.len()).sum();
        let nav_item = |id: SharedString, text: String, active: bool| {
            h_flex()
                .id(id)
                .px_3()
                .py_1p5()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .when(active, |d| d.bg(theme.accent).text_color(theme.primary))
                .when(!active, |d| d.text_color(theme.muted_foreground))
                .child(text)
        };
        let groups_nav = v_flex()
            .w(px(210.))
            .flex_shrink_0()
            .gap_0p5()
            .child(nav_item("group-all".into(), format!("All · {total}"), self.group.is_none()).on_click(cx.listener(|this, _, _, cx| {
                this.group = None;
                cx.notify();
            })))
            .children(self.groups.iter().map(|g| {
                let name = g.name.clone();
                nav_item(SharedString::from(format!("group-{}", g.name)), format!("{} · {}", g.name, g.settings.len()), self.group.as_deref() == Some(&g.name))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.group = Some(name.clone());
                        cx.notify();
                    }))
            }));
        let mut table = ui::panel(theme).gap_0().flex_1().min_w_0().overflow_hidden().child(table_header(theme));
        let mut rows = 0;
        for g in self.groups.iter().filter(|g| self.group.as_ref().is_none_or(|n| n == &g.name)) {
            for s in &g.settings {
                let changed = self.is_changed(&s.env, cx);
                let set = !self.value(&s.env, cx).is_empty();
                if self.changed_only && !(changed || set) {
                    continue;
                }
                if !needle.is_empty() && !format!("{} {} {} {}", s.label, s.env, g.name, s.help).to_lowercase().contains(&needle) {
                    continue;
                }
                rows += 1;
                let key = s.env.clone();
                let label: SharedString = if self.show_names { s.env.clone().into() } else { s.short_label().into() };
                let (field, action) = self.field_parts(s, true, cx);
                table = table.child(table_row(
                    theme,
                    label,
                    self.help(&s.env),
                    changed,
                    table_cells(
                        theme,
                        &s.env,
                        field,
                        action,
                        builtin_label(s),
                        set.then(|| {
                            crate::ui::button(SharedString::from(format!("reset-{}", s.env)))
                                .ghost()
                                .label("Reset")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set(&key, "", window, cx);
                                    cx.notify();
                                }))
                                .into_any_element()
                        }),
                    ),
                ));
            }
        }
        if rows == 0 {
            table = table.child(div().py_3().text_sm().text_color(theme.muted_foreground).child("Nothing matches."));
        }
        let mut added = section(theme, "Added variables", Some("Variables SENTIENT reads that are not in the list above. For support and special cases."));
        for (i, (n, v)) in self.custom.iter().enumerate() {
            added = added.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .py_1()
                    .child(div().w(px(250.)).child(Input::new(n)))
                    .child(div().w(px(260.)).child(Input::new(v)))
                    .child(crate::ui::button(SharedString::from(format!("remove-var-{i}"))).ghost().label("Remove").on_click(cx.listener(move |this, _, _, cx| {
                        if i < this.custom.len() {
                            this.custom.remove(i);
                        }
                        cx.notify();
                    }))),
            );
        }
        let name_ok = crate::settings::valid_name(&self.new_name.read(cx).value());
        added = added.child(
            h_flex()
                .gap_2()
                .items_center()
                .pt_1()
                .child(div().w(px(250.)).child(Input::new(&self.new_name)))
                .child(div().w(px(260.)).child(Input::new(&self.new_value)))
                .child(crate::ui::button("add-var").outline().label("Add a variable").disabled(!name_ok).on_click(cx.listener(|this, _, window, cx| this.add_variable(window, cx)))),
        );
        let filter = |id: &'static str, text: String, on: bool, only: bool| {
            let b = crate::ui::button(id).label(text).on_click(cx.listener(move |this, _, _, cx| {
                this.changed_only = only;
                cx.notify();
            }));
            if on { b.primary() } else { b.outline() }
        };
        vec![
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().child(Input::new(&self.search).cleanable(true)))
                .child(filter("filter-all", format!("All {total}"), !self.changed_only, false))
                .child(filter("filter-set", format!("Set {set_here}"), self.changed_only, true))
                .child(
                    crate::ui::button("show-names")
                        .ghost()
                        .label(if self.show_names { "Show plain names" } else { "Show variable names" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_names = !this.show_names;
                            cx.notify();
                        })),
                )
                .into_any_element(),
            h_flex().gap_4().items_start().child(groups_nav).child(table).into_any_element(),
            added.into_any_element(),
        ]
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(&theme, "Platform settings", "How SENTIENT runs on this computer. Change a value and apply it; only what changed restarts.");
        let Some(record) = self.record.clone() else {
            return page.child(ui::not_installed(&theme));
        };
        let job = self.job.read(cx);
        let busy = job.is_running();
        let changed = self.changed(cx);
        let mut problems = crate::settings::check(&self.values(cx));
        for (n, _) in self.custom_values(cx) {
            if !crate::settings::valid_name(&n) {
                problems.push(format!("{n}: not a usable variable name (letters, digits and _)"));
            }
        }

        let this = cx.entity().downgrade();
        let tabs = tab_bar(&theme, "", Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0), std::rc::Rc::new(move |i, cx| {
            this.update(cx, |p, cx| {
                p.tab = Tab::ALL[i];
                cx.notify();
            })
            .ok();
        }));

        let applybar = (!changed.is_empty()).then(|| {
            let values = self.values(cx);
            let stack = crate::settings::with_custom(&crate::settings::with_values(&record.stack, &values), &self.custom_values(cx));
            let restarts = crate::settings::restarts(&record.stack, &stack).unwrap_or("Nothing restarts");
            h_flex()
                .gap_3()
                .items_center()
                .px_4()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.accent)
                .border_1()
                .border_color(theme.primary)
                .child(div().flex_1().text_sm().child(format!(
                    "{} change{} · {}",
                    changed.len(),
                    if changed.len() == 1 { "" } else { "s" },
                    restarts
                )))
                .child(crate::ui::button("revert").outline().label("Discard").disabled(busy).on_click(cx.listener(|this, _, window, cx| this.revert(window, cx))))
                .child(
                    crate::ui::button("apply")
                        .primary()
                        .label("Apply")
                        .disabled(busy || !problems.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| this.confirm_apply(window, cx))),
                )
        });

        let body = match self.tab {
            Tab::Platform => self.platform(&theme, &record, cx),
            Tab::Database => self.database(&theme, &record, cx),
            Tab::Network => self.network(&theme, cx),
            Tab::Retention => self.retention(&theme, cx),
            Tab::Advanced => self.advanced(&theme, cx),
        };

        let active = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let _ = page;
        layout(&theme, tabs, active, v_flex()
            .gap(px(14.))
            .children(applybar)
            .children(problems.into_iter().map(|p| div().text_sm().text_color(theme.danger).child(p)))
            
            .children(body))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{PRESET_KEYS, Preset, preset_values};

    #[test]
    fn presets_set_every_key_and_grow_with_size() {
        let groups = crate::catalog::groups();
        for p in Preset::ALL {
            assert_eq!(preset_values(p, &groups, 16384).len(), PRESET_KEYS.len());
        }
        let small = preset_values(Preset::Small, &groups, 16384);
        let large = preset_values(Preset::Large, &groups, 16384);
        assert_eq!(small["pg:shared_buffers"], "2048MB");
        assert_eq!(large["pg:shared_buffers"], "4096MB");
        // Medium is the install's recommendation; pools keep SENTIENT's.
        let medium = preset_values(Preset::Medium, &groups, 16384);
        assert_eq!(medium["pg:shared_buffers"], "4096MB");
        assert_eq!(medium["DATABASE_POOL_MAX"], "");
    }
}

/// The tab titles of Platform settings, in order.
pub(crate) const TAB_TITLES: [&str; 5] = ["General", "Database", "Network and certificates", "History and cleanup", "All variables"];

/// What each category holds, under its title.
pub(crate) const TAB_ABOUT: [&str; 5] = [
    "How SENTIENT runs, its administrator account and the licence.",
    "PostgreSQL's memory and connections, and the telemetry history store.",
    "SENTIENT's ports, certificates and which sites may call it.",
    "How long data is kept and the nightly cleanup.",
    "Every setting SENTIENT reads, with its built-in value.",
];

/// The catalog settings each of the first four tabs shows on a remote
/// server (this computer's tabs add what only it can do).
pub(crate) const TAB_KEYS: [&[&str]; 4] = [
    &["RUST_LOG", "ANALYTICS_ENABLED", "LICENSE_SERVER_URL", "LICENSE_HEARTBEAT_INTERVAL_SECS"],
    &["DATABASE_POOL_MAX", "DATABASE_POOL_MIN", "DB_POOL_PRIMARY_MAX", "DB_POOL_PRIMARY_MIN", "DB_POOL_REPORT_MAX", "DB_POOL_REPORT_MIN", "DB_MAX_CONNECTIONS", "CLICKHOUSE_TS_BATCH_SIZE", "CLICKHOUSE_TS_BATCH_MAX_DELAY_MS", "CLICKHOUSE_TS_BATCH_THREADS", "SENTIENT_TS_KV_CHUNK_DAYS", "SENTIENT_TS_KV_COMPRESS_AFTER_DAYS"],
    &["EDGE_GRPC_PORT", "SSL_PEM_CERT", "SSL_PEM_KEY", "MQTT_SSL_PEM_CERT", "MQTT_SSL_PEM_KEY", "MQTT_SSL_PEM_CA", "COAP_SSL_PEM_CERT", "COAP_SSL_PEM_KEY", "SENTIENT_CORS_ALLOWED_ORIGINS", "SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT"],
    &["AUDIT_LOG_TTL_DAYS", "NOTIFICATIONS_TTL_DAYS", "SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS", "SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS", "AUDIT_LOG_CLEANUP_ENABLED", "SENTIENT__EVENT_CLEANUP__ENABLED"],
];

/// The categories of Platform settings, as a list on the left (the
/// mockup's H2). Shared with a remote server's.
pub(crate) fn tab_bar(theme: &gpui_kit::component::Theme, id: &str, active: usize, pick: std::rc::Rc<dyn Fn(usize, &mut App)>) -> Div {
    v_flex()
        .w(px(190.))
        .min_w(px(132.))
        .flex_shrink(1.)
        .gap_0p5()
        .children(TAB_TITLES.iter().enumerate().map(|(i, title)| {
            let on = i == active;
            let pick = pick.clone();
            div()
                .id(SharedString::from(format!("{id}tab-{i}")))
                .px(px(11.))
                .py(px(7.))
                .rounded(px(7.))
                .text_sm()
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .when(on, |d| d.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.hover(|d| d.bg(theme.list_hover).text_color(theme.foreground)))
                .child(*title)
                .on_click(move |_, _, cx| pick(i, cx))
        }))
}

/// Platform settings laid out as the mockup's H2: the category list, then
/// the open category's title, its line and its panels.
pub(crate) fn layout(theme: &gpui_kit::component::Theme, nav: Div, active: usize, body: impl IntoElement) -> Stateful<Div> {
    h_flex()
        .id("settings-page")
        .flex_1()
        .min_w_0()
        .h_full()
        .items_start()
        .bg(theme.muted)
        .child(v_flex().h_full().p_3().border_r_1().border_color(theme.border).child(nav))
        .child(
            v_flex()
                .id("settings-body")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .p_4()
                .gap(px(14.))
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child(TAB_TITLES[active.min(4)]))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(TAB_ABOUT[active.min(4)])),
                )
                .child(body),
        )
}

/// The Advanced table's header row. Shared with a remote server's.
pub(crate) fn table_header(theme: &gpui_kit::component::Theme) -> Div {
    h_flex()
        .gap_3()
        .pb_1()
        .text_size(px(13.))
        .text_color(theme.muted_foreground)
        .child(div().w(px(LABEL_W)).min_w(px(120.)).flex_shrink(1.).child("Setting"))
        .child(div().flex_1().min_w(px(150.)).max_w(px(VALUE_W)).child("Value"))
        .child(div().w(px(ACTION_W)).flex_shrink_0())
        .child(div().flex_1())
        .child(div().w(px(BUILTIN_W)).flex_shrink_0().text_right().child("Built-in"))
}

/// The table's label and built-in column widths; the value takes the rest
/// up to VALUE_W, so nothing crosses the panel at any window width.
const LABEL_W: f32 = 220.;
const BUILTIN_W: f32 = 120.;
const ACTION_W: f32 = 84.;

/// A table row's value and built-in cells (with `extra`, such as Reset,
/// after the value). Shared with a remote server's table.
pub(crate) fn table_cells(theme: &gpui_kit::component::Theme, key: &str, field: impl IntoElement, action: Option<AnyElement>, builtin: String, extra: Option<AnyElement>) -> Div {
    let tip: SharedString = builtin.clone().into();
    h_flex()
        .flex_1()
        .min_w_0()
        .gap_3()
        .items_center()
        // Every value field one width; a file's Check or Choose in a column
        // of its own (empty where it does not apply).
        .child(h_flex().flex_1().min_w(px(150.)).max_w(px(VALUE_W)).gap_2().items_center().child(h_flex().flex_1().min_w_0().child(field)).children(extra))
        .child(h_flex().w(px(ACTION_W)).flex_shrink_0().children(action))
        .child(div().flex_1())
        .child(
            div()
                .id(SharedString::from(format!("builtin-{key}")))
                .w(px(BUILTIN_W))
                .flex_shrink_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_right()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(builtin)
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)),
        )
}

/// A setting's built-in value, as the Advanced table shows it.
pub(crate) fn builtin_label(s: &crate::catalog::Setting) -> String {
    match (s.kind(), s.default.as_str()) {
        (_, "") => "SENTIENT's own".to_string(),
        (crate::catalog::ValueKind::Switch, "true") => "On".to_string(),
        (crate::catalog::ValueKind::Switch, "false") => "Off".to_string(),
        (crate::catalog::ValueKind::Number(Some(u)), d) => format!("{d} {u}"),
        (_, d) => d.to_string(),
    }
}
