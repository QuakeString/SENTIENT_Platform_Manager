// Copyright © 2016-2026 The SENTIENT Authors

//! The pages of a remote instance (crate::remote): Status, Control, Logs,
//! Update and Backup, read from the server's own Platform Manager over SSH
//! and worked through detached jobs there. One view per open instance; the
//! menu chooses which page it shows.

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    menu::{DropdownMenu as _, PopupMenuItem},
    input::{InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::dbstatus::{DbStatus, bytes_label};
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

/// The server agent update started when a server is opened.
enum AgentRefresh {
    Idle,
    Running,
    Failed(String),
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
    /// Migration's chosen target (an index into the types other than the one now).
    target: Option<usize>,
    /// Migration: Change was pressed; the confirmation shows.
    migrate_confirm: bool,
    /// The newest SENTIENT on the update channel (None: none newer).
    offer: Read<Option<crate::updates::Offer>>,
    /// The versions "Other version" lists.
    versions: Read<Vec<crate::online::Version>>,
    /// "add-if-one" ran once for this instance.
    auto_add_tried: bool,
    /// The server agent update that runs when the server is opened.
    agent_refresh: AgentRefresh,
    /// Platform settings: the open tab (crate::manager::settings::TAB_TITLES).
    settings_tab: usize,
    /// Control: the service whose own action runs now.
    svc_busy: Option<String>,
    /// Logs: the shared view, made when the page first shows.
    log_view: Option<Entity<crate::manager::logs::LogView>>,

    /// Platform settings: what "Check" found for a file path on the server.
    file_checks: std::collections::HashMap<String, Read<bool>>,
    /// A version picked under "Other version", waiting for its confirmation.
    pick: Option<String>,
    /// This Platform Manager's update channel (shared with the local Update).
    updates: Entity<crate::updates::UpdateState>,
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
    pub fn new(inst: Instance, kind: Kind, updates: Entity<crate::updates::UpdateState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let subs = vec![
            cx.observe(&job, |this, job, cx| {
                if !job.read(cx).is_running() {
                    this.svc_busy = None;
                    this.reload(cx);
                }
                cx.notify();
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
            target: None,
            migrate_confirm: false,
            offer: Read::Loading,
            versions: Read::Loading,
            pick: None,
            file_checks: Default::default(),
            log_view: None,
            svc_busy: None,
            settings_tab: 0,
            auto_add_tried: false,
            agent_refresh: AgentRefresh::Idle,
            updates,
            follow: true,
            job,
            job_label: None,
            show_details: false,
            scroll: ScrollHandle::new(),
            _tasks: Vec::new(),
            _subscriptions: subs,
        };
        // Development aid: open Platform settings on a tab (screens).
        #[cfg(debug_assertions)]
        if let Ok(t) = std::env::var("SPM_SETTINGS_TAB") {
            let names = ["platform", "database", "network", "retention", "advanced"];
            p.settings_tab = names.iter().position(|n| n.eq_ignore_ascii_case(&t)).unwrap_or(0);
        }
        p.reload(cx);
        p.refresh_agent(cx);
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
    /// A server set up by an earlier Platform Manager keeps its older agent
    /// until a job runs, and the pages need the new one's jobs (per-service
    /// control, log sources): bring it up to date when the server is opened,
    /// then read again. Only on a server that is set up already.
    fn refresh_agent(&mut self, cx: &mut Context<Self>) {
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_REMOTE_STATUS").is_some() {
            // Development aid: show a refresh state (screens).
            self.agent_refresh = match std::env::var("SPM_DEBUG_AGENT_REFRESH").as_deref() {
                Ok("updating") => AgentRefresh::Running,
                Ok("failed") => AgentRefresh::Failed("The server did not answer (address, port or firewall).".into()),
                _ => AgentRefresh::Idle,
            };
            return;
        }
        let inst = self.inst.clone();
        self.agent_refresh = AgentRefresh::Running;
        let task = cx.spawn(async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move {
                    remote::ready(&inst)?;
                    remote::deploy(&inst, &sentient_installer_core::progress::noop())
                })
                .await;
            this.update(cx, |p, cx| {
                match done {
                    Ok(_) => {
                        p.agent_refresh = AgentRefresh::Idle;
                        p.reload(cx);
                    }
                    Err(e) => p.agent_refresh = AgentRefresh::Failed(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let inst = self.inst.clone();
        self.fetch(cx, move || remote::ready(&inst), |p, r| p.ready = r);
        // Every page needs to know whether a SENTIENT is added there.
        if !matches!(self.kind, Kind::Status | Kind::Control | Kind::Update | Kind::Migration | Kind::Repair) {
            let inst = self.inst.clone();
            self.fetch(cx, move || remote::status(&inst), |p, r| p.status = r);
        }
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
            Kind::Logs => {}
            Kind::Update => {
                let i2 = inst.clone();
                self.fetch(cx, move || remote::status(&i2), |p, r| p.status = r);
                let native = inst.detected.as_ref().is_some_and(|d| d.install != "Docker");
                self.pick = None;
                self.fetch(
                    cx,
                    move || {
                        #[cfg(debug_assertions)]
                        if std::env::var_os("SPM_DEBUG_REMOTE_OFFER").is_some() {
                            let v = |version: &str, date: &str| crate::online::Version { version: version.into(), date: date.into(), summary: String::new() };
                            return Ok(vec![v("4.4.0.2", "2026-11-14"), v("4.4.0.1", "2026-11-13"), v("4.3.0.58", "2026-11-02")]);
                        }
                        crate::online::versions(native)
                    },
                    |p, r| p.versions = r,
                );
                self.fetch(
                    cx,
                    move || {
                        #[cfg(debug_assertions)]
                        if std::env::var_os("SPM_DEBUG_REMOTE_OFFER").is_some() {
                            return Ok(Some(canned_offer()));
                        }
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

    fn run_job(&mut self, label: impl Into<String>, args: Vec<String>, cx: &mut Context<Self>) {
        let label: String = label.into();
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
                let (headline, details) = remote::split_failure(m);
                let shown = self.show_details;
                p.child(ui::outcome(&theme, Tone::Bad, headline)).when_some(details, |p, d| {
                    p.child(crate::ui::button("job-details").ghost().label(if shown { "Hide details" } else { "Show details" }).on_click(cx.listener(|this, _, _, cx| {
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
                    p.child(h_flex().justify_end().child(crate::ui::button("job-close").outline().label("Close").on_click(cx.listener(|this, _, _, cx| {
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
                p = p.children(ui::service_rows(theme, list.iter().map(|s| (s.name.clone().into(), s.running, s.starting, s.detail.clone().into())), None));
            }
        }
        p
    }

    /// No SENTIENT added on the server yet. Not an error: what was found and
    /// the one action. Adding writes only the Platform Manager's own record.
    fn not_added_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let det = self.inst.detected.clone().unwrap_or_default();
        ui::panel(&theme)
            .child(h_flex().gap_3().items_center().child(ui::product_logo(ui::Product::Sentient, 30.)).child(ui::pill(&theme, Tone::Neutral, "Not added")))
            .child(div().text_sm().text_color(theme.muted_foreground).child(if det.install.is_empty() {
                "The server agent is set up; no SENTIENT on this server is added to the Platform Manager. Add existing finds a SENTIENT that runs on the server (in Docker, or installed some other way). Nothing on it is changed.".to_string()
            } else {
                format!("SENTIENT was found on this server ({}). Add existing lets the Platform Manager look after it. Nothing on it is changed.", det.install)
            }))
            .child(h_flex().gap_2().child(
                crate::ui::button("r-adopt").primary().label("Add existing SENTIENT").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Adding the existing SENTIENT", vec!["add-if-one".into()], cx))),
            ))
    }

    fn status_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut col = v_flex().gap_4();
        match &self.status {
            Read::Loading => col = col.child(Self::loading(&theme, "the state")),
            Read::Failed(e) => col = col.child(Self::failed(&theme, e)),
            Read::Got(s) if !s.installed => {
                // Nothing added there yet: only the calm panel, no empty
                // Services or Database panels.
                return col.child(self.not_added_panel(cx));
            }
            Read::Got(s) => {
                let det = self.inst.detected.clone().unwrap_or_default();
                // Its port answers but none of its own services runs: some
                // other program is serving there.
                let none_running = matches!(&self.services, Read::Got(list) if !list.is_empty() && !list.iter().any(|v| v.running || v.starting));
                let (tone, state) = if !s.installed {
                    (Tone::Neutral, "SENTIENT is not installed")
                } else if s.serving && none_running {
                    (Tone::Warn, "Another program answers on SENTIENT's port")
                } else if s.serving {
                    (Tone::Good, "SENTIENT is running")
                } else {
                    (Tone::Bad, "SENTIENT is stopped")
                };
                let url = format!("http://{}:{}", self.inst.host, s.http_port.unwrap_or(8080));
                col = col.child(
                    ui::panel(&theme)
                        .child(
                            h_flex()
                                .gap_3()
                                .items_center()
                                .child(ui::product_logo(ui::Product::Sentient, 30.))
                                .child(ui::pill(&theme, tone, state))
                                .child(div().flex_1())
                                .when(s.installed, |r| r.child(crate::ui::button("r-open").primary().label("Open SENTIENT").on_click(move |_, _, cx| ui::open_url(&url, cx)))),
                        )
                        .child(ui::fact(&theme, "SENTIENT version", s.sentient_version.clone()))
                        .child(ui::fact(&theme, "Database", s.database.clone()))
                        .child(ui::fact(&theme, "Runs as", if det.install.is_empty() { "Not known".to_string() } else { det.install.clone() }))
                        .children(s.vanished_files.first().map(|f| {
                            div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!("This SENTIENT was started with an extra compose file that no longer exists: {f}"))
                        }))
                        .child(ui::fact(&theme, "Server", format!("{} · {} {}", self.inst.address(), det.os_name, det.arch)))
                        .child(
                            // A pill only while the update runs or when it failed;
                            // the normal state has none.
                            h_flex()
                                .gap_3()
                                .items_center()
                                .child(ui::fact(&theme, "Server agent", s.manager_version.clone()).flex_1())
                                .map(|r| match &self.agent_refresh {
                                    AgentRefresh::Running => r.child(ui::pill(&theme, Tone::Warn, "Updating")),
                                    AgentRefresh::Failed(why) => {
                                        let why = SharedString::from(why.clone());
                                        r.child(div().id("agent-failed").child(ui::pill(&theme, Tone::Bad, "Update failed")).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(why.clone()).build(w, cx)))
                                    }
                                    AgentRefresh::Idle => r,
                                }),
                        ),
                );
            }
        }
        col = col.child(self.services_panel(&theme));
        // The same Database panel as this computer's, read without asking.
        let db = ui::panel(&theme).child(div().font_weight(FontWeight::MEDIUM).child("Database"));
        let db = match &self.db {
            Read::Loading => db.child(div().text_sm().text_color(theme.muted_foreground).child("Reading the database's size and state…")),
            Read::Failed(e) => db.child(ui::outcome(&theme, Tone::Warn, format!("The database's size and state could not be read.\n{e}"))),
            Read::Got(d) => {
                let last = self.status.got().and_then(|s| s.last_backup.clone()).unwrap_or_else(|| "None".into());
                let this = cx.entity().downgrade();
                let back_up: std::rc::Rc<dyn Fn(&mut App)> = std::rc::Rc::new(move |cx| {
                    this.update(cx, |p, cx| p.run_job("Backing up", vec!["backup-now".into()], cx)).ok();
                });
                crate::manager::status::db_details(
                    &theme,
                    db,
                    d,
                    "The server's database disk",
                    None,
                    last,
                    crate::manager::status::DbActions { set_limit: None, remove_kept: None, back_up: Some(back_up) },
                    crate::theme::is_dark(cx),
                )
            }
        };
        col.child(db)
    }

    fn control_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().is_some_and(|s| s.installed);
        let serving = self.status.got().is_some_and(|s| s.serving);
        let this = cx.entity().downgrade();
        let (t1, t2, t3) = (this.clone(), this.clone(), this);
        let row = crate::manager::control::control_row(
            &theme,
            "r-",
            busy || !installed,
            self.status.got().map(|s| s.serving),
            std::rc::Rc::new(move |_, cx| {
                t1.update(cx, |p, cx| p.run_job("Starting SENTIENT", vec!["control".into(), "start".into()], cx)).ok();
            }),
            std::rc::Rc::new(move |_, cx| {
                t2.update(cx, |p, cx| p.run_job("Restarting SENTIENT", vec!["control".into(), "restart".into()], cx)).ok();
            }),
            std::rc::Rc::new(move |w, cx| {
                let t3 = t3.clone();
                crate::manager::control::confirm_stop(w, cx, std::rc::Rc::new(move |cx| {
                    t3.update(cx, |p, cx| p.run_job("Stopping SENTIENT", vec!["control".into(), "stop".into()], cx)).ok();
                }));
            }),
        );
        let _ = serving;
        v_flex()
            .gap_4()
            .child(ui::panel(&theme).child(row))
            .child(self.services_panel_with_actions(cx))
    }

    /// Control's Services: each row with Start, Stop, Restart and Logs. The
    /// database's own stop or restart asks first while SENTIENT runs.
    fn services_panel_with_actions(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut p = ui::panel(&theme).gap_2().child(div().font_weight(FontWeight::MEDIUM).child("Services"));
        let serving = self.status.got().is_some_and(|s| s.serving);
        let this = cx.entity().downgrade();
        let act: ui::ServiceAct = std::rc::Rc::new(move |name, action, window, cx| {
            let name = name.to_string();
            let plain = ui::service_label(&name).unwrap_or(&name).to_string();
            let verb = match action {
                "start" => "Starting",
                "stop" => "Stopping",
                _ => "Restarting",
            };
            let go: std::rc::Rc<dyn Fn(&mut App)> = std::rc::Rc::new({
                let (this, name, label) = (this.clone(), name.clone(), format!("{verb} {plain}"));
                move |cx| {
                    this.update(cx, |p, cx| {
                        p.svc_busy = Some(name.clone());
                        p.run_job(label.clone(), vec!["service".into(), action.into(), name.clone()], cx);
                    })
                    .ok();
                }
            });
            let n = name.to_lowercase();
            let database = n.contains("postgres") || n.contains("clickhouse");
            if database && serving && action != "start" {
                let title = format!("{} {plain}?", if action == "stop" { "Stop" } else { "Restart" });
                crate::modal::alert(window, cx, move |d, _, _| {
                    let go = go.clone();
                    d.title(title.clone())
                        .description("SENTIENT loses its database until it is back: devices and dashboards stop updating meanwhile.")
                        .confirm()
                        .ok_text(if action == "stop" { "Stop" } else { "Restart" })
                        .on_ok(move |_, _, cx| {
                            go(cx);
                            true
                        })
                });
            } else {
                go(cx);
            }
        });
        let log: ui::ServiceLog = std::rc::Rc::new(|name, _, cx| {
            crate::logsource::request(crate::logsource::of_service(name));
            cx.refresh_windows();
        });
        match &self.services {
            Read::Loading => p = p.child(div().text_sm().text_color(theme.muted_foreground).child("Reading…")),
            Read::Failed(e) => p = p.child(ui::outcome(&theme, Tone::Bad, e.clone())),
            Read::Got(list) => {
                p = p.children(ui::service_rows(&theme, list.iter().map(|s| (s.name.clone().into(), s.running, s.starting, s.detail.clone().into())), Some((act, log, self.svc_busy.clone()))));
            }
        }
        p
    }

    /// The same Logs view as this computer's, reading through the agent.
    fn logs_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let inst = self.inst.clone();
        let view = self.log_view.get_or_insert_with(|| cx.new(|cx| crate::manager::logs::LogView::remote(inst, window, cx))).clone();
        v_flex().child(view)
    }

    /// The same layout as this computer's Update: the channel, then one
    /// SENTIENT line with its buttons; the change log opens in a dialog.
    fn update_page(&self, cx: &mut Context<Self>) -> Div {
        use crate::manager::update::{Offered, product_row, sentient_summary};
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().map(|s| s.sentient_version.clone()).unwrap_or_else(|| "Reading…".into());
        let channel = self.updates.read(cx).prefs.channel;
        let checking = matches!(self.offer, Read::Loading);
        let channels = ui::seg_track(&theme).children(sentient_installer_core::releases::Channel::ALL.into_iter().map(|c| {
            ui::seg_item(&theme, SharedString::from(format!("r-channel-{}", c.label())), c.label(), c == channel).on_click(cx.listener(move |this, _, _, cx| {
                this.updates.update(cx, |u, cx| u.set_channel(c, cx));
                this.reload(cx);
            }))
        }));
        let offer = match &self.offer {
            Read::Got(o) => o.clone(),
            _ => None,
        };
        let state = match &self.offer {
            Read::Loading => Offered::Checking,
            Read::Failed(_) => Offered::NotChecked,
            Read::Got(None) => Offered::UpToDate,
            Read::Got(Some(o)) => Offered::Newer(o),
        };
        let (sub, pill) = sentient_summary(&installed, state, false);
        let mut row = product_row(&theme, offer.is_some(), ui::Product::Sentient, "SENTIENT", sub);
        if let Some((tone, text)) = pill {
            row = row.child(ui::pill(&theme, tone, text));
        }
        if let Some(o) = &offer {
            let version = o.version.clone();
            let (o2, inst_v) = (o.clone(), installed.clone());
            row = row
                .child(
                    crate::ui::button("r-update")
                        .primary()
                        .label(format!("Update to {}", o.version))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.run_job("Updating SENTIENT", vec!["update".into(), "--version".into(), version.clone()], cx))),
                )
                .child(crate::ui::button("r-notes").outline().label("What's new").on_click(move |_, _, cx| crate::manager::update::open_notes("SENTIENT", &inst_v, &o2, cx)));
        }
        let versions: Vec<crate::online::Version> = match &self.versions {
            Read::Got(v) => v.iter().filter(|v| v.version != installed).cloned().collect(),
            _ => Vec::new(),
        };
        let this = cx.entity().downgrade();
        row = row.child(
            crate::ui::button("r-other")
                .outline()
                .label("Other version")
                .disabled(busy || versions.is_empty())
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for v in &versions {
                        let (t, ver) = (this.clone(), v.version.clone());
                        menu = menu.item(PopupMenuItem::new(v.label()).on_click(move |_, _, cx| {
                            t.update(cx, |p, cx| {
                                p.pick = Some(ver.clone());
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                }),
        );
        let failed = match &self.offer {
            Read::Failed(e) => Some(e.clone()),
            _ => None,
        };
        let pick = self.pick.clone();
        ui::panel(&theme)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .flex_wrap()
                    .child(div().text_sm().child("Channel"))
                    .child(channels)
                    .child(div().flex_1())
                    .child(
                        crate::ui::button("r-check")
                            .outline()
                            .label(if checking { "Checking…" } else { "Check for updates" })
                            .disabled(checking)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .when_some(failed, |p, e| {
                p.child(ui::outcome(
                    &theme,
                    Tone::Warn,
                    if e.contains("limiting") { "GitHub is limiting requests from this network. Try again in an hour." } else { "The release page could not be reached. Nothing was changed." },
                ))
            })
            .child(row)
            .when_some(pick, |p, v| {
                let v2 = v.clone();
                p.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .child(div().flex_1().min_w(px(220.)).text_sm().child(format!("Install SENTIENT {v} on this server instead of {installed}?")))
                        .child(crate::ui::button("r-pick-go").primary().label(format!("Install {v}")).disabled(busy).on_click(cx.listener(move |this, _, _, cx| {
                            this.pick = None;
                            this.run_job("Updating SENTIENT", vec!["update".into(), "--version".into(), v2.clone()], cx);
                        })))
                        .child(crate::ui::button("r-pick-no").ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.pick = None;
                            cx.notify();
                        }))),
                )
            })
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(
                "The update runs on the server by itself: a dropped connection does not stop it, and the previous version comes back if SENTIENT does not start.",
            ))
    }

    fn backup_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        // The same panels as this computer's Backup: "Back up now", the
        // daily backup, then the list. Rows the server cannot change from
        // here yet are left out.
        let b = match &self.backups {
            Read::Loading => return Self::loading(&theme, "the backups"),
            Read::Failed(e) => return Self::failed(&theme, e),
            Read::Got(b) => b.clone(),
        };
        let now = ui::panel(&theme)
            .gap_0()
            .child(div().pb_1().font_weight(FontWeight::MEDIUM).child("Back up now"))
            .child(crate::manager::backup::brow_pub(&theme, "Save to", None, div().text_sm().child(b.folder.display().to_string())))
            .child(h_flex().pt_2().child(crate::ui::button("r-backup").primary().label("Back up now").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Backing up", vec!["backup-now".into()], cx)))));
        let auto = ui::panel(&theme)
            .gap_0()
            .child(div().pb_1().font_weight(FontWeight::MEDIUM).child("Automatic backup"))
            .child(crate::manager::backup::brow_pub(&theme, "Back up every day at", None, div().text_sm().child(b.daily_at.clone().unwrap_or_else(|| "No automatic backup".into()))))
            .when(b.daily_at.is_some(), |p| p.child(crate::manager::backup::brow_pub(&theme, "Keep", None, div().text_sm().child(format!("{} backups", b.keep)))));
        let total: u64 = b.list.iter().map(|e| e.bytes).sum();
        let mut list = ui::panel(&theme).gap_0().child(
            h_flex()
                .gap_3()
                .pb_1()
                .items_center()
                .child(div().font_weight(FontWeight::MEDIUM).child("Backups"))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().text_color(theme.muted_foreground).child(format!(
                    "{} · {} file{}, {}",
                    b.folder.display(),
                    b.list.len(),
                    if b.list.len() == 1 { "" } else { "s" },
                    bytes_label(total)
                ))),
        );
        if b.list.is_empty() {
            list = list.child(div().py_2().text_sm().text_color(theme.muted_foreground).child("No backups in this folder."));
        }
        for (i, e) in b.list.iter().enumerate() {
            // The server cannot run the integrity check yet: no pill to act on.
            list = list.child(crate::manager::backup::entry_row_with(&theme, i, e, false));
        }
        v_flex().gap_4().child(now).child(auto).child(list)
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
                    .child(crate::ui::button("r-set-undo").ghost().label("Undo").on_click(cx.listener(|this, _, _, cx| {
                        this.inputs.clear();
                        cx.notify();
                    })))
                    .child(crate::ui::button("r-set-apply").primary().label("Apply").disabled(busy).on_click(cx.listener(move |this, _, _, cx| this.apply_settings(all.clone(), cx)))),
            );
        }
        // The same tabs, rows and Advanced table as this computer's
        // Platform settings. Rows that need this computer (presets from its
        // memory, administrator accounts) are left out.
        use crate::manager::settings as local;
        let this = cx.entity().downgrade();
        let nav = local::tab_bar(&theme, "r-", self.settings_tab, std::rc::Rc::new(move |i, cx| {
            this.update(cx, |p, cx| {
                p.settings_tab = i;
                cx.notify();
            })
            .ok();
        }));
        let active = self.settings_tab;
        let all: Vec<crate::catalog::Setting> = groups.iter().flat_map(|g| g.settings.iter().cloned()).collect();
        let field_of = |st: &crate::catalog::Setting, cx: &mut Context<Self>| -> AnyElement {
            let Some((_, input)) = self.inputs.iter().find(|(e, _)| *e == st.env) else { return div().into_any_element() };
            let this = cx.entity().downgrade();
            let inp = input.clone();
            let set: crate::manager::setting_field::SetFn = std::rc::Rc::new(move |v, window, cx| {
                inp.update(cx, |s, cx| s.set_value(v.to_string(), window, cx));
                this.update(cx, |_, cx| cx.notify()).ok();
            });
            let this = cx.entity().downgrade();
            let (env, inp) = (st.env.clone(), input.clone());
            let check: crate::manager::setting_field::ActFn = std::rc::Rc::new(move |_, cx| {
                let path = inp.read(cx).value().trim().to_string();
                let env = env.clone();
                this.update(cx, |p, cx| p.check_file(env, path, cx)).ok();
            });
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(crate::manager::setting_field::field(st, input, set, crate::manager::setting_field::FileButton::Check(check), cx))
                .children(self.file_checks.get(&st.env).map(|r| match r {
                    Read::Loading => ui::pill(&theme, Tone::Neutral, "Checking…"),
                    Read::Got(true) => ui::pill(&theme, Tone::Good, "Found on the server"),
                    Read::Got(false) => ui::pill(&theme, Tone::Bad, "Not on the server"),
                    Read::Failed(_) => ui::pill(&theme, Tone::Warn, "Could not check"),
                }))
                .into_any_element()
        };
        let help = |st: &crate::catalog::Setting| {
            let default = if st.default.is_empty() { "SENTIENT's own".to_string() } else { st.default.clone() };
            Some(format!("{}\n\nVariable: {}. Built-in value: {default}.", st.help, st.env))
        };
        if self.settings_tab < 4 {
            let mut sec = local::section(&theme, local::TAB_TITLES[self.settings_tab], None);
            for key in local::TAB_KEYS[self.settings_tab] {
                let Some(st) = all.iter().find(|s| s.env == *key) else { continue };
                let field = field_of(st, cx);
                sec = sec.child(local::row(&theme, st.short_label(), help(st), changed.contains(&st.env), field));
            }
            col = col.child(sec);
        } else {
            let mut table = ui::panel(&theme).gap_0().child(local::table_header(&theme));
            for st in &all {
                let field = field_of(st, cx);
                table = table.child(local::table_row(
                    &theme,
                    st.short_label(),
                    help(st),
                    changed.contains(&st.env),
                    h_flex().gap_3().items_center().child(h_flex().w(px(local::VALUE_W)).flex_shrink_0().child(field)).child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(local::builtin_label(st))),
                ));
            }
            col = col.child(table);
        }
        // The same category list and heading as this computer's.
        h_flex()
            .gap_4()
            .items_start()
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(14.))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_size(px(17.)).font_weight(FontWeight::MEDIUM).child(local::TAB_TITLES[active.min(4)]))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(local::TAB_ABOUT[active.min(4)])),
                    )
                    .child(col),
            )
    }

    /// "Check" beside a file setting: does the server have that file?
    fn check_file(&mut self, env: String, path: String, cx: &mut Context<Self>) {
        if path.is_empty() {
            self.file_checks.remove(&env);
            cx.notify();
            return;
        }
        self.file_checks.insert(env.clone(), Read::Loading);
        let inst = self.inst.clone();
        self.fetch(cx, move || remote::file_exists(&inst, &path), move |p, r| {
            p.file_checks.insert(env, r);
        });
        cx.notify();
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

    /// The same panel as this computer's Migration: the type now, the
    /// types to change to as cards, the steps, then Change and its confirmation.
    fn migration_page(&self, cx: &mut Context<Self>) -> Div {
        use sentient_installer_core::stack::Flavor;
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let current = self.status.got().map(|s| s.database.clone()).unwrap_or_default();
        let all = [Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse];
        let now = all.into_iter().find(|f| crate::installation::flavor_label(*f) == current);
        let targets: Vec<Flavor> = all.into_iter().filter(|f| Some(*f) != now).collect();
        let chosen = self.target.and_then(|i| targets.get(i).copied());
        let this = cx.entity().downgrade();
        ui::panel(&theme)
            .child(ui::fact(&theme, "Database now", if current.is_empty() { "Reading…".to_string() } else { current.clone() }))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Change to"))
            .child(crate::manager::migration::target_cards(&theme, "r-", &targets, self.target, crate::remote::is_windows(&self.inst), std::rc::Rc::new(move |i, cx| {
                this.update(cx, |p, cx| {
                    if !p.job.read(cx).is_running() {
                        p.target = Some(i);
                        p.migrate_confirm = false;
                        cx.notify();
                    }
                })
                .ok();
            })))
            .child(crate::manager::migration::steps_text(&theme, false))
            .child(
                h_flex()
                    .gap_2()
                    .when(!self.migrate_confirm, |r| {
                        r.child(
                            crate::ui::button("r-migrate")
                                .primary()
                                .label(chosen.map(|f| format!("Change to {}", crate::installation::flavor_label(f))).unwrap_or_else(|| "Change database".into()))
                                .disabled(busy || chosen.is_none() || now.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.migrate_confirm = true;
                                    cx.notify();
                                })),
                        )
                    })
                    .when_some(chosen.filter(|_| self.migrate_confirm), |r, f| {
                        r.child(
                            crate::ui::button("r-migrate-confirm")
                                .danger()
                                .label("Stop SENTIENT and change now")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.migrate_confirm = false;
                                    this.run_job("Moving the database", vec!["migrate".into(), f.as_str().into()], cx);
                                })),
                        )
                        .child(crate::ui::button("r-migrate-no").ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.migrate_confirm = false;
                            cx.notify();
                        })))
                    }),
            )
    }

    fn repair_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().is_some_and(|s| s.installed);
        // The same panel as this computer's Repair. Uninstalling is done on
        // the server itself, so that panel is left out here.
        ui::panel(&theme)
            .child(div().font_weight(FontWeight::MEDIUM).child("Repair"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "For a SENTIENT that does not start or a service that went missing: the services and their configuration are written again from this installation's record, and SENTIENT is started. Data and settings are kept.",
            ))
            .child(h_flex().child(crate::ui::button("r-repair").outline().label("Repair").disabled(busy || !installed).on_click(cx.listener(|this, _, _, cx| this.run_job("Repairing", vec!["repair".into()], cx)))))
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
            return p.child(div().text_sm().text_color(theme.muted_foreground).child("No gateway on this server. Install it with the Platform Manager on the server itself, so its access token stays on the server."));
        }
        p = p
            .child(ui::fact(&theme, "Version", get("version")))
            .child(ui::fact(&theme, "Connects to", format!("{}:{}", get("host"), get("port"))))
            .child(ui::fact(&theme, "Its page", get("page")));
        let act = |id: &'static str, label: &'static str, action: &'static str| {
            crate::ui::button(id).outline().label(label).disabled(busy).on_click(cx.listener(move |this, _, _, cx| this.run_job("Gateway", vec!["gateway-control".into(), action.into()], cx)))
        };
        p.child(
            h_flex()
                .gap_2()
                .child(act("r-gw-start", "Start", "start"))
                .child(act("r-gw-stop", "Stop", "stop"))
                .child(act("r-gw-restart", "Restart", "restart"))
                .child(crate::ui::button("r-gw-update").primary().label("Update to the newest").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Updating the gateway", vec!["gateway-update".into()], cx)))),
        )
    }

    /// One calm panel for a server that is not set up or cannot be reached:
    /// what is the matter and the one thing that solves it.
    fn gate_panel(&self, e: String, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let not_set_up = e == remote::NOT_SET_UP;
        ui::panel(&theme)
            .child(div().font_weight(FontWeight::MEDIUM).child(if not_set_up { "Server agent not set up" } else { "The server does not answer" }))
            .child(div().text_sm().text_color(theme.muted_foreground).child(if not_set_up {
                "The Platform Manager works on a server through its server agent. Set up copies the matching agent over SSH and checks it; nothing else on the server changes.".to_string()
            } else {
                e.clone()
            }))
            .child(
                h_flex().gap_2().child(if not_set_up {
                    crate::ui::button("r-setup").primary().label("Set up").loading(busy).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.set_up(cx)))
                } else {
                    crate::ui::button("r-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| {
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
            Kind::Logs => ("Logs", "SENTIENT's log and its database's on this server, newest at the bottom."),
            Kind::Update => ("Update", "A newer SENTIENT for this server, from the update channel."),
            Kind::Backup => ("Backup and restore", "The backups on this server."),
            Kind::Settings => ("Platform settings", "SENTIENT's settings on this server."),
            Kind::Migration => ("Migration", "Move this server's SENTIENT to another database type."),
            Kind::Repair => ("Repair and uninstall", "Write the services and their configuration again on this server."),
            Kind::Gateway => ("Gateway", "The Sentient Gateway on this server."),
        };
        // Set up earlier but nothing added: add a lone SENTIENT found there,
        // once (it changes nothing on the server; the agent asks when the
        // choice is real).
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_REMOTE_STATUS").is_some() {
            self.auto_add_tried = true; // canned screens: no server to add on
        }
        if !self.auto_add_tried && matches!(&self.ready, Read::Got(())) && matches!(&self.status, Read::Got(s) if !s.installed) && !self.job.read(cx).is_running() {
            self.auto_add_tried = true;
            self.run_job("Looking for SENTIENT on the server", vec!["add-if-one".into()], cx);
        }
        let gate: Option<Div> = match &self.ready {
            Read::Loading => Some(Self::loading(&theme, "the server's state")),
            // While Set up runs or shows its result, only its panel.
            Read::Failed(_) if self.job_label.is_some() => Some(div()),
            Read::Failed(e) => Some(self.gate_panel(e.clone(), cx)),
            Read::Got(()) if self.kind != Kind::Status && self.kind != Kind::Gateway && matches!(&self.status, Read::Got(s) if !s.installed) => {
                Some(if self.job_label.is_some() { div() } else { self.not_added_panel(cx) })
            }
            Read::Got(()) => None,
        };
        let body = if let Some(g) = gate { g } else { match self.kind {
            Kind::Status => self.status_page(cx),
            Kind::Control => self.control_page(cx),
            Kind::Logs => self.logs_page(window, cx),
            Kind::Update => self.update_page(cx),
            Kind::Backup => self.backup_page(cx),
            Kind::Settings => self.settings_page(window, cx),
            Kind::Migration => self.migration_page(cx),
            Kind::Repair => self.repair_page(cx),
            Kind::Gateway => self.gateway_page(cx),
        } };
        let job = self.job_panel(cx);
        // Which server this is about shows in the bar under the top bar
        // (crate::manager), as in the mockup.
        ui::page(&theme, title, summary)
            .children(job)
            .child(body)
    }
}

/// Screens: a canned offer with two versions of notes.
#[cfg(debug_assertions)]
fn canned_offer() -> crate::updates::Offer {
    let n = |version: &str, date: &str, summary: &str, sections: Vec<(&str, Vec<&str>)>| crate::notes::Notes {
        version: version.into(),
        date: date.into(),
        summary: summary.into(),
        sections: sections.into_iter().map(|(k, v)| (k.to_string(), v.into_iter().map(String::from).collect())).collect(),
        database: None,
    };
    crate::updates::Offer {
        version: "4.4.0.2".into(),
        tag: "4.4.0.2".into(),
        date: "2026-11-14".into(),
        notes: String::new(),
        page: String::new(),
        installer: None,
        sha256: None,
        between: vec![
            n("4.4.0.2", "2026-11-14", "Request a demo or buy a licence from inside SENTIENT, and editor fixes.", vec![
                ("Added", vec!["Licence: Request demo licence and Buy licence open dialogs inside SENTIENT."]),
                ("Fixed", vec!["The fullscreen buttons of the JSON editor no longer submit the form they sit in.", "The licence limits on telemetry and attribute keys count the customer's keys only."]),
            ]),
            n("4.4.0.1", "2026-11-13", "First release of the 4.4 line.", vec![("Added", vec!["CoAP and LwM2M devices (DTLS, OSCORE).", "18 new widgets."])]),
        ],
    }
}
