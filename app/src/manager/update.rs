// Copyright © 2016-2026 The SENTIENT Authors

//! Update: install a newer SENTIENT. Native installs update from a package
//! folder (the offline bundle of the new release); Docker installs pull the
//! image. On Windows the update is safe: the database is copied, the schema
//! changes are dry-run first, and a failure puts the previous version back
//! (crate::updating). What the last update replaced is kept until the next
//! one, and "Roll back last update" goes back to it.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    menu::PopupMenuItem,
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
use sentient_installer_core::licence_version::{self, Licence, LicenceRead};
use crate::manager::licence_gate;

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
    /// Docker: the version the container runs (crate::installation::
    /// running_version), read in the background; None until read or when
    /// it does not say.
    running: Option<String>,
    job: Entity<Job>,
    /// Installing a new Platform Manager.
    manager_job: Entity<Job>,
    /// The gateway on this computer (None while it is read), and its
    /// update (crate::manager::gateway::gateway_update).
    gateway: Option<sentient_installer_core::gateway::Status>,
    gateway_job: Entity<Job>,
    /// Applying the changes for this installation (crate::changes).
    changes_job: Entity<Job>,
    /// The operator chose "Later" for those changes.
    changes_later: bool,
    /// The update or roll back shown in place of the page
    /// (crate::manager::jobview) until "Back to Update": its title.
    run_title: Option<String>,
    /// The release notes shown in place of the update rows ("What's new").
    /// The version list and package choice ("Other version").
    show_other: bool,
    /// The licence's allowed versions, read without a sign-in from
    /// SENTIENT's database (crate::licence::read_versions); None while read,
    /// or when it cannot be read that way (then Update signs in first).
    licence: Option<Licence>,
    /// What a sign-in from this page found (or that it was cancelled).
    licence_signed: Option<LicenceRead>,
    /// Screens: open the licence check of Update on the first frame
    /// (SPM_DEBUG_LICENCE_DIALOG=<version>; "cancelled" in
    /// SPM_DEBUG_LICENCE_SIGNED shows the could-not-be-checked dialog).
    debug_gate: Option<String>,
    /// Native Linux: this computer's C library and system, read once (an
    /// update whose server needs a newer glibc is not offered).
    libc: sentient_installer_core::glibc::Libc,
    os_name: String,
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
        // Opening the screen looks, unless a check answered in the last 30 minutes.
        updates.update(cx, |u, cx| {
            if u.stale() {
                u.check(cx);
            }
        });
        let gateway_job = cx.new(|_| Job::new());
        let _observe_gateway = cx.observe(&gateway_job, |page: &mut Self, job, cx| {
            if !job.read(cx).is_running() {
                page.refresh_gateway(cx);
                page.updates.update(cx, |u, cx| u.check(cx));
            }
            cx.notify();
        });
        let changes_job = cx.new(|_| Job::new());
        let _observe_changes = cx.observe(&changes_job, |page: &mut Self, job, cx| {
            if !job.read(cx).is_running() {
                page.record = InstallationRecord::load();
            }
            cx.notify();
        });
        let native_linux = cfg!(target_os = "linux") && record.as_ref().is_some_and(|r| r.platform().needs_package());
        let (libc, os_name) = if native_linux {
            (sentient_installer_core::glibc::local(), sentient_installer_core::glibc::local_os_name())
        } else {
            (Default::default(), String::new())
        };
        let mut page = Self {
            libc,
            os_name,
            record,
            package: None,
            accept_breaking: false,
            rollback: None,
            updates,
            versions: None,
            chosen: None,
            running: None,
            job,
            manager_job,
            gateway: None,
            gateway_job,
            changes_job,
            changes_later: false,
            run_title: None,
            // Renders: the "Other version or package" panel open.
            show_other: cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_UPDATE_OTHER").is_some(),
            licence: None,
            licence_signed: if cfg!(debug_assertions) && std::env::var("SPM_DEBUG_LICENCE_SIGNED").is_ok_and(|v| v == "cancelled") {
                Some(LicenceRead::Failed("cancelled".into()))
            } else {
                None
            },
            debug_gate: std::env::var("SPM_DEBUG_LICENCE_DIALOG").ok().filter(|_| cfg!(debug_assertions)),
            _observe,
            _observe_updates,
            _observe_manager,
            _subs: vec![_observe_changes, _observe_gateway],
            _tasks: vec![],
        };
        page.refresh_rollback(cx);
        page.refresh_versions(cx);
        page.refresh_gateway(cx);
        page.refresh_licence(cx);
        page
    }

    /// Read the licence's allowed versions without a sign-in (in the
    /// background). Native Linux without administrator rights would ask for
    /// the password: left unread, Update signs in to SENTIENT instead.
    fn refresh_licence(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if crate::dbstatus::needs_admin(&record) && !crate::rootjob::is_root() {
            return;
        }
        self._tasks.push(cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { crate::licence::read_versions(&record) }).await;
            this.update(cx, |p, cx| {
                p.licence = got;
                cx.notify();
            })
            .ok();
        }));
    }

    /// What is known of the licence now (crate::manager::licence_gate).
    fn licence_read(&self) -> LicenceRead {
        let key = crate::licence::Target::Local(None).key();
        match (&self.licence_signed, licence_gate::known(&key, self.licence.as_ref())) {
            (Some(r @ LicenceRead::Read(_)), _) => r.clone(),
            (_, k @ LicenceRead::Read(_)) => k,
            (Some(f), _) => f.clone(),
            _ => LicenceRead::NotRead,
        }
    }

    /// Update to `version` (build version `build`) once the licence check
    /// allows it: `go` runs the update. Going back (`older`) keeps its own
    /// confirmation.
    fn gated(&mut self, version: String, build: String, older: bool, go: Rc<dyn Fn(&mut App)>, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let older_confirm: Rc<dyn Fn(&mut Window, &mut App)> = {
            let (this, v) = (this.clone(), version.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                this.update(cx, |p, cx| p.confirm_older(v.clone(), window, cx)).ok();
            })
        };
        let read = self.licence_read();
        let normal: Rc<dyn Fn(&mut Window, &mut App)> = if older {
            older_confirm.clone()
        } else {
            let (installed, v, b, go) = (self.installed(), version.clone(), build.clone(), go.clone());
            let read = read.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                licence_gate::confirm_update("this computer", &v, &installed, &read, &b, go.clone(), window, cx);
            })
        };
        let anyway: Rc<dyn Fn(&mut Window, &mut App)> = if older { older_confirm } else { Rc::new(move |_: &mut Window, cx: &mut App| go(cx)) };
        let ask = licence_gate::Ask {
            target: crate::licence::Target::Local(self.record.clone()),
            place: "this computer".into(),
            version,
            build,
            normal,
            anyway,
            learned: Rc::new(move |r, cx| {
                this.update(cx, |p, cx| {
                    p.licence_signed = Some(r);
                    cx.notify();
                })
                .ok();
            }),
        };
        licence_gate::guard(Rc::new(ask), read, window, cx);
    }

    /// A page action as `go` for [`Self::gated`].
    fn runs(&self, f: impl Fn(&mut Self, &mut Context<Self>) + 'static, cx: &mut Context<Self>) -> Rc<dyn Fn(&mut App)> {
        let this = cx.entity().downgrade();
        Rc::new(move |cx: &mut App| {
            this.update(cx, |p, cx| f(p, cx)).ok();
        })
    }

    /// Read the gateway here (in the background).
    fn refresh_gateway(&mut self, cx: &mut Context<Self>) {
        self._tasks.push(cx.spawn(async move |this, cx| {
            let s = cx.background_executor().spawn(async { crate::manager::gateway::local_status() }).await;
            this.update(cx, |p, cx| {
                p.gateway = Some(s);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Update the gateway to the offered release (gateway::update: the
    /// older Python gateway is carried to the Rust one the same way).
    fn start_gateway_update(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.updates.read(cx).found.as_ref().and_then(|f| f.gateway.as_ref().ok().cloned().flatten()) else { return };
        let (older, windows_service) = self.gateway.as_ref().map(|s| (!s.installed && s.legacy.is_some(), s.kind() == "windows-service")).unwrap_or_default();
        let plan = crate::manager::gateway::update_plan(older, windows_service, &offer.version);
        self.gateway_job.update(cx, |job, _| job.set_plan(plan));
        self.gateway_job.update(cx, |job, cx| job.start(move |sink| sentient_installer_core::gateway::update(&sink, &offer), cx));
        cx.notify();
    }

    /// Read which version the Docker container runs (the tag may be
    /// "latest").
    fn refresh_running(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone().filter(|r| r.engine == Engine::Docker) else { return };
        let task = cx.spawn(async move |this, cx| {
            let running = cx.background_executor().spawn(async move { crate::installation::running_version(&record) }).await;
            this.update(cx, |page, cx| {
                page.running = running;
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn refresh_versions(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let native = record.platform().needs_package();
        let task = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    // Renders: a canned list, SPM_DEBUG_VERSIONS=4.4.0.3,4.4.0.2.
                    #[cfg(debug_assertions)]
                    if let Ok(list) = std::env::var("SPM_DEBUG_VERSIONS") {
                        return Ok(list.split(',').map(|v| Version { version: v.into(), date: "2026-11-17".into(), summary: String::new() }).collect());
                    }
                    crate::online::versions(native)
                })
                .await;
            this.update(cx, |page, cx| {
                page.versions = Some(found);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Native Linux: why SENTIENT `version` cannot run on this computer's
    /// glibc (`min_glibc`: its manifest's, when read; else the release
    /// table). None: it can, or it is not known.
    fn glibc_block(&self, version: &str, min_glibc: Option<&str>) -> Option<String> {
        use sentient_installer_core::glibc;
        let r = self.record.as_ref()?;
        if !cfg!(target_os = "linux") || !r.platform().needs_package() {
            return None;
        }
        glibc::decide(version, glibc::min_glibc(min_glibc, version), &self.libc, &self.os_name, glibc::Here::Computer).blocked().map(str::to_string)
    }

    /// The picked version, or the newest listed.
    fn chosen_version(&self) -> Option<Version> {
        let list = self.versions.as_ref()?.as_ref().ok()?;
        match &self.chosen {
            Some(v) => list.iter().find(|x| &x.version == v).cloned(),
            None => list.first().cloned(),
        }
    }

    /// The next update or roll back: its title and steps, shown from the
    /// first frame.
    fn plan_run(&mut self, title: String, plan: Vec<sentient_installer_core::progress::PlanStep>, cx: &mut Context<Self>) {
        self.run_title = Some(title);
        self.job.update(cx, |job, _| {
            job.set_plan(plan);
            job.set_label("Updating SENTIENT");
            job.set_on_page(true);
        });
    }

    fn shape(&self) -> crate::jobplan::Shape {
        self.record.as_ref().map(crate::jobplan::Shape::of).unwrap_or_default()
    }

    /// The version updates compare with: what the container runs (Docker),
    /// else the recorded one.
    fn installed(&self) -> String {
        self.running.clone().or_else(|| self.record.as_ref().map(|r| r.sentient_version.clone())).unwrap_or_default()
    }

    /// The version as shown: "4.4.0.3 (image tag latest)".
    fn shown(&self) -> Option<String> {
        self.record.as_ref().map(|r| crate::headless::shown_from(r, self.running.as_deref()))
    }

    /// Docker: what moving to `version` does (crate::updating::docker_step).
    fn docker_step(&self, version: &str) -> Option<crate::updating::DockerStep> {
        let r = self.record.as_ref().filter(|r| r.engine == Engine::Docker)?;
        let tag = sentient_installer_core::distro::docker_ops::image_tag(&r.stack.image);
        Some(crate::updating::docker_step(version, self.running.as_deref(), tag, r.stack.external.is_some()))
    }

    /// Install `version` (a release number, also its tag): pull its image or
    /// download its package.
    fn install_version(&mut self, version: String, cx: &mut Context<Self>) {
        let docker = self.record.as_ref().is_some_and(|r| r.engine == Engine::Docker);
        if docker { self.run_docker_to(version, cx) } else { self.run_download(version, cx) }
    }

    fn confirm_older(&mut self, version: String, window: &mut Window, cx: &mut Context<Self>) {
        let installed = self.installed();
        let this = cx.entity().downgrade();
        let text = format!(
            "SENTIENT {version} is older than {installed}, the version installed here. It is installed like an update: \
             the database is copied first, then {version} checks the database before anything changes. \
             If a newer version already changed the database ({installed} has database changes {version} does not know), \
             that check refuses and nothing is changed: {installed} keeps running. Going back is possible only between \
             versions with the same database."
        );
        crate::modal::alert(window, cx, move |dialog, _, _| {
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

    /// Docker: update to `version` (its image pulled), and the compose file
    /// then names its tag; the version it runs already only changes the tag
    /// (crate::updating::docker_step).
    fn run_docker_to(&mut self, version: String, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let target = retag(&record.stack.image, &version);
        let accept = self.accept_breaking;
        let how = self.docker_step(&version).unwrap_or(crate::updating::DockerStep::Update);
        let title = if how == crate::updating::DockerStep::QuickPin {
            format!("Pinning SENTIENT {version} to its image tag")
        } else {
            format!("Updating SENTIENT {} to {version}", self.installed())
        };
        self.plan_run(title, crate::jobplan::docker_to(self.shape(), &version, how), cx);
        self.job.update(cx, |job, cx| {
            job.start(move |sink| crate::updating::run_to(record, None, Some(&target), accept, sink).map(|_| ()), cx)
        });
    }

    /// Download the release tagged `tag` from the releases repository, then
    /// update to it.
    fn run_download(&mut self, version: String, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let accept = self.accept_breaking;
        self.plan_run(format!("Updating SENTIENT {} to {version}", self.installed()), crate::jobplan::update(self.shape(), Some(&version), Some(&version)), cx);
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
        self.refresh_running(cx);
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
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            dialog.title("Roll back the last update?").description(text.clone()).confirm().ok_text("Roll back").on_ok(move |_, _, cx| {
                this.update(cx, |page, cx| page.run_rollback(cx)).ok();
                true
            })
        });
    }

    fn run_rollback(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.plan_run("Rolling back the last update".into(), crate::jobplan::rollback(self.shape()), cx);
        if crate::rootjob::needed(&record) {
            let args = vec!["rollback".to_string()];
            self.job.update(cx, |job, cx| job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx));
        } else {
            self.job.update(cx, |job, cx| job.start(move |sink| crate::updating::rollback(record, sink).map(|_| ()), cx));
        }
    }

    fn choose_package(&mut self, cx: &mut Context<Self>) {
        let start = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose the SENTIENT package folder", start: start.as_deref(), directory: true },
            PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Use this package".into()) },
        );
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

    /// Docker: what "Update SENTIENT" does (crate::updating::plain_update):
    /// a pinned install goes to the newest release listed, or the offer.
    fn plain_update(&self, cx: &App) -> Option<crate::updating::PlainUpdate> {
        let r = self.record.as_ref().filter(|r| r.engine == Engine::Docker)?;
        let newest = match &self.versions {
            Some(Ok(list)) if !list.is_empty() => Some(list[0].version.clone()),
            // An image not published yet is not pulled.
            _ => self.updates.read(cx).sentient().filter(|o| !o.image_missing).map(|o| o.version.clone()),
        };
        let tag = sentient_installer_core::distro::docker_ops::image_tag(&r.stack.image);
        Some(crate::updating::plain_update(tag, r.stack.external.is_some(), self.running.as_deref(), newest.as_deref()))
    }

    /// "Update SENTIENT": the licence check for the version it goes to
    /// (when known: a package folder names its version), then [`Self::run`].
    fn gate_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let to = match self.plain_update(cx) {
            Some(crate::updating::PlainUpdate::To(v)) => Some(v),
            _ => self.package.as_deref().and_then(crate::updating::package_version),
        };
        let go = self.runs(|p, cx| p.run(cx), cx);
        match to {
            Some(v) => self.gated(v.clone(), licence_version::build_version(None, &v), false, go, window, cx),
            None => go(cx),
        }
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if let Some(crate::updating::PlainUpdate::To(v)) = self.plain_update(cx) {
            self.run_docker_to(v, cx);
            return;
        }
        let Some(record) = self.record.clone() else { return };
        let package = self.package.clone();
        let accept = self.accept_breaking;
        let to = package.as_deref().and_then(crate::updating::package_version);
        let title = match &to {
            Some(v) => format!("Updating SENTIENT {} to {v}", self.installed()),
            None => "Updating SENTIENT".to_string(),
        };
        self.plan_run(title, crate::jobplan::update(self.shape(), None, to.as_deref()), cx);
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

pub(crate) use crate::maintenance_ops::retag;

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
        let how = self.docker_step(&chosen.version);
        let read = self.licence_read();
        let items: Vec<(Version, bool, &'static str)> = list
            .iter()
            .map(|v| (v.clone(), v.version == chosen.version, licence_gate::menu_note(&read, &licence_version::build_version(None, &v.version))))
            .collect();
        let installed_owned = installed.to_string();
        // Docker: the entry the container runs (the tag may say "latest").
        let same_mark = if docker { " (running)" } else { " (installed)" };
        let menu = crate::ui::fitted_dropdown(
            "version-pick",
            crate::ui::button("version-pick").outline().label(format!("{} ▾", chosen.version)).disabled(busy),
            {
                let this = cx.entity().downgrade();
                move |menu, _, _| {
                    let mut menu = menu;
                    for (v, checked, note) in &items {
                        let mark = match releases::movement(&v.version, &installed_owned) {
                            Move::Same => same_mark,
                            _ => note,
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
            },
        );
        let version = chosen.version.clone();
        let tag = self.record.as_ref().map(|r| sentient_installer_core::distro::docker_ops::image_tag(&r.stack.image).to_string()).unwrap_or_default();
        let floating = docker && !releases::is_numbered(&tag);
        let external = self.record.as_ref().is_some_and(|r| r.stack.external.is_some());
        // Native Linux: a version this glibc cannot run is not offered.
        let glibc_why = if movement == Move::Same { None } else { self.glibc_block(&chosen.version, None) };
        let off = glibc_why.is_some();
        let action = match movement {
            Move::Newer => crate::ui::button("install-version")
                .primary()
                .label(format!("Update to {version}"))
                .disabled(busy || off)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let v = version.clone();
                    let go = this.runs(move |p, cx| p.install_version(v.clone(), cx), cx);
                    this.gated(version.clone(), licence_version::build_version(None, &version), false, go, window, cx);
                })),
            // Running it under another tag (latest): only the tag changes.
            Move::Same if how == Some(crate::updating::DockerStep::QuickPin) => crate::ui::button("install-version")
                .primary()
                .label(format!("Pin to {version}"))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.install_version(version.clone(), cx))),
            Move::Same if docker => crate::ui::button("install-version").outline().label(format!("{version} is running")).disabled(true),
            Move::Same => crate::ui::button("install-version").outline().label(format!("{version} is installed")).disabled(true),
            Move::Older => crate::ui::button("install-version")
                .outline()
                .label(format!("Go back to {version}"))
                .disabled(busy || off)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let v = version.clone();
                    let go = this.runs(move |p, cx| p.install_version(v.clone(), cx), cx);
                    this.gated(version.clone(), licence_version::build_version(None, &version), true, go, window, cx);
                })),
        };
        // Whether the licence covers the picked version (not for the one running).
        let read = self.licence_read();
        let chosen_pill = (movement != Move::Same).then(|| licence_gate::pill(theme, &read, &licence_version::build_version(None, &chosen.version)));
        let released = if chosen.date.is_empty() { String::new() } else { format!(", released {}", chosen.date) };
        let about = match movement {
            _ if off => glibc_why.clone().unwrap_or_default(),
            Move::Same if how == Some(crate::updating::DockerStep::QuickPin) => {
                format!("runs now under the image tag {tag}: only the tag changes, SENTIENT restarts once, no update and no database copy")
            }
            Move::Same if floating && external => format!("runs now under the image tag {tag} of your compose file: take the compose file over first (Repair and uninstall) to pin it"),
            Move::Same => String::new(),
            Move::Older => "older than the installed version: possible only while both use the same database, checked before anything changes".to_string(),
            _ if docker && floating && external => format!("pulls quakestring/sentient:{}{released}; it runs as :{tag}, the tag your compose file names", chosen.version),
            _ if docker && floating => format!("pulls quakestring/sentient:{}{released}, and the compose file names it instead of :{tag}", chosen.version),
            _ if docker => format!("pulls quakestring/sentient:{}{released}", chosen.version),
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
                        .children(chosen_pill)
                        .child(div().min_w_0().text_sm().text_color(theme.muted_foreground).child(about)),
                )
                .when(!chosen.summary.is_empty(), |c| c.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(chosen.summary.clone())))
                .into_any_element(),
        )
    }

    /// Show the release notes of `product` ("What's new").
    /// "What's new": the change log of every version between the installed
    /// one and the offered one, in a dialog.
    /// Read the record again (another page's job may have changed it).
    pub fn refresh_record(&mut self, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        self.record = InstallationRecord::load();
        self.refresh_rollback(cx);
        cx.notify();
    }

    pub fn show_notes(&mut self, product: Product, cx: &mut Context<Self>) {
        let u = self.updates.read(cx);
        let (offer, installed, name) = match product {
            Product::Manager => (u.manager().cloned(), crate::updates::manager_version().to_string(), "Platform Manager"),
            Product::Sentient => (u.sentient().cloned(), self.installed(), "SENTIENT"),
        };
        let Some(o) = offer else { return };
        open_notes(name, &installed, &o, cx);
    }

    /// Download and install the offered Platform Manager (B1).
    pub fn start_manager_update(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.updates.read(cx).manager().cloned() else { return };
        let plan = crate::updates::SELF_UPDATE_STEPS.iter().map(|s| sentient_installer_core::progress::PlanStep::new(*s).weight(if *s == "Downloading" { 4. } else { 1. })).collect();
        self.manager_job.update(cx, |job, _| job.set_plan(plan));
        self.manager_job.update(cx, |job, cx| job.start(move |sink| crate::updates::install_manager(&offer, &sink).map(|_| ()), cx));
        cx.notify();
    }

    /// "Update to <offer>": the licence check, then the update.
    fn gate_offer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.updates.read(cx).sentient().cloned() else { return };
        let go = self.runs(|p, cx| p.start_sentient_update(cx), cx);
        self.gated(o.version.clone(), o.licence_build(), false, go, window, cx);
    }

    /// Update SENTIENT to the offered version.
    fn start_sentient_update(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.updates.read(cx).sentient().cloned() else { return };
        let docker = self.record.as_ref().is_some_and(|r| r.engine == Engine::Docker);
        if docker { self.run_docker_to(o.version, cx) } else { self.run_download(o.tag, cx) }
        cx.notify();
    }

    fn apply_changes(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.changes_later = false;
        let plan = crate::jobplan::changes(&crate::changes::pending(&record));
        self.changes_job.update(cx, |job, _| job.set_plan(plan));
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
        let start = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose a Setup program or a package folder", start: start.as_deref(), directory: false },
            PathPromptOptions { files: true, directories: true, multiple: false, prompt: Some("Update from this".into()) },
        );
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
            // The run, in the shared job view (crate::manager::jobview).
            JobState::Running | JobState::Failed(_) => {
                let title = format!("Applying {} change{}", n.max(1), if n == 1 { "" } else { "s" });
                let failed = matches!(state, JobState::Failed(_));
                let acts: Vec<AnyElement> = if failed {
                    vec![crate::ui::button("changes-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| this.apply_changes(cx))).into_any_element()]
                } else {
                    Vec::new()
                };
                return Some(crate::manager::jobview::JobView::new("changes", title, &self.changes_job).actions(acts).render(cx).into_any_element());
            }
            JobState::Done(_) if pending.is_empty() => {
                let acts = vec![crate::ui::button("changes-close").outline().label("Close").on_click(cx.listener(|this, _, _, cx| this.changes_job.update(cx, |j, cx| j.reset(cx)))).into_any_element()];
                return Some(
                    crate::manager::jobview::JobView::new("changes", "The changes for this installation", &self.changes_job)
                        .result(ui::outcome(theme, Tone::Good, "The changes for this installation are applied. SENTIENT is running."))
                        .actions(acts)
                        .render(cx)
                        .into_any_element(),
                );
            }
            _ => panel
                .child(div().font_weight(FontWeight::MEDIUM).child(format!("{n} change{} for this installation", if n == 1 { "" } else { "s" })))
                .child(v_flex().gap_1().children(pending.iter().map(|c| h_flex().gap_2().text_sm().child("•").child(c.text))))
                .child(div().text_sm().text_color(theme.muted_foreground).child(crate::changes::effect(&pending)))
                .child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .child(crate::ui::probed(crate::ui::button("changes-apply").primary().label("Apply now").on_click(cx.listener(|this, _, _, cx| this.apply_changes(cx)))))
                        .child(crate::ui::probed(crate::ui::button("changes-later").outline().label("Later").on_click(cx.listener(|this, _, _, cx| {
                            this.changes_later = true;
                            cx.notify();
                        })))),
                ),
        };
        let _ = planned;
        Some(el.into_any_element())
    }

    /// A product line: name, what is known, and its buttons on the right.
    fn row(theme: &gpui_kit::component::Theme, highlight: bool, title: &'static str, sub: String) -> Div {
        let logo = if title == "SENTIENT" { ui::Product::Sentient } else { ui::Product::Manager };
        product_row(theme, highlight, logo, title, sub)
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
        let installed = self.shown();
        let mjob = self.manager_job.read(cx);
        let installing = mjob.is_running();
        let mfailed = match mjob.state() {
            JobState::Failed(m) => Some(m.clone()),
            _ => None,
        };

        let seg_theme = cx.theme().clone();
        let channels = crate::ui::seg_track(&seg_theme).children(Channel::ALL.into_iter().map(|c| {
            crate::ui::seg_item(&seg_theme, SharedString::from(format!("channel-{}", c.label())), c.label(), c == channel).on_click(cx.listener(move |this, _, _, cx| {
                this.updates.update(cx, |u, cx| u.set_channel(c, cx));
            }))
        }));

        // The offers are from an old release list (GitHub refused): said quietly.
        let stale = self.updates.read(cx).stale_note();
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
                // The Microsoft Store build: the Store updates it.
                _ if crate::store::is_store() => (format!("{this} installed. {}", crate::updates::STORE_UPDATES), Some((Tone::Neutral, "Microsoft Store"))),
                (None, _) if checking => (format!("{this} installed. Checking…"), None),
                (None, _) => (format!("{this} installed."), Some((Tone::Neutral, "Not checked"))),
                (Some(f), _) if f.manager.is_err() => (format!("{this} installed."), Some((Tone::Neutral, "Not checked"))),
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
                    .children(signature_note(theme, o, "manager"))
                    .when(has_installer, |r| {
                        r.child(crate::ui::probed(
                            crate::ui::button("manager-install")
                                .primary()
                                .label(if installing { "Updating" } else { "Update and restart" })
                                .disabled(installing || (o.unsigned && o.signing_started))
                                .on_click(cx.listener(|this, _, _, cx| this.start_manager_update(cx))),
                        ))
                    })
                    .child(crate::ui::probed(crate::ui::button("manager-notes").outline().label("What's new").on_click(cx.listener(|this, _, _, cx| this.show_notes(Product::Manager, cx)))));
            }
            row
        };
        let manager_progress = (installing || mfailed.is_some()).then(|| {
            let o = manager_offer.clone();
            let title = format!("Updating the Platform Manager: {this} to {}. SENTIENT keeps running.", o.as_ref().map(|o| o.version.clone()).unwrap_or_default());
            let acts: Vec<AnyElement> = if mfailed.is_some() {
                vec![
                    crate::ui::button("manager-close").outline().label("Close").on_click(cx.listener(|this, _, _, cx| this.manager_job.update(cx, |job, cx| job.reset(cx)))).into_any_element(),
                    crate::ui::button("manager-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| this.start_manager_update(cx))).into_any_element(),
                ]
            } else {
                Vec::new()
            };
            crate::manager::jobview::JobView::new("mgr-update", title, &self.manager_job).actions(acts).render(cx)
        });

        // SENTIENT row.
        let sentient_row = installed.as_ref().map(|inst| {
            let docker = self.record.as_ref().is_some_and(|r| r.engine == Engine::Docker);
            let busy = self.job.read(cx).is_running();
            let state = match (&found, &sentient_offer) {
                (None, _) if checking => Offered::Checking,
                (None, _) => Offered::NotChecked,
                (Some(f), _) if f.sentient.is_err() => Offered::NotChecked,
                (Some(_), None) => Offered::UpToDate,
                (Some(_), Some(o)) => Offered::Newer(o),
            };
            let (sub, pill) = sentient_summary(inst, state, manager_offer.is_some());
            // Nothing offered: what the licence covers, as a quiet line.
            let licence_note = sentient_offer.is_none().then(|| licence_gate::row_note(&self.licence_read()));
            let mut row = product_row_noted(theme, sentient_offer.is_some() && manager_offer.is_none(), ui::Product::Sentient, "SENTIENT", sub, licence_note);
            if let Some((tone, text)) = pill {
                row = row.child(ui::pill(theme, tone, text));
            }
            if let Some(o) = &sentient_offer {
                let first = manager_offer.is_some();
                // A Docker install pulls the image; the signed manifest is
                // what a native one downloads.
                if !docker {
                    row = row.children(signature_note(theme, o, "sentient"));
                }
                // Docker, image not on Docker Hub yet: said instead of the button.
                // Native Linux, a glibc too old for it: said instead of the button.
                match o.blocker().filter(|_| docker).or_else(|| self.glibc_block(&o.version, o.min_glibc.as_deref())) {
                    Some(why) => row = row.child(div().text_sm().text_color(theme.muted_foreground).child(why)),
                    None => {
                        row = row.child(licence_gate::pill(theme, &self.licence_read(), &o.licence_build())).child(crate::ui::probed(
                            crate::ui::button("sentient-update")
                                .primary()
                                .label(format!("Update to {}", o.version))
                                .disabled(first || busy || (!docker && o.unsigned && o.signing_started))
                                .on_click(cx.listener(|this, _, window, cx| this.gate_offer(window, cx))),
                        ));
                    }
                }
                row = row.child(crate::ui::probed(crate::ui::button("sentient-notes").outline().label("What's new").on_click(cx.listener(|this, _, _, cx| this.show_notes(Product::Sentient, cx)))));
            }
            let _ = docker;
            row.child(crate::ui::probed(crate::ui::button("sentient-other").outline().label("Other version").on_click(cx.listener(|this, _, _, cx| {
                this.show_other = !this.show_other;
                cx.notify();
            }))))
        });

        // The gateway here: its update (the older Python one's too).
        let gateway_offer = found.as_ref().and_then(|f| f.gateway.as_ref().ok().cloned().flatten());
        let gateway_checked = found.as_ref().map(|f| f.gateway.is_ok());
        let gw_busy = self.gateway_job.read(cx).is_running();
        let gateway_row = self.gateway.as_ref().and_then(|s| {
            let update = crate::manager::gateway::gateway_update(s, gateway_offer.as_ref(), cfg!(windows));
            let (sub, pill) = match &update {
                Some(u) => (u.about.clone(), None),
                None => crate::manager::gateway::gateway_summary(s, if checking && found.is_none() { None } else { gateway_checked.or(Some(false)) })?,
            };
            let mut row = product_row(theme, update.as_ref().is_some_and(|u| u.why.is_none()), ui::Product::Gateway, "Sentient Gateway", sub);
            if let Some((tone, text)) = pill {
                row = row.child(ui::pill(theme, tone, text));
            }
            if let Some(u) = update {
                row = row.child(crate::manager::gateway::update_button(theme, "gateway-update", &u, gw_busy, std::rc::Rc::new({
                    let this = cx.entity().downgrade();
                    move |_, cx: &mut App| {
                        this.update(cx, |p, cx| p.start_gateway_update(cx)).ok();
                    }
                })));
            }
            Some(row)
        });
        let gateway_progress = {
            let j = self.gateway_job.read(cx);
            let failed = matches!(j.state(), JobState::Failed(_));
            (j.is_running() || failed || matches!(j.state(), JobState::Done(_))).then(|| {
                let title = format!("Updating the Sentient Gateway to {}", gateway_offer.as_ref().map(|o| o.version.clone()).unwrap_or_default());
                let mut acts: Vec<AnyElement> = Vec::new();
                if !j.is_running() {
                    acts.push(crate::ui::button("gateway-close").outline().label("Close").on_click(cx.listener(|this, _, _, cx| this.gateway_job.update(cx, |job, cx| job.reset(cx)))).into_any_element());
                }
                if failed {
                    acts.push(crate::ui::button("gateway-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| this.start_gateway_update(cx))).into_any_element());
                }
                crate::manager::jobview::JobView::new("gw-update", title, &self.gateway_job).actions(acts).render(cx)
            })
        };

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
                    .flex_wrap()
                    .gap(crate::ui::button_gap())
                    .items_center()
                    .child(div().text_sm().child("Channel"))
                    .child(channels)
                    .child(div().flex_1())
                    .child(
                        crate::ui::button("check-updates")
                            .outline()
                            .label(if checking { "Checking" } else { "Check for updates" })
                            .disabled(checking)
                            .on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.check_now(cx)))),
                    ),
            )
            .when_some(stale, |p, n| p.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(n)))
            .when_some(unreachable, |p, e| {
                p.child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .flex_wrap()
                        .bg(theme.warning)
                        .text_color(theme.warning_foreground)
                        .child(div().flex_1().min_w(px(240.)).text_sm().child(if e.contains("limiting") {
                            "GitHub is limiting requests from this network. Try again in an hour.".to_string()
                        } else {
                            "The release page could not be reached. Nothing was changed.".to_string()
                        }))
                        .when(!e.contains("limiting"), |r| {
                            r.child(crate::ui::probed(crate::ui::button("retry-check").outline().label("Try again").on_click(cx.listener(|this, _, _, cx| this.updates.update(cx, |u, cx| u.check_now(cx))))))
                        })
                        .child(crate::ui::probed(crate::ui::button("from-file").outline().label("Update from a file").on_click(cx.listener(|this, _, _, cx| this.update_from_file(cx))))),
                )
            })
            .child(manager_row)
            .children(manager_progress)
            .children(sentient_row)
            .children(gateway_row)
            .children(gateway_progress)
            .when_some(hint, |p, h| p.child(div().px_3().py_2().rounded(theme.radius).bg(theme.accent).text_sm().child(h)))
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(format!("{} Last checked: {last}.", channel.about())))
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

}

impl Render for UpdatePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(v) = self.debug_gate.take() {
            let me = cx.entity().downgrade();
            window.defer(cx, move |window, cx| {
                me.update(cx, |p, cx| {
                    let go = p.runs(|_, _| {}, cx);
                    p.gated(v.clone(), licence_version::build_version(None, &v), false, go, window, cx);
                })
                .ok();
            });
        }
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Update",
            "Install a newer SENTIENT, with its database changes, and go back automatically if it fails.",
        );
        let page = page.children(self.changes_panel(&theme, cx)).child(self.updates_panel(&theme, cx));
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };
        // An update or roll back, in place of the rest (crate::manager::jobview).
        if let Some(title) = self.run_title.clone() {
            if !matches!(self.job.read(cx).state(), JobState::Idle) {
                let back = crate::manager::jobview::back_button("upd-run-back", "Update", {
                    let me = cx.entity().downgrade();
                    move |_, cx| {
                        me.update(cx, |p, cx| {
                            p.run_title = None;
                            p.job.update(cx, |j, cx| j.reset(cx));
                            p.record = InstallationRecord::load();
                            p.refresh_rollback(cx);
                            cx.notify();
                        })
                        .ok();
                    }
                });
                let view = crate::manager::jobview::JobView::new("upd-run", title, &self.job).actions([back]).render(cx);
                return ui::page(&theme, "Update", "Install a newer SENTIENT, with its database changes, and go back automatically if it fails.").child(view);
            }
        }
        let native = record.platform().needs_package();
        let job = self.job.read(cx);
        let busy = job.is_running();
        let plain = self.plain_update(cx);
        let up_to_date = plain == Some(crate::updating::PlainUpdate::UpToDate);
        // A package folder names its version: one this glibc cannot run is refused.
        let package_why = self.package.as_deref().and_then(crate::updating::package_version).and_then(|v| self.glibc_block(&v, None));
        let can_run = !busy && !up_to_date && (!native || self.package.is_some()) && package_why.is_none();
        let update_label = match (&plain, busy) {
            (_, true) => "Updating".to_string(),
            (Some(crate::updating::PlainUpdate::To(v)), _) => format!("Update to {v}"),
            (Some(crate::updating::PlainUpdate::UpToDate), _) => "Up to date".to_string(),
            _ => "Update SENTIENT".to_string(),
        };
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };

        let show_other = self.show_other || busy || outcome.is_some() || self.package.is_some();
        page.when(show_other, |page| page.child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Other version or package"))
                .child(ui::fact(&theme, if record.engine == Engine::Docker { "Running version" } else { "Installed version" }, self.shown().unwrap_or_default()))
                .when_some(self.picker(&theme, &self.installed(), record.engine == Engine::Docker, busy, cx), |panel, picker| panel.child(picker))
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
                            let build = o.licence_build();
                            let offer_pill = licence_gate::pill(&theme, &self.licence_read(), &build);
                            let unpublished = o.blocker().filter(|_| docker).or_else(|| self.glibc_block(&o.version, o.min_glibc.as_deref()));
                            v_flex()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .gap(crate::ui::button_gap())
                                        .items_center()
                                        .child(
                                            crate::ui::button("download-latest")
                                                .primary()
                                                .label(format!("Update to {version}"))
                                                .disabled(busy || unpublished.is_some())
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    let (v, t) = (version.clone(), tag.clone());
                                                    let go = this.runs(move |p, cx| if docker { p.run_docker_to(v.clone(), cx) } else { p.run_download(t.clone(), cx) }, cx);
                                                    this.gated(version.clone(), build.clone(), false, go, window, cx);
                                                })),
                                        )
                                        .child(offer_pill)
                                        .child(div().text_sm().text_color(theme.muted_foreground).child(if let Some(why) = unpublished.clone() {
                                            why
                                        } else if docker {
                                            format!("pulls quakestring/sentient:{}{}", o.version, if o.date.is_empty() { String::new() } else { format!(", released {}", o.date) })
                                        } else {
                                            format!("downloaded from the SENTIENT releases site and checked before use{}", if o.date.is_empty() { String::new() } else { format!("; released {}", o.date) })
                                        })),
                                )
                                .when(!o.notes.is_empty(), |c| {
                                    c.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(o.notes.clone()))
                                })
                                .into_any_element()
                        }
                    }
                }))
                .when(native, |panel| {
                    panel.child(
                        h_flex()
                            .gap(crate::ui::button_gap())
                            .items_center()
                            .child(
                                crate::ui::button("choose")
                                    .outline()
                                    .label("Update from a file")
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
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .child(
                            crate::ui::button("update")
                                .primary()
                                .label(update_label)
                                .disabled(!can_run)
                                .on_click(cx.listener(|this, _, window, cx| this.gate_run(window, cx))),
                        )
                        .when_some(package_why, |r, why| r.child(div().min_w_0().text_sm().text_color(theme.muted_foreground).child(why))),
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
                Some(Ok(None)) => "Nothing to roll back to: no update since the last roll back, or the last update did not keep the previous version.".into(),
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
                        crate::ui::button("rollback")
                            .danger()
                            .outline()
                            .label("Roll back last update")
                            .disabled(busy || point.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_rollback(window, cx))),
                    ),
                )
        })
        
    }
}

/// What the update channel says about SENTIENT, for its product line.
pub(crate) enum Offered<'a> {
    Checking,
    NotChecked,
    UpToDate,
    Newer(&'a crate::updates::Offer),
}

/// The SENTIENT line's text and label: the same words for this computer and
/// for a remote server.
pub(crate) fn sentient_summary(installed: &str, state: Offered, manager_first: bool) -> (String, Option<(Tone, &'static str)>) {
    match state {
        Offered::Checking => (format!("{installed} installed. Checking…"), None),
        Offered::NotChecked => (format!("{installed} installed."), Some((Tone::Neutral, "Not checked"))),
        Offered::UpToDate => (format!("{installed} is up to date."), Some((Tone::Good, "Up to date"))),
        Offered::Newer(o) if manager_first => (format!("{} is available (installed {installed}). Update the Platform Manager first.", o.version), None),
        Offered::Newer(o) => (
            format!("{} is available (installed {installed}).{}", o.version, if o.notes.is_empty() { String::new() } else { format!(" {}", o.notes) }),
            None,
        ),
    }
}

/// A product line: logo, name, what is known; the caller adds its label and
/// buttons on the right. Shared by this computer's and a server's Update.
pub(crate) fn product_row(theme: &gpui_kit::component::Theme, highlight: bool, logo: ui::Product, title: &'static str, sub: String) -> Div {
    product_row_noted(theme, highlight, logo, title, sub, None)
}

/// [`product_row`] with a quiet line under what is known (the SENTIENT
/// row's licence when no update is offered).
pub(crate) fn product_row_noted(theme: &gpui_kit::component::Theme, highlight: bool, logo: ui::Product, title: &'static str, sub: String, note: Option<String>) -> Div {
    h_flex()
        .flex_wrap()
        .gap(crate::ui::button_gap())
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
                .min_w(px(220.))
                .gap_0p5()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_sm().text_color(theme.muted_foreground).child(sub))
                .when_some(note, |c, n| c.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(n))),
        )
}

/// "What's new" in a dialog: every version between the installed one and the
/// offered one. Nothing of it is printed on the page.
pub(crate) fn open_notes(product_name: &'static str, installed: &str, offer: &crate::updates::Offer, cx: &mut App) {
    let count = offer.between.len();
    let intro = if count > 1 { format!("You have {installed}. {count} versions are newer.") } else { format!("You have {installed}.") };
    let notes: Vec<crate::notes::Notes> = offer.between.clone();
    let fallback = offer.notes.clone();
    let view = cx.new(|_| NotesDialog { intro, notes, fallback });
    crate::modal::open(
        crate::modal::Modal { title: format!("What's new in {product_name} {}", offer.version).into(), body: view.into(), width: 680., icon: None, on_close: None },
        cx,
    );
}

struct NotesDialog {
    intro: String,
    notes: Vec<crate::notes::Notes>,
    /// The release's one-line summary, when its notes could not be read.
    fallback: String,
}

impl Render for NotesDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let loaded = self.notes.iter().any(|n| n.has_entries() || !n.summary.is_empty());
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(theme.muted_foreground).child(self.intro.clone()))
            .child(if loaded {
                ui::release_notes(&theme, &self.notes).into_any_element()
            } else {
                ui::outcome(
                    &theme,
                    Tone::Warn,
                    format!("The release notes could not be loaded.{}", if self.fallback.is_empty() { String::new() } else { format!(" Short summary: {}", self.fallback) }),
                )
                .into_any_element()
            })
    }
}

/// Beside an offer of a release without a signature: "Unsigned release"
/// while no signed release exists (it is used, with this warning), "Not
/// signed" once one does (the update refuses it). crate::signing has the rule.
fn signature_note(theme: &gpui_kit::component::Theme, o: &crate::updates::Offer, id: &'static str) -> Option<AnyElement> {
    if !o.unsigned {
        return None;
    }
    let (tone, text, help) = if o.signing_started {
        (Tone::Bad, "Not signed", "This release has no signature while newer SENTIENT releases are signed, so it is not installed. Ask your supplier for a signed release.")
    } else {
        (Tone::Warn, "Unsigned release", "Published before SENTIENT releases were signed. Its download is checked against its published checksum, not against the release signature. Releases from now on are signed, and an unsigned one is then refused.")
    };
    Some(
        h_flex()
            .flex_shrink_0()
            .gap_1()
            .items_center()
            .child(ui::pill(theme, tone, text))
            .child(ui::info(SharedString::from(format!("sig-{id}")), help.into()))
            .into_any_element(),
    )
}
