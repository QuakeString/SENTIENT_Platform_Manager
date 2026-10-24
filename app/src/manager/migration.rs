// Copyright © 2016-2026 The SENTIENT Authors

//! Migration: change the database type (crate::migration does the work).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    progress::Progress,
    radio::RadioGroup,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::stack::Flavor;

use crate::installation::{InstallationRecord, flavor_label};
use crate::jobs::{Job, JobState};
use crate::kept::{self, Kept};
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
    /// What earlier changes kept; None while it is being read.
    kept: Option<Result<Vec<Kept>, String>>,
    /// Native Linux: the operator asked to see it (it needs the password).
    kept_asked: bool,
    _observe: Subscription,
    _tasks: Vec<Task<()>>,
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
                page.refresh_kept(cx);
            }
            cx.notify();
        });
        let state = record.as_ref().and_then(migration::load_state);
        let mut page = Self { record, state, target: None, confirm: false, job, kept: None, kept_asked: false, _observe, _tasks: Vec::new() };
        page.refresh_kept(cx);
        page
    }

    fn refresh_kept(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        // Native Linux reads it as root: only when asked, not on every visit.
        if crate::rootjob::needed(&record) && !self.kept_asked {
            self.kept = Some(Ok(Vec::new()));
            return;
        }
        self.kept = None;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if crate::rootjob::needed(&record) {
                        let noop = sentient_installer_core::progress::noop();
                        crate::rootjob::run(&["kept".to_string()], &noop)
                            .map(|lines| lines.iter().filter_map(|l| kept::decode(l)).collect())
                    } else {
                        kept::list(&record)
                    }
                })
                .await;
            this.update(cx, |page, cx| {
                page.kept = Some(result);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn confirm_remove(&mut self, item: Kept, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let text = format!(
            "{} {} is deleted. This can't be undone.",
            item.name,
            match item.kind {
                kept::KeptKind::History => "and the history in it",
                kept::KeptKind::DatabaseCopy => "and everything in that database",
                kept::KeptKind::ClickHouse => "with its WSL2 distro, its data and the hidden service account",
                kept::KeptKind::ClickhouseDatabase => "and the ClickHouse history in it",
            }
        );
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let item = item.clone();
            dialog.title("Remove for good?").description(text.clone()).confirm().ok_text("Remove").on_ok(move |_, _, cx| {
                let item = item.clone();
                this.update(cx, |page, cx| {
                    let Some(record) = page.record.clone() else { return };
                    if crate::rootjob::needed(&record) {
                        let args = vec!["remove-kept".to_string(), item.name.clone()];
                        page.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
                    } else {
                        page.job.update(cx, |job, cx| job.start(move |sink| kept::remove(&record, &item, sink), cx));
                    }
                })
                .ok();
                true
            })
        });
    }

    fn targets(&self) -> Vec<Flavor> {
        self.record.as_ref().and_then(|r| migration::targets(r).ok()).unwrap_or_default()
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let (Some(record), Some(target)) = (self.record.clone(), self.target.and_then(|i| self.targets().get(i).copied()))
        else {
            return;
        };
        if crate::rootjob::needed(&record) {
            let args = vec!["migrate".to_string(), target.as_str().to_string()];
            self.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.job.update(cx, |job, cx| job.start(move |sink| migration::run(record, target, sink).map(|_| ()), cx));
        }
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if crate::rootjob::needed(&record) {
            let args = vec!["resume-migration".to_string()];
            self.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.job.update(cx, |job, cx| job.start(move |sink| migration::resume(record, sink), cx));
        }
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
        let step = job.step_with_time();
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

        let root_only = crate::rootjob::needed(record) && !self.kept_asked;
        let kept_panel = match &self.kept {
            None => ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Kept after earlier changes"))
                .child(div().text_sm().text_color(theme.muted_foreground).child("Looking…")),
            Some(Err(e)) => ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Kept after earlier changes"))
                .child(div().text_sm().text_color(theme.danger).child(format!("Could not look: {e}"))),
            Some(Ok(items)) => ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Kept after earlier changes"))
                .when(items.is_empty() && !root_only, |p| {
                    p.child(div().text_sm().text_color(theme.muted_foreground).child("Nothing: no old history and no database copies."))
                })
                .when(root_only, |p| {
                    p.child(
                        h_flex().child(
                            Button::new("show-kept")
                                .outline()
                                .small()
                                .label("Show (asks for the administrator password)")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.kept_asked = true;
                                    this.refresh_kept(cx);
                                })),
                        ),
                    )
                })
                .when(!items.is_empty(), |p| {
                    p.child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Migrations keep the old history and updates keep a copy of the database. Remove them once you are sure you no longer need them.",
                    ))
                })
                .children(items.iter().enumerate().map(|(i, item)| {
                    let item_for_click = item.clone();
                    h_flex()
                        .gap_3()
                        .items_center()
                        .text_sm()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).child(item.name.clone()))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(item.detail.clone())),
                        )
                        .child(div().text_color(theme.muted_foreground).child(kept::human(item.bytes)))
                        .child(
                            Button::new(SharedString::from(format!("remove-{i}")))
                                .outline()
                                .small()
                                .label("Remove")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_remove(item_for_click.clone(), window, cx)
                                })),
                        )
                })),
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
        .child(kept_panel)
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
