// Copyright © 2016-2026 The SENTIENT Authors

//! Install mode: the setup wizard (docs/GPUI_PLATFORM_MANAGER_PLAN.md,
//! "Installer wizard"). Twelve pages: ten collect choices without touching the
//! machine, Install runs the plan, Finish says what to do next.

mod old_pg;
mod pages;
mod plan;

pub(crate) use plan::{InstallPlan, run as run_install, run_manager_only};

#[cfg(windows)]
pub use plan::provision_clickhouse_wsl;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, h_flex, v_flex,
    button::ButtonVariants as _,
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
use crate::ui;

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
    id: String,
    label: SharedString,
    detail: SharedString,
    status: Status,
    /// The pill's words, when the status's own would be wrong.
    state: Option<SharedString>,
    /// Its actions and commands (checks::Check::fixes).
    fixes: Vec<sentient_installer_core::diagnose::Fix>,
}

impl CheckRow {
    fn from(c: checks::Check) -> Self {
        CheckRow { id: c.id, label: c.label.into(), detail: c.detail.into(), status: c.status, state: c.state.map(Into::into), fixes: c.fixes }
    }
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
    /// Seconds SENTIENT waits after Windows starts (with autostart).
    start_delay: Entity<InputState>,
    search: Entity<InputState>,
    /// Docker: Advanced data locations (mockup M14). Empty: Docker's own
    /// folder. One folder for every store, or one per store.
    loc_one: Entity<InputState>,
    loc_pg: Entity<InputState>,
    loc_ch: Entity<InputState>,
    loc_data: Entity<InputState>,
}

pub struct Installer {
    step: usize,
    /// How SENTIENT runs: always native on Windows; Docker or native on Linux.
    engine: Engine,
    accepted: bool,
    /// Install the Platform Manager alone, without SENTIENT.
    manager_only: bool,
    /// Started from the installed Platform Manager to add SENTIENT
    /// (`--add-sentient`): no choice of what to install, and nothing about
    /// installing the Platform Manager itself.
    pub(crate) adding_sentient: bool,
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
    /// Docker: a data place per store (else one folder for all).
    loc_per_store: bool,
    desktop_sentient: bool,
    desktop_manager: bool,
    start_menu: bool,
    kiosk: bool,
    open_at_signin: bool,
    /// Sign in to the computer automatically (only with open_at_signin).
    autologin: bool,
    bundle: Option<PathBuf>,
    /// No package: the latest release online (None while looking).
    online: Option<Result<sentient_installer_core::manifest::Manifest, String>>,
    /// No package: the published versions to choose from (None while
    /// looking; an error leaves the latest release alone, as before).
    versions: Option<Result<Vec<crate::online::Version>, String>>,
    /// The version picked from `versions` (None: the newest).
    chosen_version: Option<String>,
    sentient_version: SharedString,
    /// Why Next is refused on the current page.
    problem: Option<SharedString>,
    /// A database an earlier install left (where), found on the way to
    /// Database settings, and whether to keep using it.
    existing: Option<String>,
    keep_existing: bool,
    /// Start fresh by deleting the earlier database instead of keeping a copy.
    delete_existing: bool,
    /// The earlier install's passwords were found and filled in.
    kept_passwords: bool,
    fields: Fields,
    install: Entity<Job>,
    /// What an earlier install left (crate::leftovers), found with the checks.
    leftovers: Vec<String>,
    /// Removing those leftovers.
    cleanup: Entity<Job>,
    /// An earlier install that is broken or half removed: its own screen,
    /// before the wizard (crate::earlier).
    earlier: Option<Entity<crate::earlier::EarlierPage>>,
    /// That screen was answered (or there was nothing): do not show it again.
    earlier_done: bool,
    /// Who holds a busy port (checks::port_owner), looked up once per port.
    port_owners: HashMap<u16, Option<checks::PortOwner>>,
    /// A System check row's action (stop a service, remove a PostgreSQL,
    /// stop a container), run as its own job with one password prompt;
    /// the check runs again when it ends.
    pub(crate) check_fix: Entity<Job>,
    /// The row and the label of the action that runs (or ran).
    pub(crate) check_fix_at: Option<(String, String)>,
    /// Run the check again once `check_fix` ends.
    check_fix_watch: bool,
    /// The rows whose commands are shown ("Show command").
    pub(crate) commands_shown: HashSet<String>,
    /// Native Linux: a PostgreSQL data folder from an older major, and
    /// what to do with it (crate::installer::old_pg).
    pub(crate) old_pg: Option<sentient_installer_core::old_pg::OldPgData>,
    pub(crate) old_pg_choice: sentient_installer_core::old_pg::OldPgChoice,
    old_pg_probing: bool,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A start delay as typed: whole seconds, 0 to 3600.
pub(crate) fn parse_start_delay(text: &str) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(n) if n <= 3600 => Ok(n),
        _ => Err("Start delay: enter whole seconds from 0 to 3600 (0 starts SENTIENT as soon as Windows starts).".into()),
    }
}

fn input(window: &mut Window, cx: &mut Context<Installer>, value: &str) -> Entity<InputState> {
    let value = value.to_string();
    cx.new(|cx| InputState::new(window, cx).default_value(value))
}

/// A data location field (empty: Docker's own folder); screens fill it
/// from `debug_env`.
fn loc_input(window: &mut Window, cx: &mut Context<Installer>, debug_env: &str) -> Entity<InputState> {
    let value = if cfg!(debug_assertions) { std::env::var(debug_env).unwrap_or_default() } else { String::new() };
    cx.new(|cx| InputState::new(window, cx).placeholder("Docker's own folder").default_value(value))
}

fn secret(window: &mut Window, cx: &mut Context<Installer>) -> Entity<InputState> {
    secret_with(window, cx, "")
}

/// A masked field holding `value` (the database passwords' default, still
/// editable; their (i) says to change it when the port is reachable).
fn secret_with(window: &mut Window, cx: &mut Context<Installer>, value: &str) -> Entity<InputState> {
    let value = value.to_string();
    cx.new(|cx| InputState::new(window, cx).masked(true).default_value(value))
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
            // The database passwords start as the default ("sentient"):
            // the databases listen on 127.0.0.1 only (Docker publishes their
            // ports there; natively they listen on localhost).
            pg_password: secret_with(window, cx, &d.db_password),
            pg_admin_password: secret_with(window, cx, &d.db_password),
            pg_port: input(window, cx, &d.pg_port.to_string()),
            ch_db: input(window, cx, &d.ch_db),
            ch_user: input(window, cx, &d.ch_user),
            ch_password: secret_with(window, cx, &d.ch_password),
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
            start_delay: input(window, cx, &sentient_installer_core::native::DEFAULT_START_DELAY_SECS.to_string()),
            search: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search by name or variable, e.g. batch or RULE_ENGINE")
            }),
            loc_one: loc_input(window, cx, "SPM_DEBUG_LOC_ONE"),
            loc_pg: loc_input(window, cx, "SPM_DEBUG_LOC_PG"),
            loc_ch: loc_input(window, cx, "SPM_DEBUG_LOC_CH"),
            loc_data: loc_input(window, cx, "SPM_DEBUG_LOC_DATA"),
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
        let cleanup = cx.new(|_| Job::new());
        let check_fix = cx.new(|_| Job::new());
        let check_fix_sub = cx.observe(&check_fix, |this, job, cx| {
            if this.check_fix_watch && !job.read(cx).is_running() {
                this.check_fix_watch = false;
                this.run_checks(cx);
            }
            cx.notify();
        });
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
            adding_sentient: std::env::args().any(|a| a == "--add-sentient"),
            checks: None,
            flavor: None,
            load_demo: false,
            customize: false,
            groups,
            ram_mb,
            recommended,
            settings,
            autostart: true,
            loc_per_store: false,
            desktop_sentient: true,
            desktop_manager: true,
            start_menu: true,
            kiosk: false,
            open_at_signin: false,
            autologin: false,
            bundle,
            online: None,
            versions: None,
            chosen_version: None,
            sentient_version: sentient_version.into(),
            problem: None,
            existing: None,
            leftovers: Vec::new(),
            cleanup,
            port_owners: HashMap::new(),
            check_fix,
            check_fix_at: None,
            check_fix_watch: false,
            commands_shown: HashSet::new(),
            old_pg: None,
            old_pg_choice: Default::default(),
            old_pg_probing: false,
            earlier: None,
            earlier_done: resumed.is_some(),
            keep_existing: true,
            delete_existing: false,
            kept_passwords: false,
            fields,
            install,
            _tasks: Vec::new(),
            _subscriptions: vec![search_sub, install_sub, check_fix_sub],
        };
        // Enter: the next field of the step, and on its last one Next (a
        // step that is not valid keeps its message and focuses the first
        // empty field).
        let f = &this.fields;
        let all = [
            &f.pg_db, &f.pg_user, &f.pg_port, &f.pg_password, &f.pg_admin_password, &f.ch_db, &f.ch_user, &f.ch_password, &f.ch_http_port, &f.ch_native_port,
            &f.http_port, &f.mqtt_port, &f.mqtts_port, &f.coap_port, &f.coaps_port, &f.admin_email, &f.admin_password, &f.admin_confirm, &f.autologin_user, &f.autologin_password,
        ]
        .map(|e| e.clone());
        let enter = crate::ui::enter_moves_on(&all, window, cx, |this: &Self, _| this.step_fields(), |this: &mut Self, window, cx| {
            let step = this.step;
            this.next(window, cx);
            if this.step != step || this.problem.is_none() {
                return None;
            }
            let fields = this.step_fields();
            fields.iter().find(|e| e.read(cx).value().trim().is_empty()).cloned().or_else(|| fields.first().cloned())
        });
        this._subscriptions.extend(enter);
        this.run_checks(cx);
        // The error panel's page fixes: another port, or the password
        // prompt again (both run the install again).
        let me = cx.entity().downgrade();
        this.install.update(cx, |job, _| {
            job.set_fix_handler(std::rc::Rc::new(move |action, window, cx| {
                me.update(cx, |page, cx| page.apply_fix(action, window, cx)).unwrap_or(false)
            }), true)
        });
        if let Some(plan) = resumed {
            this.accepted = true;
            this.flavor = Some(plan.stack.flavor);
            this.engine = plan.engine;
            this.step = INSTALL;
            let steps = plan::steps(&plan);
            this.install.update(cx, |job, _| job.set_plan(steps));
            this.install.update(cx, |job, cx| job.start(move |sink| plan::run(plan, sink), cx));
        }
        if this.bundle.is_none() {
            let task = cx.spawn(async move |this, cx| {
                let found = cx.background_executor().spawn(async move { crate::online::latest() }).await;
                this.update(cx, |page, cx| {
                    if let Ok(m) = &found {
                        page.sentient_version = format!("{} (downloaded)", m.version).into();
                    }
                    page.online = Some(found);
                    cx.notify();
                })
                .ok();
            });
            this._tasks.push(task);
            let native = this.engine == Engine::Native;
            let task = cx.spawn(async move |this, cx| {
                let found = cx.background_executor().spawn(async move { crate::online::versions(native) }).await;
                this.update(cx, |page, cx| {
                    page.versions = Some(found);
                    cx.notify();
                })
                .ok();
            });
            this._tasks.push(task);
        }
        // Development aid: open the wizard on a given page to inspect it.
        #[cfg(debug_assertions)]
        if let Some(step) = std::env::var("SPM_WIZARD_STEP").ok().and_then(|s| s.parse::<usize>().ok()) {
            this.step = step.min(FINISH);
            this.flavor = Some(Flavor::Timescaledb);
            this.loc_per_store = std::env::var_os("SPM_DEBUG_LOC_PG").is_some();
            this.debug_content(window, cx);
            // Screens: a System check row with its commands shown.
            if let Ok(row) = std::env::var("SPM_DEBUG_SYSCHECK_COMMANDS") {
                this.commands_shown.insert(row);
            }
            // Screens: an older PostgreSQL's data on System check.
            if std::env::var_os("SPM_DEBUG_OLD_PG").is_some() {
                this.engine = Engine::Native;
                this.run_checks(cx);
            }
            // The install step failed with a canned cause (crate::diagnosis).
            if this.step == INSTALL && std::env::var_os("SPM_DEBUG_DIAGNOSIS").is_some() {
                this.engine = Engine::Native;
                this.install.update(cx, |job, cx| {
                    job.set_plan(plan::screen_steps());
                    job.start(
                        |sink| {
                            sink(sentient_installer_core::progress::Progress::Step { name: "Installing the Platform Manager".into() });
                            sink(sentient_installer_core::progress::Progress::Step { name: "Installing PostgreSQL from your distribution".into() });
                            Err("pkexec exited with 42".into())
                        },
                        cx,
                    )
                });
            }
        }
        this
    }

    /// A fix the install's error panel leaves to this page: another port
    /// for the one in use, or ask for the password again. Both run the
    /// install again. False: not this page's.
    fn apply_fix(&mut self, action: &sentient_installer_core::diagnose::FixAction, window: &mut Window, cx: &mut Context<Self>) -> bool {
        use sentient_installer_core::diagnose::FixAction;
        match action {
            FixAction::UsePort { port, instead } => {
                let f = &self.fields;
                let field = [&f.pg_port, &f.ch_http_port, &f.ch_native_port, &f.http_port, &f.mqtt_port, &f.mqtts_port, &f.coap_port, &f.coaps_port]
                    .into_iter()
                    .find(|field| self.value(field, cx) == port.to_string())
                    .cloned();
                let Some(field) = field else { return false };
                field.update(cx, |state, cx| state.set_value(instead.to_string(), window, cx));
                self.start_install(cx);
                true
            }
            FixAction::AskAgain => {
                self.start_install(cx);
                true
            }
            _ => false,
        }
    }

    fn value(&self, field: &Entity<InputState>, cx: &App) -> String {
        field.read(cx).value().trim().to_string()
    }

    /// The ports the System check looks at: the ones chosen so far (the
    /// defaults until changed; "Use another port" changes them).
    fn check_ports(&self, cx: &App) -> Vec<(u16, String)> {
        let f = &self.fields;
        let d = DeployConfig::default();
        let get = |field: &Entity<InputState>, default: u16| self.value(field, cx).parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(default);
        vec![
            (get(&f.http_port, d.http_port), "HTTP".into()),
            (get(&f.mqtt_port, d.mqtt_port), "MQTT".into()),
            (get(&f.coap_port, d.coap_port), "CoAP".into()),
            (get(&f.pg_port, d.pg_port), "PostgreSQL".into()),
        ]
    }

    fn run_checks(&mut self, cx: &mut Context<Self>) {
        self.checks = None;
        self.look_for_old_pg(cx);
        let offline = self.bundle.is_some();
        let engine = self.engine;
        let ports = self.check_ports(cx);
        #[cfg(debug_assertions)]
        let canned = std::env::var_os("SPM_DEBUG_SYSCHECK").is_some() || std::env::var_os("SPM_DEBUG_SETUP_LONG").is_some();
        #[cfg(not(debug_assertions))]
        let canned = false;
        let pg_dir = std::path::PathBuf::from(self.value(&self.fields.data_dir, cx));
        let task = cx.spawn(async move |this, cx| {
            let look_dir = pg_dir.clone();
            let found = cx.background_executor().spawn(async move { crate::leftovers::find(&look_dir) }).await;
            let first = this
                .update(cx, |page, cx| {
                    let first = !found.is_empty() && !page.earlier_done && page.step == 0 && !page.manager_only;
                    page.leftovers = found;
                    cx.notify();
                    first
                })
                .unwrap_or(false);
            // The first look found an earlier install: its own screen first.
            if first {
                let earlier = cx.background_executor().spawn(async move { crate::earlier::Found::look(None, pg_dir) }).await;
                this.update(cx, |page, cx| {
                    if earlier.any() && !page.earlier_done {
                        let view = cx.new(|cx| crate::earlier::EarlierPage::new(earlier, cx));
                        let sub = cx.subscribe(&view, |page: &mut Self, _, _: &crate::earlier::Continue, cx| {
                            page.earlier = None;
                            page.earlier_done = true;
                            page.run_checks(cx);
                            cx.notify();
                        });
                        page._subscriptions.push(sub);
                        page.earlier = Some(view);
                        cx.notify();
                    }
                })
                .ok();
            }
        });
        self._tasks.push(task);
        let task = cx.spawn(async move |this, cx| {
            let rows = cx
                .background_executor()
                .spawn(async move {
                    let mode = if cfg!(windows) {
                        "windows-native"
                    } else if engine == Engine::Docker {
                        "linux-docker"
                    } else {
                        "linux-native"
                    };
                    let found = if canned { debug_checks() } else { checks::run_for(mode, offline, Some(&ports)) };
                    let mut rows = found.into_iter().map(CheckRow::from).collect::<Vec<_>>();
                    // Installing over a running SENTIENT would replace its
                    // configuration (and, on Docker, its passwords).
                    // (Screens: the canned rows alone, whatever runs here.)
                    for c in if canned { Vec::new() } else { crate::adopt::find() } {
                        rows.push(CheckRow {
                            id: "adopt".into(),
                            state: None,
                            fixes: Vec::new(),
                            label: "SENTIENT already installed".into(),
                            detail: format!(
                                "SENTIENT {} runs on this computer as {}, from {}. Installing again would overwrite it: open the Platform Manager and use “Add existing instance” on its Status screen instead.",
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

    /// A System check row's action. "Use another port" changes the field
    /// here; the rest run as a job (one password prompt) and the check
    /// runs again when it ends. Removing a PostgreSQL asks first.
    pub(crate) fn check_action(&mut self, row: String, label: String, action: sentient_installer_core::diagnose::FixAction, window: &mut Window, cx: &mut Context<Self>) {
        use sentient_installer_core::diagnose::FixAction;
        match &action {
            FixAction::Recheck => self.run_checks(cx),
            FixAction::UsePort { port, instead } => {
                let f = &self.fields;
                let field = [&f.pg_port, &f.http_port, &f.mqtt_port, &f.coap_port, &f.mqtts_port, &f.coaps_port, &f.ch_http_port, &f.ch_native_port]
                    .into_iter()
                    .find(|field| self.value(field, cx) == port.to_string())
                    .cloned()
                    // The PostgreSQL row's own: SENTIENT's PostgreSQL port.
                    .or_else(|| (row == "existing-pg").then(|| f.pg_port.clone()));
                if let Some(field) = field {
                    field.update(cx, |state, cx| state.set_value(instead.to_string(), window, cx));
                }
                self.port_owners.clear();
                self.run_checks(cx);
            }
            FixAction::RemovePostgres { data_dir, package, .. } => {
                let what = match package {
                    Some(p) => format!("The service is stopped and the package {p} is removed. "),
                    None => "The service is stopped and cleaned up. ".into(),
                };
                let text = format!("{what}Its data folder {data_dir} is moved to {data_dir}.old-<date>: nothing is deleted.");
                let this = cx.entity().downgrade();
                crate::modal::alert(window, cx, move |dialog, _, _| {
                    let (this, row, label, action) = (this.clone(), row.clone(), label.clone(), action.clone());
                    dialog.title("Remove this PostgreSQL?").description(text.clone()).confirm().ok_text("Remove it").on_ok(move |_, _, cx| {
                        let (row, label, action) = (row.clone(), label.clone(), action.clone());
                        this.update(cx, |page, cx| page.run_check_fix(row, label, action, cx)).ok();
                        true
                    })
                });
            }
            _ => self.run_check_fix(row, label, action, cx),
        }
        cx.notify();
    }

    fn run_check_fix(&mut self, row: String, label: String, action: sentient_installer_core::diagnose::FixAction, cx: &mut Context<Self>) {
        if self.check_fix.read(cx).is_running() {
            return;
        }
        self.check_fix_at = Some((row, label));
        self.check_fix_watch = true;
        self.check_fix.update(cx, |job, cx| {
            // jobview: short (a check's one-click fix, shown on its row).
            job.start(
                move |sink| {
                    sentient_installer_core::diagnose::apply(&action, &sink)
                        .map(|message| sink(sentient_installer_core::progress::Progress::Done { message }))
                },
                cx,
            )
        });
        cx.notify();
    }

    /// Remove what an earlier install left, then look again.
    fn remove_leftovers(&mut self, cx: &mut Context<Self>) {
        let pg_dir = std::path::PathBuf::from(self.value(&self.fields.data_dir, cx));
        // jobview: short (shown on the check's row).
        self.cleanup.update(cx, |job, cx| job.start(move |sink| crate::leftovers::remove(pg_dir, sink), cx));
        let job = self.cleanup.clone();
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let running = job.read_with(cx, |job, _| job.is_running());
                if !running {
                    break;
                }
            }
            this.update(cx, |page, cx| {
                page.existing = None;
                page.run_checks(cx);
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

    /// The text fields of the current step as shown, in their visual order
    /// (Enter moves through them).
    fn step_fields(&self) -> Vec<Entity<InputState>> {
        let f = &self.fields;
        let v: Vec<&Entity<InputState>> = match self.step {
            4 => {
                let mut v = vec![&f.pg_db, &f.pg_user, &f.pg_port, &f.pg_password, &f.pg_admin_password];
                if self.flavor == Some(Flavor::Clickhouse) {
                    v.extend([&f.ch_db, &f.ch_user, &f.ch_password, &f.ch_http_port, &f.ch_native_port]);
                }
                v
            }
            5 => vec![&f.http_port, &f.mqtt_port, &f.mqtts_port, &f.coap_port, &f.coaps_port],
            6 => vec![&f.admin_email, &f.admin_password, &f.admin_confirm],
            7 if self.autologin => {
                if cfg!(windows) {
                    vec![&f.autologin_user, &f.autologin_password]
                } else {
                    vec![&f.autologin_user]
                }
            }
            _ => Vec::new(),
        };
        v.into_iter().cloned().collect()
    }

    /// Can the wizard leave the current page? `Err` says why not.
    fn validate(&self, cx: &App) -> Result<(), String> {
        let f = &self.fields;
        match self.step {
            0 if !self.accepted => Err("Accept the licence agreement to continue.".into()),
            // SENTIENT itself comes only from a package (there is no download
            // of it yet): without one a native install would stop half-way.
            0 if !self.manager_only && self.bundle.is_none() && cfg!(windows) && !self.can_download() => Err(match &self.online {
                None => "Still looking for the latest SENTIENT online…".to_string(),
                Some(Err(e)) => format!("There is no SENTIENT package next to Setup and SENTIENT could not be found online ({e}). Connect to the internet, or unzip the SENTIENT package and run SENTIENT-Setup from that folder, or choose “Only the Platform Manager”."),
                Some(Ok(_)) => String::new(),
            }),
            1 => match &self.checks {
                None => Err("The system check is still running.".into()),
                Some(rows) if rows.iter().any(|r| r.status == Status::Fail) => {
                    Err("Resolve the items marked “Blocker” first, then run the check again.".into())
                }
                Some(_) => Ok(()),
            },
            2 if self.engine == Engine::Docker => self.locations(cx).map(|_| ()),
            2 if self.bundle.is_none() && !self.can_download() => Err(
                "A native install needs SENTIENT's program files: connect to the internet (they are downloaded), or unzip the SENTIENT package and run SENTIENT-Setup from that folder, or choose Docker, which pulls the image.".into(),
            ),
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
                let mut ports = self.database_ports(cx)?;
                // A kept cluster may still be running on its port (an
                // earlier Platform Manager left it so): it is this one.
                if self.existing.is_some() && self.engine == Engine::Native {
                    ports.retain(|(_, what)| what != "PostgreSQL");
                    // So may its ClickHouse (an earlier Platform Manager left
                    // it running); this install takes it over.
                    #[cfg(windows)]
                    if sentient_installer_core::clickhouse_wsl::is_installed() {
                        ports.retain(|(_, what)| !what.starts_with("ClickHouse"));
                    }
                }
                self.ports_in_use(&ports)
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
            SHORTCUTS
                if !self.manager_only
                    && cfg!(windows)
                    && (self.autostart || self.open_at_signin)
                    && parse_start_delay(&self.value(&f.start_delay, cx)).is_err() =>
            {
                parse_start_delay(&self.value(&f.start_delay, cx)).map(|_| ())
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

    /// The current page's busy ports: (port, what, its field).
    pub(crate) fn busy_fields(&self, cx: &App) -> Vec<(u16, String, Entity<InputState>)> {
        let f = &self.fields;
        let fields: Vec<(&Entity<InputState>, &str)> = match self.step {
            4 => {
                let mut v = vec![(&f.pg_port, "PostgreSQL")];
                if self.flavor.is_some_and(|fl| fl.uses_clickhouse()) {
                    v.push((&f.ch_http_port, "ClickHouse HTTP"));
                    v.push((&f.ch_native_port, "ClickHouse native"));
                }
                v
            }
            5 => vec![
                (&f.http_port, "HTTP"),
                (&f.mqtt_port, "MQTT"),
                (&f.mqtts_port, "MQTTS"),
                (&f.coap_port, "CoAP"),
                (&f.coaps_port, "CoAPS"),
            ],
            _ => return Vec::new(),
        };
        // The same ports the page refuses (a kept cluster's own port is not).
        let refused = self.port_problem(cx).unwrap_or_default();
        fields
            .into_iter()
            .filter_map(|(field, what)| {
                let port = self.value(field, cx).parse::<u16>().ok()?;
                refused.contains(&format!("{port} ({what})")).then(|| (port, what.to_string(), field.clone()))
            })
            .collect()
    }

    /// Look up who holds each busy port on this page, once.
    pub(crate) fn lookup_port_owners(&mut self, cx: &mut Context<Self>) {
        for (port, _, _) in self.busy_fields(cx) {
            if self.port_owners.contains_key(&port) {
                continue;
            }
            self.port_owners.insert(port, None);
            let task = cx.spawn(async move |this, cx| {
                let owner = cx.background_executor().spawn(async move { checks::port_owner(port) }).await;
                this.update(cx, |page, cx| {
                    page.port_owners.insert(port, owner);
                    cx.notify();
                })
                .ok();
            });
            self._tasks.push(task);
        }
    }

    /// The folders an earlier SENTIENT install's programs run from.
    pub(crate) fn sentient_dirs(&self, cx: &App) -> Vec<PathBuf> {
        vec![PathBuf::from(self.value(&self.fields.install_dir, cx)), PathBuf::from(self.value(&self.fields.data_dir, cx))]
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

    /// Docker: the folders the data volumes are bound to (none: Docker's
    /// own folder).
    fn locations(&self, cx: &App) -> Result<sentient_installer_core::distro::Locations, String> {
        let f = &self.fields;
        let full = |s: String, what: &str| -> Result<Option<String>, String> {
            let t = s.trim().trim_end_matches('/').to_string();
            if t.is_empty() {
                Ok(None)
            } else if !t.starts_with('/') || t.contains(['\'', '"', ',']) {
                Err(format!("{what}: give a full folder path such as /mnt/disk-a/sentient, without quotes or commas."))
            } else {
                Ok(Some(t))
            }
        };
        if self.loc_per_store {
            Ok(sentient_installer_core::distro::Locations {
                pg: full(self.value(&f.loc_pg, cx), "PostgreSQL")?,
                ch: full(self.value(&f.loc_ch, cx), "ClickHouse")?,
                data: full(self.value(&f.loc_data, cx), "SENTIENT files")?,
            })
        } else {
            Ok(match full(self.value(&f.loc_one, cx), "Data folder")? {
                Some(dir) => sentient_installer_core::distro::Locations::under(&dir, true),
                None => Default::default(),
            })
        }
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
            // PostgreSQL and ClickHouse are published on 127.0.0.1 only:
            // nothing outside this computer reaches them (SENTIENT reaches
            // them over Docker's own network).
            db_loopback: true,
            ..DeployConfig::default()
        };
        // Docker pulls the picked version's image instead of :latest.
        if self.engine == Engine::Docker && self.bundle.is_none() {
            if let Some(v) = &self.chosen_version {
                stack.image = format!("quakestring/sentient:{v}");
            }
        }
        stack.tuning.instance_id = format!("sentient-{}", native::generate_secret().get(..8).unwrap_or("01"));
        if self.engine == Engine::Docker {
            stack.locations = self.locations(cx)?;
            if !stack.flavor.uses_clickhouse() {
                stack.locations.ch = None;
            }
        }
        Ok(plan::InstallPlan {
            engine: self.engine,
            sentient_version: match (&self.bundle, &self.online) {
                (None, Some(Ok(m))) => m.version.clone(),
                _ => self.sentient_version.to_string(),
            },
            install_dir: PathBuf::from(self.value(&f.install_dir, cx)),
            data_dir: PathBuf::from(self.value(&f.data_dir, cx)),
            stack,
            pg_superuser_password: self.value(&f.pg_admin_password, cx),
            existing: self.existing.as_ref().map(|_| self.keep_existing),
            delete_existing: self.existing.is_some() && !self.keep_existing && self.delete_existing,
            old_pg: self.old_pg_choice,
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
                start_delay: parse_start_delay(&self.value(&f.start_delay, cx))?,
            },
            auto_signin_password: self.fields.autologin_password.read(cx).value().to_string(),
            bundle: self.bundle.clone(),
            download: if self.bundle.is_none() {
                self.online.as_ref().and_then(|o| o.as_ref().ok()).map(|m| m.version.clone())
            } else {
                None
            },
        })
    }

    /// Pick `version` from the list: its manifest replaces the latest one, so
    /// the download, its size and the summary follow the choice.
    fn choose_version(&mut self, version: String, cx: &mut Context<Self>) {
        if self.chosen_version.as_deref() == Some(version.as_str()) {
            return;
        }
        self.chosen_version = Some(version.clone());
        self.online = None;
        self.sentient_version = format!("{version} (downloaded)").into();
        cx.notify();
        let native = self.engine == Engine::Native;
        let task = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let m = sentient_installer_core::manifest::fetch(&sentient_installer_core::manifest::release_url(&version))?;
                    // A Docker install pulls the image; only a native one needs this computer's files.
                    if native {
                        m.wanted()?;
                    }
                    Ok(m)
                })
                .await;
            this.update(cx, |page, cx| {
                page.online = Some(found);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// No package, but the latest release was found online.
    fn can_download(&self) -> bool {
        matches!(&self.online, Some(Ok(_)))
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
                start_delay: 0,
            };
            self.problem = None;
            self.step = INSTALL;
            self.install.update(cx, |job, _| job.set_plan(plan::manager_only_steps()));
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
        // Its steps from the first frame (the shared job view).
        let steps = plan::steps(&plan);
        self.install.update(cx, |job, _| job.set_plan(steps));
        self.install.update(cx, |job, cx| job.start(move |sink| plan::run(plan, sink), cx));
        cx.notify();
    }

    /// An uninstall that kept the database keeps its passwords in the secret
    /// store too: fill them in, since the kept PostgreSQL (and ClickHouse)
    /// still use them.
    fn prefill_kept_passwords(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::secrets::{self, Secret};
        let f = &self.fields;
        let mut found = false;
        for (field, secret) in [
            (f.pg_admin_password.clone(), Secret::PostgresAdmin),
            (f.pg_password.clone(), Secret::Database),
            (f.ch_password.clone(), Secret::Clickhouse),
        ] {
            if let Some(value) = secrets::get(secret).filter(|v| !v.is_empty()) {
                field.update(cx, |state, cx| state.set_value(value, window, cx));
                found |= matches!(secret, Secret::PostgresAdmin);
            }
        }
        self.kept_passwords = found;
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
                    if self.step == 4 {
                        self.existing = self.find_existing(cx);
                        if self.existing.is_some() && self.engine == Engine::Native {
                            self.prefill_kept_passwords(window, cx);
                        }
                    }
                }
                Err(e) => self.problem = Some(e.into()),
            },
        }
        cx.notify();
    }

    /// A database an earlier install left, for the Keep / Start fresh choice.
    fn find_existing(&self, cx: &App) -> Option<String> {
        match self.engine {
            Engine::Native => native::existing_cluster(std::path::Path::new(&self.value(&self.fields.data_dir, cx)))
                .map(|d| format!("PostgreSQL data from an earlier install is in {}.", d.display())),
            Engine::Docker => sentient_installer_core::distro::existing_volume(&DeployConfig::default())
                .map(|v| format!("The Docker volume {v} holds the database of an earlier install.")),
        }
    }

    fn go(&mut self, step: usize, cx: &mut Context<Self>) {
        // Finished pages can be revisited until the install starts.
        if step < self.step && self.step < INSTALL {
            self.step = step;
            self.problem = None;
            cx.notify();
        }
    }

    fn confirm_remove_leftovers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title("Remove the earlier install?")
                .description("Its services, PostgreSQL, ClickHouse and every database in them are deleted from this computer. This can't be undone.")
                .confirm()
                .ok_text("Remove everything")
                .on_ok(move |_, _, cx| {
                    this.update(cx, |page, cx| page.remove_leftovers(cx)).ok();
                    true
                })
        });
    }

    fn confirm_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let installing = self.step == INSTALL && self.install.read(cx).is_running();
        crate::modal::alert(window, cx, move |dialog, _, _| {
            dialog
                .title("Cancel setup?")
                .description(if installing {
                    "The install stops after the current task. Anything already installed stays on this computer; run setup again to finish or remove it."
                } else {
                    "Nothing has been installed."
                })
                .confirm()
                .ok_text("Cancel setup")
                .on_ok(|_, window, _| {
                    window.remove_window();
                    true
                })
        });
    }

    /// Screens and layout tests: the step's long content
    /// (SPM_DEBUG_SETUP_LONG=1): every optional block a step can show, and
    /// on Install a running or a failed install (short or long).
    #[cfg(debug_assertions)]
    fn debug_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.earlier_done = true;
        let long = std::env::var_os("SPM_DEBUG_SETUP_LONG").is_some();
        if long {
            self.flavor = Some(Flavor::Clickhouse);
            self.customize = self.step == 7;
            self.open_at_signin = true;
            self.autologin = true;
            self.kiosk = true;
            self.leftovers = vec![
                "The SENTIENT service (sentient.service), stopped".into(),
                "PostgreSQL 18 in /var/lib/postgresql/18/main, 4.2 GB".into(),
                "ClickHouse in /var/lib/clickhouse, 18.7 GB".into(),
            ];
            if self.step == 4 {
                self.existing = Some("The Docker volume st-pg-data holds the database of an earlier install.".into());
            }
            let f = &self.fields;
            for (field, value) in [(f.admin_email.clone(), "operations.manager@an-example-plant.example.com")] {
                field.update(cx, |s, cx| s.set_value(value, window, cx));
            }
        }
        if self.step == INSTALL {
            self.install.update(cx, |job, cx| {
                job.set_plan(plan::screen_steps());
                job.start(
                    move |sink| {
                        sink(sentient_installer_core::progress::Progress::Step { name: "Installing the Platform Manager".into() });
                        sink(sentient_installer_core::progress::Progress::Step { name: "Starting PostgreSQL".into() });
                        sink(sentient_installer_core::progress::Progress::Percent { value: 0.42 });
                        for i in 0..if long { 24 } else { 4 } {
                            sink(sentient_installer_core::progress::Progress::Log {
                                line: format!("[{:02}] waiting for the database to accept connections on 127.0.0.1:5432 (attempt {i} of 60, the container reports starting)", i),
                            });
                        }
                        if long {
                            return Err("PostgreSQL did not start within two minutes: the container sentient-postgres exited with code 1 (see its log under Logs).".into());
                        }
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(60));
                        }
                    },
                    cx,
                )
            });
        }
    }

    #[cfg(not(debug_assertions))]
    fn debug_content(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// The steps down the left: the Platform Manager's menu (crate::manager::shell),
    /// with the product on top, 40 px rows on a 44 px pitch, a number badge
    /// in the icon slot (a check once done) and the selection joined to the page.
    fn steps_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::manager::shell;
        let theme = cx.theme().clone();
        let pad = shell::MENU_PAD;
        let active: &'static str = STEPS[self.step];
        let rows = self.shown_steps().into_iter().enumerate().map(|(n, i)| {
            let title = self.title(i);
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
                        m.bg(theme.primary).text_color(theme.primary_foreground).child(format!("{}", n + 1))
                    } else if done {
                        m.bg(theme.primary.opacity(0.14)).text_color(theme.primary).child(Icon::new(IconName::Check).size(px(12.)))
                    } else {
                        m.border_1().border_color(theme.muted_foreground.opacity(0.6)).text_color(theme.muted_foreground).child(format!("{}", n + 1))
                    }
                });
            let id: &'static str = STEPS[i];
            h_flex()
                .id(("step", i))
                .relative()
                .h(px(shell::ROW_H))
                .flex_shrink_0()
                .gap(px(12.))
                .pl(px(shell::row_pl(pad)))
                .pr(px(12.))
                .mr(px(10.))
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
                .child(shell::row_probe(id))
                .child(ui::layout_probe("menu-row"))
                .child(mark)
                .child(
                    div()
                        .relative()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(title)
                        .child(ui::layout_probe(if current { "menu-label-on" } else { "menu-label" })),
                )
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
            .child(div().relative().size(px(shell::LOGO)).flex_shrink_0().flex().items_center().justify_center().child(ui::product_logo(ui::Product::Sentient, shell::LOGO)).child(ui::layout_probe("menu-icon")))
            .child(shell::brand_name(&theme, "SETUP"));
        v_flex()
            .w(px(shell::MENU_W))
            .h_full()
            .flex_shrink_0()
            .relative()
            .overflow_hidden()
            .bg(shell::menu_bg(&theme, false, 0.))
            // No border: the edge and the selection are one shape, under the rows.
            .child(shell::menu_probe(pad, None))
            .child(shell::selection(active, theme.muted, shell::menu_line(&theme), !ui::reduce_motion()))
            .child(brand)
            .child(
                v_flex()
                    .id("steps-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(px(shell::ROW_GAP))
                    .pt(px(shell::NAV_TOP))
                    .pb(px(12.))
                    .pl(px(pad))
                    .children(rows),
            )
    }

    /// The buttons under the page: the step count and why Next is off on
    /// the left; Cancel, Back and Next as the dialogs' buttons, 40 px tall.
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let failed = matches!(self.install.read(cx).state(), JobState::Failed(_));
        let again = match self.install.read(cx).state() {
            JobState::Failed(m) => crate::ui::retry_label(m, "Retry"),
            _ => "Retry",
        };
        // While the page can't be left, Next is off and says why.
        let blocked = if self.step < SUMMARY { self.validate(cx).err() } else { None };
        let next_label = match self.step {
            SUMMARY => "Install",
            INSTALL if failed => again,
            INSTALL => "Installing…",
            FINISH => "Close",
            _ => "Next",
        };
        let h = px(crate::theme::BAR_CTRL_H);
        h_flex()
            .flex_shrink_0()
            .px(px(ui::PAGE_PAD))
            .py(px(14.))
            .gap(crate::ui::button_gap())
            .items_center()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(div().flex_shrink_0().text_size(px(12.)).text_color(theme.muted_foreground).whitespace_nowrap().child({
                let shown = self.shown_steps();
                let at = shown.iter().position(|s| *s == self.step).unwrap_or(0);
                format!("Step {} of {}", at + 1, shown.len())
            }))
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
            .when(self.step < FINISH, |bar| {
                bar.child(crate::ui::probed(
                    crate::ui::button("cancel")
                        .outline()
                        .h(h)
                        .px_4()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, window, cx| this.confirm_cancel(window, cx))),
                ))
            })
            .when(self.step > 0 && self.step < INSTALL, |bar| {
                bar.child(crate::ui::probed(
                    crate::ui::button("back")
                        .outline()
                        .h(h)
                        .px_4()
                        .label("Back")
                        .on_click(cx.listener(|this, _, _, cx| this.go(this.previous_shown(this.step), cx))),
                ))
            })
            .child(crate::ui::probed(
                crate::ui::button("next")
                    .primary()
                    .h(h)
                    .px_5()
                    .label(next_label)
                    .disabled((self.step == INSTALL && !failed) || blocked.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
            ))
    }
}

impl Render for Installer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Windows 10: the app's caption and rounded corners (crate::win_frame).
        let root = self.render_root(window, cx).into_any_element();
        crate::win_frame::frame(crate::mode::AppMode::Install.window_title(), root, window, cx)
    }
}

impl Installer {
    fn render_root(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(earlier) = &self.earlier {
            return div().size_full().bg(cx.theme().background).child(earlier.clone()).into_any_element();
        }
        // Setup installs on this computer, which the Flatpak build cannot.
        if sentient_installer_core::host::in_flatpak() {
            let theme = cx.theme().clone();
            return v_flex().size_full().p(px(24.)).bg(theme.background).text_color(theme.foreground).child(ui::flatpak_local_notice(&theme)).into_any_element();
        }
        let width = f32::from(window.viewport_size().width);
        // Two cards side by side when the page has the room (ui::card_pair).
        ui::set_side_by_side(width - crate::manager::shell::MENU_W >= 720.);
        ui::set_narrow(width - crate::manager::shell::MENU_W < 560.);
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_LAYOUT").is_some() {
            eprintln!("LAYOUT-FRAME {:.0} {:.0}", width, f32::from(window.viewport_size().height));
            // The layout test: no database password starts empty (Setup
            // fills in its default, "sentient").
            let f = &self.fields;
            for (name, field) in [("pg-password", &f.pg_password), ("pg-admin-password", &f.pg_admin_password), ("ch-password", &f.ch_password)] {
                eprintln!("LAYOUT password {name} {}", if field.read(cx).value().is_empty() { "empty" } else { "set" });
            }
        }
        let nav = self.steps_nav(cx);
        let footer = self.footer(cx);
        let page = self.page(window, cx);
        let subtitle = self.subtitle();
        let theme = cx.theme().clone();
        // The shared dialogs, centred right of the steps (crate::modal).
        let layer = crate::modal::layer(window, cx, crate::manager::shell::MENU_W, 0.);
        // Welcome fits the window (its licence scrolls inside); every other
        // step scrolls as a whole.
        let fits = self.step == 0;
        h_flex()
            .size_full()
            .relative()
            // Clear under the translucent menu (the page and the footer paint their own).
            .bg(if crate::manager::shell::see_through() { theme.background.opacity(0.) } else { theme.background })
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
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .map(|p| if fits { p.overflow_hidden() } else { p.overflow_y_scroll() })
                            .p(px(ui::PAGE_PAD))
                            .gap(px(ui::GAP))
                            .bg(theme.muted)
                            .child(ui::layout_probe("page-area"))
                            // The title and its line, as on every Platform Manager page (ui::page).
                            .child(
                                v_flex()
                                    .flex_shrink_0()
                                    .gap_0p5()
                                    .min_h(px(32.))
                                    .justify_center()
                                    .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child(self.title(self.step)))
                                    .when_some(subtitle, |c, s| c.child(div().text_sm().text_color(theme.muted_foreground).child(s))),
                            )
                            .child(page),
                    )
                    .child(footer),
            )
            .children(layer)
            .into_any_element()
    }
}

/// Screens and the layout test (SPM_DEBUG_SYSCHECK=1, or the long
/// variant): the System check of the native Linux case it was made for, a
/// PostgreSQL whose package was removed still running on 5432, and a
/// Docker container on 8080, built by the same code as the real rows.
#[cfg(debug_assertions)]
fn debug_checks() -> Vec<checks::Check> {
    use sentient_installer_core::units::{PgService, UnitInfo};
    let pg = UnitInfo { id: "postgresql.service".into(), load_state: "not-found".into(), active_state: "active".into(), fragment_path: String::new(), main_pid: 812 };
    let mut postmaster = checks::owner_of("postgres".into(), Some(812), None, Some(pg.id.clone()), Some(&pg));
    postmaster.data_dir = Some("/var/lib/postgres/data".into());
    let docker = checks::PortOwner { process: "docker-proxy".into(), container: Some("grafana".into()), ..Default::default() };
    let wanted = [(8080, "HTTP".to_string()), (1883, "MQTT".to_string()), (5683, "CoAP".to_string()), (5432, "PostgreSQL".to_string())];
    let mut ports = checks::ports_check(&wanted, &[(5432, "PostgreSQL".into(), Some(postmaster), Some(5433)), (8080, "HTTP".into(), Some(docker), Some(8081))]);
    ports.status = Status::Warn;
    ports.label = "Ports".into();
    let row = |id: &str, label: &str, status: Status, detail: &str| checks::Check { id: id.into(), label: label.into(), status, detail: detail.into(), state: None, fixes: Vec::new() };
    let mut docker = row("docker-daemon", "Docker daemon", Status::Setup, "Docker is installed but not running. The installer will enable and start it.");
    docker.state = Some("Will be started".into());
    docker.fixes = vec![sentient_installer_core::diagnose::Fix::Action { label: "Start Docker".into(), action: sentient_installer_core::diagnose::FixAction::StartDocker }];
    let mut v = vec![row("systemd", "systemd", Status::Pass, "systemd 259 (259.5-1-arch)"), row("disk", "Disk space", Status::Pass, "412 GB free."), docker];
    v.push(checks::existing_pg_check(&PgService::LeftoverRunning(pg), None, "/var/lib/postgres/data", Some(5433)));
    v.push(ports);
    checks::share_fixes(&mut v);
    v
}

#[cfg(not(debug_assertions))]
fn debug_checks() -> Vec<checks::Check> {
    Vec::new()
}

#[cfg(test)]
mod start_delay_tests {
    use super::parse_start_delay;

    #[test]
    fn start_delay_is_whole_seconds_up_to_an_hour() {
        assert_eq!(parse_start_delay("10"), Ok(10));
        assert_eq!(parse_start_delay(" 0 "), Ok(0));
        assert_eq!(parse_start_delay("3600"), Ok(3600));
        assert!(parse_start_delay("3601").is_err());
        assert!(parse_start_delay("-1").is_err());
        assert!(parse_start_delay("1.5").is_err());
        assert!(parse_start_delay("").is_err());
    }
}
