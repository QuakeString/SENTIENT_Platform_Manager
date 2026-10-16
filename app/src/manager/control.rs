// Copyright © 2016-2026 The SENTIENT Authors

//! Control: start, stop and restart the platform. Services are started in
//! dependency order (database first) and stopped in reverse by the engine.

use gpui_kit::component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

pub struct ControlPage {
    record: Option<InstallationRecord>,
    job: Entity<Job>,
    _observe: Subscription,
}

impl ControlPage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |_, _, cx| cx.notify());
        Self { record, job, _observe }
    }

    fn run(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let Some(record) = &self.record else { return };
        let platform = record.platform();
        self.job.update(cx, |job, cx| {
            job.start(move |sink| platform.control(sink, action), cx)
        });
    }

    /// Stopping takes every device connection down, so it is confirmed.
    fn confirm_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title("Stop SENTIENT?")
                .description(
                    "Devices lose their connection and dashboards stop updating until SENTIENT is started again.",
                )
                .confirm()
                .ok_text("Stop")
                .on_ok(move |_, _, cx| {
                    this.update(cx, |page, cx| page.run("stop", cx)).ok();
                    true
                })
        });
    }
}

impl Render for ControlPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Control",
            "Start, stop and restart SENTIENT and its database.",
        );
        if self.record.is_none() {
            return page.child(ui::not_installed(&theme));
        }
        let job = self.job.read(cx);
        let busy = job.is_running();
        let outcome = match job.state() {
            JobState::Idle => None,
            JobState::Running => Some((Tone::Neutral, format!("{}…", job.step()))),
            JobState::Done(m) => Some((Tone::Good, m.to_string())),
            JobState::Failed(m) => Some((Tone::Bad, m.to_string())),
        };
        let log: Vec<SharedString> = job.log().to_vec();
        page.child(
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("start")
                                .primary()
                                .label("Start")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.run("start", cx))),
                        )
                        .child(
                            Button::new("restart")
                                .outline()
                                .label("Restart")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.run("restart", cx))),
                        )
                        .child(
                            Button::new("stop")
                                .outline()
                                .label("Stop")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| this.confirm_stop(window, cx))),
                        ),
                )
                .children(outcome.map(|(tone, text)| ui::pill(&theme, tone, text)))
                .children(log.into_iter().map(|line| {
                    div().text_sm().text_color(theme.muted_foreground).child(line)
                })),
        )
    }
}
