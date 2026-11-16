// Copyright © 2016-2026 The SENTIENT Authors

//! Manager mode: the app the operator opens from the Start menu after
//! installation. A left navigation of the management screens; each screen is
//! its own view, created the first time it is opened.

mod about;
pub(crate) mod backup;
pub(crate) mod startup;
pub(crate) mod files;
pub(crate) mod folder_chooser;
mod gateway;
mod instances;
pub(crate) mod logs;
pub(crate) mod migration;
pub(crate) mod movedata;
pub(crate) mod restore;
pub(crate) mod remote_pages;
mod repair;
mod takeover;
pub(crate) mod jobview;
mod licence;
mod licence_card;
pub(crate) mod settings;
pub(crate) mod session;
pub(crate) mod setup_guide;
pub(crate) mod shell;
pub(crate) mod setting_field;
pub(crate) mod status;
pub(crate) mod stats;
pub(crate) mod strip;
pub(crate) mod update;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, h_flex, v_flex,
    scroll::ScrollableElement as _,
    button::ButtonVariants as _,
};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::theme;
use crate::ui;
use crate::sysmon::Monitor;
use crate::updates::UpdateState;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Page {
    Status,
    Logs,
    /// Files on this computer and the open instance (SFTP).
    Files,
    Update,
    Gateway,
    Backup,
    Migration,
    Settings,
    Repair,
    /// This computer and remote instances (below the divider).
    Instances,
    /// About and preferences of the Platform Manager (the (i) at the
    /// bottom of the menu, not a menu entry).
    About,
    /// Statistics: in the menu after Status, and opened from the top bar's monitors.
    Statistics,
    /// SENTIENT's licence, directly above Platform settings.
    Licence,
}

impl Page {
    /// The menu, in its order: Platform settings last.
    const ALL: [Page; 11] = [
        Page::Status,
        Page::Statistics,
        Page::Logs,
        Page::Files,
        Page::Update,
        Page::Gateway,
        Page::Backup,
        Page::Migration,
        Page::Repair,
        Page::Licence,
        Page::Settings,
    ];

    fn id(self) -> &'static str {
        match self {
            Page::Status => "status",
            Page::Logs => "logs",
            Page::Files => "files",
            Page::Update => "update",
            Page::Gateway => "gateway",
            Page::Backup => "backup",
            Page::Migration => "migration",
            Page::Settings => "settings",
            Page::Repair => "repair",
            Page::Instances => "instances",
            Page::About => "about",
            Page::Statistics => "statistics",
            Page::Licence => "licence",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Status => "Status",
            Page::Logs => "Logs",
            Page::Files => "Files",
            Page::Update => "Update",
            Page::Gateway => "Gateway",
            Page::Backup => "Backup and restore",
            Page::Migration => "Migration",
            Page::Settings => "Platform settings",
            Page::Repair => "Repair and uninstall",
            Page::Instances => "Instances",
            Page::About => "About and preferences",
            Page::Statistics => "Statistics",
            Page::Licence => "Licence",
        }
    }

    /// The menu's icon: the heavier set (assets/icons/menu).
    fn icon(self) -> Icon {
        shell::menu_icon(match self {
            Page::Status => "activity",
            Page::Logs => "square-terminal",
            Page::Files => "folder",
            Page::Update => "refresh-cw",
            Page::Gateway => "network",
            Page::Backup => "archive",
            Page::Migration => "arrow-left-right",
            Page::Settings => "settings",
            Page::Repair => "wrench",
            Page::Instances => "server",
            Page::About => "info",
            Page::Statistics => "chart-line",
            Page::Licence => "key",
        })
    }
}

pub struct Manager {
    page: Page,
    record: Option<InstallationRecord>,
    status: Option<Entity<status::StatusPage>>,
    logs: Option<Entity<logs::LogsPage>>,
    update: Option<Entity<update::UpdatePage>>,
    gateway: Option<Entity<gateway::GatewayPage>>,
    backup: Option<Entity<backup::BackupPage>>,
    migration: Option<Entity<migration::MigrationPage>>,
    settings: Option<Entity<settings::SettingsPage>>,
    repair: Option<Entity<repair::RepairPage>>,
    about: Option<Entity<about::AboutPage>>,
    instances: Option<Entity<instances::InstancesPage>>,
    /// The remote instances (crate::remote), for the switcher.
    remotes: Vec<crate::remote::Instance>,
    /// The remote instance that is open (None: this computer).
    open_remote: Option<String>,
    /// That instance's processor, memory and disk (None until read; the
    /// last reading while a new one fails, see `remote_monitor_stale`).
    remote_monitor: Option<Monitor>,
    /// The last reading failed: `remote_monitor` is the one before.
    remote_monitor_stale: bool,
    /// Each remote instance's last monitor reading, shown at once when it is
    /// opened again (memory only, this run).
    monitor_cache: HashMap<String, Monitor>,
    /// Counts the instance switches: a monitor loop of an earlier opening
    /// stops (and its reading is dropped) when this has moved on.
    open_gen: u64,
    /// The pages opened since the last switch: one kept from before is
    /// read again (in the background) the first time it shows.
    fresh: HashSet<Page>,
    /// The pages of every remote instance opened in this run, by id: shown
    /// again with what they read, and read again behind it.
    remote_pages: HashMap<String, Entity<remote_pages::RemotePages>>,
    /// Statistics, by instance (None: this computer), kept for the run.
    stats: HashMap<Option<String>, Entity<stats::StatsPage>>,
    /// The Licence page, by instance (None: this computer).
    licence: HashMap<Option<String>, Entity<licence::LicencePage>>,
    /// Files, by the instance open when it was made (None: this computer).
    files: HashMap<Option<String>, Entity<files::FilesPage>>,
    /// Takes the keyboard while a dialog is open (Escape closes it).
    modal_focus: FocusHandle,
    /// The instance switcher's menu is open.
    switcher_open: bool,
    /// The notifications panel is open.
    bell_open: bool,
    /// The folded menu's logo is under the pointer (it shows the unfold
    /// button then).
    logo_hover: bool,
    /// Narrow window: the menu is open over the page (the logo's unfold
    /// button there; the page does not move).
    menu_overlay: bool,
    /// How far that overlay is open (0..1, the scrim's strength), set by
    /// the menu each frame.
    overlay_t: std::cell::Cell<f32>,
    /// The bell's conditions: when each was first seen (its time), and the
    /// ones cleared with their × (until they change or go and come back).
    cond_since: std::cell::RefCell<std::collections::HashMap<String, i64>>,
    cond_cleared: std::cell::RefCell<std::collections::HashSet<String>>,
    /// The window's height (the bell panel's maximum).
    window_h: std::cell::Cell<f32>,
    /// The global progress strip (strip.rs), under the top bar.
    strip: Entity<strip::Strip>,
    /// Screens: the canned job the strip follows.
    _debug_job: Option<Entity<crate::jobs::Job>>,
    /// The theme popover in the menu's bottom group is open.
    theme_open: bool,
    /// The open instance's processor, memory and disk (top bar).
    monitor: Option<Monitor>,
    updates: Entity<UpdateState>,
    /// This run follows a self-update from that version (`--updated-from`):
    /// say so once.
    updated_from: Option<String>,
    /// Setup installed this Platform Manager beside a SENTIENT it found:
    /// say so once.
    found_notice: bool,
    /// A tray or taskbar action that runs on its page once the page exists
    /// (crate::background): `backup`, `control:<verb>`.
    pending: Option<String>,
    /// The "Move data to another disk" flow last seen opened
    /// (movedata::generation): a new one shows Platform settings.
    move_gen: usize,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

/// Switch timing (SPM_DEBUG_SWITCH=1): when the last switch started, for
/// the frames that follow it.
thread_local!(static SWITCH_AT: std::cell::Cell<Option<(std::time::Instant, u32)>> = const { std::cell::Cell::new(None) });

/// Logs how long open_instance itself took (work on the UI thread).
struct SwitchTiming(std::time::Instant, Option<String>);
impl Drop for SwitchTiming {
    fn drop(&mut self) {
        if std::env::var_os("SPM_DEBUG_SWITCH").is_some() {
            eprintln!("SWITCH start to={} open_instance_ms={:.2}", self.1.as_deref().unwrap_or("local"), self.0.elapsed().as_secs_f64() * 1000.);
            SWITCH_AT.with(|s| s.set(Some((self.0, 0))));
        }
    }
}

/// The bell's events as saved (the tray's menu reads them without a window).
pub fn saved_events() -> Vec<strip::Event> {
    strip::load_events()
}

impl Manager {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // OpenGL (no Vulkan driver): the window cannot be translucent, so
        // the menu is solid and About says why.
        shell::detect_gl(_window);
        // Why the menu is solid or not (SPM_DEBUG_BLUR=1).
        if std::env::var_os("SPM_DEBUG_BLUR").is_some() {
            eprintln!(
                "BLUR supported={} on={} wayland={:?} desktop={:?} gpu={:?}",
                shell::see_through_supported(),
                shell::see_through(),
                std::env::var_os("WAYLAND_DISPLAY"),
                std::env::var("XDG_CURRENT_DESKTOP").ok(),
                _window.gpu_specs()
            );
        }
        // `--page migration` (from Setup's Change database).
        let args: Vec<String> = std::env::args().collect();
        // Without --page: the page it was left on.
        let saved = session::load();
        // Control is part of Status now: a session left there opens Status.
        let page_of = |id: &str| if id == "control" { Some(Page::Status) } else { Page::ALL.into_iter().chain([Page::Instances, Page::About]).find(|p| p.id() == id) };
        let page_arg = args.windows(2).find(|w| w[0] == "--page").and_then(|w| page_of(&w[1]));
        let page = page_arg.or_else(|| saved.page.as_deref().and_then(page_of)).unwrap_or(Page::Status);
        let record = InstallationRecord::load().inspect(InstallationRecord::use_pg_tools);
        let installed = record.as_ref().map(|r| r.sentient_version.clone());
        let native = record.as_ref().is_some_and(|r| r.platform().needs_package());
        let updates = cx.new(|cx| UpdateState::new(installed, native, cx));
        // This computer's statistics sampler: a user timer, no password
        // (an installed build only; a development build would point it at
        // itself).
        #[cfg(all(not(windows), not(debug_assertions)))]
        cx.background_executor()
            .spawn(async {
                if !crate::stats::sampler_installed() {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = crate::stats::install_sampler(&exe);
                    }
                }
            })
            .detach();
        // The progress line under the top bar follows the strip.
        let strip = strip::install(cx);
        let subscriptions = vec![
            cx.observe(&updates, |_, _, cx| cx.notify()),
            cx.observe(&strip, |_, _, cx| cx.notify()),
            cx.subscribe(&strip, |this, _, e: &strip::JobEnded, cx| this.after_job(e.instance.clone(), cx)),
        ];
        let updated_from = args.windows(2).find(|w| w[0] == "--updated-from").map(|w| w[1].clone());
        let found_notice = record.is_some() && crate::updates::take_found_installation();
        let mut manager = Self {
            page,
            record,
            status: None,
            logs: None,
            update: None,
            gateway: None,
            backup: None,
            migration: None,
            settings: None,
            repair: None,
            about: None,
            instances: None,
            remotes: crate::remote::load(),
            open_remote: None,
            remote_monitor: None,
            remote_monitor_stale: false,
            monitor_cache: HashMap::new(),
            open_gen: 0,
            fresh: HashSet::new(),
            remote_pages: HashMap::new(),
            stats: HashMap::new(),
            licence: HashMap::new(),
            files: HashMap::new(),
            modal_focus: cx.focus_handle(),
            strip: strip.clone(),
            switcher_open: false,
            bell_open: false,
            logo_hover: false,
            menu_overlay: false,
            overlay_t: std::cell::Cell::new(0.),
            cond_since: Default::default(),
            cond_cleared: Default::default(),
            window_h: std::cell::Cell::new(720.),
            _debug_job: None,
            theme_open: cfg!(debug_assertions) && std::env::var_os("SPM_THEME_OPEN").is_some(),
            monitor: None,
            updates,
            updated_from,
            pending: None,
            move_gen: 0,
            found_notice,
            _subscriptions: subscriptions,
            _tasks: Vec::new(),
        };
        manager.start_monitor(cx);
        // The instance it was left on, if it is still in the list (it shows
        // connecting; start never waits for it). Not when a page was asked
        // for on the command line.
        if page_arg.is_none() {
            if let Some(id) = saved.instance.filter(|id| manager.remotes.iter().any(|r| &r.id == id)) {
                manager.open_instance(Some(id), cx);
                manager.page = page;
            }
        }
        // Screenshots: `--instance <id>` opens a remote instance on `--page`.
        #[cfg(debug_assertions)]
        if let Some(id) = args.windows(2).find(|w| w[0] == "--instance").map(|w| w[1].clone()) {
            manager.open_instance(Some(id), cx);
            manager.page = page;
        }
        // Screens: "Move data to another disk" at a frame of its mockup.
        #[cfg(debug_assertions)]
        if std::env::var("SPM_DEBUG_MOVEDATA_FRAME").is_ok_and(|f| !matches!(f.as_str(), "M1" | "M2" | "M3" | "M13" | "M14")) {
            let target = match manager.open_remote.as_ref().and_then(|id| manager.remotes.iter().find(|r| &r.id == id)).cloned() {
                Some(inst) => crate::movedata::Target::Remote(inst),
                None => crate::movedata::Target::Local,
            };
            let on = match &target {
                crate::movedata::Target::Remote(i) => i.name.clone(),
                crate::movedata::Target::Local => "This computer".to_string(),
            };
            movedata::open(target, on, None, _window, cx);
        }
        // Screens: the bell with canned events, the bell or switcher open.
        #[cfg(debug_assertions)]
        {
            if let Some(n) = std::env::var("SPM_DEBUG_EVENTS").ok().and_then(|n| n.parse().ok()) {
                manager.strip.update(cx, |s, _| s.debug_events(n));
            }
            manager.bell_open = std::env::var_os("SPM_DEBUG_BELL").is_some();
            manager.switcher_open = std::env::var_os("SPM_DEBUG_SWITCHER").is_some();
            if std::env::var_os("SPM_DEBUG_ERROR_DIALOG").is_some() {
                crate::ui::show_error_with(
                    "The server agent could not be updated",
                    crate::remote::failed("The agent could not be put in place at /opt/sentient-platform-manager (as administrator). The agent there is unchanged.", "sudo: a password is required"),
                    Some(std::rc::Rc::new(|_: &mut App| {})),
                    cx,
                );
            }
            // Screens: every switch state on the page's colours.
            if std::env::var_os("SPM_DEBUG_SWITCH_SHEET").is_some() {
                struct Sheet;
                impl Render for Sheet {
                    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                        use gpui_kit::component::{switch::Switch, Disableable as _};
                        let row = |label: &'static str, sw: Switch| h_flex().gap_3().items_center().child(div().w(px(200.)).text_sm().child(label)).child(sw);
                        v_flex()
                            .gap_3()
                            .child(row("Off", Switch::new("s1").checked(false)))
                            .child(row("On", Switch::new("s2").checked(true)))
                            .child(row("Off, disabled", Switch::new("s3").checked(false).disabled(true)))
                            .child(row("On, disabled", Switch::new("s4").checked(true).disabled(true)))
                    }
                }
                let view = cx.new(|_| Sheet);
                crate::modal::open(crate::modal::Modal { title: "Switches".into(), body: view.into(), width: 360., icon: None, on_close: None }, cx);
            }
            // Screens: the takeover dialog with a canned plan.
            if let Ok(kind) = std::env::var("SPM_DEBUG_TAKEOVER") {
                use sentient_installer_core::distro::adopt::TakeOverPlan;
                let plan = move |compose: Option<String>| -> Result<TakeOverPlan, String> {
                    let mut p = TakeOverPlan {
                        project: "sentient-prod".into(),
                        files: if kind == "stale" {
                            // The live file, and a temporary override that was deleted.
                            vec![("/home/ops/SENTIENT/docker/docker-compose.clickhouse.yml".into(), true), ("/tmp/sentient-test/compose.override.yml".into(), false)]
                        } else {
                            vec![("/home/ops/deploy/sentient/docker-compose.yml".into(), kind != "missing"), ("/home/ops/deploy/sentient/docker-compose.override.yml".into(), kind != "missing")]
                        },
                        env_files: vec![("/home/ops/deploy/sentient/.env".into(), true), ("/home/ops/deploy/sentient/sentient.env".into(), kind != "missing")],
                        containers: vec!["sentient".into(), "sentient-postgres".into(), "sentient-clickhouse".into()],
                        image: "quakestring/sentient:4.4.0.1".into(),
                        volumes: vec!["sentient_data".into(), "sentient_pg".into(), "sentient_ch".into()],
                        ports: vec!["8080 web".into(), "1883 MQTT".into(), "8883 MQTTS".into(), "5683 CoAP".into(), "5684 CoAPS".into()],
                        extra_env: vec!["SENTIENT_LICENSE_MODE".into(), "RUST_LOG".into()],
                        loopback_port: (kind == "missing").then_some(15432),
                        problems: Vec::new(),
                        new_file: "/var/lib/sentient-platform-manager/stack/docker-compose.yml".into(),
                        ..Default::default()
                    };
                    if let Some(c) = compose {
                        p.files = vec![(c, true)];
                    }
                    match kind.as_str() {
                        "blocked" => p.problems.push("The container sentient-clickhouse is not there.".into()),
                        "password" => {
                            p.needs_password = true;
                            p.db_user = "sentient".into();
                            p.password_why = "It is not in the containers or the compose file.".into();
                        }
                        "chpassword" => {
                            p.needs_ch_password = true;
                            p.ch_user = "sentient".into();
                            p.ch_password_why = "None of the passwords in the containers or the compose file (nor an empty one) is accepted by ClickHouse.".into();
                            p.password_from = "SENTIENT's DATABASE_URL".into();
                        }
                        "failed" => return Err("ssh: connect to host 10.0.4.12 port 22: Connection timed out".into()),
                        "loading" => std::thread::sleep(std::time::Duration::from_secs(60)),
                        _ => {}
                    }
                    Ok(p)
                };
                cx.defer(move |cx| takeover::open(std::env::var_os("SPM_DEBUG_TAKEOVER_LOCAL").is_some(), std::sync::Arc::new(plan), std::sync::Arc::new(|ch: bool, pw: String| if pw == "right" { Ok(()) } else { Err(format!("{} refused this password for the user sentient.", if ch { "ClickHouse" } else { "PostgreSQL" })) }), std::rc::Rc::new(|_, _, _: &mut App| {}), None, cx));
            }
            if std::env::var_os("SPM_DEBUG_SUDO_PROMPT").is_some() {
                if let Some(inst) = crate::remote::load().into_iter().next() {
                    cx.defer(move |cx| remote_pages::ask_sudo_password(inst, std::rc::Rc::new(|_: &mut App| {}), cx));
                }
            }
            if std::env::var_os("SPM_DEBUG_PARTS_DIALOG").is_some() {
                let part = |t: &str, m: &str, n: &str, b: u64| crate::dbstatus::DetachedPart { table: t.into(), partition: m.into(), name: n.into(), reason: "broken-on-start".into(), bytes: b };
                let list = vec![
                    part("ts_kv", "202611", "202611_1_8_1", 48_000_000),
                    part("ts_kv", "202611", "202611_9_9_0", 2_100_000),
                    part("ts_kv", "202610", "202610_33_40_2", 61_000_000),
                    part("ts_kv_dictionary", "tuple()", "all_3_3_0", 12_000),
                ];
                status::BrokenParts::open(std::sync::Arc::new(move || Ok(list.clone())), std::sync::Arc::new(|_, _| Ok("ok a\nfailed b: Code: 232. Part b is broken".into())), cx);
            }
            if std::env::var_os("SPM_DEBUG_LOG_DIALOG").is_some() {
                let view = cx.new(|cx| logs::LogView::file(crate::jobs::activity_log(), _window, cx));
                crate::modal::open(crate::modal::Modal { title: "Platform Manager log".into(), body: view.into(), width: 1000., icon: None, on_close: None }, cx);
            }
        }
        // Screens: a job in the progress strip (SPM_DEBUG_STRIP=running|failed|done).
        #[cfg(debug_assertions)]
        if let Ok(kind) = std::env::var("SPM_DEBUG_STRIP") {
            let job = cx.new(|_| crate::jobs::Job::new());
            job.update(cx, |j, cx| {
                j.set_label("Stopping ClickHouse on Plant B server");
                j.start(
                    move |sink| {
                        sink(sentient_installer_core::progress::Progress::Step { name: "Stopping ClickHouse".into() });
                        sink(sentient_installer_core::progress::Progress::Log { line: "docker compose -p sentient stop clickhouse".into() });
                        sink(sentient_installer_core::progress::Progress::Log { line: " Container sentient-clickhouse  Stopping".into() });
                        std::thread::sleep(std::time::Duration::from_secs(if kind == "running" { 600 } else { 6 }));
                        match kind.as_str() {
                            "failed" => Err("ClickHouse did not stop within 30 seconds.".into()),
                            _ => {
                                sink(sentient_installer_core::progress::Progress::Done { message: "ClickHouse is stopped.".into() });
                                Ok(())
                            }
                        }
                    },
                    cx,
                )
            });
            manager._debug_job = Some(job);
        }
        manager
    }

    /// Read the open instance's processor, memory and disk every few seconds.
    fn start_monitor(&mut self, cx: &mut Context<Self>) {
        // The reader Statistics shares (crate::stats::local_read): its first
        // reading then waits for nothing.
        let record = self.record.clone();
        let task = cx.spawn(async move |this, cx| loop {
            let r = record.clone();
            let m = cx.background_executor().spawn(async move { crate::sysmon::from_sample(&crate::stats::local_read(r.as_ref(), std::time::Duration::from_millis(250))) }).await;
            if this
                .update(cx, |page, cx| {
                    page.monitor = Some(m);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
        });
        self._tasks.push(task);
    }

    /// What needs attention, for the bell: (text, page it opens).
    fn attention(&self, cx: &App) -> Vec<(String, Page)> {
        let mut items = Vec::new();
        let (sentient, manager) = self.updates.read(cx).notices();
        if let Some(o) = manager {
            items.push((format!("Platform Manager {} is available", o.version), Page::Update));
        }
        if let Some(o) = sentient {
            items.push((format!("SENTIENT {} is available", o.version), Page::Update));
        }
        if let Some(o) = self.updates.read(cx).gateway_notice() {
            items.push((format!("Sentient Gateway {} is available", o.version), Page::Gateway));
        }
        let pending = self.record.as_ref().map(|r| crate::changes::pending(r).len()).unwrap_or(0);
        if pending > 0 {
            items.push((format!("{pending} change{} for this installation waiting", if pending == 1 { "" } else { "s" }), Page::Update));
        }
        let shown = if self.open_remote.is_some() { self.remote_monitor } else { self.monitor };
        if let Some(m) = shown.filter(|m| m.disk_low()) {
            items.push((format!("Disk almost full: {} free", crate::dbstatus::bytes_label(m.disk_free)), Page::Status));
        }
        items
    }

    /// What needs attention now, for the bell: (title, detail, page, instance
    /// id, its name, its colour). They go away by themselves.
    fn conditions(&self, cx: &App) -> Vec<(String, String, Page, Option<String>, String, String)> {
        let local = |t: String, d: String, p: Page| (t, d, p, None, "This computer".to_string(), crate::remote::LOCAL_COLOR.to_string());
        let mut out: Vec<_> = Vec::new();
        let (sentient, manager) = self.updates.read(cx).notices();
        if let Some(o) = manager {
            out.push(local(format!("Platform Manager {} is available", o.version), "Update and restart from the Update page.".into(), Page::Update));
        }
        if let Some(o) = sentient {
            out.push(local(format!("SENTIENT {} is available", o.version), "Update from the Update page.".into(), Page::Update));
        }
        if let Some(o) = self.updates.read(cx).gateway_notice() {
            out.push(local(format!("Sentient Gateway {} is available", o.version), "Update from the Gateway page.".into(), Page::Gateway));
        }
        let pending = self.record.as_ref().map(|r| crate::changes::pending(r).len()).unwrap_or(0);
        if pending > 0 {
            out.push(local(format!("{pending} change{} waiting", if pending == 1 { "" } else { "s" }), "For this installation, on the Update page.".into(), Page::Update));
        }
        if let Some(m) = self.monitor.filter(|m| m.disk_low()) {
            out.push(local("Disk almost full".into(), format!("{} free", crate::dbstatus::bytes_label(m.disk_free)), Page::Status));
        }
        if let (Some(id), Some(m)) = (&self.open_remote, self.remote_monitor.filter(|m| m.disk_low())) {
            if let Some(r) = self.remotes.iter().find(|r| &r.id == id) {
                out.push(("Disk almost full".into(), format!("{} free", crate::dbstatus::bytes_label(m.disk_free)), Page::Status, Some(r.id.clone()), r.name.clone(), r.color.clone()));
            }
        }
        out
    }

    /// The conditions the bell shows: not cleared, each with its key and
    /// the time it was first seen. One gone is forgotten, so coming back is
    /// news again.
    fn bell_conditions(&self, cx: &App) -> Vec<(String, i64, (String, String, Page, Option<String>, String, String))> {
        let now = chrono::Utc::now().timestamp();
        let all = self.conditions(cx);
        let keys: Vec<String> = all.iter().map(|c| cond_key(&c.3, &c.0)).collect();
        let mut since = self.cond_since.borrow_mut();
        since.retain(|k, _| keys.contains(k));
        self.cond_cleared.borrow_mut().retain(|k| keys.contains(k));
        let cleared = self.cond_cleared.borrow();
        all.into_iter()
            .zip(keys)
            .filter(|(_, k)| !cleared.contains(k))
            .map(|(c, k)| {
                let at = *since.entry(k.clone()).or_insert(now);
                (k, at, c)
            })
            .collect()
    }

    /// The bell's panel: Notifications, Mark all as read / Clear all, the
    /// events and conditions with their instance, or an empty state.
    fn notifications_panel(&self, conditions: Vec<(String, i64, (String, String, Page, Option<String>, String, String))>, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let events = self.strip.read(cx).events.clone();
        let (unread, _) = self.strip.read(cx).unread();
        let w = (width - 24.).min(400.);
        let now = chrono::Utc::now().timestamp();
        let ago = |at: i64| {
            let m = (now - at).max(0) / 60;
            if m < 1 { "just now".to_string() } else if m < 60 { format!("{m} min ago") } else if m < 24 * 60 { format!("{} h ago", m / 60) } else { format!("{} d ago", m / (24 * 60)) }
        };
        let strip = self.strip.clone();
        let this = cx.entity().downgrade();
        // Each entry a rounded card in a faint tint of what happened (teal
        // done, red failed, amber a warning): 8 % light / 14 % dark, a little
        // stronger while unread, a step darker on hover. The instance's own
        // colour is the dot by its name.
        let dark = theme::is_dark(cx);
        let row = |key: SharedString, icon: IconName, icon_color: Hsla, title: String, detail: String, meta: String, color: &str, unread: bool, clear: AnyElement| {
            let c = crate::remote::parse_hex(color).unwrap_or(0x187074);
            let base = if dark { 0.07 } else { 0.04 };
            let tint = base + if unread { 0.02 } else { 0. };
            h_flex()
                .id(key.clone())
                .group(key)
                .relative()
                .gap_2()
                .items_start()
                .pl(px(10.))
                .pr_2()
                .py(px(9.))
                .rounded(px(8.))
                .bg(icon_color.opacity(tint))
                .cursor_pointer()
                .hover(move |r| r.bg(icon_color.opacity(tint + 0.03)))
                .child(Icon::new(icon).size_4().text_color(icon_color).mt(px(2.)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .gap(px(6.))
                                .items_center()
                                .min_w_0()
                                // Unread: a dot before the title, away from the ×.
                                .when(unread, |c| c.child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(icon_color)))
                                .child(div().min_w_0().text_sm().font_weight(if unread { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).overflow_hidden().whitespace_nowrap().text_ellipsis().child(title)),
                        )
                        .child(div().text_size(px(13.)).text_color(theme.muted_foreground).overflow_hidden().whitespace_nowrap().text_ellipsis().child(detail))
                        .child(
                            h_flex()
                                .gap(px(5.))
                                .items_center()
                                .child(div().size(px(7.)).rounded_full().bg(rgb(c)))
                                .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(meta)),
                        ),
                )
                // Right: the row's own clear (×), always shown so one line
                // can be cleared without hunting for it.
                .child(div().flex_shrink_0().child(clear))
        };
        // One × for every row: an event is cleared, a condition hidden
        // until it changes.
        let clear_button = |id: SharedString, on: Box<dyn Fn(&mut App)>| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .hover(|d| d.bg(theme.danger.opacity(0.12)))
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Clear").build(w, cx))
                .child(Icon::new(IconName::Close).size(px(16.)).text_color(theme.muted_foreground))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    on(cx);
                })
                .into_any_element()
        };
        // Events and conditions together, newest first.
        let mut rows: Vec<(i64, Stateful<Div>)> = Vec::new();
        let cond_keys: Vec<String> = conditions.iter().map(|c| c.0.clone()).collect();
        for (i, (key, at, (title, detail, page, inst, name, color))) in conditions.into_iter().enumerate() {
            let (t, t2) = (this.clone(), this.clone());
            // Per type: information (an update waiting) in the info blue, a
            // warning in amber.
            let icon = if title.contains("available") || title.contains("waiting") { (IconName::ArrowUp, theme::info(cx)) } else { (IconName::TriangleAlert, theme.warning) };
            let clear = clear_button(
                SharedString::from(format!("cond-clear-{i}")),
                Box::new(move |cx| {
                    let key = key.clone();
                    t2.update(cx, |m, cx| {
                        m.cond_cleared.borrow_mut().insert(key);
                        cx.notify();
                    })
                    .ok();
                }),
            );
            rows.push((
                at,
                row(SharedString::from(format!("cond-{i}")), icon.0, icon.1, title, detail, format!("{name} · {}", ago(at)), &color, false, clear).on_click(move |_, _, cx| {
                    let (inst, page) = (inst.clone(), page);
                    t.update(cx, |m, cx| {
                        m.bell_open = false;
                        if m.open_remote != inst {
                            m.open_instance(inst, cx);
                        }
                        m.page = page;
                        cx.notify();
                    })
                    .ok();
                }),
            ));
        }
        for e in events.iter() {
            let (id, inst) = (e.id, e.instance.clone());
            let (s1, s2, t) = (strip.clone(), strip.clone(), this.clone());
            let icon = if e.failed { (IconName::CircleX, theme.danger) } else { (IconName::CircleCheck, theme.success_foreground) };
            let clear = clear_button(SharedString::from(format!("clear-{id}")), Box::new(move |cx| s1.update(cx, |s, cx| s.clear(Some(id), cx))));
            rows.push((
                e.at,
                row(SharedString::from(format!("ev-{id}")), icon.0, icon.1, e.title.clone(), e.detail.clone(), format!("{} · {}", e.instance_name, ago(e.at)), &e.color, !e.read, clear).on_click(move |_, _, cx| {
                    let inst = inst.clone();
                    t.update(cx, |m, cx| {
                        m.bell_open = false;
                        if m.open_remote != inst {
                            m.open_instance(inst, cx);
                        }
                    })
                    .ok();
                    s2.update(cx, |s, cx| s.open_event(id, cx));
                }),
            ));
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        let empty = rows.is_empty();
        let n_rows = rows.len();
        let list = v_flex().gap(px(6.)).children(rows.into_iter().map(|(_, r)| r));
        let (s3, s4, close) = (strip.clone(), strip.clone(), this.clone());
        let header = h_flex()
            .px_2()
            .pt_1()
            .pb_2()
            .gap_2()
            .items_center()
            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("Notifications"))
            .when(unread > 0, |h| h.child(crate::ui::button("bell-read").ghost().label("Mark all as read").on_click(move |_, _, cx| s3.update(cx, |s, cx| s.mark_all_read(cx)))))
            .when(!empty, |h| {
                let close2 = close.clone();
                h.child(crate::ui::button("bell-clear").ghost().label("Clear all").on_click(move |_, _, cx| {
                    s4.update(cx, |s, cx| s.clear(None, cx));
                    let keys = cond_keys.clone();
                    close2
                        .update(cx, |m, cx| {
                            m.cond_cleared.borrow_mut().extend(keys);
                            cx.notify();
                        })
                        .ok();
                }))
            });
        // As tall as its entries, at most the window below the bar less
        // 24 px, and never over 560 px; the list scrolls inside, the header
        // stays (the 50 newest events are kept, crate::manager::strip).
        // Grows with its entries up to half the window (at least 240 px),
        // and only then scrolls.
        let max_h = (f32::from(self.window_h.get()) * 0.5).max(240.);
        // A long list: the panel takes its full height and the list scrolls
        // in it (a scrolling child has no height of its own to grow by).
        let full = 52. + n_rows as f32 * 82. > max_h;
        deferred(
            div().absolute().top(px(theme::BAR_CTRL_H + 10.)).right(px(-6.)).child(
                div()
                    .relative()
                    .w(px(w))
                    .child(
                        v_flex()
                            .id("bell-panel")
                            .relative()
                            .child(crate::ui::layout_probe("overlay"))
                            .child(crate::ui::layout_probe("bell-panel"))
                            .occlude()
                            .p_2()
                            .min_h(px(240.))
                            .max_h(px(max_h))
                            .when(full, |p| p.h(px(max_h)))
                            .rounded(px(ui::PANEL_RADIUS))
                            .bg(theme.popover)
                            .text_color(theme.popover_foreground)
                            .border_1()
                            .border_color(theme.border)
                            .shadow_lg()
                            .child(header)
                            .child(if empty {
                                v_flex()
                                    .flex_1()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .py_8()
                                    .child(Icon::new(IconName::Bell).size_6().text_color(theme.muted_foreground.opacity(0.6)))
                                    .child(div().text_sm().text_color(theme.muted_foreground).child("No new notifications"))
                                    .into_any_element()
                            } else {
                                // Short: the list as it is (the panel takes its height).
                                // Long: the panel at its most, the list scrolling in
                                // it, with room on the right so the scrollbar never
                                // sits on the cards.
                                if full {
                                    div().id("bell-list").flex_1().min_h_0().child(div().pr(px(10.)).child(list)).overflow_y_scrollbar().into_any_element()
                                } else {
                                    div().id("bell-list").child(list).into_any_element()
                                }
                            })
                            .on_mouse_down_out(move |_, _, cx| {
                                close.update(cx, |m, cx| {
                                    m.bell_open = false;
                                    cx.notify();
                                })
                                .ok();
                            }),
                    )
                    .child(ui::pointer_up(theme.popover, theme.border, px(w - 26.))),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }

    /// The top bar: the Platform Manager's icon, the instance that is
    /// open, its processor / memory / disk, and what needs attention. It
    /// takes the open instance's colour (SENTIENT teal for this computer).
    fn top_bar(&self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = theme::is_dark(cx);
        let menu_theme = cx.theme().clone();
        let remote = self.open_remote.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned();
        // The bar is tinted like the tile headers: the instance's colour
        // mixed into the card colour (theme::bar_tint), the theme's
        // own text on it (the approved shell mockup).
        let ic: Hsla = match &remote {
            Some(r) => rgb(crate::remote::parse_hex(&r.color).unwrap_or(0x187074)).into(),
            None => rgb(0x187074).into(),
        };
        // Switching instance: the tint cross-fades to the new colour.
        let ic = ui::tween_color("bar-ic", ic, 250, window);
        let bg: Hsla = menu_theme.background.blend(ic.opacity(theme::bar_tint(dark)));
        let fg: Hsla = menu_theme.foreground;
        let muted_fg: Hsla = menu_theme.muted_foreground;
        let (name, address): (String, Option<String>) = match (&remote, &self.record) {
            (Some(r), _) => (r.name.clone(), Some(format!("{} over SSH", r.host))),
            (None, Some(r)) => ("This computer".into(), Some(format!("SENTIENT {}", r.sentient_version))),
            (None, None) => ("This computer".into(), None),
        };
        let instance = format!("{name} {}", address.clone().unwrap_or_default());
        let light_bar = !dark;
        // The switcher's pill: the instance's colour over the bar.
        let inset: Hsla = ic.opacity(theme::bar_inset(dark));
        // The open instance's state: green when SENTIENT runs, red when it
        // is stopped (the page's last reading, else the checker's), amber
        // while the server is being read or does not answer.
        let status_green: Hsla = rgb(0x3ddc84).into();
        let serving = match &remote {
            Some(r) => self.remote_pages.get(&r.id).and_then(|p| p.read(cx).serving()).or_else(|| crate::bg_app::serving(Some(&r.id), cx)),
            None => crate::bg_app::serving(None, cx),
        };
        // The dot is the open instance's health (the mockup's S8): green
        // fine, amber needs a look, red a fault, grey not known yet or not
        // answering; its tooltip says why.
        let shown_m = if remote.is_some() { self.remote_monitor } else { self.monitor };
        let installed = remote.is_some() || self.record.is_some();
        let (state_dot, state_why): (Hsla, &'static str) = if !installed {
            (rgb(0x94a3b8).into(), "SENTIENT isn't installed on this computer")
        } else if remote.is_some() && (self.remote_monitor.is_none() || self.remote_monitor_stale) {
            (rgb(0x94a3b8).into(), "The server does not answer, or is being read")
        } else if serving == Some(false) {
            (rgb(0xef4444).into(), "SENTIENT is stopped")
        } else if shown_m.is_some_and(|m| m.disk_low()) {
            (rgb(0xf59e0b).into(), "Needs a look: the disk is almost full")
        } else if serving.is_none() {
            (rgb(0x94a3b8).into(), "Being checked")
        } else {
            (status_green, "SENTIENT runs")
        };
        let pulse = state_why.starts_with("SENTIENT is stopped") || state_why.starts_with("Needs a look");
        // The dot changes colour smoothly.
        let state_dot = ui::tween_color("health-dot", state_dot, 300, window);
        self.strip.read(cx).light_bar.set(light_bar);
        self.strip.read(cx).window_w.set(width);
        // The mockup's top bar: above 900 px the product name and the
        // monitors' bars show; below, only the switcher and the values.
        let wide = width > 900.;
        let warn_fill: Hsla = rgb(0xf59e0b).into();
        let warn_text: Hsla = rgb(0xfcd34d).into();
        // Monitor bars: the instance's colour mixed with the text colour on a
        // faint track of the text colour.
        let meter_fill: Hsla = fg.blend(ic.opacity(0.65));
        let meter_icon = |label: &str| -> Icon {
            match label {
                "CPU" => Icon::new(Lucide::Cpu),
                "Memory" => Icon::new(Lucide::MemoryStick),
                _ => Icon::new(Lucide::HardDrive),
            }
        };
        let meter = |id: &'static str, label: &'static str, widest: &str, value: String, fraction: f32, high: bool| {
            h_flex()
                .id(id)
                .relative()
                .gap(px(7.))
                .items_center()
                .text_size(px(13.))
                .child(meter_icon(label).size(px(16.)).text_color(muted_fg))
                .when(wide, |r| {
                    r.child(
                        div().w(px(36.)).child(ui::segment_bar(fg.opacity(0.14), 5., &[(if high { warn_fill } else { meter_fill }, fraction.clamp(0.0, 1.0))])),
                    )
                })
                // A fixed slot for the widest value: the bars and the bell
                // never move when a value changes.
                .child(ui::live_value_left(widest, 13., value).text_color(if high { warn_text } else { fg }))
                .child(crate::ui::layout_probe(id))
        };
        // What fits: a rough width per piece (about 7 px a character at
        // 12.5-14 px), kept in this order: CPU, memory, disk.
        let text_w = |s: &str| s.chars().count() as f32 * 6.9;
        // The progress chip's smallest form (spinner and time) always has its
        // room, job or not: the monitors are laid out the same either way.
        // The bar's padding, the switcher (dot, name, address, chevron,
        // REMOTE), the theme button and the bell, and the gaps between.
        let mut room = width - 36. - (text_w(&instance) + 14. + 10. + 16. + 10. + 24. + 14.) - if remote.is_some() { 64. } else { 0. } - (theme::BAR_CTRL_H + 14.) - (theme::BAR_CTRL_H + 14.) - if self.strip.read(cx).active() { strip::CHIP_MIN } else { 0. };

        let mut fits = |need: f32| {
            let ok = room >= need;
            if ok {
                room -= need;
            }
            ok
        };
        let shown = if remote.is_some() { self.remote_monitor } else { self.monitor };
        // Each value's slot is sized for its widest text on THIS machine
        // (used memory never passes the total, free disk never the disk),
        // every digit an 8 (tabular figures share one width).
        let eights = |s: String| s.chars().map(|c| if c.is_ascii_digit() { '8' } else { c }).collect::<String>();
        let gbf = |b: u64| b as f64 / (1u64 << 30) as f64;
        // (Before the first reading: typical widths, so the placeholders take
        // the room the values will.)
        let mem_widest = shown.map(|m| eights(format!("{:.1} / {:.0} GB", gbf(m.mem_total), gbf(m.mem_total)))).unwrap_or_else(|| "88.8 / 88 GB".into());
        // On a disk of a terabyte or more, less than a terabyte free reads in GB.
        let disk_widest = shown
            .map(|m| {
                let total = eights(format!("{} free", crate::dbstatus::bytes_label(m.disk_total)));
                if m.disk_total >= 1 << 40 && total.len() < "8888.8 GB free".len() { "8888.8 GB free".to_string() } else { total }
            })
            .unwrap_or_else(|| "888.8 GB free".into());
        // The same room with and without a reading (placeholders before it).
        let (show_cpu, show_mem, show_disk) = {
            // Each: its 18 px icon, the bar (when wide) and the value's slot,
            // 16 px apart.
            let bar = if wide { 36. + 7. } else { 0. };
            let slot = |w: &str| f32::from(ui::slot_width(w, 13.));
            let cpu = fits(16. + 16. + 7. + slot("100%") + bar);
            let mem = cpu && fits(16. + 16. + 7. + slot(&mem_widest) + bar);
            let disk = mem && fits(16. + 16. + 7. + slot(&disk_widest) + bar);
            (cpu, mem, disk)
        };
        // What the monitors leave between the switcher and themselves: the
        // progress chip's slot.
        self.strip.read(cx).slot_w.set(room.max(0.) + strip::CHIP_MIN);
        // Before the first reading: the monitors as placeholders in their
        // slots, so nothing moves when the values come.
        let monitor_loading = shown.is_none().then(|| {
            let slot = |label: &'static str, widest: &str| {
                h_flex()
                    .gap(px(7.))
                    .items_center()
                    .text_size(px(13.))
                    .child(meter_icon(label).size(px(16.)).text_color(muted_fg))
                    .when(wide, |r| r.child(div().w(px(36.)).h(px(5.)).rounded_full().bg(fg.opacity(0.14))))
                    .child(div().w(ui::slot_width(widest, 12.5)).flex().items_center().child(div().w(relative(0.75)).h(px(10.)).rounded(px(5.)).bg(fg.opacity(0.22))))
            };
            h_flex()
                .gap(px(16.))
                .h(px(theme::BAR_CTRL_H))
                .items_center()
                .px(px(10.))
                .when(show_cpu, |r| r.child(slot("CPU", "100%")))
                .when(show_mem, |r| r.child(slot("Memory", &mem_widest)))
                .when(show_disk, |r| r.child(slot("Disk", &disk_widest)))
        });
        // The bars glide to a new reading.
        let glide = shown.map(|m| {
            (
                ui::tween("m-cpu", m.cpu / 100.0, 400, window),
                ui::tween("m-mem", if m.mem_total > 0 { m.mem_used as f32 / m.mem_total as f32 } else { 0.0 }, 400, window),
                ui::tween("m-disk", if m.disk_total > 0 { 1.0 - m.disk_free as f32 / m.disk_total as f32 } else { 0.0 }, 400, window),
            )
        });
        let monitor = shown.zip(glide).map(|(m, (g_cpu, g_mem, g_disk))| {
            let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
            h_flex()
                .id("monitor")
                .relative()
                .child(crate::ui::layout_probe("monitors"))
                .child(crate::ui::layout_probe("bar-ctrl"))
                .gap(px(16.))
                .h(px(theme::BAR_CTRL_H))
                .items_center()
                .px(px(10.))
                .rounded(px(theme::BAR_CTRL_RADIUS))
                .cursor_pointer()
                .hover(move |d| d.bg(ic.opacity(theme::bar_ctrl_hover(dark))))
                .when(show_cpu, |r| r.child(meter("m-cpu", "CPU", "100%", format!("{:.0}%", m.cpu), g_cpu, m.cpu_high())))
                .when(show_mem, |r| r.child(meter(
                    "m-mem",
                    "Memory",
                    &mem_widest,
                    format!("{:.1} / {:.0} GB", gb(m.mem_used), gb(m.mem_total)),
                    g_mem,
                    m.mem_high(),
                )))
                .when(show_disk, |r| r.child(meter(
                    "m-disk",
                    "Disk",
                    &disk_widest,
                    format!("{} free", crate::dbstatus::bytes_label(m.disk_free)),
                    g_disk,
                    m.disk_low(),
                )))
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Statistics").build(w, cx))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.page = Page::Statistics;
                    cx.notify();
                }))
        });
        // The bell: events (clearable, kept across restarts) and conditions
        // (they stay while they hold). Every entry names its instance.
        let conditions = self.bell_conditions(cx);
        let (unread, unread_failed) = self.strip.read(cx).unread();
        let count = unread + conditions.len();
        let bell_open = self.bell_open;
        let bell = div()
            .id("bell")
            .relative()
            .child(crate::ui::layout_probe("bar-ctrl"))
            .size(px(theme::BAR_CTRL_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .rounded(px(theme::BAR_CTRL_RADIUS))
            .when(bell_open, |b| b.bg(ic.opacity(theme::bar_ctrl_hover(dark))))
            .hover(move |b| b.bg(ic.opacity(theme::bar_ctrl_hover(dark))))
            .child(Icon::new(IconName::Bell).text_color(fg).size(px(20.)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.bell_open = !this.bell_open;
                cx.notify();
            }));
        let bell_panel = bell_open.then(|| self.notifications_panel(conditions, width, cx));
        let unseen = if unread_failed { 1 } else { 0 };
        let line = self.strip.read(cx).line(window, cx);
        h_flex()
            .id("top-bar")
            .relative()
            .child(ui::layout_probe("top-bar"))
            .children(line)
            .h(px(shell::BAR_H))
            .flex_shrink_0()
            .pl(px(20.))
            .pr(px(16.))
            .gap(px(14.))
            .items_center()
            .bg(bg)
            .border_b_1()
            .border_color(menu_theme.border)
            .text_color(fg)
            .child({
                // The mockup's option B: a darker inset of the bar itself, no
                // border; hover one step darker (the text keeps its contrast).
                // Its menu is the app's popover, like the theme menu.
                let open = self.switcher_open;
                let hover_inset: Hsla = ic.opacity(theme::bar_ctrl_hover(dark));
                let chip = div()
                    .id("switcher")
                    .flex()
                    .items_center()
                    .min_w_0()
                    .overflow_hidden()
                    .h(px(theme::BAR_CTRL_H))
                    .px(px(12.))
                    .rounded(px(theme::BAR_CTRL_RADIUS))
                    .child(crate::ui::layout_probe("bar-ctrl"))
                    .cursor_pointer()
                    .text_color(fg)
                    .bg(if open { hover_inset } else { inset })
                    .hover(move |d| d.bg(hover_inset))
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .items_center()
                            .min_w_0()
                            .text_size(px(14.))
                            // The health dot, ringed in the card colour; its tooltip says why.
                            .child(
                                div()
                                    .id("health-dot")
                                    .relative()
                                    .size(px(14.))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(menu_theme.background)
                                    .bg(state_dot)
                                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(state_why).build(w, cx))
                                    // Turning amber or red: one soft pulse (300 ms).
                                    .when(pulse && !ui::reduce_motion(), |d| {
                                        d.child(
                                            div()
                                                .absolute()
                                                .top(px(-2.))
                                                .left(px(-2.))
                                                .size(px(14.))
                                                .rounded_full()
                                                .border_2()
                                                .border_color(state_dot)
                                                .with_animation(SharedString::from(format!("pulse-{state_why}")), Animation::new(std::time::Duration::from_millis(300)).with_easing(ui::ease_out), |el, t| {
                                                    let grow = 7. * t;
                                                    el.top(px(-2. - grow)).left(px(-2. - grow)).size(px(14. + 2. * grow)).opacity(1. - t)
                                                }),
                                        )
                                    }),
                            )
                            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(name))
                            // On a narrow window the address gives way first.
                            .when_some(address, |r, a| r.child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(13.)).text_color(muted_fg).child(a)))
                            .child(Icon::new(IconName::ChevronDown).size_4().text_color(muted_fg))
                            .when(remote.is_some(), |r| {
                                r.child(div().px(px(7.)).py(px(1.)).rounded(px(5.)).bg(ic).text_color(white()).text_size(px(11.)).font_weight(FontWeight::BOLD).child("REMOTE"))
                            }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.switcher_open = !this.switcher_open;
                        cx.notify();
                    }));
                let popover = open.then(|| {
                    let this = cx.entity().downgrade();
                    let go = |t: WeakEntity<Manager>, id: Option<String>| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
                        std::rc::Rc::new(move |_, cx| {
                            t.update(cx, |m, cx| {
                                m.switcher_open = false;
                                m.open_instance(id.clone(), cx);
                            })
                            .ok();
                        })
                    };
                    let mut rows = vec![ui::MenuRow::Item {
                        id: "switch-local".into(),
                        icon: Some(Icon::new(Lucide::Monitor).size(px(18.)).into_any_element()),
                        label: if self.open_remote.is_some() { "Back to this computer".into() } else { "This computer".into() },
                        secondary: None,
                        on: self.open_remote.is_none(),
                        click: go(this.clone(), None),
                    }];
                    if !self.remotes.is_empty() {
                        rows.push(ui::MenuRow::Divider);
                    }
                    for r in &self.remotes {
                        let c = crate::remote::parse_hex(&r.color).unwrap_or(0x187074);
                        rows.push(ui::MenuRow::Item {
                            id: SharedString::from(format!("switch-{}", r.id)),
                            icon: Some(div().size(px(10.)).mx(px(3.)).rounded_full().bg(rgb(c)).into_any_element()),
                            label: r.name.clone().into(),
                            secondary: Some(format!("{} over SSH", r.host).into()),
                            on: self.open_remote.as_deref() == Some(r.id.as_str()),
                            click: go(this.clone(), Some(r.id.clone())),
                        });
                    }
                    rows.push(ui::MenuRow::Divider);
                    let t = this.clone();
                    rows.push(ui::MenuRow::Item {
                        id: "switch-manage".into(),
                        icon: Some(Icon::new(Lucide::Settings).size(px(18.)).into_any_element()),
                        label: "Manage instances…".into(),
                        secondary: None,
                        on: false,
                        click: std::rc::Rc::new(move |_, cx| {
                            t.update(cx, |m, cx| {
                                m.switcher_open = false;
                                m.page = Page::Instances;
                                cx.notify();
                            })
                            .ok();
                        }),
                    });
                    let close = this.clone();
                    deferred(div().absolute().top(px(theme::BAR_CTRL_H + 10.)).left_0().child(ui::popover_menu(&menu_theme, "switcher-popover", 320., rows, true, px(24.), move |_, cx| {
                        close.update(cx, |m, cx| {
                            m.switcher_open = false;
                            cx.notify();
                        })
                        .ok();
                    })))
                    .with_priority(1)
                });
                div().relative().min_w_0().flex_shrink(1.).child(chip).children(popover)
            })
            // A running job: its chip in the middle of the bar.
            .child(h_flex().flex_1().min_w_0().overflow_hidden().justify_center().child(self.strip.clone()))
            .children(monitor)
            .children(monitor_loading)
            .child(self.theme_button(ic, cx))
            .child(
                // The bell with an amber count, as in the mockup.
                div().relative().child(bell).children(bell_panel).when(count > 0, |b| {
                    b.child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .right(px(0.))
                            .min_w(px(16.))
                            .h(px(16.))
                            .px(px(4.))
                            .rounded_full()
                            // Red while a failed job is unread, else amber.
                            .bg(if unseen > 0 { rgb(0xdc2626) } else { rgb(0xf59e0b) })
                            .text_color(if unseen > 0 { rgb(0xffffff) } else { rgb(0x1c1917) })
                            .text_size(px(10.5))
                            .font_weight(FontWeight::BOLD)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(count.to_string()),
                    )
                }),
            )
    }

    /// Switch to an instance (None: this computer). Remote instances are
    /// worked on from their card on Instances for now.
    fn open_instance(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let t0 = std::time::Instant::now();
        let _timing = SwitchTiming(t0, id.clone());
        if id.is_some() {
            self.page = Page::Status;
        }
        // The pages kept for the instance being left stop their own
        // reading (logs, statistics) until it is opened again.
        let left = self.open_remote.clone();
        if left != id {
            if let Some(p) = left.as_ref().and_then(|l| self.remote_pages.get(l)).cloned() {
                p.update(cx, |p, cx| p.set_hidden(true, cx));
            }
            if let Some(p) = self.stats.get(&left).cloned() {
                p.update(cx, |p, _| p.set_paused(true));
            }
        }
        self.open_remote = id.clone();
        self.open_gen += 1;
        self.fresh.clear();
        // The top bar shows the last reading at once (none on a first open).
        self.remote_monitor = id.as_ref().and_then(|id| self.monitor_cache.get(id).copied());
        self.remote_monitor_stale = false;
        if id.is_none() {
            for page in self.status.clone() {
                page.update(cx, |p, cx| p.refresh(cx));
            }
        }
        if let Some(inst) = id.as_ref().and_then(|id| self.remotes.iter().find(|r| &r.id == id)).cloned() {
            // Read it every few seconds while it stays open. Each reading is
            // for this instance and this opening (`gen`): one that comes
            // back after a switch is dropped.
            let gen = self.open_gen;
            let task = cx.spawn(async move |this, cx| loop {
                let i = inst.clone();
                let m = cx.background_executor().spawn(async move { crate::remote::monitor(&i).ok() }).await;
                let still_open = this
                    .update(cx, |page, cx| {
                        let open = page.open_gen == gen && page.open_remote.as_deref() == Some(inst.id.as_str());
                        if open {
                            match m {
                                Some(m) => {
                                    page.remote_monitor = Some(m);
                                    page.remote_monitor_stale = false;
                                    page.monitor_cache.insert(inst.id.clone(), m);
                                }
                                // No answer: the last reading stays, the
                                // dot says it does not answer.
                                None => page.remote_monitor_stale = true,
                            }
                            cx.notify();
                        }
                        open
                    })
                    .unwrap_or(false);
                if !still_open {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
            });
            self._tasks.push(task);
        }
        if let Some(page) = &self.instances {
            page.update(cx, |p, cx| {
                p.open = id;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// The instance list changed: what is kept for an instance that went,
    /// or whose connection changed, goes too.
    fn prune_kept(&mut self, before: &[crate::remote::Instance]) {
        let same = |a: &crate::remote::Instance| self.remotes.iter().any(|b| b.id == a.id && serde_json::to_string(a).ok() == serde_json::to_string(b).ok());
        let gone: Vec<String> = before.iter().filter(|a| !same(a)).map(|a| a.id.clone()).collect();
        for id in gone {
            self.remote_pages.remove(&id);
            self.monitor_cache.remove(&id);
            let k = Some(id);
            self.stats.remove(&k);
            self.licence.remove(&k);
            self.files.remove(&k);
        }
    }

    /// The Update screen, made when first needed.
    fn update_page(&mut self, cx: &mut Context<Self>) -> Entity<update::UpdatePage> {
        let record = self.record.clone();
        let updates = self.updates.clone();
        self.update.get_or_insert_with(|| cx.new(|cx| update::UpdatePage::new(record, updates, cx))).clone()
    }

    /// Open the Update screen on a product's release notes.
    fn whats_new(&mut self, product: update::Product, cx: &mut Context<Self>) {
        let page = self.update_page(cx);
        page.update(cx, |p, cx| p.show_notes(product, cx));
        self.page = Page::Update;
        cx.notify();
    }

    /// The notices above every screen: what this run follows (a self-update,
    /// a Setup that found SENTIENT) and a newer SENTIENT, Platform Manager
    /// or gateway on the update channel (crate::updates).
    fn notices(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let (sentient, manager) = self.updates.read(cx).notices();
        let gw = self.updates.read(cx).gateway_notice();
        let pending = self.record.as_ref().map(|r| crate::changes::pending(r).len()).unwrap_or(0);
        if sentient.is_none() && manager.is_none() && gw.is_none() && self.updated_from.is_none() && !self.found_notice {
            return None;
        }
        let bar = |id: &'static str, tone: crate::ui::Tone, text: String| {
            let (bg, fg) = match tone {
                crate::ui::Tone::Good => (theme.success, theme.success_foreground),
                _ => (theme.accent, theme.accent_foreground),
            };
            h_flex()
                .id(id)
                .gap_3()
                .px_4()
                .py_2()
                .items_center()
                .bg(bg)
                .text_color(fg)
                .border_b_1()
                .border_color(theme.border)
                .child(Icon::new(if matches!(tone, crate::ui::Tone::Good) { IconName::CircleCheck } else { IconName::Info }).small())
                // The buttons never wrap: the text is cut instead.
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().child(text))
        };
        let mut col = v_flex();
        // B3/B4: this run follows a self-update.
        if let Some(from) = self.updated_from.clone() {
            let this = crate::updates::manager_version();
            let text = if pending > 0 {
                format!("Platform Manager updated to {this} (from {from}). {pending} change{} for this installation are waiting on the Update screen.", if pending == 1 { "" } else { "s" })
            } else {
                format!("Platform Manager updated to {this}.")
            };
            col = col.child(
                bar("notice-updated", crate::ui::Tone::Good, text)
                    .when(pending > 0, |b| {
                        b.child(crate::ui::button("notice-updated-changes").primary().label("Show the changes").on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Update;
                            cx.notify();
                        })))
                    })
                    .child(crate::ui::button("notice-updated-close").ghost().label("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.updated_from = None;
                        cx.notify();
                    }))),
            );
        }
        // C7c: Setup installed this Platform Manager beside a SENTIENT.
        if self.found_notice {
            if let Some(r) = &self.record {
                col = col.child(
                    bar(
                        "notice-found",
                        crate::ui::Tone::Good,
                        format!("Found SENTIENT {} on this computer with its database, settings and backups. Nothing was changed.", r.sentient_version),
                    )
                    .child(crate::ui::button("notice-found-close").ghost().label("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.found_notice = false;
                        cx.notify();
                    }))),
                );
            }
        }
        // Several updates: one bar that counts them and opens Update.
        let several = [manager.is_some(), sentient.is_some(), gw.is_some()].iter().filter(|b| **b).count();
        if several >= 2 {
            let (mv, sv, gv) = (manager.as_ref().map(|m| m.version.clone()), sentient.as_ref().map(|s| s.version.clone()), gw.as_ref().map(|g| g.version.clone()));
            col = col.child(
                bar("notice-several", crate::ui::Tone::Neutral, format!("{several} updates available"))
                    .child(crate::ui::button("notice-several-go").primary().label("View").on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Update;
                        cx.notify();
                    })))
                    .child(crate::ui::button("notice-several-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                        let (mv, sv, gv) = (mv.clone(), sv.clone(), gv.clone());
                        this.updates.update(cx, |s, cx| {
                            if let Some(v) = mv {
                                s.dismiss_manager(v, cx);
                            }
                            if let Some(v) = sv {
                                s.dismiss_sentient(v, cx);
                            }
                            if let Some(v) = gv {
                                s.dismiss_gateway(v, cx);
                            }
                        });
                    }))),
            );
            return Some(col.into_any_element());
        }
        match (&manager, &sentient) {
            // A4: both; the Platform Manager goes first.
            (Some(m), Some(s)) => {
                let (mv, sv) = (m.version.clone(), s.version.clone());
                col = col.child(
                    bar("notice-both", crate::ui::Tone::Neutral, format!("Platform Manager {} and SENTIENT {} are available.", m.version, s.version))
                        .when(m.installer.is_some(), |b| {
                            b.child(crate::ui::button("notice-both-go").primary().label("Update Platform Manager").on_click(cx.listener(|this, _, _, cx| {
                                let page = this.update_page(cx);
                                page.update(cx, |p, cx| p.start_manager_update(cx));
                                this.page = Page::Update;
                                cx.notify();
                            })))
                        })
                        .child(crate::ui::button("notice-both-notes").ghost().label("What's new").on_click(cx.listener(|this, _, _, cx| this.whats_new(update::Product::Manager, cx))))
                        .child(crate::ui::button("notice-both-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                            let (mv, sv) = (mv.clone(), sv.clone());
                            this.updates.update(cx, |s, cx| {
                                s.dismiss_manager(mv, cx);
                                s.dismiss_sentient(sv, cx);
                            });
                        }))),
                );
            }
            // A2: a new Platform Manager.
            (Some(m), None) => {
                let v = m.version.clone();
                col = col.child(
                    bar("notice-manager", crate::ui::Tone::Neutral, format!("Platform Manager {} is available.", m.version))
                        .when(m.installer.is_some(), |b| {
                            b.child(crate::ui::button("notice-manager-go").primary().label("Update and restart").on_click(cx.listener(|this, _, _, cx| {
                                let page = this.update_page(cx);
                                page.update(cx, |p, cx| p.start_manager_update(cx));
                                this.page = Page::Update;
                                cx.notify();
                            })))
                        })
                        .child(crate::ui::button("notice-manager-notes").ghost().label("What's new").on_click(cx.listener(|this, _, _, cx| this.whats_new(update::Product::Manager, cx))))
                        .child(crate::ui::button("notice-manager-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                            let v = v.clone();
                            this.updates.update(cx, |s, cx| s.dismiss_manager(v, cx));
                        }))),
                );
            }
            // A3: a new SENTIENT.
            (None, Some(s)) => {
                let v = s.version.clone();
                col = col.child(
                    bar("notice-sentient", crate::ui::Tone::Neutral, format!("SENTIENT {} is available{}.", s.version, dated(&s.date)))
                        .child(crate::ui::button("notice-sentient-go").primary().label("Update…").on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Update;
                            cx.notify();
                        })))
                        .child(crate::ui::button("notice-sentient-notes").ghost().label("What's new").on_click(cx.listener(|this, _, _, cx| this.whats_new(update::Product::Sentient, cx))))
                        .child(crate::ui::button("notice-sentient-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                            let v = v.clone();
                            this.updates.update(cx, |s, cx| s.dismiss_sentient(v, cx));
                        }))),
                );
            }
            (None, None) => {}
        }
        if let Some(o) = gw {
            let v = o.version.clone();
            col = col.child(
                bar("notice-gateway", crate::ui::Tone::Neutral, format!("Sentient Gateway {} is available{}.", o.version, dated(&o.date)))
                    .child(crate::ui::button("notice-gateway-go").primary().label("Update…").on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Gateway;
                        cx.notify();
                    })))
                    .child(crate::ui::button("notice-gateway-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                        let v = v.clone();
                        this.updates.update(cx, |s, cx| s.dismiss_gateway(v, cx));
                    }))),
            );
        }
        Some(col.into_any_element())
    }

    /// The record changed (a migration): every screen starts again from it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let was_installed = self.record.is_some();
        self.record = InstallationRecord::load().inspect(InstallationRecord::use_pg_tools);
        if !was_installed {
            // Adopted: every screen was showing "not installed" (Migration
            // stays while it shows the job that just ran; Back reloads it).
            if !self.migration.as_ref().is_some_and(|m| m.read(cx).showing_job()) {
                self.migration = None;
            }
            self.settings = None;
        }
        self.status = None;
        self.logs = None;
        self.update = None;
        self.backup = None;
        self.repair = None;
        // Migration and Settings reload their own record and keep showing
        // the result of what just ran.
        cx.notify();
    }

    /// A job ended on `instance` (None: this computer): what shows its state
    /// is read again (Status, Update, Migration, the instance cards, the open
    /// server's pages); a page showing that job's result keeps it.
    fn after_job(&mut self, instance: Option<String>, cx: &mut Context<Self>) {
        if let Some(page) = &self.instances {
            let id = instance.clone();
            page.update(cx, |p, cx| p.after_job(id.as_deref(), cx));
        }
        match instance {
            None => {
                self.record = InstallationRecord::load().inspect(InstallationRecord::use_pg_tools);
                self.status = None;
                for page in self.update.clone() {
                    page.update(cx, |p, cx| p.refresh_record(cx));
                }
                for page in self.migration.clone() {
                    page.update(cx, |p, cx| p.refresh_record(cx));
                }
            }
            Some(id) => {
                // A kept page of an instance not open now reads again too.
                if let Some(page) = self.remote_pages.get(&id).cloned() {
                    page.update(cx, |p, cx| p.after_job(cx));
                }
            }
        }
        cx.notify();
    }

    /// Like `reload`, but the Repair screen keeps showing what it just did.
    fn reload_keeping_repair(&mut self, cx: &mut Context<Self>) {
        let repair = self.repair.take();
        self.reload(cx);
        self.migration = None;
        self.settings = None;
        self.repair = repair;
    }

    /// The theme picker in the bar, left of the bell: its icon shows the
    /// choice (sun, moon, half circle for Follow system); a popover below it
    /// with the three.
    fn theme_button(&self, tint: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = theme::is_dark(cx);
        let theme = cx.theme().clone();
        let choice = theme::Choice::saved();
        let open = self.theme_open;
        let icon_of = shell::theme_icon;
        let button = div()
            .id("theme")
            .relative()
            .child(crate::ui::layout_probe("bar-ctrl"))
            .size(px(theme::BAR_CTRL_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .text_color(theme.foreground)
            .rounded(px(theme::BAR_CTRL_RADIUS))
            .when(open, |b| b.bg(tint.opacity(theme::bar_ctrl_hover(dark))))
            .when(!open, |b| b.hover(move |b| b.bg(tint.opacity(theme::bar_ctrl_hover(dark)))))
            .child(icon_of(choice).size(px(19.)))
            .when(!open, |b| b.tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Theme").build(window, cx)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.theme_open = !this.theme_open;
                cx.notify();
            }));
        let popover = open.then(|| {
            let this = cx.entity().downgrade();
            let rows = theme::Choice::ALL
                .into_iter()
                .map(|c| {
                    let this = this.clone();
                    ui::MenuRow::Item {
                        id: SharedString::from(format!("theme-{}", c.label())),
                        icon: Some(icon_of(c).size(px(18.)).into_any_element()),
                        label: c.label().into(),
                        secondary: None,
                        on: c == choice,
                        click: std::rc::Rc::new(move |window, cx| {
                            theme::choose(c, Some(window), cx);
                            this.update(cx, |m, cx| {
                                m.theme_open = false;
                                cx.notify();
                            })
                            .ok();
                        }),
                    }
                })
                .collect();
            let close = cx.entity().downgrade();
            deferred(
                div().absolute().top(px(theme::BAR_CTRL_H + 10.)).right(px(-8.)).child(ui::popover_menu(
                    &theme,
                    "theme-popover",
                    200.,
                    rows,
                    true,
                    px(200. - 26.),
                    move |_, cx| {
                        close.update(cx, |m, cx| {
                            m.theme_open = false;
                            cx.notify();
                        })
                        .ok();
                    },
                )),
            )
            .with_priority(1)
        });
        div().relative().child(button).children(popover)
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let record = self.record.clone();
        // "Move data to another disk", in Platform settings' place while open.
        if self.page == Page::Settings {
            if let Some(flow) = movedata::shown(self.open_remote.as_ref(), cx) {
                return flow.into_any_element();
            }
        }
        // Statistics: the same view for this computer and a server.
        // Files, Statistics and Licence: one page per instance, kept for the
        // run; one kept from before reads again the first time it shows
        // after a switch.
        let again = self.fresh.insert(self.page);
        if self.page == Page::Files {
            let key = self.open_remote.clone();
            let page = match self.files.get(&key).cloned() {
                Some(p) => {
                    if again {
                        p.update(cx, |p, cx| p.reshow(cx));
                    }
                    p
                }
                None => {
                    let inst = key.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned();
                    let p = cx.new(|cx| files::FilesPage::new(inst, cx));
                    self.files.insert(key, p.clone());
                    p
                }
            };
            return page.into_any_element();
        }
        if self.page == Page::Statistics {
            let key = self.open_remote.clone();
            let page = match self.stats.get(&key).cloned() {
                Some(p) => {
                    if again {
                        p.update(cx, |p, cx| p.reshow(cx));
                    }
                    p
                }
                None => {
                    let src = match key.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned() {
                        Some(inst) => stats::Source::Remote(inst),
                        None => stats::Source::Local(record.clone()),
                    };
                    let p = cx.new(|cx| stats::StatsPage::new(src, cx));
                    self.stats.insert(key, p.clone());
                    p
                }
            };
            return page.into_any_element();
        }
        // Licence: the same view for this computer and a server.
        if self.page == Page::Licence {
            let key = self.open_remote.clone();
            let page = match self.licence.get(&key).cloned() {
                Some(p) => {
                    if again {
                        p.update(cx, |p, cx| p.reshow(cx));
                    }
                    p
                }
                None => {
                    let (target, name) = match key.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned() {
                        Some(inst) => {
                            let n = inst.name.clone();
                            (crate::licence::Target::Remote(inst), n)
                        }
                        None => (crate::licence::Target::Local(record.clone()), "this computer".to_string()),
                    };
                    let page = cx.new(|cx| licence::LicencePage::new(target, name, window, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &licence::GoSettings, cx| {
                        this.page = Page::Settings;
                        cx.notify();
                    }));
                    self.licence.insert(key, page.clone());
                    page
                }
            };
            return page.into_any_element();
        }
        if let Some(r) = self.open_remote.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned() {
            let kind = match self.page {
                Page::Status => Some(remote_pages::Kind::Status),
                Page::Logs => Some(remote_pages::Kind::Logs),
                Page::Update => Some(remote_pages::Kind::Update),
                Page::Backup => Some(remote_pages::Kind::Backup),
                Page::Settings => Some(remote_pages::Kind::Settings),
                Page::Migration => Some(remote_pages::Kind::Migration),
                Page::Repair => Some(remote_pages::Kind::Repair),
                Page::Gateway => Some(remote_pages::Kind::Gateway),
                _ => None,
            };
            if let Some(kind) = kind {
                let page = match self.remote_pages.get(&r.id).cloned() {
                    // Kept from an earlier opening: what it read shows at
                    // once, read again behind it.
                    Some(p) => {
                        p.update(cx, |p, cx| if again { p.reshow(kind, cx) } else { p.show(kind, cx) });
                        p
                    }
                    None => {
                        let updates = self.updates.clone();
                        let p = cx.new(|cx| remote_pages::RemotePages::new(r.clone(), kind, updates, window, cx));
                        self.remote_pages.insert(r.id.clone(), p.clone());
                        p
                    }
                };
                return page.into_any_element();
            }
            if !matches!(self.page, Page::Instances | Page::About) {
                let theme = cx.theme().clone();
                return ui::page(&theme, self.page.title(), "")
                    .child(
                        ui::panel(&theme)
                            .child(div().child(format!("{} is open. This page works on this computer only for now; Status, Logs, Update and Backup and restore work on {}.", r.name, r.name)))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(crate::ui::button("to-instances").primary().label(format!("Go to {}", r.name)).on_click(cx.listener(|this, _, _, cx| {
                                        this.page = Page::Instances;
                                        cx.notify();
                                    })))
,
                            ),
                    )
                    .into_any_element();
            }
        }
        match self.page {
            Page::Instances => {
                if self.instances.is_none() {
                    let open = self.open_remote.clone();
                    let page = cx.new(|cx| {
                        let mut p = instances::InstancesPage::new(record, cx);
                        // An instance opened before this page was first shown.
                        p.open = open;
                        p
                    });
                    self._subscriptions.push(cx.subscribe(&page, |this, _, e: &instances::OpenInstance, cx| {
                        this.open_instance(e.0.clone(), cx);
                    }));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &instances::ListChanged, cx| {
                        let before = std::mem::replace(&mut this.remotes, crate::remote::load());
                        this.prune_kept(&before);
                        if this.open_remote.as_ref().is_some_and(|id| !this.remotes.iter().any(|r| &r.id == id)) {
                            this.open_remote = None;
                        }
                        cx.notify();
                    }));
                    self.instances = Some(page);
                }
                self.instances.clone().unwrap().into_any_element()
            }
            Page::Status => {
                if self.status.is_none() {
                    let updates = self.updates.clone();
                    let page = cx.new(|cx| status::StatusPage::new(record, updates, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &status::OpenGateway, cx| {
                        this.page = Page::Gateway;
                        cx.notify();
                    }));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &status::OpenUpdate, cx| {
                        this.page = Page::Update;
                        cx.notify();
                    }));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, go: &status::Go, cx| {
                        this.page = match go {
                            status::Go::Backup => Page::Backup,
                            status::Go::Settings => Page::Settings,
                            status::Go::Migration => Page::Migration,
                        };
                        cx.notify();
                    }));
                    self.status = Some(page);
                }
                self.status.clone().unwrap().into_any_element()
            }
            Page::Logs => self
                .logs
                .get_or_insert_with(|| cx.new(|cx| logs::LogsPage::new(record, window, cx)))
                .clone()
                .into_any_element(),
            Page::Update => self.update_page(cx).into_any_element(),
            Page::Gateway => {
                let updates = self.updates.clone();
                self.gateway
                    .get_or_insert_with(|| cx.new(|cx| gateway::GatewayPage::new(record, updates, window, cx)))
                    .clone()
                    .into_any_element()
            }
            Page::Backup => {
                if self.backup.is_none() {
                    let page = cx.new(|cx| backup::BackupPage::new(record, window, cx));
                    self.backup = Some(page);
                }
                self.backup.clone().unwrap().into_any_element()
            }
            Page::Migration => {
                if self.migration.is_none() {
                    let page = cx.new(|cx| migration::MigrationPage::new(record, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self.migration = Some(page);
                }
                self.migration.clone().unwrap().into_any_element()
            }
            Page::Settings => {
                if self.settings.is_none() {
                    let page = cx.new(|cx| settings::SettingsPage::new(record, window, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self.settings = Some(page);
                }
                self.settings.clone().unwrap().into_any_element()
            }
            // Handled at the top of content().
            Page::Statistics | Page::Licence | Page::Files => div().into_any_element(),
            Page::About => {
                if self.about.is_none() {
                    let updates = self.updates.clone();
                    let page = cx.new(|cx| about::AboutPage::new(updates, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &about::ShowWhatsNew, cx| this.whats_new(update::Product::Manager, cx)));
                    self.about = Some(page);
                }
                self.about.clone().unwrap().into_any_element()
            }
            Page::Repair => {
                if self.repair.is_none() {
                    let page = cx.new(|cx| repair::RepairPage::new(record, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload_keeping_repair(cx)));
                    self.repair = Some(page);
                }
                self.repair.clone().unwrap().into_any_element()
            }
        }
    }
}

impl Manager {
    /// A tray or taskbar action (crate::background::Command::Action).
    pub fn act(&mut self, action: &str, cx: &mut Context<Self>) {
        match action {
            "statistics" => self.page = Page::Statistics,
            "notifications" => self.bell_open = true,
            "check-updates" => {
                self.page = Page::About;
                self.updates.update(cx, |u, cx| u.check(cx));
            }
            "backup" => {
                self.open_instance(None, cx);
                self.page = Page::Backup;
                self.pending = Some("backup".into());
            }
            a => {
                if let Some(id) = a.strip_prefix("notification:").and_then(|n| n.parse::<u64>().ok()) {
                    self.bell_open = true;
                    self.strip.update(cx, |s, cx| s.open_event(id, cx));
                } else if let Some(id) = a.strip_prefix("instance:") {
                    self.open_instance((!id.is_empty()).then(|| id.to_string()), cx);
                    self.page = Page::Status;
                } else if let Some(verb) = a.strip_prefix("control:") {
                    self.open_instance(None, cx);
                    self.page = Page::Status;
                    self.pending = Some(format!("control:{verb}"));
                }
            }
        }
        cx.notify();
    }

    /// Run the pending action on its page (it exists after a render).
    fn run_pending(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.pending.clone() else { return };
        if p == "backup" {
            if let Some(page) = self.backup.clone() {
                self.pending = None;
                cx.defer(move |cx| page.update(cx, |b, cx| b.run_preset_backup(cx)));
            }
        } else if let Some(verb) = p.strip_prefix("control:") {
            if let Some(page) = self.status.clone() {
                self.pending = None;
                let verb: &'static str = match verb {
                    "start" => "start",
                    "stop" => "stop",
                    _ => "restart",
                };
                cx.defer(move |cx| page.update(cx, |c, cx| c.run(verb, cx)));
            }
        }
    }
}

impl Render for Manager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Windows 10: the app's caption and rounded corners (crate::win_frame).
        let root = self.render_root(window, cx).into_any_element();
        crate::win_frame::frame(crate::mode::AppMode::Manager.window_title(), root, window, cx)
    }
}

impl Manager {
    fn render_root(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The layout follows the window: an icon rail below RAIL_BELOW px,
        // fewer monitors in the top bar as it narrows.
        let width = f32::from(window.viewport_size().width);
        crate::ui::set_narrow(width < 760.);
        crate::ui::set_side_by_side(width >= crate::ui::SIDE_BY_SIDE_W);
        self.window_h.set(f32::from(window.viewport_size().height));
        stats::set_open(self.page == Page::Statistics);
        // A service row's "Logs" asked for the Logs page.
        if crate::logsource::take_go() {
            self.page = Page::Logs;
        }
        // Widened past the fold while the menu is open over the page: it
        // becomes the docked, unfolded menu.
        if self.menu_overlay && width >= shell::FOLD_BELOW {
            self.menu_overlay = false;
            shell::Appearance::set(|a| a.folded = false);
        }
        // The Restore flow asked for another page (its blocks' buttons).
        if let Some(id) = restore::take_go() {
            if let Some(p) = Page::ALL.iter().copied().chain([Page::Instances, Page::About]).find(|p| p.id() == id) {
                self.page = p;
            }
        }
        // "Move data to another disk" was opened (Platform settings shows
        // it), or asked for another page.
        let gen = movedata::generation(cx);
        if gen != self.move_gen {
            self.move_gen = gen;
            if gen != 0 {
                self.page = Page::Settings;
            }
        }
        if let Some(p) = movedata::take_go() {
            self.page = match p {
                "files" => Page::Files,
                _ => self.page,
            };
        }
        let menu_w = if Self::folded(width) { shell::MENU_FOLDED_W } else { shell::MENU_W };
        let top = self.top_bar(width - menu_w, window, cx);
        let nav = self.shell_menu(width, window, cx);
        let notices = self.notices(cx);
        // Frame times while animating (SPM_DEBUG_FRAMES=1, also in release
        // builds, for measuring on a real desktop): the interval since the
        // last frame, on stderr.
        if std::env::var_os("SPM_DEBUG_FRAMES").is_some() {
            thread_local!(static LAST: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) });
            let now = std::time::Instant::now();
            let dt = LAST.with(|l| l.replace(Some(now))).map(|p| now.duration_since(p).as_secs_f64() * 1000.).unwrap_or(0.);
            eprintln!("FRAME dt={dt:.2}");
        }
        // The window as it is now, for the next start (session.json).
        if session::note(self.open_remote.clone(), self.page.id(), window) {
            let t = cx.spawn(async |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            });
            self._tasks.push(t);
        }
        // Layout tests: the probes' lines after this mark are this frame's.
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_LAYOUT").is_some() {
            eprintln!("LAYOUT-FRAME {:.0} {:.0}", f32::from(window.viewport_size().width), f32::from(window.viewport_size().height));
        }
        let content = self.content(window, cx);
        // Switch timing: each frame after a switch until the page shows
        // its data (render done, and the next frame's start ≈ presented).
        if let Some((t0, n)) = SWITCH_AT.with(|s| s.get()) {
            let data = match &self.open_remote {
                Some(id) => self.remote_pages.get(id).is_some_and(|p| p.read(cx).has_data()),
                None => self.page != Page::Status || self.status.as_ref().is_some_and(|p| p.read(cx).has_data()),
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.;
            eprintln!("SWITCH frame={} render_ms={ms:.2} data={data}", n + 1);
            if data {
                SWITCH_AT.with(|s| s.set(None));
                window.on_next_frame(move |_, _| eprintln!("SWITCH next_frame_ms={:.2}", t0.elapsed().as_secs_f64() * 1000.));
            } else if n < 400 {
                SWITCH_AT.with(|s| s.set(Some((t0, n + 1))));
            } else {
                SWITCH_AT.with(|s| s.set(None));
            }
        }
        self.run_pending(cx);
        let layer = crate::modal::layer(window, cx, menu_w, shell::BAR_H);
        if layer.is_some() && window.focused(cx).is_none() {
            window.focus(&self.modal_focus, cx);
        }
        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.modal_focus)
            // Escape closes the open dialog, from anywhere in the window.
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key != "escape" {
                    return;
                }
                if cx.try_global::<crate::modal::ModalSlot>().is_some_and(|s| s.0.is_some()) {
                    crate::modal::dismiss(window, cx);
                } else if this.menu_overlay {
                    // Escape also closes the menu open over the page.
                    this.menu_overlay = false;
                    cx.notify();
                }
            }))
            .bg(if shell::see_through() { cx.theme().background.opacity(0.) } else { cx.theme().background })
            .text_color(cx.theme().foreground)
            .child(
                // The shell: the menu the window's full height; beside it the
                // bar over the page, then the page.
                // Reversed, so the menu paints after the page: open over it
                // in a narrow window, it stays on top.
                h_flex()
                    .size_full()
                    .flex_row_reverse()
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .relative()
                            // The page's own grey behind it, so no frame shows the
                            // window's colour.
                            .bg(cx.theme().muted)
                            .child(top)
                            .children(notices)
                            .child({
                                // A new page fades in and rises 6 px.
                                let page = h_flex().flex_1().min_h_0().w_full().relative().child(content);
                                if ui::reduce_motion() {
                                    page.into_any_element()
                                } else {
                                    page.with_animation(
                                        SharedString::from(format!("page-{}", self.page.id())),
                                        Animation::new(std::time::Duration::from_millis(160)).with_easing(ui::ease_out),
                                        |el, d| el.opacity(d).top(px(6. * (1. - d))),
                                    )
                                    .into_any_element()
                                }
                            })
                            // The menu open over the page: a faint scrim on
                            // the page; a click on it closes the menu.
                            .when(self.overlay_t.get() > 0.001, |c| {
                                let dark = cx.theme().mode.is_dark();
                                let t = self.overlay_t.get();
                                c.child(
                                    div()
                                        .id("menu-scrim")
                                        .absolute()
                                        .inset_0()
                                        .bg(gpui::black().opacity(t * if dark { 0.32 } else { 0.14 }))
                                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                                            this.menu_overlay = false;
                                            cx.stop_propagation();
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(nav),
            )
            .children(layer)
    }
}

/// Below this window width the menu becomes an icon rail.
pub(crate) const RAIL_BELOW: f32 = 960.;

/// The menu's width (the mockup's 188 px at its 12.5 px text, for 14 px).
pub(crate) const NAV_W: f32 = 228.;

/// ", released 2026-10-24", or nothing.
fn dated(date: &str) -> String {
    if date.is_empty() { String::new() } else { format!(", released {date}") }
}

/// A condition's key: its instance and title (a new version is news).
fn cond_key(inst: &Option<String>, title: &str) -> String {
    format!("{}|{title}", inst.as_deref().unwrap_or(""))
}

