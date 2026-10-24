// Copyright © 2016-2026 The SENTIENT Authors

//! Control: start, stop and restart the platform. Services are started in
//! dependency order (database first) and stopped in reverse by the engine.
//! Also whether SENTIENT opens in the browser when someone signs in
//! (operator screens; kiosk::set_open_at_signin).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex,
    button::{Button, ButtonVariants as _},
    radio::RadioGroup,
};
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

pub struct ControlPage {
    record: Option<InstallationRecord>,
    job: Entity<Job>,
    _observe: Subscription,
    /// Opening at sign-in, as found: 0 off, 1 full screen, 2 window; None
    /// while looking.
    signin: Option<usize>,
    /// What the operator picked.
    signin_choice: usize,
    signin_job: Entity<Job>,
    _signin_observe: Subscription,
    _tasks: Vec<Task<()>>,
}

fn signin_index(mode: Option<bool>) -> usize {
    match mode {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

impl ControlPage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |_, _, cx| cx.notify());
        let signin_job = cx.new(|_| Job::new());
        let _signin_observe = cx.observe(&signin_job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                page.load_signin(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            record,
            job,
            _observe,
            signin: None,
            signin_choice: 0,
            signin_job,
            _signin_observe,
            _tasks: Vec::new(),
        };
        page.load_signin(cx);
        page
    }

    fn load_signin(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { sentient_installer_core::kiosk::open_at_signin() }).await;
            this.update(cx, |page, cx| {
                page.signin = Some(signin_index(found));
                page.signin_choice = signin_index(found);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn apply_signin(&mut self, cx: &mut Context<Self>) {
        let mode = match self.signin_choice {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let launcher = crate::self_install::manager_exe();
        self.signin_job.update(cx, |job, cx| {
            job.start(move |sink| sentient_installer_core::kiosk::set_open_at_signin(&sink, &launcher, mode), cx)
        });
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
                .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                .children(log.into_iter().map(|line| {
                    div().text_sm().text_color(theme.muted_foreground).child(line)
                })),
        )
        .child({
            let sjob = self.signin_job.read(cx);
            let sbusy = sjob.is_running();
            let soutcome = match sjob.state() {
                JobState::Done(m) => Some((Tone::Good, m.to_string())),
                JobState::Failed(m) => Some((Tone::Bad, m.to_string())),
                _ => None,
            };
            let changed = self.signin.is_some_and(|s| s != self.signin_choice);
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Operator screen"))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "Open SENTIENT in the browser when someone signs in to this computer, for example a control-room screen after a power cut. It waits until SENTIENT answers, then opens it.",
                ))
                .child(match self.signin {
                    None => div().text_sm().child("Checking…").into_any_element(),
                    Some(_) => RadioGroup::horizontal("open-at-signin")
                        .children(["Off", "Full screen (kiosk)", "Browser window"])
                        .selected_index(Some(self.signin_choice))
                        .disabled(sbusy)
                        .on_change(cx.listener(|this, value: &usize, _, cx| {
                            this.signin_choice = *value;
                            cx.notify();
                        }))
                        .into_any_element(),
                })
                .child(
                    h_flex().child(
                        Button::new("apply-signin")
                            .outline()
                            .label("Apply")
                            .disabled(sbusy || !changed)
                            .on_click(cx.listener(|this, _, _, cx| this.apply_signin(cx))),
                    ),
                )
                .children(soutcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
        })
    }
}
