// Copyright © 2016-2026 The SENTIENT Authors

//! Install mode: the setup wizard (docs/GPUI_PLATFORM_MANAGER_PLAN.md,
//! "Installer wizard"). Twelve pages: ten collect choices without touching the
//! machine, Install runs the plan, Finish says what to do next.

mod pages;
mod plan;

pub(crate) use plan::{InstallPlan, run as run_install, run_manager_only};

#[cfg(windows)]
pub use plan::provision_clickhouse_wsl;

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    input::{InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::checks::{self, Status};
use sentient_installer_core::distro::DeployConfig;
use sentient_installer_core::native;
use sentient_installer_core::stack::Flavor;

use crate::catalog::{self, Group};
use crate::installation::Engine;
use crate::jobs::{Job, JobState};

const STEPS: [&str; 12] = [
    "Welcome",
    "System check",
    "Location",
    "Database",
    "Database settings",
    "Network ports",
    "Administrator",
    "Configuration",
    "Shortcuts and startup",
    "Summary",
    "Install",
    "Finish",
];
const SHORTCUTS: usize = 8;
const SUMMARY: usize = 9;
const INSTALL: usize = 10;
const FINISH: usize = 11;

/// One line of the system check, kept as plain data for rendering.
#[derive(Clone)]
struct CheckRow {
    label: SharedString,
    detail: SharedString,
    status: Status,
}

/// Text fields of the wizard, each its own input state.
struct Fields {
    install_dir: Entity<InputState>,
    data_dir: Entity<InputState>,
    pg_db: Entity<InputState>,
    pg_user: Entity<InputState>,
    pg_password: Entity<InputState>,
    pg_admin_password: Entity<InputState>,
    pg_port: Entity<InputState>,
    ch_db: Entity<InputState>,
    ch_user: Entity<InputState>,
    ch_password: Entity<InputState>,
    ch_http_port: Entity<InputState>,
    ch_native_port: Entity<InputState>,
    http_port: Entity<InputState>,
    mqtt_port: Entity<InputState>,
    mqtts_port: Entity<InputState>,
    coap_port: Entity<InputState>,
    coaps_port: Entity<InputState>,
    admin_email: Entity<InputState>,
    admin_password: Entity<InputState>,
    admin_confirm: Entity<InputState>,
    autologin_user: Entity<InputState>,
    autologin_password: Entity<InputState>,
    search: Entity<InputState>,
}

pub struct Installer {
    step: usize,
    /// How SENTIENT runs: always native on Windows; Docker or native on Linux.
    engine: Engine,
    accepted: bool,
    /// Install the Platform Manager alone, without SENTIENT.
    manager_only: bool,
    checks: Option<Vec<CheckRow>>,
    flavor: Option<Flavor>,
    load_demo: bool,
    customize: bool,
    groups: Vec<Group>,
    ram_mb: u64,
    recommended: HashMap<String, String>,
    /// One input per catalog setting, created with the wizard.
    settings: HashMap<String, Entity<InputState>>,
    autostart: bool,
    desktop_sentient: bool,
    desktop_manager: bool,
    start_menu: bool,
    kiosk: bool,
    open_at_signin: bool,
    /// Sign in to the computer automatically (only with open_at_signin).
    autologin: bool,
    bundle: Option<PathBuf>,
    sentient_version: SharedString,
    /// Why Next is refused on the current page.
    problem: Option<SharedString>,
    fields: Fields,
    install: Entity<Job>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

fn input(window: &mut Window, cx: &mut Context<Installer>, value: &str) -> Entity<InputState> {
    let value = value.to_string();
    cx.new(|cx| InputState::new(window, cx).default_value(value))
}

fn secret(window: &mut Window, cx: &mut Context<Installer>) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).masked(true))
}

impl Installer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // After a restart for WSL2: the saved plan, to continue where it stopped.
        let resumed = std::env::args().any(|a| a == "--resume").then(plan::take_pending).flatten();
        let mut d = DeployConfig::default();
        if let Some(p) = &resumed {
            d.http_port = p.stack.http_port;
        }
        let admin_email = resumed.as_ref().map(|p| p.admin_email.clone()).unwrap_or_default();
        let fields = Fields {
            install_dir: input(window, cx, if cfg!(windows) { r"C:\Program Files\SENTIENT" } else { "/opt/sentient" }),
            data_dir: input(window, cx, if cfg!(windows) { r"C:\PostgreSQL\18" } else { "" }),
            pg_db: input(window, cx, &d.db_name),
            pg_user: input(window, cx, &d.db_user),
            pg_password: secret(window, cx),
            pg_admin_password: secret(window, cx),
            pg_port: input(window, cx, &d.pg_port.to_string()),
            ch_db: input(window, cx, &d.ch_db),
            ch_user: input(window, cx, &d.ch_user),
            ch_password: secret(window, cx),
            ch_http_port: input(window, cx, &d.ch_http_port.to_string()),
            ch_native_port: input(window, cx, &d.ch_native_port.to_string()),
            http_port: input(window, cx, &d.http_port.to_string()),
            mqtt_port: input(window, cx, &d.mqtt_port.to_string()),
            mqtts_port: input(window, cx, &d.mqtts_port.to_string()),
            coap_port: input(window, cx, &d.coap_port.to_string()),
            coaps_port: input(window, cx, &d.coaps_port.to_string()),
            admin_email: input(window, cx, &admin_email),
            admin_password: secret(window, cx),
            admin_confirm: secret(window, cx),
            autologin_user: input(window, cx, &sentient_installer_core::autologin::this_user()),
            autologin_password: secret(window, cx),
            search: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search by name or variable, e.g. batch or RULE_ENGINE")
            }),
        };
        let groups = catalog::groups();
        let ram_mb = catalog::total_ram_mb();
        let recommended = catalog::recommended(&groups, ram_mb);
        let mut settings = HashMap::new();
        for setting in groups.iter().flat_map(|g| g.settings.iter()) {
            let value = recommended.get(&setting.env).cloned().unwrap_or_default();
            let placeholder = if setting.default.is_empty() {
                "SENTIENT default".to_string()
            } else {
                setting.default.clone()
            };
            let state = cx.new(|cx| {
                InputState::new(window, cx).placeholder(placeholder).default_value(value)
            });
            settings.insert(setting.env.clone(), state);
        }
        let search_sub = cx.subscribe_in(&fields.search, window, |_, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let install = cx.new(|_| Job::new());
        let install_sub = cx.observe(&install, |this, job, cx| {
            if this.step == INSTALL && matches!(job.read(cx).state(), JobState::Done(_)) {
                this.step = FINISH;
            }
            cx.notify();
        });
        let bundle = native::bundled_payload_dir();
        let sentient_version = bundle
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join("VERSION")).ok())
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| "in this package".into());

        let mut this = Self {
            step: 0,
            engine: if cfg!(windows) { Engine::Native } else { Engine::Docker },
            accepted: false,
            manager_only: false,
            checks: None,
            flavor: None,
            load_demo: false,
            customize: false,
            groups,
            ram_mb,
            recommended,
            settings,
            autostart: true,
            desktop_sentient: true,
            desktop_manager: true,
            start_menu: true,
            kiosk: false,
            open_at_signin: false,
            autologin: false,
            bundle,
            sentient_version: sentient_version.into(),
            problem: None,
            fields,
            install,
            _tasks: Vec::new(),
            _subscriptions: vec![search_sub, install_sub],
        };
        this.run_checks(cx);
        if let Some(plan) = resumed {
            this.accepted = true;
            this.flavor = Some(plan.stack.flavor);
            this.engine = plan.engine;
            this.step = INSTALL;
            this.install.update(cx, |job, cx| job.start(move |sink| plan::run(plan, sink), cx));
        }
        // Development aid: open the wizard on a given page to inspect it.
        #[cfg(debug_assertions)]
        if let Some(step) = std::env::var("SPM_WIZARD_STEP").ok().and_then(|s| s.parse::<usize>().ok()) {
            this.step = step.min(FINISH);
            this.flavor = Some(Flavor::Timescaledb);
        }
        this
    }

    fn value(&self, field: &Entity<InputState>, cx: &App) -> String {
        field.read(cx).value().trim().to_string()
    }

    fn run_checks(&mut self, cx: &mut Context<Self>) {
        self.checks = None;
        let offline = self.bundle.is_some();
        let engine = self.engine;
        let task = cx.spawn(async move |this, cx| {
            let rows = cx
                .background_executor()
                .spawn(async move {
                    let found = if cfg!(windows) {
                        checks::run_native(offline)
                    } else if engine == Engine::Docker {
                        checks::run_linux_docker(offline)
                    } else {
                        checks::run_linux_native(offline)
                    };
                    let mut rows = found
                        .into_iter()
                        .map(|c| CheckRow { label: c.label.into(), detail: c.detail.into(), status: c.status })
                        .collect::<Vec<_>>();
                    // Installing over a running SENTIENT would replace its
                    // configuration (and, on Docker, its passwords).
                    for c in crate::adopt::find() {
                        rows.push(CheckRow {
                            label: "SENTIENT already installed".into(),
                            detail: format!(
                                "SENTIENT {} runs here as {}, from {}. Installing again would overwrite it: open the Platform Manager and use “Add existing instance” on its Status screen instead.",
                                c.record.sentient_version, c.kind, c.location
                            )
                            .into(),
                            status: Status::Fail,
                        });
                    }
                    rows
                })
                .await;
            this.update(cx, |page, cx| {
                page.checks = Some(rows);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn port(&self, field: &Entity<InputState>, name: &str, cx: &App) -> Result<u16, String> {
        let text = self.value(field, cx);
        text.parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| format!("{name} must be a number between 1 and 65535."))
    }

    /// Can the wizard leave the current page? `Err` says why not.
    fn validate(&self, cx: &App) -> Result<(), String> {
        let f = &self.fields;
        match self.step {
            0 if !self.accepted => Err("Accept the licence agreement to continue.".into()),
            1 => match &self.checks {
                None => Err("The system check is still running.".into()),
                Some(rows) if rows.iter().any(|r| r.status == Status::Fail) => {
                    Err("Resolve the items marked “Blocker” first, then run the check again.".into())
                }
                Some(_) => Ok(()),
            },
            2 if self.engine == Engine::Docker => Ok(()),
            2 => {
                if self.value(&f.install_dir, cx).is_empty()
                    || (cfg!(windows) && self.value(&f.data_dir, cx).is_empty())
                {
                    Err("The folders are required.".into())
                } else {
                    Ok(())
                }
            }
            3 => match self.flavor {
                None => Err("Choose a database.".into()),
                Some(flavor) if !self.flavors().contains(&flavor) => {
                    Err("That database is not available for the chosen way of running SENTIENT.".into())
                }
                Some(_) => Ok(()),
            },
            4 => {
                for (field, name) in [
                    (&f.pg_db, "Database name"),
                    (&f.pg_user, "Database user"),
                    (&f.pg_password, "Database password"),
                    (&f.pg_admin_password, "PostgreSQL admin password"),
                ] {
                    if self.value(field, cx).is_empty() {
                        return Err(format!("{name} is required."));
                    }
                }
                if self.flavor == Some(Flavor::Clickhouse) && self.value(&f.ch_password, cx).is_empty() {
                    return Err("ClickHouse password is required.".into());
                }
                self.ports_in_use(&self.database_ports(cx)?)
            }
            5 => self.ports_in_use(&self.network_ports(cx)?),
            6 => {
                let email = self.value(&f.admin_email, cx);
                let password = self.value(&f.admin_password, cx);
                if !email.contains('@') {
                    Err("Enter the administrator's e-mail address.".into())
                } else if password.len() < 8 {
                    Err("The password must be at least 8 characters.".into())
                } else if password != self.value(&f.admin_confirm, cx) {
                    Err("The passwords do not match.".into())
                } else {
                    Ok(())
                }
            }
            SHORTCUTS if !self.manager_only && self.open_at_signin && self.autologin => {
                if self.value(&f.autologin_user, cx).is_empty() {
                    Err("Enter the account that signs in automatically.".into())
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    /// The databases on offer for this OS and engine.
    fn flavors(&self) -> Vec<Flavor> {
        match (cfg!(windows), self.engine) {
            (true, _) | (false, Engine::Docker) => vec![Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse],
            // Distribution packages carry no TimescaleDB.
            (false, Engine::Native) => vec![Flavor::Stock, Flavor::Clickhouse],
        }
    }

    fn set_engine(&mut self, engine: Engine, cx: &mut Context<Self>) {
        if self.engine != engine {
            self.engine = engine;
            if self.flavor.is_some_and(|f| !self.flavors().contains(&f)) {
                self.flavor = None;
            }
            self.run_checks(cx);
        }
        cx.notify();
    }

    /// The database ports, chosen under Database settings.
    fn database_ports(&self, cx: &App) -> Result<Vec<(u16, String)>, String> {
        let f = &self.fields;
        let mut ports = vec![(self.port(&f.pg_port, "PostgreSQL port", cx)?, "PostgreSQL".into())];
        if self.flavor == Some(Flavor::Clickhouse) {
            ports.push((self.port(&f.ch_http_port, "ClickHouse HTTP port", cx)?, "ClickHouse HTTP".into()));
            ports.push((self.port(&f.ch_native_port, "ClickHouse native port", cx)?, "ClickHouse native".into()));
        }
        Ok(ports)
    }

    /// SENTIENT's own ports, chosen under Network ports.
    fn network_ports(&self, cx: &App) -> Result<Vec<(u16, String)>, String> {
        let f = &self.fields;
        Ok(vec![
            (self.port(&f.http_port, "Web port", cx)?, "HTTP".into()),
            (self.port(&f.mqtt_port, "MQTT port", cx)?, "MQTT".into()),
            (self.port(&f.mqtts_port, "MQTT over TLS port", cx)?, "MQTTS".into()),
            (self.port(&f.coap_port, "CoAP port", cx)?, "CoAP".into()),
            (self.port(&f.coaps_port, "CoAP over DTLS port", cx)?, "CoAPS".into()),
        ])
    }

    /// Refuses ports something already listens on. The install only goes
    /// ahead with ports that are free now.
    fn ports_in_use(&self, ports: &[(u16, String)]) -> Result<(), String> {
        let busy = checks::busy_ports(ports);
        if busy.is_empty() {
            Ok(())
        } else {
            Err(format!("Already in use: {}. Choose another port, or stop what is listening on it.", busy.join(", ")))
        }
    }

    /// Why the current page's ports are refused, shown on the page itself.
    fn port_problem(&self, cx: &App) -> Option<String> {
        let ports = match self.step {
            4 => self.database_ports(cx),
            5 => self.network_ports(cx),
            _ => return None,
        };
        match ports {
            Ok(ports) => self.ports_in_use(&ports).err(),
            Err(e) => Some(e),
        }
    }

    /// The settings that will be written: every catalog setting with a value.
    fn chosen_settings(&self, cx: &App) -> Vec<(String, String)> {
        self.groups
            .iter()
            .flat_map(|g| g.settings.iter())
            .filter_map(|s| {
                let value = if self.customize {
                    self.value(&self.settings[&s.env], cx)
                } else {
                    self.recommended.get(&s.env).cloned().unwrap_or_default()
                };
                (!value.is_empty()).then(|| (s.env.clone(), value))
            })
            .collect()
    }

    fn plan(&self, cx: &App) -> Result<plan::InstallPlan, String> {
        let f = &self.fields;
        let mut stack = DeployConfig {
            flavor: self.flavor.ok_or("Choose a database.")?,
            db_name: self.value(&f.pg_db, cx),
            db_user: self.value(&f.pg_user, cx),
            db_password: self.value(&f.pg_password, cx),
            pg_port: self.port(&f.pg_port, "PostgreSQL port", cx)?,
            ch_db: self.value(&f.ch_db, cx),
            ch_user: self.value(&f.ch_user, cx),
            ch_password: self.value(&f.ch_password, cx),
            ch_http_port: self.port(&f.ch_http_port, "ClickHouse HTTP port", cx)?,
            ch_native_port: self.port(&f.ch_native_port, "ClickHouse native port", cx)?,
            http_port: self.port(&f.http_port, "Web port", cx)?,
            mqtt_port: self.port(&f.mqtt_port, "MQTT port", cx)?,
            mqtts_port: self.port(&f.mqtts_port, "MQTT over TLS port", cx)?,
            coap_port: self.port(&f.coap_port, "CoAP port", cx)?,
            coaps_port: self.port(&f.coaps_port, "CoAP over DTLS port", cx)?,
            load_demo: self.load_demo,
            ..DeployConfig::default()
        };
        stack.tuning.instance_id = format!("sentient-{}", native::generate_secret().get(..8).unwrap_or("01"));
        Ok(plan::InstallPlan {
            engine: self.engine,
            sentient_version: self.sentient_version.to_string(),
            install_dir: PathBuf::from(self.value(&f.install_dir, cx)),
            data_dir: PathBuf::from(self.value(&f.data_dir, cx)),
            stack,
            pg_superuser_password: self.value(&f.pg_admin_password, cx),
            admin_email: self.value(&f.admin_email, cx),
            admin_password: self.value(&f.admin_password, cx),
            settings: self.chosen_settings(cx),
            shortcuts: plan::Shortcuts {
                autostart: self.autostart,
                desktop_sentient: self.desktop_sentient,
                desktop_manager: self.desktop_manager,
                start_menu: self.start_menu,
                kiosk: self.kiosk,
                open_at_signin: self.open_at_signin,
                auto_signin: if self.open_at_signin && self.autologin {
                    self.value(&f.autologin_user, cx)
                } else {
                    String::new()
                },
            },
            auto_signin_password: self.fields.autologin_password.read(cx).value().to_string(),
            bundle: self.bundle.clone(),
        })
    }

    /// The pages this install goes through: all of them, or for the Platform
    /// Manager alone only Welcome, Shortcuts and the last three.
    fn shown(&self, step: usize) -> bool {
        !self.manager_only || matches!(step, 0 | SHORTCUTS | SUMMARY | INSTALL | FINISH)
    }

    fn title(&self, step: usize) -> &'static str {
        if self.manager_only && step == SHORTCUTS { "Shortcuts" } else { STEPS[step] }
    }

    fn shown_steps(&self) -> Vec<usize> {
        (0..STEPS.len()).filter(|s| self.shown(*s)).collect()
    }

    fn next_shown(&self, from: usize) -> usize {
        (from + 1..STEPS.len()).find(|s| self.shown(*s)).unwrap_or(FINISH)
    }

    fn previous_shown(&self, from: usize) -> usize {
        (0..from).rev().find(|s| self.shown(*s)).unwrap_or(0)
    }

    fn start_install(&mut self, cx: &mut Context<Self>) {
        if self.manager_only {
            let shortcuts = plan::Shortcuts {
                autostart: false,
                desktop_sentient: false,
                desktop_manager: self.desktop_manager,
                start_menu: self.start_menu,
                kiosk: false,
                open_at_signin: false,
                auto_signin: String::new(),
            };
            self.problem = None;
            self.step = INSTALL;
            self.install.update(cx, |job, cx| job.start(move |sink| plan::run_manager_only(shortcuts, sink), cx));
            cx.notify();
            return;
        }
        let plan = match self.plan(cx) {
            Ok(plan) => plan,
            Err(e) => {
                self.problem = Some(e.into());
                cx.notify();
                return;
            }
        };
        self.problem = None;
        self.step = INSTALL;
        self.install.update(cx, |job, cx| job.start(move |sink| plan::run(plan, sink), cx));
        cx.notify();
    }

    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            FINISH => window.remove_window(),
            SUMMARY => self.start_install(cx),
            INSTALL => {
                // Only reachable when the install failed: try again.
                if matches!(self.install.read(cx).state(), JobState::Failed(_)) {
                    self.start_install(cx);
                }
            }
            _ => match self.validate(cx) {
                Ok(()) => {
                    self.problem = None;
                    self.step = self.next_shown(self.step);
                }
                Err(e) => self.problem = Some(e.into()),
            },
        }
        cx.notify();
    }

    fn go(&mut self, step: usize, cx: &mut Context<Self>) {
        // Finished pages can be revisited until the install starts.
        if step < self.step && self.step < INSTALL {
            self.step = step;
            self.problem = None;
            cx.notify();
        }
    }

    fn confirm_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let installing = self.step == INSTALL && self.install.read(cx).is_running();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Cancel setup?")
                .description(if installing {
                    "The install stops after the current task. Anything already installed stays on this computer; run setup again to finish or remove it."
                } else {
                    "Nothing has been installed yet."
                })
                .confirm()
                .ok_text("Cancel setup")
                .on_ok(|_, window, _| {
                    window.remove_window();
                    true
                })
        });
    }

    fn steps_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.shown_steps().into_iter().enumerate().map(|(n, i)| {
            let title = self.title(i);
            let current = i == self.step;
            let done = i < self.step;
            let mark = div()
                .size_5()
                .flex_shrink_0()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .map(|m| {
                    if current {
                        m.bg(theme.primary).text_color(theme.primary_foreground).child(format!("{}", n + 1))
                    } else if done {
                        m.bg(theme.success).text_color(theme.success_foreground)
                            .child(Icon::new(IconName::Check).xsmall())
                    } else {
                        m.border_1().border_color(theme.input).text_color(theme.muted_foreground)
                            .child(format!("{}", n + 1))
                    }
                });
            h_flex()
                .id(("step", i))
                .gap_3()
                .px_4()
                .py_1p5()
                .text_sm()
                .when(current, |r| r.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                .when(!current && !done, |r| r.text_color(theme.muted_foreground))
                .when(done && self.step < INSTALL, |r| r.cursor_pointer())
                .child(mark)
                .child(title)
                .on_click(cx.listener(move |this, _, _, cx| this.go(i, cx)))
        });
        v_flex()
            .w_56()
            .h_full()
            .flex_shrink_0()
            .py_4()
            .gap_0p5()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                v_flex()
                    .px_4()
                    .pb_4()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).text_color(theme.primary).child("SENTIENT"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(if self.manager_only {
                        "Platform Manager"
                    } else {
                        "Platform + Platform Manager"
                    })),
            )
            .children(rows)
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let failed = matches!(self.install.read(cx).state(), JobState::Failed(_));
        // While the page can't be left, Next is off and says why.
        let blocked = if self.step < SUMMARY { self.validate(cx).err() } else { None };
        let next_label = match self.step {
            SUMMARY => "Install",
            INSTALL if failed => "Retry",
            INSTALL => "Installing…",
            FINISH => "Close",
            _ => "Next",
        };
        h_flex()
            .h_16()
            .flex_shrink_0()
            .px_8()
            .gap_2()
            .items_center()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(div().text_xs().text_color(theme.muted_foreground).child({
                let shown = self.shown_steps();
                let at = shown.iter().position(|s| *s == self.step).unwrap_or(0);
                format!("Step {} of {}", at + 1, shown.len())
            }))
            .map(|bar| match (self.problem.clone(), blocked.clone()) {
                (Some(problem), _) => bar.child(div().ml_4().min_w_0().text_sm().text_color(theme.danger).child(problem)),
                (None, Some(reason)) => bar.child(
                    h_flex()
                        .ml_4()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .gap_1p5()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::Info).small())
                        .child(div().min_w_0().truncate().child(reason)),
                ),
                (None, None) => bar,
            })
            .child(div().flex_1())
            .when(self.step < FINISH, |bar| {
                bar.child(
                    Button::new("cancel")
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, window, cx| this.confirm_cancel(window, cx))),
                )
            })
            .when(self.step > 0 && self.step < INSTALL, |bar| {
                bar.child(
                    Button::new("back")
                        .outline()
                        .label("Back")
                        .on_click(cx.listener(|this, _, _, cx| this.go(this.previous_shown(this.step), cx))),
                )
            })
            .child(
                Button::new("next")
                    .primary()
                    .label(next_label)
                    .disabled((self.step == INSTALL && !failed) || blocked.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
            )
    }
}

impl Render for Installer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = self.steps_nav(cx);
        let footer = self.footer(cx);
        let page = self.page(window, cx);
        let theme = cx.theme().clone();
        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        v_flex()
                            .id("page")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p_8()
                            .gap_4()
                            .bg(theme.muted)
                            .child(div().text_2xl().child(self.title(self.step)))
                            .child(page),
                    )
                    .child(footer),
            )
    }
}
