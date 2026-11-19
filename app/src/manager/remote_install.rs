// Copyright © 2016-2026 The SENTIENT Authors

//! Install SENTIENT on a server (Status › Install SENTIENT on a server
//! without it; docs/mockups spm-093 A and B): Setup's steps for that
//! server, without Welcome, Configuration and Shortcuts. While it is open
//! its step rail stands in for the left menu (crate::manager draws it);
//! the top bar and the instance switcher stay.
//!
//! What can be chosen follows what the agent found there
//! (crate::remote::install::ServerFacts): an option the server cannot run
//! is greyed with an (i) saying why. The install runs on the server as a
//! job of the agent (crate::remote::install::start), followed in the shared
//! job view; it goes on when the connection drops, and a window opened
//! later shows it again (RemotePages re-attaches by the job's id). The
//! passwords go to the server on the agent's standard input and are kept
//! only in these fields while the flow is open.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::distro::DeployConfig;
use sentient_installer_core::progress::{PlanStep, Progress};
use sentient_installer_core::releases::Channel;
use sentient_installer_core::stack::Flavor;

use crate::installation::{Engine, flavor_label};
use crate::jobs::{Job, JobState};
use crate::manager::shell;
use crate::remote::install::{self as rinst, ServerFacts};
use crate::remote::{self, Instance, RemoteStatus};
use crate::ui::{self, Tone};

/// The steps, in order: Setup's, without Welcome, Configuration and Shortcuts.
pub const STEPS: [&str; 9] = ["System check", "Location", "Database", "Database settings", "Network ports", "Administrator", "Summary", "Install", "Finish"];
pub const CHECK: usize = 0;
pub const LOCATION: usize = 1;
pub const DATABASE: usize = 2;
pub const DB_SETTINGS: usize = 3;
pub const PORTS: usize = 4;
pub const ADMIN: usize = 5;
pub const SUMMARY: usize = 6;
pub const INSTALL: usize = 7;
pub const FINISH: usize = 8;

/// How the flow was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Close {
    /// Cancel: nothing was installed.
    Cancel,
    /// Done after the install: the instance's Status opens.
    Done(Engine),
}

struct Fields {
    install_dir: Entity<InputState>,
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
}

impl Fields {
    fn all(&self) -> [&Entity<InputState>; 19] {
        [
            &self.install_dir, &self.pg_db, &self.pg_user, &self.pg_password, &self.pg_admin_password, &self.pg_port, &self.ch_db, &self.ch_user, &self.ch_password, &self.ch_http_port, &self.ch_native_port,
            &self.http_port, &self.mqtt_port, &self.mqtts_port, &self.coap_port, &self.coaps_port, &self.admin_email, &self.admin_password, &self.admin_confirm,
        ]
    }
}

pub struct InstallFlow {
    inst: Instance,
    step: usize,
    facts: Option<Result<ServerFacts, String>>,
    /// The ports are being checked free on the server (Network ports' Next).
    ports_checking: bool,
    engine: Engine,
    flavor: Flavor,
    /// The newest SENTIENT on this Platform Manager's channel (None while looked for).
    version: Option<Result<Option<String>, String>>,
    /// That release's manifest `min_glibc`, when it says (None: the
    /// release table, sentient_installer_core::glibc::min_glibc).
    min_glibc: Option<String>,
    channel: Channel,
    fields: Fields,
    /// Why Next was refused.
    problem: Option<SharedString>,
    job: Entity<Job>,
    /// The server's status once the install went through (Finish's version and address).
    finished: Option<RemoteStatus>,
    /// The sudo password was asked for after the last failure.
    sudo_asked: bool,
    on_close: Rc<dyn Fn(Close, &mut Window, &mut App)>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl InstallFlow {
    pub fn new(inst: Instance, on_close: Rc<dyn Fn(Close, &mut Window, &mut App)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let d = DeployConfig::default();
        let input = |window: &mut Window, cx: &mut Context<Self>, v: &str| {
            let v = v.to_string();
            cx.new(|cx| InputState::new(window, cx).default_value(v))
        };
        let secret = |window: &mut Window, cx: &mut Context<Self>, v: &str| {
            let v = v.to_string();
            cx.new(|cx| InputState::new(window, cx).masked(true).default_value(v))
        };
        let fields = Fields {
            install_dir: input(window, cx, "/opt/sentient"),
            pg_db: input(window, cx, &d.db_name),
            pg_user: input(window, cx, &d.db_user),
            pg_password: secret(window, cx, &d.db_password),
            pg_admin_password: secret(window, cx, &d.db_password),
            pg_port: input(window, cx, &d.pg_port.to_string()),
            ch_db: input(window, cx, &d.ch_db),
            ch_user: input(window, cx, &d.ch_user),
            ch_password: secret(window, cx, &d.ch_password),
            ch_http_port: input(window, cx, &d.ch_http_port.to_string()),
            ch_native_port: input(window, cx, &d.ch_native_port.to_string()),
            http_port: input(window, cx, &d.http_port.to_string()),
            mqtt_port: input(window, cx, &d.mqtt_port.to_string()),
            mqtts_port: input(window, cx, &d.mqtts_port.to_string()),
            coap_port: input(window, cx, &d.coap_port.to_string()),
            coaps_port: input(window, cx, &d.coaps_port.to_string()),
            admin_email: input(window, cx, &if cfg!(debug_assertions) { std::env::var("SPM_DEBUG_INSTALL_ADMIN").unwrap_or_default() } else { String::new() }),
            admin_password: secret(window, cx, ""),
            admin_confirm: secret(window, cx, ""),
        };
        let mut subs: Vec<Subscription> = fields.all().into_iter().map(|f| cx.subscribe(f, |this: &mut Self, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.problem = None;
                cx.notify();
            }
        })).collect();
        let job = cx.new(|_| Job::new());
        subs.push(cx.observe(&job, |this, job, cx| {
            let state = job.read(cx).state().clone();
            match state {
                JobState::Done(_) if this.step == INSTALL => {
                    this.step = FINISH;
                    let inst = this.inst.clone();
                    let t = cx.spawn(async move |this, cx| {
                        let st = cx.background_executor().spawn(async move { remote::status(&inst) }).await;
                        this.update(cx, |f, cx| {
                            f.finished = st.ok();
                            cx.notify();
                        })
                        .ok();
                    });
                    this._tasks.push(t);
                }
                // sudo wants a password: ask for it, then the install again.
                JobState::Failed(m) if crate::manager::remote_pages::needs_sudo_password(&m) && !this.sudo_asked => {
                    this.sudo_asked = true;
                    if let Some(retry) = job.read(cx).retry() {
                        crate::manager::remote_pages::ask_sudo_password(this.inst.clone(), retry, cx);
                    }
                }
                _ => {}
            }
            cx.notify();
        }));
        let channel = crate::updates::Prefs::load().channel;
        let mut flow = Self {
            inst,
            step: CHECK,
            facts: None,
            ports_checking: false,
            engine: Engine::Native,
            flavor: Flavor::Clickhouse,
            version: None,
            min_glibc: None,
            channel,
            fields,
            problem: None,
            job,
            finished: None,
            sudo_asked: false,
            on_close,
            _tasks: Vec::new(),
            _subscriptions: subs,
        };
        flow.check(cx);
        let t = cx.spawn(async move |this, cx| {
            let (v, min) = cx
                .background_executor()
                .spawn(async move {
                    let v = crate::updates::newest_sentient(channel);
                    // The glibc its Linux server needs, as its manifest says.
                    let min = match &v {
                        Ok(Some(v)) if std::env::var_os("SPM_DEBUG_INSTALL_VERSION").is_none() => {
                            sentient_installer_core::manifest::fetch(&sentient_installer_core::manifest::release_url(v)).ok().and_then(|m| m.min_glibc)
                        }
                        _ => None,
                    };
                    (v, min)
                })
                .await;
            this.update(cx, |f, cx| {
                f.version = Some(v);
                f.min_glibc = min;
                f.fit_engine_to_glibc();
                cx.notify();
            })
            .ok();
        });
        flow._tasks.push(t);
        // Screens: a step opened directly (SPM_DEBUG_INSTALL_STEP=<n>).
        #[cfg(debug_assertions)]
        if let Some(n) = std::env::var("SPM_DEBUG_INSTALL_STEP").ok().and_then(|s| s.parse::<usize>().ok()) {
            flow.step = n.min(FINISH);
            if let Ok(v) = std::env::var("SPM_DEBUG_INSTALL_VERSION") {
                flow.version = Some(Ok(Some(v)));
            }
            if flow.step == INSTALL {
                let plan = flow.plan(cx).ok();
                let steps = plan.map(|p| job_plan(&p)).unwrap_or_default();
                crate::manager::jobview::debug_job(&flow.job, steps, cx);
            }
        }
        flow
    }

    /// The flow shown for an install already running on the server (job
    /// `id`, started from this or another window): at Install, following it.
    pub fn reattach(&mut self, id: String, cx: &mut Context<Self>) {
        self.step = INSTALL;
        let inst = self.inst.clone();
        // The agent sends its own steps first (Progress::Plan) as the log is
        // read again from its start; these stand in until then.
        self.job.update(cx, |job, cx| {
            job.set_plan(vec![PlanStep::new(crate::jobplan::CONNECT), PlanStep::new("Installing SENTIENT").weight(6.)]);
            job.set_label(format!("Installing SENTIENT on {}", inst.name));
            job.set_on_page(true);
            job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
            job.start(
                move |sink| {
                    sink(Progress::Step { name: crate::jobplan::CONNECT.into() });
                    remote::follow(&inst, &id, &sink)
                },
                cx,
            )
        });
        cx.notify();
    }

    /// Read the server's facts again (System check, Check again).
    fn check(&mut self, cx: &mut Context<Self>) {
        self.facts = None;
        self.problem = None;
        cx.notify();
        let inst = self.inst.clone();
        let t = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { rinst::facts(&inst, &[]) }).await;
            this.update(cx, |f, cx| {
                if let Ok(facts) = &r {
                    // The first reading picks what the server can run.
                    if f.facts.is_none() && rinst::engine_offer(facts, f.engine, "").is_err() {
                        f.engine = rinst::default_engine(facts);
                    }
                    if rinst::flavor_offer(facts, f.engine, f.flavor).is_err() {
                        f.flavor = rinst::default_flavor(facts, f.engine);
                    }
                }
                f.facts = Some(r);
                f.fit_engine_to_glibc();
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(t);
    }

    fn value(&self, field: &Entity<InputState>, cx: &App) -> String {
        field.read(cx).value().trim().to_string()
    }

    fn port(&self, field: &Entity<InputState>, name: &str, cx: &App) -> Result<u16, String> {
        self.value(field, cx).parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| format!("{name}: enter a port from 1 to 65535."))
    }

    /// Every port the configuration binds on the server.
    fn ports(&self, cx: &App) -> Result<Vec<(u16, &'static str)>, String> {
        let f = &self.fields;
        let mut v = vec![
            (self.port(&f.http_port, "Web port", cx)?, "Web"),
            (self.port(&f.mqtt_port, "MQTT port", cx)?, "MQTT"),
            (self.port(&f.mqtts_port, "MQTT over TLS port", cx)?, "MQTT over TLS"),
            (self.port(&f.coap_port, "CoAP port", cx)?, "CoAP"),
            (self.port(&f.coaps_port, "CoAP over DTLS port", cx)?, "CoAP over DTLS"),
            (self.port(&f.pg_port, "PostgreSQL port", cx)?, "PostgreSQL"),
        ];
        if self.flavor.uses_clickhouse() {
            v.push((self.port(&f.ch_http_port, "ClickHouse HTTP port", cx)?, "ClickHouse HTTP"));
            v.push((self.port(&f.ch_native_port, "ClickHouse native port", cx)?, "ClickHouse native"));
        }
        let mut seen = std::collections::HashSet::new();
        if let Some((p, n)) = v.iter().find(|(p, _)| !seen.insert(*p)) {
            return Err(format!("{n}: port {p} is used twice."));
        }
        Ok(v)
    }

    fn facts_got(&self) -> Option<&ServerFacts> {
        self.facts.as_ref().and_then(|r| r.as_ref().ok())
    }

    /// Whether a native SENTIENT (the version installed) runs on the
    /// server's glibc; Allowed until the facts and the version are read.
    fn native_glibc(&self) -> sentient_installer_core::glibc::NativeGlibc {
        use sentient_installer_core::glibc;
        match (self.facts_got(), self.version_text()) {
            (Some(f), Some(v)) => {
                let min = glibc::min_glibc(self.min_glibc.as_deref(), &v);
                // An agent older than 0.9.3 does not say: what the first
                // connection read (crate::remote::detect) then.
                match self.inst.detected.as_ref().map(|d| &d.libc).filter(|l| f.libc == glibc::Libc::Unknown && **l != glibc::Libc::Unknown) {
                    Some(l) => ServerFacts { libc: l.clone(), ..f.clone() }.native_glibc(&v, min),
                    None => f.native_glibc(&v, min),
                }
            }
            _ => glibc::NativeGlibc::Allowed,
        }
    }

    /// Native cannot run on the server: Docker is chosen when it can be
    /// (both off: Next says why).
    fn fit_engine_to_glibc(&mut self) {
        if self.engine != Engine::Native || self.native_glibc().blocked().is_none() {
            return;
        }
        let Some(f) = self.facts_got() else { return };
        if rinst::engine_offer(f, Engine::Docker, "").is_ok() {
            let flavor = if rinst::flavor_offer(f, Engine::Docker, self.flavor).is_err() { Some(rinst::default_flavor(f, Engine::Docker)) } else { None };
            self.engine = Engine::Docker;
            if let Some(fl) = flavor {
                self.flavor = fl;
            }
        }
    }

    /// Why the current step cannot be left with Next (None: it can).
    fn validate(&self, cx: &App) -> Result<(), String> {
        let f = &self.fields;
        match self.step {
            CHECK => {
                let facts = match &self.facts {
                    None => return Err(format!("Checking {}…", self.inst.name)),
                    Some(Err(e)) => return Err(e.lines().next().unwrap_or("The check did not finish.").to_string()),
                    Some(Ok(facts)) => facts,
                };
                rinst::engine_offer(facts, self.engine, &self.inst.name).map_err(|e| headline(&e))?;
                if self.engine == Engine::Native {
                    if let Some(why) = self.native_glibc().blocked() {
                        // Still native after fit_engine_to_glibc: no Docker either.
                        return Err(sentient_installer_core::glibc::and_no_docker(why, sentient_installer_core::glibc::Here::Server));
                    }
                }
                if facts.checks(self.engine).iter().any(|c| c.blocks()) {
                    return Err("Resolve the items marked “Blocker” on the server first, then check again.".into());
                }
                match &self.version {
                    None => Err("Looking for the newest SENTIENT…".into()),
                    Some(Ok(Some(_))) => Ok(()),
                    Some(Ok(None)) => Err(format!("No SENTIENT is published on the {} channel.", self.channel.label())),
                    Some(Err(e)) => Err(format!("The SENTIENT releases could not be read: {e}")),
                }
            }
            LOCATION => {
                let dir = self.value(&f.install_dir, cx);
                if !dir.starts_with('/') || dir.contains(['\'', '"', '\n']) {
                    Err("Program folder: a full path on the server, such as /opt/sentient.".into())
                } else {
                    Ok(())
                }
            }
            DATABASE => match self.facts_got() {
                Some(facts) => rinst::flavor_offer(facts, self.engine, self.flavor).map_err(|e| headline(&e)),
                None => Ok(()),
            },
            DB_SETTINGS => {
                let mut required = vec![(&f.pg_db, "Database name"), (&f.pg_user, "Database user"), (&f.pg_password, "Database password"), (&f.pg_admin_password, "PostgreSQL admin password")];
                if self.flavor.uses_clickhouse() {
                    required.extend([(&f.ch_db, "ClickHouse database"), (&f.ch_user, "ClickHouse user"), (&f.ch_password, "ClickHouse password")]);
                }
                if let Some((_, name)) = required.into_iter().find(|(s, _)| self.value(s, cx).is_empty()) {
                    return Err(format!("{name} is required."));
                }
                self.ports(cx).map(|_| ())
            }
            PORTS => self.ports(cx).map(|_| ()),
            ADMIN => {
                let email = self.value(&f.admin_email, cx);
                let password = f.admin_password.read(cx).value().to_string();
                if !email.contains('@') {
                    Err("Enter the administrator's e-mail address.".into())
                } else if password.chars().count() < 8 {
                    Err("The password must be at least 8 characters.".into())
                } else if password != f.admin_confirm.read(cx).value().as_ref() {
                    Err("The passwords do not match.".into())
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    /// The version installed: the newest on the channel.
    fn version_text(&self) -> Option<String> {
        self.version.as_ref().and_then(|v| v.as_ref().ok()).and_then(|v| v.clone())
    }

    /// The install plan for the server, as Setup builds it, with its
    /// passwords (they are taken out before it leaves this computer).
    fn plan(&self, cx: &App) -> Result<crate::installer::InstallPlan, String> {
        let f = &self.fields;
        let version = self.version_text().ok_or("No SENTIENT version was found to install.")?;
        let mut stack = DeployConfig {
            flavor: self.flavor,
            db_name: self.value(&f.pg_db, cx),
            db_user: self.value(&f.pg_user, cx),
            db_password: f.pg_password.read(cx).value().to_string(),
            pg_port: self.port(&f.pg_port, "PostgreSQL port", cx)?,
            ch_db: self.value(&f.ch_db, cx),
            ch_user: self.value(&f.ch_user, cx),
            ch_password: f.ch_password.read(cx).value().to_string(),
            ch_http_port: self.port(&f.ch_http_port, "ClickHouse HTTP port", cx)?,
            ch_native_port: self.port(&f.ch_native_port, "ClickHouse native port", cx)?,
            http_port: self.port(&f.http_port, "Web port", cx)?,
            mqtt_port: self.port(&f.mqtt_port, "MQTT port", cx)?,
            mqtts_port: self.port(&f.mqtts_port, "MQTT over TLS port", cx)?,
            coap_port: self.port(&f.coap_port, "CoAP port", cx)?,
            coaps_port: self.port(&f.coaps_port, "CoAP over DTLS port", cx)?,
            // The databases listen on the server's loopback only.
            db_loopback: true,
            ..DeployConfig::default()
        };
        if self.engine == Engine::Docker {
            stack.image = crate::installer::pinned_image(&stack.image, Some(&version));
        }
        stack.tuning.instance_id = format!("sentient-{}", sentient_installer_core::native::generate_secret().get(..8).unwrap_or("01"));
        Ok(crate::installer::InstallPlan {
            engine: self.engine,
            sentient_version: version.clone(),
            install_dir: self.value(&f.install_dir, cx).into(),
            data_dir: Default::default(),
            stack,
            pg_superuser_password: f.pg_admin_password.read(cx).value().to_string(),
            admin_email: self.value(&f.admin_email, cx),
            admin_password: f.admin_password.read(cx).value().to_string(),
            settings: Vec::new(),
            shortcuts: crate::installer::Shortcuts { autostart: true, desktop_sentient: false, desktop_manager: false, start_menu: false, kiosk: false, open_at_signin: false, auto_signin: String::new(), start_delay: 0 },
            auto_signin_password: String::new(),
            bundle: None,
            download: (self.engine == Engine::Native).then_some(version),
            existing: None,
            delete_existing: false,
            old_pg: Default::default(),
            wsl_restarted: false,
            remote: true,
            pack: None,
        })
    }

    fn go(&mut self, step: usize, cx: &mut Context<Self>) {
        if step < self.step && self.step < INSTALL {
            self.step = step;
            self.problem = None;
            cx.notify();
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.step > CHECK && self.step < INSTALL {
            self.go(self.step - 1, cx);
        }
    }

    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            FINISH => {
                let engine = self.engine;
                (self.on_close)(Close::Done(engine), window, cx);
            }
            INSTALL => {
                if matches!(self.job.read(cx).state(), JobState::Failed(_)) {
                    self.start_install(cx);
                }
            }
            SUMMARY => self.start_install(cx),
            PORTS => self.check_ports(cx),
            _ => match self.validate(cx) {
                Ok(()) => {
                    self.step += 1;
                    self.problem = None;
                    cx.notify();
                }
                Err(e) => {
                    self.problem = Some(e.into());
                    cx.notify();
                }
            },
        }
    }

    /// Network ports' Next: every chosen port checked free on the server.
    fn check_ports(&mut self, cx: &mut Context<Self>) {
        let ports = match self.ports(cx) {
            Ok(p) => p,
            Err(e) => {
                self.problem = Some(e.into());
                return cx.notify();
            }
        };
        self.ports_checking = true;
        self.problem = None;
        cx.notify();
        let inst = self.inst.clone();
        let t = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { rinst::facts(&inst, &ports) }).await;
            this.update(cx, |f, cx| {
                f.ports_checking = false;
                match r {
                    Ok(facts) => match facts.checks(f.engine).iter().find(|c| c.id == "ports" && c.status != "pass") {
                        Some(c) => f.problem = Some(c.detail.clone().into()),
                        None => {
                            f.step = ADMIN;
                            f.problem = None;
                        }
                    },
                    Err(e) => f.problem = Some(format!("The ports could not be checked on {}: {}", f.inst.name, e.lines().next().unwrap_or("")).into()),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(t);
    }

    fn start_install(&mut self, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let plan = match self.plan(cx) {
            Ok(p) => p,
            Err(e) => {
                self.problem = Some(e.into());
                return cx.notify();
            }
        };
        self.step = INSTALL;
        self.problem = None;
        let inst = self.inst.clone();
        let me = cx.entity().downgrade();
        let steps = job_plan(&plan);
        self.job.update(cx, |job, cx| {
            job.set_plan(steps);
            job.set_label(format!("Installing SENTIENT on {}", inst.name));
            job.set_on_page(true);
            job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
            job.set_retry(Rc::new(move |cx: &mut App| {
                me.update(cx, |f, cx| f.start_install(cx)).ok();
            }));
            job.start(
                move |sink| {
                    // The agent brought up to date first: an older one does
                    // not know the install jobs.
                    sink(Progress::Step { name: crate::jobplan::CONNECT.into() });
                    if let Err(e) = remote::deploy(&inst, &sink) {
                        sink(Progress::Log { line: format!("The agent could not be updated: {}", remote::split_failure(&e).0) });
                    }
                    let id = rinst::start(&inst, plan)?;
                    remote::follow(&inst, &id, &sink)
                },
                cx,
            )
        });
        cx.notify();
    }

    // -- rendering ----------------------------------------------------------

    /// Setup's step rail, in place of the left menu while the flow is open.
    /// `width`: the menu's (folded: the numbers only).
    pub fn rail(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let pad = shell::MENU_PAD;
        let folded = width < 150.;
        let active: &'static str = STEPS[self.step];
        let rows = STEPS.iter().enumerate().map(|(i, title)| {
            let current = i == self.step;
            let done = i < self.step;
            let mark = div()
                .relative()
                .size(px(shell::ICON))
                .flex_shrink_0()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui::layout_probe("menu-icon"))
                .map(|m| {
                    if current {
                        m.bg(theme.primary).text_color(theme.primary_foreground).child(format!("{}", i + 1))
                    } else if done {
                        m.bg(theme.primary.opacity(0.14)).text_color(theme.primary).child(Icon::new(IconName::Check).size(px(12.)))
                    } else {
                        m.border_1().border_color(theme.muted_foreground.opacity(0.6)).text_color(theme.muted_foreground).child(format!("{}", i + 1))
                    }
                });
            let title: &'static str = title;
            h_flex()
                .id(("rinst-step", i))
                .relative()
                .h(px(shell::ROW_H))
                .flex_shrink_0()
                .gap(px(12.))
                .pl(px(shell::row_pl(pad)))
                .when(!folded, |r| r.pr(px(12.)).mr(px(10.)))
                .rounded(px(8.))
                .text_sm()
                .font_weight(if current { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .text_color(if current {
                    shell::selected_label(&theme)
                } else if done {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(done && self.step < INSTALL, |r| r.cursor_pointer().hover(|r| r.bg(theme.list_hover)))
                .child(shell::row_probe(title))
                .child(ui::layout_probe("menu-row"))
                .child(mark)
                .when(!folded, |r| {
                    r.child(div().relative().min_w_0().whitespace_nowrap().text_ellipsis().child(title).child(ui::layout_probe(if current { "menu-label-on" } else { "menu-label" })))
                })
                .when(folded, |r| r.tooltip(ui::tooltip(title)))
                .on_click(cx.listener(move |this, _, _, cx| this.go(i, cx)))
        });
        let brand = h_flex()
            .relative()
            .h(px(shell::BAR_H))
            .flex_shrink_0()
            .items_center()
            .gap(px(10.))
            .pl(px(shell::ICON_X - shell::LOGO / 2.))
            .pr(px(10.))
            .child(ui::layout_probe("menu-brand"))
            .child(crate::win_caption::drag_layer())
            .child(div().relative().size(px(shell::LOGO)).flex_shrink_0().flex().items_center().justify_center().child(ui::product_logo(ui::Product::Sentient, shell::LOGO)).child(ui::layout_probe("menu-icon")))
            .when(!folded, |b| b.child(shell::brand_name(&theme, format!("SETUP ON {}", self.inst.name.to_uppercase()))));
        v_flex()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .relative()
            .overflow_hidden()
            .bg(shell::menu_bg(&theme, false, 0.))
            .child(shell::menu_probe(pad, None))
            .child(shell::selection(active, theme.muted, shell::menu_line(&theme), !ui::reduce_motion()))
            .child(brand)
            .child(
                v_flex()
                    .id("rinst-steps")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(px(shell::ROW_GAP))
                    .pt(px(shell::NAV_TOP))
                    .pb(px(12.))
                    .pl(px(pad))
                    .when(folded, |c| c.pr(px(pad)))
                    .children(rows),
            )
            .into_any_element()
    }

    fn subtitle(&self) -> String {
        let n = &self.inst.name;
        match self.step {
            CHECK => format!("What SENTIENT needs on {n}, checked there by its agent."),
            LOCATION => "Where SENTIENT and its database go on the server.".into(),
            DATABASE => "How SENTIENT stores its data. The type can be changed later with Migration in the Platform Manager.".into(),
            DB_SETTINGS => "The database SENTIENT signs in to.".into(),
            PORTS => "The ports SENTIENT listens on. They are checked free on the server before moving on.".into(),
            ADMIN => "The first SENTIENT sign-in, with full system rights.".into(),
            SUMMARY => format!("Check your choices. Nothing has been installed on {n} yet."),
            INSTALL => format!("The install runs on {n} through its agent. It goes on if this computer sleeps or the connection drops."),
            _ => format!("SENTIENT is installed and running on {n}."),
        }
    }

    fn label(theme: &gpui_kit::component::Theme, text: &'static str) -> Div {
        h_flex().h(px(22.)).gap_1().items_center().text_sm().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(text)
    }

    fn field(theme: &gpui_kit::component::Theme, text: &'static str, state: &Entity<InputState>) -> Div {
        v_flex().flex_1().min_w_0().gap_1p5().child(Self::label(theme, text)).child(ui::probed_input(Input::new(state)))
    }

    fn secret_field(theme: &gpui_kit::component::Theme, text: &'static str, help: Option<&'static str>, state: &Entity<InputState>) -> Div {
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1p5()
            .child(Self::label(theme, text).when_some(help, |l, h| l.child(ui::info(SharedString::from(format!("rinst-{text}-help")), h.into()))))
            .child(ui::probed_input(Input::new(state).mask_toggle()))
    }

    /// Two fields side by side.
    fn pair(a: Div, b: Div) -> Div {
        h_flex().gap(px(ui::GAP)).items_start().child(a).child(b)
    }

    fn check_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let name = self.inst.name.clone();
        let facts = self.facts_got();
        let glibc = self.native_glibc();
        let glibc_off = glibc.blocked().map(|why| format!("Not on this system\n\n{why}"));
        let card = |engine: Engine, title: &'static str, desc: &'static str, recommended: bool, cx: &mut Context<Self>| -> Stateful<Div> {
            let off = facts.and_then(|f| rinst::engine_offer(f, engine, &name).err()).or_else(|| glibc_off.clone().filter(|_| engine == Engine::Native));
            let id = if engine == Engine::Native { "rinst-native" } else { "rinst-docker" };
            match off {
                Some(why) => {
                    let title = h_flex().gap_2().items_center().child(div().font_weight(FontWeight::MEDIUM).opacity(ui::OFF_OPACITY).child(title)).child(ui::info(SharedString::from(format!("{id}-why")), why.into())).into_any_element();
                    ui::radio_card_off(&theme, id, title, "Not available on this server.").relative().child(ui::layout_probe("panel"))
                }
                None => {
                    let title = h_flex().gap_2().items_center().child(div().font_weight(FontWeight::MEDIUM).child(title)).when(recommended, |r| r.child(ui::pill(&theme, Tone::Good, "Recommended"))).into_any_element();
                    ui::radio_card_el(&theme, id, title, desc, self.engine == engine, true).relative().child(ui::layout_probe("panel")).on_click(cx.listener(move |this, _, _, cx| {
                        this.engine = engine;
                        if let Some(f) = this.facts_got() {
                            if rinst::flavor_offer(f, engine, this.flavor).is_err() {
                                this.flavor = rinst::default_flavor(f, engine);
                            }
                        }
                        this.problem = None;
                        cx.notify();
                    }))
                }
            }
        };
        let native = card(Engine::Native, "In the system", "Services from the distribution's packages, run by systemd.", true, cx);
        let docker = card(Engine::Docker, "Docker", "SENTIENT and its databases as containers. Every database type.", false, cx);
        let rows: Div = match &self.facts {
            None => ui::panel(&theme).child(div().text_sm().text_color(theme.muted_foreground).child(format!("Checking {name}…"))),
            Some(Err(e)) => ui::notice(&theme, Tone::Bad, format!("{name} could not be checked\n{}", e.lines().next().unwrap_or(""))),
            Some(Ok(f)) => {
                let list = f.checks(self.engine);
                ui::panel(&theme).gap_0().py(px(4.)).children(list.iter().enumerate().map(|(i, c)| {
                    let (tone, text) = match c.status.as_str() {
                        "pass" => (Tone::Good, "Ready"),
                        "setup" => (Tone::Warn, "Will be installed"),
                        "fail" => (Tone::Bad, "Blocker"),
                        "warn" => (Tone::Warn, "Attention"),
                        _ => (Tone::Neutral, "Unknown"),
                    };
                    let text = c.state.clone().unwrap_or_else(|| text.to_string());
                    h_flex()
                        .gap_3()
                        .items_start()
                        .py(px(10.))
                        .when(i > 0, |r| r.border_t_1())
                        .border_color(theme.border)
                        .child(div().w(px(150.)).flex_shrink_0().child(ui::pill(&theme, tone, text)))
                        .child(div().w(px(150.)).flex_shrink_0().text_sm().font_weight(FontWeight::MEDIUM).child(c.label.clone()))
                        .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(c.detail.clone()))
                }))
            }
        };
        let glibc_notice = match &glibc {
            sentient_installer_core::glibc::NativeGlibc::Blocked(why) => Some(ui::notice(&theme, Tone::Warn, format!("“In the system” is not available\n{why}"))),
            sentient_installer_core::glibc::NativeGlibc::Unknown(why) if self.engine == Engine::Native => Some(ui::notice(&theme, Tone::Warn, format!("The C library could not be checked\n{why}"))),
            _ => None,
        };
        v_flex()
            .gap(px(ui::GAP))
            .child(ui::card_pair(v_flex().child(native.flex_1()), v_flex().child(docker.flex_1())))
            .children(glibc_notice)
            .child(rows)
            .child(h_flex().child(crate::ui::button("rinst-recheck").outline().label("Check again").disabled(self.facts.is_none()).on_click(cx.listener(|this, _, _, cx| this.check(cx)))))
    }

    fn free_text(&self) -> String {
        self.facts_got().and_then(|f| f.disk_free).map(|b| format!("{} free on /", crate::dbstatus::bytes_label(b))).unwrap_or_default()
    }

    fn location_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        let data = match self.engine {
            Engine::Native => "PostgreSQL's own folder from the distribution's packages (/var/lib/postgresql)",
            Engine::Docker => "Docker volumes on the server",
        };
        ui::panel(&theme)
            .child(Self::field(&theme, "Program folder", &f.install_dir))
            .child(ui::fact(&theme, "Database folder", data))
            .when(!self.free_text().is_empty(), |p| p.child(ui::fact(&theme, "Disk", self.free_text())))
    }

    fn database_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let facts = self.facts_got().cloned().unwrap_or_default();
        let runs = |flavor: Flavor| -> Vec<String> {
            let place = if self.engine == Engine::Native { "in the system" } else { "in Docker" };
            match flavor {
                Flavor::Clickhouse => vec![format!("PostgreSQL: {place}"), format!("ClickHouse: {place}")],
                _ => vec![if self.engine == Engine::Native { "In the system".into() } else { "In Docker".into() }],
            }
        };
        let options: [(Flavor, &'static str); 3] = [
            (Flavor::Stock, "Simplest and smallest. For small sites and a few hundred devices."),
            (Flavor::Timescaledb, "Compressed time-series inside PostgreSQL."),
            (Flavor::Clickhouse, "Best for large telemetry volumes and long history."),
        ];
        let runs_col = |lines: Vec<String>, fade: f32| {
            v_flex().flex_shrink_0().items_end().gap_0p5().text_sm().text_color(theme.muted_foreground).when(fade < 1., |c| c.opacity(fade)).children(lines.into_iter().map(|l| div().whitespace_nowrap().child(l)))
        };
        let cards: Vec<AnyElement> = options
            .into_iter()
            .map(|(flavor, desc)| match rinst::flavor_offer(&facts, self.engine, flavor) {
                Ok(()) => {
                    let title = h_flex()
                        .gap_2()
                        .items_center()
                        .child(ui::flavor_logos(flavor, 18.))
                        .child(div().font_weight(FontWeight::MEDIUM).child(flavor_label(flavor)))
                        .when(flavor == Flavor::Clickhouse, |r| r.child(ui::pill(&theme, Tone::Good, "Recommended")))
                        .into_any_element();
                    ui::radio_card_el(&theme, SharedString::from(format!("rinst-{}", flavor.as_str())), title, desc, self.flavor == flavor, true)
                        .relative()
                        .child(ui::layout_probe("panel"))
                        .child(runs_col(runs(flavor), 1.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.flavor = flavor;
                            this.problem = None;
                            cx.notify();
                        }))
                        .into_any_element()
                }
                Err(why) => {
                    let title = h_flex()
                        .gap_2()
                        .items_center()
                        .child(h_flex().gap_2().items_center().opacity(ui::OFF_OPACITY).child(ui::flavor_logos(flavor, 18.)).child(div().font_weight(FontWeight::MEDIUM).child(flavor_label(flavor))))
                        .child(ui::info(SharedString::from(format!("rinst-{}-why", flavor.as_str())), why.into()))
                        .into_any_element();
                    let desc = format!("{} Not available with In the system on this server.", desc);
                    ui::radio_card_off(&theme, SharedString::from(format!("rinst-{}", flavor.as_str())), title, desc).relative().child(ui::layout_probe("panel")).child(runs_col(vec!["—".into()], ui::OFF_OPACITY)).into_any_element()
                }
            })
            .collect();
        let checked = [Some(if facts.os_name.is_empty() { "its packages".to_string() } else { format!("{} packages", facts.os_name) }), (!self.free_text().is_empty()).then(|| self.free_text())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        v_flex()
            .gap(px(ui::GAP))
            .children(cards)
            .child(ui::notice(&theme, Tone::Neutral, format!("Checked on {}\n{checked}.", self.inst.name)))
    }

    fn db_settings_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        const DEFAULT: &str = "Default password. Change it if the database port is reachable from other computers.";
        let pg = ui::panel(&theme)
            .gap(px(ui::GAP))
            .child(div().font_weight(FontWeight::MEDIUM).child("PostgreSQL"))
            .child(Self::pair(Self::field(&theme, "Database name", &f.pg_db), Self::field(&theme, "User", &f.pg_user)))
            .child(Self::pair(Self::secret_field(&theme, "Password", Some(DEFAULT), &f.pg_password), Self::secret_field(&theme, "PostgreSQL admin password", Some("The postgres account's password on the server. It goes there over the SSH connection and is not kept on this computer."), &f.pg_admin_password)))
            .child(Self::pair(Self::field(&theme, "Port", &f.pg_port), div().flex_1()));
        let ch = self.flavor.uses_clickhouse().then(|| {
            ui::panel(&theme)
                .gap(px(ui::GAP))
                .child(div().font_weight(FontWeight::MEDIUM).child("ClickHouse"))
                .child(Self::pair(Self::field(&theme, "Database name", &f.ch_db), Self::field(&theme, "User", &f.ch_user)))
                .child(Self::pair(Self::secret_field(&theme, "Password", Some(DEFAULT), &f.ch_password), div().flex_1()))
                .child(Self::pair(Self::field(&theme, "HTTP port", &f.ch_http_port), Self::field(&theme, "Native port", &f.ch_native_port)))
        });
        v_flex().gap(px(ui::GAP)).child(pg).children(ch)
    }

    fn ports_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        ui::panel(&theme)
            .gap(px(ui::GAP))
            .child(Self::pair(Self::field(&theme, "Web (HTTP)", &f.http_port), div().flex_1()))
            .child(Self::pair(Self::field(&theme, "MQTT", &f.mqtt_port), Self::field(&theme, "MQTT over TLS", &f.mqtts_port)))
            .child(Self::pair(Self::field(&theme, "CoAP", &f.coap_port), Self::field(&theme, "CoAP over DTLS", &f.coaps_port)))
    }

    fn admin_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        ui::panel(&theme)
            .gap(px(ui::GAP))
            .child(Self::pair(Self::field(&theme, "Email", &f.admin_email), div().flex_1()))
            .child(Self::pair(Self::secret_field(&theme, "Password (at least 8 characters)", None, &f.admin_password), Self::secret_field(&theme, "Confirm the password", None, &f.admin_confirm)))
    }

    /// Summary's rows: the label, the value and the step that changes it.
    fn summary_rows(&self, cx: &App) -> Vec<(&'static str, String, usize)> {
        let f = &self.fields;
        let v = |s: &Entity<InputState>| self.value(s, cx);
        let det = self.inst.detected.clone().unwrap_or_default();
        let os = self.facts_got().map(|f| f.os_name.clone()).filter(|s| !s.is_empty()).unwrap_or(det.os_name);
        let version = self.version_text().unwrap_or_else(|| "—".into());
        let mut rows = vec![
            ("Server", format!("{}, {} over SSH{}", self.inst.name, self.inst.host, if os.is_empty() { String::new() } else { format!(" ({os})") }), CHECK),
            (
                "Version",
                format!("SENTIENT {version}, {}, {}", self.channel.label(), if self.engine == Engine::Native { "downloaded on the server and checked before use" } else { "its image pulled on the server" }),
                CHECK,
            ),
            (
                "Runs as",
                match (self.engine, self.flavor.uses_clickhouse()) {
                    (Engine::Native, true) => "SENTIENT server, PostgreSQL and ClickHouse with systemd".into(),
                    (Engine::Native, false) => "SENTIENT server and PostgreSQL with systemd".into(),
                    (Engine::Docker, _) => "Containers in Docker on the server".into(),
                },
                CHECK,
            ),
            ("Program folder", v(&f.install_dir), LOCATION),
            ("Database", flavor_label(self.flavor).to_string(), DATABASE),
            ("PostgreSQL", format!("{} as {} on port {}", v(&f.pg_db), v(&f.pg_user), v(&f.pg_port)), DB_SETTINGS),
        ];
        if self.flavor.uses_clickhouse() {
            rows.push(("ClickHouse", format!("{} as {} on port {}", v(&f.ch_db), v(&f.ch_user), v(&f.ch_http_port)), DB_SETTINGS));
        }
        rows.push(("Ports", format!("Web {} · MQTT {} / {} · CoAP {} / {}", v(&f.http_port), v(&f.mqtt_port), v(&f.mqtts_port), v(&f.coap_port), v(&f.coaps_port)), PORTS));
        rows.push(("Administrator", v(&f.admin_email), ADMIN));
        rows.push(("Startup", "Starts with the server".into(), SUMMARY));
        rows
    }

    fn summary_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let rows = self.summary_rows(cx);
        let table = ui::panel(&theme).gap_0().py(px(8.)).children(rows.into_iter().enumerate().map(|(i, (label, value, step))| {
            h_flex()
                .gap_4()
                .py(px(6.))
                .items_center()
                .min_h(px(44.))
                .when(i > 0, |r| r.border_t_1())
                .border_color(theme.border)
                .text_sm()
                .child(div().w(px(ui::CARD_LABEL_W + 30.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(value))
                .when(step < SUMMARY, |r| {
                    r.child(crate::ui::button(SharedString::from(format!("rinst-change-{i}"))).ghost().label("Change").on_click(cx.listener(move |this, _, _, cx| this.go(step, cx))))
                })
        }));
        v_flex().gap(px(ui::GAP)).child(table).child(
            h_flex()
                .gap_1p5()
                .items_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::Info).size(px(14.)).flex_shrink_0())
                .child("The administrator password and the database passwords go to the server over the SSH connection. They are not kept on this computer."),
        )
    }

    fn install_page(&self, cx: &mut Context<Self>) -> Div {
        crate::manager::jobview::JobView::new("rinst-job", format!("Installing SENTIENT on {}", self.inst.name), &self.job).render(cx)
    }

    /// The address SENTIENT answers on, on the server.
    fn address(&self, cx: &App) -> String {
        let port = self.finished.as_ref().and_then(|s| s.http_port).map(|p| p.to_string()).unwrap_or_else(|| self.value(&self.fields.http_port, cx));
        format!("http://{}:{port}", self.inst.host)
    }

    fn finish_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let address = self.address(cx);
        let version = self.finished.as_ref().map(|s| s.comparable_version()).filter(|v| !v.is_empty()).or_else(|| self.version_text()).unwrap_or_default();
        let url = address.clone();
        ui::panel(&theme)
            .child(h_flex().gap_3().items_center().child(ui::product_logo(ui::Product::Sentient, 30.)).child(ui::pill(&theme, Tone::Good, "Installed")))
            .child(ui::fact(&theme, "Address", address))
            .when(!version.is_empty(), |p| p.child(ui::fact(&theme, "Version", version)))
            .child(ui::fact(&theme, "Administrator", self.value(&self.fields.admin_email, cx)))
            .children(ui::card_actions(
                &theme,
                h_flex().gap(crate::ui::button_gap()).child(crate::ui::button("rinst-open").outline().label("Open SENTIENT").on_click(move |_, _, cx| ui::open_url(&url, cx))),
            ))
    }

    /// Cancel, Back and Next under the page, as Setup's.
    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let job = self.job.read(cx);
        let failed = matches!(job.state(), JobState::Failed(_));
        let running = job.is_running();
        let again = match job.state() {
            JobState::Failed(m) => crate::ui::retry_label(m, "Retry"),
            _ => "Retry",
        };
        let blocked = if self.step < SUMMARY { self.validate(cx).err() } else { None };
        let next_label = match self.step {
            SUMMARY => "Install",
            PORTS if self.ports_checking => "Checking…",
            INSTALL if failed => again,
            INSTALL => "Installing…",
            FINISH => "Done",
            _ => "Next",
        };
        let next_off = (self.step == INSTALL && !failed) || self.ports_checking || blocked.is_some();
        h_flex()
            .flex_shrink_0()
            .px(px(ui::PAGE_PAD))
            .py(px(14.))
            .gap(crate::ui::button_gap())
            .items_center()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(div().flex_shrink_0().text_size(px(12.)).text_color(theme.muted_foreground).whitespace_nowrap().child(format!("Step {} of {}", self.step + 1, STEPS.len())))
            .map(|bar| match (self.problem.clone(), blocked.clone()) {
                (Some(problem), _) => bar.child(div().ml_4().flex_1().min_w_0().text_sm().line_clamp(2).text_color(theme.danger).child(problem)),
                (None, Some(reason)) => bar.child(
                    h_flex()
                        .ml_4()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::Info).size(px(16.)).flex_shrink_0())
                        .child(div().flex_1().min_w_0().line_clamp(2).child(reason)),
                ),
                (None, None) => bar.child(div().flex_1()),
            })
            // While the install runs it cannot be left from here (it goes on
            // on the server; the progress strip follows it).
            .when(self.step < FINISH && !running, |bar| {
                bar.child(crate::ui::probed(crate::ui::button("rinst-cancel").outline().label("Cancel").on_click(cx.listener(|this, _, window, cx| {
                    let close = this.on_close.clone();
                    close(Close::Cancel, window, cx);
                }))))
            })
            .when(self.step > CHECK && self.step < INSTALL, |bar| bar.child(crate::ui::probed(crate::ui::button("rinst-back").outline().label("Back").on_click(cx.listener(|this, _, _, cx| this.back(cx))))))
            .child(crate::ui::probed(crate::ui::button("rinst-next").primary().label(next_label).disabled(next_off).on_click(cx.listener(|this, _, window, cx| this.next(window, cx)))))
    }
}

/// An (i)'s first line (its title), for the footer's reason.
fn headline(why: &str) -> String {
    why.split("\n\n").next().unwrap_or(why).trim().to_string()
}

/// The install job's steps as the window shows them before the agent sends
/// its own: reaching the server, then Setup's steps for a server install.
pub fn job_plan(plan: &crate::installer::InstallPlan) -> Vec<PlanStep> {
    let mut v = vec![PlanStep::new(crate::jobplan::CONNECT).weight(0.5)];
    v.extend(crate::installer::plan_steps_on(plan, crate::installer::PlanHost { windows: false, wsl_ready: true, manager_in_place: true }));
    v
}

impl Render for InstallFlow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let body = match self.step {
            CHECK => self.check_page(cx),
            LOCATION => self.location_page(cx),
            DATABASE => self.database_page(cx),
            DB_SETTINGS => self.db_settings_page(cx),
            PORTS => self.ports_page(cx),
            ADMIN => self.admin_page(cx),
            SUMMARY => self.summary_page(cx),
            INSTALL => self.install_page(cx),
            _ => self.finish_page(cx),
        };
        let footer = self.footer(cx);
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .id(SharedString::from(format!("rinst-page-{}", self.step)))
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(ui::PAGE_PAD))
                    .gap(px(ui::GAP))
                    .bg(theme.muted)
                    .child(ui::layout_probe("page-area"))
                    .child(
                        v_flex()
                            .flex_shrink_0()
                            .gap_0p5()
                            .min_h(px(32.))
                            .justify_center()
                            .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child(STEPS[self.step]))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(self.subtitle())),
                    )
                    .child(body),
            )
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::{DeployConfig, Engine, Flavor, FINISH, INSTALL, STEPS, SUMMARY, job_plan};

    #[test]
    fn the_flow_is_setups_steps_without_welcome_configuration_and_shortcuts() {
        assert_eq!(STEPS, ["System check", "Location", "Database", "Database settings", "Network ports", "Administrator", "Summary", "Install", "Finish"]);
        for left_out in ["Welcome", "Configuration", "Shortcuts and startup"] {
            assert!(!STEPS.contains(&left_out));
        }
        assert_eq!((STEPS[SUMMARY], STEPS[INSTALL], STEPS[FINISH]), ("Summary", "Install", "Finish"));
    }

    #[test]
    fn a_server_install_neither_installs_the_manager_nor_makes_shortcuts() {
        let plan = crate::installer::InstallPlan {
            engine: Engine::Native,
            sentient_version: "4.4.0.3".into(),
            install_dir: "/opt/sentient".into(),
            data_dir: Default::default(),
            stack: DeployConfig { flavor: Flavor::Stock, ..DeployConfig::default() },
            pg_superuser_password: String::new(),
            admin_email: "a@b.c".into(),
            admin_password: String::new(),
            settings: Vec::new(),
            shortcuts: crate::installer::Shortcuts { autostart: true, desktop_sentient: false, desktop_manager: false, start_menu: false, kiosk: false, open_at_signin: false, auto_signin: String::new(), start_delay: 0 },
            auto_signin_password: String::new(),
            bundle: None,
            download: Some("4.4.0.3".into()),
            existing: None,
            delete_existing: false,
            old_pg: Default::default(),
            wsl_restarted: false,
            remote: true,
            pack: None,
        };
        let names: Vec<String> = job_plan(&plan).into_iter().map(|s| s.name).collect();
        assert_eq!(names, [crate::jobplan::CONNECT, "Downloading SENTIENT 4.4.0.3", "Installing SENTIENT and its database", "Setting the administrator sign-in", "Saving the installation record"]);
    }
}
