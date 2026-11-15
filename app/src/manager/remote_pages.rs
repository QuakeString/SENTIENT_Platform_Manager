// Copyright © 2016-2026 The SENTIENT Authors

//! The pages of a remote instance (crate::remote): Status, Control, Logs,
//! Update and Backup, read from the server's own Platform Manager over SSH
//! and worked through detached jobs there. One view per open instance; the
//! menu chooses which page it shows.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::dbstatus::{DbStatus, bytes_label, count_label, day_label};
use crate::jobs::{Job, JobState};
use crate::remote::{self, Instance, RemoteBackups, RemoteService, RemoteStatus};
use crate::ui::{self, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Status,
    Control,
    Logs,
    Update,
    Backup,
    Settings,
    Migration,
    Repair,
    Gateway,
}

/// What a page has read, or why it could not.
enum Read<T> {
    Loading,
    Got(T),
    Failed(String),
}

impl<T> Read<T> {
    fn got(&self) -> Option<&T> {
        match self {
            Read::Got(t) => Some(t),
            _ => None,
        }
    }
}

pub struct RemotePages {
    inst: Instance,
    pub kind: Kind,
    /// The server answers and has the Platform Manager.
    ready: Read<()>,
    status: Read<RemoteStatus>,
    services: Read<Vec<RemoteService>>,
    db: Read<DbStatus>,
    backups: Read<RemoteBackups>,
    settings: Read<std::collections::HashMap<String, String>>,
    /// One input per catalog setting, made once the values are read.
    inputs: Vec<(String, Entity<InputState>)>,
    gateway: Read<std::collections::HashMap<String, String>>,
    /// Migration's chosen target: stock, timescaledb or clickhouse.
    target: usize,
    logs: Read<String>,
    /// The newest SENTIENT on the update channel (None: none newer).
    offer: Read<Option<crate::updates::Offer>>,
    filter: Entity<InputState>,
    follow: bool,
    job: Entity<Job>,
    job_label: Option<SharedString>,
    /// The failed job's technical details are shown.
    show_details: bool,
    scroll: ScrollHandle,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl RemotePages {
    pub fn new(inst: Instance, kind: Kind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter lines, e.g. ERROR"));
        let subs = vec![
            cx.observe(&job, |this, job, cx| {
                if !job.read(cx).is_running() {
                    this.reload(cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(&filter, window, |_, _, e, _, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut p = Self {
            inst,
            kind,
            ready: Read::Loading,
            status: Read::Loading,
            services: Read::Loading,
            db: Read::Loading,
            backups: Read::Loading,
            settings: Read::Loading,
            inputs: Vec::new(),
            gateway: Read::Loading,
            target: 2,
            logs: Read::Loading,
            offer: Read::Loading,
            filter,
            follow: true,
            job,
            job_label: None,
            show_details: false,
            scroll: ScrollHandle::new(),
            _tasks: Vec::new(),
            _subscriptions: subs,
        };
        p.reload(cx);
        p.start_log_follow(cx);
        p
    }

    pub fn show(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if self.kind != kind {
            self.kind = kind;
            self.reload(cx);
        }
        cx.notify();
    }

    /// Read what the page shows (in the background).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let inst = self.inst.clone();
        self.fetch(cx, move || remote::ready(&inst), |p, r| p.ready = r);
        let inst = self.inst.clone();
        match self.kind {
            Kind::Status | Kind::Control => {
                self.fetch(cx, move || remote::status(&inst), |p, r| p.status = r);
                let inst = self.inst.clone();
                self.fetch(cx, move || remote::services(&inst), |p, r| p.services = r);
                if self.kind == Kind::Status {
                    let inst = self.inst.clone();
                    self.fetch(cx, move || remote::dbstatus(&inst), |p, r| p.db = r);
                }
            }
            Kind::Logs => self.fetch(cx, move || remote::logs(&inst, 500).map(|t| ui::strip_ansi(&t)), |p, r| p.logs = r),
            Kind::Update => {
                let i2 = inst.clone();
                self.fetch(cx, move || remote::status(&i2), |p, r| p.status = r);
                let native = inst.detected.as_ref().is_some_and(|d| d.install != "Docker");
                self.fetch(
                    cx,
                    move || {
                        let installed = remote::status(&inst)?.sentient_version;
                        let channel = crate::updates::Prefs::load().channel;
                        crate::updates::check(channel, Some(installed), native).sentient
                    },
                    |p, r| p.offer = r,
                );
            }
            Kind::Backup => self.fetch(cx, move || remote::backups(&inst), |p, r| p.backups = r),
            Kind::Settings => {
                self.inputs.clear();
                self.fetch(cx, move || remote::settings(&inst), |p, r| p.settings = r);
            }
            Kind::Migration | Kind::Repair => self.fetch(cx, move || remote::status(&inst), |p, r| p.status = r),
            Kind::Gateway => self.fetch(cx, move || remote::gateway(&inst), |p, r| p.gateway = r),
        }
    }

    fn fetch<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
        put: impl FnOnce(&mut Self, Read<T>) + 'static,
    ) {
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { work() }).await;
            this.update(cx, |p, cx| {
                put(p, match got {
                    Ok(v) => Read::Got(v),
                    Err(e) => Read::Failed(e),
                });
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The log page reads again every few seconds while it is shown and
    /// "Follow" is on.
    fn start_log_follow(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
            let alive = this
                .update(cx, |p, cx| {
                    if p.kind == Kind::Logs && p.follow && !matches!(p.logs, Read::Loading) {
                        let inst = p.inst.clone();
                        p.fetch(cx, move || remote::logs(&inst, 500).map(|t| ui::strip_ansi(&t)), |p, r| {
                            p.logs = r;
                            p.scroll.scroll_to_bottom();
                        });
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        });
        self._tasks.push(task);
    }

    fn run_job(&mut self, label: &'static str, args: Vec<String>, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let inst = self.inst.clone();
        self.job_label = Some(format!("{label} on {}", inst.name).into());
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    sink(sentient_installer_core::progress::Progress::Step { name: label.to_string() });
                    let a: Vec<&str> = args.iter().map(String::as_str).collect();
                    let id = remote::start_job(&inst, &a)?;
                    remote::follow(&inst, &id, &sink)
                },
                cx,
            )
        });
        cx.notify();
    }

    // -- rendering ----------------------------------------------------------

    fn failed(theme: &gpui_kit::component::Theme, e: &str) -> Div {
        ui::panel(theme).child(ui::outcome(theme, Tone::Bad, e.to_string()))
    }

    fn loading(theme: &gpui_kit::component::Theme, what: &str) -> Div {
        ui::panel(theme).child(div().text_sm().text_color(theme.muted_foreground).child(format!("Reading {what} from the server…")))
    }

    fn job_panel(&self, cx: &mut Context<Self>) -> Option<Div> {
        let label = self.job_label.clone()?;
        let theme = cx.theme().clone();
        let job = self.job.read(cx);
        let log: Vec<SharedString> = job.log().iter().rev().take(12).rev().cloned().collect();
        let mut p = ui::panel(&theme).gap_2().child(div().font_weight(FontWeight::MEDIUM).child(label));
        p = match job.state() {
            JobState::Running => p.child(div().text_sm().child(job.step_with_time())),
            JobState::Done(m) => p.child(ui::outcome(&theme, Tone::Good, m.clone())),
            JobState::Failed(m) => {
                let (headline, details) = match m.split_once(remote::DETAILS) {
                    Some((h, d)) => (h.to_string(), Some(d.to_string())),
                    None => (m.to_string(), None),
                };
                let shown = self.show_details;
                p.child(ui::outcome(&theme, Tone::Bad, headline)).when_some(details, |p, d| {
                    p.child(Button::new("job-details").ghost().label(if shown { "Hide details" } else { "Show details" }).on_click(cx.listener(|this, _, _, cx| {
                        this.show_details = !this.show_details;
                        cx.notify();
                    })))
                    .when(shown, |p| p.child(div().p_2().rounded(theme.radius).bg(theme.muted).text_size(px(13.)).font_family(theme.mono_font_family.clone()).child(d)))
                })
            }
            JobState::Idle => p,
        };
        let done = !job.is_running();
        Some(
            p.child(v_flex().p_2().rounded(theme.radius).bg(theme.muted).text_size(px(13.)).font_family(theme.mono_font_family.clone()).children(log.into_iter().map(|l| div().child(l))))
                .when(done, |p| {
                    p.child(h_flex().justify_end().child(Button::new("job-close").outline().label("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.job_label = None;
                        this.job.update(cx, |j, cx| j.reset(cx));
                        cx.notify();
                    }))))
                }),
        )
    }

    fn services_panel(&self, theme: &gpui_kit::component::Theme) -> Div {
        let mut p = ui::panel(theme).gap_2().child(div().font_weight(FontWeight::MEDIUM).child("Services"));
        match &self.services {
            Read::Loading => p = p.child(div().text_sm().text_color(theme.muted_foreground).child("Reading…")),
            Read::Failed(e) => p = p.child(ui::outcome(theme, Tone::Bad, e.clone())),
            Read::Got(list) => {
                for s in list {
                    let (tone, label) = if s.running {
                        (Tone::Good, "Running")
                    } else if s.starting {
                        (Tone::Warn, "Starting")
                    } else {
                        (Tone::Bad, "Stopped")
                    };
                    p = p.child(
                        h_flex()
                            .gap_3()
                            .text_sm()
                            .items_center()
                            .child(h_flex().w(px(90.)).child(ui::pill(theme, tone, label)))
                            .child(div().w(px(220.)).child(s.name.clone()))
                            .child(div().flex_1().min_w_0().text_ellipsis().text_color(theme.muted_foreground).child(s.detail.clone())),
                    );
                }
            }
        }
        p
    }

    fn status_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut col = v_flex().gap_4();
        match &self.status {
            Read::Loading => col = col.child(Self::loading(&theme, "the state")),
            Read::Failed(e) => col = col.child(Self::failed(&theme, e)),
            Read::Got(s) if !s.installed => {
                // SENTIENT may run there already, started some other way
                // (Docker compose, an older installer): the Platform Manager
                // there finds it and takes it over without changing it.
                let busy = self.job.read(cx).is_running();
                let det = self.inst.detected.clone().unwrap_or_default();
                col = col.child(
                    ui::panel(&theme)
                        .child(div().font_weight(FontWeight::MEDIUM).child("The Platform Manager does not look after a SENTIENT here yet"))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(if det.install.is_empty() {
                            "If SENTIENT already runs on this server (in Docker, or installed some other way), Add existing finds it and takes it over: its data, settings and containers stay as they are.".to_string()
                        } else {
                            format!("SENTIENT was found on this server ({}). Add existing takes it over: its data, settings and containers stay as they are.", det.install)
                        }))
                        .child(h_flex().gap_2().child(
                            Button::new("r-adopt").primary().label("Add existing SENTIENT").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Adding the existing SENTIENT", vec!["add-existing".into()], cx))),
                        )),
                );
            }
            Read::Got(s) => {
                let det = self.inst.detected.clone().unwrap_or_default();
                let (tone, state) = if !s.installed {
                    (Tone::Neutral, "SENTIENT is not installed")
                } else if s.serving {
                    (Tone::Good, "SENTIENT is running")
                } else {
                    (Tone::Bad, "SENTIENT is stopped")
                };
                col = col.child(
                    ui::panel(&theme)
                        .child(h_flex().gap_3().items_center().child(ui::product_logo(ui::Product::Sentient, 28.)).child(ui::pill(&theme, tone, state)))
                        .child(ui::fact(&theme, "SENTIENT version", s.sentient_version.clone()))
                        .child(ui::fact(&theme, "Database", s.database.clone()))
                        .child(ui::fact(&theme, "Server", format!("{} · {} {}", self.inst.address(), det.os_name, det.arch)))
                        .child(ui::fact(&theme, "Runs as", if det.install.is_empty() { "—".to_string() } else { det.install.clone() }))
                        .child(ui::fact(&theme, "Disk free", s.disk_free.map(bytes_label).unwrap_or_else(|| "—".into())))
                        .child(ui::fact(&theme, "Last backup", s.last_backup.clone().unwrap_or_else(|| "none yet".into())))
                        .child(ui::fact(&theme, "Platform Manager there", s.manager_version.clone())),
                );
            }
        }
        col = col.child(self.services_panel(&theme));
        let mut db = ui::panel(&theme).gap_2().child(div().font_weight(FontWeight::MEDIUM).child("Database"));
        match &self.db {
            Read::Loading => db = db.child(div().text_sm().text_color(theme.muted_foreground).child("Reading…")),
            Read::Failed(e) => db = db.child(ui::outcome(&theme, Tone::Bad, e.clone())),
            Read::Got(d) => {
                if let Some(pg) = &d.pg {
                    db = db
                        .child(h_flex().gap_2().items_center().child(ui::database_logo("postgresql", 16.)).child(div().text_sm().child(format!("PostgreSQL {}", pg.version))))
                        .child(ui::fact(&theme, "Size", bytes_label(pg.db_bytes)))
                        .child(ui::fact(&theme, "Connections", format!("{} of {}", pg.connections, pg.max_connections)));
                    for t in pg.tables.iter().take(5) {
                        db = db.child(
                            h_flex()
                                .gap_4()
                                .text_sm()
                                .child(div().w_56().flex_shrink_0().text_color(theme.muted_foreground).child(crate::dbstatus::plain_table(&t.name).map(|(l, _)| l.to_string()).unwrap_or_else(|| t.name.clone())))
                                .child(div().child(bytes_label(t.bytes))),
                        );
                    }
                }
                if let Some(e) = &d.pg_error {
                    db = db.child(ui::outcome(&theme, Tone::Bad, format!("PostgreSQL: {e}")));
                }
                if let Some(ch) = &d.ch {
                    db = db
                        .child(h_flex().gap_2().items_center().child(ui::database_logo("clickhouse", 16.)).child(div().text_sm().child(format!("ClickHouse {}", ch.version))))
                        .child(ui::fact(&theme, "Telemetry history", format!("{} values, {}", count_label(ch.rows), bytes_label(ch.disk_bytes))))
                        .when_some(ch.first_ms.zip(ch.last_ms), |p, (a, b)| p.child(ui::fact(&theme, "From – to", format!("{} – {}", day_label(a), day_label(b)))));
                }
                if let Some(e) = &d.ch_error {
                    db = db.child(ui::outcome(&theme, Tone::Bad, format!("ClickHouse: {e}")));
                }
            }
        }
        col.child(db)
    }

    fn control_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().is_some_and(|s| s.installed);
        let serving = self.status.got().is_some_and(|s| s.serving);
        let btn = |id: &'static str, label: &'static str, icon: Lucide, primary: bool| {
            let b = Button::new(id).label(label).icon(Icon::new(icon)).disabled(busy || !installed);
            if primary { b.primary() } else { b.outline() }
        };
        v_flex()
            .gap_4()
            .child(
                ui::panel(&theme)
                    .child(div().text_sm().text_color(theme.muted_foreground).child(if serving { "SENTIENT is running on this server." } else { "SENTIENT is not running on this server." }))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(btn("r-start", "Start", Lucide::Play, !serving).on_click(cx.listener(|this, _, _, cx| this.run_job("Starting SENTIENT", vec!["control".into(), "start".into()], cx))))
                            .child(btn("r-stop", "Stop", Lucide::Square, false).on_click(cx.listener(|this, _, _, cx| this.run_job("Stopping SENTIENT", vec!["control".into(), "stop".into()], cx))))
                            .child(btn("r-restart", "Restart", Lucide::RotateCw, serving).on_click(cx.listener(|this, _, _, cx| this.run_job("Restarting SENTIENT", vec!["control".into(), "restart".into()], cx)))),
                    ),
            )
            .child(self.services_panel(&theme))
    }

    fn logs_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let needle = self.filter.read(cx).value().to_lowercase();
        let body: AnyElement = match &self.logs {
            Read::Loading => div().text_sm().text_color(theme.muted_foreground).child("Reading the log…").into_any_element(),
            Read::Failed(e) => ui::outcome(&theme, Tone::Bad, e.clone()).into_any_element(),
            Read::Got(t) => {
                let lines: Vec<SharedString> = t.lines().filter(|l| needle.is_empty() || l.to_lowercase().contains(&needle)).map(|l| SharedString::from(l.to_string())).collect();
                div()
                    .id("remote-log")
                    .track_scroll(&self.scroll)
                    .h(px(460.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .text_size(px(13.))
                    .font_family(theme.mono_font_family.clone())
                    .children(lines.into_iter().map(|l| div().child(l)))
                    .into_any_element()
            }
        };
        ui::panel(&theme)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().w_80().child(Input::new(&self.filter)))
                    .child(
                        Button::new("r-follow")
                            .label(if self.follow { "Following" } else { "Follow" })
                            .when(self.follow, |b| b.primary())
                            .when(!self.follow, |b| b.outline())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.follow = !this.follow;
                                cx.notify();
                            })),
                    )
                    .child(Button::new("r-logs-again").ghost().small().icon(IconName::RefreshCw).tooltip("Read again").on_click(cx.listener(|this, _, _, cx| this.reload(cx)))),
            )
            .child(body)
    }

    fn update_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().map(|s| s.sentient_version.clone());
        let mut p = ui::panel(&theme)
            .child(h_flex().gap_3().items_center().child(ui::product_logo(ui::Product::Sentient, 24.)).child(div().font_weight(FontWeight::MEDIUM).child("SENTIENT")))
            .child(ui::fact(&theme, "Installed there", installed.clone().unwrap_or_else(|| "reading…".into())))
            .child(ui::fact(&theme, "Update channel", format!("{} (this Platform Manager's preference)", crate::updates::Prefs::load().channel.label())));
        match &self.offer {
            Read::Loading => p = p.child(div().text_sm().text_color(theme.muted_foreground).child("Looking for a newer SENTIENT…")),
            Read::Failed(e) => p = p.child(ui::outcome(&theme, Tone::Bad, e.clone())),
            Read::Got(None) => p = p.child(ui::outcome(&theme, Tone::Good, "SENTIENT is up to date on this server.")),
            Read::Got(Some(o)) => {
                let version = o.version.clone();
                p = p
                    .child(ui::outcome(&theme, Tone::Neutral, format!("SENTIENT {} is available{}.", o.version, if o.date.is_empty() { String::new() } else { format!(", released {}", o.date) })))
                    .when(!o.between.is_empty(), |p| p.child(ui::release_notes(&theme, &o.between)))
                    .child(
                        h_flex().child(
                            Button::new("r-update")
                                .primary()
                                .label(format!("Update to {}", o.version))
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.run_job("Updating SENTIENT", vec!["update".into(), "--version".into(), version.clone()], cx))),
                        ),
                    )
                    .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(
                        "The update runs on the server by itself: a dropped connection does not stop it, and the previous version comes back if SENTIENT does not start.",
                    ));
            }
        }
        p
    }

    fn backup_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let mut p = ui::panel(&theme).gap_2();
        match &self.backups {
            Read::Loading => p = p.child(div().text_sm().text_color(theme.muted_foreground).child("Reading the backups…")),
            Read::Failed(e) => p = p.child(ui::outcome(&theme, Tone::Bad, e.clone())),
            Read::Got(b) => {
                p = p
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(div().flex_1().text_sm().child(format!(
                                "On the server in {}. {}",
                                b.folder.display(),
                                match &b.daily_at {
                                    Some(t) => format!("Every day at {t}, keeping {}.", b.keep),
                                    None => "No daily backup.".into(),
                                }
                            )))
                            .child(Button::new("r-backup").primary().label("Back up now").icon(Icon::new(Lucide::Archive)).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Backing up", vec!["backup-now".into()], cx)))),
                    )
                    .when(b.list.is_empty(), |p| p.child(div().text_sm().text_color(theme.muted_foreground).child("No backups on the server yet.")));
                for (i, e) in b.list.iter().enumerate() {
                    let check = match &e.check {
                        Some(c) if c.ok => ui::pill(&theme, Tone::Good, format!("Checked {}", c.at.get(..10).unwrap_or(&c.at))),
                        Some(_) => ui::pill(&theme, Tone::Bad, "Damaged"),
                        None => ui::pill(&theme, Tone::Neutral, "Not checked"),
                    };
                    p = p.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .py_1p5()
                            .when(i > 0, |r| r.border_t_1())
                            .border_color(theme.border)
                            .child(v_flex().flex_1().min_w_0().child(div().text_sm().text_ellipsis().child(e.name.clone())).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!("{} · {}", e.made, if e.automatic { "automatic" } else { "by hand" }))))
                            .child(div().text_sm().child(format!("{}{}", bytes_label(e.bytes), if e.parts > 1 { format!(" · {} parts", e.parts) } else { String::new() })))
                            .child(check),
                    );
                }
            }
        }
        p
    }

    // -- Settings, Migration, Repair, Gateway --------------------------------

    fn settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let values = match &self.settings {
            Read::Loading => return Self::loading(&theme, "the settings"),
            Read::Failed(e) => return Self::failed(&theme, e),
            Read::Got(v) => v.clone(),
        };
        let groups = crate::catalog::groups();
        if self.inputs.is_empty() {
            for g in &groups {
                for st in &g.settings {
                    let v = values.get(&st.env).cloned().unwrap_or_default();
                    let ph = if st.default.is_empty() { "not set".to_string() } else { format!("default {}", st.default) };
                    let input = cx.new(|cx| InputState::new(window, cx).default_value(v).placeholder(ph));
                    self._subscriptions.push(cx.subscribe(&input, |_, _, e: &InputEvent, cx| {
                        if matches!(e, InputEvent::Change) {
                            cx.notify();
                        }
                    }));
                    self.inputs.push((st.env.clone(), input));
                }
            }
        }
        let busy = self.job.read(cx).is_running();
        let value_of = |env: &str, cx: &App| self.inputs.iter().find(|(e, _)| e == env).map(|(_, i)| i.read(cx).value().trim().to_string()).unwrap_or_default();
        let changed: Vec<String> = self.inputs.iter().filter(|(e, i)| values.get(e).cloned().unwrap_or_default() != i.read(cx).value().trim()).map(|(e, _)| e.clone()).collect();
        let mut col = v_flex().gap_4();
        if !changed.is_empty() {
            let mut all = values.clone();
            for (e, _) in &self.inputs {
                let v = value_of(e, cx);
                if v.is_empty() {
                    all.remove(e);
                } else {
                    all.insert(e.clone(), v);
                }
            }
            let n = changed.len();
            col = col.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.primary)
                    .bg(theme.accent)
                    .child(div().flex_1().text_sm().child(format!("{n} change{} to apply on {}. SENTIENT restarts when a setting needs it.", if n == 1 { "" } else { "s" }, self.inst.name)))
                    .child(Button::new("r-set-undo").ghost().label("Undo").on_click(cx.listener(|this, _, _, cx| {
                        this.inputs.clear();
                        cx.notify();
                    })))
                    .child(Button::new("r-set-apply").primary().label("Apply").disabled(busy).on_click(cx.listener(move |this, _, _, cx| this.apply_settings(all.clone(), cx)))),
            );
        }
        for g in groups {
            let mut panel = ui::panel(&theme).gap_1().child(div().font_weight(FontWeight::MEDIUM).child(g.name.clone()));
            let mut first = true;
            for st in g.settings {
                let Some((_, input)) = self.inputs.iter().find(|(e, _)| *e == st.env) else { continue };
                let is_changed = changed.contains(&st.env);
                panel = panel.child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .py_1()
                        .when(!std::mem::take(&mut first), |r| r.border_t_1())
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .w(px(260.))
                                .flex_shrink_0()
                                .gap_1()
                                .items_center()
                                .child(div().text_sm().when(is_changed, |d| d.text_color(theme.primary).font_weight(FontWeight::MEDIUM)).child(st.label.clone()))
                                .child(Button::new(SharedString::from(format!("r-help-{}", st.env))).ghost().xsmall().icon(IconName::Info).tooltip(st.help.clone())),
                        )
                        .child(div().flex_1().min_w_0().child(Input::new(input))),
                );
            }
            col = col.child(panel);
        }
        col
    }

    fn apply_settings(&mut self, values: std::collections::HashMap<String, String>, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let inst = self.inst.clone();
        self.job_label = Some(format!("Applying settings on {}", inst.name).into());
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    let file = format!("{}/.spm-settings.json", remote::home(&inst)?);
                    remote::put_text(&inst, &file, &serde_json::to_string(&values).map_err(|e| e.to_string())?)?;
                    sink(sentient_installer_core::progress::Progress::Step { name: "Applying the settings".into() });
                    let id = remote::start_job(&inst, &["apply-settings", &file])?;
                    let r = remote::follow(&inst, &id, &sink);
                    remote::remove_file(&inst, &file);
                    r
                },
                cx,
            )
        });
        self.inputs.clear();
        cx.notify();
    }

    fn migration_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let current = self.status.got().map(|s| s.database.clone()).unwrap_or_default();
        const TARGETS: [(&str, &str); 3] = [("stock", "PostgreSQL"), ("timescaledb", "PostgreSQL + TimescaleDB"), ("clickhouse", "PostgreSQL + ClickHouse")];
        let (flavor, label) = TARGETS[self.target.min(2)];
        let same = current == label;
        ui::panel(&theme)
            .child(ui::fact(&theme, "Database now", if current.is_empty() { "reading…".to_string() } else { current.clone() }))
            .child(
                h_flex().gap_3().items_center().child(div().text_sm().child("Move to")).child(
                    gpui_kit::component::radio::RadioGroup::horizontal("r-target")
                        .children(TARGETS.map(|t| t.1))
                        .selected_index(Some(self.target))
                        .on_change(cx.listener(|this, v: &usize, _, cx| {
                            this.target = *v;
                            cx.notify();
                        })),
                ),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "The data is copied into the new type beside the old one, SENTIENT switches over, and the old database is kept until you remove it. SENTIENT stops while it switches.",
            ))
            .child(
                h_flex().child(
                    Button::new("r-migrate")
                        .primary()
                        .label(if same { "This is the database type already".to_string() } else { format!("Move to {label}") })
                        .disabled(busy || same || current.is_empty())
                        .on_click(cx.listener(move |this, _, _, cx| this.run_job("Moving the database", vec!["migrate".into(), flavor.into()], cx))),
                ),
            )
    }

    fn repair_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().is_some_and(|s| s.installed);
        ui::panel(&theme)
            .child(div().text_sm().child("Repair puts SENTIENT's services, files and settings on this server back as the Platform Manager installed them. The database and its data are kept."))
            .child(h_flex().child(Button::new("r-repair").primary().label("Repair").icon(Icon::new(Lucide::Wrench)).disabled(busy || !installed).on_click(cx.listener(|this, _, _, cx| this.run_job("Repairing", vec!["repair".into()], cx)))))
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("Uninstalling SENTIENT is done on the server itself, not from another computer."))
    }

    fn gateway_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let g = match &self.gateway {
            Read::Loading => return Self::loading(&theme, "the gateway"),
            Read::Failed(e) => return Self::failed(&theme, e),
            Read::Got(g) => g.clone(),
        };
        let get = |k: &str| g.get(k).cloned().unwrap_or_default();
        let installed = get("installed") == "true";
        let running = get("running") == "true";
        let mut p = ui::panel(&theme).child(
            h_flex()
                .gap_3()
                .items_center()
                .child(ui::product_logo(ui::Product::Gateway, 24.))
                .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                .child(if !installed { ui::pill(&theme, Tone::Neutral, "Not installed") } else if running { ui::pill(&theme, Tone::Good, "Running") } else { ui::pill(&theme, Tone::Bad, "Stopped") }),
        );
        if !installed {
            return p.child(div().text_sm().text_color(theme.muted_foreground).child("No gateway on this server. Install it from the Platform Manager on the server itself; its access token stays there."));
        }
        p = p
            .child(ui::fact(&theme, "Version", get("version")))
            .child(ui::fact(&theme, "Connects to", format!("{}:{}", get("host"), get("port"))))
            .child(ui::fact(&theme, "Its page", get("page")));
        let act = |id: &'static str, label: &'static str, action: &'static str| {
            Button::new(id).outline().label(label).disabled(busy).on_click(cx.listener(move |this, _, _, cx| this.run_job("Gateway", vec!["gateway-control".into(), action.into()], cx)))
        };
        p.child(
            h_flex()
                .gap_2()
                .child(act("r-gw-start", "Start", "start"))
                .child(act("r-gw-stop", "Stop", "stop"))
                .child(act("r-gw-restart", "Restart", "restart"))
                .child(Button::new("r-gw-update").primary().label("Update to the newest").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Updating the gateway", vec!["gateway-update".into()], cx)))),
        )
    }

    /// One calm panel for a server that is not set up or cannot be reached:
    /// what is the matter and the one thing that solves it.
    fn gate_panel(&self, e: String, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let not_set_up = e == remote::NOT_SET_UP;
        ui::panel(&theme)
            .child(div().font_weight(FontWeight::MEDIUM).child(if not_set_up { "Not set up yet" } else { "The server does not answer" }))
            .child(div().text_sm().text_color(theme.muted_foreground).child(if not_set_up {
                "The Platform Manager works on a server through its own copy there. Set up copies the matching build over SSH and checks it; nothing else on the server changes.".to_string()
            } else {
                e.clone()
            }))
            .child(
                h_flex().gap_2().child(if not_set_up {
                    Button::new("r-setup").primary().label("Set up").loading(busy).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.set_up(cx)))
                } else {
                    Button::new("r-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| {
                        this.ready = Read::Loading;
                        this.reload(cx);
                    }))
                }),
            )
    }

    fn set_up(&mut self, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let inst = self.inst.clone();
        self.job_label = Some(format!("Setting up {}", inst.name).into());
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    sink(sentient_installer_core::progress::Progress::Done { message: format!("The Platform Manager is set up on {}.", inst.name) });
                    Ok(())
                },
                cx,
            )
        });
        cx.notify();
    }
}

impl Render for RemotePages {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (title, summary) = match self.kind {
            Kind::Status => ("Status", "SENTIENT on this server: its state, services and database."),
            Kind::Control => ("Control", "Start, stop or restart SENTIENT on this server."),
            Kind::Logs => ("Logs", "The most recent lines of the SENTIENT log on this server."),
            Kind::Update => ("Update", "A newer SENTIENT for this server, from the update channel."),
            Kind::Backup => ("Backup and restore", "The backups on this server."),
            Kind::Settings => ("Platform settings", "SENTIENT's settings on this server."),
            Kind::Migration => ("Migration", "Move this server's SENTIENT to another database type."),
            Kind::Repair => ("Repair", "Put SENTIENT's services and files on this server back in order."),
            Kind::Gateway => ("Gateway", "The Sentient Gateway on this server."),
        };
        let gate: Option<Div> = match &self.ready {
            Read::Loading => Some(Self::loading(&theme, "the server's state")),
            // While Set up runs or shows its result, only its panel.
            Read::Failed(_) if self.job_label.is_some() => Some(div()),
            Read::Failed(e) => Some(self.gate_panel(e.clone(), cx)),
            Read::Got(()) => None,
        };
        let body = if let Some(g) = gate { g } else { match self.kind {
            Kind::Status => self.status_page(cx),
            Kind::Control => self.control_page(cx),
            Kind::Logs => self.logs_page(cx),
            Kind::Update => self.update_page(cx),
            Kind::Backup => self.backup_page(cx),
            Kind::Settings => self.settings_page(window, cx),
            Kind::Migration => self.migration_page(cx),
            Kind::Repair => self.repair_page(cx),
            Kind::Gateway => self.gateway_page(cx),
        } };
        let job = self.job_panel(cx);
        ui::page(&theme, title, summary)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().size(px(10.)).rounded_full().bg(rgb(remote::parse_hex(&self.inst.color).unwrap_or(0x187074))))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(format!("{} · {}", self.inst.name, self.inst.host)))
                    .child(div().flex_1())
                    .when(self.kind != Kind::Logs, |r| r.child(Button::new("r-reload").ghost().small().icon(IconName::RefreshCw).tooltip("Read again").on_click(cx.listener(|this, _, _, cx| this.reload(cx))))),
            )
            .children(job)
            .child(body)
    }
}
