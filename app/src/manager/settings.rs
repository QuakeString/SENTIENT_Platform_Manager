// Copyright © 2016-2026 The SENTIENT Authors

//! Settings, in six tabs instead of one list of environment variables:
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
//! - Startup: the operator screen (open at sign-in, automatic sign-in) and,
//!   on Windows, starting with Windows (crate::manager::startup).
//! - Advanced: every variable, grouped and searchable, with its built-in
//!   value and a reset; variables the list does not know can be added.
//!
//! Every tab edits the same values; the bar under the tabs says how many
//! changed and what applying restarts (crate::settings applies them and goes
//! back if SENTIENT does not start).

use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex,
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
    Startup,
    Advanced,
}

impl Tab {
    const ALL: [Tab; 6] = [Tab::Platform, Tab::Database, Tab::Network, Tab::Retention, Tab::Startup, Tab::Advanced];

    /// Its number in session.json: the remote pages' numbers (0 to 4, All
    /// variables 4), Startup after them, so a saved tab means the same on
    /// both and from before Startup existed.
    fn session_index(self) -> usize {
        match self {
            Tab::Platform => 0,
            Tab::Database => 1,
            Tab::Network => 2,
            Tab::Retention => 3,
            Tab::Advanced => 4,
            Tab::Startup => 5,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Startup => STARTUP_TITLE,
            t => TAB_TITLES[t.session_index()],
        }
    }

    fn about(self) -> &'static str {
        match self {
            Tab::Startup => STARTUP_ABOUT,
            t => TAB_ABOUT[t.session_index()],
        }
    }
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
    /// The Startup tab (crate::manager::startup).
    startup: Entity<super::startup::StartupPanel>,
    /// Advanced: the group shown (None: all), and only the ones set here.
    group: Option<String>,
    changed_only: bool,
    /// Database: the exact numbers are open.
    custom_db: bool,
    ram_mb: u64,
    job: Entity<Job>,
    /// System administrators and the default retention, read through
    /// SENTIENT's own command (crate::sentient_admin).
    /// `Err(Failure::Stopped)`: SENTIENT is stopped, nothing was asked.
    admins: Option<Result<Vec<crate::sentient_admin::Admin>, crate::sentient_admin::Failure>>,
    retention_now: Option<Result<crate::sentient_admin::Retention, crate::sentient_admin::Failure>>,
    retention_edit: crate::sentient_admin::Retention,
    reading_admin: bool,
    /// The account whose password form is open.
    reset_for: Option<String>,
    new_pw: Entity<InputState>,
    repeat_pw: Entity<InputState>,
    /// The last administration result.
    admin_result: Option<(Tone, String)>,
    admin_busy: bool,
    /// Database: where each store keeps its data (crate::manager::movedata).
    locations: Option<Entity<super::movedata::LocationsCard>>,
    _subscriptions: Vec<Subscription>,
    _admin_tasks: Vec<Task<()>>,
    /// Enter in the fields (crate::ui::enter_moves_on).
    _enter: Vec<Subscription>,
}

impl EventEmitter<RecordChanged> for SettingsPage {}

impl SettingsPage {
    /// The takeover from here (crate::manager::takeover); the record is read
    /// again when the job ends, and the notice goes.
    fn open_take_over(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let this = cx.entity().downgrade();
        crate::manager::takeover::open(
            true,
            std::sync::Arc::new({
                let record = record.clone();
                move |compose: Option<String>| crate::adopt::take_over_plan(&record, compose.as_deref())
            }),
            std::sync::Arc::new(move |ch: bool, pw: String| crate::adopt::check_takeover_password(&record, ch, &pw)),
            std::rc::Rc::new(move |compose: Option<String>, creds: crate::manager::takeover::Creds, cx: &mut App| {
                this.update(cx, |page, cx| {
                    let Some(record) = page.record.clone() else { return };
                    page.job.update(cx, |job, cx| {
                        job.set_label("Taking over the compose file");
                        job.start(move |sink| crate::adopt::take_over_compose(record, compose.as_deref(), creds.db.as_deref(), creds.ch.as_deref(), sink).map(|_| ()), cx)
                    });
                })
                .ok();
            }),
            None,
            cx,
        );
    }

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
            tab: super::session::load().settings_tab.and_then(|i| Tab::ALL.into_iter().find(|t| t.session_index() == i)).unwrap_or(Tab::Platform),
            startup: cx.new(|cx| super::startup::StartupPanel::new(window, cx)),
            group: None,
            changed_only: false,
            custom_db: false,
            ram_mb: catalog::total_ram_mb(),
            job,
            locations: None,
            _subscriptions: subscriptions,
            _enter: Vec::new(),
        };
        // Data locations, where the data can move (Docker; ClickHouse in WSL
        // on Windows), or in the mockup's screens.
        let movable = page.record.as_ref().is_some_and(|r| crate::movedata::unsupported(r).is_none()) || (cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_MOVEDATA").is_some());
        if movable {
            page.locations = Some(cx.new(|cx| super::movedata::LocationsCard::new(crate::movedata::Target::Local, "This computer".into(), cfg!(windows), cx)));
        }
        // Development aid: open on a tab (screens).
        #[cfg(debug_assertions)]
        if let Ok(t) = std::env::var("SPM_SETTINGS_TAB") {
            page.tab = Tab::ALL.into_iter().find(|x| format!("{x:?}").eq_ignore_ascii_case(&t)).unwrap_or(Tab::Platform);
        }
        page.custom_db = page.preset(cx).is_none();
        // Enter: the next field shown; on the last one, Apply (or the first
        // field that is not valid). The add-a-variable and password forms
        // end on their own buttons.
        let fields: Vec<Entity<InputState>> = page.inputs.values().cloned().collect();
        page._enter = ui::enter_moves_on(&fields, window, cx, |p: &Self, cx| p.shown_inputs(cx), |p: &mut Self, window, cx| p.submit(window, cx));
        page._enter.extend(ui::enter_moves_on(
            &[page.new_name.clone(), page.new_value.clone()],
            window,
            cx,
            |p: &Self, _| vec![p.new_name.clone(), p.new_value.clone()],
            |p: &mut Self, window, cx| {
                p.add_variable(window, cx);
                None
            },
        ));
        page._enter.extend(ui::enter_moves_on(
            &[page.new_pw.clone(), page.repeat_pw.clone()],
            window,
            cx,
            |p: &Self, _| vec![p.new_pw.clone(), p.repeat_pw.clone()],
            |p: &mut Self, window, cx| {
                let pw = p.new_pw.read(cx).value().to_string();
                if pw.chars().count() < 8 {
                    return Some(p.new_pw.clone());
                }
                if pw != p.repeat_pw.read(cx).value().as_ref() {
                    return Some(p.repeat_pw.clone());
                }
                if !p.admin_busy {
                    p.reset_password(window, cx);
                }
                None
            },
        ));
        // Startup's fields end on the same Apply.
        let me = cx.entity().downgrade();
        page.startup.update(cx, |s, _| {
            s.on_submit = Some(std::rc::Rc::new(move |window, cx| {
                me.update(cx, |p, cx| {
                    if let Some(f) = p.submit(window, cx) {
                        f.update(cx, |s, cx| s.focus(window, cx));
                    }
                })
                .ok();
            }));
        });
        // Leaving the page with changes not applied asks first.
        let (a, b, c, d) = (cx.entity().downgrade(), cx.entity().downgrade(), cx.entity().downgrade(), cx.entity().downgrade());
        ui::set_unsaved(
            None,
            ui::Unsaved {
                pending: std::rc::Rc::new(move |cx| a.upgrade().map(|p| p.read(cx).pending(cx)).unwrap_or((0, 0))),
                list: std::rc::Rc::new(move |cx| d.upgrade().map(|p| p.read(cx).change_lines(cx)).unwrap_or_default()),
                apply: std::rc::Rc::new(move |window, cx| {
                    b.update(cx, |p, cx| p.apply_all(false, window, cx)).ok();
                }),
                discard: std::rc::Rc::new(move |window, cx| {
                    c.update(cx, |p, cx| p.discard_all(window, cx)).ok();
                }),
            },
        );
        // Screens and the layout test: some settings changed and one field
        // not valid, so the bar shows (SPM_DEBUG_SETTINGS_PENDING=short on
        // General, =long on All variables).
        #[cfg(debug_assertions)]
        if let Ok(which) = std::env::var("SPM_DEBUG_SETTINGS_PENDING") {
            page.tab = if which == "long" { Tab::Advanced } else { Tab::Platform };
            let mut set = vec![("RUST_LOG", "debug"), ("LICENSE_SERVER_URL", "https://licence.example.com/a/rather/long/address")];
            if which != "valid" && which != "leave" {
                set.push(("LICENSE_BINDING_CHECK_INTERVAL_SECS", "jj"));
            }
            for (k, v) in set {
                page.set(k, v, window, cx);
            }
        }
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
                // A read: never starts SENTIENT's stopped WSL distro.
                .spawn(async move { sentient_installer_core::distro::idle::reading(|| (crate::sentient_admin::admins(&record), crate::sentient_admin::retention(&record))) })
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
    fn admin_action(&mut self, work: impl FnOnce(&InstallationRecord) -> Result<String, crate::sentient_admin::Failure> + Send + 'static, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.admin_busy = true;
        self.admin_result = None;
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    // A button the user pressed: it may ask for the password, once.
                    let _action = sentient_installer_core::elevation::user_action();
                    work(&record)
                })
                .await;
            this.update(cx, |page, cx| {
                page.admin_busy = false;
                page.admin_result = Some(match r {
                    Ok(m) => (Tone::Good, m.lines().last().unwrap_or("Done.").to_string()),
                    // Stopped meanwhile: said plainly, not as a failure.
                    Err(crate::sentient_admin::Failure::Stopped) => (Tone::Neutral, crate::sentient_admin::STOPPED.to_string()),
                    Err(crate::sentient_admin::Failure::Error(e)) => (Tone::Bad, e),
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

    /// Why a field's value cannot be used (one line), or None.
    fn problem(&self, key: &str, cx: &App) -> Option<String> {
        let v = self.value(key, cx);
        match self.find(key) {
            Some(s) => s.problem(&v),
            // SENTIENT's own ports: a port number.
            None if key.starts_with("port:") => crate::catalog::Setting { env: "X_PORT".into(), label: String::new(), default: String::new(), recommended: None, help: String::new() }.problem(&v),
            None => None,
        }
    }

    /// The fields whose value cannot be used, by name.
    fn invalid_keys(&self, cx: &App) -> Vec<String> {
        let mut k: Vec<String> = self.inputs.keys().filter(|k| self.problem(k, cx).is_some()).cloned().collect();
        k.sort();
        k
    }

    /// Changes not applied on every tab (settings, Startup, the history
    /// kept) and the fields not valid: the bar and leaving the page.
    fn pending(&self, cx: &App) -> (usize, usize) {
        let retention = match &self.retention_now {
            Some(Ok(now)) => (self.retention_edit.telemetry_days != now.telemetry_days) as usize + (self.retention_edit.alarms_days != now.alarms_days) as usize,
            _ => 0,
        };
        let startup = self.startup.read(cx);
        (self.changed(cx).len() + startup.pending_count(cx) + retention, self.invalid_keys(cx).len() + startup.invalid_count(cx))
    }

    /// Apply every change (`confirm`: the settings' own confirmation, with
    /// what restarts; leaving the page has asked already).
    fn apply_all(&mut self, confirm: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending(cx).1 > 0 || self.job.read(cx).is_running() {
            return;
        }
        if let Some(Ok(now)) = &self.retention_now {
            if self.retention_edit != *now && !self.admin_busy {
                let r = self.retention_edit;
                self.admin_action(move |rec| crate::sentient_admin::set_retention(rec, r), cx);
            }
        }
        self.startup.update(cx, |s, cx| s.apply_pending(cx));
        if !self.changed(cx).is_empty() {
            if confirm {
                self.confirm_apply(window, cx);
            } else {
                self.start_apply(cx);
            }
        }
        cx.notify();
    }

    fn discard_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.revert(window, cx);
        self.startup.update(cx, |s, cx| s.discard(window, cx));
        if let Some(Ok(now)) = &self.retention_now {
            self.retention_edit = *now;
        }
        cx.notify();
    }

    /// Enter on a form's last field: the first field not valid (shown
    /// first), else Apply.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<InputState>> {
        let bad = self.invalid_keys(cx);
        if !bad.is_empty() {
            let shown = self.shown_inputs(cx);
            let first_shown = shown.into_iter().find(|i| bad.iter().any(|k| self.inputs.get(k) == Some(i)));
            return first_shown.or_else(|| self.inputs.get(&bad[0]).cloned());
        }
        if self.startup.read(cx).invalid_count(cx) > 0 {
            return None;
        }
        self.apply_all(true, window, cx);
        None
    }

    /// The inputs the open tab shows, in their order (Enter).
    fn shown_inputs(&self, cx: &App) -> Vec<Entity<InputState>> {
        let typed = |k: &str| self.find(k).is_none_or(is_input);
        let keys: Vec<String> = match self.tab {
            Tab::Platform => vec!["LICENSE_SERVER_URL".into(), "LICENSE_HEARTBEAT_INTERVAL_SECS".into()],
            Tab::Database => {
                let mut k: Vec<String> = Vec::new();
                if self.custom_db || self.preset(cx).is_none() {
                    k.extend(DB_EXACT.iter().map(|s| s.to_string()));
                }
                k.extend(self.record.as_ref().map(|r| ts_keys(r.flavor())).unwrap_or_default().iter().map(|s| s.to_string()));
                k
            }
            Tab::Network => PORTS
                .iter()
                .map(|(k, _)| k.to_string())
                .chain(std::iter::once("EDGE_GRPC_PORT".to_string()))
                .chain(CERTS.iter().map(|(k, _)| k.to_string()))
                .chain(std::iter::once("SENTIENT_CORS_ALLOWED_ORIGINS".to_string()))
                .collect(),
            Tab::Retention | Tab::Startup => Vec::new(),
            Tab::Advanced => {
                let needle = self.search.read(cx).value().to_lowercase();
                let needle = needle.as_str();
                self.groups
                    .iter()
                    .filter(|g| self.group.as_ref().is_none_or(|n| n == &g.name))
                    .flat_map(|g| g.settings.iter().filter(move |s| self.table_shows(g, s, needle, cx)).map(|s| s.env.clone()))
                    .collect()
            }
        };
        keys.iter().filter(|k| typed(k)).filter_map(|k| self.inputs.get(k).cloned()).collect()
    }

    /// Whether the All variables table shows this setting now.
    fn table_shows(&self, g: &Group, s: &Setting, needle: &str, cx: &App) -> bool {
        let changed = self.is_changed(&s.env, cx);
        let set = !self.value(&s.env, cx).is_empty();
        if self.changed_only && !(changed || set) {
            return false;
        }
        needle.is_empty() || format!("{} {} {} {}", s.label, s.env, g.name, s.help).to_lowercase().contains(needle)
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
                this.update(cx, |page, cx| page.run_apply(record, values, custom, cx)).ok();
                true
            })
        });
    }

    /// Apply without the confirmation (leaving the page asked already).
    fn start_apply(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let (values, custom) = (self.values(cx), self.custom_values(cx));
        self.run_apply(record, values, custom, cx);
    }

    fn run_apply(&mut self, record: InstallationRecord, values: HashMap<String, String>, custom: Vec<(String, String)>, cx: &mut Context<Self>) {
        self.job.update(cx, |job, cx| {
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
    }
}

// ---------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------

/// A row whose control takes the field and current columns (an account,
/// a plain value, a set of buttons). Shared with Startup.
pub(crate) fn row(_theme: &gpui_kit::component::Theme, label: impl Into<SharedString>, help: Option<String>, changed: bool, control: impl IntoElement) -> ui::SettingRow {
    let mut r = ui::SettingRow::wide(label, help, control);
    r.changed = changed;
    r
}

/// A plain value in a wide row.
fn text_value(theme: &gpui_kit::component::Theme, text: impl Into<SharedString>) -> Div {
    ui::row_text(text).text_color(theme.foreground)
}

/// The exact PostgreSQL and pool numbers under Database › Custom.
const DB_EXACT: [&str; 11] = [
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
];

/// The time-series store's settings, by the database type.
fn ts_keys(flavor: Flavor) -> &'static [&'static str] {
    match flavor {
        Flavor::Clickhouse => &["CLICKHOUSE_TS_BATCH_SIZE", "CLICKHOUSE_TS_BATCH_MAX_DELAY_MS", "CLICKHOUSE_TS_BATCH_THREADS"],
        Flavor::Timescaledb => &["SENTIENT_TS_KV_CHUNK_DAYS", "SENTIENT_TS_KV_COMPRESS_AFTER_DAYS"],
        _ => &[],
    }
}

/// A setting typed into an input (not a choice or a switch): Enter moves
/// through these.
pub(crate) fn is_input(s: &Setting) -> bool {
    crate::manager::setting_field::choices_for(&s.env).is_none() && s.kind() != crate::catalog::ValueKind::Switch
}

/// The tab a setting is on (0 General … 3 History and cleanup, 4 All
/// variables for the rest), for the tabs' counts. Shared with a remote
/// server's.
pub(crate) fn tab_index_of(key: &str) -> usize {
    if key.starts_with("port:") {
        return 2;
    }
    if key.starts_with("pg:") {
        return 1;
    }
    TAB_KEYS.iter().position(|keys| keys.contains(&key)).unwrap_or(4)
}

/// A setting's line in the leave prompt: "Name: old to new".
pub(crate) fn change_line(s: &Setting, old: &str, new: &str) -> String {
    use crate::manager::setting_field::current_words;
    format!("{}: {} to {}", s.short_label(), current_words(s, old), current_words(s, new))
}

/// The bar's small line when a field is not valid: its name and whether
/// it needs a number or another number.
pub(crate) fn first_invalid_of(s: &Setting, value: &str) -> (String, bool) {
    (s.short_label(), value.trim().parse::<u64>().is_err())
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

    /// A catalog setting's control (crate::manager::setting_field).
    fn control(&self, setting: &Setting, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        use crate::manager::setting_field::{self, FileButton};
        let Some(input) = self.inputs.get(&setting.env) else { return div().into_any_element() };
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
        setting_field::control(setting, input, set, FileButton::Choose(choose), window, cx)
    }

    /// A catalog setting's row: name, (i), Restart when its change
    /// restarts SENTIENT, the field, the value in effect in words. `desc`:
    /// the small line under the name.
    fn setting_row(&self, key: &str, desc: Option<String>, window: &Window, cx: &mut Context<Self>) -> Option<ui::SettingRow> {
        let st = self.find(key)?.clone();
        let changed = self.is_changed(key, cx);
        let applied = self.applied.get(key).cloned().unwrap_or_default();
        Some(ui::SettingRow {
            label: st.short_label().into(),
            help: self.help(key),
            desc,
            restart: changed,
            changed,
            control: self.control(&st, window, cx),
            wide: false,
            current: Some(crate::manager::setting_field::current_words(&st, &applied)),
            first: false,
        })
    }

    /// One of SENTIENT's own ports: a port field (not a catalog setting).
    fn port_row(&self, key: &'static str, label: &'static str, window: &Window, cx: &mut Context<Self>) -> ui::SettingRow {
        let theme = cx.theme().clone();
        let now = self.value(key, cx);
        let changed = self.is_changed(key, cx);
        let busy = changed && now.parse::<u16>().is_ok_and(sentient_installer_core::checks::port_busy);
        let applied = self.applied.get(key).cloned().unwrap_or_default();
        ui::SettingRow {
            label: label.into(),
            help: None,
            desc: busy.then(|| "In use by another program".to_string()),
            restart: changed,
            changed,
            control: ui::field_input(&theme, &self.inputs[key], Some(ui::Slot::Unit("port".into())), self.problem(key, cx), false, window, cx).into_any_element(),
            wide: false,
            current: Some(if applied.is_empty() { "Not set".to_string() } else { applied }),
            first: false,
        }
    }

    // ---- tabs --------------------------------------------------------------

    fn platform(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let start = crate::self_install::autostart()
            .map(|(on, delay)| if on { format!("With Windows, after {delay} s") } else { "Manual".into() })
            .unwrap_or_else(|| "With the system".into());
        let platform: Vec<ui::SettingRow> = vec![
            row(theme, "Address users open", Some("Where browsers open SENTIENT. The web port is under Network and certificates.".into()), false, text_value(theme, record.url())),
        ]
        .into_iter()
        .chain(["RUST_LOG", "ANALYTICS_ENABLED"].iter().filter_map(|k| self.setting_row(k, None, window, cx)))
        .chain(std::iter::once(row(theme, "Starts", Some("Start with Windows, the start delay and the operator screen are under Startup.".into()), false, text_value(theme, start))))
        .collect();
        let licence: Vec<ui::SettingRow> = ["LICENSE_SERVER_URL", "LICENSE_HEARTBEAT_INTERVAL_SECS", "LICENSE_BINDING_CHECK_INTERVAL_SECS"].iter().filter_map(|k| self.setting_row(k, None, window, cx)).collect();
        vec![
            self.admin_section(theme, cx).into_any_element(),
            ui::group(theme, "Platform", platform, vec![], cx).into_any_element(),
            ui::group(theme, "Licence", licence, vec![], cx).into_any_element(),
        ]
    }

    /// The system administrators, their password and lockout, and signing
    /// everyone out: all through SENTIENT's own command.
    fn admin_section(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let mut rows: Vec<ui::SettingRow> = Vec::new();
        let mut extra: Vec<AnyElement> = Vec::new();
        match &self.admins {
            None => {
                rows.push(row(
                    theme,
                    "Accounts",
                    Some("For when nobody can sign in. Changes go through SENTIENT's own administration command and are written to its audit log; SENTIENT keeps running.".into()),
                    false,
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(if self.reading_admin { "Reading the accounts…" } else { "Not read. Reading asks for the administrator password." }))
                        .when(!self.reading_admin, |r| r.child(crate::ui::probed(crate::ui::button("read-admins").outline().label("Read").on_click(cx.listener(|this, _, _, cx| this.read_admin(cx)))))),
                ));
            }
            // SENTIENT stopped: a plain line (nothing failed), and Start.
            Some(Err(crate::sentient_admin::Failure::Stopped)) => rows.push(row(
                theme,
                "Accounts",
                Some("For when nobody can sign in. Changes go through SENTIENT's own administration command and are written to its audit log; SENTIENT keeps running.".into()),
                false,
                h_flex()
                    .gap(crate::ui::button_gap())
                    .items_center()
                    .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(crate::sentient_admin::STOPPED))
                    .child(crate::ui::probed(crate::ui::button("admin-start").outline().label("Start SENTIENT").on_click(|_, _, cx| crate::manager::request("control:start", cx)))),
            )),
            Some(Err(crate::sentient_admin::Failure::Error(e))) => extra.push(ui::outcome(theme, Tone::Warn, e.clone()).into_any_element()),
            Some(Ok(list)) => {
                for a in list.iter() {
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
                    rows.push(row(
                        theme,
                        email.clone(),
                        None,
                        false,
                        h_flex()
                            .w_full()
                            .gap(crate::ui::button_gap())
                            .items_center()
                            .child(div().flex_1().min_w_0().text_sm().text_color(if locked { theme.danger } else { theme.muted_foreground }).child(format!("{state}{last}")))
                            .when(locked, |r| {
                                r.child(crate::ui::probed(crate::ui::button(SharedString::from(format!("unlock-{email}"))).outline().label("Unlock").disabled(self.admin_busy).on_click(cx.listener(move |this, _, _, cx| {
                                    let e = e1.clone();
                                    this.admin_action(move |r| crate::sentient_admin::unlock(r, &e), cx);
                                }))))
                            })
                            .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("reset-{email}"))).outline().label("Reset password").disabled(self.admin_busy).on_click(cx.listener(move |this, _, _, cx| {
                                this.reset_for = Some(e2.clone());
                                this.admin_result = None;
                                cx.notify();
                            })))),
                    ));
                }
                rows.push(row(
                    theme,
                    "Everyone signed in",
                    Some("Makes every open session sign in again, on every device.".into()),
                    false,
                    h_flex().child(crate::ui::probed(crate::ui::button("sign-out-all").outline().label("Sign everyone out").disabled(self.admin_busy).on_click(cx.listener(|this, _, window, cx| this.confirm_sign_out_all(window, cx))))),
                ));
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
            extra.push(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.primary)
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Reset the password of {email}")))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("The new password works at once; the account's open sessions are signed out and it is unlocked."))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_3()
                            .child(v_flex().gap_1().w(px(240.)).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("New password")).child(Input::new(&self.new_pw).mask_toggle()))
                            .child(v_flex().gap_1().w(px(240.)).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("Repeat it")).child(Input::new(&self.repeat_pw).mask_toggle())),
                    )
                    .when_some(problem.filter(|_| !pw.is_empty()), |c, p| c.child(div().text_sm().text_color(theme.danger).child(p)))
                    .child(
                        // Actions on the right: Cancel, then the primary.
                        h_flex()
                            .justify_end()
                            .gap(crate::ui::button_gap())
                            .child(crate::ui::probed(crate::ui::button("cancel-reset").outline().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                                this.reset_for = None;
                                cx.notify();
                            }))))
                            .child(crate::ui::probed(crate::ui::button("do-reset").primary().label("Reset password").disabled(problem.is_some() || self.admin_busy).on_click(cx.listener(|this, _, window, cx| this.reset_password(window, cx))))),
                    )
                    .into_any_element(),
            );
        }
        if self.admin_busy {
            extra.push(div().text_sm().text_color(theme.muted_foreground).child("Working…").into_any_element());
        }
        if let Some((tone, text)) = &self.admin_result {
            extra.push(ui::outcome(theme, *tone, text.clone()).into_any_element());
        }
        ui::group(theme, "System administrator", rows, extra, cx)
    }

    fn database(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
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
        let mut rows = vec![
            row(theme, "This computer", None, false, text_value(theme, format!("{} of memory", crate::dbstatus::bytes_label(self.ram_mb << 20)))),
            row(theme, "Size for", Some("Sets PostgreSQL's memory and connections and SENTIENT's connection pools together. Custom shows the exact numbers.".into()), false, h_flex().child(presets)),
        ];
        if open_custom {
            for k in DB_EXACT {
                rows.extend(self.setting_row(k, Some(k.trim_start_matches("pg:").to_string()), window, cx));
            }
        } else {
            for (label, k) in [("Memory for PostgreSQL", "pg:shared_buffers"), ("Memory per query", "pg:work_mem"), ("Connections allowed", "pg:max_connections"), ("SENTIENT's connection pool", "DATABASE_POOL_MAX")] {
                rows.push(row(theme, label, self.help(k), self.is_changed(k, cx), text_value(theme, shown(k))));
            }
        }
        let warn = if pg_changed {
            vec![ui::outcome(theme, Tone::Warn, "Changing PostgreSQL's memory or connections restarts PostgreSQL: SENTIENT is offline for about half a minute.").into_any_element()]
        } else {
            vec![]
        };
        let mut out = vec![ui::group(theme, "PostgreSQL memory and connections", rows, warn, cx).into_any_element()];
        let title = match record.flavor() {
            Flavor::Clickhouse => Some("Telemetry history (ClickHouse)"),
            Flavor::Timescaledb => Some("Telemetry history (TimescaleDB)"),
            _ => None,
        };
        if let Some(title) = title {
            let rows: Vec<ui::SettingRow> = ts_keys(record.flavor()).iter().filter_map(|k| self.setting_row(k, None, window, cx)).collect();
            out.push(ui::group(theme, title, rows, vec![], cx).into_any_element());
        }
        if let Some(l) = &self.locations {
            out.push(ui::group(theme, "Where the data is", vec![], vec![l.clone().into_any_element()], cx).into_any_element());
            return out;
        }
        out.push(
            ui::group(
                theme,
                "Where the data is",
                vec![row(theme, "PostgreSQL", Some("Sizes, free disk and growth are on Status.".into()), false, text_value(theme, if record.engine == Engine::Docker {
                    "In its Docker volume".to_string()
                } else {
                    format!("{}, port {}", record.pg_dir.display(), record.stack.pg_port)
                }))],
                vec![],
                cx,
            )
            .into_any_element(),
        );
        out
    }

    fn network(&self, theme: &gpui_kit::component::Theme, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut ports: Vec<ui::SettingRow> = PORTS.into_iter().map(|(key, label)| self.port_row(key, label, window, cx)).collect();
        ports.extend(self.setting_row("EDGE_GRPC_PORT", None, window, cx));
        let certs: Vec<ui::SettingRow> = CERTS
            .into_iter()
            .filter_map(|(key, label)| {
                let mut r = self.setting_row(key, None, window, cx)?;
                r.label = label.into();
                Some(r)
            })
            .collect();
        let access: Vec<ui::SettingRow> = ["SENTIENT_CORS_ALLOWED_ORIGINS", "SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT"].iter().filter_map(|k| self.setting_row(k, None, window, cx)).collect();
        vec![
            ui::group(theme, "Ports", ports, vec![], cx).into_any_element(),
            ui::group(theme, "Certificates", certs, vec![], cx).into_any_element(),
            ui::group(theme, "Access", access, vec![], cx).into_any_element(),
        ]
    }

    fn retention(&self, theme: &gpui_kit::component::Theme, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut keep: Vec<ui::SettingRow> = ["AUDIT_LOG_TTL_DAYS", "NOTIFICATIONS_TTL_DAYS", "SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS", "SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS"].iter().filter_map(|k| self.setting_row(k, None, window, cx)).collect();
        keep.extend(self.history_rows(theme, cx));
        let cleanup: Vec<ui::SettingRow> = ["AUDIT_LOG_CLEANUP_ENABLED", "SENTIENT__EVENT_CLEANUP__ENABLED"].iter().filter_map(|k| self.setting_row(k, None, window, cx)).collect();
        vec![ui::group(theme, "How long data is kept", keep, vec![], cx).into_any_element(), ui::group(theme, "Cleanup", cleanup, vec![], cx).into_any_element()]
    }

    /// Telemetry history and alarms: the default tenant profile's retention,
    /// through SENTIENT's own command (applied with the bar).
    fn history_rows(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Vec<ui::SettingRow> {
        const DAYS: [(&str, i64); 5] = [("Forever", 0), ("30 days", 30), ("90 days", 90), ("1 year", 365), ("3 years", 1095)];
        let help = "For tenants that use the default tenant profile (a tenant with its own profile keeps its own). Older values are deleted by SENTIENT's nightly cleanup.";
        let words = |d: i64| DAYS.iter().find(|(_, x)| *x == d).map(|(l, _)| l.to_string()).unwrap_or_else(|| format!("{d} days"));
        match &self.retention_now {
            None => vec![row(
                theme,
                "Telemetry history and alarms",
                Some(help.into()),
                false,
                h_flex()
                    .gap(crate::ui::button_gap())
                    .items_center()
                    .child(div().text_sm().text_color(theme.muted_foreground).child(if self.reading_admin { "Reading…" } else { "Not read." }))
                    .when(!self.reading_admin, |r| r.child(crate::ui::probed(crate::ui::button("read-retention").outline().label("Read").on_click(cx.listener(|this, _, _, cx| this.read_admin(cx)))))),
            )],
            Some(Err(e)) => vec![row(theme, "Telemetry history and alarms", Some(help.into()), false, text_value(theme, e.message()))],
            Some(Ok(now)) => {
                let now = *now;
                let edit = self.retention_edit;
                let labels: Vec<&str> = DAYS.iter().map(|(l, _)| *l).collect();
                let mut out = Vec::new();
                for (label, id, current, was, telemetry) in [("Telemetry history", "ret-tel", edit.telemetry_days, now.telemetry_days, true), ("Alarms", "ret-alarm", edit.alarms_days, now.alarms_days, false)] {
                    let this = cx.entity().downgrade();
                    let pick = std::rc::Rc::new(move |i: usize, _: &mut Window, cx: &mut App| {
                        let d = DAYS[i].1;
                        this.update(cx, |p, cx| {
                            if telemetry { p.retention_edit.telemetry_days = d } else { p.retention_edit.alarms_days = d }
                            cx.notify();
                        })
                        .ok();
                    });
                    out.push(ui::SettingRow {
                        label: label.into(),
                        help: Some(help.into()),
                        desc: None,
                        restart: false,
                        changed: current != was,
                        control: ui::chips(theme, id, &labels, DAYS.iter().position(|(_, d)| *d == current), pick).into_any_element(),
                        wide: false,
                        current: Some(words(was)),
                        first: false,
                    });
                }
                out
            }
        }
    }

    fn advanced(&self, theme: &gpui_kit::component::Theme, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let needle = self.search.read(cx).value().to_lowercase();
        let set_here = self.groups.iter().flat_map(|g| g.settings.iter()).filter(|s| !self.value(&s.env, cx).is_empty() || self.is_changed(&s.env, cx)).count();
        let total: usize = self.groups.iter().map(|g| g.settings.len()).sum();
        let filter = |id: &'static str, text: String, on: bool, only: bool| {
            let b = crate::ui::button(id).label(text).on_click(cx.listener(move |this, _, _, cx| {
                this.changed_only = only;
                cx.notify();
            }));
            if on { b.primary() } else { b.outline() }
        };
        let mut out = vec![
            h_flex()
                .flex_wrap()
                .gap(crate::ui::button_gap())
                .items_center()
                .px(px(18.))
                .pt(px(14.))
                .child(div().flex_1().min_w(px(200.)).child(Input::new(&self.search).cleanable(true)))
                .child(crate::ui::probed(filter("filter-all", format!("All {total}"), !self.changed_only, false)))
                .child(crate::ui::probed(filter("filter-set", format!("Set {set_here}"), self.changed_only, true)))
                .into_any_element(),
        ];
        let mut any = false;
        for g in &self.groups {
            let mut rows = Vec::new();
            let shown: Vec<Setting> = g.settings.iter().filter(|s| self.table_shows(g, s, &needle, cx)).cloned().collect();
            for s in &shown {
                let Some(mut r) = self.setting_row(&s.env, Some(s.env.clone()), window, cx) else { continue };
                // A value set here can go back to the built-in one.
                if !self.value(&s.env, cx).is_empty() {
                    let key = s.env.clone();
                    r.label = r.label.clone();
                    r.desc = Some(format!("{} · set here", s.env));
                    let reset = crate::ui::button(SharedString::from(format!("reset-{}", s.env))).ghost().xsmall().label("Reset").on_click(cx.listener(move |this, _, window, cx| {
                        this.set(&key, "", window, cx);
                        cx.notify();
                    }));
                    r.control = h_flex().gap_2().items_start().child(r.control).child(ui::on_control_line(reset)).into_any_element();
                }
                rows.push(r);
            }
            if !rows.is_empty() {
                any = true;
                out.push(ui::group(theme, &g.name, rows, vec![], cx).into_any_element());
            }
        }
        if !any {
            out.push(div().px(px(18.)).py_3().text_sm().text_color(theme.muted_foreground).child("Nothing matches.").into_any_element());
        }
        let mut added: Vec<ui::SettingRow> = Vec::new();
        for (i, (n, v)) in self.custom.iter().enumerate() {
            added.push(row(
                theme,
                n.read(cx).value().to_string(),
                None,
                false,
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(ui::FIELD_W)).child(Input::new(v)))
                    .child(crate::ui::button(SharedString::from(format!("remove-var-{i}"))).ghost().label("Remove").on_click(cx.listener(move |this, _, _, cx| {
                        if i < this.custom.len() {
                            this.custom.remove(i);
                        }
                        cx.notify();
                    }))),
            ));
        }
        let name_ok = crate::settings::valid_name(&self.new_name.read(cx).value());
        added.push(row(
            theme,
            "Add a variable",
            Some("Variables SENTIENT reads that are not in the list above. For support and special cases.".into()),
            false,
            h_flex()
                .flex_wrap()
                .gap_2()
                .items_center()
                .child(div().w(px(180.)).child(Input::new(&self.new_name)))
                .child(div().w(px(140.)).child(Input::new(&self.new_value)))
                .child(crate::ui::probed(crate::ui::button("add-var").outline().label("Add").disabled(!name_ok).on_click(cx.listener(|this, _, window, cx| this.add_variable(window, cx))))),
        ));
        out.push(ui::group(theme, "Added variables", added, vec![], cx).into_any_element());
        out
    }

    /// The tabs' counts: (changes, holds a field not valid) per tab.
    fn tab_badges(&self, cx: &App) -> Vec<(usize, bool)> {
        let mut b = vec![(0usize, false); Tab::ALL.len()];
        let at = |t: Tab| Tab::ALL.iter().position(|x| *x == t).unwrap_or(0);
        let tab_of = |k: &str| match tab_index_of(k) {
            0 => Tab::Platform,
            1 => Tab::Database,
            2 => Tab::Network,
            3 => Tab::Retention,
            _ => Tab::Advanced,
        };
        for k in self.inputs.keys() {
            let i = at(tab_of(k));
            if self.is_changed(k, cx) {
                b[i].0 += 1;
            }
            if self.problem(k, cx).is_some() {
                b[i].1 = true;
            }
        }
        if self.custom_values(cx) != self.custom_applied {
            b[at(Tab::Advanced)].0 += 1;
        }
        if let Some(Ok(now)) = &self.retention_now {
            b[at(Tab::Retention)].0 += (self.retention_edit.telemetry_days != now.telemetry_days) as usize + (self.retention_edit.alarms_days != now.alarms_days) as usize;
        }
        let st = self.startup.read(cx);
        b[at(Tab::Startup)] = (st.pending_count(cx), st.invalid_count(cx) > 0);
        b
    }

    /// The changes as the leave prompt lists them.
    fn change_lines(&self, cx: &App) -> Vec<String> {
        let mut out = Vec::new();
        for k in self.changed(cx) {
            let old = self.applied.get(&k).cloned().unwrap_or_default();
            let new = self.value(&k, cx);
            match self.find(&k) {
                Some(s) => out.push(change_line(s, &old, &new)),
                None => {
                    let label = PORTS.iter().find(|(p, _)| *p == k).map(|(_, l)| format!("{l} port")).unwrap_or_else(|| k.clone());
                    out.push(format!("{label}: {} to {}", if old.is_empty() { "not set" } else { &old }, if new.is_empty() { "not set" } else { &new }));
                }
            }
        }
        if let Some(Ok(now)) = &self.retention_now {
            if self.retention_edit.telemetry_days != now.telemetry_days {
                out.push(format!("Telemetry history: {} to {} days", now.telemetry_days, self.retention_edit.telemetry_days));
            }
            if self.retention_edit.alarms_days != now.alarms_days {
                out.push(format!("Alarms: {} to {} days", now.alarms_days, self.retention_edit.alarms_days));
            }
        }
        let n = self.startup.read(cx).pending_count(cx);
        if n > 0 {
            out.push(format!("Startup: {n} change{}", if n == 1 { "" } else { "s" }));
        }
        out
    }

    /// The Apply bar's small line.
    fn bar_line(&self, cx: &App) -> (String, bool) {
        let (changes, _) = self.pending(cx);
        let changed = self.changed(cx);
        let restart = !changed.is_empty();
        if let Some(k) = self.invalid_keys(cx).first() {
            let (label, number) = match self.find(k) {
                Some(s) => first_invalid_of(s, &self.value(k, cx)),
                None => (PORTS.iter().find(|(p, _)| p == k).map(|(_, l)| format!("{l} port")).unwrap_or_else(|| k.clone()), self.value(k, cx).parse::<u64>().is_err()),
            };
            return (ui::pending_line(changes, &[], restart, Some((&label, number))), restart);
        }
        if self.startup.read(cx).invalid_count(cx) > 0 {
            return (ui::pending_line(changes, &[], restart, Some(("Start delay", true))), restart);
        }
        let mut names: Vec<String> = changed
            .iter()
            .map(|k| self.find(k).map(|s| s.short_label()).or_else(|| PORTS.iter().find(|(p, _)| p == k).map(|(_, l)| format!("{l} port"))).unwrap_or_else(|| k.clone()))
            .collect();
        if let Some(Ok(now)) = &self.retention_now {
            if self.retention_edit.telemetry_days != now.telemetry_days {
                names.push("Telemetry history".into());
            }
            if self.retention_edit.alarms_days != now.alarms_days {
                names.push("Alarms".into());
            }
        }
        if self.startup.read(cx).pending_count(cx) > 0 {
            names.push("Startup".into());
        }
        (ui::pending_line(changes, &names, restart, None), restart)
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        ui::set_stacked(f32::from(window.viewport_size().width) < ui::ROW_STACK_W);
        let Some(record) = self.record.clone() else {
            return ui::page(&theme, "Platform settings", "How SENTIENT runs on this computer. Change a value and apply it; only what changed restarts.").child(ui::not_installed(&theme)).into_any_element();
        };
        #[cfg(debug_assertions)]
        {
            static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if std::env::var("SPM_DEBUG_SETTINGS_PENDING").is_ok_and(|v| v == "leave") && !ASKED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                window.defer(cx, |window, cx| ui::debug_leave_prompt(None, window, cx));
            }
        }
        let job = self.job.read(cx);
        let busy = job.is_running();
        // Someone else's compose file: the notice, and Apply stays off.
        let external = record.engine == crate::installation::Engine::Docker && record.stack.external.is_some();
        let foreign = external.then(|| {
            crate::manager::remote_pages::foreign_compose_notice(
                &theme,
                "The settings cannot be changed now.",
                crate::ui::button("set-take-over").outline().label("Take over the compose file").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.open_take_over(cx))).into_any_element(),
            )
        });
        // What no single field shows: two ports alike, an added variable's name.
        let mut problems: Vec<String> = crate::settings::check(&self.values(cx)).into_iter().filter(|p| p.contains("already used")).collect();
        for (n, _) in self.custom_values(cx) {
            if !crate::settings::valid_name(&n) {
                problems.push(format!("{n}: not a usable variable name (letters, digits and _)"));
            }
        }

        let this = cx.entity().downgrade();
        let titles: Vec<&'static str> = Tab::ALL.iter().map(|t| t.title()).collect();
        let tabs = ui::card_tabs(&theme, "", &titles, Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0), &self.tab_badges(cx), std::rc::Rc::new(move |i, cx| {
            this.update(cx, |p, cx| {
                p.tab = Tab::ALL[i];
                super::session::set_settings_tab(Tab::ALL[i].session_index());
                cx.notify();
            })
            .ok();
        }));

        let (changes, invalid) = self.pending(cx);
        let (line, restart) = self.bar_line(cx);
        let bar = ui::pending_bar(
            &theme,
            "settings-pending",
            changes,
            invalid,
            line,
            restart,
            !busy && !external && problems.is_empty(),
            cx.listener(|this, _, window, cx| this.discard_all(window, cx)),
            cx.listener(|this, _, window, cx| this.apply_all(true, window, cx)),
        );

        let body = match self.tab {
            Tab::Platform => self.platform(&theme, &record, window, cx),
            Tab::Database => self.database(&theme, &record, window, cx),
            Tab::Network => self.network(&theme, window, cx),
            Tab::Retention => self.retention(&theme, window, cx),
            Tab::Startup => vec![self.startup.clone().into_any_element()],
            Tab::Advanced => self.advanced(&theme, window, cx),
        };

        let notices = (foreign.is_some() || !problems.is_empty()).then(|| v_flex().gap_2().children(foreign).children(problems.into_iter().map(|p| div().text_sm().text_color(theme.danger).child(p))));
        settings_page(&theme, "Platform settings", self.tab.about(), notices, ui::settings_card(&theme, "settings-body", tabs, v_flex().pb(px(12.)).children(body), bar))
            .into_any_element()
    }
}

/// Platform settings' page: its title and line, notices, then the settings
/// card filling the rest (it scrolls inside; the page does not). Shared
/// with a remote server's.
pub(crate) fn settings_page(theme: &gpui_kit::component::Theme, title: &'static str, about: &'static str, notices: Option<Div>, card: impl IntoElement) -> Stateful<Div> {
    v_flex()
        .id("settings-page")
        .flex_1()
        .min_w_0()
        .h_full()
        .p(px(ui::PAGE_PAD))
        .gap(px(ui::GAP))
        .bg(theme.muted)
        .child(
            v_flex()
                .gap_0p5()
                .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_sm().text_color(theme.muted_foreground).child(about)),
        )
        .children(notices)
        .child(card)
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

    /// The tabs' counts put a setting on the tab that shows it.
    #[test]
    fn settings_count_on_their_tab() {
        use super::tab_index_of;
        assert_eq!(tab_index_of("LICENSE_BINDING_CHECK_INTERVAL_SECS"), 0);
        assert_eq!(tab_index_of("pg:work_mem"), 1);
        assert_eq!(tab_index_of("port:http"), 2);
        assert_eq!(tab_index_of("NOTIFICATIONS_TTL_DAYS"), 3);
        assert_eq!(tab_index_of("SQL_TS_BATCH_THREADS"), 4);
    }
}

/// The Startup tab's title and line (this computer only).
const STARTUP_TITLE: &str = "Startup";
const STARTUP_ABOUT: &str = "What happens when this computer starts and someone signs in.";

/// The tab titles of Platform settings a remote server has, in order; this
/// computer's add Startup before All variables.
pub(crate) const TAB_TITLES: [&str; 5] = ["General", "Database", "Network and certificates", "History and cleanup", "All variables"];

/// What each category holds (the page's line under its title).
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
    &["RUST_LOG", "ANALYTICS_ENABLED", "LICENSE_SERVER_URL", "LICENSE_HEARTBEAT_INTERVAL_SECS", "LICENSE_BINDING_CHECK_INTERVAL_SECS"],
    &["DATABASE_POOL_MAX", "DATABASE_POOL_MIN", "DB_POOL_PRIMARY_MAX", "DB_POOL_PRIMARY_MIN", "DB_POOL_REPORT_MAX", "DB_POOL_REPORT_MIN", "DB_MAX_CONNECTIONS", "CLICKHOUSE_TS_BATCH_SIZE", "CLICKHOUSE_TS_BATCH_MAX_DELAY_MS", "CLICKHOUSE_TS_BATCH_THREADS", "SENTIENT_TS_KV_CHUNK_DAYS", "SENTIENT_TS_KV_COMPRESS_AFTER_DAYS"],
    &["EDGE_GRPC_PORT", "SSL_PEM_CERT", "SSL_PEM_KEY", "MQTT_SSL_PEM_CERT", "MQTT_SSL_PEM_KEY", "MQTT_SSL_PEM_CA", "COAP_SSL_PEM_CERT", "COAP_SSL_PEM_KEY", "SENTIENT_CORS_ALLOWED_ORIGINS", "SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT"],
    &["AUDIT_LOG_TTL_DAYS", "NOTIFICATIONS_TTL_DAYS", "SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS", "SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS", "AUDIT_LOG_CLEANUP_ENABLED", "SENTIENT__EVENT_CLEANUP__ENABLED"],
];

/// The setting groups each of the first four tabs shows on a remote
/// server: (heading, keys).
pub(crate) const TAB_GROUPS: [&[(&str, &[&str])]; 4] = [
    &[("Platform", &["RUST_LOG", "ANALYTICS_ENABLED"]), ("Licence", &["LICENSE_SERVER_URL", "LICENSE_HEARTBEAT_INTERVAL_SECS", "LICENSE_BINDING_CHECK_INTERVAL_SECS"])],
    &[
        ("Connection pools", &["DATABASE_POOL_MAX", "DATABASE_POOL_MIN", "DB_POOL_PRIMARY_MAX", "DB_POOL_PRIMARY_MIN", "DB_POOL_REPORT_MAX", "DB_POOL_REPORT_MIN", "DB_MAX_CONNECTIONS"]),
        ("Telemetry history", &["CLICKHOUSE_TS_BATCH_SIZE", "CLICKHOUSE_TS_BATCH_MAX_DELAY_MS", "CLICKHOUSE_TS_BATCH_THREADS", "SENTIENT_TS_KV_CHUNK_DAYS", "SENTIENT_TS_KV_COMPRESS_AFTER_DAYS"]),
    ],
    &[
        ("Ports", &["EDGE_GRPC_PORT"]),
        ("Certificates", &["SSL_PEM_CERT", "SSL_PEM_KEY", "MQTT_SSL_PEM_CERT", "MQTT_SSL_PEM_KEY", "MQTT_SSL_PEM_CA", "COAP_SSL_PEM_CERT", "COAP_SSL_PEM_KEY"]),
        ("Access", &["SENTIENT_CORS_ALLOWED_ORIGINS", "SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT"]),
    ],
    &[("How long data is kept", &["AUDIT_LOG_TTL_DAYS", "NOTIFICATIONS_TTL_DAYS", "SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS", "SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS"]), ("Cleanup", &["AUDIT_LOG_CLEANUP_ENABLED", "SENTIENT__EVENT_CLEANUP__ENABLED"])],
];

impl super::Manager {
    /// Leave the page (a menu row, another instance): from Platform settings
    /// with changes not applied, asks first (crate::ui::ask_before_leaving);
    /// `go` runs at once, or after Apply or Discard, never after Stay.
    pub(super) fn leave_then(&mut self, window: &mut Window, cx: &mut Context<Self>, go: impl Fn(&mut Self, &mut Context<Self>) + 'static) {
        let scope = self.open_remote.clone();
        if self.page != super::Page::Settings || ui::unsaved(&scope, cx) == (0, 0) {
            go(self, cx);
            return;
        }
        let me = cx.entity().downgrade();
        let go = std::rc::Rc::new(go);
        ui::ask_before_leaving(
            scope,
            window,
            cx,
            std::rc::Rc::new(move |_, cx| {
                let go = go.clone();
                me.update(cx, |m, cx| go(m, cx)).ok();
            }),
        );
    }
}
