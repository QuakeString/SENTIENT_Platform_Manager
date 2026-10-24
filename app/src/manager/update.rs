// Copyright © 2016-2026 The SENTIENT Authors

//! Update: install a newer SENTIENT. Native installs update from a package
//! folder (the offline bundle of the new release); Docker installs pull the
//! image. On Windows the update is safe: the database is copied, the schema
//! changes are dry-run first, and a failure puts the previous version back
//! (crate::updating). What the last update replaced is kept until the next
//! one, and "Roll back last update" goes back to it.

use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    progress::Progress,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

use sentient_installer_core::native::safe_update::RollbackPoint;
use sentient_installer_core::stack::Flavor;

use crate::installation::{Engine, InstallationRecord};
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};
use crate::updates::UpdateState;
use sentient_installer_core::releases::Channel;

pub struct UpdatePage {
    record: Option<InstallationRecord>,
    package: Option<PathBuf>,
    accept_breaking: bool,
    /// What the last update replaced: None while it is being looked up.
    rollback: Option<Result<Option<RollbackPoint>, String>>,
    /// What the update channel offers (shared with the notice bar).
    updates: Entity<UpdateState>,
    job: Entity<Job>,
    /// Installing a new Platform Manager.
    manager_job: Entity<Job>,
    _observe: Subscription,
    _observe_updates: Subscription,
    _observe_manager: Subscription,
    _tasks: Vec<Task<()>>,
}

impl UpdatePage {
    pub fn new(record: Option<InstallationRecord>, updates: Entity<UpdateState>, cx: &mut Context<Self>) -> Self {
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
        let _observe_updates = cx.observe(&updates, |_, _, cx| cx.notify());
        let manager_job = cx.new(|_| Job::new());
        let _observe_manager = cx.observe(&manager_job, |_, job, cx| {
            // The new program is running and replaces this one.
            if matches!(job.read(cx).state(), JobState::Done(_)) {
                cx.quit();
            }
            cx.notify();
        });
        // Opening the screen looks, unless a check already answered.
        updates.update(cx, |u, cx| {
            if u.found.is_none() {
                u.check(cx);
            }
        });
        let mut page = Self {
            record,
            package: None,
            accept_breaking: false,
            rollback: None,
            updates,
            job,
            manager_job,
            _observe,
            _observe_updates,
            _observe_manager,
            _tasks: Vec::new(),
        };
        page.refresh_rollback(cx);
        page
    }

    /// Docker: update to `version` by moving the image to that tag.
    fn run_docker_to(&mut self, version: String, cx: &mut Context<Self>) {
        let Some(mut record) = self.record.clone() else { return };
        record.stack.image = retag(&record.stack.image, &version);
        let accept = self.accept_breaking;
        self.job.update(cx, |job, cx| {
            job.start(move |sink| crate::updating::run(record, None, accept, sink).map(|_| ()), cx)
        });
    }

    /// Download the release tagged `tag` from the releases repository, then
    /// update to it.
    fn run_download(&mut self, version: String, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let accept = self.accept_breaking;
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    let (_, dir) = crate::online::download(Some(&version), &sink)?;
                    if crate::rootjob::needed(&record) {
                        let mut args = vec!["update".to_string(), dir.display().to_string()];
                        if accept {
                            args.push("--accept-breaking".into());
                        }
                        crate::rootjob::run(&args, &sink).map(|_| ())
                    } else {
                        crate::updating::run(record, Some(&dir), accept, sink).map(|_| ())
                    }
                },
                cx,
            )
        });
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

/// `image` at tag `version`: `quakestring/sentient:latest` → `…:4.3.0.50`.
fn retag(image: &str, version: &str) -> String {
    let repo = match image.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => repo,
        _ => image,
    };
    format!("{repo}:{version}")
}

impl UpdatePage {
    /// The channel, Check for updates, the daily check, and the Platform
    /// Manager's own update.
    fn updates_panel(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let u = self.updates.read(cx);
        let channel = u.prefs.channel;
        let checking = u.checking;
        let daily = u.prefs.daily;
        let last = u.last_checked();
        let manager = u.found.as_ref().map(|f| f.manager.clone());
        let mjob = self.manager_job.read(cx);
        let installing = mjob.is_running();
        let mstep = mjob.step_with_time();
        let mfailed = match mjob.state() {
            JobState::Failed(m) => Some(m.clone()),
            _ => None,
        };
        let channels = h_flex().gap_2().children(Channel::ALL.into_iter().map(|c| {
            let b = Button::new(SharedString::from(format!("channel-{}", c.label())))
                .small()
                .label(c.label())
                .tooltip(c.about())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.updates.update(cx, |u, cx| u.set_channel(c, cx));
                }));
            if c == channel { b.primary() } else { b.outline() }
        }));
        let manager_line: AnyElement = match manager {
            None => div().into_any_element(),
            Some(Err(e)) => div().text_sm().text_color(theme.muted_foreground).child(format!("Platform Manager: {e}")).into_any_element(),
            Some(Ok(None)) => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("Platform Manager {} is up to date.", crate::updates::manager_version()))
                .into_any_element(),
            Some(Ok(Some(o))) => {
                let page = o.page.clone();
                let has_installer = o.installer.is_some();
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .when(has_installer, |row| {
                                let offer = o.clone();
                                row.child(
                                    Button::new("manager-install")
                                        .primary()
                                        .label(if installing { "Installing…".to_string() } else { format!("Install Platform Manager {} and restart", o.version) })
                                        .disabled(installing)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let offer = offer.clone();
                                            this.manager_job.update(cx, |job, cx| {
                                                job.start(move |sink| crate::updates::install_manager(&offer, &sink).map(|_| ()), cx)
                                            });
                                        })),
                                )
                            })
                            .child(Button::new("manager-notes").outline().label("Release notes").on_click(move |_, _, cx| ui::open_url(&page, cx)))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(format!(
                                "this is {}{}",
                                crate::updates::manager_version(),
                                if o.date.is_empty() { String::new() } else { format!("; {} released {}", o.version, o.date) }
                            ))),
                    )
                    .when(installing, |c| c.child(div().text_sm().child(mstep.clone())))
                    .children(mfailed.map(|e| ui::outcome(theme, Tone::Bad, e)))
                    .when(!o.notes.is_empty(), |c| c.child(div().text_xs().text_color(theme.muted_foreground).child(o.notes.clone())))
                    .into_any_element()
            }
        };
        ui::panel(theme)
            .child(div().font_weight(FontWeight::MEDIUM).child("Updates"))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().text_sm().child("Channel"))
                    .child(channels)
                    .child(div().flex_1())
                    .child(
                        Button::new("check-updates")
                            .outline()
                            .label(if checking { "Checking…" } else { "Check for updates" })
                            .disabled(checking)
                            .on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.check(cx)))),
                    ),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} Last checked: {last}.", channel.about())))
            .child(
                Checkbox::new("daily-check")
                    .label("Check once a day and show a notice when there is a newer version")
                    .checked(daily)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        let v = *value;
                        this.updates.update(cx, |u, cx| u.set_daily(v, cx));
                    })),
            )
            .child(manager_line)
    }
}

impl Render for UpdatePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Update",
            "Install a newer SENTIENT, with its database changes, and go back automatically if it fails.",
        )
        .child(self.updates_panel(&theme, cx));
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };
        let native = record.platform().needs_package();
        let job = self.job.read(cx);
        let busy = job.is_running();
        let can_run = !busy && (!native || self.package.is_some());
        let percent = job.percent();
        let step = job.step_with_time();
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let log: Vec<SharedString> = job.log().to_vec();

        page.child(
            ui::panel(&theme)
                .child(ui::fact(&theme, "Installed version", record.sentient_version.clone()))
                .child({
                    let u = self.updates.read(cx);
                    let offer = u.sentient().cloned();
                    let looking = u.checking && u.found.is_none();
                    let error = u.found.as_ref().and_then(|f| f.sentient.as_ref().err().cloned());
                    let docker = record.engine == Engine::Docker;
                    match (offer, looking, error) {
                        (_, true, _) => div().text_sm().text_color(theme.muted_foreground).child("Looking for a newer SENTIENT online…").into_any_element(),
                        (None, _, Some(e)) => div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(if native { format!("{e} Choose a package folder instead.") } else { e })
                            .into_any_element(),
                        (None, _, None) => div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!("No newer SENTIENT on the {} channel.", u.prefs.channel.label()))
                            .into_any_element(),
                        (Some(o), _, _) => {
                            let (tag, version) = (o.tag.clone(), o.version.clone());
                            v_flex()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .gap_3()
                                        .items_center()
                                        .child(
                                            Button::new("download-latest")
                                                .primary()
                                                .label(format!("Update to {version}"))
                                                .disabled(busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if docker {
                                                        this.run_docker_to(version.clone(), cx)
                                                    } else {
                                                        this.run_download(tag.clone(), cx)
                                                    }
                                                })),
                                        )
                                        .child(div().text_sm().text_color(theme.muted_foreground).child(if docker {
                                            format!("pulls quakestring/sentient:{}{}", o.version, if o.date.is_empty() { String::new() } else { format!(", released {}", o.date) })
                                        } else {
                                            format!("downloaded from the SENTIENT releases site and checked before use{}", if o.date.is_empty() { String::new() } else { format!("; released {}", o.date) })
                                        })),
                                )
                                .when(!o.notes.is_empty(), |c| {
                                    c.child(div().text_xs().text_color(theme.muted_foreground).child(o.notes.clone()))
                                })
                                .into_any_element()
                        }
                    }
                })
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
