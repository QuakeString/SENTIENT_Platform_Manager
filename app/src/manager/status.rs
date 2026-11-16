// Copyright © 2016-2026 The SENTIENT Authors

//! Status: what is installed and whether each service is running, with
//! Start, Stop and Restart for SENTIENT and for each service. Refreshes
//! every few seconds while the screen is open. With nothing installed by the
//! Platform Manager, it lists the SENTIENT already running here (installed
//! some other way) and offers to manage it (crate::adopt).

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
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
    /// SENTIENT's or one service's start, stop or restart.
    job: Entity<Job>,
    /// The service whose own action runs now.
    svc_busy: Option<String>,
    /// SENTIENT's version as shown ("4.4.0.2 (image tag latest)").
    shown_version: Option<String>,
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
    _job_observe: Subscription,
}

impl EventEmitter<RecordChanged> for StatusPage {}

/// The gateway card's button: show the Gateway screen.
pub struct OpenGateway;
impl EventEmitter<OpenGateway> for StatusPage {}

/// "Apply…" on the line about changes waiting for this installation.
pub struct OpenUpdate;
impl EventEmitter<OpenUpdate> for StatusPage {}

/// A button that leads to another screen.
#[derive(Clone, Copy)]
pub enum Go {
    Backup,
    Settings,
    Migration,
}
impl EventEmitter<Go> for StatusPage {}

/// "Disk C:" on Windows, "The database's disk" elsewhere.
fn disk_name(record: &InstallationRecord) -> String {
    let p = record.pg_dir.display().to_string();
    match p.chars().next() {
        Some(c) if cfg!(windows) && p.chars().nth(1) == Some(':') => format!("Disk {}:", c.to_ascii_uppercase()),
        _ => "The database's disk".into(),
    }
}

/// A narrow label / value row for the two database columns.
fn small_fact(theme: &gpui_kit::component::Theme, label: &'static str, value: impl Into<SharedString>) -> Div {
    h_flex()
        .gap_2()
        .text_sm()
        .child(div().w_32().flex_shrink_0().text_color(theme.muted_foreground).child(label))
        .child(div().flex_1().min_w_0().child(value.into()))
}

/// The history's period: a start before 2000 (a reading stored with time 0,
/// 1970, when a device sent no time) is not shown as the date; the (i) says
/// such readings exist.
fn period_fact(theme: &gpui_kit::component::Theme, key: &'static str, first: i64, last: i64) -> Div {
    if first >= crate::dbstatus::VALID_FROM_MS {
        return small_fact(theme, "Period", format!("{} to {}", crate::dbstatus::day_label(first), crate::dbstatus::day_label(last)));
    }
    small_fact(theme, "Period", format!("up to {}", crate::dbstatus::day_label(last))).child(ui::info(
        SharedString::from(format!("period-{key}")),
        "Some readings carry a time before 2000, most likely 1970: a device sent its data without a time. They are kept, but the period's start is not shown from them.".into(),
    ))
}

/// "Audit log  who did what  [bar]  255 MB"
fn table_row(theme: &gpui_kit::component::Theme, name: String, hint: String, fraction: f32, size: String) -> Div {
    h_flex()
        .gap_2()
        .items_center()
        .text_sm()
        .child(
            // The name and its description stay inside their column: the
            // description is cut short, never drawn over the bar.
            h_flex()
                .w(px(270.))
                .min_w(px(120.))
                .flex_shrink(1.)
                .overflow_hidden()
                .whitespace_nowrap()
                .gap_1p5()
                .items_baseline()
                .child(div().flex_shrink_0().child(name))
                .when(!hint.is_empty(), |r| r.child(div().min_w_0().overflow_hidden().text_ellipsis().text_size(px(13.)).text_color(theme.muted_foreground).child(hint))),
        )
        .child(
            div().flex_1().min_w(px(40.)).child(ui::segment_bar(theme.secondary, 6., &[(theme.primary, fraction.clamp(0.02, 1.0))])),
        )
        .child(div().w(px(64.)).flex_shrink_0().text_right().child(size))
}

impl StatusPage {
    pub fn new(record: Option<InstallationRecord>, updates: Entity<crate::updates::UpdateState>, cx: &mut Context<Self>) -> Self {
        let poll = record.as_ref().map(|record| {
            let platform = record.platform();
            let rec = record.clone();
            cx.spawn(async move |this, cx| {
                loop {
                    let platform = platform.clone();
                    let rec = rec.clone();
                    let (snapshot, version) = cx
                        .background_executor()
                        .spawn(async move { (platform.snapshot(), crate::headless::shown_version(&rec)) })
                        .await;
                    let alive = this
                        .update(cx, |page, cx| {
                            page.snapshot = Some(snapshot);
                            page.shown_version = Some(version);
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
        let job = cx.new(|_| Job::new());
        let _job_observe = cx.observe(&job, |page: &mut Self, job, cx| {
            // An action ended: read the services again at once.
            if !job.read(cx).is_running() && matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                page.svc_busy = None;
                page.refresh(cx);
            }
            cx.notify();
        });
        let gateway_poll = cx.spawn(async move |this, cx| loop {
            let status = cx.background_executor().spawn(async { crate::manager::gateway::local_status() }).await;
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
            job,
            svc_busy: None,
            shown_version: None,
            gateway: None,
            updates,
            db: None,
            db_reading: false,
            db_read_at: None,
            last_backup: crate::dbstatus::last_backup(),
            _poll: poll,
            _tasks: vec![gateway_poll],
            _observe,
            _job_observe,
        };
        if page.record.is_none() {
            page.look_for_existing(cx);
        }
        // The databases are read now and every few minutes, not every few
        // seconds: their sizes change slowly. Where reading asks for the
        // administrator password (native Linux) only when asked.
        // Screens: a canned reading needs no password.
        let canned = cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_DBSTATUS").is_some();
        if page.record.as_ref().is_some_and(|r| canned || !crate::dbstatus::needs_admin(r)) {
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

    /// Switch timing (SPM_DEBUG_SWITCH): the page shows what it read.
    pub(crate) fn has_data(&self) -> bool {
        self.snapshot.is_some() || (self.record.is_none() && self.found.is_some())
    }

    /// Read the services now (after an action, or shown again), apart from
    /// the poll.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let task = cx.spawn(async move |this, cx| {
            let snapshot = cx.background_executor().spawn(async move { record.platform().snapshot() }).await;
            this.update(cx, |page, cx| {
                page.snapshot = Some(snapshot);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Start, stop or restart SENTIENT (also the tray's and the taskbar's).
    pub(crate) fn run(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let Some(record) = &self.record else { return };
        let platform = record.platform();
        self.job.update(cx, |job, cx| {
            job.set_label(match action {
                "start" => "Starting SENTIENT",
                "stop" => "Stopping SENTIENT",
                _ => "Restarting SENTIENT",
            });
            job.start(move |sink| platform.control(sink, action), cx)
        });
        cx.notify();
    }

    /// One service's own start, stop or restart.
    fn run_service(&mut self, name: String, action: &'static str, cx: &mut Context<Self>) {
        let Some(record) = &self.record else { return };
        let platform = record.platform();
        self.svc_busy = Some(name.clone());
        let label = service_job_label(&name, action);
        self.job.update(cx, |job, cx| {
            job.set_label(label);
            job.start(move |sink| platform.service_control(sink, &name, action), cx)
        });
        cx.notify();
    }

    /// Read the databases' details in the background.
    fn read_db(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if self.db_reading {
            return;
        }
        self.db_reading = true;
        cx.notify();
        // Read is pressed (the timer reads only what needs no password).
        // Once the account only has to sign in again, it does not ask.
        let ask = !crate::dbstatus::after_sign_in(&record);
        let task = cx.spawn(async move |this, cx| {
            let status = cx
                .background_executor()
                .spawn(async move {
                    let _action = ask.then(sentient_installer_core::elevation::user_action);
                    crate::dbstatus::read(&record)
                })
                .await;
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

    /// The Database panel: who uses the disk and how fast it fills, then
    /// PostgreSQL and (on hybrid installs) ClickHouse side by side.
    fn db_panel(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, cx: &mut Context<Self>) -> Div {
        use crate::dbstatus::bytes_label;
        let header = h_flex()
            .gap_3()
            .items_center()
            .child(div().font_weight(FontWeight::MEDIUM).child("Database"))
            .child(div().flex_1())
            .children(self.db_read_at.clone().map(|t| {
                div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!("read at {t}"))
            }))
            .child(
                crate::ui::button("db-read")
                    .outline()
                    .label(if self.db_reading { "Reading…" } else if self.db.is_none() { "Read" } else { "Refresh" })
                    .disabled(self.db_reading)
                    .on_click(cx.listener(|this, _, _, cx| this.read_db(cx))),
            );
        let panel = ui::panel(theme).child(header);
        let Some(db) = &self.db else {
            return panel.child(div().text_sm().text_color(theme.muted_foreground).child(if self.db_reading {
                "Reading the database's size and state…"
            } else if crate::dbstatus::after_sign_in(record) {
                "The database's size and state are available after you sign in again."
            } else if crate::dbstatus::needs_admin(record) {
                "Read shows the database's size and state. It asks for the administrator password once; after a Repair it reads without one."
            } else {
                "Not read."
            }));
        };
        let this = cx.entity().downgrade();
        let go = move |to: Go| -> std::rc::Rc<dyn Fn(&mut App)> {
            let this = this.clone();
            std::rc::Rc::new(move |cx| {
                this.update(cx, |_, cx| cx.emit(to)).ok();
            })
        };
        let last_backup = match &self.last_backup {
            Some(b) => format!("{}{}", crate::dbstatus::ago_label(&b.at, chrono::Local::now()), b.bytes.map(|n| format!(", {}", bytes_label(n))).unwrap_or_default()),
            None => "None".to_string(),
        };
        db_details(
            theme,
            panel,
            db,
            &disk_name(record),
            Some(crate::dbstatus::growth_per_day()),
            last_backup,
            DbActions {
                move_data: (crate::movedata::unsupported(record).is_none()).then(|| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
                    std::rc::Rc::new(|window, cx| crate::manager::movedata::open(crate::movedata::Target::Local, "This computer", None, window, cx))
                }),
                set_limit: Some(go(Go::Settings)),
                remove_kept: Some(go(Go::Migration)),
                back_up: Some(go(Go::Backup)),
                broken_parts: Some({
                    let r = record.clone();
                    std::rc::Rc::new(move |cx: &mut App| {
                        let (r1, r2) = (r.clone(), r.clone());
                        BrokenParts::open(
                            std::sync::Arc::new(move || crate::dbstatus::detached_parts(&r1)),
                            std::sync::Arc::new(move |attach, parts| {
                                let res = if attach { crate::dbstatus::attach_parts(&r2, &parts) } else { crate::dbstatus::drop_parts(&r2, &parts) }?;
                                Ok(res.into_iter().map(|(p, r)| match r {
                                    Ok(()) => format!("ok {p}"),
                                    Err(e) => format!("failed {p}: {}", e.lines().next().unwrap_or("")),
                                }).collect::<Vec<_>>().join("\n"))
                            }),
                            cx,
                        )
                    })
                }),
            },
            crate::theme::is_dark(cx),
        )
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
                "It runs from a compose file the Platform Manager did not write."
            } else if candidate.sets_admin_password {
                "The Platform Manager does not know the PostgreSQL admin (postgres) password, which updates need: it sets a new one and keeps it in its secret store. Anything else that signs in to PostgreSQL as postgres will need the new password."
            } else {
                ""
            }
        );
        crate::modal::alert(window, cx, move |dialog, _, _| {
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
                "No existing SENTIENT instance was found on this computer. One started some other way (SENTIENT compose files, an older installer) is listed in this panel, so the Platform Manager can manage it.",
            ));
        }
        panel = panel
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().text_sm().text_color(theme.muted_foreground).child("Found on this computer."))
                    .child(ui::info("existing-help".into(), "Add one and the Platform Manager manages it as it is, without reinstalling anything.".into())),
            )
            .when(busy, |p| p.child(div().text_sm().child(step)))
            .children(failed.map(|m| ui::outcome(theme, Tone::Bad, m)));
        for (i, c) in found.iter().enumerate() {
            let candidate = c.clone();
            let from = c.facts().into_iter().find(|(l, _)| *l == "From").map(|(_, v)| v).unwrap_or_default();
            let mut help = format!("Started from: {from}");
            for n in &c.notes {
                help.push_str("\n\n");
                help.push_str(n);
            }
            let missing = c.notes.iter().any(|n| n.contains("no longer at"));
            panel = panel.child(
                div()
                    .mt_2()
                    .p_3()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    // The essentials; where it came from and the notes behind an
                    // (i); a short amber line only when its compose file is gone.
                    .children(c.facts().into_iter().filter(|(l, _)| ["Runs as", "Version", "Database type", "State", "Web port"].contains(l)).map(|(label, value)| ui::fact(theme, label, value)))
                    .when(missing, |d| d.child(div().mt_1().text_sm().text_color(if theme.mode.is_dark() { theme.warning_foreground } else { gpui::rgb(0xb45309).into() }).child("Compose file missing")))
                    .children(c.problem.clone().map(|p| ui::outcome(theme, Tone::Warn, format!("Cannot be added: {p}"))))
                    .child(
                        h_flex()
                            .mt_2()
                            .gap_2()
                            .items_center()
                            .child(
                                crate::ui::button(("adopt", i))
                                    .primary()
                                    .label("Add this instance…")
                                    .disabled(busy || c.problem.is_some())
                                    .on_click(cx.listener(move |this, _, window, cx| this.confirm_adopt(candidate.clone(), window, cx))),
                            )
                            .child(ui::info(SharedString::from(format!("existing-{i}")), help.into())),
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
            // "Add existing instance" only when one was found, with its Add
            // button; nothing while looking or when there is none.
            let existing = self.found.as_ref().is_some_and(|f| !f.is_empty()).then(|| self.existing(&theme, cx));
            let gw = self.gateway_card(&theme, cx);
            return page.child(ui::not_installed(&theme)).children(existing).child(gw);
        };

        let (overall, tone) = match &self.snapshot {
            None => ("Checking…", Tone::Neutral),
            // Its port answers but none of its own services runs: some
            // other program is serving there.
            Some(s) if s.serving && !s.services.is_empty() && !s.services.iter().any(|svc| svc.running || svc.starting) => ("Another program answers on SENTIENT's port", Tone::Warn),
            Some(s) if s.serving => ("SENTIENT is running", Tone::Good),
            Some(s) if s.services.iter().any(|svc| svc.running) => ("Starting or partly stopped", Tone::Warn),
            Some(s) if s.services.iter().any(|svc| svc.starting) => ("SENTIENT is starting with Windows", Tone::Warn),
            Some(_) => ("SENTIENT is stopped", Tone::Bad),
        };
        let stopped = self.snapshot.as_ref().is_some_and(|s| !s.serving);
        let services = self.snapshot.as_ref().map(|s| s.services.clone()).unwrap_or_default();
        let st = &record.stack;
        // The newest backup in the backup folder failed its check.
        let damaged = crate::backups::list_all(&crate::backups::BackupPrefs::load())
            .into_iter()
            .next()
            .filter(|e| e.check.as_ref().is_some_and(|c| !c.ok));
        let page = page.when_some(damaged, |page, e| {
            page.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .px_4()
                    .py_2()
                    .rounded(theme.radius)
                    .bg(theme.danger.opacity(0.14))
                    .text_color(theme.danger)
                    .text_sm()
                    .child(div().flex_1().child(format!(
                        "The newest backup, {}, is damaged: {}",
                        e.name,
                        e.check.map(|c| c.summary).unwrap_or_default()
                    )))
                    .child(crate::ui::button("backup-damaged").outline().label("Back up now").on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Backup)))),
            )
        });
        // B7: changes from a newer Platform Manager, waiting quietly.
        let waiting = crate::changes::pending(record).len();
        let page = page.when(waiting > 0, |page| {
            page.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .px_4()
                    .py_2()
                    .rounded(theme.radius)
                    .bg(theme.accent)
                    .text_sm()
                    .child(div().flex_1().child(format!(
                        "{waiting} change{} from Platform Manager {} {} waiting.",
                        if waiting == 1 { "" } else { "s" },
                        crate::updates::manager_version(),
                        if waiting == 1 { "is" } else { "are" }
                    )))
                    .child(crate::ui::button("changes-waiting").outline().label("Apply…").on_click(cx.listener(|_, _, _, cx| cx.emit(OpenUpdate)))),
            )
        });

        let busy = self.job.read(cx).is_running();
        let running = self.snapshot.as_ref().filter(|s| !s.services.is_empty()).map(|s| s.serving);
        let this = cx.entity().downgrade();
        let (t1, t2, t3) = (this.clone(), this.clone(), this.clone());
        let sentient = ui::panel(&theme)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_3()
                    .items_center()
                    .child(ui::product_logo(ui::Product::Sentient, 30.))
                    .child(ui::pill(&theme, tone, overall))
                    .child(div().flex_1())
                    .child(h_flex().ml_auto().child(ui::open_sentient_button("open", !stopped, {
                        let record = record.clone();
                        // Asked on click: whether the kiosk shortcut is there reads the disk.
                        move |_, _, cx| ui::open_sentient(record.stack.http_port, record.opens_as_kiosk(), cx)
                    }))),
            )
            .child(ui::card_fact(&theme, "SENTIENT version", self.shown_version.clone().unwrap_or_else(|| record.sentient_version.clone())))
            .child(ui::card_fact(&theme, "Database", flavor_label(record.flavor())))
            .child(ui::card_fact(
                &theme,
                "Runs as",
                match record.engine {
                    crate::installation::Engine::Native if cfg!(windows) => "Windows services",
                    crate::installation::Engine::Native => "systemd services",
                    crate::installation::Engine::Docker => "Docker containers",
                },
            ))
            .child(ui::card_fact(&theme, "Address", record.url()))
            .child(ui::card_fact(
                &theme,
                "Ports",
                format!(
                    "Web {} · MQTT {} / {} · CoAP {} / {} · PostgreSQL {}",
                    st.http_port, st.mqtt_port, st.mqtts_port, st.coap_port, st.coaps_port, st.pg_port
                ),
            ))
            .child(match &st.external {
                // A temporary override that is gone since is left out: nothing needs it.
                Some(ext) => {
                    let gone = st.vanished_files();
                    let files: Vec<&String> = ext.files.iter().filter(|f| !gone.contains(f)).collect();
                    ui::card_fact(&theme, "Runs from", format!("a compose file the Platform Manager did not write ({})", files.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", ")))
                }
                None => ui::card_fact(&theme, "Installed in", record.install_dir.display().to_string()),
            })
            .children(ui::card_foot(
                &theme,
                control_buttons(
                    &theme,
                    "",
                    busy,
                    running,
                    std::rc::Rc::new(move |_, cx| {
                        t1.update(cx, |p, cx| p.run("start", cx)).ok();
                    }),
                    std::rc::Rc::new(move |_, cx| {
                        t2.update(cx, |p, cx| p.run("restart", cx)).ok();
                    }),
                    std::rc::Rc::new(move |w, cx| {
                        let t3 = t3.clone();
                        confirm_stop(w, cx, std::rc::Rc::new(move |cx| {
                            t3.update(cx, |p, cx| p.run("stop", cx)).ok();
                        }));
                    }),
                ),
            ));
        let act = service_act(self.snapshot.as_ref().is_some_and(|s| s.serving), std::rc::Rc::new(move |name, action, cx| {
            this.update(cx, |p, cx| p.run_service(name, action, cx)).ok();
        }));
        let mut services_card = ui::panel(&theme).gap_2().child(services_title());
        match &self.snapshot {
            None => services_card = services_card.children(ui::skeleton_service_rows(&theme, "svc", 3)),
            Some(_) => {
                let up = services.iter().filter(|s| s.running).count();
                let total = services.len();
                services_card = services_card
                    .children(ui::service_rows(&theme, services.into_iter().map(|svc| (svc.name.into(), svc.running, svc.starting, svc.detail.into())), (act, service_log(), self.svc_busy.clone())))
                    .children(ui::card_foot(&theme, ui::services_foot(&theme, up, total)));
            }
        }

        page.child(ui::card_pair(sentient, services_card))
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
                    g.version.clone().unwrap_or_else(|| "(version unknown)".into()),
                    g.host.as_ref().map(|h| format!(" · sends to {h}{}", g.port.map(|p| format!(":{p}")).unwrap_or_default())).unwrap_or_default(),
                    newer.as_ref().map(|o| format!(" · {} available", o.version)).unwrap_or_default()
                ),
            ),
        };
        let installed = self.gateway.as_ref().is_some_and(|g| g.installed);
        ui::panel(theme)
            .child(h_flex().gap_2().items_center().child(ui::product_logo(ui::Product::Gateway, 24.)).child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway")))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .text_sm()
                    .child(ui::pill(theme, tone, pill).flex_shrink_0())
                    .child(div().flex_1().min_w_0().text_color(theme.muted_foreground).child(line))
                    .child(
                        crate::ui::button("open-gateway")
                            .outline()
                            .label(if installed { "Manage" } else { "Install…" })
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenGateway))),
                    ),
            )
    }
}

/// "Restarting Database (PostgreSQL)": a service action's job label.
pub(crate) fn service_job_label(name: &str, action: &str) -> String {
    let plain = ui::service_label(name).unwrap_or(name);
    format!(
        "{} {plain}",
        match action {
            "start" => "Starting",
            "stop" => "Stopping",
            _ => "Restarting",
        }
    )
}

/// The Services card's title, as tall as the SENTIENT card's header row.
pub(crate) fn services_title() -> Div {
    h_flex().min_h(px(32.)).items_center().child(div().font_weight(FontWeight::MEDIUM).child("Services"))
}

/// A service row's Logs: the Logs page on that service's source.
pub(crate) fn service_log() -> ui::ServiceLog {
    std::rc::Rc::new(|name, _, cx| {
        crate::logsource::request(crate::logsource::of_service(name));
        cx.refresh_windows();
    })
}

/// A service row's Start, Stop and Restart, through `run`. Stopping or
/// restarting a database while SENTIENT serves is confirmed first. The same
/// here and on a server.
pub(crate) fn service_act(serving: bool, run: std::rc::Rc<dyn Fn(String, &'static str, &mut App)>) -> ui::ServiceAct {
    std::rc::Rc::new(move |name, action, window, cx| {
        let name = name.to_string();
        let plain = ui::service_label(&name).unwrap_or(&name).to_string();
        let go: std::rc::Rc<dyn Fn(&mut App)> = std::rc::Rc::new({
            let (run, name) = (run.clone(), name.clone());
            move |cx| run(name.clone(), action, cx)
        });
        let n = name.to_lowercase();
        if (n.contains("postgres") || n.contains("clickhouse")) && serving && action != "start" {
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
    })
}

/// Stopping takes every device connection down, so it is confirmed. The
/// same here and on a server.
pub(crate) fn confirm_stop(window: &mut Window, cx: &mut App, stop: std::rc::Rc<dyn Fn(&mut App)>) {
    crate::modal::alert(window, cx, move |dialog, _, _| {
        let stop = stop.clone();
        dialog
            .title("Stop SENTIENT?")
            .description("Devices lose their connection and dashboards stop updating until SENTIENT is started again.")
            .confirm()
            .ok_text("Stop")
            .on_ok(move |_, _, cx| {
                stop(cx);
                true
            })
    });
}

/// The SENTIENT card's Start, Stop… and Restart, in that order here and on
/// a server. Start is the primary action while SENTIENT is stopped (Open
/// SENTIENT is while it runs); actions that do not apply are off; Stop
/// hovers red. `running`: None while not known.
pub(crate) fn control_buttons(
    theme: &gpui_kit::component::Theme,
    id: &str,
    busy: bool,
    running: Option<bool>,
    start: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    restart: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    stop: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
) -> Div {
    use gpui_kit::component::{Icon, IconName};
    let up = running == Some(true);
    let down = running == Some(false);
    let btn = |key: &str, label: &'static str, icon: IconName, primary: bool, on: bool| {
        let b = crate::ui::button(SharedString::from(format!("{id}{key}"))).label(label).icon(Icon::new(icon)).disabled(busy || running.is_none() || !on);
        if primary { b.primary() } else { b.outline() }
    };
    let (bg, border, fg) = ui::stop_hover(theme);
    h_flex()
        .flex_wrap()
        .gap_2()
        .child(btn("start", "Start", IconName::Play, down, !up).on_click(move |_, w, cx| start(w, cx)))
        .child(btn("stop", "Stop…", IconName::Square, false, !down).hover_colors(bg, border, fg).on_click(move |_, w, cx| stop(w, cx)))
        .child(btn("restart", "Restart", IconName::RotateCw, false, !down).on_click(move |_, w, cx| restart(w, cx)))
}

/// What the database details may offer, each where the page can do it.
pub(crate) struct DbActions {
    pub set_limit: Option<std::rc::Rc<dyn Fn(&mut App)>>,
    pub remove_kept: Option<std::rc::Rc<dyn Fn(&mut App)>>,
    pub back_up: Option<std::rc::Rc<dyn Fn(&mut App)>>,
    /// Open the list of ClickHouse parts set aside.
    pub broken_parts: Option<std::rc::Rc<dyn Fn(&mut App)>>,
    /// Open "Move data to another disk" (the low-disk warning's first way out).
    pub move_data: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
}

/// Reads the parts set aside; restores (true) or deletes them.
pub(crate) type PartsLoad = std::sync::Arc<dyn Fn() -> Result<Vec<crate::dbstatus::DetachedPart>, String> + Send + Sync>;
pub(crate) type PartsAct = std::sync::Arc<dyn Fn(bool, Vec<(String, String)>) -> Result<String, String> + Send + Sync>;

/// ClickHouse's parts set aside: grouped by table and month, with Try to
/// restore and Delete (confirmed). The same here and on a server.
pub(crate) struct BrokenParts {
    load: PartsLoad,
    act: PartsAct,
    list: Option<Result<Vec<crate::dbstatus::DetachedPart>, String>>,
    result: Option<(bool, String)>,
    busy: bool,
    confirm_delete: bool,
}

impl BrokenParts {
    pub(crate) fn open(load: PartsLoad, act: PartsAct, cx: &mut App) {
        let view = cx.new(|cx| {
            let mut v = BrokenParts { load, act, list: None, result: None, busy: false, confirm_delete: false };
            v.reload(cx);
            v
        });
        crate::modal::open(crate::modal::Modal { title: "ClickHouse parts set aside".into(), body: view.into(), width: 640., icon: None, on_close: None }, cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let load = self.load.clone();
        cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { load() }).await;
            this.update(cx, |v, cx| {
                v.list = Some(got);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn run(&mut self, attach: bool, cx: &mut Context<Self>) {
        let Some(Ok(list)) = &self.list else { return };
        let parts: Vec<(String, String)> = list.iter().map(|p| (p.table.clone(), p.name.clone())).collect();
        let act = self.act.clone();
        self.busy = true;
        self.confirm_delete = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { act(attach, parts) }).await;
            this.update(cx, |v, cx| {
                v.busy = false;
                v.result = Some(match r {
                    Ok(out) => (true, summarize(&out, attach)),
                    Err(e) => (false, summarize(&e, attach)),
                });
                v.reload(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// The agent's per-part lines ("ok x" / "failed x: why") as a short report.
fn summarize(out: &str, attach: bool) -> String {
    let ok = out.lines().filter(|l| l.starts_with("ok ")).count();
    let failed: Vec<&str> = out.lines().filter(|l| l.starts_with("failed ")).collect();
    let verb = if attach { "restored" } else { "deleted" };
    let mut s = format!("{ok} part{} {verb}.", if ok == 1 { "" } else { "s" });
    if !failed.is_empty() {
        s.push_str(&format!(" {} could not be {verb}:\n{}", failed.len(), failed.iter().map(|l| l.trim_start_matches("failed ")).collect::<Vec<_>>().join("\n")));
    } else if ok == 0 && !out.trim().is_empty() {
        s = out.trim().to_string();
    }
    s
}

impl Render for BrokenParts {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut col = v_flex().gap_3().child(div().text_sm().text_color(theme.muted_foreground).child(
            "Parts of the telemetry history ClickHouse set aside after an unclean stop. Their readings are missing from the history. Try to restore brings back the ones it can read; Delete removes them for good.",
        ));
        match &self.list {
            None => col = col.child(ui::skeleton_card(&theme, "parts", None, &["", ""])),
            Some(Err(e)) => col = col.child(ui::notice(&theme, Tone::Bad, format!("The list could not be read.\n{e}"))),
            Some(Ok(list)) if list.is_empty() => col = col.child(ui::notice(&theme, Tone::Good, "No parts are set aside.")),
            Some(Ok(list)) => {
                // Grouped by table and month.
                let mut groups: Vec<(String, String, usize, u64)> = Vec::new();
                for p in list {
                    match groups.iter_mut().find(|g| g.0 == p.table && g.1 == p.partition) {
                        Some(g) => {
                            g.2 += 1;
                            g.3 += p.bytes;
                        }
                        None => groups.push((p.table.clone(), p.partition.clone(), 1, p.bytes)),
                    }
                }
                let mut t = v_flex().gap_0();
                for (i, (table, part, n, bytes)) in groups.into_iter().enumerate() {
                    let month = if part == "tuple()" { "whole table".to_string() } else if part.len() == 6 { format!("{}-{}", &part[..4], &part[4..]) } else { part };
                    t = t.child(
                        h_flex()
                            .gap_3()
                            .py(px(6.))
                            .when(i > 0, |r| r.border_t_1())
                            .border_color(theme.border)
                            .text_sm()
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(table))
                            .child(div().w(px(80.)).child(month))
                            .child(div().w(px(80.)).text_right().child(format!("{n} part{}", if n == 1 { "" } else { "s" })))
                            .child(div().w(px(80.)).text_right().child(crate::dbstatus::bytes_label(bytes))),
                    );
                }
                col = col.child(t);
            }
        }
        if let Some((ok, text)) = &self.result {
            col = col.child(ui::notice(&theme, if *ok { Tone::Good } else { Tone::Warn }, text.clone()));
        }
        let has = matches!(&self.list, Some(Ok(l)) if !l.is_empty());
        let busy = self.busy;
        col.child(
            h_flex()
                .gap_2()
                .justify_end()
                .when(self.confirm_delete, |r| {
                    r.child(div().flex_1().text_sm().text_color(theme.danger).child("Delete them for good? Their readings cannot come back."))
                        .child(crate::ui::button("parts-delete-no").outline().label("Cancel").on_click(cx.listener(|v, _, _, cx| {
                            v.confirm_delete = false;
                            cx.notify();
                        })))
                        .child(
                            div()
                                .id("parts-delete-yes")
                                .h(px(32.))
                                .px_3()
                                .flex()
                                .items_center()
                                .rounded(theme.radius)
                                .bg(theme.danger)
                                .text_color(gpui_kit::white())
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .cursor_pointer()
                                .child("Delete")
                                .on_click(cx.listener(|v, _, _, cx| v.run(false, cx))),
                        )
                })
                .when(!self.confirm_delete, |r| {
                    r.child(crate::ui::button("parts-delete").outline().text_color(theme.danger).label("Delete…").disabled(!has || busy).on_click(cx.listener(|v, _, _, cx| {
                        v.confirm_delete = true;
                        cx.notify();
                    })))
                    .child(crate::ui::button("parts-restore").primary().label("Try to restore").loading(busy).disabled(!has || busy).on_click(cx.listener(|v, _, _, cx| v.run(true, cx))))
                    .child(crate::ui::button("parts-close").outline().label("Close").on_click(|_, _, cx| crate::modal::close(cx)))
                }),
        )
    }
}

/// The Database panel's body: the disk, growth, PostgreSQL and ClickHouse.
/// The same on this computer and on a remote server.
#[allow(clippy::too_many_arguments)]
pub(crate) fn db_details(
    theme: &gpui_kit::component::Theme,
    panel: Div,
    db: &crate::dbstatus::DbStatus,
    drive: &str,
    // None: not measured there (the line is left out).
    growth: Option<Option<(i64, i64)>>,
    last_backup: String,
    actions: DbActions,
    dark: bool,
) -> Div {
    use crate::dbstatus::{CRITICAL_DISK, LOW_DISK, bytes_label, count_label, month_label, plain_table};
    let mut panel = panel;
        let pg_bytes = db.pg.as_ref().map(|p| p.db_bytes).unwrap_or(0);
    let ch_bytes = db.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0);
    let free = db.pg.as_ref().and_then(|p| p.free_bytes);
    let total = db.pg.as_ref().and_then(|p| p.disk_total);
    let lasts = match (free, growth.flatten()) {
        (Some(f), Some((per_day, _))) => crate::dbstatus::lasts_label(f, per_day),
        _ => None,
    };

    // G3: the disk is running low, with the two ways to free space.
    if let Some(f) = free.filter(|f| *f < LOW_DISK) {
        let tone = if f < CRITICAL_DISK { Tone::Bad } else { Tone::Warn };
        let (bg, fg) = match tone {
            Tone::Bad => (theme.danger, theme.danger_foreground),
            _ => (theme.warning, theme.warning_foreground),
        };
        let when = match &lasts {
            Some(l) => format!(" At this rate it is full in {}, and SENTIENT stops storing data when it is.", l.trim_start_matches("about ").replace("less than a day", "less than a day")),
            None => " SENTIENT stops storing data when it is full.".to_string(),
        };
        let kept = db.kept_bytes.filter(|b| *b > 0);
        panel = panel.child(
            h_flex()
                .gap_2()
                .items_center()
                .flex_wrap()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(bg)
                .text_color(fg)
                .text_sm()
                .child(div().flex_1().min_w(px(240.)).child(format!("{drive} is almost full ({} free).{when}", bytes_label(f))))
                .when_some(actions.move_data.clone(), |r, open| {
                    r.child(
                        crate::ui::button("low-move")
                            .primary()
                            .icon(gpui_kit::component::Icon::empty().path("icons/move-data.svg"))
                            .label("Move data to another disk…")
                            .on_click(move |_, window, cx| open(window, cx)),
                    )
                })
                .when(db.hybrid && actions.set_limit.is_some(), |r| {
                    let set_limit = actions.set_limit.clone().unwrap();
                    r.child(crate::ui::button("low-limit").outline().label("Set a history limit…").on_click({ let f = set_limit.clone(); move |_, _, cx| f(cx) }))
                })
                .when_some(kept.zip(actions.remove_kept.clone()), |r, (k, remove_kept)| {
                    r.child(
                        crate::ui::button("low-kept")
                            .outline()
                            .label(format!("Remove kept copies ({})", bytes_label(k)))
                            .on_click(move |_, _, cx| remove_kept(cx)),
                    )
                }),
        );
    }

    // Who uses the disk.
    if let (Some(free), Some(total)) = (free, total.filter(|t| *t > 0)) {
        let other = total.saturating_sub(free).saturating_sub(pg_bytes).saturating_sub(ch_bytes);
        let frac = |b: u64| (b as f32 / total as f32).clamp(0.0, 1.0);
        // A blue that stands apart from the teal and the grey in both themes.
        let ch_color: Hsla = if dark { rgb(0x64b5f6).into() } else { rgb(0x1e6fb8).into() };
        let segs: Vec<(Hsla, u64, &str)> = [
            (theme.primary, pg_bytes, "PostgreSQL"),
            (ch_color, ch_bytes, "ClickHouse"),
            (theme.muted_foreground.opacity(0.45), other, "Other files"),
        ]
        .into_iter()
        .filter(|(_, b, name)| *b > 0 || *name == "PostgreSQL")
        .collect();
        let bar = ui::segment_bar(theme.secondary, 14., &segs.iter().map(|(c, b, _)| (*c, frac(*b))).collect::<Vec<_>>());
        let legend = h_flex()
            .gap_4()
            .flex_wrap()
            .text_size(px(13.))
            .text_color(theme.muted_foreground)
            .children(segs.iter().map(|(c, b, name)| {
                h_flex().gap_1p5().items_center().child(div().size(px(9.)).rounded_sm().bg(*c)).child(format!("{name} {}", bytes_label(*b)))
            }))
            .child(h_flex().gap_1p5().items_center().child(div().size(px(9.)).rounded_sm().bg(theme.secondary)).child(format!("Free {}", bytes_label(free))));
        panel = panel
            .child(div().pt_1().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child({
                // Sentence case, like "Largest tables".
                let mut d = drive.to_string();
                if let Some(f) = d.get(..1) {
                    d = format!("{}{}", f.to_uppercase(), &d[1..]);
                }
                format!("{d} ({})", bytes_label(total))
            }))
            .child(bar)
            .child(legend);
    } else if let Some(f) = free {
        panel = panel.child(ui::fact(theme, "Free disk", bytes_label(f)));
    }
    panel = panel
        .when_some(growth, |p, growth| p.child(ui::fact(theme, "Growth", match growth {
            Some((per_day, days)) if per_day > 0 => format!("about {} a day over the last {days} day{}", bytes_label(per_day as u64), if days == 1 { "" } else { "s" }),
            Some((_, days)) => format!("not growing over the last {days} day{}", if days == 1 { "" } else { "s" }),
            None => "measured from tomorrow (one size reading a day)".into(),
        })))
        .when_some(lasts.clone(), |p, l| p.child(ui::fact(theme, "Disk lasts", format!("{l} at this rate"))));

    // PostgreSQL.
    let pg_col = {
        let mut col = v_flex().flex_1().min_w(px(280.)).gap_2();
        match (&db.pg, &db.pg_error) {
            (Some(pg), _) => {
                col = col
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(ui::database_logo("postgresql", 16.))
                            .when(pg.timescaledb.is_some(), |r| r.child(ui::database_logo("timescale", 16.)))
                            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(match &pg.timescaledb {
                                Some(ts) => format!("PostgreSQL {} · TimescaleDB {ts}", pg.version),
                                None => format!("PostgreSQL {}", pg.version),
                            })),
                    )
                    .child(small_fact(theme, "Size", bytes_label(pg.db_bytes)))
                    .child(small_fact(theme, "Connections", format!("{} open of {} allowed", pg.connections, pg.max_connections)))
                    // Telemetry history kept in PostgreSQL: the same rows as
                    // ClickHouse shows (approximate count from statistics).
                    .when(!db.hybrid, |c| {
                        let ts_bytes = pg.tables.iter().find(|t| t.name == "ts_kv").map(|t| t.bytes);
                        c.when(pg.ts_rows.is_some() || ts_bytes.is_some(), |c| {
                            c.child(small_fact(theme, "Telemetry history", match (pg.ts_rows, ts_bytes) {
                                (Some(r), Some(b)) => format!("about {} values, {}", count_label(r), bytes_label(b)),
                                (Some(r), None) => format!("about {} values", count_label(r)),
                                (None, Some(b)) => bytes_label(b),
                                (None, None) => String::new(),
                            }))
                        })
                        .when_some(pg.ts_first_ms.zip(pg.ts_last_ms), |c, (a, b)| c.child(period_fact(theme, "pg", a, b)))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .child(div().w_32().flex_shrink_0().text_color(theme.muted_foreground).child("Last backup"))
                            .child(div().flex_1().min_w_0().child(last_backup.clone()))
                            .when_some(actions.back_up.clone(), |r, f| r.child(crate::ui::button("backup-now").outline().label("Back up now").on_click(move |_, _, cx| f(cx)))),
                    )
                    .child(div().pt_1().text_size(px(13.)).text_color(theme.muted_foreground).child("Largest tables"));
                let shown: Vec<&crate::dbstatus::Table> = pg.tables.iter().filter(|t| t.bytes > 0).take(5).collect();
                let max = shown.iter().map(|t| t.bytes).max().unwrap_or(1).max(1);
                for t in &shown {
                    let (name, hint) = plain_table(&t.name).unwrap_or(("", ""));
                    let name = if name.is_empty() { t.name.clone() } else { name.to_string() };
                    col = col.child(table_row(theme, name, hint.to_string(), t.bytes as f32 / max as f32, bytes_label(t.bytes)));
                }
                let shown_bytes: u64 = shown.iter().map(|t| t.bytes).sum();
                let rest = pg.db_bytes.saturating_sub(shown_bytes);
                let rest_count = (pg.table_count as usize).saturating_sub(shown.len());
                if rest_count > 0 {
                    col = col.child(table_row(theme, "Everything else".into(), format!("{rest_count} tables"), rest as f32 / max as f32, bytes_label(rest)));
                }
            }
            (None, Some(e)) => col = col.child(ui::outcome(theme, Tone::Warn, format!("Could not read PostgreSQL: {e}"))),
            (None, None) => {}
        }
        col
    };

    // ClickHouse.
    let ch_col = db.hybrid.then(|| {
        let mut col = v_flex().flex_1().min_w(px(280.)).gap_2();
        match (&db.ch, &db.ch_error) {
            (Some(ch), _) => {
                // Healthy (or not readable): no row at all. Parts set aside: a
                // pill that opens the list, with an (i) saying what they are.
                let broken = ch.broken_parts.filter(|n| *n > 0);
                col = col
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(ui::database_logo("clickhouse", 14.))
                            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(format!("ClickHouse {}", ch.version))),
                    )
                    .child(small_fact(theme, "Size", if ch.uncompressed_bytes > ch.disk_bytes {
                        format!("{} on disk ({} before compression)", bytes_label(ch.disk_bytes), bytes_label(ch.uncompressed_bytes))
                    } else {
                        format!("{} on disk", bytes_label(ch.disk_bytes))
                    }))
                    .child(small_fact(theme, "Telemetry history", format!("{} values, {}", count_label(ch.rows), bytes_label(ch.disk_bytes))))
                    .children(match (ch.first_ms, ch.last_ms) {
                        (Some(a), Some(b)) => Some(period_fact(theme, "ch", a, b)),
                        _ => None,
                    })
                    .when(ch.invalid_rows > 0, |c| {
                        c.child(div().pl(px(136.)).text_size(px(13.)).text_color(theme.muted_foreground).child(format!("{} values with no valid time", count_label(ch.invalid_rows))))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .child(div().w_32().flex_shrink_0().text_color(theme.muted_foreground).child("Kept for"))
                            .child(div().flex_1().child(match ch.ttl_days {
                                Some(d) => format!("{d} days"),
                                None => "forever".into(),
                            }))
                            .when_some(actions.set_limit.clone(), |r, f| r.child(crate::ui::button("history-limit").outline().label("Set a limit…").on_click(move |_, _, cx| f(cx)))),
                    )
                    .when_some(broken, |c, n| {
                        let open = actions.broken_parts.clone();
                        c.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .child(div().w_32().flex_shrink_0().text_color(theme.muted_foreground).child("Health"))
                                .child(
                                    div()
                                        .id("broken-parts")
                                        .when(open.is_some(), |d| d.cursor_pointer())
                                        .child(ui::pill(theme, Tone::Warn, format!("{n} damaged part{} set aside ›", if n == 1 { "" } else { "s" })))
                                        .when_some(open, |d, f| d.on_click(move |_, _, cx| f(cx))),
                                )
                                .child(ui::info(
                                    "broken-help".into(),
                                    "After an unclean stop (a power cut, a full disk) ClickHouse moves parts of the history it cannot read aside. They are kept, not deleted, but their readings are missing from the history until restored. Open the list to try to restore them or delete them.".into(),
                                )),
                        )
                    });
                if !ch.months.is_empty() {
                    col = col.child(div().pt_1().text_size(px(13.)).text_color(theme.muted_foreground).child("History by month"));
                    let max = ch.months.iter().map(|(_, b)| *b).max().unwrap_or(1).max(1);
                    let this_month = chrono::Local::now().format("%Y%m").to_string();
                    for (p, b) in ch.months.iter().take(3) {
                        let label = if p.trim_matches('\'') == this_month { format!("{} so far", month_label(p)) } else { month_label(p) };
                        col = col.child(table_row(theme, label, String::new(), *b as f32 / max as f32, bytes_label(*b)));
                    }
                    let older: Vec<&(String, u64)> = ch.months.iter().skip(3).collect();
                    if !older.is_empty() {
                        let bytes: u64 = older.iter().map(|(_, b)| *b).sum();
                        col = col.child(table_row(theme, "Older".into(), format!("{} months", older.len()), bytes as f32 / max as f32, bytes_label(bytes)));
                    }
                }
            }
            (None, Some(e)) => col = col.child(ui::outcome(theme, Tone::Warn, format!("Could not read ClickHouse: {e}"))),
            (None, None) => {}
        }
        col
    });
    panel.child(h_flex().pt_2().gap_8().items_start().flex_wrap().child(pg_col).children(ch_col))
    }
