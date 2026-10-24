// Copyright © 2016-2026 The SENTIENT Authors

//! Repair and uninstall, from the Platform Manager (the same work as Setup's
//! maintenance mode, crate::maintenance). An adopted Docker stack that runs
//! from its own compose file can be taken over here (crate::adopt).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    progress::Progress,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::manager::migration::RecordChanged;
use crate::ui::{self, Tone};

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Repair,
    Uninstall,
    TakeOver,
}

pub struct RepairPage {
    record: Option<InstallationRecord>,
    keep_data: bool,
    running: Option<Action>,
    job: Entity<Job>,
    _observe: Subscription,
}

impl EventEmitter<RecordChanged> for RepairPage {}

impl RepairPage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |page: &mut Self, job, cx| {
            if page.running == Some(Action::TakeOver) && matches!(job.read(cx).state(), JobState::Done(_)) {
                page.record = InstallationRecord::load();
                cx.emit(RecordChanged);
            }
            cx.notify();
        });
        Self { record, keep_data: true, running: None, job, _observe }
    }

    fn start(&mut self, action: Action, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let keep = self.keep_data;
        self.running = Some(action);
        self.job.update(cx, |job, cx| {
            if action == Action::TakeOver {
                return job.start(move |sink| crate::adopt::take_over_compose(record, sink).map(|_| ()), cx);
            }
            if crate::rootjob::needed(&record) {
                let mut args = vec![match action {
                    Action::Repair => "repair".to_string(),
                    _ => "uninstall".to_string(),
                }];
                if action == Action::Uninstall && !keep {
                    args.push("--delete-data".into());
                }
                job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx)
            } else {
                match action {
                    Action::Repair => job.start(move |sink| crate::maintenance::run_repair(record, sink), cx),
                    _ => job.start(move |sink| crate::maintenance::run_uninstall(record, keep, sink), cx),
                }
            }
        });
    }

    fn confirm(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let keep = self.keep_data;
        let (title, text, ok) = match action {
            Action::Repair => (
                "Repair SENTIENT?",
                "The services and their configuration are written again and SENTIENT is restarted. Data and settings are kept.",
                "Repair",
            ),
            Action::Uninstall if keep => (
                "Uninstall SENTIENT?",
                "SENTIENT and the Platform Manager are removed. The database is kept and can be used by a new install.",
                "Uninstall",
            ),
            Action::TakeOver => (
                "Take over the compose file?",
                "SENTIENT stops for about a minute: its containers are removed (the data volumes stay) and started again, the same, from the Platform Manager's compose file. If SENTIENT does not answer, it is started from its own compose file again. Afterwards start and stop it with the Platform Manager, not with the old compose file.",
                "Take over",
            ),
            Action::Uninstall => (
                "Uninstall SENTIENT and delete its data?",
                "SENTIENT, the Platform Manager, the databases and everything kept are removed. This can't be undone.",
                "Delete everything",
            ),
        };
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog.title(title).description(text).confirm().ok_text(ok).on_ok(move |_, _, cx| {
                this.update(cx, |page, cx| page.start(action, cx)).ok();
                true
            })
        });
    }
}

impl Render for RepairPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Repair and uninstall",
            "Write the services and their configuration again, or remove SENTIENT from this computer.",
        );
        if self.record.is_none() {
            return page.child(ui::not_installed(&theme));
        }
        let job = self.job.read(cx);
        let busy = job.is_running();
        let step = job.step_with_time();
        let percent = job.percent();
        let uninstalled = self.running == Some(Action::Uninstall) && matches!(job.state(), JobState::Done(_));
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let log: Vec<SharedString> = job.log().iter().rev().take(8).rev().cloned().collect();
        let done = busy || uninstalled;
        let external = self.record.as_ref().and_then(|r| r.stack.external.clone()).filter(|_| self.record.as_ref().is_some_and(|r| r.engine == crate::installation::Engine::Docker));

        page.when_some(external, |page, ext| {
            page.child(
                ui::panel(&theme)
                    .child(div().font_weight(FontWeight::MEDIUM).child("Take over the compose file"))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(format!(
                        "SENTIENT runs from its own compose file ({}), which the Platform Manager does not change. Changing settings or the database type needs the Platform Manager's own compose file: the same containers on the same data.",
                        ext.files.join(", ")
                    )))
                    .child(
                        h_flex().child(
                            Button::new("take-over")
                                .outline()
                                .label("Take over the compose file…")
                                .disabled(done)
                                .on_click(cx.listener(|this, _, window, cx| this.confirm(Action::TakeOver, window, cx))),
                        ),
                    ),
            )
        })
        .child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Repair"))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "For a SENTIENT that does not start or a service that went missing: the services and their configuration are written again from this installation's record, and SENTIENT is started. Data and settings are kept.",
                ))
                .child(
                    h_flex().child(
                        Button::new("repair")
                            .outline()
                            .label("Repair")
                            .disabled(done)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(Action::Repair, window, cx))),
                    ),
                ),
        )
        .child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Uninstall"))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "Removes SENTIENT, its services and the Platform Manager. Keeping the database lets a new install use it.",
                ))
                .child(
                    Checkbox::new("keep-data")
                        .label("Keep the database")
                        .checked(self.keep_data)
                        .disabled(done)
                        .on_change(cx.listener(|this, value: &bool, _, cx| {
                            this.keep_data = *value;
                            cx.notify();
                        })),
                )
                .child(
                    h_flex().child(
                        Button::new("uninstall")
                            .danger()
                            .label(if self.keep_data { "Uninstall" } else { "Uninstall and delete the data" })
                            .disabled(done)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(Action::Uninstall, window, cx))),
                    ),
                ),
        )
        .when(busy || outcome.is_some(), |page| {
            page.child(
                ui::panel(&theme)
                    .when(busy, |p| {
                        p.child(div().text_sm().child(step.clone()))
                            .when_some(percent, |p, v| p.child(Progress::new("repair-progress").value(v)))
                    })
                    .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                    .children(log.into_iter().map(|line| div().text_xs().text_color(theme.muted_foreground).child(line)))
                    .when(uninstalled, |p| {
                        p.child(
                            h_flex().child(
                                Button::new("close")
                                    .primary()
                                    .label("Close")
                                    .on_click(|_, window, _| window.remove_window()),
                            ),
                        )
                    }),
            )
        })
    }
}
