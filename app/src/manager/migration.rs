// Copyright © 2016-2026 The SENTIENT Authors

//! Migration: change the database type (crate::migration does the work).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    progress::Progress,
    radio::RadioGroup,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::stack::Flavor;

use crate::installation::{InstallationRecord, flavor_label};
use crate::jobs::{Job, JobState};
use crate::migration::{self, MigrationState};
use crate::ui::{self, Tone};

/// The installation record changed on disk (the database type).
pub struct RecordChanged;

pub struct MigrationPage {
    record: Option<InstallationRecord>,
    state: Option<MigrationState>,
    target: Option<usize>,
    confirm: bool,
    job: Entity<Job>,
    _observe: Subscription,
}

impl EventEmitter<RecordChanged> for MigrationPage {}

fn describe(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Stock => "Everything in PostgreSQL. Simplest; suits small sites.",
        Flavor::Timescaledb => "PostgreSQL with TimescaleDB: history compressed and in time chunks.",
        Flavor::Clickhouse => "History in ClickHouse (Docker in WSL2), everything else in PostgreSQL. For large sites.",
    }
}

impl MigrationPage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |page: &mut Self, job, cx| {
            let done = matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_));
            if done {
                // The record and the state file may have changed either way.
                page.record = InstallationRecord::load();
                page.state = page.record.as_ref().and_then(migration::load_state);
                page.target = None;
                page.confirm = false;
                cx.emit(RecordChanged);
            }
            cx.notify();
        });
        let state = record.as_ref().and_then(migration::load_state);
        Self { record, state, target: None, confirm: false, job, _observe }
    }

    fn targets(&self) -> Vec<Flavor> {
        self.record.as_ref().and_then(|r| migration::targets(r).ok()).unwrap_or_default()
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let (Some(record), Some(target)) = (self.record.clone(), self.target.and_then(|i| self.targets().get(i).copied()))
        else {
            return;
        };
        self.job.update(cx, |job, cx| job.start(move |sink| migration::run(record, target, sink).map(|_| ()), cx));
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.job.update(cx, |job, cx| job.start(move |sink| migration::resume(record, sink), cx));
    }
}

impl Render for MigrationPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Migration",
            "Change the database type. Every row of history is copied and checked; nothing is deleted.",
        );
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };
        let job = self.job.read(cx);
        let busy = job.is_running();
        let step = job.step().clone();
        let percent = job.percent();
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let log: Vec<SharedString> = job.log().iter().rev().take(12).rev().cloned().collect();
        let current = record.flavor();
        let targets = migration::targets(record);
        let unfinished = self.state.clone().filter(|s| !s.done);
        let chosen = self.target.and_then(|i| targets.as_ref().ok()?.get(i).copied());

        let main = match &targets {
            Err(why) => ui::panel(&theme)
                .child(ui::fact(&theme, "Database", flavor_label(current)))
                .child(div().text_sm().text_color(theme.muted_foreground).child(*why)),
            Ok(targets) => ui::panel(&theme)
                .child(ui::fact(&theme, "Database now", flavor_label(current)))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Change to"))
                .child(
                    RadioGroup::vertical("target")
                        .children(targets.iter().map(|f| format!("{} — {}", flavor_label(*f), describe(*f))))
                        .selected_index(self.target)
                        .disabled(busy || unfinished.is_some())
                        .on_change(cx.listener(|this, value: &usize, _, cx| {
                            this.target = Some(*value);
                            this.confirm = false;
                            cx.notify();
                        })),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("1. A full backup, history included, is written to the backups folder of the installation.")
                        .child("2. SENTIENT stops for about a minute and starts again on the new type.")
                        .child("3. The history from before the switch is then copied month by month while SENTIENT runs; charts fill in as it goes.")
                        .child("4. The old history is kept where it was until you remove it.")
                        .when(chosen == Some(Flavor::Clickhouse), |c| {
                            c.child("ClickHouse is installed first (WSL2 and Docker under a hidden service account); if Windows must restart for WSL2, nothing is changed and you start again afterwards.")
                        }),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .when(!self.confirm, |r| {
                            r.child(
                                Button::new("migrate")
                                    .primary()
                                    .label(chosen.map(|f| format!("Change to {}", flavor_label(f))).unwrap_or_else(|| "Change database".into()))
                                    .disabled(busy || chosen.is_none() || unfinished.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm = true;
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(self.confirm, |r| {
                            r.child(
                                Button::new("migrate-confirm")
                                    .danger()
                                    .label("Stop SENTIENT and change now")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                            )
                            .child(
                                Button::new("migrate-cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm = false;
                                        cx.notify();
                                    })),
                            )
                        }),
                ),
        };

        page.when_some(unfinished, |p, state| {
            p.child(
                ui::panel(&theme)
                    .child(ui::pill(&theme, Tone::Warn, "History copy not finished"))
                    .child(div().text_sm().child(format!(
                        "SENTIENT runs on {} since the switch from {}, but part of the history from before it is still being copied. Continue the copy; months already copied are skipped.",
                        flavor_label(state.to),
                        flavor_label(state.from)
                    )))
                    .child(
                        h_flex().child(
                            Button::new("resume")
                                .primary()
                                .label("Continue copying")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.resume(cx))),
                        ),
                    ),
            )
        })
        .child(main)
        .when(busy || outcome.is_some() || !log.is_empty(), |page| {
            page.child(
                ui::panel(&theme)
                    .when(busy, |p| {
                        p.child(div().text_sm().child(step.clone()))
                            .when_some(percent, |p, v| p.child(Progress::new("migration-progress").value(v)))
                    })
                    .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                    .children(log.into_iter().map(|line| div().text_xs().text_color(theme.muted_foreground).child(line))),
            )
        })
    }
}
