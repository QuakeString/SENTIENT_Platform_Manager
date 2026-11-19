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

use sentient_installer_core::gateway_settings::{self as gs, GwSettings};

use crate::gateway_record::GatewayRecord;
use crate::manager::gateway_settings::{GatewaySettingsCard, Work};
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
    form: InstallForm,
    /// A removal or an install runs: once it went through, the form is
    /// cleared (once).
    clear_armed: bool,
    problem: Option<SharedString>,
    job: Entity<Job>,
    /// The settings card (crate::manager::gateway_settings), what it shows,
    /// when and of which gateway it was read.
    card: Entity<GatewaySettingsCard>,
    settings: Option<Result<GwSettings, String>>,
    settings_read: Option<(String, std::time::Instant)>,
    /// The settings' Apply and password job, shown in place of the card
    /// (crate::manager::jobview) until "Back to Gateway".
    settings_job: Entity<Job>,
    shown: Option<String>,
    /// This computer's SENTIENT, for the card's "Open in SENTIENT".
    sentient_url: Option<String>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

/// The card's values are read again at most this often while the page shows.
const SETTINGS_REREAD: Duration = Duration::from_secs(30);

impl GatewayPage {
    pub fn new(record: Option<InstallationRecord>, updates: Entity<UpdateState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // This SENTIENT's MQTT, the usual place for a gateway on the same computer.
        let port = record.as_ref().map(|r| r.stack.mqtt_port).unwrap_or(1883);
        let sentient_url = record.as_ref().map(|r| r.url());
        let form = InstallForm::new("127.0.0.1", port, window, cx);
        let job = cx.new(|_| Job::new());
        let settings_job = cx.new(|_| Job::new());
        let me = cx.entity().downgrade();
        let card = cx.new(|_| {
            GatewaySettingsCard::new(
                None,
                std::rc::Rc::new(move |w: Work, window: &mut Window, cx: &mut App| {
                    me.update(cx, |p, cx| p.run_work(w, window, cx)).ok();
                }),
            )
        });
        let mut subs = vec![
            cx.observe(&settings_job, |page: &mut Self, job, cx| {
                if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                    page.settings_read = None;
                    page.card.update(cx, |c, _| c.reset());
                    page.refresh(cx);
                }
                cx.notify();
            }),
            cx.observe(&job, |page: &mut Self, job, cx| {
                // The gateway removed (or installed): its old token and
                // password leave the form.
                if clears_form(page.clear_armed, job.read(cx).state()) {
                    page.form.clear();
                }
                if !job.read(cx).is_running() {
                    page.clear_armed = false;
                }
                if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                    page.refresh(cx);
                    page.updates.update(cx, |u, cx| u.check(cx));
                }
                cx.notify();
            }),
            cx.observe(&updates, |_, _, cx| cx.notify()),
        ];
        for i in form.inputs() {
            subs.push(cx.subscribe(i, |_, _, _: &InputEvent, cx| cx.notify()));
        }
        let poll = cx.spawn(async move |this, cx| loop {
            // Hidden (another page, or a server open): no reading.
            super::until_shown(super::Page::Gateway, cx).await;
            let (status, record) = cx.background_executor().spawn(async { (local_status(), crate::gateway_record::load()) }).await;
            if this.update(cx, |p, cx| {
                let first = p.status.is_none();
                p.read_settings_if_due(&status, cx);
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
        Self {
            status: None,
            record: None,
            action: None,
            updates,
            form,
            clear_armed: false,
            problem: None,
            job,
            card,
            settings: None,
            settings_read: None,
            settings_job,
            shown: None,
            sentient_url,
            _subscriptions: subs,
            _poll: poll,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (status, record) = cx.background_executor().spawn(async { (local_status(), crate::gateway_record::load()) }).await;
            this.update(cx, |p, cx| {
                p.read_settings_if_due(&status, cx);
                p.status = Some(status);
                p.record = record;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read the card's settings again: the first time, for another gateway
    /// (or the same one started or stopped), after a job, and every
    /// SETTINGS_REREAD. In the background; nothing is read while a
    /// settings job runs.
    fn read_settings_if_due(&mut self, status: &Status, cx: &mut Context<Self>) {
        if !status.installed {
            self.settings = None;
            self.settings_read = None;
            return;
        }
        if self.settings_job.read(cx).is_running() {
            return;
        }
        let key = format!("{}|{}|{}|{}", status.kind(), RemoveTarget::of(status, None).name, status.running, status.config_unreadable);
        let due = self.settings_read.as_ref().is_none_or(|(k, at)| *k != key || at.elapsed() > SETTINGS_REREAD);
        if !due {
            return;
        }
        self.settings_read = Some((key, std::time::Instant::now()));
        let s = status.clone();
        cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { local_settings(&s) }).await;
            this.update(cx, |p, cx| {
                p.settings = Some(got);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// What the settings card asks for: Apply and the password as a job
    /// shown in place of the card; the test in the background, its answer
    /// on the card. A native gateway's run as root (crate::rootjob).
    fn run_work(&mut self, work: Work, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = self.status.clone() else { return };
        let as_root = status.kind() == "service" && cfg!(target_os = "linux") && !crate::rootjob::is_root();
        // Screens: a canned gateway is never changed; the job says so.
        let canned = cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_GATEWAY").is_some();
        match work {
            Work::Test => {
                let card = self.card.clone();
                cx.spawn(async move |_, cx| {
                    let r = if canned { Ok("Screens: a canned gateway; nothing was tested.".to_string()) } else { cx.background_executor().spawn(async move { test_here(&status, as_root) }).await };
                    card.update(cx, |c, cx| c.set_test(Some(r), cx));
                })
                .detach();
            }
            Work::Apply(plan) => {
                if self.settings_job.read(cx).is_running() {
                    return;
                }
                self.shown = Some("Applying the gateway's settings".into());
                let names = format!("{}. {}", plan.names().join(", "), plan.restart_words(status.kind()));
                self.settings_job.update(cx, |job, cx| {
                    job.set_label("Applying the gateway's settings");
                    job.set_plan(vec![
                        sentient_installer_core::progress::PlanStep::new("Applying the settings").detail(names).weight(3.),
                        sentient_installer_core::progress::PlanStep::new("Reading the settings back"),
                    ]);
                    job.set_on_page(true);
                    job.start(
                        move |sink| {
                            sink(sentient_installer_core::progress::Progress::Step { name: "Applying the settings".into() });
                            if canned {
                                sink(sentient_installer_core::progress::Progress::Done { message: "Screens: a canned gateway; nothing was changed.".into() });
                                return Ok(());
                            }
                            if as_root {
                                let path = crate::updates::prefs_dir().join("gateway-apply.json");
                                let text = serde_json::to_string(&plan).map_err(|e| e.to_string())?;
                                let _ = std::fs::create_dir_all(crate::updates::prefs_dir());
                                std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
                                let r = crate::rootjob::run(&["gateway-apply".to_string(), path.display().to_string()], &sink).map(|_| ());
                                let _ = std::fs::remove_file(&path);
                                r
                            } else {
                                gs::apply(&sink, &status, &plan)
                            }
                        },
                        cx,
                    )
                });
            }
            Work::Password(pw) => {
                if self.settings_job.read(cx).is_running() {
                    return;
                }
                self.shown = Some("Changing the web page's password".into());
                self.settings_job.update(cx, |job, cx| {
                    job.set_label("Changing the gateway page's password");
                    job.set_plan(vec![sentient_installer_core::progress::PlanStep::new("Setting the password")]);
                    job.set_on_page(true);
                    job.start(
                        move |sink| {
                            sink(sentient_installer_core::progress::Progress::Step { name: "Setting the password".into() });
                            if canned {
                                sink(sentient_installer_core::progress::Progress::Done { message: "Screens: a canned gateway; nothing was changed.".into() });
                                return Ok(());
                            }
                            if as_root {
                                crate::rootjob::run_with_input(&["gateway-ui-password".to_string()], Some(format!("{pw}\n").as_str()), &sink).map(|_| ())
                            } else {
                                gs::set_password(&status, &pw).map(|m| sink(sentient_installer_core::progress::Progress::Done { message: m }))
                            }
                        },
                        cx,
                    )
                });
            }
        }
        cx.notify();
    }

    /// The settings job, in place of the card: running, or ended and not
    /// yet left with "Back to Gateway".
    fn settings_job_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let title = self.shown.clone()?;
        let job = self.settings_job.read(cx);
        if matches!(job.state(), JobState::Idle) {
            return None;
        }
        let this = cx.entity().downgrade();
        let back = crate::manager::jobview::back_button("gw-job-back", "Gateway", move |_, cx| {
            this.update(cx, |p, cx| {
                p.shown = None;
                p.settings_job.update(cx, |j, cx| j.reset(cx));
                cx.notify();
            })
            .ok();
        });
        Some(crate::manager::jobview::JobView::new("gw-set-job", title, &self.settings_job).actions([back]).render(cx))
    }

    fn install(&mut self, offer: Offer, cx: &mut Context<Self>) {
        let settings = match self.form.settings(cx, cfg!(windows)) {
            Ok(s) => s,
            Err(e) => {
                self.problem = Some(e.into());
                return;
            }
        };
        self.problem = None;
        self.action = Some("install");
        self.clear_armed = true;
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

    /// "Read its settings": the gateway's settings folder is closed to this
    /// account, so an administrator reads them (one password prompt; the
    /// root helper then serves the page's later reads for a while).
    fn read_as_admin(&mut self, cx: &mut Context<Self>) {
        self.action = Some("read");
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    let s = admin_read()?;
                    sink(sentient_installer_core::progress::Progress::Done {
                        message: match (&s.host, s.port) {
                            (Some(h), Some(p)) => format!("Read as administrator: it sends to {h}:{p}."),
                            _ => "Read as administrator: it is not commissioned yet.".to_string(),
                        },
                    });
                    Ok(())
                },
                cx,
            )
        });
    }

    fn remove(&mut self, keep: bool, image: bool, cx: &mut Context<Self>) {
        let Some(status) = self.status.clone() else { return };
        self.action = Some("remove");
        self.clear_armed = true;
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
                        crate::ui::solid_button("gw-remove-ok", ok.into(), theme.danger, gpui_kit::white(), move |_, _, cx| {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.form.apply_clear(window, cx);
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Gateway",
            "The Sentient Gateway on this computer: it collects data from the plant's devices and sends it to SENTIENT.",
        );
        let u = self.updates.read(cx);
        let newest = u.found.as_ref().map(|f| f.gateway.clone());
        // The offer is from an old release list (GitHub refused): said quietly.
        let stale = u.stale_note().filter(|_| matches!(newest, Some(Ok(Some(_)))));
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
        // Closed to this account (a native gateway's 0700 folder) and not
        // read as administrator yet.
        let unreadable = status.config_unreadable;
        // Someone else's gateway: a compose container, or a unit by hand.
        let outside = !status.managed && !recorded && status.kind() != "windows-service";
        let controllable = status.kind() != "process";
        let outcome = match (self.action, job.state()) {
            (Some("adopt" | "remove" | "read" | "update"), JobState::Done(m)) => Some((Tone::Good, m.clone())),
            (Some(_), JobState::Failed(m)) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        // The older Python gateway beside the Rust one: one line.
        let legacy = status.legacy.clone().filter(|_| status.installed).map(|n| div().text_sm().text_color(theme.muted_foreground).child(legacy_line(&n, "this computer")));
        let body = if !status.installed && status.legacy.is_some() {
            // The older Python gateway alone: offered as an update.
            let o = offer.clone();
            let why = o.as_ref().and_then(|o| gateway::offer_blocker(&status, o, cfg!(windows)));
            let looking = match &newest {
                Some(Err(e)) => Some(format!("Gateway updates: {e}")),
                Some(Ok(None)) => Some("No gateway release on this channel.".to_string()),
                _ => None,
            };
            let this = cx.entity().downgrade();
            let o2 = o.clone();
            legacy_panel(
                &theme,
                "gw-older",
                status.legacy.as_deref().unwrap_or_default(),
                status.legacy_image.as_deref(),
                o.as_ref(),
                looking,
                why,
                busy,
                std::rc::Rc::new(move |_, cx: &mut App| {
                    if let Some(o) = o2.clone() {
                        this.update(cx, |p, cx| {
                            p.action = Some("update");
                            p.update_to(o, cx)
                        })
                        .ok();
                    }
                }),
            )
            .children(outcome.clone().map(|(tone, m)| ui::outcome(&theme, tone, m)))
        } else if status.installed {
            let (pill, tone) = if status.running { ("Gateway is running", Tone::Good) } else { ("Gateway is stopped", Tone::Bad) };
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_3()
                        // A narrow window: "Open local page" goes to its own line.
                        .flex_wrap()
                        .items_center()
                        .child(ui::product_logo(ui::Product::Gateway, 28.))
                        .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                        .child(ui::pill(&theme, tone, pill))
                        .children(crate::manager::gateway_settings::connected_pill(&theme, self.settings.as_ref(), status.running))
                        .child(div().flex_1())
                        .when_some(status.page_url.clone(), |r, url| {
                            r.child(crate::ui::button("gw-page").outline().label("Open local page").on_click(move |_, _, cx| ui::open_url(&url, cx)))
                        }),
                )
                .children(legacy)
                .child(ui::fact(&theme, "Version", status.version.clone().unwrap_or_else(|| "unknown".into())))
                .child(match (&status.host, status.port) {
                    (Some(h), Some(p)) => ui::fact(&theme, "Sends to", format!("{h}:{p}")),
                    (Some(h), None) => ui::fact(&theme, "Sends to", h.clone()),
                    _ if unreadable => ui::fact_help(&theme, "Sends to", "Readable by an administrator only", UNREADABLE_HELP),
                    _ => ui::fact(&theme, "Sends to", "Not commissioned (set the platform on its local page)"),
                })
                .child(ui::fact(&theme, "Runs as", runs_as(status.kind(), &RemoveTarget::of(&status, None).name, status.managed, recorded)))
                .children(self.settings.as_ref().and_then(|s| s.as_ref().ok()).and_then(|s| {
                    let path = s.config_path.clone()?;
                    let n = s.connectors.len();
                    Some(ui::fact(&theme, "Configuration", format!("{path} · {n} connector{}", if n == 1 { "" } else { "s" })))
                }))
                .when_some(status.compose_dir.clone(), |p, dir| {
                    p.child(ui::fact(&theme, "Compose", match &status.compose_project {
                        Some(proj) => format!("{proj}, in {dir}"),
                        None => dir,
                    }))
                })
                .when(!status.volumes.is_empty(), |p| p.child(ui::fact(&theme, "Volumes", status.volumes.join(", "))))
                .child(match status.page_url.clone() {
                    Some(url) => ui::fact(&theme, "Web page", url),
                    None if unreadable => ui::fact_help(&theme, "Web page", "Not known", "Nothing answers on its usual address (127.0.0.1:8642). Its settings may name another one: they are readable by an administrator only."),
                    None => ui::fact_help(&theme, "Web page", "Off", page_off_help(status.kind())),
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
                    ui::card_actions(
                        &theme,
                        h_flex()
                            .gap(crate::ui::button_gap())
                            .flex_wrap()
                            // A wrapped line ends at the right too.
                            .justify_end()
                            .items_center()
                            .child(crate::ui::probed(crate::ui::button("gw-remove").danger().label("Remove the Gateway").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.open_remove(cx)))))
                            .when(unreadable, |r| {
                                r.child(crate::ui::probed(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(crate::ui::button("gw-read").outline().label("Read its settings").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.read_as_admin(cx))))
                                        .child(ui::info("gw-read-help".into(), UNREADABLE_HELP.into())),
                                ))
                            })
                            .when(outside, |r| {
                                r.child(crate::ui::probed(
                                    h_flex().gap_1().items_center().child(crate::ui::button("gw-adopt").outline().label("Manage it here").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.adopt(cx)))).child(ui::info(
                                        "gw-adopt-help".into(),
                                        "Records this gateway as the one the Platform Manager manages: it lists it, starts and stops it, and offers its removal. Nothing about it changes: the container, its compose file and its volumes stay as they are.".into(),
                                    )),
                                ))
                            })
                            .child(crate::ui::probed(crate::manager::status::logs_button("gw-", |_, _, cx| {
                                crate::logsource::request(crate::logsource::Source::Gateway);
                                cx.refresh_windows();
                            })))
                            .children(services.into_iter().map(crate::ui::probed)),
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
                                        let why = gateway::offer_blocker(&status, o, cfg!(windows)).unwrap_or_default();
                                        let blocked = !why.is_empty();
                                        let button = crate::ui::button("gw-update").primary().label(format!("Update to {}", o.version)).disabled(busy || blocked).on_click(cx.listener(move |p, _, _, cx| p.update_to(o2.clone(), cx)));
                                        // A disabled button carries its reason as a tooltip.
                                        if blocked {
                                            div().id("gw-update-why").child(button).tooltip(crate::ui::tooltip(why.clone())).into_any_element()
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
                .children(stale.clone().map(|n| div().text_size(px(13.)).text_color(theme.muted_foreground).child(n)))
        } else {
            let o = offer.clone();
            ui::panel(&theme)
                .child(h_flex().gap_3().items_center().child(ui::product_logo(ui::Product::Gateway, 28.)).child(div().font_weight(FontWeight::MEDIUM).child("No gateway on this computer")))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "The gateway reads devices next to this computer. In SENTIENT, add a gateway device and copy its access token (Manage credentials).",
                ))
                .child(self.form.fields(&theme, cfg!(windows), None, install_runs_as(cfg!(windows), None, "this computer").ok()))
                .children(outcome.clone().map(|(tone, m)| ui::outcome(&theme, tone, m)))
                .children(self.problem.clone().map(|p| ui::outcome(&theme, Tone::Bad, p)))
                .child(match o {
                    Some(o) => {
                        let label = format!("Install Sentient Gateway {}", o.version);
                        // Docker pulls the image: not before it is published.
                        let unpublished = (o.image_missing && !cfg!(windows)).then(|| sentient_installer_core::dockerhub::not_published(&o.version));
                        h_flex()
                            .gap_3()
                            .flex_wrap()
                            .items_center()
                            .child(
                                crate::ui::button("gw-install")
                                    .primary()
                                    .label(label)
                                    .disabled(busy || (cfg!(windows) && o.installer.is_none()) || unpublished.is_some())
                                    .on_click(cx.listener(move |p, _, _, cx| p.install(o.clone(), cx))),
                            )
                            .child(div().text_sm().text_color(theme.muted_foreground).child(match unpublished {
                                Some(why) => why,
                                None if cfg!(windows) => "the installer from the SENTIENT releases site, checked before use".to_string(),
                                None => "the quakestring/sentient-gateway-rs image, run by Docker".to_string(),
                            }))
                            .children(stale.clone().map(|n| div().text_size(px(13.)).text_color(theme.muted_foreground).child(n)))
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

        let below = if status.installed {
            match self.settings_job_view(cx) {
                Some(v) => Some(v.into_any_element()),
                None => {
                    let url = crate::manager::gateway_settings::sentient_page(self.sentient_url.as_deref(), status.host.as_deref(), &[]);
                    let settings = self.settings.clone();
                    let busy = busy || self.settings_job.read(cx).is_running();
                    self.card.update(cx, |c, _| c.feed(settings, url, busy));
                    Some(self.card.clone().into_any_element())
                }
            }
        } else {
            None
        };
        page.child(body.flex_shrink_0()).children(below)
    }
}

/// The gateway's install form, here and on a server: the platform it
/// sends to, the gateway device's access token (masked, with its show
/// toggle), the local page's password where the gateway has one (Windows),
/// and how it will run. Its values are read once for the install; after a
/// removal it is cleared ([`InstallForm::clear`]), so an old token is never
/// offered again.
pub(crate) struct InstallForm {
    pub host: Entity<InputState>,
    pub port: Entity<InputState>,
    pub token: Entity<InputState>,
    pub password: Entity<InputState>,
    pub password2: Entity<InputState>,
    default_host: String,
    default_port: String,
    /// Cleared at the next draw (InputState needs the window).
    clear_pending: bool,
}

/// What [`InstallForm::clear`] puts in each field: the defaults for the
/// platform, nothing in the token and the passwords.
pub(crate) fn cleared_values(default_host: &str, default_port: &str) -> [(&'static str, String); 5] {
    [("host", default_host.to_string()), ("port", default_port.to_string()), ("token", String::new()), ("password", String::new()), ("password2", String::new())]
}

/// Whether a job's new state clears the install form: a removal or an
/// install of the gateway (`clearing`) that went through. A failed install
/// keeps what was typed, for another try.
pub(crate) fn clears_form(clearing: bool, state: &JobState) -> bool {
    clearing && matches!(state, JobState::Done(_))
}

/// A server's job that clears its install form when it went through
/// ([`clears_form`]): the gateway's removal or install.
pub(crate) fn job_clears_form(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("gateway-remove" | "gateway-install"))
}

/// The (i) on a server's token: where it goes.
pub(crate) const REMOTE_TOKEN_HELP: &str = "It travels to the server over the encrypted SSH connection and is written there to a file only root can read. It is not kept on this computer.";

impl InstallForm {
    /// `host` empty: a placeholder asks for SENTIENT's address.
    pub fn new(host: &str, port: u16, window: &mut Window, cx: &mut App) -> Self {
        let h = host.to_string();
        let host_input = cx.new(|cx| {
            let i = InputState::new(window, cx).placeholder("SENTIENT's address, as the gateway reaches it");
            if h.is_empty() { i } else { i.default_value(h.clone()) }
        });
        let port_input = cx.new(|cx| InputState::new(window, cx).default_value(port.to_string()));
        let token = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("From SENTIENT: the gateway device, Manage credentials"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("8 characters or more"));
        let password2 = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Again"));
        Self { host: host_input, port: port_input, token, password, password2, default_host: host.to_string(), default_port: port.to_string(), clear_pending: false }
    }

    /// Every input, for the page's subscriptions.
    pub fn inputs(&self) -> [&Entity<InputState>; 5] {
        [&self.host, &self.port, &self.token, &self.password, &self.password2]
    }

    fn value(state: &Entity<InputState>, cx: &App) -> String {
        state.read(cx).value().trim().to_string()
    }

    /// The install's settings, or what is wrong in the form. `page_password`:
    /// the gateway has a local page password to set here (Windows).
    pub fn settings(&self, cx: &App, page_password: bool) -> Result<Settings, String> {
        let port: u16 = match Self::value(&self.port, cx).parse() {
            Ok(p) if p > 0 => p,
            _ => return Err("The MQTT port is a number from 1 to 65535.".into()),
        };
        let host = Self::value(&self.host, cx);
        let token = Self::value(&self.token, cx);
        if host.is_empty() || token.is_empty() {
            return Err("Enter the SENTIENT host and the gateway device's access token.".into());
        }
        let (p1, p2) = (Self::value(&self.password, cx), Self::value(&self.password2, cx));
        if page_password && (!p1.is_empty() || !p2.is_empty()) {
            if p1.len() < 8 {
                return Err("The local page's password needs 8 characters or more.".into());
            }
            if p1 != p2 {
                return Err("The two passwords differ.".into());
            }
        }
        Ok(Settings { host, port, token, ui_password: if page_password { p1 } else { String::new() } })
    }

    /// Empty the token and the passwords and put the platform back to its
    /// default, at the next draw ([`InstallForm::apply_clear`]).
    pub fn clear(&mut self) {
        self.clear_pending = true;
    }

    /// The draw that follows [`InstallForm::clear`].
    pub fn apply_clear(&mut self, window: &mut Window, cx: &mut App) {
        if !std::mem::take(&mut self.clear_pending) {
            return;
        }
        let values = cleared_values(&self.default_host, &self.default_port);
        for (input, (_, v)) in self.inputs().into_iter().zip(values) {
            input.update(cx, |s, cx| s.set_value(v, window, cx));
        }
    }

    /// The fields: platform and port on one line, the token, the page's
    /// password (`page_password`), and "Runs as" with what the computer can
    /// run (`runs_as`, its (i) `runs_help`). `token_help`: the token's (i).
    pub fn fields(&self, theme: &gpui_kit::component::Theme, page_password: bool, token_help: Option<&'static str>, runs_as: Option<(String, String)>) -> Div {
        let label = |text: &'static str, help: Option<(&'static str, String)>| {
            h_flex()
                .gap_1()
                .items_center()
                .h(px(22.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(text))
                .children(help.map(|(id, h)| ui::info(id.into(), h.into())))
        };
        let field = |l: Div, input: AnyElement| v_flex().gap_1p5().child(l).child(input);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(field(label("SENTIENT host", None), ui::probed_input(Input::new(&self.host)).into_any_element()).flex_1())
                    .child(field(label("MQTT port", None), ui::probed_input(Input::new(&self.port)).into_any_element()).w_24()),
            )
            .child(field(label("Device access token", token_help.map(|h| ("gw-token-help", h.to_string()))), ui::probed_input(Input::new(&self.token).mask_toggle()).into_any_element()))
            .when(page_password, |p| {
                p.child(
                    h_flex()
                        .gap_3()
                        .child(field(label("Local page password", None), div().child(Input::new(&self.password).mask_toggle()).into_any_element()).flex_1())
                        .child(field(label("Again", None), div().child(Input::new(&self.password2).mask_toggle()).into_any_element()).flex_1()),
                )
            })
            .when_some(runs_as, |p, (value, help)| {
                p.child(field(label("Runs as", Some(("gw-runs-help", help))), div().text_sm().child(value).into_any_element()))
            })
    }
}

/// What "Runs as" offers for an install, with its (i): only what the
/// computer can run. `docker`: Docker is there (None: not known, an older
/// agent). Err: nothing it can run (its reason, for the disabled Install).
pub(crate) fn install_runs_as(windows: bool, docker: Option<bool>, place: &str) -> Result<(String, String), String> {
    if windows {
        return Ok(("Windows service SentientGateway".into(), format!("Only what {place} can run\n\nOn Windows the gateway runs as the service SentientGateway, from its installer.")));
    }
    if docker == Some(false) {
        return Err(format!("Docker is not on {place}. Install Docker Engine there and refresh: the Platform Manager runs the gateway on Linux as a Docker container."));
    }
    Ok((
        format!("Docker container {}", gateway::CONTAINER),
        format!("Only what {place} can run\n\nOn Linux the Platform Manager runs the gateway as the Docker container {}, its configuration, data and logs in volumes of their own.", gateway::CONTAINER),
    ))
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

/// Why "Sends to" is not shown for a native gateway an unprivileged
/// Platform Manager cannot read.
const UNREADABLE_HELP: &str = "The gateway keeps its settings in /etc/sentient-gateway, open to its own account only (the access token is in them). The Platform Manager does not loosen that: Read its settings reads them as administrator, once with your password, and keeps only the platform host, the port and the web page's address, never the token or a password.";

/// The one line for an older Python gateway's container (Status::legacy).
pub(crate) fn legacy_line(name: &str, place: &str) -> String {
    format!("The older Sentient Gateway (the Python one) is on {place}, in the container {name}. The Platform Manager leaves it as it is.")
}

/// The (i) beside "Update to <v>" for the older Python gateway: what the
/// update does and what does not carry over
/// (sentient_installer_core::distro::gateway_legacy).
pub(crate) fn legacy_update_help() -> String {
    let mut t = "Updates the older Python gateway to the Sentient Gateway in Docker. Its configuration is backed up first; the connection, the connectors and the gateway's identity (token, client id) carry over, so SENTIENT sees the same gateway. The older gateway is removed only once the new one is connected; if anything fails, it runs again as before.\n\nWhat does not carry over:".to_string();
    for n in sentient_installer_core::distro::gateway_legacy::NOT_CARRIED {
        t.push_str("\n• ");
        t.push_str(n);
    }
    t
}

/// The older gateway's version: its image's tag when it is one.
fn legacy_version(image: Option<&str>) -> String {
    image
        .and_then(|i| i.split('@').next())
        .and_then(|i| i.rsplit_once(':'))
        .map(|(_, t)| t)
        .filter(|t| !t.contains('/') && t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|t| format!("{t} (the older Python gateway)"))
        .unwrap_or_else(|| "the older Python gateway".into())
}

/// The older Python gateway alone, here or on a server: what it is and
/// "Update to <v>" (the normal update job: gateway::update carries it to
/// the Rust gateway). `offer`: the newest release (None: still looking or
/// none); `why`: why it cannot run there (the button is then off with it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn legacy_panel(
    theme: &gpui_kit::component::Theme,
    id: &'static str,
    name: &str,
    image: Option<&str>,
    offer: Option<&Offer>,
    looking: Option<String>,
    why: Option<String>,
    busy: bool,
    on_update: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
) -> Div {
    // The update at the card's foot, at the right (the actions rule).
    let update: Vec<AnyElement> = match offer {
        Some(o) => {
            let button = crate::ui::button(SharedString::from(format!("{id}-update")))
                .primary()
                .label(format!("Update to {}", o.version))
                .disabled(busy || why.is_some())
                .on_click(move |_, w, cx| on_update(w, cx));
            let button = match why {
                Some(w) => div().id(SharedString::from(format!("{id}-update-why"))).child(button).tooltip(crate::ui::tooltip(w)).into_any_element(),
                None => button.into_any_element(),
            };
            ui::card_actions(
                theme,
                h_flex()
                    .gap(crate::ui::button_gap())
                    .flex_wrap()
                    .justify_end()
                    .items_center()
                    .child(div().text_sm().text_color(theme.muted_foreground).child(format!("Released {}", o.date)))
                    .child(ui::info(SharedString::from(format!("{id}-update-help")), legacy_update_help().into()))
                    .child(crate::ui::probed(button)),
            )
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect()
        }
        None => vec![div().text_sm().text_color(theme.muted_foreground).child(looking.unwrap_or_else(|| "Looking for gateway updates…".into())).into_any_element()],
    };
    ui::panel(theme)
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .items_center()
                .child(ui::product_logo(ui::Product::Gateway, 28.))
                .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                .child(ui::pill(theme, Tone::Neutral, match offer {
                    Some(o) => format!("Update available: Sentient Gateway {}", o.version),
                    None => "Update available".to_string(),
                })),
        )
        .child(ui::fact(theme, "Version", legacy_version(image)))
        .child(ui::fact(theme, "Runs as", format!("Docker container {name}")))
        .children(update)
}

/// The gateway's line on the Update pages and in Setup's choices: what
/// its update would be.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GatewayUpdate {
    pub version: String,
    /// One sentence: from what to what, and what it keeps.
    pub about: String,
    /// Why it cannot run here (the line is then greyed with it).
    pub why: Option<String>,
    /// The older Python gateway (its (i) says what does not carry over).
    pub older: bool,
}

/// The update `offer` gives the gateway found as `s` (None: no gateway, or
/// nothing newer). `windows`: the computer it runs on.
pub(crate) fn gateway_update(s: &Status, offer: Option<&Offer>, windows: bool) -> Option<GatewayUpdate> {
    let o = offer?;
    let why = gateway::offer_blocker(s, o, windows);
    if !s.installed {
        let name = s.legacy.clone()?;
        return Some(GatewayUpdate {
            version: o.version.clone(),
            about: format!(
                "The older Python gateway (the container {name}) to Sentient Gateway {}. Its configuration is backed up and carried over with the same identity; if anything fails, it runs again as before.",
                o.version
            ),
            why,
            older: true,
        });
    }
    let installed = s.version.clone()?;
    if !sentient_installer_core::releases::is_newer(&o.version, &installed, false) {
        return None;
    }
    let keeps = match s.kind() {
        "docker" => " The container starts again with its volumes and settings.",
        "windows-service" => " The configuration and the web page's password are kept.",
        _ => "",
    };
    Some(GatewayUpdate { version: o.version.clone(), about: format!("{installed} to {}.{keeps}", o.version), why, older: false })
}

/// The line's "Update to <v>": off with its reason as a tooltip, and for
/// the older gateway an (i) with what does not carry over.
pub(crate) fn update_button(_theme: &gpui_kit::component::Theme, id: &'static str, u: &GatewayUpdate, busy: bool, on_click: std::rc::Rc<dyn Fn(&mut Window, &mut App)>) -> Div {
    let button = crate::ui::button(id).primary().label(format!("Update to {}", u.version)).disabled(busy || u.why.is_some()).on_click(move |_, w, cx| on_click(w, cx));
    let button = match &u.why {
        Some(w) => div().id(SharedString::from(format!("{id}-why"))).child(button).tooltip(crate::ui::tooltip(w.clone())).into_any_element(),
        None => button.into_any_element(),
    };
    h_flex()
        .gap_1()
        .items_center()
        .child(crate::ui::probed(button))
        .when(u.older, |r| r.child(ui::info(SharedString::from(format!("{id}-help")), legacy_update_help().into())))
}

/// The steps a gateway update shows (the engine's own step names): the
/// older Python gateway's update, a Windows service's installer, or a
/// container started again on the new image.
pub(crate) fn update_plan(older: bool, windows_service: bool, version: &str) -> Vec<sentient_installer_core::progress::PlanStep> {
    use sentient_installer_core::progress::PlanStep;
    let start = format!("Starting Sentient Gateway {version}");
    if older {
        vec![
            PlanStep::new("Pulling the new gateway").when(&["Pulling "]).weight(3.),
            PlanStep::new("Backing up the configuration").when(&["Backing up the gateway's configuration"]),
            PlanStep::new("Stopping the older gateway").when(&["Stopping the older gateway"]),
            PlanStep::new(start),
            PlanStep::new("Waiting for the new gateway to connect to SENTIENT").weight(2.),
            PlanStep::new("Removing the older gateway's container").when(&["Removing the older gateway's container"]),
        ]
    } else if windows_service {
        vec![
            PlanStep::new("Downloading the installer").when(&["Downloading Sentient Gateway"]).weight(3.),
            PlanStep::new(format!("Installing Sentient Gateway {version}")).weight(2.),
        ]
    } else {
        vec![PlanStep::new("Pulling the new gateway").when(&["Pulling "]).weight(3.), PlanStep::new(start)]
    }
}

/// The gateway's line on the Update page when there is no update: what is
/// installed, and a label. None: no gateway here at all.
pub(crate) fn gateway_summary(s: &Status, checked: Option<bool>) -> Option<(String, Option<(Tone, &'static str)>)> {
    if !s.installed && s.legacy.is_none() {
        return None;
    }
    let what = if s.installed { s.version.clone().map(|v| format!("{v} installed")).unwrap_or_else(|| "Installed, its version not known".into()) } else { "The older Python gateway".into() };
    Some(match checked {
        None => (format!("{what}. Checking…"), None),
        Some(false) => (format!("{what}."), Some((Tone::Neutral, "Not checked"))),
        Some(true) if s.installed && s.version.is_none() => (format!("{what}: updates cannot be checked."), None),
        Some(true) => (format!("{what}. Up to date."), Some((Tone::Good, "Up to date"))),
    })
}

/// The web page's "Off" explained for how the gateway runs.
pub(crate) fn page_off_help(kind: &str) -> &'static str {
    match kind {
        "docker" => "The gateway's own web page is off. In a container it is turned on with ST_GW_UI=on (the container is created again).",
        "windows-service" => "The gateway's own web page is off in its settings (localUi in st_gateway.json, or ST_GW_UI in the service's environment). The gateway's installer turns it on again.",
        _ => "The gateway's own web page is off in its settings (localUi.enabled in /etc/sentient-gateway/st_gateway.json, or ST_GW_UI in the service's environment).",
    }
}

/// The last administrator's read of a native gateway's settings: the
/// gateway it was (its service or program), when (for a re-read), and the
/// non-secret values (host, port, page).
struct AdminRead {
    of: String,
    at: std::time::Instant,
    host: Option<String>,
    port: Option<u16>,
    page_url: Option<String>,
    /// The settings card's values (gateway-settings-json; no secret in them).
    settings: Option<GwSettings>,
}

static ADMIN_READ: std::sync::Mutex<Option<AdminRead>> = std::sync::Mutex::new(None);
/// A root helper that is alive anyway re-reads at most this often.
const ADMIN_REREAD: Duration = Duration::from_secs(60);

fn native_key(s: &Status) -> String {
    s.service.clone().or_else(|| s.program.clone()).unwrap_or_default()
}

/// Run `gateway-status` as root (crate::rootjob: the job's own line, which
/// carries no secret: no token, no password) and keep its values for the
/// page. Asks for the password only inside a user action
/// (sentient_installer_core::elevation::may_prompt).
fn admin_read() -> Result<Status, String> {
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let l2 = lines.clone();
    let sink: sentient_installer_core::progress::ProgressFn = std::sync::Arc::new(move |p| {
        if let sentient_installer_core::progress::Progress::Log { line } = p {
            if let Ok(mut v) = l2.lock() {
                v.push(line);
            }
        }
    });
    crate::rootjob::run(&["gateway-status".to_string()], &sink)?;
    let line = lines.lock().ok().and_then(|v| v.iter().rev().find(|l| l.starts_with("installed=")).cloned()).ok_or("The administrator's read gave no gateway state.")?;
    let s = status_of_line(&line);
    // The card's values in the same helper (no second prompt).
    let quiet: sentient_installer_core::progress::ProgressFn = std::sync::Arc::new(|_| {});
    let settings = crate::rootjob::run(&["gateway-settings-json".to_string()], &quiet).ok().and_then(|lines| crate::remote::settings_of_output(&lines.join("\n")).ok());
    if let Ok(mut g) = ADMIN_READ.lock() {
        *g = Some(AdminRead { of: native_key(&s), at: std::time::Instant::now(), host: s.host.clone(), port: s.port, page_url: s.page_url.clone(), settings });
    }
    Ok(s)
}

/// `s` with an administrator's read of the same gateway applied: its
/// settings are known again.
fn with_admin_read(mut s: Status) -> Status {
    if !s.config_unreadable {
        return s;
    }
    let due = ADMIN_READ.lock().ok().is_none_or(|g| g.as_ref().is_none_or(|r| r.of != native_key(&s) || r.at.elapsed() > ADMIN_REREAD));
    // Only while the root helper lives (no prompt from a page's poll).
    if due && sentient_installer_core::elevation::alive() {
        let _ = admin_read();
    }
    if let Ok(g) = ADMIN_READ.lock() {
        if let Some(r) = g.as_ref().filter(|r| r.of == native_key(&s)) {
            s.host = r.host.clone();
            s.port = r.port;
            s.page_url = r.page_url.clone();
            s.config_unreadable = false;
        }
    }
    s
}

/// The gateway on this computer (gateway::status). Screens: SPM_DEBUG_GATEWAY
/// holds a canned `gateway-status` line (`none` for no gateway), or the
/// name of one of debug_fixture's.
pub(crate) fn local_status() -> Status {
    sentient_installer_core::distro::gateway_docker::set_cache_dir(crate::updates::prefs_dir());
    #[cfg(debug_assertions)]
    if let Ok(t) = std::env::var("SPM_DEBUG_GATEWAY") {
        let t = debug_fixture(&t).map(str::to_string).unwrap_or(t);
        return status_of_line(&t);
    }
    // A read: SENTIENT's stopped WSL distro stays stopped (Windows).
    with_admin_read(sentient_installer_core::distro::idle::reading(gateway::status))
}

/// The card's settings of the gateway on this computer: read here (Docker,
/// the Windows service, a readable native one), or from the last
/// administrator's read of a native one. Screens: SPM_DEBUG_GATEWAY_SETTINGS
/// names a fixture (crate::manager::gateway_settings::debug_settings).
pub(crate) fn local_settings(status: &Status) -> Result<GwSettings, String> {
    // Screens: the fixture named, else the one of the canned gateway's name;
    // a canned gateway is never read for real.
    #[cfg(debug_assertions)]
    if let Some(n) = std::env::var("SPM_DEBUG_GATEWAY_SETTINGS").ok().or_else(|| std::env::var("SPM_DEBUG_GATEWAY").ok()) {
        return crate::manager::gateway_settings::debug_settings(&n).ok_or_else(|| format!("no settings fixture {n}"));
    }
    if status.kind() == "service" && cfg!(target_os = "linux") && !crate::rootjob::is_root() {
        if let Some(s) = ADMIN_READ.lock().ok().and_then(|g| g.as_ref().filter(|r| r.of == native_key(status)).and_then(|r| r.settings.clone())) {
            return Ok(s);
        }
        if status.config_unreadable {
            return Err("readable by an administrator only. Read its settings (above) reads them, once with your password.".into());
        }
    }
    Ok(gs::read(status))
}

/// Test connection on this computer (as root for a native gateway).
fn test_here(status: &Status, as_root: bool) -> Result<String, String> {
    if !as_root {
        return gs::test_connection(status);
    }
    let done = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let d2 = done.clone();
    let sink: sentient_installer_core::progress::ProgressFn = std::sync::Arc::new(move |p| {
        if let sentient_installer_core::progress::Progress::Done { message } = p {
            if let Ok(mut g) = d2.lock() {
                *g = Some(message);
            }
        }
    });
    crate::rootjob::run(&["gateway-test".to_string()], &sink)?;
    let m = done.lock().ok().and_then(|g| g.clone());
    m.ok_or_else(|| "The test gave no answer.".to_string())
}

/// Screens: canned gateways by name, one per way it runs (fictional names).
#[cfg(debug_assertions)]
pub(crate) fn debug_fixture(name: &str) -> Option<&'static str> {
    Some(match name {
        "docker" => "installed=true version=4.16.1 running=true host=127.0.0.1 port=1883 page=off container=sentient-gateway managed=true kind=docker image=quakestring/sentient-gateway-rs:4.16.1 volumes=st-gw-config,st-gw-data,st-gw-logs",
        "compose" => "installed=true version=4.16.1 running=true host=10.0.0.5 port=1883 page=off container=plant-gw managed=false kind=docker image=quakestring/sentient-gateway-rs:4.16.1 compose=/srv/example-site/gateway project=gateway volumes=gw-config,gw-data",
        "legacy" => "installed=false legacy=example-old-gw legacy_image=quakestring/sentient-gateway:3.7.10",
        "legacy-running" => "installed=false legacy=example-old-gw legacy_image=quakestring/sentient-gateway:latest",
        "docker-legacy" => "installed=true version=4.16.1 running=true host=127.0.0.1 port=1883 page=off container=sentient-gateway managed=true kind=docker image=quakestring/sentient-gateway-rs:4.16.1 volumes=st-gw-config,st-gw-data,st-gw-logs legacy=example-old-gw",
        "native" => "installed=true version=4.16.1 running=true host=127.0.0.1 port=1883 page=http://127.0.0.1:8642/ kind=service service=sentient-gateway.service program=/usr/bin/sentient-gateway package=sentient-gateway",
        "native-unreadable" => "installed=true version=4.16.1 running=true page=off kind=service service=sentient-gateway.service program=/usr/bin/sentient-gateway package=sentient-gateway unreadable=true",
        "native-hand" => "installed=true version=4.16.1 running=false host=192.168.10.20 port=1883 page=off kind=service service=example-gw.service program=/usr/local/lib/sentient-gateway/sentient-gateway",
        "windows" => "installed=true version=4.16.1 running=true host=127.0.0.1 port=1883 page=http://127.0.0.1:8642/ managed=true kind=windows-service",
        // None installed: the install form, with Docker there and without.
        "none" => "installed=false docker=true",
        "none-nodocker" => "installed=false docker=false",
        _ => return None,
    })
}

/// A Status from a `gateway-status` line (headless: `key=value` with
/// spaces as %20).
pub(crate) fn status_of_line(line: &str) -> Status {
    let kv: std::collections::HashMap<String, String> =
        line.split([' ', '+']).filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.replace("%20", " ").replace("%25", "%"))).collect();
    status_of(&|k| kv.get(k).cloned().unwrap_or_default())
}

/// A Status from a `gateway-status` line's values.
pub(crate) fn status_of(get: &dyn Fn(&str) -> String) -> Status {
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
        config_unreadable: get("unreadable") == "true",
        legacy: some("legacy"),
        legacy_image: some("legacy_image"),
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{cleared_values, clears_form, install_runs_as, job_clears_form, JobState, ADMIN_READ, AdminRead, Offer, Status, gateway_summary, gateway_update, update_plan, legacy_update_help, legacy_version, page_off_help, status_of_line, with_admin_read};

    #[test]
    fn a_status_line_round_trips() {
        let s = status_of_line("installed=true version=4.16.1 running=true page=off kind=service service=sentient-gateway.service program=/usr/bin/sentient-gateway package=sentient-gateway unreadable=true");
        assert_eq!((s.kind(), s.config_unreadable, s.page_url.clone(), s.host.clone()), ("service", true, None, None));
        let d = status_of_line("installed=true kind=docker container=sentient-gateway managed=true image=quakestring/sentient-gateway-rs:4.16.1 legacy=example-old-gw compose=/srv/example%20site");
        assert!(d.managed);
        assert_eq!(d.legacy.as_deref(), Some("example-old-gw"));
        assert_eq!(d.compose_dir.as_deref(), Some("/srv/example site"));
    }

    #[test]
    fn the_page_off_hint_fits_how_it_runs() {
        assert!(page_off_help("docker").contains("ST_GW_UI=on"));
        assert!(!page_off_help("service").contains("container"));
        assert!(page_off_help("service").contains("/etc/sentient-gateway"));
    }

    #[test]
    fn an_admin_read_fills_only_its_own_gateway() {
        let unreadable = Status { installed: true, service: Some("sentient-gateway.service".into()), config_unreadable: true, ..Status::default() };
        *ADMIN_READ.lock().unwrap() = Some(AdminRead { of: "other.service".into(), at: std::time::Instant::now(), host: Some("10.0.0.9".into()), port: Some(1883), page_url: None, settings: None });
        assert!(with_admin_read(unreadable.clone()).host.is_none());
        *ADMIN_READ.lock().unwrap() = Some(AdminRead { of: "sentient-gateway.service".into(), at: std::time::Instant::now(), host: Some("10.0.0.9".into()), port: Some(1883), page_url: Some("http://127.0.0.1:8642/".into()), settings: None });
        let s = with_admin_read(unreadable);
        assert_eq!((s.host.as_deref(), s.port, s.config_unreadable), (Some("10.0.0.9"), Some(1883), false));
        *ADMIN_READ.lock().unwrap() = None;
    }

    #[test]
    fn the_older_gateway_is_offered_an_update() {
        assert_eq!(legacy_version(Some("quakestring/sentient-gateway:3.7.10")), "3.7.10 (the older Python gateway)");
        assert_eq!(legacy_version(Some("quakestring/sentient-gateway:latest")), "the older Python gateway");
        assert_eq!(legacy_version(None), "the older Python gateway");
        let help = legacy_update_help();
        assert!(help.contains("What does not carry over") && help.contains("converters"));
        // It is an update, never anything else to the user.
        assert!(!help.to_lowercase().contains("migrat"));
        let s = status_of_line("installed=false legacy=example-old-gw legacy_image=quakestring/sentient-gateway:3.7.10");
        assert_eq!((s.installed, s.legacy.as_deref(), s.legacy_image.as_deref()), (false, Some("example-old-gw"), Some("quakestring/sentient-gateway:3.7.10")));
    }

    /// "Remove the Gateway" that went through leaves nothing of the last
    /// install in the form (an rc6 VM still had the old token there).
    #[test]
    fn a_removal_clears_the_install_form() {
        assert!(clears_form(true, &JobState::Done("Removed.".into())));
        assert!(!clears_form(true, &JobState::Failed("docker rm failed".into())));
        assert!(!clears_form(false, &JobState::Done("Started.".into())));
        let v = cleared_values("127.0.0.1", "1883");
        let get = |k: &str| v.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!((get("host").as_str(), get("port").as_str()), ("127.0.0.1", "1883"));
        for k in ["token", "password", "password2"] {
            assert!(get(k).is_empty(), "{k}");
        }
        // On a server: its removal and its install, nothing else.
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(job_clears_form(&args(&["gateway-remove", "--delete-data"])));
        assert!(job_clears_form(&args(&["gateway-install", "10.0.0.5", "1883", "--staged"])));
        assert!(!job_clears_form(&args(&["gateway-control", "stop"])));
        assert!(!job_clears_form(&[]));
    }

    #[test]
    fn runs_as_offers_only_what_the_server_runs() {
        assert!(install_runs_as(false, Some(true), "PLANT_B").unwrap().0.starts_with("Docker container"));
        assert!(install_runs_as(false, None, "PLANT_B").is_ok());
        assert!(install_runs_as(false, Some(false), "PLANT_B").unwrap_err().contains("Docker is not on PLANT_B"));
        assert!(install_runs_as(true, Some(false), "PLANT_B").unwrap().0.starts_with("Windows service"));
    }

    fn offer(v: &str, installer: bool) -> Offer {
        let asset = sentient_installer_core::releases::Asset { name: format!("SentientGatewaySetup-{v}.exe"), browser_download_url: String::new(), size: 1, digest: None };
        Offer { version: v.into(), date: "2026-11-17".into(), notes: String::new(), page: String::new(), installer: installer.then_some(asset), checksum: None, image_missing: false }
    }

    #[test]
    fn the_gateway_line_on_update_and_setup() {
        let o = offer("4.16.3", false);
        // Ours in Docker, older: an update that keeps its volumes.
        let docker = status_of_line("installed=true version=4.16.1 kind=docker container=sentient-gateway managed=true");
        let u = gateway_update(&docker, Some(&o), false).unwrap();
        assert_eq!((u.version.as_str(), u.why.is_none(), u.older), ("4.16.3", true, false));
        assert!(u.about.starts_with("4.16.1 to 4.16.3.") && u.about.contains("volumes"));
        // Up to date, or no offer: no line to update.
        assert!(gateway_update(&status_of_line("installed=true version=4.16.3 kind=docker container=sentient-gateway managed=true"), Some(&o), false).is_none());
        assert!(gateway_update(&docker, None, false).is_none());
        // A version not known is never compared.
        assert!(gateway_update(&status_of_line("installed=true kind=docker container=sentient-gateway managed=true"), Some(&o), false).is_none());
        // A package service: the line is there, greyed with its reason.
        let native = status_of_line("installed=true version=4.16.1 kind=service service=sentient-gateway.service program=/usr/bin/sentient-gateway package=sentient-gateway");
        assert!(gateway_update(&native, Some(&o), false).unwrap().why.unwrap().contains("package manager"));
        // Windows service: needs the release's installer.
        let win = status_of_line("installed=true version=4.16.1 kind=windows-service managed=true");
        assert!(gateway_update(&win, Some(&o), true).unwrap().why.is_some());
        assert!(gateway_update(&win, Some(&offer("4.16.3", true)), true).unwrap().why.is_none());
        // The older Python gateway alone: always its update (not on Windows).
        let older = status_of_line("installed=false legacy=example-old-gw legacy_image=quakestring/sentient-gateway:latest");
        let u = gateway_update(&older, Some(&o), false).unwrap();
        assert!(u.older && u.why.is_none() && u.about.contains("example-old-gw"));
        assert!(!u.about.to_lowercase().contains("migrat"));
        assert!(gateway_update(&older, Some(&o), true).unwrap().why.is_some());
        // Nothing here at all: no line.
        assert!(gateway_update(&status_of_line("installed=false"), Some(&o), false).is_none());
        assert!(gateway_summary(&status_of_line("installed=false"), Some(true)).is_none());
        assert_eq!(gateway_summary(&docker, Some(true)).unwrap().1.map(|p| p.1), Some("Up to date"));
        assert_eq!(gateway_summary(&docker, None).unwrap().0, "4.16.1 installed. Checking…");
    }

    #[test]
    fn the_update_steps_match_the_engines_names() {
        let p = update_plan(true, false, "4.16.3");
        assert!(p[0].matches("Pulling quakestring/sentient-gateway-rs:4.16.3"));
        assert!(p[1].matches("Backing up the gateway's configuration to the volume gw-config-backup-x"));
        assert!(p[2].matches("Stopping the older gateway (st-gateway)"));
        assert!(p[3].matches("Starting Sentient Gateway 4.16.3"));
        assert!(p[5].matches("Removing the older gateway's container (st-gateway-older-x)"));
        let w = update_plan(false, true, "4.16.3");
        assert!(w[0].matches("Downloading Sentient Gateway 4.16.3 (24 MB)") && w[1].matches("Installing Sentient Gateway 4.16.3"));
        assert_eq!(update_plan(false, false, "4.16.3").len(), 2);
    }
}
