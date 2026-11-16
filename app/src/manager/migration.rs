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
    /// The type picked to change to (None: the current one is selected).
    target: Option<Flavor>,
    confirm: bool,
    job: Entity<Job>,
    /// What earlier changes kept; None while it is being read.
    kept: Option<Result<Vec<Kept>, String>>,
    /// Native Linux: the operator asked to see it (it needs the password).
    kept_asked: bool,
    /// crate::migration::preflight, checked before the confirmation.
    preflight: crate::manager::remote_pages::Preflight,
    /// The job followed on this page in place of the form (its title), until
    /// "Back to Migration" (crate::manager::jobview).
    shown: Option<String>,
    _observe: Subscription,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<RecordChanged> for MigrationPage {}

fn describe(flavor: Flavor, windows: bool) -> &'static str {
    match flavor {
        Flavor::Stock => "Everything in PostgreSQL. Simplest; suits small sites.",
        Flavor::Timescaledb => "History compressed and kept in time chunks inside PostgreSQL.",
        Flavor::Clickhouse if windows => "History in ClickHouse (Docker in WSL2), everything else in PostgreSQL. For large sites.",
        Flavor::Clickhouse => "History in ClickHouse, everything else in PostgreSQL. For large sites.",
    }
}

/// All three database types as choice cards (the approved mockup, A1-A3):
/// the one SENTIENT runs on carries "Current" and is selected until another
/// is picked; `allowed` are the ones it can change to; `locked` (a job, the
/// compose file not ours yet) leaves only the current one lit. Shared with
/// a remote server's Migration.
pub(crate) fn type_cards(
    theme: &gpui_kit::component::Theme,
    id: &str,
    current: Option<Flavor>,
    picked: Option<Flavor>,
    allowed: &[Flavor],
    locked: bool,
    windows: bool,
    pick: std::rc::Rc<dyn Fn(Option<Flavor>, &mut App)>,
) -> Div {
    let selected = picked.or(current);
    v_flex().gap_2().children([Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse].into_iter().map(|f| {
        let is_current = current == Some(f);
        let enabled = is_current || (!locked && allowed.contains(&f));
        let title = h_flex()
            .gap_2()
            .items_center()
            .child(div().text_size(px(15.)).font_weight(FontWeight::MEDIUM).child(flavor_label(f)))
            .when(is_current, |t| t.child(ui::pill(theme, Tone::Neutral, "Current").text_size(px(12.))))
            .into_any_element();
        let pick = pick.clone();
        ui::radio_card_el(theme, SharedString::from(format!("{id}type-{}", f.as_str())), title, describe(f, windows), selected == Some(f), enabled)
            .when(!enabled, |c| c.opacity(0.5))
            .when(enabled, |c| c.on_click(move |_, _, cx| pick(if is_current { None } else { Some(f) }, cx)))
    }))
}

/// The fourth card (mockup M2): moving the data to another disk, below the
/// database types. Opens the same flow as Platform settings › Database.
pub(crate) fn move_data_card(theme: &gpui_kit::component::Theme, id: &str, open: std::rc::Rc<dyn Fn(&mut Window, &mut App)>) -> Div {
    v_flex()
        .gap(px(10.))
        .pt(px(10.))
        .border_t_1()
        .border_color(theme.border)
        .child(div().text_sm().text_color(theme.muted_foreground).child("Data location"))
        .child(
            h_flex()
                .gap(px(12.))
                .items_start()
                .px(px(16.))
                .py(px(14.))
                .rounded(px(12.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::HardDrive).size(px(18.)).mt(px(2.)).text_color(theme.primary))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_size(px(15.)).font_weight(FontWeight::MEDIUM).child("Move data to another disk"))
                        .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("PostgreSQL, ClickHouse, SENTIENT files or backups, each to its own disk or folder.")),
                )
                .child(ui::button(SharedString::from(format!("{id}move-data"))).outline().label("Choose…").on_click(move |_, window, cx| open(window, cx))),
        )
}

/// Under the button: why it is off, when it is.
pub(crate) fn change_hint(theme: &gpui_kit::component::Theme, external: bool, picked: bool) -> Option<Div> {
    let text = if external {
        "Take over the compose file first."
    } else if !picked {
        "Pick another type to change to it."
    } else {
        return None;
    };
    Some(div().text_sm().text_color(theme.muted_foreground).child(text))
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
        let mut page = Self { record, state, target: None, confirm: false, job, kept: None, kept_asked: false, preflight: std::env::var("SPM_DEBUG_PREFLIGHT").ok().filter(|_| cfg!(debug_assertions)).map(crate::manager::remote_pages::Preflight::Failed).unwrap_or(crate::manager::remote_pages::Preflight::Idle), shown: None, _observe, _tasks: Vec::new() };
        page.refresh_kept(cx);
        // Screens: the job view with a scripted migration (running, failed
        // or done), where a real one would change this computer's database.
        #[cfg(debug_assertions)]
        if let Some(mode) = std::env::var("SPM_DEBUG_MIGRATION_JOB").ok().filter(|_| !DEBUG_JOB_RAN.swap(true, std::sync::atomic::Ordering::Relaxed)) {
            let again: std::rc::Rc<dyn Fn(&mut Self, &mut Context<Self>)> = std::rc::Rc::new(|_, _| {});
            page.run_on_page("Changing the database to PostgreSQL + ClickHouse".into(), move |sink| debug_migration(&mode, &sink), Some(again), cx);
        }
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
                        // Only when asked (see above): a user action.
                        let _action = sentient_installer_core::elevation::user_action();
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

    /// A long job on this page: shown in place of the form, with Try again
    /// running `again` when it fails.
    fn run_on_page(
        &mut self,
        title: String,
        work: impl FnOnce(sentient_installer_core::progress::ProgressFn) -> Result<(), String> + Send + 'static,
        again: Option<std::rc::Rc<dyn Fn(&mut Self, &mut Context<Self>)>>,
        cx: &mut Context<Self>,
    ) {
        self.shown = Some(title.clone());
        let me = cx.entity().downgrade();
        self.job.update(cx, |job, cx| {
            job.set_label(title);
            job.set_on_page(true);
            if let Some(again) = again {
                job.set_retry(std::rc::Rc::new(move |cx: &mut App| {
                    let again = again.clone();
                    me.update(cx, |p, cx| again(p, cx)).ok();
                }));
            }
            job.start(work, cx)
        });
        cx.notify();
    }

    /// Read the record again (another page's job may have changed it); the
    /// form only, a job's view stays.
    pub fn refresh_record(&mut self, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        self.record = InstallationRecord::load();
        self.state = self.record.as_ref().and_then(migration::load_state);
        cx.notify();
    }

    /// A job's view is on the page (kept when the manager reloads).
    pub fn showing_job(&self) -> bool {
        self.shown.is_some()
    }

    fn targets(&self) -> Vec<Flavor> {
        self.record.as_ref().and_then(|r| migration::targets(r).ok()).unwrap_or_default()
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target else { return };
        self.migrate_to(target, cx);
    }

    fn migrate_to(&mut self, target: Flavor, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let title = format!("Changing the database to {}", flavor_label(target));
        let again: std::rc::Rc<dyn Fn(&mut Self, &mut Context<Self>)> = std::rc::Rc::new(move |p, cx| p.migrate_to(target, cx));
        if crate::rootjob::needed(&record) {
            let args = vec!["migrate".to_string(), target.as_str().to_string()];
            self.run_on_page(title, move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), Some(again), cx);
        } else {
            self.run_on_page(title, move |sink| migration::run(record, target, sink).map(|_| ()), Some(again), cx);
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
            true,
            std::sync::Arc::new({
                let record = record.clone();
                move |compose: Option<String>| crate::adopt::take_over_plan(&record, compose.as_deref())
            }),
            std::sync::Arc::new(move |ch: bool, pw: String| crate::adopt::check_takeover_password(&record, ch, &pw)),
            std::rc::Rc::new(move |compose: Option<String>, creds: crate::manager::takeover::Creds, cx: &mut App| {
                this.update(cx, |page, cx| {
                    let Some(record) = page.record.clone() else { return };
                    page.preflight = crate::manager::remote_pages::Preflight::Idle;
                    page.run_on_page("Taking over the compose file".into(), move |sink| crate::adopt::take_over_compose(record, compose.as_deref(), creds.db.as_deref(), creds.ch.as_deref(), sink).map(|_| ()), None, cx);
                })
                .ok();
            }),
            None,
            cx,
        );
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let title = "Copying the history from before the switch".to_string();
        let again: std::rc::Rc<dyn Fn(&mut Self, &mut Context<Self>)> = std::rc::Rc::new(|p, cx| p.resume(cx));
        if crate::rootjob::needed(&record) {
            let args = vec!["resume-migration".to_string()];
            self.run_on_page(title, move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), Some(again), cx);
        } else {
            self.run_on_page(title, move |sink| migration::resume(record, sink), Some(again), cx);
        }
    }
}

/// The scripted job runs once (the page is made again after a job ends
/// with no installation).
#[cfg(debug_assertions)]
static DEBUG_JOB_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(debug_assertions)]
fn debug_migration(mode: &str, sink: &sentient_installer_core::progress::ProgressFn) -> Result<(), String> {
    use sentient_installer_core::progress::Progress;
    let pause = || std::thread::sleep(std::time::Duration::from_millis(400));
    let log = |l: &str| sink(Progress::Log { line: l.into() });
    sink(Progress::Step { name: "Backing up everything first".into() });
    log("pg_dump: dumping contents of table \"public.ts_kv\"");
    log("Backup written to /opt/sentient/backups/sentient-2026-11-16.tar.zst and checked");
    pause();
    sink(Progress::Step { name: "Preparing ClickHouse".into() });
    log("ClickHouse 24.8 is up");
    pause();
    sink(Progress::Step { name: "Starting SENTIENT on PostgreSQL + ClickHouse".into() });
    log("SENTIENT answers on port 8080");
    pause();
    sink(Progress::Step { name: "Copying the history from before the switch".into() });
    for m in ["2026-08", "2026-09", "2026-10"] {
        log(&format!("{m}: copied and checked"));
    }
    match mode {
        "running" => {
            sink(Progress::Percent { value: 62.0 });
            std::thread::sleep(std::time::Duration::from_secs(3600));
            Ok(())
        }
        "failed" => Err("Copying the history stopped: ClickHouse did not answer on port 9000.".into()),
        _ => {
            log("2026-11: copied and checked");
            sink(Progress::Done { message: "SENTIENT runs on PostgreSQL + ClickHouse with its full history.".into() });
            Ok(())
        }
    }
}

impl MigrationPage {
    /// The job started here, in place of the form: running, or ended and not
    /// yet left with "Back to Migration".
    fn job_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let title = self.shown.clone()?;
        let job = self.job.read(cx);
        if matches!(job.state(), JobState::Idle) {
            return None;
        }
        let this = cx.entity().downgrade();
        let back = crate::manager::jobview::back_button("mig-job-back", "Migration", move |_, cx| {
            this.update(cx, |p, cx| {
                p.shown = None;
                p.job.update(cx, |j, cx| j.reset(cx));
                cx.notify();
            })
            .ok();
        });
        let retry = match job.state() {
            JobState::Failed(m) => job.retry().map(|r| crate::ui::button("mig-job-retry").primary().label(crate::ui::retry_label(m, "Try again")).on_click(move |_, _, cx| r(cx)).into_any_element()),
            _ => None,
        };
        let stages: &[crate::manager::jobview::Stage] = if title.starts_with("Changing the database") { crate::manager::jobview::MIGRATION } else { &[] };
        Some(crate::manager::jobview::view("mjob", title, stages, job, Some(back), retry, cx))
    }
}

impl Render for MigrationPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Migration",
            "",
        );
        if let Some(view) = self.job_view(cx) {
            return page.child(view);
        }
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };
        let job = self.job.read(cx);
        let busy = job.is_running();
        let current = record.flavor();
        let targets = migration::targets(record);
        let unfinished = self.state.clone().filter(|s| !s.done);
        // A compose file the Platform Manager did not write: known before
        // any check, the change stays off until it is taken over.
        let external = record.engine == crate::installation::Engine::Docker && record.stack.external.is_some();
        let chosen = self.target;

        let main = match &targets {
            Err(why) => ui::panel(&theme)
                .child(ui::fact(&theme, "Database", flavor_label(current)))
                .child(div().text_sm().text_color(theme.muted_foreground).child(*why)),
            Ok(targets) => ui::panel(&theme)
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Database type"))
                .child({
                    let locked = busy || unfinished.is_some() || external;
                    let this = cx.entity().downgrade();
                    type_cards(&theme, "", Some(current), self.target, targets, locked, cfg!(windows), std::rc::Rc::new(move |f, cx| {
                        this.update(cx, |this, cx| {
                            this.target = f;
                            this.confirm = false;
                            this.preflight = crate::manager::remote_pages::Preflight::Idle;
                            cx.notify();
                        })
                        .ok();
                    }))
                })
                .when(chosen.is_some(), |p| p.child(steps_text(&theme, cfg!(windows) && chosen == Some(Flavor::Clickhouse))))
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .when(!self.confirm, |r| {
                            r.child(
                                crate::ui::button("migrate")
                                    .primary()
                                    .label(chosen.map(|f| format!("Change to {}", flavor_label(f))).unwrap_or_else(|| "Change database".into()))
                                    // Off while the preflight says the change cannot be made.
                                    .disabled(busy || external || chosen.is_none() || unfinished.is_some() || self.preflight != crate::manager::remote_pages::Preflight::Idle)
                                    .loading(self.preflight == crate::manager::remote_pages::Preflight::Checking)
                                    .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                            )
                            .children(change_hint(&theme, external, chosen.is_some()))
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
                )
                .when(crate::movedata::unsupported(record).is_none(), |p| {
                    p.child(move_data_card(&theme, "", std::rc::Rc::new(|window, cx| {
                        crate::manager::movedata::open(crate::movedata::Target::Local, "This computer", None, window, cx)
                    })))
                }),
        };

        let preflight_failed = match &self.preflight {
            _ if external => Some((String::new(), true)),
            crate::manager::remote_pages::Preflight::Failed(why) => Some((why.clone(), false)),
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
            page.child(if external {
                crate::manager::remote_pages::foreign_compose_notice(
                    &theme,
                    "The database type cannot be changed now.",
                    crate::ui::button("mig-take-over").outline().label("Take over the compose file…").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.open_take_over(cx))).into_any_element(),
                )
            } else {
                crate::manager::remote_pages::preflight_notice(&theme, &why, None)
            })
        })
        .child(main)
        .child(kept_panel)
        
    }
}
