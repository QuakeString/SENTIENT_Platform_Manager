// Copyright © 2016-2026 The SENTIENT Authors

//! Status: what is installed and whether each service is running. Refreshes
//! every few seconds while the screen is open. With nothing installed by the
//! Platform Manager, it lists the SENTIENT already running here (installed
//! some other way) and offers to manage it (crate::adopt).

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::adopt::Candidate;
use crate::installation::{InstallationRecord, Snapshot, flavor_label};
use crate::jobs::{Job, JobState};
use crate::manager::migration::RecordChanged;
use crate::ui::{self, Tone};

const REFRESH: Duration = Duration::from_secs(5);
/// How often the database details are read again by themselves.
const DB_REFRESH: Duration = Duration::from_secs(5 * 60);

pub struct StatusPage {
    record: Option<InstallationRecord>,
    snapshot: Option<Snapshot>,
    /// SENTIENT found running here when nothing is managed yet; None while
    /// looking.
    found: Option<Vec<Candidate>>,
    adopt_job: Entity<Job>,
    /// The gateway on this computer (None while looking).
    gateway: Option<sentient_installer_core::gateway::Status>,
    updates: Entity<crate::updates::UpdateState>,
    /// The databases' details (crate::dbstatus): None until first read.
    db: Option<crate::dbstatus::DbStatus>,
    db_reading: bool,
    db_read_at: Option<String>,
    last_backup: Option<crate::dbstatus::LastBackup>,
    _poll: Option<Task<()>>,
    _tasks: Vec<Task<()>>,
    _observe: Subscription,
}

impl EventEmitter<RecordChanged> for StatusPage {}

/// The gateway card's button: show the Gateway screen.
pub struct OpenGateway;
impl EventEmitter<OpenGateway> for StatusPage {}

impl StatusPage {
    pub fn new(record: Option<InstallationRecord>, updates: Entity<crate::updates::UpdateState>, cx: &mut Context<Self>) -> Self {
        let poll = record.as_ref().map(|record| {
            let platform = record.platform();
            cx.spawn(async move |this, cx| {
                loop {
                    let platform = platform.clone();
                    let snapshot = cx
                        .background_executor()
                        .spawn(async move { platform.snapshot() })
                        .await;
                    let alive = this
                        .update(cx, |page, cx| {
                            page.snapshot = Some(snapshot);
                            cx.notify();
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                    cx.background_executor().timer(REFRESH).await;
                }
            })
        });
        let adopt_job = cx.new(|_| Job::new());
        let _observe = cx.observe(&adopt_job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_)) {
                cx.emit(RecordChanged);
            }
            if matches!(job.read(cx).state(), JobState::Failed(_)) {
                page.look_for_existing(cx);
            }
            cx.notify();
        });
        let gateway_poll = cx.spawn(async move |this, cx| loop {
            let status = cx.background_executor().spawn(async { sentient_installer_core::gateway::status() }).await;
            if this.update(cx, |p, cx| { p.gateway = Some(status); cx.notify(); }).is_err() {
                break;
            }
            cx.background_executor().timer(REFRESH).await;
        });
        let mut page = Self {
            record,
            snapshot: None,
            found: None,
            adopt_job,
            gateway: None,
            updates,
            db: None,
            db_reading: false,
            db_read_at: None,
            last_backup: crate::dbstatus::last_backup(),
            _poll: poll,
            _tasks: vec![gateway_poll],
            _observe,
        };
        if page.record.is_none() {
            page.look_for_existing(cx);
        }
        // The databases are read now and every few minutes, not every few
        // seconds: their sizes change slowly. Where reading asks for the
        // administrator password (native Linux) only when asked.
        if page.record.as_ref().is_some_and(|r| !crate::dbstatus::needs_admin(r)) {
            let db_poll = cx.spawn(async move |this, cx| loop {
                if this.update(cx, |p, cx| p.read_db(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(DB_REFRESH).await;
            });
            page._tasks.push(db_poll);
        }
        page
    }

    /// Read the databases' details in the background.
    fn read_db(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if self.db_reading {
            return;
        }
        self.db_reading = true;
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let status = cx.background_executor().spawn(async move { crate::dbstatus::read(&record) }).await;
            this.update(cx, |page, cx| {
                page.db = Some(status);
                page.db_reading = false;
                page.db_read_at = Some(chrono::Local::now().format("%H:%M").to_string());
                page.last_backup = crate::dbstatus::last_backup();
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The Database panel: PostgreSQL, and ClickHouse on hybrid installs.
    fn db_panel(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, cx: &mut Context<Self>) -> Div {
        use crate::dbstatus::{CRITICAL_DISK, LOW_DISK, bytes_label, count_label, day_label, table_label};
        let header = h_flex()
            .gap_3()
            .items_center()
            .child(div().font_weight(FontWeight::MEDIUM).child("Database"))
            .child(div().flex_1())
            .children(self.db_read_at.clone().map(|t| {
                div().text_xs().text_color(theme.muted_foreground).child(format!("read at {t}"))
            }))
            .child(
                Button::new("db-read")
                    .outline()
                    .label(if self.db_reading { "Reading…" } else if self.db.is_none() { "Read" } else { "Refresh" })
                    .disabled(self.db_reading)
                    .on_click(cx.listener(|this, _, _, cx| this.read_db(cx))),
            );
        let mut panel = ui::panel(theme).child(header);
        let free_row = |label: &'static str, free: Option<u64>| -> Option<Div> {
            let free = free?;
            let tone = if free < CRITICAL_DISK { Some(Tone::Bad) } else if free < LOW_DISK { Some(Tone::Warn) } else { None };
            Some(match tone {
                None => ui::fact(theme, label, bytes_label(free)),
                Some(tone) => h_flex()
                    .gap_4()
                    .text_sm()
                    .child(div().w_56().flex_shrink_0().text_color(theme.muted_foreground).child(label))
                    .child(ui::pill(theme, tone, format!("{} free: low disk space", bytes_label(free)))),
            })
        };
        let Some(db) = &self.db else {
            return panel.child(div().text_sm().text_color(theme.muted_foreground).child(if self.db_reading {
                "Reading the database's size and state…"
            } else if crate::dbstatus::needs_admin(record) {
                "Read shows the database's size and state. It asks for the administrator password, as the database password is kept where only an administrator can read it."
            } else {
                "Not read yet."
            }));
        };
        panel = panel.child(div().pt_1().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child("POSTGRESQL"));
        match (&db.pg, &db.pg_error) {
            (Some(pg), _) => {
                panel = panel
                    .child(ui::fact(theme, "Version", match &pg.timescaledb {
                        Some(ts) => format!("PostgreSQL {}, TimescaleDB {ts}", pg.version),
                        None => format!("PostgreSQL {}", pg.version),
                    }))
                    .child(ui::fact(theme, "Database size", bytes_label(pg.db_bytes)))
                    .children(free_row("Free disk", pg.free_bytes))
                    .child(ui::fact(theme, "Connections", format!("{} open of {} allowed", pg.connections, pg.max_connections)))
                    .child(ui::fact(theme, "Largest tables", pg.tables.iter().map(|t| match table_label(&t.name) {
                        Some(what) => format!("{} ({what}) {}", t.name, bytes_label(t.bytes)),
                        None => format!("{} {}", t.name, bytes_label(t.bytes)),
                    }).collect::<Vec<_>>().join(" · ")));
            }
            (None, Some(e)) => panel = panel.child(ui::outcome(theme, Tone::Warn, format!("Could not read PostgreSQL: {e}"))),
            (None, None) => {}
        }
        panel = panel.child(ui::fact(theme, "Last backup", match &self.last_backup {
            Some(b) => format!(
                "{}{}, {}",
                crate::dbstatus::ago_label(&b.at, chrono::Local::now()),
                b.bytes.map(|n| format!(", {}", bytes_label(n))).unwrap_or_default(),
                b.file
            ),
            None => "None made from the Platform Manager".to_string(),
        }));
        if db.hybrid {
            panel = panel.child(div().pt_2().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child("CLICKHOUSE"));
            match (&db.ch, &db.ch_error) {
                (Some(ch), _) => {
                    panel = panel
                        .child(ui::fact(theme, "Version", format!("ClickHouse {}", ch.version)))
                        .child(ui::fact(theme, "Disk used", bytes_label(ch.disk_bytes)))
                        .children(free_row("Free disk", ch.free_bytes))
                        .child(ui::fact(theme, "Telemetry history", match (ch.first_ms, ch.last_ms) {
                            (Some(a), Some(b)) => format!("{} rows, {} to {}", count_label(ch.rows), day_label(a), day_label(b)),
                            _ => format!("{} rows", count_label(ch.rows)),
                        }));
                }
                (None, Some(e)) => panel = panel.child(ui::outcome(theme, Tone::Warn, format!("Could not read ClickHouse: {e}"))),
                (None, None) => {}
            }
        }
        panel
    }

    fn look_for_existing(&mut self, cx: &mut Context<Self>) {
        self.found = None;
        let task = cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { crate::adopt::find() }).await;
            this.update(cx, |page, cx| {
                page.found = Some(found);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn confirm_adopt(&mut self, candidate: Candidate, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let text = format!(
            "The Platform Manager checks it can reach this instance's databases, then manages it from now on: status, start and stop, logs, updates, backups and the rest. \
             Nothing is reinstalled, moved or restarted. {}",
            if candidate.record.stack.external.is_some() {
                "It keeps running from its own compose file."
            } else if candidate.sets_admin_password {
                "The Platform Manager does not know the PostgreSQL admin (postgres) password, which updates need: it sets a new one and keeps it in its secret store. Anything else that signs in to PostgreSQL as postgres will need the new password."
            } else {
                ""
            }
        );
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let candidate = candidate.clone();
            dialog.title("Add this existing instance?").description(text.clone()).confirm().ok_text("Add instance").on_ok(move |_, _, cx| {
                let candidate = candidate.clone();
                this.update(cx, |page, cx| {
                    page.adopt_job.update(cx, |job, cx| job.start(move |sink| crate::adopt::adopt_as_needed(&candidate, None, sink), cx));
                })
                .ok();
                true
            })
        });
    }

    fn existing(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let job = self.adopt_job.read(cx);
        let busy = job.is_running();
        let failed = match job.state() {
            JobState::Failed(m) => Some(m.clone()),
            _ => None,
        };
        let step = job.step_with_time();
        let mut panel = ui::panel(theme).child(div().font_weight(FontWeight::MEDIUM).child("Add existing instance"));
        let Some(found) = &self.found else {
            return panel.child(div().text_sm().text_color(theme.muted_foreground).child("Looking for SENTIENT instances on this computer…"));
        };
        if found.is_empty() {
            return panel.child(div().text_sm().text_color(theme.muted_foreground).child(
                "No existing SENTIENT instance was found on this computer. One started some other way (our compose files, an older installer) shows here, so the Platform Manager can manage it.",
            ));
        }
        panel = panel
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "SENTIENT instances already on this computer. Add one and the Platform Manager manages it as it is, without reinstalling anything.",
            ))
            .when(busy, |p| p.child(div().text_sm().child(step)))
            .children(failed.map(|m| ui::outcome(theme, Tone::Bad, m)));
        for (i, c) in found.iter().enumerate() {
            let candidate = c.clone();
            panel = panel.child(
                div()
                    .mt_2()
                    .p_3()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    .children(c.facts().into_iter().map(|(label, value)| ui::fact(theme, label, value)))
                    .children(c.notes.iter().map(|n| div().text_xs().text_color(theme.muted_foreground).child(n.clone())))
                    .children(c.problem.clone().map(|p| ui::outcome(theme, Tone::Warn, format!("Cannot be added: {p}"))))
                    .child(
                        h_flex().mt_2().child(
                            Button::new(("adopt", i))
                                .primary()
                                .label("Add this instance…")
                                .disabled(busy || c.problem.is_some())
                                .on_click(cx.listener(move |this, _, window, cx| this.confirm_adopt(candidate.clone(), window, cx))),
                        ),
                    ),
            );
        }
        panel
    }
}

impl Render for StatusPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(&theme, "Status", "Services, version, database type and ports.");
        let Some(record) = &self.record else {
            let existing = self.existing(&theme, cx);
            let gw = self.gateway_card(&theme, cx);
            return page.child(ui::not_installed(&theme)).child(existing).child(gw);
        };

        let (overall, tone) = match &self.snapshot {
            None => ("Checking…", Tone::Neutral),
            Some(s) if s.serving => ("SENTIENT is running", Tone::Good),
            Some(s) if s.services.iter().any(|svc| svc.running) => ("Starting or partly stopped", Tone::Warn),
            Some(s) if s.services.iter().any(|svc| svc.starting) => ("SENTIENT is starting with Windows", Tone::Warn),
            Some(_) => ("SENTIENT is stopped", Tone::Bad),
        };
        let services = self.snapshot.as_ref().map(|s| s.services.clone()).unwrap_or_default();
        let st = &record.stack;

        page.child(
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(ui::pill(&theme, tone, overall))
                        .child(div().flex_1())
                        .child(
                            Button::new("open")
                                .primary()
                                .label("Open SENTIENT")
                                .on_click({
                                    let record = record.clone();
                                    // Asked on click: whether the kiosk shortcut is there reads the disk.
                                    move |_, _, cx| ui::open_sentient(record.stack.http_port, record.opens_as_kiosk(), cx)
                                }),
                        ),
                )
                .child(ui::fact(&theme, "SENTIENT version", record.sentient_version.clone()))
                .child(ui::fact(&theme, "Database", flavor_label(record.flavor())))
                .child(ui::fact(
                    &theme,
                    "Runs as",
                    match record.engine {
                        crate::installation::Engine::Native if cfg!(windows) => "Windows services",
                        crate::installation::Engine::Native => "systemd services",
                        crate::installation::Engine::Docker => "Docker containers",
                    },
                ))
                .child(ui::fact(&theme, "Address", record.url()))
                .child(ui::fact(
                    &theme,
                    "Ports",
                    format!(
                        "Web {} · MQTT {} / {} · CoAP {} / {} · PostgreSQL {}",
                        st.http_port, st.mqtt_port, st.mqtts_port, st.coap_port, st.coaps_port, st.pg_port
                    ),
                ))
                .child(match &st.external {
                    Some(ext) => ui::fact(&theme, "Runs from", format!("its own compose file ({})", ext.files.join(", "))),
                    None => ui::fact(&theme, "Installed in", record.install_dir.display().to_string()),
                }),
        )
        .child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Services"))
                .children(services.into_iter().map(|svc| {
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(ui::pill(
                            &theme,
                            if svc.running { Tone::Good } else if svc.starting { Tone::Warn } else { Tone::Bad },
                            if svc.running { "Running" } else if svc.starting { "Starting" } else { "Stopped" },
                        ))
                        .child(div().w_48().child(svc.name))
                        .child(div().text_color(theme.muted_foreground).child(svc.detail))
                })),
        )
        .child(self.db_panel(&theme, record, cx))
        .child(self.gateway_card(&theme, cx))
    }
}

impl StatusPage {
    /// The gateway on this computer, in one line, with the way to the
    /// Gateway screen (install, update, control).
    fn gateway_card(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let newer = self.updates.read(cx).gateway_notice();
        let (tone, pill, line): (Tone, &str, String) = match &self.gateway {
            None => (Tone::Neutral, "Checking…", String::new()),
            Some(g) if !g.installed => (Tone::Neutral, "Not installed", "No Sentient Gateway on this computer.".into()),
            Some(g) => (
                if g.running { Tone::Good } else { Tone::Bad },
                if g.running { "Running" } else { "Stopped" },
                format!(
                    "Sentient Gateway {}{}{}",
                    g.version.clone().unwrap_or_default(),
                    g.host.as_ref().map(|h| format!(" · sends to {h}{}", g.port.map(|p| format!(":{p}")).unwrap_or_default())).unwrap_or_default(),
                    newer.as_ref().map(|o| format!(" · {} available", o.version)).unwrap_or_default()
                ),
            ),
        };
        let installed = self.gateway.as_ref().is_some_and(|g| g.installed);
        ui::panel(theme)
            .child(div().font_weight(FontWeight::MEDIUM).child("Gateway"))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .text_sm()
                    .child(ui::pill(theme, tone, pill))
                    .child(div().flex_1().text_color(theme.muted_foreground).child(line))
                    .child(
                        Button::new("open-gateway")
                            .outline()
                            .label(if installed { "Manage" } else { "Install…" })
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenGateway))),
                    ),
            )
    }
}
