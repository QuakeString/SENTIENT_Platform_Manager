// Copyright © 2016-2026 The SENTIENT Authors

//! The pages of a remote instance (crate::remote): Status, Logs,
//! Update and Backup, read from the server's own Platform Manager over SSH
//! and worked through detached jobs there. One view per open instance; the
//! menu chooses which page it shows.

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    menu::{DropdownMenu as _, PopupMenuItem},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::dbstatus::DbStatus;
use crate::jobs::{Job, JobState};
use crate::remote::{self, Instance, RemoteBackups, RemoteService, RemoteStatus};
use crate::ui::{self, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Status,
    Logs,
    Update,
    Backup,
    Settings,
    Migration,
    Repair,
    Gateway,
}

/// The SENTIENT card's Start, Stop and Restart while the page loads.
fn control_buttons_loading(theme: &gpui_kit::component::Theme) -> Div {
    h_flex()
        .gap_2()
        .child(ui::skeleton_block_r(theme, "ctl-a", px(78.).into(), 32., 6.))
        .child(ui::skeleton_block_r(theme, "ctl-b", px(78.).into(), 32., 6.))
        .child(ui::skeleton_block_r(theme, "ctl-c", px(92.).into(), 32., 6.))
}

/// What a page has read, or why it could not.
enum Read<T> {
    Loading,
    Got(T),
    Failed(String),
}

impl<T> Read<T> {
    fn got(&self) -> Option<&T> {
        match self {
            Read::Got(t) => Some(t),
            _ => None,
        }
    }
}

/// sudo on the server wants a password the Platform Manager does not have
/// (or refused the one it has).
pub(crate) fn needs_sudo_password(e: &str) -> bool {
    e.contains("needs administrator rights") || e.contains("sudo refused") || e.contains("a password is required")
}

/// Ask for the server's sudo password, keep it like the instance form does
/// (the secret store, never a log), then run `then`.
/// Deferred: from a click the window is mid-update, and updating it again
/// there fails without a word (crate::manager::takeover::open, the same).
pub(crate) fn ask_sudo_password(inst: Instance, then: std::rc::Rc<dyn Fn(&mut App)>, cx: &mut App) {
    cx.defer(move |cx| ask_sudo_password_now(inst, then, cx));
}

fn ask_sudo_password_now(inst: Instance, then: std::rc::Rc<dyn Fn(&mut App)>, cx: &mut App) {
    struct SudoPrompt {
        inst: Instance,
        input: Entity<InputState>,
        then: std::rc::Rc<dyn Fn(&mut App)>,
    }
    impl Render for SudoPrompt {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().text_sm().child(format!("sudo on {} asks for {}'s password.", self.inst.name, self.inst.user)))
                        .child(crate::ui::info(
                            "sudo-help".into(),
                            "It is kept in this computer's secret store, never in a log.\n\nIf the account has no sudo, the agent is set up in its home folder instead, and jobs that need root say so.".into(),
                        )),
                )
                .child(Input::new(&self.input).mask_toggle())
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        // An account without sudo: the agent goes in its home
                        // folder (crate::remote::choose_user_install).
                        .child(crate::ui::button("sudo-none").label("This account has no sudo").on_click(cx.listener(|p, _, _, cx| {
                            let inst = p.inst.clone();
                            let then = p.then.clone();
                            crate::modal::close(cx);
                            cx.spawn(async move |_, cx| {
                                let r = cx.background_executor().spawn(async move { remote::choose_user_install(&inst) }).await;
                                let _ = cx.update(|cx| match r {
                                    Ok(()) => then(cx),
                                    Err(e) => ui::show_error("The agent could not be set up for this account", e, cx),
                                });
                            })
                            .detach();
                        })))
                        .child(div().flex_1())
                        .child(crate::ui::button("sudo-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx)))
                        .child(crate::ui::button("sudo-save").primary().label("Save and retry").on_click(cx.listener(|p, _, _, cx| {
                            let pw = p.input.read(cx).value().to_string();
                            if pw.is_empty() {
                                return;
                            }
                            remote::set_secret(&remote::sudo_key(&p.inst.id), &pw);
                            crate::modal::close(cx);
                            (p.then)(cx);
                        }))),
                )
        }
    }
    let Some(handle) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
    let _ = handle.update(cx, |_, window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("sudo password"));
        let view = cx.new(|_| SudoPrompt { inst, input, then });
        crate::modal::open(crate::modal::Modal { title: "Administrator password needed".into(), body: view.into(), width: 480., icon: None, on_close: None }, cx);
    });
}

/// A migration's preflight, local or on a server (crate::manager::migration too).
#[derive(Clone, PartialEq)]
pub(crate) enum Preflight {
    Idle,
    Checking,
    Failed(String),
}

/// Why a migration cannot start now, in the page, with "Take over the
/// compose file…" when that is the reason.
/// A page whose action needs the Platform Manager's own compose file, on a
/// SENTIENT that runs from someone else's: the same notice as Migration's,
/// with "Take over the compose file…".
pub(crate) fn foreign_compose_notice(theme: &gpui_kit::component::Theme, headline: &str, take_over: AnyElement) -> Div {
    ui::notice_with(
        theme,
        Tone::Warn,
        format!("{headline}\nSENTIENT runs from a compose file the Platform Manager did not write.\nLet the Platform Manager take it over first; the data stays."),
        Some(take_over),
    )
}

pub(crate) fn preflight_notice(theme: &gpui_kit::component::Theme, why: &str, take_over: Option<AnyElement>) -> Div {
    // With the button beside it, the way there needs no words.
    let why = if take_over.is_some() && (why.contains("did not write") || why.contains("own compose file")) {
        "SENTIENT runs from a compose file the Platform Manager did not write.\nLet the Platform Manager take it over first; the data stays."
    } else {
        why
    };
    ui::notice_with(theme, Tone::Warn, format!("The database type cannot be changed now.\n{why}"), take_over)
}

/// How long a reading may take before the page says the server does not answer.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The server agent update started when a server is opened.
enum AgentRefresh {
    Idle,
    Running,
    Failed(String),
}

pub struct RemotePages {
    inst: Instance,
    pub kind: Kind,
    /// The server answers and has the Platform Manager.
    ready: Read<()>,
    status: Read<RemoteStatus>,
    services: Read<Vec<RemoteService>>,
    db: Read<DbStatus>,
    backups: Read<RemoteBackups>,
    settings: Read<std::collections::HashMap<String, String>>,
    /// One input per catalog setting, made once the values are read.
    inputs: Vec<(String, Entity<InputState>)>,
    gateway: Read<std::collections::HashMap<String, String>>,
    /// Migration's chosen target (an index into the types other than the one now).
    /// Migration: the type picked to change to (None: the current one).
    target: Option<sentient_installer_core::stack::Flavor>,
    /// Migration: Change was pressed; the confirmation shows.
    migrate_confirm: bool,
    /// The migration's preflight (crate::migration::preflight on the server):
    /// checked before the confirmation.
    preflight: Preflight,
    /// The page a long job was started from: it shows the job (progress,
    /// log, result) in place of its form until "Back to …".
    shown_job: Option<Kind>,
    /// A takeover started for a migration: the migration to this type runs
    /// by itself once the takeover succeeded.
    pending_migrate: Option<String>,
    /// Readings under way (fetch).
    in_flight: usize,
    /// A small save under way (run_quiet), by its section.
    saving: Option<&'static str>,
    /// The last small save: its section, how it went and its job (Retry).
    saved: Option<(&'static str, Result<(), String>, Vec<String>)>,
    /// The newest SENTIENT on the update channel (None: none newer).
    offer: Read<Option<crate::updates::Offer>>,
    /// The versions "Other version" lists.
    versions: Read<Vec<crate::online::Version>>,
    /// "add-if-one" ran once for this instance.
    auto_add_tried: bool,
    /// The server agent update that runs when the server is opened.
    agent_refresh: AgentRefresh,
    /// The sudo password was asked for after the last failure.
    sudo_asked: bool,
    /// Platform settings: the open tab (crate::manager::settings::TAB_TITLES).
    settings_tab: usize,
    /// Status: the service whose own action runs now.
    svc_busy: Option<String>,
    /// Logs: the shared view, made when the page first shows.
    log_view: Option<Entity<crate::manager::logs::LogView>>,

    /// Platform settings: what "Check" found for a file path on the server.
    file_checks: std::collections::HashMap<String, Read<bool>>,
    /// A version picked under "Other version", waiting for its confirmation.
    pick: Option<String>,
    /// This Platform Manager's update channel (shared with the local Update).
    updates: Entity<crate::updates::UpdateState>,
    follow: bool,
    job: Entity<Job>,
    job_label: Option<SharedString>,
    /// The failed job's technical details are shown.
    show_details: bool,
    scroll: ScrollHandle,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
    /// The Backup card's Keep field (made at the first draw of the page).
    keep_input: Option<Entity<InputState>>,
    /// The Restore flow, in place of the Backup page while it is open.
    restore_flow: Option<Entity<crate::manager::restore::RestoreFlow>>,
    /// A backup picked in Restore from…: the flow opens on the next draw.
    restore_pending: Option<crate::manager::restore::Source>,
    /// The restores on this server (R1).
    restores: Read<Vec<crate::restore_flow::Entry>>,
    /// Platform settings › Database: where each store keeps its data.
    locations: Option<Entity<super::movedata::LocationsCard>>,
}

impl RemotePages {
    pub fn new(inst: Instance, kind: Kind, updates: Entity<crate::updates::UpdateState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let subs = vec![
            cx.observe(&job, |this, job, cx| {
                if !job.read(cx).is_running() {
                    this.svc_busy = None;
                    // An agent update run as a job: its pill follows it.
                    if matches!(this.agent_refresh, AgentRefresh::Running) {
                        this.agent_refresh = match job.read(cx).state() {
                            JobState::Failed(m) => AgentRefresh::Failed(m.to_string()),
                            _ => AgentRefresh::Idle,
                        };
                    }
                    // sudo wants a password: ask for it and run the job again.
                    if let JobState::Failed(m) = job.read(cx).state() {
                        if needs_sudo_password(m) && !this.sudo_asked {
                            this.sudo_asked = true;
                            if let Some(retry) = job.read(cx).retry() {
                                ask_sudo_password(this.inst.clone(), retry, cx);
                            }
                        }
                    } else if !job.read(cx).is_running() {
                        this.sudo_asked = false;
                    }
                    // A takeover for a migration: the migration carries on.
                    match job.read(cx).state() {
                        JobState::Done(_) => {
                            if let Some(f) = this.pending_migrate.take() {
                                let target = f.clone();
                                let me = cx.entity().downgrade();
                                cx.defer(move |cx| {
                                    me.update(cx, |p, cx| p.run_job("Moving the database", vec!["migrate".into(), target], cx)).ok();
                                });
                            }
                        }
                        JobState::Failed(_) => this.pending_migrate = None,
                        _ => {}
                    }
                    // Whatever ran may have changed what the page knows (a
                    // takeover ends "cannot be changed now"): every reading,
                    // the preflight's too, is taken again at once, while the
                    // job's result still shows.
                    this.preflight = Preflight::Idle;
                    this.migrate_confirm = false;
                    this.reload(cx);
                }
                cx.notify();
            }),
        ];
        let mut p = Self {
            inst,
            kind,
            ready: Read::Loading,
            status: Read::Loading,
            services: Read::Loading,
            db: Read::Loading,
            backups: Read::Loading,
            settings: Read::Loading,
            inputs: Vec::new(),
            gateway: Read::Loading,
            target: None,
            migrate_confirm: false,
            // Screens: a canned preflight failure.
            shown_job: None,
            pending_migrate: None,
            in_flight: 0,
            saving: None,
            // Screens: "Saved" shown on a section (SPM_DEBUG_SAVED=folder).
            saved: std::env::var("SPM_DEBUG_SAVED").ok().filter(|_| cfg!(debug_assertions)).map(|s| (&*s.leak(), Ok(()), Vec::new())),
            preflight: std::env::var("SPM_DEBUG_PREFLIGHT").ok().filter(|_| cfg!(debug_assertions)).map(Preflight::Failed).unwrap_or(Preflight::Idle),
            offer: Read::Loading,
            versions: Read::Loading,
            pick: None,
            file_checks: Default::default(),
            log_view: None,
            svc_busy: None,
            settings_tab: super::session::load().settings_tab.filter(|i| *i < 5).unwrap_or(0),
            auto_add_tried: false,
            agent_refresh: AgentRefresh::Idle,
            sudo_asked: false,
            updates,
            follow: true,
            job,
            job_label: None,
            show_details: false,
            scroll: ScrollHandle::new(),
            _tasks: Vec::new(),
            _subscriptions: subs,
            keep_input: None,
            restore_flow: None,
            restore_pending: None,
            restores: Read::Loading,
            locations: None,
        };
        #[cfg(debug_assertions)]
        if let (Ok(frame), Kind::Backup) = (std::env::var("SPM_DEBUG_RESTORE"), kind) {
            p.restore_pending = Some(crate::manager::restore::canned::source(&frame));
        }
        // Development aid: open Platform settings on a tab (screens).
        #[cfg(debug_assertions)]
        if let Ok(t) = std::env::var("SPM_SETTINGS_TAB") {
            let names = ["platform", "database", "network", "retention", "advanced"];
            p.settings_tab = names.iter().position(|n| n.eq_ignore_ascii_case(&t)).unwrap_or(0);
        }
        p.reload(cx);
        p.refresh_agent(cx);
        p
    }

    pub fn show(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if self.kind != kind {
            self.kind = kind;
            self.reload(cx);
        }
        cx.notify();
    }

    /// Read what the page shows (in the background).
    /// A server set up by an earlier Platform Manager keeps its older agent
    /// until a job runs, and the pages need the new one's jobs (per-service
    /// control, log sources): bring it up to date when the server is opened,
    /// then read again. Only on a server that is set up already.
    fn refresh_agent(&mut self, cx: &mut Context<Self>) {
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_REMOTE_STATUS").is_some() {
            // Development aid: show a refresh state (screens).
            self.agent_refresh = match std::env::var("SPM_DEBUG_AGENT_REFRESH").as_deref() {
                Ok("updating") => AgentRefresh::Running,
                Ok("failed") => AgentRefresh::Failed("The server did not answer (address, port or firewall).".into()),
                _ => AgentRefresh::Idle,
            };
            return;
        }
        let inst = self.inst.clone();
        self.agent_refresh = AgentRefresh::Running;
        let task = cx.spawn(async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move {
                    remote::ready(&inst)?;
                    remote::deploy(&inst, &sentient_installer_core::progress::noop())
                })
                .await;
            this.update(cx, |p, cx| {
                match done {
                    Ok(_) => {
                        p.agent_refresh = AgentRefresh::Idle;
                        p.reload(cx);
                    }
                    Err(e) => p.agent_refresh = AgentRefresh::Failed(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// A reading came back "agent out of date".
    fn agent_old(&self) -> bool {
        let old = |e: &str| e == remote::AGENT_OLD;
        matches!(&self.ready, Read::Failed(e) if old(e))
            || matches!(&self.status, Read::Failed(e) if old(e))
            || matches!(&self.services, Read::Failed(e) if old(e))
            || matches!(&self.db, Read::Failed(e) if old(e))
            || matches!(&self.backups, Read::Failed(e) if old(e))
            || matches!(&self.settings, Read::Failed(e) if old(e))
            || matches!(&self.gateway, Read::Failed(e) if old(e))
    }

    /// Update the server agent as a job (the progress chip; on failure its
    /// dialog with the error, Copy and Retry).
    pub(crate) fn update_agent(&mut self, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let inst = self.inst.clone();
        let me = cx.entity().downgrade();
        self.agent_refresh = AgentRefresh::Running;
        self.job.update(cx, |job, cx| {
            job.set_label(format!("Updating the server agent on {}", inst.name));
            job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
            job.set_retry(std::rc::Rc::new(move |cx: &mut App| {
                me.update(cx, |p, cx| p.update_agent(cx)).ok();
            }));
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    sink(sentient_installer_core::progress::Progress::Done { message: format!("The server agent on {} is up to date.", inst.name) });
                    Ok(())
                },
                cx,
            )
        });
        cx.notify();
    }

    /// Whether SENTIENT runs there, as this page last read it.
    pub(crate) fn serving(&self) -> Option<bool> {
        match &self.status {
            Read::Got(s) if s.installed => Some(s.serving),
            _ => None,
        }
    }

    /// A job ended on this server: the page reads again, except one showing
    /// that job's result (it reads again on "Back to …"); its status is read
    /// either way.
    pub(crate) fn after_job(&mut self, cx: &mut Context<Self>) {
        if self.shown_job.is_some() {
            let inst = self.inst.clone();
            self.fetch(cx, move || remote::status(&inst), |p, r| p.status = r);
        } else {
            self.reload(cx);
        }
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(l) = self.locations.clone() {
            l.update(cx, |l, cx| l.reload(cx));
        }
        let inst = self.inst.clone();
        self.fetch(cx, move || remote::ready(&inst), |p, r| p.ready = r);
        // Every page needs to know whether a SENTIENT is added there.
        if !matches!(self.kind, Kind::Status | Kind::Update | Kind::Migration | Kind::Repair) {
            let inst = self.inst.clone();
            self.fetch(cx, move || remote::status(&inst), |p, r| p.status = r);
        }
        let inst = self.inst.clone();
        match self.kind {
            Kind::Status => {
                self.fetch(cx, move || remote::status(&inst), |p, r| p.status = r);
                let inst = self.inst.clone();
                self.fetch(cx, move || remote::services(&inst), |p, r| p.services = r);
                let inst = self.inst.clone();
                self.fetch(cx, move || remote::dbstatus(&inst), |p, r| p.db = r);
            }
            Kind::Logs => {}
            Kind::Update => {
                let i2 = inst.clone();
                self.fetch(cx, move || remote::status(&i2), |p, r| p.status = r);
                let native = inst.detected.as_ref().is_some_and(|d| d.install != "Docker");
                self.pick = None;
                self.fetch(
                    cx,
                    move || {
                        #[cfg(debug_assertions)]
                        if std::env::var_os("SPM_DEBUG_REMOTE_OFFER").is_some() {
                            let v = |version: &str, date: &str| crate::online::Version { version: version.into(), date: date.into(), summary: String::new() };
                            return Ok(vec![v("4.4.0.2", "2026-11-14"), v("4.4.0.1", "2026-11-13"), v("4.3.0.58", "2026-11-02")]);
                        }
                        crate::online::versions(native)
                    },
                    |p, r| p.versions = r,
                );
                self.fetch(
                    cx,
                    move || {
                        #[cfg(debug_assertions)]
                        if std::env::var_os("SPM_DEBUG_REMOTE_OFFER").is_some() {
                            return Ok(Some(canned_offer()));
                        }
                        let installed = remote::status(&inst)?.sentient_version;
                        let channel = crate::updates::Prefs::load().channel;
                        crate::updates::check(channel, Some(installed), native).sentient
                    },
                    |p, r| p.offer = r,
                );
            }
            Kind::Backup => {
                let i2 = inst.clone();
                self.fetch(cx, move || remote::backups(&i2), |p, r| p.backups = r);
                self.fetch(
                    cx,
                    move || {
                        #[cfg(debug_assertions)]
                        if std::env::var("SPM_DEBUG_RESTORE_HISTORY").is_ok_and(|v| v == "canned") {
                            return Ok(crate::manager::restore::canned::history());
                        }
                        remote::restore_history(&inst)
                    },
                    |p, r| p.restores = r,
                );
            }
            Kind::Settings => {
                self.inputs.clear();
                self.fetch(cx, move || remote::settings(&inst), |p, r| p.settings = r);
            }
            Kind::Migration | Kind::Repair => self.fetch(cx, move || remote::status(&inst), |p, r| p.status = r),
            Kind::Gateway => self.fetch(cx, move || remote::gateway(&inst), |p, r| p.gateway = r),
        }
    }

    fn fetch<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
        put: impl FnOnce(&mut Self, Read<T>) + 'static,
    ) {
        // Whichever comes first: the answer, or 20 seconds without one (a
        // reading never waits forever).
        // Counted while it runs (the Refresh icon turns meanwhile).
        self.in_flight += 1;
        let put = std::rc::Rc::new(std::cell::RefCell::new(Some(put)));
        let p2 = put.clone();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { work() }).await;
            this.update(cx, |p, cx| {
                if let Some(put) = put.borrow_mut().take() {
                    p.in_flight = p.in_flight.saturating_sub(1);
                    put(p, match got {
                        Ok(v) => Read::Got(v),
                        Err(e) => Read::Failed(e),
                    });
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        let timeout = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(READ_TIMEOUT).await;
            this.update(cx, |p, cx| {
                if let Some(put) = p2.borrow_mut().take() {
                    p.in_flight = p.in_flight.saturating_sub(1);
                    put(p, Read::Failed(format!("The server did not answer within {} seconds.", READ_TIMEOUT.as_secs())));
                    cx.notify();
                }
            })
            .ok();
        });
        self._tasks.push(timeout);
    }

    /// A small save (a setting, a delete): no job view, no chip, nothing in
    /// the bell. The thin line under the top bar runs meanwhile; then the
    /// section says Saved, or what went wrong with Retry, and is read again.
    fn run_quiet(&mut self, section: &'static str, args: Vec<String>, cx: &mut Context<Self>) {
        if self.saving.is_some() || self.job.read(cx).is_running() {
            return;
        }
        self.saving = Some(section);
        self.saved = None;
        crate::manager::strip::quiet(true, cx);
        let inst = self.inst.clone();
        let a2 = args.clone();
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let sink = sentient_installer_core::progress::noop();
                    let a: Vec<&str> = a2.iter().map(String::as_str).collect();
                    remote::start_job(&inst, &a).and_then(|id| remote::follow(&inst, &id, &sink))
                })
                .await;
            let _ = cx.update(|cx| crate::manager::strip::quiet(false, cx));
            this.update(cx, |p, cx| {
                p.saving = None;
                p.saved = Some((section, r.map_err(|e| remote::split_failure(&e).0), args));
                p.reload(cx);
                cx.notify();
                // The tick goes after a few seconds; an error stays.
                let hide = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                    this.update(cx, |p, cx| {
                        if p.saved.as_ref().is_some_and(|(s, r, _)| *s == section && r.is_ok()) {
                            p.saved = None;
                            cx.notify();
                        }
                    })
                    .ok();
                });
                p._tasks.push(hide);
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// A section's title with its small save's outcome beside it.
    fn section_title(&self, theme: &gpui_kit::component::Theme, title: &'static str, section: &'static str, cx: &mut Context<Self>) -> Div {
        let outcome = self.saved_note(theme, section, cx);
        h_flex().gap_3().items_center().min_h(px(28.)).child(div().font_weight(FontWeight::MEDIUM).child(title)).children(outcome)
    }

    /// "Saved" after a small save of `section` (it fades in), or why not
    /// with Retry.
    fn saved_note(&self, theme: &gpui_kit::component::Theme, section: &'static str, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.saved {
            Some((s, Ok(()), _)) if *s == section => Some(
                h_flex()
                    .gap_1()
                    .items_center()
                    .text_sm()
                    .text_color(theme.success_foreground)
                    .child(gpui_kit::component::Icon::new(gpui_kit::component::IconName::Check).size_4())
                    .child("Saved")
                    // It fades in (150 ms).
                    .with_animation(SharedString::from(format!("{section}-saved")), Animation::new(std::time::Duration::from_millis(150)).with_easing(crate::ui::ease_out), |el, t| el.opacity(t))
                    .into_any_element(),
            ),
            Some((s, Err(e), args)) if *s == section => {
                let args = args.clone();
                Some(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .min_w_0()
                        .child(div().min_w_0().text_sm().text_color(theme.danger).overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("Not saved: {e}")))
                        .child(crate::ui::button(SharedString::from(format!("{section}-retry"))).outline().label("Retry").on_click(cx.listener(move |this, _, _, cx| this.run_quiet(section, args.clone(), cx))))
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }

    fn run_job(&mut self, label: impl Into<String>, args: Vec<String>, cx: &mut Context<Self>) {
        let label: String = label.into();
        // Never silently: another job still runs here.
        if self.job.read(cx).is_running() {
            let other = self.job_label.clone().map(|l| l.to_string()).unwrap_or_else(|| "another job".into());
            ui::show_error(format!("{label} cannot start yet"), format!("{other} is still running on {}. It starts once that has finished; try again then.", self.inst.name), cx);
            return;
        }
        // Long actions show on their page (crate::manager::jobview).
        if matches!(self.kind, Kind::Migration | Kind::Update | Kind::Backup | Kind::Repair | Kind::Settings) {
            self.shown_job = Some(self.kind);
        }
        let inst = self.inst.clone();
        self.job_label = Some(format!("{label} on {}", inst.name).into());
        let strip_label = format!("{label} on {}", inst.name);
        let me = cx.entity().downgrade();
        let (l2, a2) = (label.clone(), args.clone());
        let on_page = self.shown_job == Some(self.kind);
        self.job.update(cx, |job, cx| {
            job.set_label(strip_label);
            job.set_on_page(on_page);
            job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
            job.set_retry(std::rc::Rc::new(move |cx: &mut App| {
                let (l, a) = (l2.clone(), a2.clone());
                me.update(cx, |p, cx| p.run_job(l, a, cx)).ok();
            }));
            job.start(
                move |sink| {
                    // The agent is brought up to date first; when that fails
                    // the job still runs on the agent there, and only a job it
                    // does not know says the agent could not be updated.
                    let updated = remote::deploy(&inst, &sink);
                    if let Err(e) = &updated {
                        sink(sentient_installer_core::progress::Progress::Log { line: format!("The agent could not be updated: {}", remote::split_failure(e).0) });
                    }
                    sink(sentient_installer_core::progress::Progress::Step { name: label.to_string() });
                    let a: Vec<&str> = args.iter().map(String::as_str).collect();
                    let run = remote::start_job(&inst, &a).and_then(|id| remote::follow(&inst, &id, &sink));
                    match (run, updated) {
                        (Err(e), Err(u)) if e == remote::AGENT_OLD || e.contains("usage: --job") => Err(remote::failed("The agent on this server could not be updated, and the one there is too old for this.", &remote::split_failure(&u).1.unwrap_or(u.clone()))),
                        (r, _) => r,
                    }
                },
                cx,
            )
        });
        cx.notify();
    }

    // -- rendering ----------------------------------------------------------

    /// The long job started from this page, in place of its form: running,
    /// or ended and not yet left with "Back to …".
    fn job_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.shown_job != Some(self.kind) {
            return None;
        }
        let job = self.job.read(cx);
        if matches!(job.state(), JobState::Idle) {
            return None;
        }
        let label = self.job_label.clone().map(|l| l.to_string()).unwrap_or_default();
        let migrating = label.starts_with("Moving the database") || self.pending_migrate.is_some();
        let page = match self.kind {
            Kind::Migration => "Migration",
            Kind::Update => "Update",
            Kind::Backup => "Backup and restore",
            Kind::Repair => "Repair and uninstall",
            Kind::Settings => "Platform settings",
            _ => "the page",
        };
        let title = if self.pending_migrate.is_some() { format!("{label} (the migration follows)") } else { label };
        let this = cx.entity().downgrade();
        let back = crate::manager::jobview::back_button("job-back", page, move |_, cx| {
            this.update(cx, |p, cx| {
                p.shown_job = None;
                p.job.update(cx, |j, cx| j.reset(cx));
                p.reload(cx);
            })
            .ok();
        });
        let retry = match job.state() {
            JobState::Failed(_) => job.retry().map(|r| crate::ui::button("job-retry").primary().label("Try again").on_click(move |_, _, cx| r(cx)).into_any_element()),
            _ => None,
        };
        let stages: &[crate::manager::jobview::Stage] = if migrating && self.pending_migrate.is_none() { crate::manager::jobview::MIGRATION } else { &[] };
        Some(crate::manager::jobview::view("rjob", title, stages, job, Some(back), retry, cx))
    }

    fn failed(theme: &gpui_kit::component::Theme, e: &str) -> Div {
        ui::notice(theme, Tone::Bad, e.to_string())
    }

    /// This page's final layout with placeholders, while it loads: the same
    /// cards and labels, so nothing moves when the values arrive.
    fn skeleton(&self, theme: &gpui_kit::component::Theme) -> Div {
        let status_card = || {
            ui::panel(theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(ui::skeleton_block(theme, "st-logo", px(30.).into(), 30.))
                        .child(ui::skeleton_block(theme, "st-pill", px(150.).into(), 24.))
                        .child(div().flex_1())
                        .child(ui::skeleton_block(theme, "st-open", px(124.).into(), 32.)),
                )
                .child(ui::card_fact_loading(theme, "SENTIENT version", 120.))
                .child(ui::card_fact_loading(theme, "Database", 170.))
                .child(ui::card_fact_loading(theme, "Runs as", 70.))
                .child(ui::card_fact_loading(theme, "Server", 220.))
                .child(ui::card_fact_loading(theme, "Server agent", 160.))
                .children(ui::card_foot(theme, control_buttons_loading(theme)))
        };
        let services = || {
            ui::panel(theme)
                .gap_2()
                .child(crate::manager::status::services_title())
                .children(ui::skeleton_service_rows(theme, "svc", 3))
                .children(ui::card_foot(theme, ui::skeleton_line(theme, "svc-foot", 200.)))
        };
        let col = v_flex().gap(px(ui::GAP));
        match self.kind {
            Kind::Status => col
                .child(ui::card_pair(status_card(), services()))
                .child(
                    ui::panel(theme)
                        .child(div().font_weight(FontWeight::MEDIUM).child("Database"))
                        .child(ui::skeleton_line(theme, "db-disk", 220.))
                        .child(ui::skeleton_block(theme, "db-bar", relative(1.).into(), 10.))
                        .child(ui::skeleton_line(theme, "db-legend", 420.)),
                ),
            Kind::Backup => col
                .child(ui::skeleton_card(theme, "bk-now", Some("Back up now"), &["Save to"]))
                .child(ui::skeleton_card(theme, "bk-auto", Some("Automatic backup"), &["Back up every day", "Keep", "Include"]))
                .child(ui::skeleton_card(theme, "bk-list", Some("Backups"), &["", "", ""])),
            Kind::Gateway => col.child(ui::skeleton_card(theme, "gw", Some("Sentient Gateway"), &["Version", "Runs as", "Connects to"])),
            Kind::Settings => col.child(ui::skeleton_card(theme, "set", None, &["", "", "", "", ""])),
            Kind::Update => col.child(ui::skeleton_card(theme, "upd", None, &["", ""])),
            _ => col.child(ui::skeleton_card(theme, "page", None, &["", "", ""])),
        }
    }

    /// No SENTIENT added on the server yet. Not an error: what was found and
    /// the one action. Adding writes only the Platform Manager's own record.
    fn not_added_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let det = self.inst.detected.clone().unwrap_or_default();
        ui::panel(&theme)
            .child(h_flex().gap_3().items_center().child(ui::product_logo(ui::Product::Sentient, 30.)).child(ui::pill(&theme, Tone::Neutral, "Not added")))
            .child(div().text_sm().text_color(theme.muted_foreground).child(if det.install.is_empty() {
                "The server agent is set up; no SENTIENT on this server is added to the Platform Manager. Add existing finds a SENTIENT that runs on the server (in Docker, or installed some other way). Nothing on it is changed.".to_string()
            } else {
                format!("SENTIENT was found on this server ({}). Add existing lets the Platform Manager look after it. Nothing on it is changed.", det.install)
            }))
            .child(h_flex().gap_2().child(
                crate::ui::button("r-adopt").primary().label("Add existing SENTIENT").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Adding the existing SENTIENT", vec!["add-if-one".into()], cx))),
            ))
    }

    fn status_page(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.job.read(cx).is_running();
        let theme = cx.theme().clone();
        let mut col = v_flex().gap(px(ui::GAP));
        let services = self.services_card(cx);
        match &self.status {
            Read::Loading => return self.skeleton(&theme),
            Read::Failed(e) => col = col.child(Self::failed(&theme, e)).child(services),
            Read::Got(s) if !s.installed => {
                // Nothing added there yet: only the calm panel, no empty
                // Services or Database panels.
                return col.child(self.not_added_panel(cx));
            }
            Read::Got(s) => {
                let det = self.inst.detected.clone().unwrap_or_default();
                // Its port answers but none of its own services runs: some
                // other program is serving there.
                let none_running = matches!(&self.services, Read::Got(list) if !list.is_empty() && !list.iter().any(|v| v.running || v.starting));
                let (tone, state) = if !s.installed {
                    (Tone::Neutral, "SENTIENT is not installed")
                } else if s.serving && none_running {
                    (Tone::Warn, "Another program answers on SENTIENT's port")
                } else if s.serving {
                    (Tone::Good, "SENTIENT is running")
                } else {
                    (Tone::Bad, "SENTIENT is stopped")
                };
                let url = format!("http://{}:{}", self.inst.host, s.http_port.unwrap_or(8080));
                let this = cx.entity().downgrade();
                let (t1, t2, t3) = (this.clone(), this.clone(), this);
                let controls = crate::manager::status::control_buttons(
                    &theme,
                    "r-",
                    busy,
                    Some(s.serving),
                    std::rc::Rc::new(move |_, cx| {
                        t1.update(cx, |p, cx| p.run_job("Starting SENTIENT", vec!["control".into(), "start".into()], cx)).ok();
                    }),
                    std::rc::Rc::new(move |_, cx| {
                        t2.update(cx, |p, cx| {
                            p.run_job("Restarting SENTIENT", vec!["control".into(), "restart".into()], cx)
                        })
                        .ok();
                    }),
                    std::rc::Rc::new(move |w, cx| {
                        let t3 = t3.clone();
                        crate::manager::status::confirm_stop(w, cx, std::rc::Rc::new(move |cx| {
                            t3.update(cx, |p, cx| p.run_job("Stopping SENTIENT", vec!["control".into(), "stop".into()], cx)).ok();
                        }));
                    }),
                );
                let card = ui::panel(&theme)
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_3()
                                .items_center()
                                .child(ui::product_logo(ui::Product::Sentient, 30.))
                                .child(ui::pill(&theme, tone, state))
                                .child(div().flex_1())
                                .child(h_flex().ml_auto().child(ui::open_sentient_button("r-open", s.serving, move |_, _, cx| ui::open_url(&url, cx)))),
                        )
                        .child(ui::card_fact(&theme, "SENTIENT version", s.sentient_version.clone()))
                        .child(ui::card_fact(&theme, "Database", s.database.clone()))
                        .when(!det.install.is_empty(), |p| p.child(ui::card_fact(&theme, "Runs as", det.install.clone())))
                        .child(ui::card_fact(&theme, "Server", format!("{} · {} {}", self.inst.address(), det.os_name, det.arch)))
                        .child(
                            // A pill only while the update runs or when it failed;
                            // the normal state has none.
                            // On a narrow card the pill goes under the row.
                            h_flex()
                                .flex_wrap()
                                .gap_x_3()
                                .gap_y_1()
                                .items_center()
                                .child(ui::card_fact(&theme, "Server agent", s.manager_version.clone()).flex_1().min_w(px(200.)))
                                .map(|r| match &self.agent_refresh {
                                    AgentRefresh::Running => r.child(ui::pill(&theme, Tone::Warn, "Updating")),
                                    AgentRefresh::Failed(why) => {
                                        // A button: the real error, Copy and Retry.
                                        let why = why.clone();
                                        let this = cx.entity().downgrade();
                                        let inst_for_sudo = self.inst.clone();
                                        r.child(div().id("agent-failed").cursor_pointer().child(ui::pill(&theme, Tone::Bad, "Update failed ›")).on_click(move |_, _, cx| {
                                            let this = this.clone();
                                            if needs_sudo_password(&why) {
                                                let t = this.clone();
                                                ask_sudo_password(inst_for_sudo.clone(), std::rc::Rc::new(move |cx: &mut App| {
                                                    t.update(cx, |p, cx| p.update_agent(cx)).ok();
                                                }), cx);
                                                return;
                                            }
                                            ui::show_error_with(
                                                "The server agent could not be updated",
                                                why.clone(),
                                                Some(std::rc::Rc::new(move |cx: &mut App| {
                                                    this.update(cx, |p, cx| p.update_agent(cx)).ok();
                                                })),
                                                cx,
                                            );
                                        }))
                                    }
                                    AgentRefresh::Idle => r,
                                }),
                        )
                        .children(ui::card_foot(&theme, controls));
                col = col.child(ui::card_pair(card, services));
            }
        }
        // The same Database panel as this computer's, read without asking.
        let db = ui::panel(&theme).child(div().font_weight(FontWeight::MEDIUM).child("Database"));
        let db = match &self.db {
            Read::Loading => db.child(ui::skeleton_line(&theme, "db-disk", 220.)).child(ui::skeleton_block(&theme, "db-bar", relative(1.).into(), 10.)).child(ui::skeleton_line(&theme, "db-legend", 420.)),
            // A notice in the page in place of the card, with Retry.
            Read::Failed(e) => ui::notice_with(
                &theme,
                Tone::Warn,
                format!("The database's size and state could not be read.\n{e}"),
                Some(crate::ui::button("db-retry").outline().label("Retry").on_click(cx.listener(|p, _, _, cx| p.reload(cx))).into_any_element()),
            ),
            Read::Got(d) => {
                // From the server's status (the same as its tile); "None" only
                // once the status says so, never while it is being read.
                let last = match &self.status {
                    Read::Got(s) => s.last_backup.clone().unwrap_or_else(|| "None".into()),
                    _ => String::new(),
                };
                let this = cx.entity().downgrade();
                let back_up: std::rc::Rc<dyn Fn(&mut App)> = std::rc::Rc::new(move |cx| {
                    this.update(cx, |p, cx| p.run_job("Backing up", vec!["backup-now".into()], cx)).ok();
                });
                crate::manager::status::db_details(
                    &theme,
                    db,
                    d,
                    "The server's database disk",
                    None,
                    last,
                    crate::manager::status::DbActions {
                        move_data: Some({
                            let inst = self.inst.clone();
                            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                                crate::manager::movedata::open(crate::movedata::Target::Remote(inst.clone()), inst.name.clone(), None, window, cx)
                            })
                        }),
                        set_limit: None,
                        remove_kept: None,
                        back_up: Some(back_up),
                        broken_parts: Some({
                            let inst = self.inst.clone();
                            std::rc::Rc::new(move |cx: &mut App| {
                                let (i1, i2) = (inst.clone(), inst.clone());
                                crate::manager::status::BrokenParts::open(
                                    std::sync::Arc::new(move || remote::ch_detached(&i1)),
                                    std::sync::Arc::new(move |attach, parts| remote::ch_parts(&i2, attach, &parts)),
                                    cx,
                                )
                            })
                        }),
                    },
                    crate::theme::is_dark(cx),
                )
            }
        };
        col.child(db)
    }

    /// Status's Services: each row with Start, Stop, Restart and Logs, and
    /// the totals. The database's own stop or restart asks first while
    /// SENTIENT runs.
    fn services_card(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut p = ui::panel(&theme).gap_2().child(crate::manager::status::services_title());
        let serving = self.status.got().is_some_and(|s| s.serving);
        let this = cx.entity().downgrade();
        let act = crate::manager::status::service_act(serving, std::rc::Rc::new(move |name, action, cx| {
            this.update(cx, |p, cx| {
                p.svc_busy = Some(name.clone());
                p.run_job(crate::manager::status::service_job_label(&name, action), vec!["service".into(), action.into(), name.clone()], cx);
            })
            .ok();
        }));
        match &self.services {
            Read::Loading => p = p.children(ui::skeleton_service_rows(&theme, "svc", 3)),
            Read::Failed(e) => p = p.child(ui::outcome(&theme, Tone::Bad, e.clone())),
            Read::Got(list) => {
                let up = list.iter().filter(|s| s.running).count();
                p = p
                    .children(ui::service_rows(&theme, list.iter().map(|s| (s.name.clone().into(), s.running, s.starting, s.detail.clone().into())), (act, crate::manager::status::service_log(), self.svc_busy.clone())))
                    .children(ui::card_foot(&theme, ui::services_foot(&theme, up, list.len())));
            }
        }
        p
    }

    /// The same Logs view as this computer's, reading through the agent.
    fn logs_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let inst = self.inst.clone();
        let view = self.log_view.get_or_insert_with(|| cx.new(|cx| crate::manager::logs::LogView::remote(inst, window, cx))).clone();
        v_flex().child(view)
    }

    /// The same layout as this computer's Update: the channel, then one
    /// SENTIENT line with its buttons; the change log opens in a dialog.
    fn update_page(&self, cx: &mut Context<Self>) -> Div {
        use crate::manager::update::{Offered, product_row, sentient_summary};
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().map(|s| s.sentient_version.clone()).unwrap_or_else(|| "Reading…".into());
        let channel = self.updates.read(cx).prefs.channel;
        let checking = matches!(self.offer, Read::Loading);
        let channels = ui::seg_track(&theme).children(sentient_installer_core::releases::Channel::ALL.into_iter().map(|c| {
            ui::seg_item(&theme, SharedString::from(format!("r-channel-{}", c.label())), c.label(), c == channel).on_click(cx.listener(move |this, _, _, cx| {
                this.updates.update(cx, |u, cx| u.set_channel(c, cx));
                this.reload(cx);
            }))
        }));
        let offer = match &self.offer {
            Read::Got(o) => o.clone(),
            _ => None,
        };
        let state = match &self.offer {
            Read::Loading => Offered::Checking,
            Read::Failed(_) => Offered::NotChecked,
            Read::Got(None) => Offered::UpToDate,
            Read::Got(Some(o)) => Offered::Newer(o),
        };
        let (sub, pill) = sentient_summary(&installed, state, false);
        let mut row = product_row(&theme, offer.is_some(), ui::Product::Sentient, "SENTIENT", sub);
        if let Some((tone, text)) = pill {
            row = row.child(ui::pill(&theme, tone, text));
        }
        if let Some(o) = &offer {
            let version = o.version.clone();
            let (o2, inst_v) = (o.clone(), installed.clone());
            row = row
                .child(
                    crate::ui::button("r-update")
                        .primary()
                        .label(format!("Update to {}", o.version))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.run_job("Updating SENTIENT", vec!["update".into(), "--version".into(), version.clone()], cx))),
                )
                .child(crate::ui::button("r-notes").outline().label("What's new").on_click(move |_, _, cx| crate::manager::update::open_notes("SENTIENT", &inst_v, &o2, cx)));
        }
        let versions: Vec<crate::online::Version> = match &self.versions {
            Read::Got(v) => v.iter().filter(|v| v.version != installed).cloned().collect(),
            _ => Vec::new(),
        };
        let this = cx.entity().downgrade();
        row = row.child(
            crate::ui::button("r-other")
                .outline()
                .label("Other version")
                .disabled(busy || versions.is_empty())
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for v in &versions {
                        let (t, ver) = (this.clone(), v.version.clone());
                        menu = menu.item(PopupMenuItem::new(v.label()).on_click(move |_, _, cx| {
                            t.update(cx, |p, cx| {
                                p.pick = Some(ver.clone());
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                }),
        );
        let failed = match &self.offer {
            Read::Failed(e) => Some(e.clone()),
            _ => None,
        };
        let pick = self.pick.clone();
        ui::panel(&theme)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .flex_wrap()
                    .child(div().text_sm().child("Channel"))
                    .child(channels)
                    .child(div().flex_1())
                    .child(
                        crate::ui::button("r-check")
                            .outline()
                            .label(if checking { "Checking…" } else { "Check for updates" })
                            .disabled(checking)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .when_some(failed, |p, e| {
                p.child(ui::outcome(
                    &theme,
                    Tone::Warn,
                    if e.contains("limiting") { "GitHub is limiting requests from this network. Try again in an hour." } else { "The release page could not be reached. Nothing was changed." },
                ))
            })
            .child(row)
            .when_some(pick, |p, v| {
                let v2 = v.clone();
                p.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .child(div().flex_1().min_w(px(220.)).text_sm().child(format!("Install SENTIENT {v} on this server instead of {installed}?")))
                        .child(crate::ui::button("r-pick-go").primary().label(format!("Install {v}")).disabled(busy).on_click(cx.listener(move |this, _, _, cx| {
                            this.pick = None;
                            this.run_job("Updating SENTIENT", vec!["update".into(), "--version".into(), v2.clone()], cx);
                        })))
                        .child(crate::ui::button("r-pick-no").ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.pick = None;
                            cx.notify();
                        }))),
                )
            })
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(
                "The update runs on the server by itself: a dropped connection does not stop it, and the previous version comes back if SENTIENT does not start.",
            ))
    }

    fn confirm_delete(&mut self, path: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::modal::alert(window, cx, move |d, _, _| {
            let (this, path) = (this.clone(), path.clone());
            d.title(format!("Delete {name}?")).description("The file is deleted from the server. This cannot be undone.").confirm().ok_text("Delete").on_ok(move |_, _, cx| {
                this.update(cx, |p, cx| p.run_quiet("backups", vec!["backup-delete".into(), path.clone()], cx)).ok();
                true
            })
        });
    }

    /// The Keep field: made once, following the server's value while it is
    /// not being edited; saved on Enter or when it is left.
    fn ensure_keep_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Read::Got(b) = &self.backups else { return };
        let keep = b.keep;
        if let Some(input) = &self.keep_input {
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            if !focused && input.read(cx).value().trim() != keep.to_string() && self.saving.is_none() {
                input.update(cx, |s, cx| s.set_value(keep.to_string(), window, cx));
            }
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(keep.to_string()));
        self._subscriptions.push(cx.subscribe_in(&input, window, |p: &mut Self, input, e: &InputEvent, window, cx| {
            if !matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                return;
            }
            let Read::Got(b) = &p.backups else { return };
            let (now, at, include) = (
                b.keep,
                b.daily_at.clone().unwrap_or_else(|| "off".into()),
                match (b.preset, b.history) {
                    (crate::backups::Preset::Configuration, _) => "configuration",
                    (_, crate::backups::History::LastDays(_)) => "last90",
                    _ => "everything",
                },
            );
            match crate::manager::backup::keep_value(&input.read(cx).value()) {
                Some(k) => {
                    input.update(cx, |s, cx| s.set_value(k.to_string(), window, cx));
                    if k != now {
                        p.run_quiet("auto", vec!["backup-auto".into(), at, k.to_string(), include.into()], cx);
                    }
                }
                None => input.update(cx, |s, cx| s.set_value(now.to_string(), window, cx)),
            }
        }));
        self.keep_input = Some(input);
    }

    /// B2–B4: the folder chooser on the server; the choice is saved there
    /// (the backups in the old folder moved along or left).
    fn open_folder_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Read::Got(b) = &self.backups else { return };
        let here: Vec<&crate::backups::Entry> = b.list.iter().filter(|e| !e.old_folder).collect();
        let this = cx.entity().downgrade();
        crate::manager::folder_chooser::open(
            crate::manager::folder_chooser::Opening {
                target: crate::manager::folder_chooser::Target::Remote(self.inst.clone()),
                on: self.inst.name.clone(),
                current: b.folder.clone(),
                here: (here.len(), here.iter().map(|e| e.bytes).sum()),
                keep: b.keep,
                full_bytes: b.list.first().map(|e| e.bytes),
                done: std::rc::Rc::new(move |dir, move_them, cx| {
                    let args = vec!["backup-folder".into(), dir.display().to_string(), if move_them { "move" } else { "leave" }.into()];
                    this.update(cx, |p, cx| p.run_quiet("folder", args, cx)).ok();
                }),
                title: None,
            },
            window,
            cx,
        );
    }

    fn backup_page(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        use crate::manager::backup as bk;
        let theme = cx.theme().clone();
        // A small save under way keeps the controls where they are, off.
        let busy = self.job.read(cx).is_running() || self.saving.is_some();
        let b = match &self.backups {
            Read::Loading => return self.skeleton(&theme),
            Read::Failed(e) => return Self::failed(&theme, e),
            Read::Got(b) => b.clone(),
        };
        // B1: the Backup card. A change is saved on the server at once.
        let on = b.daily_at.is_some();
        let time = b.daily_at.clone().unwrap_or_else(|| "02:00".into());
        let include: &'static str = match (b.preset, b.history) {
            (crate::backups::Preset::Configuration, _) => "configuration",
            (_, crate::backups::History::LastDays(_)) => "last90",
            _ => "everything",
        };
        let save = |at: String, keep: u32, include: &'static str| -> Vec<String> { vec!["backup-auto".into(), at, keep.to_string(), include.into()] };
        let keep = b.keep;
        let this = cx.entity().downgrade();

        let folder_row = bk::card_row(
            &theme,
            narrow,
            "Backup folder",
            None,
            bk::folder_value(&theme, &b.folder, b.free, self.saved_note(&theme, "folder", cx)),
            Some(bk::change_button("r-folder-change", busy).on_click(cx.listener(|this, _, window, cx| this.open_folder_chooser(window, cx))).into_any_element()),
            false,
        );
        let auto_row = bk::card_row(&theme, narrow, "Automatic backup", Some("A backup every day at this time, on the server's clock."), {
            let (t1, this1) = (time.clone(), this.clone());
            h_flex()
                .gap_3()
                .items_center()
                .child(gpui_kit::component::switch::Switch::new("r-auto-on").checked(on).disabled(busy).on_click(cx.listener(move |this, v: &bool, _, cx| {
                    let at = if *v { t1.clone() } else { "off".into() };
                    this.run_quiet("auto", save(at, keep, include), cx);
                })))
                .child(
                    crate::ui::button("r-auto-time")
                        .ghost()
                        .small()
                        .disabled(!on || busy)
                        .label(if on { format!("Every day at {time}") } else { "Off".to_string() })
                        .dropdown_menu(move |menu, _, _| {
                            let mut menu = menu;
                            for h in 0..24 {
                                for m in [0, 30] {
                                    let t = format!("{h:02}:{m:02}");
                                    let (this, t2) = (this1.clone(), t.clone());
                                    menu = menu.item(PopupMenuItem::new(t).on_click(move |_, _, cx| {
                                        this.update(cx, |p, cx| p.run_quiet("auto", save(t2.clone(), keep, include), cx)).ok();
                                    }));
                                }
                            }
                            menu
                        }),
                )
        }, None, false);
        let keep_row = match &self.keep_input {
            Some(input) => bk::card_row(&theme, narrow, "Keep", Some("Older backups are deleted, across the folder and any folder used before."), bk::keep_field(&theme, input, busy), None, false),
            None => bk::card_row(&theme, narrow, "Keep", None, div().text_sm().child(format!("{keep} backups")), None, false),
        };
        let what_row = bk::card_row(
            &theme,
            narrow,
            "What to back up",
            Some("Everything: settings and all telemetry.\nLast 90 days: settings and the last 90 days of telemetry.\nConfiguration only: devices, dashboards, users and settings, no telemetry."),
            {
                let at = if on { time.clone() } else { "off".into() };
                let options = [("everything", "Everything"), ("last90", "Last 90 days"), ("configuration", "Configuration only")];
                // Wide: the mockup's segments; narrow: the same as a menu.
                if narrow {
                    bk::choice_menu(
                        "r-inc-menu",
                        busy,
                        options
                            .iter()
                            .map(|&(key, label)| {
                                let (this, at) = (this.clone(), at.clone());
                                (label, include == key, std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
                                    this.update(cx, |p, cx| {
                                        if include != key && !p.job.read(cx).is_running() && p.saving.is_none() {
                                            p.run_quiet("auto", save(at.clone(), keep, key), cx);
                                        }
                                    })
                                    .ok();
                                }) as std::rc::Rc<dyn Fn(&mut Window, &mut App)>)
                            })
                            .collect(),
                    )
                    .into_any_element()
                } else {
                    let mut seg = ui::seg_track(&theme);
                    for (key, label) in options {
                        let at = at.clone();
                        seg = seg.child(ui::seg_item(&theme, SharedString::from(format!("r-inc-{key}")), label, include == key).on_click(cx.listener(move |this, _, _, cx| {
                            if include != key && !this.job.read(cx).is_running() && this.saving.is_none() {
                                this.run_quiet("auto", save(at.clone(), keep, key), cx);
                            }
                        })));
                    }
                    seg.into_any_element()
                }
            },
            None,
            true,
        );
        let card = ui::panel(&theme)
            .gap_0()
            .child(div().pb_1().child(self.section_title(&theme, "Backup", "auto", cx)))
            .child(folder_row)
            .child(auto_row)
            .child(keep_row)
            .child(what_row);

        // The Backups table.
        let mut list = ui::panel(&theme).gap_0().child(
            h_flex()
                .gap_3()
                .pb_2()
                .items_center()
                .child(div().flex_1().min_w_0().child(self.section_title(&theme, "Backups", "backups", cx)))
                .child(crate::ui::button("r-restore-from").outline().icon(crate::manager::restore::icon("history")).label("Restore from…").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.restore_from(cx))))
                .child(crate::ui::button("r-backup").outline().label("Back up now").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Backing up", vec!["backup-now".into()], cx)))),
        );
        if b.list.is_empty() {
            list = list.child(div().py_2().text_sm().text_color(theme.muted_foreground).child("No backups in this folder. Restore from… can still take a file from this computer or another instance."));
        } else {
            list = list.child(bk::table_header(&theme, narrow));
        }
        for (i, e) in b.list.iter().enumerate() {
            let damaged = e.check.as_ref().is_some_and(|c| !c.ok);
            let path = e.path.display().to_string();
            let (p1, p2, p3, name, name2) = (path.clone(), path.clone(), path, e.name.clone(), e.name.clone());
            let (t1, t2, t3) = (this.clone(), this.clone(), this.clone());
            list = list.child(bk::table_row(
                &theme,
                narrow,
                i,
                e,
                busy,
                bk::RowActions {
                    restore: (!damaged).then(|| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
                        std::rc::Rc::new(move |_, cx| {
                            t1.update(cx, |p, cx| {
                                p.restore_pending = Some(crate::manager::restore::Source { file: std::path::PathBuf::from(&p1), sub: name.clone(), note: None });
                                cx.notify();
                            })
                            .ok();
                        })
                    }),
                    verify: std::rc::Rc::new(move |_, cx| {
                        t2.update(cx, |p, cx| p.run_job("Verifying the backup", vec!["backup-verify".into(), p2.clone()], cx)).ok();
                    }),
                    delete: std::rc::Rc::new(move |window, cx| {
                        t3.update(cx, |p, cx| p.confirm_delete(p3.clone(), name2.clone(), window, cx)).ok();
                    }),
                },
            ));
        }
        let restores = match &self.restores {
            Read::Loading => None,
            Read::Got(l) => Some(Ok(l.clone())),
            Read::Failed(e) => Some(Err(remote::split_failure(e).0)),
        };
        let history = crate::manager::restore::history_card(&theme, &crate::manager::restore::Target::Server(self.inst.clone()), restores.as_ref(), narrow);
        v_flex().gap(px(ui::GAP)).child(list).child(history).child(card)
    }

    /// Restore from… (R4).
    fn restore_from(&mut self, cx: &mut Context<Self>) {
        let (folder, free) = match &self.backups {
            Read::Got(b) => (b.folder.clone(), b.free),
            _ => (crate::backups::default_folder(), None),
        };
        let this = cx.entity().downgrade();
        crate::manager::restore::RestoreFrom::open(
            crate::manager::restore::Target::Server(self.inst.clone()),
            folder,
            free,
            std::rc::Rc::new(move |src, cx| {
                this.update(cx, |p, cx| {
                    p.restore_pending = Some(src);
                    cx.notify();
                })
                .ok();
            }),
            cx,
        );
    }

    /// The Restore flow for `src`, in place of the Backup page.
    fn open_restore(&mut self, src: crate::manager::restore::Source, window: &mut Window, cx: &mut Context<Self>) {
        let flow = cx.new(|cx| crate::manager::restore::RestoreFlow::new(crate::manager::restore::Target::Server(self.inst.clone()), src, window, cx));
        self._subscriptions.push(cx.subscribe(&flow, |p: &mut Self, _, _: &crate::manager::restore::Leave, cx| {
            p.restore_flow = None;
            p.reload(cx);
            cx.notify();
        }));
        self.restore_flow = Some(flow);
        cx.notify();
    }

    // -- Settings, Migration, Repair, Gateway --------------------------------

    fn settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let values = match &self.settings {
            Read::Loading => return self.skeleton(&theme),
            Read::Failed(e) => return Self::failed(&theme, e),
            Read::Got(v) => v.clone(),
        };
        let groups = crate::catalog::groups();
        if self.inputs.is_empty() {
            for g in &groups {
                for st in &g.settings {
                    let v = values.get(&st.env).cloned().unwrap_or_default();
                    let ph = if st.default.is_empty() { "not set".to_string() } else { format!("default {}", st.default) };
                    let input = cx.new(|cx| InputState::new(window, cx).default_value(v).placeholder(ph));
                    self._subscriptions.push(cx.subscribe(&input, |_, _, e: &InputEvent, cx| {
                        if matches!(e, InputEvent::Change) {
                            cx.notify();
                        }
                    }));
                    self.inputs.push((st.env.clone(), input));
                }
            }
        }
        let busy = self.job.read(cx).is_running();
        let value_of = |env: &str, cx: &App| self.inputs.iter().find(|(e, _)| e == env).map(|(_, i)| i.read(cx).value().trim().to_string()).unwrap_or_default();
        let changed: Vec<String> = self.inputs.iter().filter(|(e, i)| values.get(e).cloned().unwrap_or_default() != i.read(cx).value().trim()).map(|(e, _)| e.clone()).collect();
        let mut col = v_flex().gap(px(ui::GAP));
        // Someone else's compose file: the notice, and Apply stays off (also
        // while the status, which says so, is still being read).
        let external = self.status.got().is_some_and(|s| s.external_compose);
        if external {
            col = col.child(foreign_compose_notice(&theme, "The settings cannot be changed now.", self.take_over_button("r-set-take-over", busy, cx)));
        }
        if !changed.is_empty() {
            let mut all = values.clone();
            for (e, _) in &self.inputs {
                let v = value_of(e, cx);
                if v.is_empty() {
                    all.remove(e);
                } else {
                    all.insert(e.clone(), v);
                }
            }
            let n = changed.len();
            col = col.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.primary)
                    .bg(theme.accent)
                    .child(div().flex_1().text_sm().child(format!("{n} change{} to apply on {}. SENTIENT restarts when a setting needs it.", if n == 1 { "" } else { "s" }, self.inst.name)))
                    .child(crate::ui::button("r-set-undo").ghost().label("Undo").on_click(cx.listener(|this, _, _, cx| {
                        this.inputs.clear();
                        cx.notify();
                    })))
                    .child(crate::ui::button("r-set-apply").primary().label("Apply").disabled(busy || external || self.status.got().is_none()).on_click(cx.listener(move |this, _, _, cx| this.apply_settings(all.clone(), cx)))),
            );
        }
        // The same tabs, rows and Advanced table as this computer's
        // Platform settings. Rows that need this computer (presets from its
        // memory, administrator accounts) are left out.
        use crate::manager::settings as local;
        let this = cx.entity().downgrade();
        let nav = local::tab_bar(&theme, "r-", &local::TAB_TITLES, self.settings_tab, std::rc::Rc::new(move |i, cx| {
            this.update(cx, |p, cx| {
                p.settings_tab = i;
                super::session::set_settings_tab(i);
                cx.notify();
            })
            .ok();
        }));
        let active = self.settings_tab;
        let all: Vec<crate::catalog::Setting> = groups.iter().flat_map(|g| g.settings.iter().cloned()).collect();
        let parts_of = |st: &crate::catalog::Setting, table: bool, cx: &mut Context<Self>| -> (AnyElement, Option<AnyElement>) {
            let Some((_, input)) = self.inputs.iter().find(|(e, _)| *e == st.env) else { return (div().into_any_element(), None) };
            let this = cx.entity().downgrade();
            let inp = input.clone();
            let set: crate::manager::setting_field::SetFn = std::rc::Rc::new(move |v, window, cx| {
                inp.update(cx, |s, cx| s.set_value(v.to_string(), window, cx));
                this.update(cx, |_, cx| cx.notify()).ok();
            });
            let this = cx.entity().downgrade();
            let (env, inp) = (st.env.clone(), input.clone());
            let check: crate::manager::setting_field::ActFn = std::rc::Rc::new(move |_, cx| {
                let path = inp.read(cx).value().trim().to_string();
                let env = env.clone();
                this.update(cx, |p, cx| p.check_file(env, path, cx)).ok();
            });
            let (f, button) = crate::manager::setting_field::field_parts(st, input, set, crate::manager::setting_field::FileButton::Check(check), table, cx);
            let checked = self.file_checks.get(&st.env).map(|r| match r {
                Read::Loading => ui::pill(&theme, Tone::Neutral, "Checking…"),
                Read::Got(true) => ui::pill(&theme, Tone::Good, "Found on the server"),
                Read::Got(false) => ui::pill(&theme, Tone::Bad, "Not on the server"),
                Read::Failed(_) => ui::pill(&theme, Tone::Warn, "Could not check"),
            });
            (h_flex().flex_1().min_w_0().gap_2().items_center().child(f).children(checked).into_any_element(), button)
        };
        let field_of = |st: &crate::catalog::Setting, cx: &mut Context<Self>| -> AnyElement {
            match parts_of(st, false, cx) {
                (f, Some(b)) => h_flex().flex_1().min_w_0().gap_2().items_center().child(f).child(b).into_any_element(),
                (f, None) => f,
            }
        };
        let help = |st: &crate::catalog::Setting| {
            let default = if st.default.is_empty() { "SENTIENT's own".to_string() } else { st.default.clone() };
            Some(format!("{}\n\nVariable: {}. Built-in value: {default}.", st.help, st.env))
        };
        if self.settings_tab < 4 {
            // Screens and the layout test: the Data locations card first,
            // where it shows without scrolling.
            if self.settings_tab == 1 && cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_MOVEDATA_FIRST").is_some() {
                let inst = self.inst.clone();
                let card = self.locations.get_or_insert_with(|| {
                    let windows = crate::remote::is_windows(&inst);
                    cx.new(|cx| super::movedata::LocationsCard::new(crate::movedata::Target::Remote(inst.clone()), inst.name.clone(), windows, cx))
                });
                col = col.child(card.clone());
            }
            let mut sec = local::section(&theme, local::TAB_TITLES[self.settings_tab], None);
            for key in local::TAB_KEYS[self.settings_tab] {
                let Some(st) = all.iter().find(|s| s.env == *key) else { continue };
                let field = field_of(st, cx);
                sec = sec.child(local::row(&theme, st.short_label(), help(st), changed.contains(&st.env), field));
            }
            col = col.child(sec);
            // Database: where each store keeps its data, each with its Move….
            if self.settings_tab == 1 && !(cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_MOVEDATA_FIRST").is_some()) {
                let inst = self.inst.clone();
                let card = self.locations.get_or_insert_with(|| {
                    let windows = crate::remote::is_windows(&inst);
                    cx.new(|cx| super::movedata::LocationsCard::new(crate::movedata::Target::Remote(inst.clone()), inst.name.clone(), windows, cx))
                });
                col = col.child(card.clone());
            }
        } else {
            let mut table = ui::panel(&theme).gap_0().overflow_hidden().child(local::table_header(&theme));
            for st in &all {
                let (field, action) = parts_of(st, true, cx);
                table = table.child(local::table_row(
                    &theme,
                    st.short_label(),
                    help(st),
                    changed.contains(&st.env),
                    local::table_cells(&theme, &st.env, field, action, local::builtin_label(st), None),
                ));
            }
            col = col.child(table);
        }
        // The same category list and heading as this computer's; on a narrow
        // window the list sits above the page.
        h_flex()
            .gap_4()
            .items_start()
            .when(ui::narrow(), |r| r.flex_col().items_stretch())
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(14.))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_size(px(17.)).font_weight(FontWeight::MEDIUM).child(local::TAB_TITLES[active.min(4)]))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(local::TAB_ABOUT[active.min(4)])),
                    )
                    .child(col),
            )
    }

    /// "Check" beside a file setting: does the server have that file?
    fn check_file(&mut self, env: String, path: String, cx: &mut Context<Self>) {
        if path.is_empty() {
            self.file_checks.remove(&env);
            cx.notify();
            return;
        }
        self.file_checks.insert(env.clone(), Read::Loading);
        let inst = self.inst.clone();
        self.fetch(cx, move || remote::file_exists(&inst, &path), move |p, r| {
            p.file_checks.insert(env, r);
        });
        cx.notify();
    }

    fn take_over_with_password(&mut self, mut args: Vec<String>, creds: crate::manager::takeover::Creds, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let inst = self.inst.clone();
        self.job_label = Some(format!("Taking over the compose file on {}", inst.name).into());
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    let mut files = Vec::new();
                    for (flag, pw) in [("--db-password-file", &creds.db), ("--ch-password-file", &creds.ch)] {
                        if let Some(pw) = pw {
                            let file = remote::put_secret_file(&inst, pw)?;
                            args.push(flag.into());
                            args.push(file.clone());
                            files.push(file);
                        }
                    }
                    let a: Vec<&str> = args.iter().map(String::as_str).collect();
                    let r = remote::start_job(&inst, &a).and_then(|id| remote::follow(&inst, &id, &sink));
                    for f in files {
                        remote::remove_file(&inst, &f);
                    }
                    r
                },
                cx,
            )
        });
        cx.notify();
    }

    fn apply_settings(&mut self, values: std::collections::HashMap<String, String>, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let inst = self.inst.clone();
        self.job_label = Some(format!("Applying settings on {}", inst.name).into());
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    let file = format!("{}/.spm-settings.json", remote::home(&inst)?);
                    remote::put_text(&inst, &file, &serde_json::to_string(&values).map_err(|e| e.to_string())?)?;
                    sink(sentient_installer_core::progress::Progress::Step { name: "Applying the settings".into() });
                    let id = remote::start_job(&inst, &["apply-settings", &file])?;
                    let r = remote::follow(&inst, &id, &sink);
                    remote::remove_file(&inst, &file);
                    r
                },
                cx,
            )
        });
        self.inputs.clear();
        cx.notify();
    }

    /// The same panel as this computer's Migration: the type now, the
    /// types to change to as cards, the steps, then Change and its confirmation.
    fn migration_page(&self, cx: &mut Context<Self>) -> Div {
        use sentient_installer_core::stack::Flavor;
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let current = self.status.got().map(|s| s.database.clone()).unwrap_or_default();
        let all = [Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse];
        let now = all.into_iter().find(|f| crate::installation::flavor_label(*f) == current);
        let targets: Vec<Flavor> = all.into_iter().filter(|f| Some(*f) != now).collect();
        let chosen = self.target.filter(|f| Some(*f) != now);
        let this = cx.entity().downgrade();
        // Why it cannot start now: a page notice above the card.
        let external = self.status.got().is_some_and(|s| s.external_compose);
        // A compose file the Platform Manager did not write is known before
        // any check: the notice shows and the change stays off until the
        // takeover is done.
        let notice = match &self.preflight {
            _ if external => Some(foreign_compose_notice(&theme, "The database type cannot be changed now.", self.take_over_button("r-mig-take-over", busy, cx))),
            Preflight::Failed(why) => Some(preflight_notice(&theme, why, None)),
            _ => None,
        };
        // Until the status says which type runs, nothing can be picked.
        let locked = busy || external || now.is_none();
        v_flex().gap(px(ui::GAP)).children(notice).child(ui::panel(&theme)
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Database type"))
            .child(crate::manager::migration::type_cards(&theme, "r-", now, chosen, &targets, locked, crate::remote::is_windows(&self.inst), std::rc::Rc::new(move |f, cx| {
                this.update(cx, |p, cx| {
                    if !p.job.read(cx).is_running() {
                        p.target = f;
                        p.migrate_confirm = false;
                        p.preflight = Preflight::Idle;
                        cx.notify();
                    }
                })
                .ok();
            })))
            .when(chosen.is_some(), |p| p.child(crate::manager::migration::steps_text(&theme, false)))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .when(!self.migrate_confirm, |r| {
                        r.child(
                            crate::ui::button("r-migrate")
                                .primary()
                                .label(chosen.map(|f| format!("Change to {}", crate::installation::flavor_label(f))).unwrap_or_else(|| "Change database".into()))
                                // Off while the preflight says the change cannot be made.
                                .disabled(busy || external || chosen.is_none() || now.is_none() || self.preflight != Preflight::Idle)
                                .loading(self.preflight == Preflight::Checking)
                                .on_click(cx.listener(|this, _, _, cx| this.check_migration(cx))),
                        )
                        .children(crate::manager::migration::change_hint(&theme, external, chosen.is_some()))
                    })
                    .when_some(chosen.filter(|_| self.migrate_confirm), |r, f| {
                        r.child(
                            crate::ui::button("r-migrate-confirm")
                                .danger()
                                .label("Stop SENTIENT and change now")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.migrate_confirm = false;
                                    this.run_job("Moving the database", vec!["migrate".into(), f.as_str().into()], cx);
                                })),
                        )
                        .child(crate::ui::button("r-migrate-no").ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.migrate_confirm = false;
                            cx.notify();
                        })))
                    }),
            )
            .child({
                let inst = self.inst.clone();
                crate::manager::migration::move_data_card(&theme, "r-", std::rc::Rc::new(move |window, cx| {
                    crate::manager::movedata::open(crate::movedata::Target::Remote(inst.clone()), inst.name.clone(), None, window, cx)
                }))
            }))
    }

    /// The preflight on the server, then the confirmation.
    fn check_migration(&mut self, cx: &mut Context<Self>) {
        self.preflight = Preflight::Checking;
        self.migrate_confirm = false;
        let inst = self.inst.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { remote::migrate_preflight(&inst) }).await;
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(()) => {
                        p.preflight = Preflight::Idle;
                        p.migrate_confirm = true;
                    }
                    Err(e) => p.preflight = Preflight::Failed(e),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Take the server's compose-file SENTIENT over: the plan from its agent
    /// for confirmation (crate::manager::takeover), then the job there.
    fn open_take_over(&mut self, cx: &mut Context<Self>) {
        // From Migration with a type chosen: the migration follows the takeover.
        if self.kind == Kind::Migration {
            use sentient_installer_core::stack::Flavor;
            let current = self.status.got().map(|s| s.database.clone()).unwrap_or_default();
            let all = [Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse];
            let now = all.into_iter().find(|f| crate::installation::flavor_label(*f) == current);
            self.pending_migrate = self.target.filter(|f| Some(*f) != now).map(|f| f.as_str().to_string());
        } else {
            self.pending_migrate = None;
        }
        let inst = self.inst.clone();
        let this = cx.entity().downgrade();
        crate::manager::takeover::open(
            false,
            // An agent too old for the plan is updated first (sudo's
            // password, when it asks, from the dialog), then asked again.
            std::sync::Arc::new({
                let inst = inst.clone();
                move |compose: Option<String>| remote::takeover_plan_updating(&inst, compose.as_deref())
            }),
            std::sync::Arc::new({
                let inst = inst.clone();
                move |ch: bool, pw: String| remote::check_takeover_password(&inst, ch, &pw)
            }),
            std::rc::Rc::new(move |compose: Option<String>, creds: crate::manager::takeover::Creds, cx: &mut App| {
                this.update(cx, |p, cx| {
                    let mut args = vec!["take-over-compose".to_string()];
                    if let Some(c) = compose {
                        args.push("--compose".into());
                        args.push(c);
                    }
                    p.preflight = Preflight::Idle;
                    if creds.db.is_none() && creds.ch.is_none() {
                        p.run_job("Taking over the compose file", args, cx);
                    } else {
                        // Passwords go in files only the account reads, which
                        // the agent removes when it read them.
                        p.take_over_with_password(args, creds, cx);
                    }
                })
                .ok();
            }),
            Some(self.inst.clone()),
            cx,
        );
    }

    fn take_over_button(&self, id: &'static str, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        crate::ui::button(id).outline().label("Take over the compose file…").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.open_take_over(cx))).into_any_element()
    }

    fn repair_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().is_some_and(|s| s.installed);
        let external = self.status.got().is_some_and(|s| s.external_compose);
        // The same panels as this computer's Repair. Uninstalling is done on
        // the server itself, so that panel is left out here.
        let take_over = external.then(|| {
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Take over the compose file"))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().text_sm().text_color(theme.muted_foreground).child("SENTIENT runs from a compose file the Platform Manager did not write."))
                        .child(ui::info("r-take-over-help".into(), "The Platform Manager does not change that file. Changing settings or the database type needs the Platform Manager's own compose file: the same containers on the same data.".into())),
                )
                .child(h_flex().child(self.take_over_button("r-take-over", busy, cx)))
        });
        v_flex().gap(px(ui::GAP)).children(take_over).child(ui::panel(&theme)
            .child(div().font_weight(FontWeight::MEDIUM).child("Repair"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "For a SENTIENT that does not start or a service that went missing: the services and their configuration are written again from this installation's record, and SENTIENT is started. Data and settings are kept.",
            ))
            .child(h_flex().child(crate::ui::button("r-repair").outline().label("Repair").disabled(busy || !installed).on_click(cx.listener(|this, _, _, cx| this.run_job("Repairing", vec!["repair".into()], cx))))))
    }

    fn gateway_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let g = match &self.gateway {
            Read::Loading => return self.skeleton(&theme),
            Read::Failed(e) => return Self::failed(&theme, e),
            Read::Got(g) => g.clone(),
        };
        let get = |k: &str| g.get(k).cloned().unwrap_or_default();
        let installed = get("installed") == "true";
        let running = get("running") == "true";
        let mut p = ui::panel(&theme).child(
            h_flex()
                .gap_3()
                .items_center()
                .child(ui::product_logo(ui::Product::Gateway, 24.))
                .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                .child(if !installed { ui::pill(&theme, Tone::Neutral, "Not installed") } else if running { ui::pill(&theme, Tone::Good, "Running") } else { ui::pill(&theme, Tone::Bad, "Stopped") }),
        );
        if !installed {
            // The pill says it; the how-to and what was looked for are behind an (i).
            return ui::panel(&theme).child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(ui::product_logo(ui::Product::Gateway, 24.))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                    .child(ui::pill(&theme, Tone::Neutral, "Not installed"))
                    .child(ui::info(
                        "gw-not-installed".into(),
                        format!(
                            "Install it with the Platform Manager on the server itself, so its access token stays on the server.\n\nLooked for: {}",
                            sentient_installer_core::gateway::CHECKS
                        )
                        .into(),
                    )),
            );
        }
        let kind = get("kind");
        let runs_as = match kind.as_str() {
            "docker" => format!("Docker container {}", get("container")),
            "service" => format!("Service {}", get("service")),
            "process" => format!("A process ({}), started outside a service", get("process")),
            "windows-service" => "Windows service".to_string(),
            _ => "Not known".to_string(),
        };
        // A row only when it has a value.
        let host = get("host");
        let version = get("version");
        let page = get("page");
        p = p
            .when(!version.is_empty(), |p| p.child(ui::fact(&theme, "Version", version.clone())))
            .when(!kind.is_empty(), |p| p.child(ui::fact(&theme, "Runs as", runs_as.clone())))
            .when(!host.is_empty(), |p| p.child(ui::fact(&theme, "Connects to", format!("{host}:{}", get("port")))))
            .when(!page.is_empty(), |p| {
                p.child(if page == "off" {
                    ui::fact_help(&theme, "Web page", "Off", "The gateway's own web page is off. In a container it is turned on with ST_GW_UI=on.")
                } else {
                    ui::fact(&theme, "Web page", page.clone())
                })
            });
        // A plain process has no service to start it again: no controls.
        let controllable = kind != "process";
        let updatable = (kind == "docker" && get("managed") == "true") || kind == "windows-service";
        let act = |id: &'static str, label: &'static str, action: &'static str| {
            crate::ui::button(id).outline().label(label).disabled(busy).on_click(cx.listener(move |this, _, _, cx| this.run_job("Gateway", vec!["gateway-control".into(), action.into()], cx)))
        };
        p.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .when(controllable, |r| {
                    r.when(!running, |r| r.child(act("r-gw-start", "Start", "start")))
                        .when(running, |r| r.child(act("r-gw-stop", "Stop", "stop")).child(act("r-gw-restart", "Restart", "restart")))
                })
                .child(crate::ui::button("r-gw-logs").outline().label("Logs").on_click(|_, _, cx| {
                    crate::logsource::request(crate::logsource::Source::Gateway);
                    cx.refresh_windows();
                }))
                .when(updatable, |r| {
                    r.child(crate::ui::button("r-gw-update").primary().label("Update to the newest").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Updating the gateway", vec!["gateway-update".into()], cx))))
                }),
        )
    }

    /// One calm panel for a server that is not set up or cannot be reached:
    /// what is the matter and the one thing that solves it.
    fn gate_panel(&self, e: String, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let not_set_up = e == remote::NOT_SET_UP;
        ui::panel(&theme)
            .child(div().font_weight(FontWeight::MEDIUM).child(if not_set_up { "Server agent not set up" } else { "The server does not answer" }))
            .child(div().text_sm().text_color(theme.muted_foreground).child(if not_set_up {
                "The Platform Manager works on a server through its server agent. Set up copies the matching agent over SSH and checks it; nothing else on the server changes.".to_string()
            } else {
                e.clone()
            }))
            .child(
                h_flex().gap_2().child(if not_set_up {
                    crate::ui::button("r-setup").primary().label("Set up").loading(busy).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.set_up(cx)))
                } else {
                    crate::ui::button("r-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| {
                        this.ready = Read::Loading;
                        this.reload(cx);
                    }))
                }),
            )
    }

    fn set_up(&mut self, cx: &mut Context<Self>) {
        if self.job.read(cx).is_running() {
            return;
        }
        let inst = self.inst.clone();
        self.job_label = Some(format!("Setting up {}", inst.name).into());
        let strip_label = format!("Setting up the server agent on {}", inst.name);
        self.job.update(cx, |job, cx| {
            job.set_label(strip_label);
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    sink(sentient_installer_core::progress::Progress::Done { message: format!("The Platform Manager is set up on {}.", inst.name) });
                    Ok(())
                },
                cx,
            )
        });
        cx.notify();
    }
}

impl Render for RemotePages {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (title, summary) = match self.kind {
            Kind::Status => ("Status", "SENTIENT on this server: its state, services and database."),
            Kind::Logs => ("Logs", "SENTIENT's log and its database's on this server, newest at the bottom."),
            Kind::Update => ("Update", "A newer SENTIENT for this server, from the update channel."),
            Kind::Backup => ("Backup and restore", "The backups on this server."),
            Kind::Settings => ("Platform settings", "SENTIENT's settings on this server."),
            Kind::Migration => ("Migration", ""),
            Kind::Repair => ("Repair and uninstall", "Write the services and their configuration again on this server."),
            Kind::Gateway => ("Gateway", "The Sentient Gateway on this server."),
        };
        // Set up earlier but nothing added: add a lone SENTIENT found there,
        // once (it changes nothing on the server; the agent asks when the
        // choice is real).
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_REMOTE_STATUS").is_some() {
            self.auto_add_tried = true; // canned screens: no server to add on
        }
        if !self.auto_add_tried && matches!(&self.ready, Read::Got(())) && matches!(&self.status, Read::Got(s) if !s.installed) && !self.job.read(cx).is_running() {
            self.auto_add_tried = true;
            self.run_job("Looking for SENTIENT on the server", vec!["add-if-one".into()], cx);
        }
        let gate: Option<Div> = match &self.ready {
            Read::Loading => Some(self.skeleton(&theme)),
            // While Set up runs or shows its result, only its panel.
            Read::Failed(_) if self.job_label.is_some() => Some(div()),
            Read::Failed(e) => Some(self.gate_panel(e.clone(), cx)),
            Read::Got(()) if self.kind != Kind::Status && self.kind != Kind::Gateway && matches!(&self.status, Read::Got(s) if !s.installed) => {
                Some(if self.job_label.is_some() { div() } else { self.not_added_panel(cx) })
            }
            Read::Got(()) => None,
        };
        if self.kind == Kind::Backup && gate.is_none() {
            if let Some(src) = self.restore_pending.take() {
                self.open_restore(src, window, cx);
            }
            if let Some(flow) = &self.restore_flow {
                return flow.clone().into_any_element();
            }
        }
        let body = if let Some(v) = self.job_view(cx) { v } else if let Some(g) = gate { g } else { match self.kind {
            Kind::Status => self.status_page(cx),
            Kind::Logs => self.logs_page(window, cx),
            Kind::Update => self.update_page(cx),
            Kind::Backup => {
                self.ensure_keep_input(window, cx);
                // Screens: a row's Restore or Delete confirmation open
                // (SPM_DEBUG_BACKUP_DIALOG=restore|delete), or the folder
                // chooser (=folder).
                #[cfg(debug_assertions)]
                {
                    static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if let (Ok(which), Read::Got(b)) = (std::env::var("SPM_DEBUG_BACKUP_DIALOG"), &self.backups) {
                        if let Some(e) = b.list.first().cloned() {
                            if !SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
                                let path = e.path.display().to_string();
                                match which.as_str() {
                                    "delete" => self.confirm_delete(path, e.name, window, cx),
                                    // The folder chooser (B2–B4).
                                    "folder" => self.open_folder_chooser(window, cx),
                                    _ => {}
                                }
                            }
                        }
                    }
                    // The Restore from… dialog (R4–R6, B8, B9) and a restore's details (R3).
                    static SHOWN_R: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if matches!(self.backups, Read::Got(_)) && !matches!(self.restores, Read::Loading) {
                        if std::env::var("SPM_DEBUG_RESTORE_FROM").is_ok() && !SHOWN_R.swap(true, std::sync::atomic::Ordering::SeqCst) {
                            self.restore_from(cx);
                        }
                        if std::env::var("SPM_DEBUG_RESTORE_DETAILS").is_ok() && !SHOWN_R.swap(true, std::sync::atomic::Ordering::SeqCst) {
                            if let Read::Got(h) = &self.restores {
                                if let Some(e) = h.first().cloned() {
                                    crate::manager::restore::HistoryDialog::open(crate::manager::restore::Target::Server(self.inst.clone()), e, cx);
                                }
                            }
                        }
                    }
                }
                self.backup_page(crate::manager::backup::narrow(window), cx)
            }
            Kind::Settings => self.settings_page(window, cx),
            Kind::Migration => self.migration_page(cx),
            Kind::Repair => self.repair_page(cx),
            Kind::Gateway => self.gateway_page(cx),
        } };
        // An agent too old for this page's jobs: one notice and Update agent,
        // in place of every panel that cannot load.
        let body = if self.agent_old() {
            let busy = self.job.read(cx).is_running();
            ui::notice_with(
                &theme,
                Tone::Warn,
                "The agent on this server is out of date.\nUpdate it to use this page. Nothing else on the server changes.",
                Some(crate::ui::button("agent-update").primary().label("Update agent").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.update_agent(cx))).into_any_element()),
            )
        } else {
            body
        };
        // Which server this is shows in the top bar (its colour, the
        // switcher with REMOTE); a job shows in the progress strip.
        let me = cx.entity().downgrade();
        let reload = crate::ui::refresh_button("remote-reload", self.in_flight > 0, window, move |_, cx| {
            me.update(cx, |this, cx| this.reload(cx)).ok();
        });
        ui::page_with(&theme, title, summary, reload).child(body).into_any_element()
    }
}

/// Screens: a canned offer with two versions of notes.
#[cfg(debug_assertions)]
fn canned_offer() -> crate::updates::Offer {
    let n = |version: &str, date: &str, summary: &str, sections: Vec<(&str, Vec<&str>)>| crate::notes::Notes {
        version: version.into(),
        date: date.into(),
        summary: summary.into(),
        sections: sections.into_iter().map(|(k, v)| (k.to_string(), v.into_iter().map(String::from).collect())).collect(),
        database: None,
    };
    crate::updates::Offer {
        version: "4.4.0.2".into(),
        tag: "4.4.0.2".into(),
        date: "2026-11-14".into(),
        notes: String::new(),
        page: String::new(),
        installer: None,
        sha256: None,
        between: vec![
            n("4.4.0.2", "2026-11-14", "Request a demo or buy a licence from inside SENTIENT, and editor fixes.", vec![
                ("Added", vec!["Licence: Request demo licence and Buy licence open dialogs inside SENTIENT."]),
                ("Fixed", vec!["The fullscreen buttons of the JSON editor no longer submit the form they sit in.", "The licence limits on telemetry and attribute keys count the customer's keys only."]),
            ]),
            n("4.4.0.1", "2026-11-13", "First release of the 4.4 line.", vec![("Added", vec!["CoAP and LwM2M devices (DTLS, OSCORE).", "18 new widgets."])]),
        ],
    }
}
