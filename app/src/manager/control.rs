// Copyright © 2016-2026 The SENTIENT Authors

//! Control: start, stop and restart the platform. Services are started in
//! dependency order (database first) and stopped in reverse by the engine.
//! Also whether SENTIENT opens in the browser when someone signs in
//! (operator screens; kiosk::set_open_at_signin), and whether the computer
//! signs in by itself after a restart (autologin).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputState},
    radio::RadioGroup,
};
use gpui_kit::prelude::FluentBuilder as _;
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
    /// The account the computer signs in to by itself, as found: None
    /// while looking, Some(None) when it does not.
    autologin: Option<Option<String>>,
    autologin_on: bool,
    autologin_user: Entity<InputState>,
    autologin_password: Entity<InputState>,
    signin_job: Entity<Job>,
    _signin_observe: Subscription,
    /// Starting with Windows, as found: (with Windows, delay seconds); None
    /// while looking, or where there is nothing to choose (Linux).
    start: Option<(bool, u32)>,
    start_on: bool,
    start_delay: Entity<InputState>,
    start_job: Entity<Job>,
    _start_observe: Subscription,
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
    pub fn new(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |_, _, cx| cx.notify());
        let signin_job = cx.new(|_| Job::new());
        let _signin_observe = cx.observe(&signin_job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                page.load_signin(cx);
            }
            cx.notify();
        });
        let start_job = cx.new(|_| Job::new());
        let _start_observe = cx.observe(&start_job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                page.load_start(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            record,
            job,
            _observe,
            signin: None,
            signin_choice: 0,
            autologin: None,
            autologin_on: false,
            autologin_user: cx.new(|cx| {
                InputState::new(window, cx).default_value(sentient_installer_core::autologin::this_user())
            }),
            autologin_password: cx.new(|cx| InputState::new(window, cx).masked(true)),
            signin_job,
            _signin_observe,
            start: None,
            start_on: true,
            start_delay: cx.new(|cx| InputState::new(window, cx)),
            start_job,
            _start_observe,
            _tasks: Vec::new(),
        };
        page.load_signin(cx);
        page.load_start(cx);
        page
    }

    fn load_start(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { crate::self_install::autostart() }).await;
            this.update_in(cx, |page, window, cx| {
                if let Some((on, delay)) = found {
                    page.start_on = on;
                    page.start_delay.update(cx, |input, cx| input.set_value(delay.to_string(), window, cx));
                }
                page.start = found;
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The start choice differs from what is set.
    fn start_changed(&self, cx: &App) -> bool {
        let Some((on, delay)) = self.start else { return false };
        let typed = self.start_delay.read(cx).value().trim().to_string();
        on != self.start_on || (self.start_on && typed != delay.to_string())
    }

    fn apply_start(&mut self, cx: &mut Context<Self>) {
        let on = self.start_on;
        let typed = self.start_delay.read(cx).value().to_string();
        self.start_job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    let delay = if on { crate::installer::parse_start_delay(&typed)? } else { 0 };
                    crate::self_install::set_autostart(on, delay)?;
                    sink(sentient_installer_core::progress::Progress::Done {
                        message: match (on, delay) {
                            (false, _) => "SENTIENT no longer starts with Windows: start it here.".to_string(),
                            (true, 0) => "SENTIENT starts together with Windows.".to_string(),
                            (true, d) => format!("SENTIENT starts {d} seconds after Windows."),
                        },
                    });
                    Ok(())
                },
                cx,
            )
        });
    }

    fn load_signin(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            let (found, account) = cx
                .background_executor()
                .spawn(async move {
                    (sentient_installer_core::kiosk::open_at_signin(), sentient_installer_core::autologin::current())
                })
                .await;
            this.update_in(cx, |page, window, cx| {
                page.signin = Some(signin_index(found));
                page.signin_choice = signin_index(found);
                page.autologin_on = account.is_some();
                if let Some(user) = &account {
                    page.autologin_user.update(cx, |input, cx| input.set_value(user.clone(), window, cx));
                }
                page.autologin_password.update(cx, |input, cx| input.set_value("", window, cx));
                page.autologin = Some(account);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn autologin_user(&self, cx: &App) -> String {
        self.autologin_user.read(cx).value().trim().to_string()
    }

    /// What Apply would change: (opening at sign-in, automatic sign-in).
    fn pending(&self, cx: &App) -> (bool, bool) {
        let signin = self.signin.is_some_and(|s| s != self.signin_choice);
        let autologin = match &self.autologin {
            None => false,
            Some(found) => {
                found.is_some() != self.autologin_on
                    || (self.autologin_on
                        && (found.as_deref() != Some(self.autologin_user(cx).as_str())
                            || !self.autologin_password.read(cx).value().is_empty()))
            }
        };
        (signin, autologin)
    }

    fn apply_signin(&mut self, cx: &mut Context<Self>) {
        let (signin, autologin) = self.pending(cx);
        let mode = match self.signin_choice {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let account = self
            .autologin_on
            .then(|| (self.autologin_user(cx), self.autologin_password.read(cx).value().to_string()));
        self.signin_job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    if signin {
                        crate::self_install::set_open_at_signin(&sink, mode)?;
                    }
                    if autologin {
                        let account = account.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
                        sentient_installer_core::autologin::set(&sink, account)?;
                    }
                    Ok(())
                },
                cx,
            )
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
            let (signin_changed, autologin_changed) = self.pending(cx);
            let changed = signin_changed || autologin_changed;
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Operator screen"))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "Open SENTIENT in the browser when someone signs in to this computer, for example a control-room screen after a power cut. It opens at once and shows SENTIENT as soon as it answers.",
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
                .when(self.autologin.is_some(), |panel| {
                    panel
                        .child(
                            Checkbox::new("autologin")
                                .label("Sign in to this computer automatically after a restart")
                                .checked(self.autologin_on)
                                .disabled(sbusy)
                                .on_change(cx.listener(|this, value: &bool, _, cx| {
                                    this.autologin_on = *value;
                                    cx.notify();
                                })),
                        )
                        .when(self.autologin_on, |panel| {
                            panel
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            v_flex()
                                                .gap_1p5()
                                                .w_64()
                                                .child(div().text_xs().text_color(theme.muted_foreground).child("Operator account"))
                                                .child(Input::new(&self.autologin_user)),
                                        )
                                        .when(cfg!(windows), |row| {
                                            let found = self.autologin.as_ref().is_some_and(|f| f.is_some());
                                            row.child(
                                                v_flex()
                                                    .gap_1p5()
                                                    .w_64()
                                                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                                                        if found { "Password (only to change it)" } else { "Account password" },
                                                    ))
                                                    .child(Input::new(&self.autologin_password).mask_toggle()),
                                            )
                                        }),
                                )
                                .child(ui::outcome(
                                    &theme,
                                    Tone::Warn,
                                    if cfg!(windows) {
                                        "Anyone at this screen gets this account without typing a password. Use an operator account with only the rights the operator needs. Windows keeps the password in its secret store, readable only by administrators."
                                    } else {
                                        "Anyone at this screen gets this account without typing a password. Use an operator account with only the rights the operator needs."
                                    },
                                ))
                        })
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
        .when(self.start.is_some(), |page| {
            let job = self.start_job.read(cx);
            let busy = job.is_running();
            let outcome = match job.state() {
                JobState::Done(m) => Some((Tone::Good, m.to_string())),
                JobState::Failed(m) => Some((Tone::Bad, m.to_string())),
                _ => None,
            };
            let changed = self.start_changed(cx);
            page.child(
                ui::panel(&theme)
                    .child(div().font_weight(FontWeight::MEDIUM).child("Start with Windows"))
                    .child(
                        Checkbox::new("start-with-windows")
                            .label("Start SENTIENT automatically with Windows")
                            .checked(self.start_on)
                            .disabled(busy)
                            .on_change(cx.listener(|this, value: &bool, _, cx| {
                                this.start_on = *value;
                                cx.notify();
                            })),
                    )
                    .when(self.start_on, |panel| panel.child(div().pl_6().child(ui::start_delay_row(&theme, &self.start_delay))))
                    .child(
                        h_flex().child(
                            Button::new("apply-start")
                                .outline()
                                .label("Apply")
                                .disabled(busy || !changed)
                                .on_click(cx.listener(|this, _, _, cx| this.apply_start(cx))),
                        ),
                    )
                    .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text))),
            )
        })
    }
}
