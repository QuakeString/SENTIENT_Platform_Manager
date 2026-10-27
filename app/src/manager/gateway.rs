// Copyright © 2016-2026 The SENTIENT Authors

//! Gateway: the Sentient Gateway on this computer (crate::gateway in the
//! installer core). Shows what is installed and whether it runs; installs it
//! when there is none, updates it to the newest gateway release on the
//! update channel, starts, stops and restarts it, and opens its local page.

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    progress::Progress,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use sentient_installer_core::gateway::{self, Offer, Settings, Status};

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};
use crate::updates::UpdateState;

const REFRESH: Duration = Duration::from_secs(5);

pub struct GatewayPage {
    status: Option<Status>,
    updates: Entity<UpdateState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    token: Entity<InputState>,
    password: Entity<InputState>,
    password2: Entity<InputState>,
    problem: Option<SharedString>,
    job: Entity<Job>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl GatewayPage {
    pub fn new(record: Option<InstallationRecord>, updates: Entity<UpdateState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // This SENTIENT's MQTT, the usual place for a gateway on the same computer.
        let port = record.as_ref().map(|r| r.stack.mqtt_port).unwrap_or(1883);
        let input = |cx: &mut Context<Self>, window: &mut Window, value: String| cx.new(|cx| InputState::new(window, cx).default_value(value));
        let host = input(cx, window, "127.0.0.1".into());
        let port = input(cx, window, port.to_string());
        let token = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("From SENTIENT: the gateway device, Manage credentials"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("8 characters or more"));
        let password2 = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Again"));
        let job = cx.new(|_| Job::new());
        let mut subs = vec![
            cx.observe(&job, |page: &mut Self, job, cx| {
                if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                    page.refresh(cx);
                    page.updates.update(cx, |u, cx| u.check(cx));
                }
                cx.notify();
            }),
            cx.observe(&updates, |_, _, cx| cx.notify()),
        ];
        for i in [&host, &port, &token, &password, &password2] {
            subs.push(cx.subscribe(i, |_, _, _: &InputEvent, cx| cx.notify()));
        }
        let poll = cx.spawn(async move |this, cx| loop {
            let status = cx.background_executor().spawn(async { gateway::status() }).await;
            if this.update(cx, |p, cx| { p.status = Some(status); cx.notify(); }).is_err() {
                break;
            }
            cx.background_executor().timer(REFRESH).await;
        });
        updates.update(cx, |u, cx| {
            if u.found.is_none() {
                u.check(cx);
            }
        });
        Self { status: None, updates, host, port, token, password, password2, problem: None, job, _subscriptions: subs, _poll: poll }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let status = cx.background_executor().spawn(async { gateway::status() }).await;
            this.update(cx, |p, cx| { p.status = Some(status); cx.notify(); }).ok();
        })
        .detach();
    }

    fn value(&self, state: &Entity<InputState>, cx: &App) -> String {
        state.read(cx).value().trim().to_string()
    }

    fn install(&mut self, offer: Offer, cx: &mut Context<Self>) {
        let port: u16 = match self.value(&self.port, cx).parse() {
            Ok(p) if p > 0 => p,
            _ => {
                self.problem = Some("The MQTT port is a number from 1 to 65535.".into());
                return;
            }
        };
        let host = self.value(&self.host, cx);
        let token = self.value(&self.token, cx);
        let (p1, p2) = (self.value(&self.password, cx), self.value(&self.password2, cx));
        if host.is_empty() || token.is_empty() {
            self.problem = Some("Enter the platform host and the gateway device's access token.".into());
            return;
        }
        if cfg!(windows) && (!p1.is_empty() || !p2.is_empty()) {
            if p1.len() < 8 {
                self.problem = Some("The local page's password needs 8 characters or more.".into());
                return;
            }
            if p1 != p2 {
                self.problem = Some("The two passwords differ.".into());
                return;
            }
        }
        self.problem = None;
        let settings = Settings { host, port, token, ui_password: p1 };
        self.job.update(cx, |job, cx| job.start(move |sink| gateway::install(&sink, &offer, &settings), cx));
    }

    fn update_to(&mut self, offer: Offer, cx: &mut Context<Self>) {
        self.job.update(cx, |job, cx| job.start(move |sink| gateway::update(&sink, &offer), cx));
    }

    fn control(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.job.update(cx, |job, cx| job.start(move |sink| gateway::control(&sink, action), cx));
    }
}

impl Render for GatewayPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Gateway",
            "The Sentient Gateway on this computer: it collects data from the plant's devices and sends it to SENTIENT.",
        );
        let u = self.updates.read(cx);
        let newest = u.found.as_ref().map(|f| f.gateway.clone());
        let channel = u.prefs.channel;
        let job = self.job.read(cx);
        let busy = job.is_running();
        let step = job.step_with_time();
        let percent = job.percent();
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let log: Vec<SharedString> = job.log().to_vec();

        let Some(status) = self.status.clone() else {
            return page.child(ui::panel(&theme).child(div().text_sm().text_color(theme.muted_foreground).child("Looking for the gateway…")));
        };
        let offer: Option<Offer> = match &newest {
            Some(Ok(Some(o))) => Some(o.clone()),
            _ => None,
        };
        let newer = offer
            .clone()
            .filter(|o| status.installed && status.version.as_deref().is_some_and(|v| sentient_installer_core::releases::is_newer(&o.version, v, false)));

        let body = if status.installed {
            let (pill, tone) = if status.running { ("Running", Tone::Good) } else { ("Stopped", Tone::Bad) };
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(ui::pill(&theme, tone, pill))
                        .child(div().flex_1())
                        .when_some(status.page_url.clone(), |r, url| {
                            r.child(Button::new("gw-page").outline().label("Open local page").on_click(move |_, _, cx| ui::open_url(&url, cx)))
                        }),
                )
                .child(ui::fact(&theme, "Version", status.version.clone().unwrap_or_else(|| "unknown".into())))
                .child(ui::fact(
                    &theme,
                    "Sends to",
                    match (&status.host, status.port) {
                        (Some(h), Some(p)) => format!("{h}:{p}"),
                        (Some(h), None) => h.clone(),
                        _ => "nowhere yet: not commissioned (set the platform on its local page)".into(),
                    },
                ))
                .child(ui::fact(
                    &theme,
                    "Runs as",
                    match (&status.container, status.managed) {
                        (None, _) => "Windows service SentientGateway".to_string(),
                        (Some(c), true) => format!("Docker container {c}"),
                        (Some(c), false) => format!("Docker container {c} (from its own compose file)"),
                    },
                ))
                .child(ui::fact(
                    &theme,
                    "Local page",
                    status.page_url.clone().unwrap_or_else(|| "off (in a container it is on with ST_GW_UI=on)".into()),
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("gw-start").outline().label("Start").disabled(busy || status.running).on_click(cx.listener(|p, _, _, cx| p.control("start", cx))))
                        .child(Button::new("gw-stop").outline().label("Stop").disabled(busy || !status.running).on_click(cx.listener(|p, _, _, cx| p.control("stop", cx))))
                        .child(Button::new("gw-restart").outline().label("Restart").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.control("restart", cx)))),
                )
                .child(match (&newer, &newest) {
                    (Some(o), _) => {
                        let o2 = o.clone();
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .child(
                                        Button::new("gw-update")
                                            .primary()
                                            .label(format!("Update to {}", o.version))
                                            .disabled(busy || !status.managed || (status.container.is_none() && o.installer.is_none()))
                                            .on_click(cx.listener(move |p, _, _, cx| p.update_to(o2.clone(), cx))),
                                    )
                                    .child(div().text_sm().text_color(theme.muted_foreground).child(if status.managed {
                                        format!("released {}; the configuration and the local page's password are kept", o.date)
                                    } else {
                                        format!(
                                            "released {}; this container comes from a compose file: set its image to quakestring/sentient-gateway-rs:{} there and run docker compose up -d",
                                            o.date, o.version
                                        )
                                    })),
                            )
                            .when(!o.notes.is_empty(), |c| c.child(div().text_xs().text_color(theme.muted_foreground).child(o.notes.clone())))
                            .into_any_element()
                    }
                    (None, Some(Err(e))) => div().text_sm().text_color(theme.muted_foreground).child(format!("Gateway updates: {e}")).into_any_element(),
                    (None, Some(Ok(_))) => div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!("Up to date on the {} channel.", channel.label()))
                        .into_any_element(),
                    (None, None) => div().text_sm().text_color(theme.muted_foreground).child("Looking for gateway updates…").into_any_element(),
                })
        } else {
            let field = |label: &'static str, input: Div| {
                v_flex().gap_1p5().child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(label)).child(input)
            };
            let o = offer.clone();
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("No gateway on this computer"))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "Install one here to read devices next to this computer. In SENTIENT, add a gateway device and copy its access token (Manage credentials).",
                ))
                .child(
                    h_flex()
                        .gap_3()
                        .child(field("Platform host (MQTT)", div().child(Input::new(&self.host))).flex_1())
                        .child(field("Port", div().child(Input::new(&self.port))).w_24()),
                )
                .child(field("Access token", div().child(Input::new(&self.token).mask_toggle())))
                .when(cfg!(windows), |p| {
                    p.child(
                        h_flex()
                            .gap_3()
                            .child(field("Local page password", div().child(Input::new(&self.password).mask_toggle())).flex_1())
                            .child(field("Again", div().child(Input::new(&self.password2).mask_toggle())).flex_1()),
                    )
                })
                .children(self.problem.clone().map(|p| ui::outcome(&theme, Tone::Bad, p)))
                .child(match o {
                    Some(o) => {
                        let label = format!("Install Sentient Gateway {}", o.version);
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                Button::new("gw-install")
                                    .primary()
                                    .label(label)
                                    .disabled(busy || (cfg!(windows) && o.installer.is_none()))
                                    .on_click(cx.listener(move |p, _, _, cx| p.install(o.clone(), cx))),
                            )
                            .child(div().text_sm().text_color(theme.muted_foreground).child(if cfg!(windows) {
                                "the installer from the SENTIENT releases site, checked before use"
                            } else {
                                "the quakestring/sentient-gateway-rs image, run by Docker"
                            }))
                            .into_any_element()
                    }
                    None => div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(match &newest {
                            Some(Err(e)) => format!("The gateway releases could not be read: {e}"),
                            Some(Ok(_)) => "No gateway release on this channel.".to_string(),
                            None => "Looking for the newest gateway…".to_string(),
                        })
                        .into_any_element(),
                })
        };

        page.child(body).when(busy || outcome.is_some() || !log.is_empty(), |page| {
            page.child(
                ui::panel(&theme)
                    .when(busy, |p| p.child(div().text_sm().child(step.clone())).child(Progress::new("gw-progress").value(percent.unwrap_or(0.0))))
                    .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                    .children(log.into_iter().map(|line| div().text_xs().text_color(theme.muted_foreground).child(line))),
            )
        })
    }
}
