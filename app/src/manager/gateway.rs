// Copyright © 2016-2026 The SENTIENT Authors

//! Gateway: the Sentient Gateway on this computer (crate::gateway in the
//! installer core). Shows what is installed and whether it runs; installs it
//! when there is none, updates it to the newest gateway release on the
//! update channel, starts, stops and restarts it, and opens its local page.

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use sentient_installer_core::gateway::{self, Offer, Settings, Status};

use crate::gateway_record::GatewayRecord;
use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};
use crate::updates::UpdateState;

const REFRESH: Duration = Duration::from_secs(5);

pub struct GatewayPage {
    status: Option<Status>,
    /// The gateway the Platform Manager was asked to manage (crate::gateway_record).
    record: Option<GatewayRecord>,
    /// The job that runs now: shown under the buttons when it ends.
    action: Option<&'static str>,
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
            let (status, record) = cx.background_executor().spawn(async { (local_status(), crate::gateway_record::load()) }).await;
            if this.update(cx, |p, cx| {
                let first = p.status.is_none();
                p.status = Some(status);
                p.record = record;
                // Screens: the removal dialog open (SPM_DEBUG_GATEWAY_REMOVE=1).
                if first && cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_GATEWAY_REMOVE").is_some() && p.status.as_ref().is_some_and(|s| s.installed) {
                    p.open_remove(cx);
                }
                cx.notify();
            }).is_err() {
                break;
            }
            cx.background_executor().timer(REFRESH).await;
        });
        updates.update(cx, |u, cx| {
            if u.found.is_none() {
                u.check(cx);
            }
        });
        Self { status: None, record: None, action: None, updates, host, port, token, password, password2, problem: None, job, _subscriptions: subs, _poll: poll }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (status, record) = cx.background_executor().spawn(async { (local_status(), crate::gateway_record::load()) }).await;
            this.update(cx, |p, cx| { p.status = Some(status); p.record = record; cx.notify(); }).ok();
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
        self.action = Some(action);
        self.job.update(cx, |job, cx| job.start(move |sink| gateway::control(&sink, action), cx));
    }

    /// "Manage it here": record the gateway as it is (nothing about it changes).
    fn adopt(&mut self, cx: &mut Context<Self>) {
        self.action = Some("adopt");
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    crate::gateway_record::adopt().map(|m| sink(sentient_installer_core::progress::Progress::Done { message: m }))
                },
                cx,
            )
        });
    }

    fn remove(&mut self, keep: bool, image: bool, cx: &mut Context<Self>) {
        let Some(status) = self.status.clone() else { return };
        self.action = Some("remove");
        // One password prompt for all of it on Linux: a service, or a record
        // to clear, needs root.
        let as_root = cfg!(target_os = "linux") && !crate::rootjob::is_root() && (status.container.is_none() || self.record.is_some());
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    if as_root {
                        let mut args = vec!["gateway-remove".to_string()];
                        if !keep {
                            args.push("--delete-data".into());
                        }
                        if image {
                            args.push("--remove-image".into());
                        }
                        crate::rootjob::run(&args, &sink).map(|_| ())
                    } else {
                        let msg = gateway::remove(&sink, keep, image)?;
                        let _ = crate::gateway_record::save(None);
                        sink(sentient_installer_core::progress::Progress::Done { message: msg });
                        Ok(())
                    }
                },
                cx,
            )
        });
    }

    fn open_remove(&mut self, cx: &mut Context<Self>) {
        let Some(status) = self.status.clone() else { return };
        let this = cx.entity().downgrade();
        open_remove_dialog(
            RemoveTarget::of(&status, None),
            std::rc::Rc::new(move |keep, image, cx: &mut App| {
                this.update(cx, |p, cx| p.remove(keep, image, cx)).ok();
            }),
            cx,
        );
    }
}

/// What "Remove the Gateway…" removes, for its dialog: here or on a server.
#[derive(Clone)]
pub(crate) struct RemoveTarget {
    /// docker, service, process or windows-service.
    pub kind: String,
    pub name: String,
    pub image: Option<String>,
    pub volumes: Vec<String>,
    pub package: Option<String>,
    /// The server's name; None: this computer.
    pub server: Option<String>,
}

impl RemoveTarget {
    pub fn of(s: &Status, server: Option<String>) -> Self {
        RemoveTarget {
            kind: s.kind().to_string(),
            name: s.container.clone().or_else(|| s.service.clone()).or_else(|| s.process.clone()).unwrap_or_else(|| "SentientGateway".into()),
            image: s.image.clone(),
            volumes: s.volumes.clone(),
            package: s.package.clone(),
            server,
        }
    }

    /// From a server's `gateway-status` line.
    pub fn of_remote(get: &dyn Fn(&str) -> String, server: String) -> Self {
        let kind = get("kind");
        let name = match kind.as_str() {
            "docker" => get("container"),
            "service" => get("service"),
            "process" => get("process"),
            _ => "SentientGateway".into(),
        };
        let some = |v: String| (!v.is_empty()).then_some(v);
        RemoveTarget {
            kind,
            name,
            image: some(get("image")),
            volumes: get("volumes").split(',').filter(|v| !v.is_empty()).map(str::to_string).collect(),
            package: some(get("package")),
            server: Some(server),
        }
    }

    /// What the removal does, in one sentence.
    fn what(&self) -> String {
        let place = self.server.as_ref().map(|s| format!(" on {s}")).unwrap_or_default();
        match self.kind.as_str() {
            "docker" => format!("The Docker container {} is stopped and removed{place}. A compose file it came from is not changed.", self.name),
            "service" => match &self.package {
                Some(p) => format!("The service {} is stopped and disabled, and the package {p} is removed{place}.", self.name),
                None => format!("The service {} is stopped and disabled, and its unit file and program are removed{place}.", self.name),
            },
            "windows-service" => format!("The Windows service SentientGateway is stopped and the gateway is uninstalled{place}."),
            _ => format!("The gateway's program {} is removed{place}.", self.name),
        }
    }

    /// Where "Keep its configuration and data" keeps them.
    fn kept_where(&self) -> String {
        match self.kind.as_str() {
            "docker" if !self.volumes.is_empty() => format!("Its volumes are kept: {}. A new gateway container can use them again.", self.volumes.join(", ")),
            "docker" => "The container has no volumes: its configuration lives in the container and goes with it.".into(),
            "windows-service" => "Moved to a dated folder: ProgramData\\Sentient Gateway kept <date>.".into(),
            _ => "The configuration (/etc/sentient-gateway, with the platform's access token) and the local buffer (/var/lib/sentient-gateway) are moved to a dated folder: /var/backups/sentient-gateway-<date>-<time>.".into(),
        }
    }
}

/// "Remove the Gateway…": what goes, the "Keep its configuration and data"
/// switch (on), and for a container the image too (off). `on_ok(keep, image)`.
pub(crate) fn open_remove_dialog(target: RemoveTarget, on_ok: std::rc::Rc<dyn Fn(bool, bool, &mut App)>, cx: &mut App) {
    let view = cx.new(|_| RemoveDialog { target, keep: true, image: false, on_ok });
    crate::modal::open(crate::modal::Modal { title: "Remove the Sentient Gateway?".into(), body: view.into(), width: 520., icon: None, on_close: None }, cx);
}

struct RemoveDialog {
    target: RemoveTarget,
    keep: bool,
    image: bool,
    on_ok: std::rc::Rc<dyn Fn(bool, bool, &mut App)>,
}

impl Render for RemoveDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::{checkbox::Checkbox, switch::Switch};
        let theme = cx.theme().clone();
        let t = &self.target;
        let docker = t.kind == "docker";
        let ok = if self.keep { "Remove" } else { "Remove and delete the data" };
        let (keep, image) = (self.keep, self.image);
        let on_ok = self.on_ok.clone();
        v_flex()
            .gap_4()
            .child(div().text_sm().text_color(theme.muted_foreground).child(t.what()))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .items_center()
                            .child(div().text_sm().child("Keep its configuration and data"))
                            .child(ui::info("gw-remove-keep".into(), t.kept_where().into())),
                    )
                    .child(Switch::new("gw-remove-keep-switch").checked(self.keep).on_click(cx.listener(|d, v: &bool, _, cx| {
                        d.keep = *v;
                        cx.notify();
                    }))),
            )
            .when(docker && t.image.is_some(), |b| {
                b.child(
                    Checkbox::new("gw-remove-image")
                        .label(format!("Also remove the image {}", t.image.clone().unwrap_or_default()))
                        .checked(self.image)
                        .on_change(cx.listener(|d, v: &bool, _, cx| {
                            d.image = *v;
                            cx.notify();
                        })),
                )
            })
            .when(!self.keep, |b| {
                b.child(ui::outcome(
                    &theme,
                    Tone::Bad,
                    if docker {
                        "Its volumes are deleted: the gateway's configuration, its access token and the data it buffered. This can't be undone."
                    } else {
                        "Its configuration, its access token and the data it buffered are deleted. This can't be undone."
                    },
                ))
            })
            .child(
                h_flex()
                    .gap(crate::ui::button_gap())
                    .justify_end()
                    .child(crate::ui::probed(crate::ui::button("gw-remove-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx))))
                    .child(crate::ui::probed(
                        h_flex()
                            .id("gw-remove-ok")
                            .h(px(32.))
                            .px_3()
                            .items_center()
                            .rounded(theme.radius)
                            .bg(theme.danger)
                            .text_color(gpui_kit::white())
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .hover(|d| d.opacity(0.9))
                            .child(ok)
                            .on_click(move |_, _, cx| {
                                crate::modal::close(cx);
                                on_ok(keep, image, cx);
                            }),
                    )),
            )
    }
}

/// How the gateway runs, for "Runs as": a container also says who manages it.
pub(crate) fn runs_as(kind: &str, name: &str, managed: bool, recorded: bool) -> String {
    match kind {
        "docker" if managed => format!("Docker container {name}"),
        "docker" if recorded => format!("Docker container {name}, managed here (it was installed outside the Platform Manager)"),
        "docker" => format!("Docker container {name}, installed outside the Platform Manager"),
        "service" if recorded => format!("Service {name}, managed here"),
        "service" => format!("Service {name}"),
        "process" => format!("A process ({name}), started outside a service"),
        _ => "Windows service SentientGateway".to_string(),
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

        let recorded = self.record.as_ref().is_some_and(|r| r.is(&status));
        // Someone else's gateway: a compose container, or a unit by hand.
        let outside = !status.managed && !recorded && status.kind() != "windows-service";
        let controllable = status.kind() != "process";
        let outcome = match (self.action, job.state()) {
            (Some("adopt" | "remove"), JobState::Done(m)) => Some((Tone::Good, m.clone())),
            (Some(_), JobState::Failed(m)) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let body = if status.installed {
            let (pill, tone) = if status.running { ("Gateway is running", Tone::Good) } else { ("Gateway is stopped", Tone::Bad) };
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(ui::product_logo(ui::Product::Gateway, 28.))
                        .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                        .child(ui::pill(&theme, tone, pill))
                        .child(div().flex_1())
                        .when_some(status.page_url.clone(), |r, url| {
                            r.child(crate::ui::button("gw-page").outline().label("Open local page").on_click(move |_, _, cx| ui::open_url(&url, cx)))
                        }),
                )
                .child(ui::fact(&theme, "Version", status.version.clone().unwrap_or_else(|| "unknown".into())))
                .child(ui::fact(
                    &theme,
                    "Sends to",
                    match (&status.host, status.port) {
                        (Some(h), Some(p)) => format!("{h}:{p}"),
                        (Some(h), None) => h.clone(),
                        _ => "Not commissioned (set the platform on its local page)".into(),
                    },
                ))
                .child(ui::fact(&theme, "Runs as", runs_as(status.kind(), &RemoveTarget::of(&status, None).name, status.managed, recorded)))
                .when_some(status.compose_dir.clone(), |p, dir| {
                    p.child(ui::fact(&theme, "Compose", match &status.compose_project {
                        Some(proj) => format!("{proj}, in {dir}"),
                        None => dir,
                    }))
                })
                .when(!status.volumes.is_empty(), |p| p.child(ui::fact(&theme, "Volumes", status.volumes.join(", "))))
                .child(match status.page_url.clone() {
                    Some(url) => ui::fact(&theme, "Web page", url),
                    None => ui::fact_help(&theme, "Web page", "Off", "The gateway's own web page is off. In a container it is turned on with ST_GW_UI=on."),
                })
                .children({
                    let this = cx.entity().downgrade();
                    let act = |action: &'static str| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
                        let this = this.clone();
                        std::rc::Rc::new(move |_, cx| {
                            this.update(cx, |p, cx| p.control(action, cx)).ok();
                        })
                    };
                    let running = Some(crate::manager::status::Running { serving: status.running, any_up: status.running });
                    let services = if controllable { crate::manager::status::service_buttons(&theme, "gw-", busy, running, "Stop", act("start"), act("restart"), act("stop")) } else { Vec::new() };
                    ui::card_foot(
                        &theme,
                        h_flex()
                            .gap(crate::ui::button_gap())
                            .flex_wrap()
                            .items_center()
                            .children(services.into_iter().map(crate::ui::probed))
                            .child(crate::ui::probed(crate::manager::status::logs_button("gw-", |_, _, cx| {
                                crate::logsource::request(crate::logsource::Source::Gateway);
                                cx.refresh_windows();
                            })))
                            .when(outside, |r| {
                                r.child(crate::ui::probed(
                                    h_flex().gap_1().items_center().child(crate::ui::button("gw-adopt").outline().label("Manage it here").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.adopt(cx)))).child(ui::info(
                                        "gw-adopt-help".into(),
                                        "Records this gateway as the one the Platform Manager manages: it lists it, starts and stops it, and offers its removal. Nothing about it changes: the container, its compose file and its volumes stay as they are.".into(),
                                    )),
                                ))
                            })
                            .child(div().flex_1())
                            .child(crate::ui::probed(crate::ui::button("gw-remove").danger().label("Remove the Gateway…").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.open_remove(cx))))),
                    )
                })
                .children(outcome.clone().map(|(tone, m)| ui::outcome(&theme, tone, m)))
                .child(match (&newer, &newest) {
                    (Some(o), _) => {
                        let o2 = o.clone();
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .child({
                                        let blocked = !status.managed || (status.container.is_none() && o.installer.is_none());
                                        let why = if !status.managed {
                                            format!("This container comes from a compose file: set its image to quakestring/sentient-gateway-rs:{} in that file and run docker compose up -d.", o.version)
                                        } else {
                                            "No installer for this system in this release.".to_string()
                                        };
                                        let button = crate::ui::button("gw-update").primary().label(format!("Update to {}", o.version)).disabled(busy || blocked).on_click(cx.listener(move |p, _, _, cx| p.update_to(o2.clone(), cx)));
                                        // A disabled button carries its reason as a tooltip.
                                        if blocked {
                                            div().id("gw-update-why").child(button).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(why.clone()).build(w, cx)).into_any_element()
                                        } else {
                                            button.into_any_element()
                                        }
                                    })
                                    .child(div().text_sm().text_color(theme.muted_foreground).child(format!("Released {}", o.date)))
                                    .child(ui::info("gw-update-help".into(), {
                                        let mut t = if status.managed { "The configuration and the web page's password are kept.".to_string() } else { String::new() };
                                        if !o.notes.is_empty() {
                                            if !t.is_empty() {
                                                t.push_str("\n\n");
                                            }
                                            t.push_str(&o.notes);
                                        }
                                        if t.is_empty() { "What's in it: see the gateway's change log.".into() } else { t.into() }
                                    })),
                            )
                            .into_any_element()
                    }
                    (None, Some(Err(e))) => div().text_sm().text_color(theme.muted_foreground).child(format!("Gateway updates: {e}")).into_any_element(),
                    // Up to date only against a known version.
                    (None, Some(Ok(_))) if status.version.is_none() => div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Its version is not known, so updates cannot be checked.")
                        .into_any_element(),
                    (None, Some(Ok(_))) => div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!("Up to date on the {} channel.", channel.label()))
                        .into_any_element(),
                    (None, None) => div().text_sm().text_color(theme.muted_foreground).child("Looking for gateway updates…").into_any_element(),
                })
        } else {
            let field = |label: &'static str, input: Div| {
                v_flex().gap_1p5().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(label)).child(input)
            };
            let o = offer.clone();
            ui::panel(&theme)
                .child(h_flex().gap_3().items_center().child(ui::product_logo(ui::Product::Gateway, 28.)).child(div().font_weight(FontWeight::MEDIUM).child("No gateway on this computer")))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "The gateway reads devices next to this computer. In SENTIENT, add a gateway device and copy its access token (Manage credentials).",
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
                .children(outcome.clone().map(|(tone, m)| ui::outcome(&theme, tone, m)))
                .children(self.problem.clone().map(|p| ui::outcome(&theme, Tone::Bad, p)))
                .child(match o {
                    Some(o) => {
                        let label = format!("Install Sentient Gateway {}", o.version);
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                crate::ui::button("gw-install")
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

        page.child(body)
    }
}

/// Repair and uninstall's Gateway choice: the gateway found, and its removal.
pub(crate) fn remove_panel(theme: &gpui_kit::component::Theme, id: &'static str, runs_as: String, button: AnyElement) -> Div {
    ui::panel(theme)
        .child(h_flex().gap_2().items_center().child(ui::product_logo(ui::Product::Gateway, 24.)).child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway")))
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().text_sm().text_color(theme.muted_foreground).child(runs_as))
                .child(ui::info(SharedString::from(format!("{id}-help")), "Remove the Gateway stops it and removes it. Its configuration and data are kept unless you choose otherwise. SENTIENT is not touched.".into())),
        )
        .child(ui::button_row(vec![button]))
}

/// The gateway on this computer (gateway::status). Screens: SPM_DEBUG_GATEWAY
/// holds a canned `gateway-status` line (`none` for no gateway).
pub(crate) fn local_status() -> Status {
    sentient_installer_core::distro::gateway_docker::set_cache_dir(crate::updates::prefs_dir());
    #[cfg(debug_assertions)]
    if let Ok(t) = std::env::var("SPM_DEBUG_GATEWAY") {
        let kv: std::collections::HashMap<String, String> = t.split([' ', '+']).filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.replace("%20", " "))).collect();
        return status_of(&|k| kv.get(k).cloned().unwrap_or_default());
    }
    gateway::status()
}

/// A Status from a `gateway-status` line's values.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
fn status_of(get: &dyn Fn(&str) -> String) -> Status {
    let some = |k: &str| Some(get(k)).filter(|v| !v.is_empty());
    let kind = get("kind");
    Status {
        installed: get("installed") == "true",
        version: some("version"),
        running: get("running") == "true",
        host: some("host"),
        port: get("port").parse().ok(),
        page_url: some("page").filter(|p| p != "off"),
        container: some("container").filter(|_| kind == "docker"),
        managed: get("managed") == "true",
        service: some("service"),
        process: some("process"),
        image: some("image"),
        compose_project: some("project"),
        compose_dir: some("compose"),
        volumes: get("volumes").split(',').filter(|v| !v.is_empty()).map(str::to_string).collect(),
        program: some("program"),
        package: some("package"),
    }
}
