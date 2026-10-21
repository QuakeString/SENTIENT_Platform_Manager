// Copyright © 2016-2026 The SENTIENT Authors

//! Update: install a newer SENTIENT. Native installs update from a package
//! folder (the offline bundle of the new release); Docker installs pull the
//! image. On Windows the update is safe: the database is copied, the schema
//! changes are dry-run first, and a failure puts the previous version back
//! (crate::updating). What the last update replaced is kept until the next
//! one, and "Roll back last update" goes back to it.

use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    progress::Progress,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

use sentient_installer_core::native::safe_update::RollbackPoint;
use sentient_installer_core::stack::Flavor;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

pub struct UpdatePage {
    record: Option<InstallationRecord>,
    package: Option<PathBuf>,
    accept_breaking: bool,
    /// What the last update replaced: None while it is being looked up.
    rollback: Option<Result<Option<RollbackPoint>, String>>,
    job: Entity<Job>,
    _observe: Subscription,
    _tasks: Vec<Task<()>>,
}

impl UpdatePage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |page: &mut Self, job, cx| {
            match job.read(cx).state() {
                JobState::Done(_) => {
                    page.record = InstallationRecord::load();
                    page.refresh_rollback(cx);
                }
                JobState::Failed(_) => page.refresh_rollback(cx),
                _ => {}
            }
            cx.notify();
        });
        let mut page = Self { record, package: None, accept_breaking: false, rollback: None, job, _observe, _tasks: Vec::new() };
        page.refresh_rollback(cx);
        page
    }

    fn refresh_rollback(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.rollback = None;
        let task = cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { crate::updating::rollback_point(&record) }).await;
            this.update(cx, |page, cx| {
                page.rollback = Some(result);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn confirm_rollback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(Some(point))) = &self.rollback else { return };
        let clickhouse = self.record.as_ref().is_some_and(|r| r.flavor() == Flavor::Clickhouse);
        let this = cx.entity().downgrade();
        let text = format!(
            "SENTIENT stops and starts again as {}, on the database copied just before that update. \
             Everything written since the update (telemetry, alarms, users, dashboards and other changes) is no longer in use; \
             the current database is kept aside under Kept on the Migration screen until you remove it.{} \
             If the previous version does not start, the current one is put back and started again.",
            point.describe(),
            if clickhouse { " The telemetry history in ClickHouse is not rolled back: it keeps what was recorded since the update." } else { "" }
        );
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog.title("Roll back the last update?").description(text.clone()).confirm().ok_text("Roll back").on_ok(move |_, _, cx| {
                this.update(cx, |page, cx| page.run_rollback(cx)).ok();
                true
            })
        });
    }

    fn run_rollback(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if crate::rootjob::needed(&record) {
            let args = vec!["rollback".to_string()];
            self.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.job.update(cx, |job, cx| job.start(move |sink| crate::updating::rollback(record, sink).map(|_| ()), cx));
        }
    }

    fn choose_package(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this package".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update(cx, |page, cx| {
                page.package = paths.into_iter().next();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let package = self.package.clone();
        let accept = self.accept_breaking;
        if crate::rootjob::needed(&record) {
            let mut args = vec!["update".to_string()];
            args.extend(package.map(|p| p.display().to_string()));
            if accept {
                args.push("--accept-breaking".into());
            }
            self.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.job.update(cx, |job, cx| {
                job.start(move |sink| crate::updating::run(record, package.as_deref(), accept, sink).map(|_| ()), cx)
            });
        }
    }
}

impl Render for UpdatePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Update",
            "Install a newer SENTIENT, with its database changes, and go back automatically if it fails.",
        );
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };
        let native = record.platform().needs_package();
        let job = self.job.read(cx);
        let busy = job.is_running();
        let can_run = !busy && (!native || self.package.is_some());
        let percent = job.percent();
        let step = job.step().clone();
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let log: Vec<SharedString> = job.log().to_vec();

        page.child(
            ui::panel(&theme)
                .child(ui::fact(&theme, "Installed version", record.sentient_version.clone()))
                .when(native, |panel| {
                    panel.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                Button::new("choose")
                                    .outline()
                                    .label("Choose package…")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.choose_package(cx))),
                            )
                            .child(div().text_sm().text_color(theme.muted_foreground).child(
                                self.package
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|| "The folder of the new SENTIENT package".into()),
                            )),
                    )
                })
                .when(native && cfg!(windows), |panel| {
                    panel.child(div().text_sm().text_color(theme.muted_foreground).child(
                        "SENTIENT stops while it updates. The database is copied first, the new version's database changes are tried out before they are made, and if anything fails the previous version is put back and started.",
                    ))
                })
                .child(
                    Checkbox::new("accept-breaking")
                        .label("Accept breaking changes (only after reading what the update reports)")
                        .checked(self.accept_breaking)
                        .disabled(busy)
                        .on_change(cx.listener(|this, value: &bool, _, cx| {
                            this.accept_breaking = *value;
                            cx.notify();
                        })),
                )
                .child(
                    h_flex().child(
                        Button::new("update")
                            .primary()
                            .label(if busy { "Updating…" } else { "Update SENTIENT" })
                            .disabled(!can_run)
                            .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                    ),
                ),
        )
        .child({
            let point = match &self.rollback {
                Some(Ok(Some(p))) => Some(p.clone()),
                _ => None,
            };
            let about: SharedString = match &self.rollback {
                None => "Looking for the version the last update replaced…".into(),
                Some(Err(e)) => format!("Could not tell whether there is a version to go back to: {e}").into(),
                Some(Ok(None)) => "Nothing to roll back to: no update has been made here since the last roll back, or the last update did not keep the previous version.".into(),
                Some(Ok(Some(p))) => format!("Goes back to {}.", p.describe()).into(),
            };
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Roll back last update"))
                .child(div().text_sm().child(about))
                .when(point.is_some(), |panel| {
                    panel.child(ui::outcome(
                        &theme,
                        Tone::Warn,
                        "Everything written since that update is no longer in use after the roll back: SENTIENT goes back to the database copied just before it. The current database is kept aside until you remove it.",
                    ))
                })
                .child(
                    h_flex().child(
                        Button::new("rollback")
                            .danger()
                            .outline()
                            .label("Roll back last update…")
                            .disabled(busy || point.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_rollback(window, cx))),
                    ),
                )
        })
        .when(busy || outcome.is_some() || !log.is_empty(), |page| {
            page.child(
                ui::panel(&theme)
                    .when(busy, |p| {
                        p.child(div().text_sm().child(step.clone()))
                            .child(Progress::new("update-progress").value(percent.unwrap_or(0.0)))
                    })
                    .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                    .children(log.into_iter().map(|line| {
                        div().text_xs().text_color(theme.muted_foreground).child(line)
                    })),
            )
        })
    }
}
