// Copyright © 2016-2026 The SENTIENT Authors

//! Manager mode: the app the operator opens from the Start menu after
//! installation. A left navigation of the management screens; each screen is
//! its own view, created the first time it is opened.

mod about;
pub(crate) mod backup;
pub(crate) mod control;
mod gateway;
mod instances;
pub(crate) mod logs;
pub(crate) mod migration;
pub(crate) mod remote_pages;
mod repair;
mod takeover;
pub(crate) mod settings;
pub(crate) mod setting_field;
pub(crate) mod status;
pub(crate) mod stats;
pub(crate) mod strip;
pub(crate) mod update;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, h_flex, v_flex,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Status,
    Control,
    Logs,
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
}

impl Page {
    /// The menu, in its order: Platform settings last.
    const ALL: [Page; 10] = [
        Page::Status,
        Page::Statistics,
        Page::Control,
        Page::Logs,
        Page::Update,
        Page::Gateway,
        Page::Backup,
        Page::Migration,
        Page::Repair,
        Page::Settings,
    ];

    fn id(self) -> &'static str {
        match self {
            Page::Status => "status",
            Page::Control => "control",
            Page::Logs => "logs",
            Page::Update => "update",
            Page::Gateway => "gateway",
            Page::Backup => "backup",
            Page::Migration => "migration",
            Page::Settings => "settings",
            Page::Repair => "repair",
            Page::Instances => "instances",
            Page::About => "about",
            Page::Statistics => "statistics",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Status => "Status",
            Page::Control => "Control",
            Page::Logs => "Logs",
            Page::Update => "Update",
            Page::Gateway => "Gateway",
            Page::Backup => "Backup and restore",
            Page::Migration => "Migration",
            Page::Settings => "Platform settings",
            Page::Repair => "Repair and uninstall",
            Page::Instances => "Instances",
            Page::About => "About and preferences",
            Page::Statistics => "Statistics",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Page::Status => Icon::new(Lucide::Activity),
            Page::Control => Icon::new(Lucide::Power),
            Page::Logs => Icon::new(IconName::SquareTerminal),
            Page::Update => Icon::new(IconName::RefreshCw),
            Page::Gateway => Icon::new(Lucide::Network),
            Page::Backup => Icon::new(Lucide::Archive),
            Page::Migration => Icon::new(Lucide::ArrowLeftRight),
            Page::Settings => Icon::new(IconName::Settings),
            Page::Repair => Icon::new(Lucide::Wrench),
            Page::Instances => Icon::new(Lucide::Server),
            Page::About => Icon::new(IconName::Info),
            Page::Statistics => Icon::new(Lucide::ChartLine),
        }
    }
}

pub struct Manager {
    page: Page,
    record: Option<InstallationRecord>,
    status: Option<Entity<status::StatusPage>>,
    control: Option<Entity<control::ControlPage>>,
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
    /// That instance's processor, memory and disk (None until read, or
    /// when it does not answer).
    remote_monitor: Option<Monitor>,
    /// The pages of the open remote instance.
    remote_pages: Option<Entity<remote_pages::RemotePages>>,
    /// Statistics of the open instance (None: this computer).
    stats: Option<(Option<String>, Entity<stats::StatsPage>)>,
    /// Takes the keyboard while a dialog is open (Escape closes it).
    modal_focus: FocusHandle,
    /// The instance switcher's menu is open.
    switcher_open: bool,
    /// The notifications panel is open.
    bell_open: bool,
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
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

/// The bell's events as saved (the tray's menu reads them without a window).
pub fn saved_events() -> Vec<strip::Event> {
    strip::load_events()
}

impl Manager {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // `--page migration` (from Setup's Change database).
        let args: Vec<String> = std::env::args().collect();
        let page = args
            .windows(2)
            .find(|w| w[0] == "--page")
            .and_then(|w| Page::ALL.into_iter().chain([Page::Instances, Page::About]).find(|p| p.id() == w[1]))
            .unwrap_or(Page::Status);
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
        let subscriptions = vec![cx.observe(&updates, |_, _, cx| cx.notify()), cx.observe(&strip, |_, _, cx| cx.notify())];
        let updated_from = args.windows(2).find(|w| w[0] == "--updated-from").map(|w| w[1].clone());
        let found_notice = record.is_some() && crate::updates::take_found_installation();
        let mut manager = Self {
            page,
            record,
            status: None,
            control: None,
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
            remote_pages: None,
            stats: None,
            modal_focus: cx.focus_handle(),
            strip: strip.clone(),
            switcher_open: false,
            bell_open: false,
            window_h: std::cell::Cell::new(720.),
            _debug_job: None,
            theme_open: cfg!(debug_assertions) && std::env::var_os("SPM_THEME_OPEN").is_some(),
            monitor: None,
            updates,
            updated_from,
            pending: None,
            found_notice,
            _subscriptions: subscriptions,
            _tasks: Vec::new(),
        };
        manager.start_monitor(cx);
        // Screenshots: `--instance <id>` opens a remote instance on `--page`.
        #[cfg(debug_assertions)]
        if let Some(id) = args.windows(2).find(|w| w[0] == "--instance").map(|w| w[1].clone()) {
            manager.open_instance(Some(id), cx);
            manager.page = page;
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
                crate::modal::open(crate::modal::Modal { title: "Switches".into(), body: view.into(), width: 360., on_close: None }, cx);
            }
            // Screens: the takeover dialog with a canned plan.
            if let Ok(kind) = std::env::var("SPM_DEBUG_TAKEOVER") {
                use sentient_installer_core::distro::adopt::TakeOverPlan;
                let plan = move |compose: Option<String>| -> Result<TakeOverPlan, String> {
                    let mut p = TakeOverPlan {
                        project: "sentient-prod".into(),
                        files: vec![("/home/ops/deploy/sentient/docker-compose.yml".into(), kind != "missing"), ("/home/ops/deploy/sentient/docker-compose.override.yml".into(), kind != "missing")],
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
                        }
                        "failed" => return Err("ssh: connect to host 10.0.4.12 port 22: Connection timed out".into()),
                        "loading" => std::thread::sleep(std::time::Duration::from_secs(60)),
                        _ => {}
                    }
                    Ok(p)
                };
                cx.defer(move |cx| takeover::open(std::env::var_os("SPM_DEBUG_TAKEOVER_LOCAL").is_some(), std::sync::Arc::new(plan), std::sync::Arc::new(|pw: String| if pw == "right" { Ok(()) } else { Err("PostgreSQL refused this password for the user sentient.".into()) }), std::rc::Rc::new(|_, _, _: &mut App| {}), None, cx));
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
                crate::modal::open(crate::modal::Modal { title: "Platform Manager log".into(), body: view.into(), width: 1000., on_close: None }, cx);
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
        let dir = self
            .record
            .as_ref()
            .map(|r| if r.pg_dir.as_os_str().is_empty() { r.install_dir.clone() } else { r.pg_dir.clone() })
            .unwrap_or_else(|| std::path::PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }));
        let task = cx.spawn(async move |this, cx| loop {
            let d = dir.clone();
            let m = cx.background_executor().spawn(async move { crate::sysmon::sample(&d) }).await;
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
        // SENTIENT stopped or a server not answering (crate::bg_app's checks):
        // they stay while it lasts.
        for p in crate::bg_app::problems(cx).into_iter().rev() {
            out.insert(0, (p.title().to_string(), p.detail(), Page::Status, p.instance.clone(), p.name.clone(), p.color.clone()));
        }
        if let (Some(id), Some(m)) = (&self.open_remote, self.remote_monitor.filter(|m| m.disk_low())) {
            if let Some(r) = self.remotes.iter().find(|r| &r.id == id) {
                out.push(("Disk almost full".into(), format!("{} free", crate::dbstatus::bytes_label(m.disk_free)), Page::Status, Some(r.id.clone()), r.name.clone(), r.color.clone()));
            }
        }
        out
    }

    /// The bell's panel: Notifications, Mark all as read / Clear all, the
    /// events and conditions with their instance, or an empty state.
    fn notifications_panel(&self, conditions: Vec<(String, String, Page, Option<String>, String, String)>, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let events = self.strip.read(cx).events.clone();
        let (unread, _) = self.strip.read(cx).unread();
        let w = (width - 24.).min(360.);
        let now = chrono::Utc::now().timestamp();
        let ago = |at: i64| {
            let m = (now - at).max(0) / 60;
            if m < 1 { "just now".to_string() } else if m < 60 { format!("{m} min ago") } else if m < 24 * 60 { format!("{} h ago", m / 60) } else { format!("{} d ago", m / (24 * 60)) }
        };
        let strip = self.strip.clone();
        let this = cx.entity().downgrade();
        let row = |key: SharedString, icon: IconName, icon_color: Hsla, title: String, detail: String, meta: String, color: &str, unread: bool, clear: Option<AnyElement>| {
            let c = crate::remote::parse_hex(color).unwrap_or(0x187074);
            h_flex()
                .id(key.clone())
                .group(key)
                .relative()
                .gap_2()
                .items_start()
                .pl(px(10.))
                .pr_2()
                .py(px(7.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|r| r.bg(theme.list_hover))
                // The instance's colour along the left.
                .child(div().absolute().left(px(2.)).top(px(6.)).bottom(px(6.)).w(px(3.)).rounded(px(2.)).bg(rgb(c)))
                .child(Icon::new(icon).size_4().text_color(icon_color).mt(px(2.)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_weight(if unread { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                        .child(div().text_size(px(12.5)).text_color(theme.muted_foreground).overflow_hidden().whitespace_nowrap().text_ellipsis().child(detail))
                        .child(div().text_size(px(11.5)).text_color(theme.muted_foreground).child(meta)),
                )
                // Right: the unread dot, then the row's own clear (×), always
                // shown so one line can be cleared without hunting for it.
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(px(6.))
                        .when(unread, |c| c.child(div().size(px(7.)).rounded_full().bg(theme.danger)))
                        .children(clear),
                )
        };
        let mut list = v_flex().gap_px();
        for (i, (title, detail, page, inst, name, color)) in conditions.into_iter().enumerate() {
            let t = this.clone();
            let icon = if title.contains("available") || title.contains("waiting") { (IconName::ArrowUp, theme.primary) } else { (IconName::TriangleAlert, rgb(0xd97706).into()) };
            list = list.child(
                row(SharedString::from(format!("cond-{i}")), icon.0, icon.1, title, detail, format!("{name}"), &color, false, None).on_click(move |_, _, cx| {
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
            );
        }
        for e in events.iter() {
            let (id, inst) = (e.id, e.instance.clone());
            let (s1, s2, t) = (strip.clone(), strip.clone(), this.clone());
            let icon = if e.failed { (IconName::CircleX, theme.danger) } else { (IconName::CircleCheck, theme.primary) };
            let clear = div()
                .id(SharedString::from(format!("clear-{id}")))
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .hover(|d| d.bg(theme.danger.opacity(0.12)))
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Clear").build(w, cx))
                .child(Icon::new(IconName::Close).size(px(13.)).text_color(theme.muted_foreground))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    s1.update(cx, |s, cx| s.clear(Some(id), cx));
                })
                .into_any_element();
            list = list.child(
                row(SharedString::from(format!("ev-{id}")), icon.0, icon.1, e.title.clone(), e.detail.clone(), format!("{} · {}", e.instance_name, ago(e.at)), &e.color, !e.read, Some(clear)).on_click(move |_, _, cx| {
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
            );
        }
        let empty = list_is_empty(&events, self.conditions(cx).len());
        let (s3, s4, close) = (strip.clone(), strip.clone(), this.clone());
        let header = h_flex()
            .px_2()
            .pt_1()
            .pb_2()
            .gap_2()
            .items_center()
            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("Notifications"))
            .when(unread > 0, |h| h.child(crate::ui::button("bell-read").ghost().xsmall().label("Mark all as read").on_click(move |_, _, cx| s3.update(cx, |s, cx| s.mark_all_read(cx)))))
            .when(!events.is_empty(), |h| h.child(crate::ui::button("bell-clear").ghost().xsmall().label("Clear all").on_click(move |_, _, cx| s4.update(cx, |s, cx| s.clear(None, cx)))));
        let max_h = (f32::from(self.window_h.get()) * 0.6).max(260.);
        deferred(
            div().absolute().top(px(30. + 9.)).right(px(-6.)).child(
                div()
                    .relative()
                    .w(px(w))
                    .child(
                        v_flex()
                            .id("bell-panel")
                            .occlude()
                            .p_2()
                            .min_h(px(240.))
                            .max_h(px(max_h))
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
                                div().id("bell-list").flex_1().min_h_0().overflow_y_scroll().child(list).into_any_element()
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
    fn top_bar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = theme::is_dark(cx);
        let menu_theme = cx.theme().clone();
        let theme_radius = menu_theme.radius;
        let remote = self.open_remote.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned();
        let (bg, fg): (Hsla, Hsla) = match &remote {
            Some(r) => {
                let c = crate::remote::parse_hex(&r.color).unwrap_or(0x187074);
                let bg: Hsla = rgb(c).into();
                (if dark { bg.blend(gpui_kit::black().opacity(0.25)) } else { bg }, rgb(crate::remote::text_on(&r.color)).into())
            }
            None => (if dark { rgb(0x155e61).into() } else { rgb(0x187074).into() }, rgb(0xffffff).into()),
        };
        let (name, address): (String, Option<String>) = match (&remote, &self.record) {
            (Some(r), _) => (r.name.clone(), Some(format!("{} over SSH", r.host))),
            (None, Some(r)) => ("This computer".into(), Some(format!("SENTIENT {}", r.sentient_version))),
            (None, None) => ("This computer".into(), None),
        };
        let instance = format!("{name} {}", address.clone().unwrap_or_default());
        // Light bar (dark text): a fainter inset.
        let light_bar = fg.l < 0.5;
        let inset: Hsla = black().opacity(if light_bar { 0.10 } else { 0.22 });
        // The open instance's state: green when its reading came back,
        // amber while it is being read.
        let status_green: Hsla = rgb(0x3ddc84).into();
        let state_dot: Hsla = if remote.is_some() && self.remote_monitor.is_none() { rgb(0xf3b744).into() } else { status_green };
        self.strip.read(cx).light_bar.set(light_bar);
        self.strip.read(cx).window_w.set(width);
        // The mockup's top bar: above 900 px the product name and the
        // monitors' bars show; below, only the switcher and the values.
        let wide = width > 900.;
        let warn_fill: Hsla = rgb(0xf59e0b).into();
        let warn_text: Hsla = rgb(0xfcd34d).into();
        let meter = |id: &'static str, label: &'static str, widest: &str, value: String, fraction: f32, high: bool| {
            h_flex()
                .id(id)
                .relative()
                .gap(px(6.))
                .items_center()
                .text_size(px(12.5))
                .child(div().text_color(fg.opacity(0.65)).child(label))
                .when(wide, |r| {
                    r.child(
                        div()
                            .w(px(42.))
                            .h(px(5.))
                            .rounded_full()
                            .bg(fg.opacity(0.2))
                            .child(div().h_full().rounded_full().w(relative(fraction.clamp(0.0, 1.0))).bg(if high { warn_fill } else { fg })),
                    )
                })
                // A fixed slot for the widest value: the bars and the bell
                // never move when a value changes.
                .child(ui::live_value_left(widest, 12.5, value).text_color(if high { warn_text } else { fg }))
                .child(crate::ui::layout_probe(id))
        };
        // What fits: a rough width per piece (about 7 px a character at
        // 12.5-14 px), kept in this order: CPU, memory, disk.
        let text_w = |s: &str| s.chars().count() as f32 * 6.9;
        // The progress chip's smallest form (spinner and time) always has its
        // room, job or not: the monitors are laid out the same either way.
        let mut room = width - 20. - (20. + 12.) - (text_w(&instance) + 60. + 12.) - if remote.is_some() { 64. } else { 0. } - 40. - 12. - strip::CHIP_MIN;

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
            let bar = if wide { 48. } else { 0. };
            let slot = |w: &str| f32::from(ui::slot_width(w, 12.5));
            let cpu = fits(16. + 6. + 26. + slot("100%") + bar);
            let mem = cpu && fits(16. + 6. + 52. + slot(&mem_widest) + bar);
            let disk = mem && fits(16. + 6. + 30. + slot(&disk_widest) + bar);
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
                    .gap(px(6.))
                    .items_center()
                    .text_size(px(12.5))
                    .child(div().text_color(fg.opacity(0.65)).child(label))
                    .when(wide, |r| r.child(div().w(px(42.)).h(px(5.)).rounded_full().bg(fg.opacity(0.2))))
                    .child(div().w(ui::slot_width(widest, 12.5)).flex().items_center().child(div().w(relative(0.75)).h(px(10.)).rounded(px(5.)).bg(fg.opacity(0.22))))
            };
            h_flex()
                .gap(px(16.))
                .px_2()
                .py_1()
                .when(show_cpu, |r| r.child(slot("CPU", "100%")))
                .when(show_mem, |r| r.child(slot("Memory", &mem_widest)))
                .when(show_disk, |r| r.child(slot("Disk", &disk_widest)))
        });
        let monitor = shown.map(|m| {
            let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
            h_flex()
                .id("monitor")
                .relative()
                .child(crate::ui::layout_probe("monitors"))
                .gap(px(16.))
                .px_2()
                .py_1()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(move |d| d.bg(inset))
                .when(show_cpu, |r| r.child(meter("m-cpu", "CPU", "100%", format!("{:.0}%", m.cpu), m.cpu / 100.0, m.cpu_high())))
                .when(show_mem, |r| r.child(meter(
                    "m-mem",
                    "Memory",
                    &mem_widest,
                    format!("{:.1} / {:.0} GB", gb(m.mem_used), gb(m.mem_total)),
                    if m.mem_total > 0 { m.mem_used as f32 / m.mem_total as f32 } else { 0.0 },
                    m.mem_high(),
                )))
                .when(show_disk, |r| r.child(meter(
                    "m-disk",
                    "Disk",
                    &disk_widest,
                    format!("{} free", crate::dbstatus::bytes_label(m.disk_free)),
                    if m.disk_total > 0 { 1.0 - m.disk_free as f32 / m.disk_total as f32 } else { 0.0 },
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
        let conditions = self.conditions(cx);
        let (unread, unread_failed) = self.strip.read(cx).unread();
        let count = unread + conditions.len();
        let bell_open = self.bell_open;
        let bell = div()
            .id("bell")
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme_radius)
            .cursor_pointer()
            .when(bell_open, |b| b.bg(black().opacity(if light_bar { 0.16 } else { 0.30 })))
            .hover(move |b| b.bg(black().opacity(if light_bar { 0.16 } else { 0.30 })))
            .child(Icon::new(IconName::Bell).text_color(fg).size_4())
            .on_click(cx.listener(|this, _, _, cx| {
                this.bell_open = !this.bell_open;
                cx.notify();
            }));
        let bell_panel = bell_open.then(|| self.notifications_panel(conditions, width, cx));
        let unseen = if unread_failed { 1 } else { 0 };
        let line = self.strip.read(cx).line(cx);
        h_flex()
            .id("top-bar")
            .relative()
            .children(line)
            .h(px(42.))
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .items_center()
            .bg(bg)
            .text_color(fg)
            // On a white tile: the icon's own teal would fade into a teal
            // (or any instance-coloured) bar.
            .child(
                div()
                    .size(px(28.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .bg(white().opacity(0.94))
                    .child(ui::product_logo(ui::Product::Manager, 22.)),
            )
            // No product name here: the window's own title carries it.
            .child({
                // The mockup's option B: a darker inset of the bar itself, no
                // border; hover one step darker (the text keeps its contrast).
                // Its menu is the app's popover, like the theme menu.
                let open = self.switcher_open;
                let hover_inset: Hsla = black().opacity(if light_bar { 0.16 } else { 0.30 });
                let chip = div()
                    .id("switcher")
                    .flex()
                    .items_center()
                    .h(px(30.))
                    .px(px(10.))
                    .rounded(theme_radius)
                    .cursor_pointer()
                    .text_color(fg)
                    .bg(if open { hover_inset } else { inset })
                    .hover(move |d| d.bg(hover_inset))
                    .child(
                        h_flex()
                            .gap(px(7.))
                            .items_center()
                            .text_size(px(13.5))
                            // The state dot keeps its colour, ringed in the text colour.
                            .child(div().size(px(11.)).rounded_full().border(px(1.5)).border_color(fg.opacity(0.9)).bg(state_dot))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                            .when_some(address, |r, a| r.child(div().text_color(fg.opacity(0.85)).child(a)))
                            .child(Icon::new(IconName::ChevronDown).size_3().text_color(fg.opacity(0.85)))
                            .when(remote.is_some(), |r| {
                                r.child(div().px(px(6.)).py(px(1.)).rounded(px(4.)).bg(fg).text_color(bg).text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).child("REMOTE"))
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
                        icon: Some(Icon::new(Lucide::Monitor).size_4().into_any_element()),
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
                        icon: Some(Icon::new(Lucide::Settings).size_4().into_any_element()),
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
                    deferred(div().absolute().top(px(30. + 9.)).left_0().child(ui::popover_menu(&menu_theme, "switcher-popover", 320., rows, true, px(24.), move |_, cx| {
                        close.update(cx, |m, cx| {
                            m.switcher_open = false;
                            cx.notify();
                        })
                        .ok();
                    })))
                    .with_priority(1)
                });
                div().relative().child(chip).children(popover)
            })
            // A running job: its chip in the middle of the bar.
            .child(h_flex().flex_1().min_w_0().overflow_hidden().justify_center().child(self.strip.clone()))
            .children(monitor)
            .children(monitor_loading)
            .child(
                // The bell with an amber count, as in the mockup.
                div().relative().child(bell).children(bell_panel).when(count > 0, |b| {
                    b.child(
                        div()
                            .absolute()
                            .top(px(1.))
                            .right(px(1.))
                            .min_w(px(15.))
                            .h(px(15.))
                            .px(px(3.))
                            .rounded_full()
                            // Red while a failed job is unread, else amber.
                            .bg(if unseen > 0 { rgb(0xdc2626) } else { rgb(0xf59e0b) })
                            .text_color(if unseen > 0 { rgb(0xffffff) } else { rgb(0x1c1917) })
                            .text_size(px(10.))
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
        if id.is_some() {
            self.page = Page::Status;
        }
        self.open_remote = id.clone();
        self.remote_monitor = None;
        self.remote_pages = None;
        if let Some(inst) = id.as_ref().and_then(|id| self.remotes.iter().find(|r| &r.id == id)).cloned() {
            // Read it every few seconds while it stays open.
            let task = cx.spawn(async move |this, cx| loop {
                let i = inst.clone();
                let m = cx.background_executor().spawn(async move { crate::remote::monitor(&i).ok() }).await;
                let still_open = this
                    .update(cx, |page, cx| {
                        let open = page.open_remote.as_deref() == Some(inst.id.as_str());
                        if open {
                            page.remote_monitor = m;
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
            // Adopted: every screen was showing "not installed".
            self.migration = None;
            self.settings = None;
        }
        self.status = None;
        self.control = None;
        self.logs = None;
        self.update = None;
        self.backup = None;
        self.repair = None;
        // Migration and Settings reload their own record and keep showing
        // the result of what just ran.
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

    fn nav(&self, rail: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let items = Page::ALL.into_iter().map(|page| {
            let active = page == self.page;
            // The mockup's menu item: muted text, the open page tinted in
            // the primary colour and medium weight.
            h_flex()
                .id(page.id())
                .gap(px(9.))
                .when(rail, |r| r.justify_center().px_2())
                .when(!rail, |r| r.px(px(11.)))
                .py(px(7.))
                .rounded(px(7.))
                .cursor_pointer()
                .text_sm()
                .text_color(theme.muted_foreground)
                .when(active, |row| row.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground).font_weight(FontWeight::MEDIUM))
                .when(!active, |row| row.hover(|row| row.bg(theme.list_hover).text_color(theme.foreground)))
                .child(page.icon().size(px(17.)))
                .when(!rail, |r| r.child(page.title()))
                .when(rail, |r| r.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(page.title()).build(window, cx)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = page;
                    cx.notify();
                }))
        });
        v_flex()
            .when(rail, |n| n.w(px(64.)))
            .when(!rail, |n| n.w(px(NAV_W)))
            .h_full()
            .flex_shrink_0()
            .p_3()
            .gap_1()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                // The menu scrolls when the window is short; the bottom group stays.
                v_flex()
                    .id("nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(items)
                    .child(div().my_1().h(px(1.)).flex_shrink_0().bg(theme.sidebar_border))
                    .child({
                let active = self.page == Page::Instances;
                h_flex()
                    .id("instances")
                    .gap(px(9.))
                    .when(rail, |r| r.justify_center().px_2())
                    .when(!rail, |r| r.px(px(11.)))
                    .py(px(7.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .when(active, |row| row.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground).font_weight(FontWeight::MEDIUM))
                    .when(!active, |row| row.hover(|row| row.bg(theme.list_hover).text_color(theme.foreground)))
                    .child(Page::Instances.icon().size(px(17.)))
                    .when(!rail, |r| r.child(Page::Instances.title()))
                    .when(rail, |r| r.tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Instances").build(window, cx)))
                    .when(!self.remotes.is_empty() && !rail, |r| r.child(div().flex_1()).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child((self.remotes.len() + 1).to_string())))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Instances;
                        cx.notify();
                    }))
                    }),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .when(rail, |r| r.flex_col().px_0())
                    .when(!rail, |r| r.px_2())
                    .pt_2()
                    .gap_1()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .when(!rail, |r| r.child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(format!("v{}", crate::updates::manager_version())),
                    ))
                    .child({
                        // The same 30 px icon box as the theme icon; tinted
                        // while About is open.
                        let on = self.page == Page::About;
                        div()
                            .id("about")
                            .size(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius)
                            .cursor_pointer()
                            .text_color(if on { theme.primary } else { theme.muted_foreground })
                            .when(on, |b| b.bg(theme.primary.opacity(0.10)))
                            .when(!on, |b| b.hover(|b| b.bg(theme.list_hover)))
                            .child(Icon::new(IconName::Info).size_4())
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("About and preferences").build(window, cx))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.page = Page::About;
                                cx.notify();
                            }))
                    })
                    .child(self.theme_button(rail, cx)),
            )
    }

    /// The theme icon and its popover: above the icon, pointing at it, one
    /// row per choice with its icon, the current one tinted with a tick
    /// (the approved mockup). At rail width it opens to the right.
    fn theme_button(&self, rail: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let choice = theme::Choice::saved();
        let open = self.theme_open;
        let icon_of = |c: theme::Choice| match c {
            theme::Choice::Light => Icon::new(IconName::Sun),
            theme::Choice::Dark => Icon::new(IconName::Moon),
            theme::Choice::System => Icon::new(Lucide::Monitor),
        };
        let button = div()
            .id("theme")
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_color(if open { theme.primary } else { theme.muted_foreground })
            .when(open, |b| b.bg(theme.primary.opacity(0.10)))
            .when(!open, |b| b.hover(|b| b.bg(theme.list_hover)))
            .child(icon_of(choice).size_4())
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
                        icon: Some(icon_of(c).size_4().into_any_element()),
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
            let pointer_x = if rail { px(9.) } else { px(162. - 24.) };
            let close = cx.entity().downgrade();
            deferred(
                div().absolute().bottom(px(30. + 9.)).when(rail, |d| d.left(px(-2.))).when(!rail, |d| d.right(px(-6.))).child(ui::popover_menu(
                    &theme,
                    "theme-popover",
                    162.,
                    rows,
                    false,
                    pointer_x,
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
        // Statistics: the same view for this computer and a server.
        if self.page == Page::Statistics {
            let key = self.open_remote.clone();
            if self.stats.as_ref().is_none_or(|(k, _)| *k != key) {
                let src = match key.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned() {
                    Some(inst) => stats::Source::Remote(inst),
                    None => stats::Source::Local(record.clone()),
                };
                self.stats = Some((key, cx.new(|cx| stats::StatsPage::new(src, cx))));
            }
            return self.stats.as_ref().map(|(_, p)| p.clone().into_any_element()).unwrap_or_else(|| div().into_any_element());
        }
        if let Some(r) = self.open_remote.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned() {
            let kind = match self.page {
                Page::Status => Some(remote_pages::Kind::Status),
                Page::Control => Some(remote_pages::Kind::Control),
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
                let page = match &self.remote_pages {
                    Some(p) => {
                        p.update(cx, |p, cx| p.show(kind, cx));
                        p.clone()
                    }
                    None => {
                        let updates = self.updates.clone();
                        let p = cx.new(|cx| remote_pages::RemotePages::new(r.clone(), kind, updates, window, cx));
                        self.remote_pages = Some(p.clone());
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
                            .child(div().child(format!("{} is open. This page works on this computer only for now; Status, Control, Logs, Update and Backup and restore work on {}.", r.name, r.name)))
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
                        this.remotes = crate::remote::load();
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
            Page::Control => self
                .control
                .get_or_insert_with(|| cx.new(|cx| control::ControlPage::new(record, window, cx)))
                .clone()
                .into_any_element(),
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
                    self._subscriptions.push(cx.subscribe(&page, |this, _, go: &backup::Go, cx| {
                        this.page = match go {
                            backup::Go::Update => Page::Update,
                            backup::Go::Settings => Page::Settings,
                        };
                        cx.notify();
                    }));
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
            Page::Statistics => div().into_any_element(),
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
                    self.page = Page::Control;
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
            if let Some(page) = self.control.clone() {
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
        // The layout follows the window: an icon rail below RAIL_BELOW px,
        // fewer monitors in the top bar as it narrows.
        let width = f32::from(window.viewport_size().width);
        crate::ui::set_narrow(width < 760.);
        self.window_h.set(f32::from(window.viewport_size().height));
        stats::set_open(self.page == Page::Statistics);
        // A Control row's "Logs" asked for the Logs page.
        if crate::logsource::take_go() {
            self.page = Page::Logs;
        }
        let top = self.top_bar(width, cx);
        let nav = self.nav(width < RAIL_BELOW, cx);
        let notices = self.notices(cx);
        let content = self.content(window, cx);
        self.run_pending(cx);
        let rail = width < RAIL_BELOW;
        let layer = crate::modal::layer(window, cx, if rail { 64. } else { NAV_W }, 42.);
        if layer.is_some() && window.focused(cx).is_none() {
            window.focus(&self.modal_focus, cx);
        }
        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.modal_focus)
            // Escape closes the open dialog, from anywhere in the window.
            .on_key_down(|e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && cx.try_global::<crate::modal::ModalSlot>().is_some_and(|s| s.0.is_some()) {
                    crate::modal::dismiss(window, cx);
                }
            })
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(top)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(nav)
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .relative()
                            .children(notices)
                            .child(crate::ui::appear(h_flex().flex_1().min_h_0().w_full().child(content), SharedString::from(format!("page-{}", self.page.id())), 140)),
                    ),
            )
            .children(layer)
    }
}

/// Below this window width the menu becomes an icon rail.
pub(crate) const RAIL_BELOW: f32 = 960.;

/// The menu's width (the mockup's 188 px at its 12.5 px text, for 14 px).
pub(crate) const NAV_W: f32 = 210.;

/// ", released 2026-10-24", or nothing.
fn dated(date: &str) -> String {
    if date.is_empty() { String::new() } else { format!(", released {date}") }
}

fn list_is_empty(events: &[strip::Event], conditions: usize) -> bool {
    events.is_empty() && conditions == 0
}
