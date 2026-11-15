// Copyright © 2016-2026 The SENTIENT Authors

//! Status: what is installed and whether each service is running. Refreshes
//! every few seconds while the screen is open. With nothing installed by the
//! Platform Manager, it lists the SENTIENT already running here (installed
//! some other way) and offers to manage it (crate::adopt).

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex, v_flex,
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

/// "Audit log  who did what  [bar]  255 MB"
fn table_row(theme: &gpui_kit::component::Theme, name: String, hint: String, fraction: f32, size: String) -> Div {
    h_flex()
        .gap_2()
        .items_center()
        .text_sm()
        .child(
            h_flex()
                .w(px(190.))
                .flex_shrink_0()
                .gap_1p5()
                .items_baseline()
                .child(div().child(name))
                .when(!hint.is_empty(), |r| r.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(hint))),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(40.))
                .h(px(6.))
                .rounded_full()
                .bg(theme.secondary)
                .child(div().h_full().rounded_full().w(relative(fraction.clamp(0.02, 1.0))).bg(theme.primary)),
        )
        .child(div().w(px(64.)).flex_shrink_0().text_right().child(size))
}

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

    /// The Database panel: who uses the disk and how fast it fills, then
    /// PostgreSQL and (on hybrid installs) ClickHouse side by side.
    fn db_panel(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, cx: &mut Context<Self>) -> Div {
        use crate::dbstatus::{CRITICAL_DISK, LOW_DISK, bytes_label, count_label, day_label, month_label, plain_table};
        let header = h_flex()
            .gap_3()
            .items_center()
            .child(div().font_weight(FontWeight::MEDIUM).child("Database"))
            .child(div().flex_1())
            .children(self.db_read_at.clone().map(|t| {
                div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!("read at {t}"))
            }))
            .child(
                Button::new("db-read")
                    .outline()
                    .label(if self.db_reading { "Reading…" } else if self.db.is_none() { "Read" } else { "Refresh" })
                    .disabled(self.db_reading)
                    .on_click(cx.listener(|this, _, _, cx| this.read_db(cx))),
            );
        let mut panel = ui::panel(theme).child(header);
        let Some(db) = &self.db else {
            return panel.child(div().text_sm().text_color(theme.muted_foreground).child(if self.db_reading {
                "Reading the database's size and state…"
            } else if crate::dbstatus::needs_admin(record) {
                "Read shows the database's size and state. It asks for the administrator password, as the database password is kept where only an administrator can read it."
            } else {
                "Not read yet."
            }));
        };
        let drive = disk_name(record);
        let pg_bytes = db.pg.as_ref().map(|p| p.db_bytes).unwrap_or(0);
        let ch_bytes = db.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0);
        let free = db.pg.as_ref().and_then(|p| p.free_bytes);
        let total = db.pg.as_ref().and_then(|p| p.disk_total);
        let growth = crate::dbstatus::growth_per_day();
        let lasts = match (free, growth) {
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
                    .when(db.hybrid, |r| {
                        r.child(Button::new("low-limit").outline().label("Set a history limit…").on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Settings))))
                    })
                    .when_some(kept, |r, k| {
                        r.child(
                            Button::new("low-kept")
                                .outline()
                                .label(format!("Remove kept copies ({})", bytes_label(k)))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Migration))),
                        )
                    }),
            );
        }

        // Who uses the disk.
        if let (Some(free), Some(total)) = (free, total.filter(|t| *t > 0)) {
            let other = total.saturating_sub(free).saturating_sub(pg_bytes).saturating_sub(ch_bytes);
            let frac = |b: u64| (b as f32 / total as f32).clamp(0.0, 1.0);
            // A blue that stands apart from the teal and the grey in both themes.
            let ch_color: Hsla = if crate::theme::is_dark(cx) { rgb(0x64b5f6).into() } else { rgb(0x1e6fb8).into() };
            let segs: Vec<(Hsla, u64, &str)> = [
                (theme.primary, pg_bytes, "PostgreSQL"),
                (ch_color, ch_bytes, "ClickHouse"),
                (theme.muted_foreground.opacity(0.45), other, "Other files"),
            ]
            .into_iter()
            .filter(|(_, b, name)| *b > 0 || *name == "PostgreSQL")
            .collect();
            let bar = h_flex()
                .w_full()
                .h(px(14.))
                .rounded_full()
                .overflow_hidden()
                .bg(theme.secondary)
                .children(segs.iter().map(|(c, b, _)| div().h_full().w(relative(frac(*b))).bg(*c)));
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
                .child(div().pt_1().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(format!("{} ({})", drive.to_uppercase(), bytes_label(total))))
                .child(bar)
                .child(legend);
        } else if let Some(f) = free {
            panel = panel.child(ui::fact(theme, "Free disk", bytes_label(f)));
        }
        panel = panel
            .child(ui::fact(theme, "Growth", match growth {
                Some((per_day, days)) if per_day > 0 => format!("about {} a day over the last {days} day{}", bytes_label(per_day as u64), if days == 1 { "" } else { "s" }),
                Some((_, days)) => format!("not growing over the last {days} day{}", if days == 1 { "" } else { "s" }),
                None => "measured from tomorrow (one size reading a day)".into(),
            }))
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
                                    Some(ts) => format!("POSTGRESQL {} · TIMESCALEDB {ts}", pg.version),
                                    None => format!("POSTGRESQL {}", pg.version),
                                })),
                        )
                        .child(small_fact(theme, "Size", bytes_label(pg.db_bytes)))
                        .child(small_fact(theme, "Connections", format!("{} open of {} allowed", pg.connections, pg.max_connections)))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .child(div().w_32().flex_shrink_0().text_color(theme.muted_foreground).child("Last backup"))
                                .child(div().flex_1().min_w_0().child(match &self.last_backup {
                                    Some(b) => format!(
                                        "{}{}",
                                        crate::dbstatus::ago_label(&b.at, chrono::Local::now()),
                                        b.bytes.map(|n| format!(", {}", bytes_label(n))).unwrap_or_default()
                                    ),
                                    None => "none from the Platform Manager".to_string(),
                                }))
                                .child(Button::new("backup-now").outline().label("Back up now").on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Backup)))),
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
                    let health = match ch.broken_parts {
                        Some(0) => ("No broken parts".to_string(), None),
                        Some(n) => (format!("{n} broken part{} set aside", if n == 1 { "" } else { "s" }), Some(Tone::Warn)),
                        None => ("Not checked".to_string(), None),
                    };
                    col = col
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(ui::database_logo("clickhouse", 14.))
                                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(format!("CLICKHOUSE {}", ch.version))),
                        )
                        .child(small_fact(theme, "Size", if ch.uncompressed_bytes > ch.disk_bytes {
                            format!("{} on disk ({} before compression)", bytes_label(ch.disk_bytes), bytes_label(ch.uncompressed_bytes))
                        } else {
                            format!("{} on disk", bytes_label(ch.disk_bytes))
                        }))
                        .child(small_fact(theme, "Telemetry history", format!("{} values", count_label(ch.rows))))
                        .children(match (ch.first_ms, ch.last_ms) {
                            (Some(a), Some(b)) => Some(small_fact(theme, "From, to", format!("{} to {}", day_label(a), day_label(b)))),
                            _ => None,
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
                                .child(Button::new("history-limit").outline().label("Set a limit…").on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Settings)))),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .child(div().w_32().flex_shrink_0().text_color(theme.muted_foreground).child("Health"))
                                .child(match health.1 {
                                    Some(tone) => ui::pill(theme, tone, health.0),
                                    None => div().child(health.0),
                                }),
                        );
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
                    .children(c.notes.iter().map(|n| div().text_size(px(13.)).text_color(theme.muted_foreground).child(n.clone())))
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
            // Its port answers but none of its own services runs: some
            // other program is serving there.
            Some(s) if s.serving && !s.services.is_empty() && !s.services.iter().any(|svc| svc.running || svc.starting) => ("Another program answers on SENTIENT's port", Tone::Warn),
            Some(s) if s.serving => ("SENTIENT is running", Tone::Good),
            Some(s) if s.services.iter().any(|svc| svc.running) => ("Starting or partly stopped", Tone::Warn),
            Some(s) if s.services.iter().any(|svc| svc.starting) => ("SENTIENT is starting with Windows", Tone::Warn),
            Some(_) => ("SENTIENT is stopped", Tone::Bad),
        };
        let services = self.snapshot.as_ref().map(|s| s.services.clone()).unwrap_or_default();
        let st = &record.stack;
        // The newest backup in the backup folder failed its check.
        let damaged = crate::backups::list(&crate::backups::BackupPrefs::load().folder)
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
                    .child(Button::new("backup-damaged").outline().label("Back up now").on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Backup)))),
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
                    .child(Button::new("changes-waiting").outline().label("Apply…").on_click(cx.listener(|_, _, _, cx| cx.emit(OpenUpdate)))),
            )
        });

        page.child(
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(ui::product_logo(ui::Product::Sentient, 30.))
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
            .child(h_flex().gap_2().items_center().child(ui::product_logo(ui::Product::Gateway, 24.)).child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway")))
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
