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
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

use sentient_installer_core::native::safe_update::RollbackPoint;
use sentient_installer_core::stack::Flavor;

use crate::installation::{Engine, InstallationRecord};
use crate::online::Version;
use sentient_installer_core::releases::{self, Move};
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
    /// The published versions to choose from (None while looking). When the
    /// list cannot be read the page offers the channel's newest alone.
    versions: Option<Result<Vec<Version>, String>>,
    /// The version picked in the list; None is the newest.
    chosen: Option<String>,
    job: Entity<Job>,
    /// Installing a new Platform Manager.
    manager_job: Entity<Job>,
    /// Applying the changes for this installation (crate::changes).
    changes_job: Entity<Job>,
    /// The operator chose "Later" for those changes.
    changes_later: bool,
    /// The release notes shown in place of the update rows ("What's new").
    notes_for: Option<Product>,
    /// The version list and package choice ("Other version").
    show_other: bool,
    show_changes_log: bool,
    _observe: Subscription,
    _observe_updates: Subscription,
    _observe_manager: Subscription,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

/// Which product's notes "What's new" shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Product {
    Manager,
    Sentient,
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
        let changes_job = cx.new(|_| Job::new());
        let _observe_changes = cx.observe(&changes_job, |page: &mut Self, job, cx| {
            if !job.read(cx).is_running() {
                page.record = InstallationRecord::load();
            }
            cx.notify();
        });
        let mut page = Self {
            record,
            package: None,
            accept_breaking: false,
            rollback: None,
            updates,
            versions: None,
            chosen: None,
            job,
            manager_job,
            changes_job,
            changes_later: false,
            notes_for: None,
            show_other: false,
            show_changes_log: false,
            _observe,
            _observe_updates,
            _observe_manager,
            _subs: vec![_observe_changes],
            _tasks: vec![],
        };
        page.refresh_rollback(cx);
        page.refresh_versions(cx);
        page
    }

    fn refresh_versions(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let native = record.platform().needs_package();
        let task = cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { crate::online::versions(native) }).await;
            this.update(cx, |page, cx| {
                page.versions = Some(found);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The picked version, or the newest listed.
    fn chosen_version(&self) -> Option<Version> {
        let list = self.versions.as_ref()?.as_ref().ok()?;
        match &self.chosen {
            Some(v) => list.iter().find(|x| &x.version == v).cloned(),
            None => list.first().cloned(),
        }
    }

    /// Install `version` (a release number, also its tag): pull its image or
    /// download its package.
    fn install_version(&mut self, version: String, cx: &mut Context<Self>) {
        let docker = self.record.as_ref().is_some_and(|r| r.engine == Engine::Docker);
        if docker { self.run_docker_to(version, cx) } else { self.run_download(version, cx) }
    }

    fn confirm_older(&mut self, version: String, window: &mut Window, cx: &mut Context<Self>) {
        let installed = self.record.as_ref().map(|r| r.sentient_version.clone()).unwrap_or_default();
        let this = cx.entity().downgrade();
        let text = format!(
            "SENTIENT {version} is older than {installed}, the version installed here. It is installed like an update: \
             the database is copied first, then {version} checks the database before anything changes. \
             If a newer version already changed the database ({installed} has database changes {version} does not know), \
             that check refuses and nothing is changed: {installed} keeps running. Going back is possible only between \
             versions with the same database."
        );
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let version = version.clone();
            dialog
                .title(format!("Install the older SENTIENT {version}?"))
                .description(text.clone())
                .confirm()
                .ok_text("Install older version")
                .on_ok(move |_, _, cx| {
                    let version = version.clone();
                    this.update(cx, |page, cx| page.install_version(version, cx)).ok();
                    true
                })
        });
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
pub(crate) fn retag(image: &str, version: &str) -> String {
    let repo = match image.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => repo,
        _ => image,
    };
    format!("{repo}:{version}")
}

impl UpdatePage {
    /// Whether the version list was read and has entries; otherwise the
    /// page offers the channel's newest alone, as before.
    fn has_list(&self) -> bool {
        matches!(&self.versions, Some(Ok(list)) if !list.is_empty())
    }

    /// The version list: a dropdown of the published versions (newest
    /// picked), and the button that installs the picked one: an update,
    /// nothing for the installed one, or going back behind a confirm.
    fn picker(&self, theme: &gpui_kit::component::Theme, installed: &str, docker: bool, busy: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.has_list() {
            return None;
        }
        let Some(Ok(list)) = &self.versions else { return None };
        let chosen = self.chosen_version()?;
        let movement = releases::movement(&chosen.version, installed);
        let items: Vec<(Version, bool)> = list.iter().map(|v| (v.clone(), v.version == chosen.version)).collect();
        let installed_owned = installed.to_string();
        let menu = Button::new("version-pick")
            .outline()
            .label(format!("{} ▾", chosen.version))
            .disabled(busy)
            .dropdown_menu({
                let this = cx.entity().downgrade();
                move |menu, _, _| {
                    let mut menu = menu.max_h(px(320.)).scrollable(true);
                    for (v, checked) in &items {
                        let mark = match releases::movement(&v.version, &installed_owned) {
                            Move::Same => " (installed)",
                            _ => "",
                        };
                        let this = this.clone();
                        let pick = v.version.clone();
                        menu = menu.item(PopupMenuItem::new(format!("{}{mark}", v.label())).checked(*checked).on_click(move |_, _, cx| {
                            let pick = pick.clone();
                            this.update(cx, |page, cx| {
                                page.chosen = Some(pick);
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                }
            });
        let version = chosen.version.clone();
        let action = match movement {
            Move::Newer => Button::new("install-version")
                .primary()
                .label(format!("Update to {version}"))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.install_version(version.clone(), cx))),
            Move::Same => Button::new("install-version").outline().label(format!("{version} is installed")).disabled(true),
            Move::Older => Button::new("install-version")
                .outline()
                .label(format!("Go back to {version}…"))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| this.confirm_older(version.clone(), window, cx))),
        };
        let about = match movement {
            Move::Older => "older than the installed version: possible only while both use the same database, checked before anything changes".to_string(),
            _ if docker => format!("pulls quakestring/sentient:{}{}", chosen.version, if chosen.date.is_empty() { String::new() } else { format!(", released {}", chosen.date) }),
            _ => format!("downloaded from the SENTIENT releases site and checked before use{}", if chosen.date.is_empty() { String::new() } else { format!("; released {}", chosen.date) }),
        };
        Some(
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(div().text_sm().child("Version"))
                        .child(menu)
                        .child(action)
                        .child(div().min_w_0().text_sm().text_color(theme.muted_foreground).child(about)),
                )
                .when(!chosen.summary.is_empty(), |c| c.child(div().text_xs().text_color(theme.muted_foreground).child(chosen.summary.clone())))
                .into_any_element(),
        )
    }

    /// Show the release notes of `product` ("What's new").
    pub fn show_notes(&mut self, product: Product, cx: &mut Context<Self>) {
        self.notes_for = Some(product);
        cx.notify();
    }

    /// Download and install the offered Platform Manager (B1).
    pub fn start_manager_update(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.updates.read(cx).manager().cloned() else { return };
        self.notes_for = None;
        self.manager_job.update(cx, |job, cx| job.start(move |sink| crate::updates::install_manager(&offer, &sink).map(|_| ()), cx));
        cx.notify();
    }

    /// Update SENTIENT to the offered version.
    fn start_sentient_update(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.updates.read(cx).sentient().cloned() else { return };
        let docker = self.record.as_ref().is_some_and(|r| r.engine == Engine::Docker);
        self.notes_for = None;
        if docker { self.run_docker_to(o.version, cx) } else { self.run_download(o.tag, cx) }
        cx.notify();
    }

    fn apply_changes(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.changes_later = false;
        if crate::rootjob::needed(&record) {
            let args = vec!["apply-changes".to_string()];
            self.changes_job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.changes_job.update(cx, |job, cx| job.start(move |sink| crate::changes::apply(record, &sink).map(|_| ()), cx));
        }
    }

    /// "Update from a file…": a Setup program (started, and this window
    /// closes) or a SENTIENT package folder (updated from).
    fn update_from_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: true, multiple: false, prompt: Some("Update from this".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |page, cx| {
                let is_setup = path.is_file()
                    && path.file_name().map(|n| n.to_string_lossy().to_lowercase()).is_some_and(|n| n.contains("setup"));
                if is_setup {
                    if std::process::Command::new(&path).spawn().is_ok() {
                        cx.quit();
                    }
                    return;
                }
                let dir = if path.is_file() { path.parent().map(|p| p.to_path_buf()).unwrap_or(path) } else { path };
                page.package = Some(dir);
                page.show_other = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// B4–B7: what this Platform Manager changes in the installation.
    fn changes_panel(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let record = self.record.as_ref()?;
        let job = self.changes_job.read(cx);
        let pending = crate::changes::pending(record);
        let state = job.state().clone();
        if pending.is_empty() && matches!(state, JobState::Idle) {
            return None;
        }
        if self.changes_later && matches!(state, JobState::Idle) {
            return None;
        }
        let planned = crate::changes::planned_steps(&pending);
        let n = pending.len();
        let panel = ui::panel(theme).border_color(theme.primary);
        let el = match state {
            JobState::Running => {
                let marked = ui::mark_steps(&self.changes_planned(), job.steps(), None);
                panel
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Applying {} change{}", self.changes_planned_count(), if self.changes_planned_count() == 1 { "" } else { "s" })))
                    .child(ui::steps(theme, &marked))
                    .child(div().text_xs().text_color(theme.muted_foreground).child("The database is not reinstalled or re-checked for these changes."))
            }
            JobState::Done(_) if pending.is_empty() => {
                let marked = ui::mark_steps(&self.changes_planned(), job.steps(), Some(true));
                panel
                    .child(ui::outcome(theme, Tone::Good, "The changes for this installation are applied. SENTIENT is running."))
                    .child(ui::steps(theme, &marked))
            }
            JobState::Failed(m) => {
                let marked = ui::mark_steps(&self.changes_planned(), job.steps(), Some(false));
                let log: Vec<SharedString> = job.log().to_vec();
                panel
                    .child(ui::outcome(theme, Tone::Bad, m.clone()))
                    .child(ui::steps(theme, &marked))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("changes-retry").primary().small().label("Try again").on_click(cx.listener(|this, _, _, cx| this.apply_changes(cx))))
                            .child(Button::new("changes-log").outline().small().label(if self.show_changes_log { "Hide log" } else { "Show log" }).on_click(cx.listener(|this, _, _, cx| {
                                this.show_changes_log = !this.show_changes_log;
                                cx.notify();
                            }))),
                    )
                    .when(self.show_changes_log, |p| p.children(log.into_iter().map(|l| div().text_xs().text_color(theme.muted_foreground).child(l))))
            }
            _ => panel
                .child(div().font_weight(FontWeight::MEDIUM).child(format!("{n} change{} for this installation", if n == 1 { "" } else { "s" })))
                .child(v_flex().gap_1().children(pending.iter().map(|c| h_flex().gap_2().text_sm().child("•").child(c.text))))
                .child(div().text_sm().text_color(theme.muted_foreground).child(crate::changes::effect(&pending)))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("changes-apply").primary().label("Apply now").on_click(cx.listener(|this, _, _, cx| this.apply_changes(cx))))
                        .child(Button::new("changes-later").outline().label("Later").on_click(cx.listener(|this, _, _, cx| {
                            this.changes_later = true;
                            cx.notify();
                        }))),
                ),
        };
        let _ = planned;
        Some(el.into_any_element())
    }

    /// The steps the running change job shows: what was pending when it began.
    fn changes_planned(&self) -> Vec<String> {
        let record = self.record.clone();
        let all = crate::changes::all();
        // While it runs the record still lists them as pending; afterwards
        // the steps it started name them.
        let pending: Vec<crate::changes::Change> = match &record {
            Some(r) => crate::changes::pending(r),
            None => Vec::new(),
        };
        if !pending.is_empty() {
            return crate::changes::planned_steps(&pending);
        }
        let _ = all;
        Vec::new()
    }

    fn changes_planned_count(&self) -> usize {
        self.record.as_ref().map(|r| crate::changes::pending(r).len()).unwrap_or(0)
    }

    /// A product line: name, what is known, and its buttons on the right.
    fn row(theme: &gpui_kit::component::Theme, highlight: bool, title: &'static str, sub: String) -> Div {
        let logo = if title == "SENTIENT" { ui::Product::Sentient } else { ui::Product::Manager };
        h_flex()
            .gap_3()
            .p_3()
            .items_center()
            .rounded(theme.radius)
            .bg(theme.background)
            .border_1()
            .border_color(if highlight { theme.primary } else { theme.border })
            .child(ui::product_logo(logo, 30.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(sub)),
            )
    }

    /// A1–A7 and B1–B2: the channel, both products and what each offers.
    fn updates_panel(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let u = self.updates.read(cx);
        let channel = u.prefs.channel;
        let checking = u.checking;
        let daily = u.prefs.daily;
        let last = u.last_checked();
        let found = u.found.clone();
        let this = crate::updates::manager_version();
        let installed = self.record.as_ref().map(|r| r.sentient_version.clone());
        let mjob = self.manager_job.read(cx);
        let installing = mjob.is_running();
        let mfinished = match mjob.state() {
            JobState::Failed(_) => Some(false),
            _ if installing => None,
            _ => None,
        };
        let mfailed = match mjob.state() {
            JobState::Failed(m) => Some(m.clone()),
            _ => None,
        };
        let mlog_last = mjob.log().last().cloned();
        let msteps: Vec<String> = crate::updates::SELF_UPDATE_STEPS.iter().map(|s| s.to_string()).collect();
        let marked = ui::mark_steps(&msteps, mjob.steps(), mfinished);

        let seg_theme = cx.theme().clone();
        let channels = crate::ui::seg_track(&seg_theme).children(Channel::ALL.into_iter().map(|c| {
            crate::ui::seg_item(&seg_theme, SharedString::from(format!("channel-{}", c.label())), c.label(), c == channel).on_click(cx.listener(move |this, _, _, cx| {
                this.updates.update(cx, |u, cx| u.set_channel(c, cx));
            }))
        }));

        // A6/A7: neither product could be checked.
        let unreachable = found.as_ref().and_then(|f| match (&f.manager, &f.sentient) {
            (Err(e), Err(_)) | (Err(e), Ok(None)) if installed.is_none() || f.sentient.is_err() => Some(e.clone()),
            _ => None,
        });
        let manager_offer = found.as_ref().and_then(|f| f.manager.as_ref().ok().cloned().flatten());
        let sentient_offer = found.as_ref().and_then(|f| f.sentient.as_ref().ok().cloned().flatten());

        // Platform Manager row.
        let manager_row = {
            let (sub, pill): (String, Option<(Tone, &str)>) = match (&found, &manager_offer) {
                (None, _) if checking => (format!("{this} installed. Checking…"), None),
                (None, _) => (format!("{this} installed. Not checked."), Some((Tone::Neutral, "Unknown"))),
                (Some(f), _) if f.manager.is_err() => (format!("{this} installed. Not checked."), Some((Tone::Neutral, "Unknown"))),
                (Some(_), None) => (format!("{this} is up to date."), Some((Tone::Good, "Up to date"))),
                (Some(_), Some(o)) => (
                    format!("{} is available (this is {this}).{}", o.version, if o.notes.is_empty() { String::new() } else { format!(" {}", o.notes) }),
                    None,
                ),
            };
            let mut row = Self::row(theme, manager_offer.is_some(), "Platform Manager", sub);
            if let Some((tone, text)) = pill {
                row = row.child(ui::pill(theme, tone, text));
            }
            if let Some(o) = &manager_offer {
                let has_installer = o.installer.is_some();
                row = row
                    .when(has_installer, |r| {
                        r.child(
                            Button::new("manager-install")
                                .primary()
                                .label(if installing { "Updating…" } else { "Update and restart" })
                                .disabled(installing)
                                .on_click(cx.listener(|this, _, _, cx| this.start_manager_update(cx))),
                        )
                    })
                    .child(Button::new("manager-notes").outline().label("What's new").on_click(cx.listener(|this, _, _, cx| this.show_notes(Product::Manager, cx))));
            }
            row
        };
        let manager_progress = (installing || mfailed.is_some()).then(|| {
            let o = manager_offer.clone();
            v_flex()
                .gap_2()
                .when_some(mfailed.clone(), |c, m| {
                    c.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().child(ui::outcome(theme, Tone::Bad, m)))
                            .child(Button::new("manager-retry").primary().small().label("Try again").on_click(cx.listener(|this, _, _, cx| this.start_manager_update(cx))))
                            .child(Button::new("manager-close").outline().small().label("Close").on_click(cx.listener(|this, _, _, cx| {
                                this.manager_job.update(cx, |job, cx| job.reset(cx));
                            }))),
                    )
                })
                .when(installing, |c| {
                    c.child(div().font_weight(FontWeight::MEDIUM).child(format!(
                        "Updating the Platform Manager: {this} to {}. SENTIENT keeps running.",
                        o.as_ref().map(|o| o.version.clone()).unwrap_or_default()
                    )))
                    .when_some(mlog_last.clone(), |c, l| c.child(div().text_xs().text_color(theme.muted_foreground).child(l)))
                    .when_some(mjob.percent(), |c, v| c.child(Progress::new("manager-progress").value(v)))
                })
                .child(ui::steps(theme, &marked))
        });

        // SENTIENT row.
        let sentient_row = installed.as_ref().map(|inst| {
            let docker = self.record.as_ref().is_some_and(|r| r.engine == Engine::Docker);
            let busy = self.job.read(cx).is_running();
            let (sub, pill): (String, Option<(Tone, &str)>) = match (&found, &sentient_offer) {
                (None, _) if checking => (format!("{inst} installed. Checking…"), None),
                (None, _) => (format!("{inst} installed. Not checked."), Some((Tone::Neutral, "Unknown"))),
                (Some(f), _) if f.sentient.is_err() => (format!("{inst} installed. Not checked."), Some((Tone::Neutral, "Unknown"))),
                (Some(_), None) => (format!("{inst} is up to date."), Some((Tone::Good, "Up to date"))),
                (Some(_), Some(o)) if manager_offer.is_some() => (format!("{} is available (installed {inst}). Update the Platform Manager first.", o.version), None),
                (Some(_), Some(o)) => (
                    format!("{} is available (installed {inst}).{}", o.version, if o.notes.is_empty() { String::new() } else { format!(" {}", o.notes) }),
                    None,
                ),
            };
            let mut row = Self::row(theme, sentient_offer.is_some() && manager_offer.is_none(), "SENTIENT", sub);
            if let Some((tone, text)) = pill {
                row = row.child(ui::pill(theme, tone, text));
            }
            if let Some(o) = &sentient_offer {
                let first = manager_offer.is_some();
                row = row.child(
                    Button::new("sentient-update")
                        .primary()
                        .label(format!("Update to {}", o.version))
                        .disabled(first || busy)
                        .on_click(cx.listener(|this, _, _, cx| this.start_sentient_update(cx))),
                );
                row = row.child(Button::new("sentient-notes").outline().label("What's new").on_click(cx.listener(|this, _, _, cx| this.show_notes(Product::Sentient, cx))));
            }
            let _ = docker;
            row.child(Button::new("sentient-other").outline().label("Other version").on_click(cx.listener(|this, _, _, cx| {
                this.show_other = !this.show_other;
                cx.notify();
            })))
        });

        // A5: a candidate the Stable channel leaves out.
        let hint = found.as_ref().and_then(|f| {
            let m = f.manager_candidate.as_ref().map(|v| format!("Platform Manager {v}"));
            let s = f.sentient_candidate.as_ref().map(|v| format!("SENTIENT {v}"));
            let names: Vec<String> = [m, s].into_iter().flatten().collect();
            (!names.is_empty()).then(|| {
                format!(
                    "A release candidate, {}, is available on the \"Release candidate\" channel.",
                    names.join(" and ")
                )
            })
        });

        ui::panel(theme)
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
            .when_some(unreachable, |p, e| {
                p.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .bg(theme.warning)
                        .text_color(theme.warning_foreground)
                        .child(div().flex_1().text_sm().child(if e.contains("limiting") {
                            "GitHub is limiting requests from this network. Try again in an hour.".to_string()
                        } else {
                            "The release page could not be reached. Nothing was changed.".to_string()
                        }))
                        .when(!e.contains("limiting"), |r| {
                            r.child(Button::new("retry-check").outline().small().label("Try again").on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.check(cx)))))
                        })
                        .child(Button::new("from-file").outline().small().label("Update from a file…").on_click(cx.listener(|this, _, _, cx| this.update_from_file(cx)))),
                )
            })
            .child(manager_row)
            .children(manager_progress)
            .children(sentient_row)
            .when_some(hint, |p, h| p.child(div().px_3().py_2().rounded(theme.radius).bg(theme.accent).text_sm().child(h)))
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
    }

    /// E1–E4: the notes of every version between the installed one and the
    /// offered one, and what updating does, with the Update button under them.
    fn notes_view(&self, theme: &gpui_kit::component::Theme, product: Product, cx: &mut Context<Self>) -> Div {
        let u = self.updates.read(cx);
        let (offer, installed, title) = match product {
            Product::Manager => (u.manager().cloned(), crate::updates::manager_version().to_string(), "What's new in the Platform Manager"),
            Product::Sentient => (
                u.sentient().cloned(),
                self.record.as_ref().map(|r| r.sentient_version.clone()).unwrap_or_default(),
                "What's new in SENTIENT",
            ),
        };
        let manager_first = product == Product::Sentient && u.manager().is_some();
        let back = Button::new("notes-back").outline().label("Not now").on_click(cx.listener(|this, _, _, cx| {
            this.notes_for = None;
            cx.notify();
        }));
        let Some(o) = offer else {
            return ui::panel(theme)
                .child(div().text_lg().child(title))
                .child(div().text_sm().text_color(theme.muted_foreground).child(format!("You have {installed}. Nothing newer on this channel.")))
                .child(h_flex().child(back));
        };
        let count = o.between.len();
        let you_have = if count > 1 { format!("You have {installed}. {count} versions are newer.") } else { format!("You have {installed}.") };
        let effect: String = match product {
            Product::Manager => {
                let changes = self.record.as_ref().map(|r| crate::changes::pending(r).len()).unwrap_or(0);
                if changes > 0 {
                    format!("Changes for this installation: {changes}. They are listed after the update and applied on your click. SENTIENT's version and data are not touched.")
                } else {
                    "SENTIENT keeps running. Its version and data are not touched.".into()
                }
            }
            Product::Sentient => {
                let docker = self.record.as_ref().is_some_and(|r| r.engine == Engine::Docker);
                format!(
                    "{} SENTIENT is stopped for about 2 minutes.{}",
                    crate::notes::database_line(&o.between),
                    if docker { "" } else { " The database is copied first." }
                )
            }
        };
        let page = o.page.clone();
        let update: Button = match product {
            Product::Manager => Button::new("notes-update")
                .primary()
                .label("Update and restart")
                .disabled(o.installer.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.start_manager_update(cx))),
            Product::Sentient => Button::new("notes-update")
                .primary()
                .label(format!("Update to {}", o.version))
                .disabled(manager_first)
                .on_click(cx.listener(|this, _, _, cx| this.start_sentient_update(cx))),
        };
        let product_name = if product == Product::Manager { "Platform Manager" } else { "SENTIENT" };
        let notes: Vec<crate::notes::Notes> = o
            .between
            .iter()
            .cloned()
            .map(|mut n| {
                n.version = format!("{product_name} {}", n.version);
                n
            })
            .collect();
        let loaded = notes.iter().any(|n| n.has_entries() || !n.summary.is_empty());
        let logo = if product == Product::Manager { ui::Product::Manager } else { ui::Product::Sentient };
        ui::panel(theme)
            .child(h_flex().gap_2().items_center().child(ui::product_logo(logo, 26.)).child(div().text_lg().child(title)))
            .child(div().text_sm().text_color(theme.muted_foreground).child(you_have))
            .child(if loaded {
                v_flex()
                    .id("notes-scroll")
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(ui::release_notes(theme, &notes))
                    .into_any_element()
            } else {
                h_flex()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .bg(theme.warning)
                    .text_color(theme.warning_foreground)
                    .child(div().flex_1().text_sm().child(format!(
                        "The release notes could not be loaded.{}",
                        if o.notes.is_empty() { String::new() } else { format!(" Short summary: {}", o.notes) }
                    )))
                    .child(Button::new("notes-retry").outline().small().label("Try again").on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.check(cx)))))
                    .into_any_element()
            })
            .child(div().text_sm().child(effect))
            .when(manager_first, |p| p.child(div().text_sm().text_color(theme.muted_foreground).child("Update the Platform Manager first, so SENTIENT is updated by the newest updater.")))
            .child(
                h_flex()
                    .gap_2()
                    .child(update)
                    .child(back)
                    .when(!page.is_empty(), |r| r.child(Button::new("notes-page").ghost().label("Open release page").on_click(move |_, _, cx| ui::open_url(&page, cx)))),
            )
    }
}

impl Render for UpdatePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Update",
            "Install a newer SENTIENT, with its database changes, and go back automatically if it fails.",
        );
        if let Some(product) = self.notes_for {
            return page.child(self.notes_view(&theme, product, cx));
        }
        let page = page.children(self.changes_panel(&theme, cx)).child(self.updates_panel(&theme, cx));
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

        let show_other = self.show_other || busy || outcome.is_some() || self.package.is_some();
        page.when(show_other, |page| page.child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Other version or package"))
                .child(ui::fact(&theme, "Installed version", record.sentient_version.clone()))
                .when_some(self.picker(&theme, &record.sentient_version, record.engine == Engine::Docker, busy, cx), |panel, picker| panel.child(picker))
                .when(!self.has_list(), |panel| panel.child({
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
                }))
                .when(native, |panel| {
                    panel.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                Button::new("choose")
                                    .outline()
                                    .label("Update from a file…")
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
        ))
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
