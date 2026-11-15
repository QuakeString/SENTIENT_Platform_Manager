// Copyright © 2016-2026 The SENTIENT Authors

//! Migration: change the database type (crate::migration does the work).

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::stack::Flavor;

use crate::installation::{InstallationRecord, flavor_label};
use crate::jobs::{Job, JobState};
use crate::kept::{self, Kept};
use crate::migration::{self, MigrationState};
use crate::ui::{self, Tone};

/// The installation record changed on disk (the database type).
pub struct RecordChanged;

pub struct MigrationPage {
    record: Option<InstallationRecord>,
    state: Option<MigrationState>,
    target: Option<usize>,
    confirm: bool,
    job: Entity<Job>,
    /// What earlier changes kept; None while it is being read.
    kept: Option<Result<Vec<Kept>, String>>,
    /// Native Linux: the operator asked to see it (it needs the password).
    kept_asked: bool,
    /// crate::migration::preflight, checked before the confirmation.
    preflight: crate::manager::remote_pages::Preflight,
    _observe: Subscription,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<RecordChanged> for MigrationPage {}

fn describe(flavor: Flavor, windows: bool) -> &'static str {
    match flavor {
        Flavor::Stock => "Everything in PostgreSQL. Simplest; suits small sites.",
        Flavor::Timescaledb => "PostgreSQL with TimescaleDB: history compressed and in time chunks.",
        Flavor::Clickhouse if windows => "History in ClickHouse (Docker in WSL2), everything else in PostgreSQL. For large sites.",
        Flavor::Clickhouse => "History in ClickHouse, everything else in PostgreSQL. For large sites.",
    }
}

/// The database types to change to, as choice cards. Shared with a remote
/// server's Migration.
pub(crate) fn target_cards(theme: &gpui_kit::component::Theme, id: &str, targets: &[Flavor], selected: Option<usize>, windows: bool, pick: std::rc::Rc<dyn Fn(usize, &mut App)>) -> Div {
    v_flex().gap_2().children(targets.iter().enumerate().map(|(i, f)| {
        let pick = pick.clone();
        ui::radio_card(theme, SharedString::from(format!("{id}target-{i}")), flavor_label(*f), describe(*f, windows), selected == Some(i)).on_click(move |_, _, cx| pick(i, cx))
    }))
}

/// What changing the database does, step by step.
pub(crate) fn steps_text(theme: &gpui_kit::component::Theme, clickhouse_on_windows: bool) -> Div {
    v_flex()
        .gap_1()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child("1. A full backup, history included, is written to the backups folder of the installation.")
        .child("2. SENTIENT stops for about a minute and starts again on the new type.")
        .child("3. The history from before the switch is then copied month by month while SENTIENT runs; charts fill in as it goes.")
        .child("4. The old history is kept where it was until you remove it.")
        .when(clickhouse_on_windows, |c| {
            c.child("ClickHouse is installed first (WSL2 and Docker under a hidden service account); if Windows must restart for WSL2, nothing is changed and you start again afterwards.")
        })
}

impl MigrationPage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |page: &mut Self, job, cx| {
            let done = matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_));
            if done {
                // The record and the state file may have changed either way.
                page.record = InstallationRecord::load();
                page.state = page.record.as_ref().and_then(migration::load_state);
                page.target = None;
                page.confirm = false;
                page.preflight = crate::manager::remote_pages::Preflight::Idle;
                cx.emit(RecordChanged);
                page.refresh_kept(cx);
            }
            cx.notify();
        });
        let state = record.as_ref().and_then(migration::load_state);
        let mut page = Self { record, state, target: None, confirm: false, job, kept: None, kept_asked: false, preflight: std::env::var("SPM_DEBUG_PREFLIGHT").ok().filter(|_| cfg!(debug_assertions)).map(crate::manager::remote_pages::Preflight::Failed).unwrap_or(crate::manager::remote_pages::Preflight::Idle), _observe, _tasks: Vec::new() };
        page.refresh_kept(cx);
        page
    }

    fn refresh_kept(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        // Native Linux reads it as root: only when asked, not on every visit.
        if crate::rootjob::needed(&record) && !self.kept_asked {
            self.kept = Some(Ok(Vec::new()));
            return;
        }
        self.kept = None;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if crate::rootjob::needed(&record) {
                        let noop = sentient_installer_core::progress::noop();
                        crate::rootjob::run(&["kept".to_string()], &noop)
                            .map(|lines| lines.iter().filter_map(|l| kept::decode(l)).collect())
                    } else {
                        kept::list(&record)
                    }
                })
                .await;
            this.update(cx, |page, cx| {
                page.kept = Some(result);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn confirm_remove(&mut self, item: Kept, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let text = format!(
            "{} {} is deleted. This can't be undone.",
            item.name,
            match item.kind {
                kept::KeptKind::History => "and the history in it",
                kept::KeptKind::DatabaseCopy => "and everything in that database",
                kept::KeptKind::ClickHouse => "with its WSL2 distro, its data and the hidden service account",
                kept::KeptKind::ClickhouseDatabase => "and the ClickHouse history in it",
            }
        );
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            let item = item.clone();
            dialog.title("Remove for good?").description(text.clone()).confirm().ok_text("Remove").on_ok(move |_, _, cx| {
                let item = item.clone();
                this.update(cx, |page, cx| {
                    let Some(record) = page.record.clone() else { return };
                    if crate::rootjob::needed(&record) {
                        let args = vec!["remove-kept".to_string(), item.name.clone()];
                        page.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
                    } else {
                        page.job.update(cx, |job, cx| job.start(move |sink| kept::remove(&record, &item, sink), cx));
                    }
                })
                .ok();
                true
            })
        });
    }

    fn targets(&self) -> Vec<Flavor> {
        self.record.as_ref().and_then(|r| migration::targets(r).ok()).unwrap_or_default()
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let (Some(record), Some(target)) = (self.record.clone(), self.target.and_then(|i| self.targets().get(i).copied()))
        else {
            return;
        };
        if crate::rootjob::needed(&record) {
            let args = vec!["migrate".to_string(), target.as_str().to_string()];
            self.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.job.update(cx, |job, cx| job.start(move |sink| migration::run(record, target, sink).map(|_| ()), cx));
        }
    }

    /// The preflight, then the confirmation. As administrator the migration
    /// job checks it itself (and changes nothing when it fails).
    fn check(&mut self, cx: &mut Context<Self>) {
        use crate::manager::remote_pages::Preflight;
        let Some(record) = self.record.clone() else { return };
        if crate::rootjob::needed(&record) {
            self.confirm = true;
            cx.notify();
            return;
        }
        self.preflight = Preflight::Checking;
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { migration::preflight(&record) }).await;
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(()) => {
                        p.preflight = Preflight::Idle;
                        p.confirm = true;
                    }
                    Err(e) => p.preflight = Preflight::Failed(e),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The compose-file takeover from here (the preflight's reason).
    fn open_take_over(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let this = cx.entity().downgrade();
        crate::manager::takeover::open(
            std::sync::Arc::new(move |compose: Option<String>| crate::adopt::take_over_plan(&record, compose.as_deref())),
            std::rc::Rc::new(move |compose: Option<String>, cx: &mut App| {
                this.update(cx, |page, cx| {
                    let Some(record) = page.record.clone() else { return };
                    page.preflight = crate::manager::remote_pages::Preflight::Idle;
                    page.job.update(cx, |job, cx| job.start(move |sink| crate::adopt::take_over_compose(record, compose.as_deref(), sink).map(|_| ()), cx));
                })
                .ok();
            }),
            cx,
        );
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if crate::rootjob::needed(&record) {
            let args = vec!["resume-migration".to_string()];
            self.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.job.update(cx, |job, cx| job.start(move |sink| migration::resume(record, sink), cx));
        }
    }
}

impl Render for MigrationPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Migration",
            "Change the database type. Every row of history is copied and checked; nothing is deleted.",
        );
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };
        let job = self.job.read(cx);
        let busy = job.is_running();
        let current = record.flavor();
        let targets = migration::targets(record);
        let unfinished = self.state.clone().filter(|s| !s.done);
        let chosen = self.target.and_then(|i| targets.as_ref().ok()?.get(i).copied());

        let main = match &targets {
            Err(why) => ui::panel(&theme)
                .child(ui::fact(&theme, "Database", flavor_label(current)))
                .child(div().text_sm().text_color(theme.muted_foreground).child(*why)),
            Ok(targets) => ui::panel(&theme)
                .child(ui::fact(&theme, "Database now", flavor_label(current)))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Change to"))
                .child({
                    let locked = busy || unfinished.is_some();
                    let this = cx.entity().downgrade();
                    target_cards(&theme, "", targets, self.target, cfg!(windows), std::rc::Rc::new(move |i, cx| {
                        if !locked {
                            this.update(cx, |this, cx| {
                                this.target = Some(i);
                                this.confirm = false;
                                this.preflight = crate::manager::remote_pages::Preflight::Idle;
                                cx.notify();
                            })
                            .ok();
                        }
                    }))
                })
                .child(steps_text(&theme, cfg!(windows) && chosen == Some(Flavor::Clickhouse)))
                .child(
                    h_flex()
                        .gap_2()
                        .when(!self.confirm, |r| {
                            r.child(
                                crate::ui::button("migrate")
                                    .primary()
                                    .label(chosen.map(|f| format!("Change to {}", flavor_label(f))).unwrap_or_else(|| "Change database".into()))
                                    .disabled(busy || chosen.is_none() || unfinished.is_some() || self.preflight == crate::manager::remote_pages::Preflight::Checking)
                                    .loading(self.preflight == crate::manager::remote_pages::Preflight::Checking)
                                    .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                            )
                        })
                        .when(self.confirm, |r| {
                            r.child(
                                crate::ui::button("migrate-confirm")
                                    .danger()
                                    .label("Stop SENTIENT and change now")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                            )
                            .child(
                                crate::ui::button("migrate-cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm = false;
                                        cx.notify();
                                    })),
                            )
                        }),
                ),
        };

        let preflight_failed = match &self.preflight {
            crate::manager::remote_pages::Preflight::Failed(why) => Some((why.clone(), record.engine == crate::installation::Engine::Docker && record.stack.external.is_some())),
            _ => None,
        };
        let root_only = crate::rootjob::needed(record) && !self.kept_asked;
        let kept_panel = match &self.kept {
            None => ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Kept after earlier changes"))
                .child(div().text_sm().text_color(theme.muted_foreground).child("Looking…")),
            Some(Err(e)) => ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Kept after earlier changes"))
                .child(div().text_sm().text_color(theme.danger).child(format!("Could not look: {e}"))),
            Some(Ok(items)) => ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Kept after earlier changes"))
                .when(items.is_empty() && !root_only, |p| {
                    p.child(div().text_sm().text_color(theme.muted_foreground).child("Nothing: no old history and no database copies."))
                })
                .when(root_only, |p| {
                    p.child(
                        h_flex().child(
                            crate::ui::button("show-kept")
                                .outline()
                                .label("Show (asks for the administrator password)")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.kept_asked = true;
                                    this.refresh_kept(cx);
                                })),
                        ),
                    )
                })
                .when(!items.is_empty(), |p| {
                    p.child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Migrations keep the old history and updates keep a copy of the database. Remove them once you are sure you no longer need them.",
                    ))
                })
                .children(items.iter().enumerate().map(|(i, item)| {
                    let item_for_click = item.clone();
                    h_flex()
                        .gap_3()
                        .items_center()
                        .text_sm()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).child(item.name.clone()))
                                .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(item.detail.clone())),
                        )
                        .child(div().text_color(theme.muted_foreground).child(kept::human(item.bytes)))
                        .child(
                            crate::ui::button(SharedString::from(format!("remove-{i}")))
                                .outline()
                                .label("Remove")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_remove(item_for_click.clone(), window, cx)
                                })),
                        )
                })),
        };

        page.when_some(unfinished, |p, state| {
            p.child(
                ui::panel(&theme)
                    .child(ui::pill(&theme, Tone::Warn, "History copy not finished"))
                    .child(div().text_sm().child(format!(
                        "SENTIENT runs on {} since the switch from {}, but part of the history from before it is still being copied. Continue the copy; months already copied are skipped.",
                        flavor_label(state.to),
                        flavor_label(state.from)
                    )))
                    .child(
                        h_flex().child(
                            crate::ui::button("resume")
                                .primary()
                                .label("Continue copying")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.resume(cx))),
                        ),
                    ),
            )
        })
        .when_some(preflight_failed, |page, (why, external)| {
            page.child(crate::manager::remote_pages::preflight_notice(
                &theme,
                &why,
                external.then(|| crate::ui::button("mig-take-over").outline().label("Take over the compose file…").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.open_take_over(cx))).into_any_element()),
            ))
        })
        .child(main)
        .child(kept_panel)
        
    }
}
