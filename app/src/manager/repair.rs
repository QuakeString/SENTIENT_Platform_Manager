// Copyright © 2016-2026 The SENTIENT Authors

//! Repair and uninstall, from the Platform Manager (the same work as Setup's
//! maintenance mode, crate::maintenance). An adopted Docker stack that runs
//! from its own compose file can be taken over here (crate::adopt).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex,
    button::ButtonVariants as _,
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
    RemoveGateway,
}

pub struct RepairPage {
    record: Option<InstallationRecord>,
    /// The Sentient Gateway here (its removal is offered too).
    gateway: Option<sentient_installer_core::gateway::Status>,
    gateway_record: Option<crate::gateway_record::GatewayRecord>,
    _gateway_poll: Task<()>,
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
            // A takeover changes the record; an uninstall removes it, and
            // the manager goes on without SENTIENT.
            if matches!(page.running, Some(Action::TakeOver | Action::Uninstall)) && matches!(job.read(cx).state(), JobState::Done(_)) {
                page.record = InstallationRecord::load();
                cx.emit(RecordChanged);
            }
            cx.notify();
        });
        let _gateway_poll = cx.spawn(async move |this, cx| loop {
            let (status, rec) = cx.background_executor().spawn(async { (crate::manager::gateway::local_status(), crate::gateway_record::load()) }).await;
            if this.update(cx, |p, cx| { p.gateway = Some(status); p.gateway_record = rec; cx.notify(); }).is_err() {
                break;
            }
            cx.background_executor().timer(std::time::Duration::from_secs(10)).await;
        });
        Self { record, gateway: None, gateway_record: None, _gateway_poll, keep_data: true, running: None, job, _observe }
    }

    /// The Gateway choice, when there is a gateway here.
    fn gateway_panel(&self, theme: &gpui_kit::component::Theme, busy: bool, cx: &mut Context<Self>) -> Option<Div> {
        let g = self.gateway.clone().filter(|g| g.installed)?;
        let target = crate::manager::gateway::RemoveTarget::of(&g, None);
        let recorded = self.gateway_record.as_ref().is_some_and(|r| r.is(&g));
        let line = crate::manager::gateway::runs_as(g.kind(), &target.name, g.managed, recorded);
        let has_record = self.gateway_record.is_some();
        let button = crate::ui::button("repair-gw-remove")
            .danger()
            .label("Remove the Gateway…")
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                let page = cx.entity().downgrade();
                let (g2, has_record) = (g.clone(), has_record);
                crate::manager::gateway::open_remove_dialog(
                    target.clone(),
                    std::rc::Rc::new(move |keep, image, cx: &mut App| {
                        let g = g2.clone();
                        page.update(cx, |p, cx| p.remove_gateway(g, has_record, keep, image, cx)).ok();
                    }),
                    cx,
                );
                let _ = this;
            }))
            .into_any_element();
        Some(crate::manager::gateway::remove_panel(theme, "repair-gw", line, button))
    }

    fn remove_gateway(&mut self, g: sentient_installer_core::gateway::Status, has_record: bool, keep: bool, image: bool, cx: &mut Context<Self>) {
        self.running = Some(Action::RemoveGateway);
        let as_root = cfg!(target_os = "linux") && !crate::rootjob::is_root() && (g.container.is_none() || has_record);
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
                        let msg = sentient_installer_core::gateway::remove(&sink, keep, image)?;
                        let _ = crate::gateway_record::save(None);
                        sink(sentient_installer_core::progress::Progress::Done { message: msg });
                        Ok(())
                    }
                },
                cx,
            )
        });
    }

    /// The Uninstall dialog (crate::manager::uninstall_dialog).
    fn open_uninstall(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let r1 = record.clone();
        let load: crate::manager::uninstall_dialog::LoadFacts = std::sync::Arc::new(move || Ok(crate::uninstall::facts_full(&r1)));
        let r2 = record.clone();
        let run: crate::manager::uninstall_dialog::RunUninstall = std::sync::Arc::new(move |args: Vec<String>, sink| {
            if crate::rootjob::needed(&r2) {
                // One root job (one password), then this account's own copies
                // of the shortcuts and its menu cache.
                crate::rootjob::run(&args, &sink)?;
                if args.iter().any(|a| a.split(',').any(|p| p == "shortcuts")) {
                    crate::shortcuts::remove(crate::shortcuts::Owner::Sentient, &r2.shortcuts, &sink);
                }
                Ok(())
            } else {
                let choice = crate::uninstall::from_args(&args[1..])?;
                let mut facts = crate::uninstall::facts_quick(&r2);
                facts.pg_others = Some(crate::uninstall::pg_others(&r2));
                crate::uninstall::run(r2.clone(), choice, facts, sink)
            }
        });
        let page = cx.entity().downgrade();
        self.running = Some(Action::Uninstall);
        crate::manager::uninstall_dialog::open(
            crate::manager::uninstall_dialog::Opts {
                name: crate::uninstall::computer_name(),
                load,
                run,
                remote: false,
                on_done: std::rc::Rc::new(move |cx: &mut App| {
                    page.update(cx, |p, cx| {
                        p.record = InstallationRecord::load();
                        cx.emit(RecordChanged);
                        cx.notify();
                    })
                    .ok();
                }),
                on_close: std::rc::Rc::new(|after, cx: &mut App| crate::manager::uninstall_dialog::finish_local(after, cx)),
            },
            cx,
        );
    }

    /// The plan for confirmation (crate::manager::takeover), then the takeover.
    pub(crate) fn open_take_over(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let this = cx.entity().downgrade();
        crate::manager::takeover::open(
            true,
            std::sync::Arc::new({
                let record = record.clone();
                move |compose: Option<String>| crate::adopt::take_over_plan(&record, compose.as_deref())
            }),
            std::sync::Arc::new(move |ch: bool, pw: String| crate::adopt::check_takeover_password(&record, ch, &pw)),
            std::rc::Rc::new(move |compose, creds, cx: &mut App| {
                this.update(cx, |page, cx| page.take_over(compose, creds, cx)).ok();
            }),
            None,
            cx,
        );
    }

    fn take_over(&mut self, compose: Option<String>, creds: crate::manager::takeover::Creds, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.running = Some(Action::TakeOver);
        self.job.update(cx, |job, cx| job.start(move |sink| crate::adopt::take_over_compose(record, compose.as_deref(), creds.db.as_deref(), creds.ch.as_deref(), sink).map(|_| ()), cx));
    }

    fn start(&mut self, action: Action, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let keep = self.keep_data;
        self.running = Some(action);
        self.job.update(cx, |job, cx| {
            if crate::rootjob::needed(&record) {
                let mut args = vec![match action {
                    Action::Repair => "repair".to_string(),
                    _ => "uninstall".to_string(),
                }];
                if action == Action::Uninstall && !keep {
                    args.push("--delete-data".into());
                }
                job.start(
                    move |sink| {
                        crate::rootjob::run(&args, &sink)?;
                        // The root job cleaned by the record's manifest; this
                        // account's own copies (older installs) and its menu
                        // cache are cleaned as this account.
                        if action == Action::Uninstall {
                            crate::shortcuts::remove(crate::shortcuts::Owner::Sentient, &[], &sink);
                        }
                        Ok(())
                    },
                    cx,
                )
            } else {
                match action {
                    Action::Repair => job.start(move |sink| crate::maintenance_ops::run_repair(record, sink), cx),
                    _ => job.start(move |sink| crate::maintenance_ops::run_uninstall(record, keep, sink), cx),
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
                "SENTIENT and its services are removed. The database is kept and can be used by a new install. The Platform Manager stays.",
                "Uninstall",
            ),
            // Confirmed in their own dialogs.
            Action::TakeOver | Action::RemoveGateway => return,
            Action::Uninstall => (
                "Uninstall SENTIENT and delete its data?",
                "SENTIENT, its databases and everything kept are removed. This can't be undone. The Platform Manager stays.",
                "Delete everything",
            ),
        };
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            dialog.title(title).description(text).confirm().ok_text(ok).on_ok(move |_, _, cx| {
                this.update(cx, |page, cx| page.start(action, cx)).ok();
                true
            })
        });
    }
}

impl RepairPage {
    /// The gateway's removal: how it ended.
    fn gateway_outcome(&self, theme: &gpui_kit::component::Theme, cx: &App) -> Option<Div> {
        if self.running != Some(Action::RemoveGateway) {
            return None;
        }
        match self.job.read(cx).state() {
            JobState::Done(m) => Some(ui::outcome(theme, Tone::Good, m.clone())),
            JobState::Failed(m) => Some(ui::outcome(theme, Tone::Bad, m.clone())),
            _ => None,
        }
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
            // Without SENTIENT, Repair keeps one thing: removing the
            // Platform Manager itself. Just after an uninstall from here,
            // what it did comes first; the Gateway panel after it.
            let gw = self.gateway_panel(&theme, self.job.read(cx).is_running(), cx);
            let gw_outcome = self.gateway_outcome(&theme, cx);
            let done = match self.job.read(cx).state() {
                JobState::Done(m) if self.running == Some(Action::Uninstall) => Some(m.clone()),
                _ => None,
            };
            let page = match done {
                Some(m) => page.child(ui::panel(&theme).child(ui::outcome(&theme, Tone::Good, m))),
                None => page.child(ui::not_installed(&theme)),
            };
            return page.children(gw).children(gw_outcome).child(
                ui::panel(&theme)
                    .child(div().font_weight(FontWeight::MEDIUM).child("Remove the Platform Manager"))
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().text_sm().text_color(theme.muted_foreground).child("Removes this program from this computer."))
                            .child(ui::info("remove-manager-help".into(), "Its settings, saved servers and logs go too. SENTIENT, its database and backups are not touched.".into())),
                    )
                    .child(ui::button_row(vec![crate::ui::button("remove-manager").outline().label("Remove the Platform Manager…").on_click(|_, window, cx| crate::standalone::remove_manager(window, cx)).into_any_element()])),
            );
        }
        let job = self.job.read(cx);
        let busy = job.is_running();
        let uninstalled = self.running == Some(Action::Uninstall) && matches!(job.state(), JobState::Done(_));
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let done = busy || uninstalled;
        let gw = self.gateway_panel(&theme, done, cx);
        let gw_outcome = self.gateway_outcome(&theme, cx);
        let external = self.record.as_ref().and_then(|r| r.stack.external.clone()).filter(|_| self.record.as_ref().is_some_and(|r| r.engine == crate::installation::Engine::Docker));

        page.when_some(external, |page, ext| {
            page.child(
                ui::panel(&theme)
                    .child(div().font_weight(FontWeight::MEDIUM).child("Take over the compose file"))
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().text_sm().text_color(theme.muted_foreground).child("SENTIENT runs from a compose file the Platform Manager did not write."))
                            .child(ui::info(
                                "take-over-help".into(),
                                format!(
                                    "Its compose file ({}) is not changed by the Platform Manager. Changing settings or the database type needs the Platform Manager's own compose file: the same containers on the same data.",
                                    ext.files.join(", ")
                                )
                                .into(),
                            )),
                    )
                    .child(
                        h_flex().child(
                            crate::ui::button("take-over")
                                .outline()
                                .label("Take over the compose file…")
                                .disabled(done)
                                .on_click(cx.listener(|this, _, _, cx| this.open_take_over(cx))),
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
                        crate::ui::button("repair")
                            .outline()
                            .label("Repair")
                            .disabled(done)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(Action::Repair, window, cx))),
                    ),
                ),
        )
        .children(gw)
        .children(gw_outcome)
        .child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Uninstall"))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().text_sm().text_color(theme.muted_foreground).child("Choose what to remove: SENTIENT, its databases, the shortcuts, the Platform Manager."))
                        .child(ui::info("uninstall-help".into(), "Each part has its own switch, and each database a Keep its data switch, on by default. A database Setup did not install is never removed. Afterwards the Platform Manager checks that nothing is left and lists what is, with the command to remove it.".into())),
                )
                .child(ui::button_row(vec![crate::ui::button("uninstall").danger().label("Uninstall…").disabled(done).on_click(cx.listener(|this, _, _, cx| this.open_uninstall(cx))).into_any_element()])),
        )
        .when(uninstalled, |page| {
            page.child(
                ui::panel(&theme)
                    .children(outcome.map(|(tone, text)| crate::diagnosis::outcome(&theme, tone, text, self.job.read(cx))))
                    .child(h_flex().child(crate::ui::button("close").primary().label("Close").on_click(|_, window, _| window.remove_window()))),
            )
        })
    }
}
