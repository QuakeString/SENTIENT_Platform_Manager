// Copyright © 2016-2026 The SENTIENT Authors

//! Control: start, stop and restart the platform. Services are started in
//! dependency order (database first) and stopped in reverse by the engine.
//! Also whether SENTIENT opens in the browser when someone signs in
//! (operator screens; kiosk::set_open_at_signin), and whether the computer
//! signs in by itself after a restart (autologin).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    input::{Input, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

pub struct ControlPage {
    record: Option<InstallationRecord>,
    /// The services, as last read.
    services: Vec<crate::installation::Service>,
    serving: bool,
    /// The service whose own action runs now.
    svc_busy: Option<String>,
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
        let _observe = cx.observe(&job, |page: &mut Self, job, cx| {
            if !job.read(cx).is_running() && page.svc_busy.is_some() {
                page.svc_busy = None;
                page.read_services(cx);
            }
            cx.notify();
        });
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
            services: Vec::new(),
            serving: false,
            svc_busy: None,
        };
        page.load_signin(cx);
        // The services, read now and every few seconds.
        if page.record.is_some() {
            let poll = cx.spawn(async move |this, cx| loop {
                if this.update(cx, |p, cx| p.read_services(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
            });
            page._tasks.push(poll);
        }
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
                            (false, _) => "SENTIENT no longer starts with Windows: use Start on the Control page.".to_string(),
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

    fn read_services(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        cx.spawn(async move |this, cx| {
            let snap = cx.background_executor().spawn(async move { record.platform().snapshot() }).await;
            this.update(cx, |p, cx| {
                p.services = snap.services;
                p.serving = snap.serving;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// One service's own start, stop or restart.
    fn run_service(&mut self, name: String, action: &'static str, cx: &mut Context<Self>) {
        let Some(record) = &self.record else { return };
        let platform = record.platform();
        self.svc_busy = Some(name.clone());
        self.job.update(cx, |job, cx| job.start(move |sink| platform.service_control(sink, &name, action), cx));
        cx.notify();
    }

    /// Stopping takes every device connection down, so it is confirmed.
    fn confirm_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        confirm_stop(window, cx, std::rc::Rc::new(move |cx| {
            this.update(cx, |page, cx| page.run("stop", cx)).ok();
        }));
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
                .child({
                    let this = cx.entity().downgrade();
                    let (t1, t2, t3) = (this.clone(), this.clone(), this);
                    control_row(
                        &theme,
                        "",
                        busy,
                        self.record.as_ref().map(|_| self.serving).filter(|_| !self.services.is_empty()),
                        std::rc::Rc::new(move |_, cx| {
                            t1.update(cx, |p, cx| p.run("start", cx)).ok();
                        }),
                        std::rc::Rc::new(move |_, cx| {
                            t2.update(cx, |p, cx| p.run("restart", cx)).ok();
                        }),
                        std::rc::Rc::new(move |w, cx| {
                            t3.update(cx, |p, cx| p.confirm_stop(w, cx)).ok();
                        }),
                    )
                })
                .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                .children(log.into_iter().map(|line| {
                    div().text_sm().text_color(theme.muted_foreground).child(line)
                })),
        )
        .child({
            // Each service on its own (shared rows with a remote server's).
            let this = cx.entity().downgrade();
            let serving = self.serving;
            let act: ui::ServiceAct = std::rc::Rc::new(move |name, action, window, cx| {
                let name = name.to_string();
                let plain = ui::service_label(&name).unwrap_or(&name).to_string();
                let go: std::rc::Rc<dyn Fn(&mut App)> = std::rc::Rc::new({
                    let (this, name) = (this.clone(), name.clone());
                    move |cx| {
                        this.update(cx, |p, cx| p.run_service(name.clone(), action, cx)).ok();
                    }
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
            });
            let log: ui::ServiceLog = std::rc::Rc::new(|name, _, cx| {
                crate::logsource::request(crate::logsource::of_service(name));
                cx.refresh_windows();
            });
            ui::panel(&theme)
                .gap_2()
                .child(div().font_weight(FontWeight::MEDIUM).child("Services"))
                .children(ui::service_rows(&theme, self.services.iter().map(|s| (s.name.clone().into(), s.running, s.starting, s.detail.clone().into())), Some((act, log, self.svc_busy.clone()))))
        })
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
                // Setting rows like every other panel: label left, control
                // right; the segmented control hugs its options.
                .child(crate::manager::settings::row(&theme, "Open SENTIENT at sign-in", None, false, match self.signin {
                    None => div().text_sm().child("Checking…").into_any_element(),
                    Some(_) => ui::seg_track(&theme)
                        .children(["Off", "Full screen (kiosk)", "Browser window"].into_iter().enumerate().map(|(i, label)| {
                            ui::seg_item(&theme, SharedString::from(format!("signin-{i}")), label, self.signin_choice == i).on_click(cx.listener(move |this, _, _, cx| {
                                if !sbusy {
                                    this.signin_choice = i;
                                    cx.notify();
                                }
                            }))
                        }))
                        .into_any_element(),
                }))
                .when(self.autologin.is_some(), |panel| {
                    panel
                        .child(crate::manager::settings::row(
                            &theme,
                            "Sign in to this computer automatically after a restart",
                            None,
                            false,
                            gpui_kit::component::switch::Switch::new("autologin").checked(self.autologin_on).on_click(cx.listener(move |this, value: &bool, _, cx| {
                                if !sbusy {
                                    this.autologin_on = *value;
                                    cx.notify();
                                }
                            })),
                        ))
                        .when(self.autologin_on, |panel| {
                            let found = self.autologin.as_ref().is_some_and(|f| f.is_some());
                            panel
                                .child(crate::manager::settings::row(&theme, "Operator account", None, false, div().w(px(260.)).child(Input::new(&self.autologin_user))))
                                .when(cfg!(windows), |p| {
                                    p.child(crate::manager::settings::row(
                                        &theme,
                                        if found { "Password (only to change it)" } else { "Account password" },
                                        None,
                                        false,
                                        div().w(px(260.)).child(Input::new(&self.autologin_password).mask_toggle()),
                                    ))
                                })
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
                        crate::ui::button("apply-signin")
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
                            crate::ui::button("apply-start")
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

/// Stopping takes every device connection down, so it is confirmed. Shared
/// with a remote server's Control.
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

/// Start, Restart and Stop, in that order and style, on this computer and
/// on a remote server.
/// The whole install's state and its actions, in one order everywhere:
/// Start, Stop, Restart. The action that fits the state is the primary one;
/// actions that do not apply are off. `running`: None while not known.
pub(crate) fn control_row(
    theme: &gpui_kit::component::Theme,
    id: &str,
    busy: bool,
    running: Option<bool>,
    start: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    restart: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    stop: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
) -> Div {
    let up = running == Some(true);
    let down = running == Some(false);
    let btn = |key: &str, label: &'static str, primary: bool, on: bool| {
        let b = crate::ui::button(SharedString::from(format!("{id}{key}"))).label(label).disabled(busy || !on);
        if primary { b.primary() } else { b.outline() }
    };
    v_flex()
        .gap_3()
        .child(div().text_sm().text_color(theme.muted_foreground).child(match running {
            Some(true) => "SENTIENT is running.",
            Some(false) => "SENTIENT is stopped.",
            None => "Reading SENTIENT's state…",
        }))
        .child(
            h_flex()
                .gap_2()
                .child(btn("start", "Start", down, !up).on_click(move |_, w, cx| start(w, cx)))
                .child(btn("stop", "Stop", false, !down).on_click(move |_, w, cx| stop(w, cx)))
                .child(btn("restart", "Restart", up, !down).on_click(move |_, w, cx| restart(w, cx))),
        )
}
