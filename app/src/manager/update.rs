// Copyright © 2016-2026 The SENTIENT Authors

//! Update: install a newer SENTIENT. Native installs update from a package
//! folder (the offline bundle of the new release); Docker installs pull the
//! image. On Windows the update is safe: the database is copied, the schema
//! changes are dry-run first, and a failure puts the previous version back
//! (crate::updating).

use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    progress::Progress,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

pub struct UpdatePage {
    record: Option<InstallationRecord>,
    package: Option<PathBuf>,
    accept_breaking: bool,
    job: Entity<Job>,
    _observe: Subscription,
}

impl UpdatePage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_)) {
                page.record = InstallationRecord::load();
            }
            cx.notify();
        });
        Self { record, package: None, accept_breaking: false, job, _observe }
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
        self.job.update(cx, |job, cx| {
            job.start(move |sink| crate::updating::run(record, package.as_deref(), accept, sink).map(|_| ()), cx)
        });
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
        .when(busy || outcome.is_some() || !log.is_empty(), |page| {
            page.child(
                ui::panel(&theme)
                    .when(busy, |p| {
                        p.child(div().text_sm().child(step.clone()))
                            .child(Progress::new("update-progress").value(percent.unwrap_or(0.0)))
                    })
                    .children(outcome.map(|(tone, text)| ui::pill(&theme, tone, text)))
                    .children(log.into_iter().map(|line| {
                        div().text_xs().text_color(theme.muted_foreground).child(line)
                    })),
            )
        })
    }
}
