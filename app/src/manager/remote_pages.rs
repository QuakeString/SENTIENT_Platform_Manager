// Copyright © 2016-2026 The SENTIENT Authors

//! The pages of a remote instance (crate::remote): Status, Logs,
//! Update and Backup, read from the server's own Platform Manager over SSH
//! and worked through detached jobs there. One view per open instance; the
//! menu chooses which page it shows.

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
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

/// The menu asked for the open server's Gateway page (Status's gateway row,
/// "Install the gateway" on an empty server): crate::manager takes it.
static GO_GATEWAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn go_gateway(cx: &mut App) {
    GO_GATEWAY.store(true, std::sync::atomic::Ordering::SeqCst);
    cx.refresh_windows();
}

/// Whether a page asked for the Gateway page (once).
pub(crate) fn take_go_gateway() -> bool {
    GO_GATEWAY.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// The menu asked for the open server's Status page (Done after an install).
static GO_STATUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a page asked for the Status page (once).
pub(crate) fn take_go_status() -> bool {
    GO_STATUS.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Installing SENTIENT or the gateway on a Windows server from here is not
/// built yet (the owner's decision for 0.9.3): both installs say so.
pub(crate) const WINDOWS_REMOTE_INSTALL: &str = "Remote install is not supported on Windows servers yet.";

/// Why Install SENTIENT cannot start on this server (its tooltip), or None.
fn install_blocked(inst: &Instance, busy: bool) -> Option<&'static str> {
    if remote::is_windows(inst) {
        Some(WINDOWS_REMOTE_INSTALL)
    } else if busy {
        Some("Another job is running on this server.")
    } else {
        None
    }
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
                        .gap(crate::ui::button_gap())
                        .justify_end()
                        // An account without sudo: the agent goes in its home
                        // folder (crate::remote::choose_user_install).
                        .child(crate::ui::probed(crate::ui::button("sudo-none").label("This account has no sudo").on_click(cx.listener(|p, _, _, cx| {
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
                        }))))
                        .child(div().flex_1())
                        .child(crate::ui::probed(crate::ui::button("sudo-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx))))
                        .child(crate::ui::probed(crate::ui::button("sudo-save").primary().label("Save and retry").on_click(cx.listener(|p, _, _, cx| {
                            let pw = p.input.read(cx).value().to_string();
                            if pw.is_empty() {
                                return;
                            }
                            remote::set_secret(&remote::sudo_key(&p.inst.id), &pw);
                            crate::modal::close(cx);
                            (p.then)(cx);
                        })))),
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

/// add-if-one found one native SENTIENT on the server that needs the
/// PostgreSQL admin (postgres) password: ask for it, then add it with
/// `add-existing --native --pg-admin-password-stdin` (crate::remote::
/// add_existing_native). A wrong password shows the server's message and
/// the field stays for another try. The password lives only in this dialog
/// and the one call's standard input: never saved here, never logged.
/// `then` runs once it was added (the page reads its state again).
pub(crate) fn ask_pg_admin_password(inst: Instance, ask: crate::adopt::NeedsPgAdmin, then: std::rc::Rc<dyn Fn(&mut App)>, cx: &mut App) {
    cx.defer(move |cx| ask_pg_admin_password_now(inst, ask, then, cx));
}

fn ask_pg_admin_password_now(inst: Instance, ask: crate::adopt::NeedsPgAdmin, then: std::rc::Rc<dyn Fn(&mut App)>, cx: &mut App) {
    struct PgAdminPrompt {
        inst: Instance,
        ask: crate::adopt::NeedsPgAdmin,
        input: Entity<InputState>,
        then: std::rc::Rc<dyn Fn(&mut App)>,
        busy: bool,
        error: Option<String>,
        _enter: Subscription,
    }
    impl PgAdminPrompt {
        /// `password` None: the server sets a new one.
        fn add(&mut self, password: Option<String>, cx: &mut Context<Self>) {
            if self.busy || password.as_deref().is_some_and(str::is_empty) {
                return;
            }
            self.busy = true;
            self.error = None;
            cx.notify();
            let inst = self.inst.clone();
            cx.spawn(async move |this, cx| {
                let r = cx.background_executor().spawn(async move { remote::add_existing_native(&inst, password.as_deref()) }).await;
                let _ = this.update(cx, |p, cx| match r {
                    Ok(msg) => {
                        let then = p.then.clone();
                        let name = p.inst.name.clone();
                        crate::modal::close(cx);
                        cx.defer(move |cx| {
                            ui::show_info(format!("Added on {name}"), msg, cx);
                            then(cx);
                        });
                    }
                    Err(e) => {
                        p.busy = false;
                        p.error = Some(e);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }
    impl Render for PgAdminPrompt {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme().clone();
            // "Windows services (SentientServer)" reads "Windows services" here:
            // one parenthesis in the sentence, the details are in the (i).
            let kind = self.ask.kind.split(" (").next().unwrap_or("").trim();
            let on = if kind.is_empty() { self.inst.name.clone() } else { format!("{} ({kind})", self.inst.name) };
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_1()
                        .items_start()
                        .child(div().flex_1().min_w_0().text_sm().child(format!("SENTIENT on {on} can be added with its PostgreSQL admin (postgres) password.")))
                        .child(crate::ui::info(
                            "pg-admin-help".into(),
                            format!(
                                "{}The Platform Manager needs it for updates, backups and repairs. It goes to the server once, on standard input, and is kept only in that server's secret store: not on this computer, never in a log.\n\nNobody knows it? Set a new one gives postgres a new password through SENTIENT's own database role. Anything else that signs in to PostgreSQL as postgres then needs the new one.",
                                if self.ask.location.is_empty() { String::new() } else { format!("Found in {}.\n\n", self.ask.location) }
                            )
                            .into(),
                        )),
                )
                .child(Input::new(&self.input).mask_toggle().disabled(self.busy))
                .when_some(self.error.clone(), |c, e| c.child(div().min_w_0().text_sm().text_color(theme.danger).child(e)))
                .child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .justify_end()
                        .child(crate::ui::probed(crate::ui::button("pg-admin-new").outline().label("Set a new one").disabled(self.busy).on_click(cx.listener(|p, _, _, cx| p.add(None, cx)))))
                        .child(div().flex_1())
                        .child(crate::ui::probed(crate::ui::button("pg-admin-cancel").outline().label("Cancel").disabled(self.busy).on_click(|_, _, cx| crate::modal::close(cx))))
                        .child(crate::ui::probed(crate::ui::button("pg-admin-add").primary().label(if self.busy { "Adding" } else { "Add" }).disabled(self.busy).on_click(cx.listener(|p, _, _, cx| {
                            let pw = p.input.read(cx).value().to_string();
                            p.add(Some(pw), cx);
                        })))),
                )
        }
    }
    let Some(handle) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
    let _ = handle.update(cx, |_, window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("PostgreSQL admin password"));
        let view = cx.new(|cx| {
            // Enter adds, as the button does.
            let _enter = cx.subscribe(&input, |p: &mut PgAdminPrompt, input, e: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    let pw = input.read(cx).value().to_string();
                    p.add(Some(pw), cx);
                }
            });
            PgAdminPrompt { inst, ask, input, then, busy: false, error: None, _enter }
        });
        crate::modal::open(crate::modal::Modal { title: "PostgreSQL admin password needed".into(), body: view.into(), width: 520., icon: None, on_close: None }, cx);
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

/// An answer that is the server's state, not a failure to read it: it
/// replaces what was read before.
fn definite(e: &str) -> bool {
    e == remote::AGENT_OLD || e == remote::NOT_SET_UP || e == remote::NOT_ADDED || needs_sudo_password(e)
}

/// What a server's pages read, kept when the window closes while the
/// Platform Manager keeps running (the tray), so a window opened again
/// draws them at once, as they were, and reads them again behind them.
/// Memory only: a new start of the Platform Manager shows the loading
/// skeleton until the server answers (crate::manager::licence keeps its
/// readings the same way).
struct Kept {
    /// The connection the readings came over ([`conn_of`]): an instance
    /// edited since is read afresh.
    conn: String,
    ready: Read<()>,
    status: Read<RemoteStatus>,
    services: Read<Vec<RemoteService>>,
    db: Read<DbStatus>,
    backups: Read<RemoteBackups>,
    settings: Read<std::collections::HashMap<String, String>>,
    gateway: Read<std::collections::HashMap<String, String>>,
    gw_settings: Read<sentient_installer_core::gateway_settings::GwSettings>,
    offer: Read<Option<crate::updates::Offer>>,
    versions: Read<Vec<crate::online::Version>>,
    restores: Read<Vec<crate::restore_flow::Entry>>,
    updated_at: Option<String>,
    auto_add_tried: bool,
}

/// Each server's [`Kept`], by instance id, for this run.
static KEPT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Kept>>> = std::sync::LazyLock::new(Default::default);

/// The server an instance reaches: where, and as whom (what is found
/// there, `detected`, may change as it is read).
fn conn_of(inst: &Instance) -> String {
    serde_json::to_string(&(&inst.host, inst.port, &inst.user, &inst.auth, &inst.key_file)).unwrap_or_default()
}

/// What the pages of `inst` draw first: what this run read before (a
/// window opened again), or None on a cold start, the loading skeleton.
/// Taken once: the pages that take it own it from then on.
fn take_kept(kept: &mut std::collections::HashMap<String, Kept>, inst: &Instance) -> Option<Kept> {
    let k = kept.remove(&inst.id)?;
    (k.conn == conn_of(inst)).then_some(k)
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
    /// The Gateway page's settings card (crate::manager::gateway_settings)
    /// and what it shows (the agent's gateway-settings-json).
    gw_settings: Read<sentient_installer_core::gateway_settings::GwSettings>,
    gw_card: Entity<crate::manager::gateway_settings::GatewaySettingsCard>,
    /// "Install the gateway" on a server without one: the form (shared with
    /// this computer's page) and what is wrong in it.
    gw_form: crate::manager::gateway::InstallForm,
    gw_problem: Option<SharedString>,
    /// The last job's arguments: a removal or an install of the gateway
    /// that went through clears the install form.
    last_args: Vec<String>,
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
    /// The last reading's generation (fetch), and each slot's newest
    /// answer applied: an older answer never overwrites a newer one.
    gen: u64,
    applied: std::collections::HashMap<usize, u64>,
    /// Slots whose last refresh failed (the cached value still shows).
    stale: std::collections::HashMap<usize, String>,
    /// When a reading last came back (HH:MM).
    updated_at: Option<String>,
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
    /// Install SENTIENT (crate::manager::remote_install), while it is open:
    /// it stands in for every page and its rail for the menu.
    install: Option<Entity<crate::manager::remote_install::InstallFlow>>,
    /// An install found running on the server was shown again once.
    reattach_tried: bool,
    /// The newest SENTIENT on the update channel, for "Install SENTIENT".
    newest: Read<Option<String>>,
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
    /// What a sign-in from Update found about the licence (or that it was
    /// cancelled); without one, the agent's reading (RemoteStatus::
    /// licence_versions) labels the versions (crate::manager::licence_gate).
    licence_signed: Option<sentient_installer_core::licence_version::LicenceRead>,
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
    /// Platform settings: Enter in the fields (made with the inputs).
    settings_enter: Vec<Subscription>,
    /// All variables: the search box and the All / Set filter, as on the
    /// local page.
    vars_search: Entity<InputState>,
    vars_set_only: bool,
}

impl RemotePages {
    pub fn new(inst: Instance, kind: Kind, updates: Entity<crate::updates::UpdateState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let me = cx.entity().downgrade();
        let gw_card = cx.new(|_| {
            crate::manager::gateway_settings::GatewaySettingsCard::new(
                Some(inst.name.clone()),
                std::rc::Rc::new(move |w, _: &mut Window, cx: &mut App| {
                    me.update(cx, |p, cx| p.run_gw_work(w, cx)).ok();
                }),
            )
        });
        let vars_search = cx.new(|cx| InputState::new(_window, cx).placeholder("Search settings and variables"));
        let vars_sub = cx.subscribe(&vars_search, |_, _, _: &InputEvent, cx| cx.notify());
        let gw_form = crate::manager::gateway::InstallForm::new("", 1883, _window, cx);
        let gw_form_subs: Vec<Subscription> = gw_form.inputs().into_iter().map(|i| cx.subscribe(i, |_, _, _: &InputEvent, cx| cx.notify())).collect();
        // The gateway's "Update to <v>" follows the newest release found.
        let updates_sub = cx.observe(&updates, |_, _, cx| cx.notify());
        let subs = vec![
            cx.observe(&job, |this, job, cx| {
                // The gateway removed or installed there: nothing of the old
                // token or password stays in the install form.
                if crate::manager::gateway::clears_form(crate::manager::gateway::job_clears_form(&this.last_args), job.read(cx).state()) {
                    this.last_args.clear();
                    this.gw_form.clear();
                }
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
                            // add-if-one found a SENTIENT that needs the
                            // PostgreSQL admin password: ask for it here.
                            if let Some(ask) = remote::take_needs_pg_admin(&this.inst.id) {
                                let me = cx.entity().downgrade();
                                ask_pg_admin_password(this.inst.clone(), ask, std::rc::Rc::new(move |cx: &mut App| {
                                    me.update(cx, |p, cx| p.reload(cx)).ok();
                                }), cx);
                            }
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
            gw_settings: Read::Loading,
            gw_card,
            gw_form,
            gw_problem: None,
            last_args: Vec::new(),
            target: None,
            migrate_confirm: false,
            // Screens: a canned preflight failure.
            shown_job: None,
            pending_migrate: None,
            in_flight: 0,
            gen: 0,
            applied: Default::default(),
            stale: Default::default(),
            updated_at: None,
            saving: None,
            // Screens: "Saved" shown on a section (SPM_DEBUG_SAVED=folder).
            saved: std::env::var("SPM_DEBUG_SAVED").ok().filter(|_| cfg!(debug_assertions)).map(|s| (&*s.leak(), Ok(()), Vec::new())),
            preflight: std::env::var("SPM_DEBUG_PREFLIGHT").ok().filter(|_| cfg!(debug_assertions)).map(Preflight::Failed).unwrap_or(Preflight::Idle),
            offer: Read::Loading,
            versions: Read::Loading,
            pick: None,
            licence_signed: None,
            file_checks: Default::default(),
            log_view: None,
            svc_busy: None,
            settings_tab: super::session::load().settings_tab.filter(|i| *i < 5).unwrap_or(0),
            auto_add_tried: false,
            install: None,
            reattach_tried: false,
            newest: Read::Loading,
            agent_refresh: AgentRefresh::Idle,
            sudo_asked: false,
            updates,
            follow: true,
            job,
            job_label: None,
            show_details: false,
            scroll: ScrollHandle::new(),
            _tasks: Vec::new(),
            _subscriptions: {
                let mut s = subs;
                s.push(vars_sub);
                s.push(updates_sub);
                s.extend(gw_form_subs);
                s
            },
            keep_input: None,
            restore_flow: None,
            restore_pending: None,
            restores: Read::Loading,
            locations: None,
            settings_enter: Vec::new(),
            vars_search,
            vars_set_only: false,
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
        // Leaving Platform settings with changes not applied asks first.
        let (a, b, c, d) = (cx.entity().downgrade(), cx.entity().downgrade(), cx.entity().downgrade(), cx.entity().downgrade());
        crate::ui::set_unsaved(
            Some(p.inst.id.clone()),
            crate::ui::Unsaved {
                pending: std::rc::Rc::new(move |cx| a.upgrade().map(|p| p.read(cx).settings_pending(cx)).unwrap_or((0, 0))),
                list: std::rc::Rc::new(move |cx| d.upgrade().map(|p| p.read(cx).settings_change_lines(cx)).unwrap_or_default()),
                apply: std::rc::Rc::new(move |_, cx| {
                    b.update(cx, |p, cx| p.apply_pending_settings(cx)).ok();
                }),
                discard: std::rc::Rc::new(move |_, cx| {
                    c.update(cx, |p, cx| {
                        p.inputs.clear();
                        cx.notify();
                    })
                    .ok();
                }),
            },
        );
        // A window opened again: what this run read before shows at once
        // (the reload below reads it again behind it, as for an instance
        // opened again in the same window).
        if let Some(k) = take_kept(&mut KEPT.lock().unwrap_or_else(|e| e.into_inner()), &p.inst) {
            p.seed(k);
        }
        p.reload(cx);
        p.refresh_agent(cx);
        p
    }

    fn seed(&mut self, k: Kept) {
        self.ready = k.ready;
        self.status = k.status;
        self.services = k.services;
        self.db = k.db;
        self.backups = k.backups;
        self.settings = k.settings;
        self.gateway = k.gateway;
        self.gw_settings = k.gw_settings;
        self.offer = k.offer;
        self.versions = k.versions;
        self.restores = k.restores;
        self.updated_at = k.updated_at;
        self.auto_add_tried = k.auto_add_tried;
    }

    /// Platform settings: the values read, and the catalog setting of each input.
    fn settings_read(&self) -> Option<&std::collections::HashMap<String, String>> {
        match &self.settings {
            Read::Got(v) => Some(v),
            _ => None,
        }
    }

    /// Platform settings: the inputs whose value differs from the one read.
    fn settings_changed(&self, cx: &App) -> Vec<String> {
        let Some(values) = self.settings_read() else { return Vec::new() };
        self.inputs.iter().filter(|(e, i)| values.get(e).cloned().unwrap_or_default() != i.read(cx).value().trim()).map(|(e, _)| e.clone()).collect()
    }

    /// Platform settings: the inputs whose value cannot be used, in order.
    fn settings_invalid(&self, cx: &App) -> Vec<(crate::catalog::Setting, String)> {
        let all: Vec<crate::catalog::Setting> = crate::catalog::groups().into_iter().flat_map(|g| g.settings).collect();
        self.inputs
            .iter()
            .filter_map(|(e, i)| {
                let v = i.read(cx).value().to_string();
                let st = all.iter().find(|s| &s.env == e)?;
                st.problem(&v).map(|_| (st.clone(), v))
            })
            .collect()
    }

    /// (changes, fields not valid) on Platform settings.
    fn settings_pending(&self, cx: &App) -> (usize, usize) {
        (self.settings_changed(cx).len(), self.settings_invalid(cx).len())
    }

    /// The changes as the leave prompt lists them ("Name: old to new").
    fn settings_change_lines(&self, cx: &App) -> Vec<String> {
        let Some(values) = self.settings_read() else { return Vec::new() };
        let all: Vec<crate::catalog::Setting> = crate::catalog::groups().into_iter().flat_map(|g| g.settings).collect();
        self.settings_changed(cx)
            .iter()
            .filter_map(|e| {
                let st = all.iter().find(|s| &s.env == e)?;
                let new = self.inputs.iter().find(|(k, _)| k == e).map(|(_, i)| i.read(cx).value().trim().to_string()).unwrap_or_default();
                Some(crate::manager::settings::change_line(st, &values.get(e).cloned().unwrap_or_default(), &new))
            })
            .collect()
    }

    /// The values to write: those read, with the fields' over them.
    fn settings_to_apply(&self, cx: &App) -> Option<std::collections::HashMap<String, String>> {
        let mut all = self.settings_read()?.clone();
        for (e, i) in &self.inputs {
            let v = i.read(cx).value().trim().to_string();
            if v.is_empty() {
                all.remove(e);
            } else {
                all.insert(e.clone(), v);
            }
        }
        Some(all)
    }

    /// Apply the changes (the bar's Apply, Enter, leaving the page).
    fn apply_pending_settings(&mut self, cx: &mut Context<Self>) {
        let (changes, invalid) = self.settings_pending(cx);
        let external = self.status.got().is_none_or(|s| s.external_compose);
        if changes == 0 || invalid > 0 || external || self.job.read(cx).is_running() {
            return;
        }
        if let Some(all) = self.settings_to_apply(cx) {
            self.apply_settings(all, cx);
        }
    }

    /// The inputs the open settings tab shows, in order (Enter).
    /// All variables: a setting matches the search (label, variable, group,
    /// help) and, with Set, has a value on the server or a change here.
    fn var_shown(&self, group: &str, st: &crate::catalog::Setting, cx: &App) -> bool {
        if self.vars_set_only {
            let set = matches!(&self.settings, Read::Got(v) if v.get(&st.env).is_some_and(|x| !x.is_empty()));
            let here = self.inputs.iter().find(|(e, _)| e == &st.env).map(|(_, i)| i.read(cx).value().trim().to_string()).unwrap_or_default();
            let server = match &self.settings {
                Read::Got(v) => v.get(&st.env).cloned().unwrap_or_default(),
                _ => String::new(),
            };
            if !set && here == server.trim() {
                return false;
            }
        }
        let needle = self.vars_search.read(cx).value().trim().to_lowercase();
        needle.is_empty() || format!("{} {} {} {}", st.label, st.env, group, st.help).to_lowercase().contains(&needle)
    }

    fn settings_shown_inputs(&self, cx: &App) -> Vec<Entity<InputState>> {
        use crate::manager::settings as local;
        let all: Vec<crate::catalog::Setting> = crate::catalog::groups().into_iter().flat_map(|g| g.settings).collect();
        let keys: Vec<String> = if self.settings_tab < 4 {
            local::TAB_GROUPS[self.settings_tab].iter().flat_map(|(_, k)| k.iter().map(|s| s.to_string())).collect()
        } else {
            crate::catalog::groups().into_iter().flat_map(|g| g.settings.into_iter().filter(|s| self.var_shown(&g.name, s, cx)).map(|s| s.env).collect::<Vec<_>>()).collect()
        };
        keys.iter()
            .filter(|k| all.iter().find(|s| &s.env == *k).is_some_and(local::is_input))
            .filter_map(|k| self.inputs.iter().find(|(e, _)| e == k).map(|(_, i)| i.clone()))
            .collect()
    }

    /// Shown again after another instance was open: what it read before
    /// shows at once and is read again behind it.
    pub fn reshow(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.set_hidden(false, cx);
        self.reload(cx);
        cx.notify();
    }

    /// Reopen tests: what the page draws now: "skeleton" before anything
    /// was read, "data" with what it read, "state" for the server's answer
    /// that is not data (not set up, an error).
    pub fn drawn(&self) -> &'static str {
        if matches!(self.ready, Read::Loading) {
            "skeleton"
        } else if self.has_data() {
            "data"
        } else {
            "state"
        }
    }

    /// Switch timing (SPM_DEBUG_SWITCH): Status shows what it read.
    pub fn has_data(&self) -> bool {
        shows_data(self.kind, &self.ready, &self.status, &self.services, &self.db)
    }

    /// Another instance is open: the log stops following meanwhile.
    pub fn set_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        if let Some(v) = self.log_view.clone() {
            v.update(cx, |v, cx| v.set_paused(hidden, cx));
        }
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
            // jobview: short (one step, followed in the progress strip).
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
            self.fetch(cx, move || remote::status(&inst), |p| &mut p.status);
        } else {
            self.reload(cx);
        }
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        // Only what this reading fails to refresh is said to be stale.
        self.stale.clear();
        if let Some(l) = self.locations.clone() {
            l.update(cx, |l, cx| l.reload(cx));
        }
        let inst = self.inst.clone();
        self.fetch(cx, move || remote::ready(&inst), |p| &mut p.ready);
        // Every page needs to know whether a SENTIENT is added there.
        if !matches!(self.kind, Kind::Status | Kind::Update | Kind::Migration | Kind::Repair) {
            let inst = self.inst.clone();
            self.fetch(cx, move || remote::status(&inst), |p| &mut p.status);
        }
        let inst = self.inst.clone();
        match self.kind {
            Kind::Status => {
                // A gateway-only server's Status shows its gateway's row:
                // read only where SENTIENT is not (the last status says).
                let i4 = inst.clone();
                let sentient_there = matches!(remote::status_cache::view(&inst.id).status, Some(Ok(ref s)) if s.installed);
                if !sentient_there {
                    self.fetch(cx, move || remote::gateway(&i4), |p| &mut p.gateway);
                    let channel = crate::updates::Prefs::load().channel;
                    self.fetch(cx, move || crate::updates::newest_sentient(channel), |p| &mut p.newest);
                }
                self.fetch(cx, move || remote::status(&inst), |p| &mut p.status);
                let inst = self.inst.clone();
                self.fetch(cx, move || remote::services(&inst), |p| &mut p.services);
                let inst = self.inst.clone();
                self.fetch(cx, move || remote::dbstatus(&inst), |p| &mut p.db);
            }
            Kind::Logs => {}
            Kind::Update => {
                let i2 = inst.clone();
                self.fetch(cx, move || remote::status(&i2), |p| &mut p.status);
                // The server's gateway and the newest gateway release.
                let i3 = inst.clone();
                self.fetch(cx, move || remote::gateway(&i3), |p| &mut p.gateway);
                self.updates.update(cx, |u, cx| {
                    if u.found.is_none() {
                        u.check(cx);
                    }
                });
                let native = inst.detected.as_ref().is_some_and(|d| d.install != "Docker");
                self.pick = None;
                // Renders: a version picked from "Other version".
                #[cfg(debug_assertions)]
                if let Ok(v) = std::env::var("SPM_DEBUG_REMOTE_PICK") {
                    self.pick = Some(v);
                }
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
                    |p| &mut p.versions,
                );
                self.fetch(
                    cx,
                    move || {
                        #[cfg(debug_assertions)]
                        if std::env::var_os("SPM_DEBUG_REMOTE_OFFER").is_some() {
                            // Nothing newer for a server that runs it already.
                            let running = remote::status(&inst).map(|s| s.comparable_version()).unwrap_or_default();
                            let offer = canned_offer();
                            return Ok((sentient_installer_core::releases::movement(&offer.version, &running) == sentient_installer_core::releases::Move::Newer).then_some(offer));
                        }
                        let installed = remote::status(&inst)?.comparable_version();
                        let channel = crate::updates::Prefs::load().channel;
                        crate::updates::check(channel, Some(installed), native).sentient
                    },
                    |p| &mut p.offer,
                );
            }
            Kind::Backup => {
                let i2 = inst.clone();
                self.fetch(cx, move || remote::backups(&i2), |p| &mut p.backups);
                self.fetch(
                    cx,
                    move || {
                        #[cfg(debug_assertions)]
                        if std::env::var("SPM_DEBUG_RESTORE_HISTORY").is_ok_and(|v| v == "canned") {
                            return Ok(crate::manager::restore::canned::history());
                        }
                        remote::restore_history(&inst)
                    },
                    |p| &mut p.restores,
                );
            }
            Kind::Settings => {
                self.inputs.clear();
                self.fetch(cx, move || remote::settings(&inst), |p| &mut p.settings);
            }
            Kind::Migration => self.fetch(cx, move || remote::status(&inst), |p| &mut p.status),
            Kind::Repair => {
                let i2 = inst.clone();
                self.fetch(cx, move || remote::status(&inst), |p| &mut p.status);
                self.fetch(cx, move || remote::gateway(&i2), |p| &mut p.gateway);
            }
            Kind::Gateway => {
                // The newest gateway release, for "Update to <v>".
                self.updates.update(cx, |u, cx| {
                    if u.found.is_none() {
                        u.check(cx);
                    }
                });
                let (i2, i3) = (inst.clone(), inst.clone());
                self.gw_card.update(cx, |c, _| c.reset());
                self.fetch(cx, move || remote::gateway(&inst), |p| &mut p.gateway);
                self.fetch(cx, move || remote::gateway_settings(&i2), |p| &mut p.gw_settings);
                // SENTIENT's web port there, for the card's "Open in SENTIENT".
                self.fetch(cx, move || remote::status(&i3), |p| &mut p.status)
            }
        }
    }

    /// Read one of the page's values in the background (stale-while-
    /// revalidate): what was read before stays shown while it is read
    /// again; a reading that fails for want of an answer keeps it and says
    /// so softly (`stale`); an answer that IS the state (not set up, not
    /// added, agent too old, sudo) replaces it. Each reading carries this
    /// page's instance id and a generation: an older answer never overwrites
    /// a newer one.
    fn fetch<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
        slot: fn(&mut Self) -> &mut Read<T>,
    ) {
        // The slot's key: its place in the page (no pointer is followed).
        let key = (slot(self) as *mut Read<T> as usize).wrapping_sub(self as *mut Self as usize);
        self.gen += 1;
        let gen = self.gen;
        let id = self.inst.id.clone();
        // Click tests: refreshes fail while this file exists.
        #[cfg(debug_assertions)]
        let fail = std::env::var_os("SPM_DEBUG_REFRESH_FAIL").filter(|f| std::path::Path::new(f).exists()).is_some();
        #[cfg(not(debug_assertions))]
        let fail = false;
        let work = move || if fail { Err("ssh: connect to host: Connection timed out".to_string()) } else { work() };
        self.fetch_raw(cx, work, move |p, r| {
            if p.inst.id != id || p.applied.get(&key).is_some_and(|g| *g > gen) {
                return;
            }
            p.applied.insert(key, gen);
            match r {
                Read::Failed(e) if matches!(slot(p), Read::Got(_)) && !definite(&e) => {
                    p.stale.insert(key, e);
                }
                r => {
                    if matches!(r, Read::Got(_)) {
                        p.updated_at = Some(chrono::Local::now().format("%H:%M").to_string());
                    }
                    *slot(p) = r;
                    p.stale.remove(&key);
                }
            }
        });
    }

    /// The soft "Couldn't refresh" beside the title's Refresh: the last
    /// failure, while nothing is being read.
    fn stale_note(&self, theme: &gpui_kit::component::Theme, cx: &App) -> Option<AnyElement> {
        let log = if self.kind == Kind::Logs { self.log_view.as_ref().and_then(|v| v.read(cx).stale().map(String::from)) } else { None };
        let e = log.or_else(|| self.stale.values().next().cloned()).or_else(|| ui::debug_stale().filter(|_| self.has_data()))?;
        (self.in_flight == 0).then(|| ui::stale_note(theme, "remote-stale", &e, self.updated_at.as_deref()))
    }

    fn fetch_raw<T: Send + 'static>(
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
                        .gap(crate::ui::button_gap())
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
        self.last_args = args.clone();
        self.job_label = Some(format!("{label} on {}", inst.name).into());
        let strip_label = format!("{label} on {}", inst.name);
        let me = cx.entity().downgrade();
        let (l2, a2) = (label.clone(), args.clone());
        let on_page = self.shown_job == Some(self.kind);
        // Its steps from the first frame (crate::jobplan): reaching the
        // server, then what the agent will do (its own list replaces these
        // when it answers; an older agent's steps still fill them in).
        let docker = inst.detected.as_ref().is_none_or(|d| d.install.eq_ignore_ascii_case("docker"));
        let mut plan = crate::jobplan::guess(&args, docker, remote::is_windows(&inst));
        let generic = plan.len() == 1;
        if generic {
            plan.push(sentient_installer_core::progress::PlanStep::new(label.clone()).weight(3.));
        }
        self.job.update(cx, |job, cx| {
            job.set_plan(plan);
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
                    sink(sentient_installer_core::progress::Progress::Step { name: crate::jobplan::CONNECT.into() });
                    let updated = remote::deploy(&inst, &sink);
                    if let Err(e) = &updated {
                        sink(sentient_installer_core::progress::Progress::Log { line: format!("The agent could not be updated: {}", remote::split_failure(e).0) });
                    }
                    if generic {
                        sink(sentient_installer_core::progress::Progress::Step { name: label.to_string() });
                    }
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
            Kind::Gateway => "Gateway",
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
        let _ = migrating;
        Some(crate::manager::jobview::JobView::new("rjob", title, &self.job).actions(std::iter::once(back).chain(retry)).render(cx))
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
                .children(ui::card_actions(theme, control_buttons_loading(theme)))
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

    /// Install SENTIENT on this server (crate::manager::remote_install):
    /// its flow in place of the pages, its rail in place of the menu.
    /// `reattach`: the id of an install already running there, shown again.
    fn open_install(&mut self, reattach: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        let inst = self.inst.clone();
        let on_close: std::rc::Rc<dyn Fn(crate::manager::remote_install::Close, &mut Window, &mut App)> = std::rc::Rc::new(move |c, w, cx| {
            me.update(cx, |p, cx| p.close_install(c, w, cx)).ok();
        });
        let flow = cx.new(|cx| {
            let mut f = crate::manager::remote_install::InstallFlow::new(inst, on_close, window, cx);
            if let Some(id) = reattach {
                f.reattach(id, cx);
            }
            f
        });
        self.install = Some(flow);
        cx.refresh_windows();
        cx.notify();
    }

    fn close_install(&mut self, close: crate::manager::remote_install::Close, _window: &mut Window, cx: &mut Context<Self>) {
        use crate::manager::remote_install::Close;
        self.install = None;
        if let Close::Done(engine) = close {
            // What the server runs now, as Add server would detect it: the
            // instance's record is the one the install wrote there, and the
            // window opens its Status.
            let mut det = self.inst.detected.clone().unwrap_or_default();
            det.install = if engine == crate::installation::Engine::Docker { "Docker" } else { "systemd services" }.into();
            self.inst.detected = Some(det.clone());
            let mut list = remote::load();
            if let Some(i) = list.iter_mut().find(|i| i.id == self.inst.id) {
                i.detected = Some(det);
            }
            if let Err(e) = remote::save(&list) {
                crate::maintenance_ops::log_line(&format!("{}: the instance list was not saved: {e}", self.inst.name));
            }
            remote::LIST_CHANGED.store(true, std::sync::atomic::Ordering::SeqCst);
            GO_STATUS.store(true, std::sync::atomic::Ordering::SeqCst);
            self.kind = Kind::Status;
        }
        self.reload(cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// The flow open on this server, for the menu's place (crate::manager).
    pub(crate) fn install_flow(&self) -> Option<Entity<crate::manager::remote_install::InstallFlow>> {
        self.install.clone()
    }

    /// "Install SENTIENT": the flow, or off with why.
    fn install_sentient_button(&self, primary: bool, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.job.read(cx).is_running();
        let b = crate::ui::button("r-install-sentient").label("Install SENTIENT");
        let b = if primary { b.primary() } else { b.outline() };
        match install_blocked(&self.inst, busy) {
            Some(why) => div().id("r-install-sentient-why").child(b.disabled(true)).tooltip(crate::ui::tooltip(why)).into_any_element(),
            None => b.on_click(cx.listener(|this, _, window, cx| this.open_install(None, window, cx))).into_any_element(),
        }
    }

    /// No SENTIENT on the server yet. Not an error: what the agent found,
    /// and what can be installed (SENTIENT, the gateway) or added (a
    /// SENTIENT found there). Adding writes only the Platform Manager's
    /// own record.
    fn not_added_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let det = self.inst.detected.clone().unwrap_or_default();
        let found = !(det.install.is_empty() || det.install.eq_ignore_ascii_case("none"));
        // The facts the agent already read: the system, Docker (the
        // gateway's reading says), the disk.
        let docker = self.gateway.got().and_then(|g| g.get("docker").cloned());
        let disk = self.status.got().and_then(|s| s.disk_free);
        let system = [det.os_name.clone(), if det.arch.is_empty() { String::new() } else { format!("({})", det.arch) }].iter().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        let found_line = h_flex()
            .gap_4()
            .flex_wrap()
            .items_center()
            .text_sm()
            .text_color(theme.muted_foreground)
            .when(!system.is_empty(), |r| r.child(div().text_color(theme.foreground).child(system.clone())))
            .when_some(docker.filter(|d| d == "true" || d == "false"), |r, d| r.child(format!("Docker: {}", if d == "true" { "yes" } else { "no" })))
            .when_some(disk, |r, b| r.child(format!("{} free", crate::dbstatus::bytes_label(b))));
        let version = match &self.newest {
            Read::Got(Some(v)) => format!("{v}, {}", crate::updates::Prefs::load().channel.label()),
            Read::Loading => "Looking for the newest…".into(),
            _ => String::new(),
        };
        let gw_version = self.updates.read(cx).found.as_ref().and_then(|f| f.gateway.clone().ok()).flatten().map(|o| o.version).unwrap_or_default();
        let offer = |logo: ui::Product, name: &'static str, text: &'static str, button: AnyElement, version: String| {
            v_flex()
                .flex_1()
                .gap_2()
                .p(px(16.))
                .rounded(px(12.))
                .bg(theme.muted)
                .child(h_flex().gap_2().items_center().child(ui::product_logo(logo, 24.)).child(div().font_weight(FontWeight::MEDIUM).child(name)))
                .child(div().flex_1().text_sm().text_color(theme.muted_foreground).child(text))
                .child(h_flex().gap_3().items_center().flex_wrap().child(button).when(!version.is_empty(), |r| r.child(div().text_sm().text_color(theme.muted_foreground).child(version))))
        };
        let sentient = offer(
            ui::Product::Sentient,
            "SENTIENT",
            "The platform: devices, dashboards, rules and its database. Runs here in Docker or as services.",
            self.install_sentient_button(true, cx),
            version,
        );
        let gateway = (self.kind != Kind::Gateway).then(|| {
            offer(
                ui::Product::Gateway,
                "Sentient Gateway",
                "Reads the plant's devices next to this server and sends their data to a SENTIENT, here or elsewhere.",
                {
                    let b = crate::ui::button("r-go-gateway").outline().label("Install the gateway").disabled(busy || remote::is_windows(&self.inst)).on_click(|_, _, cx| go_gateway(cx));
                    if remote::is_windows(&self.inst) {
                        div().id("r-go-gateway-why").flex_shrink_0().child(b).tooltip(crate::ui::tooltip(WINDOWS_REMOTE_INSTALL)).into_any_element()
                    } else {
                        b.into_any_element()
                    }
                },
                gw_version,
            )
        });
        let offers = match gateway {
            Some(g) => ui::card_pair(sentient, g),
            None => v_flex().child(sentient),
        };
        // Something from SENTIENT was found there: Add existing.
        let existing = found.then(|| {
            h_flex()
                .gap_3()
                .items_center()
                .p(px(12.))
                .rounded(px(12.))
                .bg(theme.accent)
                .child(ui::product_logo(ui::Product::Sentient, 22.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(format!("SENTIENT runs on this server ({}).", det.install)))
                        .child(div().text_sm().text_color(theme.muted_foreground).child("Add existing lets the Platform Manager look after it. Nothing on it is changed.")),
                )
                .child(crate::ui::button("r-adopt").outline().label("Add existing").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Adding the existing SENTIENT", vec!["add-if-one".into()], cx))))
        });
        ui::panel(&theme)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .flex_wrap()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Nothing from SENTIENT is on this server yet"))
                    .when(found, |r| r.child(ui::pill(&theme, Tone::Neutral, "SENTIENT found, not added"))),
            )
            .child(found_line)
            .child(offers)
            .children(existing)
            .child(div().text_sm().text_color(theme.muted_foreground).child("Either, both, or one now and the other later. Each install has its own steps and can be removed on its own."))
    }

    /// A server with the gateway and no SENTIENT: the server itself, and
    /// one quiet row each for the gateway and for SENTIENT.
    fn gateway_only_status(&self, s: &RemoteStatus, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let det = self.inst.detected.clone().unwrap_or_default();
        let system = [det.os_name.clone(), if det.arch.is_empty() { String::new() } else { format!("({})", det.arch) }].iter().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        let server = ui::panel(&theme)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().font_weight(FontWeight::MEDIUM).child(self.inst.name.clone()))
                    .child(ui::pill(&theme, Tone::Good, "Agent connected")),
            )
            .child(ui::fact(&theme, "Address", format!("{} over SSH, as {}", self.inst.host, self.inst.user)))
            .when(!system.is_empty(), |p| p.child(ui::fact(&theme, "System", system.clone())))
            .when_some(s.disk_free, |p, free| p.child(ui::fact(&theme, "Disk", format!("{} free", crate::dbstatus::bytes_label(free)))))
            .when(!s.manager_version.is_empty(), |p| p.child(ui::fact(&theme, "Server agent", s.manager_version.clone())));
        let g = self.gateway.got().cloned().unwrap_or_default();
        let get = |k: &str| g.get(k).cloned().unwrap_or_default();
        let running = get("running") == "true";
        let host = get("host");
        let detail = match (get("version"), host.is_empty()) {
            (v, true) if v.is_empty() => String::new(),
            (v, true) => v,
            (v, false) if v.is_empty() => format!("sends to {host}:{}", get("port")),
            (v, false) => format!("{v}, sends to {host}:{}", get("port")),
        };
        let row = |logo: ui::Product, name: &'static str, pill: AnyElement, detail: String, action: AnyElement| {
            h_flex()
                .gap_3()
                .items_center()
                .min_h(px(44.))
                .child(ui::product_logo(logo, 22.))
                .child(div().w(px(150.)).flex_shrink_0().child(name))
                .child(pill)
                .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(detail))
                .child(action)
        };
        let gw_pill = if self.gateway.got().is_none() {
            ui::pill(&theme, Tone::Neutral, "Installed")
        } else if running {
            ui::pill(&theme, Tone::Good, "Running")
        } else {
            ui::pill(&theme, Tone::Bad, "Stopped")
        };
        let products = ui::panel(&theme)
            .gap_0()
            .child(row(ui::Product::Gateway, "Sentient Gateway", gw_pill.into_any_element(), detail, crate::ui::button("r-st-gateway").ghost().label("Open").on_click(|_, _, cx| go_gateway(cx)).into_any_element()))
            .child(div().h(px(1.)).bg(theme.border))
            .child(row(ui::Product::Sentient, "SENTIENT", ui::pill(&theme, Tone::Neutral, "Not installed").into_any_element(), String::new(), self.install_sentient_button(false, cx)));
        v_flex().gap(px(ui::GAP)).child(server).child(products)
    }

    fn status_page(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.job.read(cx).is_running();
        let theme = cx.theme().clone();
        let mut col = v_flex().gap(px(ui::GAP));
        let services = self.services_card(cx);
        match &self.status {
            Read::Loading => return self.skeleton(&theme),
            Read::Failed(e) => col = col.child(Self::failed(&theme, e)).child(services),
            Read::Got(s) if s.gateway_only() => return self.gateway_only_status(&s.clone(), cx),
            Read::Got(s) if !s.installed => {
                // Nothing added there yet: only the calm panel, no empty
                // Services or Database panels.
                return col.child(self.not_added_panel(cx));
            }
            Read::Got(s) => {
                let det = self.inst.detected.clone().unwrap_or_default();
                // The same pill as on this computer (status::overall_state);
                // "Starting" and the like only while our own action runs.
                let list: Vec<(&str, bool, bool)> = match &self.services {
                    Read::Got(list) => list.iter().map(|v| (v.name.as_str(), v.running, v.starting)).collect(),
                    _ => Vec::new(),
                };
                let label = self.job_label.as_ref().map(|l| l.to_string()).unwrap_or_default();
                let action = busy.then(|| if label.starts_with("Starting SENTIENT") { Some("start") } else if label.starts_with("Stopping SENTIENT") { Some("stop") } else if label.starts_with("Restarting SENTIENT") { Some("restart") } else { None }).flatten();
                let (state, tone) = crate::manager::status::overall_state(s.serving, &list, action);
                let any_up = s.serving || list.iter().any(|v| v.1);
                let url = format!("http://{}:{}", self.inst.host, s.http_port.unwrap_or(8080));
                let this = cx.entity().downgrade();
                let (t1, t2, t3) = (this.clone(), this.clone(), this);
                let inst = self.inst.clone();
                let controls = crate::manager::status::control_buttons(
                    &theme,
                    "r-",
                    busy,
                    Some(crate::manager::status::Running { serving: s.serving, any_up }),
                    std::rc::Rc::new(move |_, cx| {
                        t1.update(cx, |p, cx| p.run_job("Starting SENTIENT", vec!["control".into(), "start".into()], cx)).ok();
                    }),
                    std::rc::Rc::new(move |_, cx| {
                        t2.update(cx, |p, cx| {
                            p.run_job("Restarting SENTIENT", vec!["control".into(), "restart".into()], cx)
                        })
                        .ok();
                    }),
                    std::rc::Rc::new(move |_, cx| {
                        let t3 = t3.clone();
                        // The server's PostgreSQL may be shared (native Linux):
                        // the agent lists what else it holds.
                        let inst = inst.clone();
                        let probe: crate::manager::status::PgProbe = std::sync::Arc::new(move || crate::remote::pg_others(&inst));
                        crate::manager::status::confirm_stop(cx, Some(probe), std::rc::Rc::new(move |force, cx| {
                            let mut args: Vec<String> = vec!["control".into(), "stop".into()];
                            if force {
                                args.push("--stop-shared-pg".into());
                            }
                            t3.update(cx, |p, cx| p.run_job("Stopping SENTIENT", args, cx)).ok();
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
                        .children(ui::card_actions(&theme, controls));
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

    /// What is known of the server's licence now: a sign-in in this run,
    /// else what its agent read without one.
    fn licence_read(&self) -> sentient_installer_core::licence_version::LicenceRead {
        use sentient_installer_core::licence_version::LicenceRead;
        let key = crate::licence::Target::Remote(self.inst.clone()).key();
        let agent = self.status.got().and_then(|s| s.licence_versions.clone());
        match (&self.licence_signed, crate::manager::licence_gate::known(&key, agent.as_ref())) {
            (Some(r @ LicenceRead::Read(_)), _) => r.clone(),
            (_, k @ LicenceRead::Read(_)) => k,
            (Some(f), _) => f.clone(),
            _ => LicenceRead::NotRead,
        }
    }

    /// Update the server to `version` once the licence check allows it.
    /// `confirm`: the normal confirmation shows first (the pick bar is one
    /// already).
    fn gated_update(&mut self, version: String, build: String, confirm: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::manager::licence_gate;
        let this = cx.entity().downgrade();
        let go: std::rc::Rc<dyn Fn(&mut App)> = {
            let (this, v) = (this.clone(), version.clone());
            std::rc::Rc::new(move |cx: &mut App| {
                this.update(cx, |p, cx| {
                    p.pick = None;
                    p.run_job("Updating SENTIENT", vec!["update".into(), "--version".into(), v.clone()], cx);
                })
                .ok();
            })
        };
        let read = self.licence_read();
        let installed = self.status.got().map(|s| s.sentient_version.clone()).unwrap_or_default();
        let place = self.inst.name.clone();
        let normal: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = {
            let (go, read, place, v, b) = (go.clone(), read.clone(), place.clone(), version.clone(), build.clone());
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                if confirm {
                    licence_gate::confirm_update(&place, &v, &installed, &read, &b, go.clone(), window, cx);
                } else {
                    go(cx);
                }
            })
        };
        let ask = licence_gate::Ask {
            target: crate::licence::Target::Remote(self.inst.clone()),
            place,
            version,
            build,
            normal,
            anyway: std::rc::Rc::new(move |_: &mut Window, cx: &mut App| go(cx)),
            learned: std::rc::Rc::new(move |r, cx| {
                this.update(cx, |p, cx| {
                    p.licence_signed = Some(r);
                    cx.notify();
                })
                .ok();
            }),
        };
        licence_gate::guard(std::rc::Rc::new(ask), read, window, cx);
    }

    /// The same layout as this computer's Update: the channel, then one
    /// SENTIENT line with its buttons; the change log opens in a dialog.
    fn update_page(&self, cx: &mut Context<Self>) -> Div {
        use crate::manager::update::{Offered, product_row, sentient_summary};
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().map(|s| s.sentient_version.clone()).unwrap_or_else(|| "Reading…".into());
        // What updates compare with: the version the container runs (Docker
        // on "latest"), and whether its tag can be pinned to a release here.
        let running = self.status.got().map(|s| s.comparable_version()).unwrap_or_default();
        let can_pin = self.status.got().is_some_and(|s| s.floating_tag() && !s.external_compose && s.running_version.is_some());
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
        let licence = self.licence_read();
        // Nothing offered: what the licence covers, as a quiet line.
        let licence_note = offer.is_none().then(|| crate::manager::licence_gate::row_note(&licence));
        let mut row = crate::manager::update::product_row_noted(&theme, offer.is_some(), ui::Product::Sentient, "SENTIENT", sub, licence_note);
        if let Some((tone, text)) = pill {
            row = row.child(ui::pill(&theme, tone, text));
        }
        if let Some(o) = &offer {
            let version = o.version.clone();
            let build = o.licence_build();
            let (o2, inst_v) = (o.clone(), installed.clone());
            // Docker there, image not on Docker Hub yet; or a native install
            // whose glibc is older than the release needs: said instead of
            // the button.
            let det = self.inst.detected.clone().unwrap_or_default();
            let native = !det.install.eq_ignore_ascii_case("docker") && !remote::is_windows(&self.inst);
            let too_old = native
                .then(|| {
                    use sentient_installer_core::glibc;
                    glibc::decide(&o.version, glibc::min_glibc(o.min_glibc.as_deref(), &o.version), &det.libc, &det.os_name, glibc::Here::Server).blocked().map(str::to_string)
                })
                .flatten();
            row = match o.blocker().or(too_old) {
                Some(why) => row.child(div().text_sm().text_color(theme.muted_foreground).child(why)),
                None => row.child(crate::manager::licence_gate::pill(&theme, &licence, &build)).child(
                    crate::ui::button("r-update")
                        .primary()
                        .label(format!("Update to {}", o.version))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| this.gated_update(version.clone(), build.clone(), true, window, cx))),
                ),
            };
            row = row
                .child(crate::ui::button("r-notes").outline().label("What's new").on_click(move |_, _, cx| crate::manager::update::open_notes("SENTIENT", &inst_v, &o2, cx)));
        }
        // The running version stays listed only to pin its tag.
        let versions: Vec<(crate::online::Version, bool, &'static str)> = match &self.versions {
            Read::Got(v) => v
                .iter()
                .filter(|v| can_pin || v.version != running)
                .map(|v| {
                    let note = crate::manager::licence_gate::menu_note(&licence, &sentient_installer_core::licence_version::build_version(None, &v.version));
                    (v.clone(), v.version == running, note)
                })
                .collect(),
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
                    for (v, is_running, note) in &versions {
                        let (t, ver) = (this.clone(), v.version.clone());
                        let label = if *is_running { format!("{} (running)", v.label()) } else { format!("{}{note}", v.label()) };
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
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
        // The server's gateway: its update (the older Python one's too), run
        // there by the same gateway-update job.
        let gateway_row = match &self.gateway {
            Read::Got(g) => {
                let get = |k: &str| g.get(k).cloned().unwrap_or_default();
                let status = crate::manager::gateway::status_of(&get);
                let found = self.updates.read(cx).found.as_ref().map(|f| f.gateway.clone());
                let gw_offer = found.clone().and_then(|r| r.ok()).flatten();
                let update = crate::manager::gateway::gateway_update(&status, gw_offer.as_ref(), crate::remote::is_windows(&self.inst));
                let summary = match &update {
                    Some(u) => Some((u.about.clone(), None)),
                    None => crate::manager::gateway::gateway_summary(&status, found.as_ref().map(|r| r.is_ok())),
                };
                summary.map(|(sub, pill)| {
                    let mut r = product_row(&theme, update.as_ref().is_some_and(|u| u.why.is_none()), ui::Product::Gateway, "Sentient Gateway", sub);
                    if let Some((tone, text)) = pill {
                        r = r.child(ui::pill(&theme, tone, text));
                    }
                    if let Some(u) = update {
                        let this = cx.entity().downgrade();
                        let v = u.version.clone();
                        r = r.child(crate::manager::gateway::update_button(&theme, "r-gateway-update", &u, busy, std::rc::Rc::new(move |_, cx: &mut App| {
                            let v = v.clone();
                            this.update(cx, |p, cx| p.run_job("Updating the gateway", vec!["gateway-update".into(), v], cx)).ok();
                        })));
                    }
                    r
                })
            }
            _ => None,
        };
        let failed = match &self.offer {
            Read::Failed(e) => Some(e.clone()),
            _ => None,
        };
        let stale = self.updates.read(cx).stale_note();
        let pick = self.pick.clone();
        let pin_only = pick.as_deref() == Some(running.as_str());
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
                            .label(if checking { "Checking" } else { "Check for updates" })
                            .disabled(checking)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            // The offers are from an old release list (GitHub refused): said quietly.
            .when_some(stale, |p, n| p.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(n)))
            .when_some(failed, |p, e| {
                p.child(ui::outcome(
                    &theme,
                    Tone::Warn,
                    if e.contains("limiting") { "GitHub is limiting requests from this network. Try again in an hour." } else { "The release page could not be reached. Nothing was changed." },
                ))
            })
            .child(row)
            .children(gateway_row)
            .when_some(pick, |p, v| {
                let v2 = v.clone();
                p.child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .flex_wrap()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .child(div().flex_1().min_w(px(220.)).text_sm().child(if pin_only {
                            format!("Keep this server on SENTIENT {v}, the version it runs? Only the image tag changes: no update and no database copy.")
                        } else if can_pin {
                            format!("Install SENTIENT {v} on this server instead of {installed}? The compose file then names {v}.")
                        } else {
                            format!("Install SENTIENT {v} on this server instead of {installed}?")
                        }))
                        // Whether the licence covers it (not for the version running).
                        .when(!pin_only, |r| r.child(crate::manager::licence_gate::pill(&theme, &licence, &sentient_installer_core::licence_version::build_version(None, &v))))
                        .child(crate::ui::probed(crate::ui::button("r-pick-go").primary().label(if pin_only { format!("Pin to {v}") } else { format!("Install {v}") }).disabled(busy).on_click(cx.listener(move |this, _, window, cx| {
                            if pin_only {
                                this.pick = None;
                                this.run_job("Pinning SENTIENT's image tag", vec!["update".into(), "--version".into(), v2.clone()], cx);
                            } else {
                                let build = sentient_installer_core::licence_version::build_version(None, &v2);
                                this.gated_update(v2.clone(), build, false, window, cx);
                            }
                        }))))
                        .child(crate::ui::probed(crate::ui::button("r-pick-no").ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.pick = None;
                            cx.notify();
                        })))),
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
                .relative()
                .gap(crate::ui::button_gap())
                .pb_2()
                .items_center()
                .child(ui::layout_probe("header-actions"))
                .child(div().flex_1().min_w_0().child(self.section_title(&theme, "Backups", "backups", cx)))
                .child(crate::ui::probed(crate::ui::button("r-restore-from").outline().icon(crate::manager::restore::icon("history")).label("Restore from file").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.restore_from(cx)))))
                .child(crate::ui::probed(crate::ui::button("r-backup").outline().label("Back up now").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Backing up", vec!["backup-now".into()], cx))))),
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
                    // On the server: no folder to show here.
                    reveal: None,
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
        use crate::manager::settings as local;
        let theme = cx.theme().clone();
        ui::set_stacked(f32::from(window.viewport_size().width) < ui::ROW_STACK_W);
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
            // Enter: the next field shown; on the last one Apply, or the
            // first field that is not valid.
            let fields: Vec<Entity<InputState>> = self.inputs.iter().map(|(_, i)| i.clone()).collect();
            self.settings_enter = ui::enter_moves_on(&fields, window, cx, |p: &Self, cx| p.settings_shown_inputs(cx), |p: &mut Self, _, cx| {
                let bad = p.settings_invalid(cx);
                if let Some((st, _)) = bad.first() {
                    let shown = p.settings_shown_inputs(cx);
                    let first = shown.into_iter().find(|i| bad.iter().any(|(s, _)| p.inputs.iter().any(|(e, x)| e == &s.env && x == i)));
                    return first.or_else(|| p.inputs.iter().find(|(e, _)| e == &st.env).map(|(_, i)| i.clone()));
                }
                p.apply_pending_settings(cx);
                None
            });
            // Screens and the layout test: some settings changed and one
            // field not valid, so the bar shows (SPM_DEBUG_SETTINGS_PENDING=
            // short, long on All variables, valid without the bad field,
            // leave with the leave prompt open).
            #[cfg(debug_assertions)]
            if let Ok(which) = std::env::var("SPM_DEBUG_SETTINGS_PENDING") {
                if which == "long" {
                    self.settings_tab = 4;
                }
                let mut set = vec![("RUST_LOG", "debug"), ("LICENSE_SERVER_URL", "https://licence.example.com/a/rather/long/address")];
                if which != "valid" && which != "leave" {
                    set.push(("LICENSE_BINDING_CHECK_INTERVAL_SECS", "jj"));
                }
                for (k, v) in set {
                    if let Some((_, i)) = self.inputs.iter().find(|(e, _)| e == k) {
                        i.update(cx, |s, cx| s.set_value(v.to_string(), window, cx));
                    }
                }
                if which == "leave" {
                    let id = self.inst.id.clone();
                    window.defer(cx, move |window, cx| ui::debug_leave_prompt(Some(id), window, cx));
                }
            }
        }
        let busy = self.job.read(cx).is_running();
        let changed = self.settings_changed(cx);
        let invalid = self.settings_invalid(cx);
        let mut notices = v_flex().gap(px(ui::GAP));
        // Someone else's compose file: the notice, and Apply stays off (also
        // while the status, which says so, is still being read).
        let external = self.status.got().is_some_and(|s| s.external_compose);
        if external {
            notices = notices.child(foreign_compose_notice(&theme, "The settings cannot be changed now.", self.take_over_button("r-set-take-over", busy, cx)));
        }
        // The tabs' counts: changes on each, red where a field is not valid.
        let mut badges = vec![(0usize, false); 5];
        for e in &changed {
            badges[local::tab_index_of(e)].0 += 1;
        }
        for (st, _) in &invalid {
            badges[local::tab_index_of(&st.env)].1 = true;
        }
        let this = cx.entity().downgrade();
        let tabs = ui::card_tabs(&theme, "r-", &local::TAB_TITLES, self.settings_tab, &badges, std::rc::Rc::new(move |i, cx| {
            this.update(cx, |p, cx| {
                p.settings_tab = i;
                super::session::set_settings_tab(i);
                cx.notify();
            })
            .ok();
        }));
        let all: Vec<crate::catalog::Setting> = groups.iter().flat_map(|g| g.settings.iter().cloned()).collect();
        let help = |st: &crate::catalog::Setting| {
            let default = if st.default.is_empty() { "SENTIENT's own".to_string() } else { st.default.clone() };
            Some(format!("{}\n\nVariable: {}. Built-in value: {default}.", st.help, st.env))
        };
        let row_of = |st: &crate::catalog::Setting, desc: Option<String>, cx: &mut Context<Self>| -> Option<ui::SettingRow> {
            let (_, input) = self.inputs.iter().find(|(e, _)| *e == st.env)?;
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
            let control = crate::manager::setting_field::control(st, input, set, crate::manager::setting_field::FileButton::Check(check), window, cx);
            // What Check found, on the line under the name.
            let checked = self.file_checks.get(&st.env).map(|r| match r {
                Read::Loading => "Checking…".to_string(),
                Read::Got(true) => "Found on the server".to_string(),
                Read::Got(false) => "Not on the server".to_string(),
                Read::Failed(_) => "Could not check".to_string(),
            });
            let is_changed = changed.contains(&st.env);
            Some(ui::SettingRow {
                label: st.short_label().into(),
                help: help(st),
                desc: checked.or(desc),
                restart: is_changed,
                changed: is_changed,
                control,
                wide: false,
                current: Some(crate::manager::setting_field::current_words(st, &values.get(&st.env).cloned().unwrap_or_default())),
                first: false,
            })
        };
        let mut body = v_flex().pb(px(12.));
        if self.settings_tab < 4 {
            // Screens and the layout test: the Data locations card first,
            // where it shows without scrolling.
            let first = cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_MOVEDATA_FIRST").is_some();
            let inst = self.inst.clone();
            let card = (self.settings_tab == 1).then(|| {
                self.locations
                    .get_or_insert_with(|| {
                        let windows = crate::remote::is_windows(&inst);
                        cx.new(|cx| super::movedata::LocationsCard::new(crate::movedata::Target::Remote(inst.clone()), inst.name.clone(), windows, cx))
                    })
                    .clone()
            });
            if let Some(c) = card.clone().filter(|_| first) {
                body = body.child(ui::group(&theme, "Where the data is", vec![], vec![c.into_any_element()], cx));
            }
            for (title, keys) in local::TAB_GROUPS[self.settings_tab] {
                let rows: Vec<ui::SettingRow> = keys.iter().filter_map(|k| all.iter().find(|s| s.env == *k)).filter_map(|st| row_of(st, None, cx)).collect();
                if !rows.is_empty() {
                    body = body.child(ui::group(&theme, title, rows, vec![], cx));
                }
            }
            // Database: where each store keeps its data, each with its Move….
            if let Some(c) = card.filter(|_| !first) {
                body = body.child(ui::group(&theme, "Where the data is", vec![], vec![c.into_any_element()], cx));
            }
        } else {
            // The same search box and All / Set filter as the local page.
            let total: usize = groups.iter().map(|g| g.settings.len()).sum();
            let set_n = match &self.settings {
                Read::Got(v) => groups.iter().flat_map(|g| g.settings.iter()).filter(|s| v.get(&s.env).is_some_and(|x| !x.is_empty())).count(),
                _ => 0,
            };
            let filter = |id: &'static str, text: String, on: bool, only: bool| {
                let b = crate::ui::button(id).label(text).on_click(cx.listener(move |this, _, _, cx| {
                    this.vars_set_only = only;
                    cx.notify();
                }));
                if on { b.primary() } else { b.outline() }
            };
            body = body.child(
                h_flex()
                    .flex_wrap()
                    .gap(crate::ui::button_gap())
                    .items_center()
                    .px(px(18.))
                    .pt(px(14.))
                    .child(div().flex_1().min_w(px(200.)).child(Input::new(&self.vars_search).cleanable(true)))
                    .child(crate::ui::probed(filter("r-filter-all", format!("All {total}"), !self.vars_set_only, false)))
                    .child(crate::ui::probed(filter("r-filter-set", format!("Set {set_n}"), self.vars_set_only, true))),
            );
            let mut any = false;
            for g in &groups {
                let shown: Vec<&crate::catalog::Setting> = g.settings.iter().filter(|st| self.var_shown(&g.name, st, cx)).collect();
                let rows: Vec<ui::SettingRow> = shown.into_iter().filter_map(|st| row_of(st, Some(st.env.clone()), cx)).collect();
                if !rows.is_empty() {
                    any = true;
                    body = body.child(ui::group(&theme, &g.name, rows, vec![], cx));
                }
            }
            if !any {
                body = body.child(div().px(px(18.)).py(px(14.)).text_sm().text_color(theme.muted_foreground).child("No setting matches."));
            }
        }
        let (changes, n_invalid) = (changed.len(), invalid.len());
        let names: Vec<String> = changed.iter().filter_map(|e| all.iter().find(|s| &s.env == e)).map(|s| s.short_label()).collect();
        let first_bad = invalid.first().map(|(st, v)| local::first_invalid_of(st, v));
        let line = ui::pending_line(changes, &names, changes > 0, first_bad.as_ref().map(|(l, n)| (l.as_str(), *n)));
        let bar = ui::pending_bar(
            &theme,
            "r-settings-pending",
            changes,
            n_invalid,
            line,
            changes > 0,
            !busy && !external && self.status.got().is_some(),
            cx.listener(|this, _, _, cx| {
                this.inputs.clear();
                cx.notify();
            }),
            cx.listener(|this, _, _, cx| this.apply_pending_settings(cx)),
        );
        v_flex()
            .flex_1()
            .min_h_0()
            .gap(px(ui::GAP))
            // The notice only when there is one (an empty one would add a gap).
            .when(external, |c| c.child(notices))
            .child(ui::settings_card(&theme, "r-settings-body", tabs, body, bar))
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
        self.fetch_raw(cx, move || remote::file_exists(&inst, &path), move |p, r| {
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
            job.set_plan(std::iter::once(crate::jobplan::connect()).chain(crate::jobplan::takeover()).collect());
            job.start(
                move |sink| {
                    sink(sentient_installer_core::progress::Progress::Step { name: crate::jobplan::CONNECT.into() });
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
            job.set_plan(vec![crate::jobplan::connect(), sentient_installer_core::progress::PlanStep::new("Applying the settings").detail("SENTIENT restarts where a setting needs it").weight(3.)]);
            job.start(
                move |sink| {
                    sink(sentient_installer_core::progress::Progress::Step { name: crate::jobplan::CONNECT.into() });
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
        crate::ui::button(id).outline().label("Take over the compose file").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.open_take_over(cx))).into_any_element()
    }

    fn repair_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let installed = self.status.got().is_some_and(|s| s.installed);
        let external = self.status.got().is_some_and(|s| s.external_compose);
        // The same panels as this computer's Repair.
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
        // The Gateway choice, when the server has one.
        let gw = self.gateway.got().filter(|g| g.get("installed").is_some_and(|v| v == "true")).map(|g| {
            let get = |k: &str| g.get(k).cloned().unwrap_or_default();
            let target = crate::manager::gateway::RemoveTarget::of_remote(&get, self.inst.name.clone());
            let line = crate::manager::gateway::runs_as(&get("kind"), &target.name, get("managed") == "true", get("recorded") == "true");
            let button = crate::ui::button("r-repair-gw-remove")
                .danger()
                .label("Remove the Gateway")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.open_gateway_remove(target.clone(), cx)))
                .into_any_element();
            crate::manager::gateway::remove_panel(&theme, "r-repair-gw", line, button)
        });
        v_flex().gap(px(ui::GAP)).children(take_over).child(ui::panel(&theme)
            .child(div().font_weight(FontWeight::MEDIUM).child("Repair"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "For a SENTIENT that does not start or a service that went missing: the services and their configuration are written again from this installation's record, and SENTIENT is started. Data and settings are kept.",
            ))
            .child(h_flex().child(crate::ui::button("r-repair").outline().label("Repair").disabled(busy || !installed).on_click(cx.listener(|this, _, _, cx| this.run_job("Repairing", vec!["repair".into()], cx)))))).children(gw)
            .when(installed, |c| {
                c.child(
                    ui::panel(&theme)
                        .child(div().font_weight(FontWeight::MEDIUM).child("Uninstall"))
                        .child(div().text_sm().text_color(theme.muted_foreground).child("Choose what to remove from this server, the same way as on this computer."))
                        .child(ui::button_row(vec![crate::ui::button("r-uninstall").danger().label("Uninstall").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.open_uninstall(cx))).into_any_element()])),
                )
            })
    }

    /// The Uninstall dialog for this server: its facts and the job through
    /// its agent (crate::manager::uninstall_dialog).
    fn open_uninstall(&mut self, cx: &mut Context<Self>) {
        let inst = self.inst.clone();
        let (i1, i2, id) = (inst.clone(), inst.clone(), inst.id.clone());
        crate::manager::uninstall_dialog::open(
            crate::manager::uninstall_dialog::Opts {
                name: inst.name.clone(),
                load: std::sync::Arc::new(move || remote::uninstall_facts(&i1)),
                run: std::sync::Arc::new(move |args: Vec<String>, sink| {
                    let a: Vec<&str> = args.iter().map(String::as_str).collect();
                    remote::start_job(&i2, &a).and_then(|job| remote::follow(&i2, &job, &sink))
                }),
                remote: true,
                on_done: std::rc::Rc::new(|cx: &mut App| cx.refresh_windows()),
                on_close: std::rc::Rc::new(move |after, cx: &mut App| {
                    if after.forget_instance {
                        remote::forget_instance(&id);
                        cx.refresh_windows();
                    }
                }),
            },
            cx,
        );
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
        // The older Python gateway alone: offered as an update (the same
        // gateway-update job); beside the Rust one: one line.
        if !installed && !get("legacy").is_empty() {
            let status = crate::manager::gateway::status_of(&get);
            let found = self.updates.read(cx).found.as_ref().map(|f| f.gateway.clone());
            let offer = found.clone().and_then(|r| r.ok()).flatten();
            let looking = match &found {
                Some(Err(e)) => Some(format!("Gateway updates: {e}")),
                Some(Ok(None)) => Some("No gateway release on this channel.".to_string()),
                _ => None,
            };
            let why = offer.as_ref().and_then(|o| sentient_installer_core::gateway::offer_blocker(&status, o, crate::remote::is_windows(&self.inst)));
            let this = cx.entity().downgrade();
            let v = offer.as_ref().map(|o| o.version.clone()).unwrap_or_default();
            return crate::manager::gateway::legacy_panel(
                &theme,
                "r-gw-older",
                &get("legacy"),
                Some(get("legacy_image")).filter(|i| !i.is_empty()).as_deref(),
                offer.as_ref(),
                looking,
                why,
                busy,
                std::rc::Rc::new(move |_, cx: &mut App| {
                    let v = v.clone();
                    this.update(cx, |p, cx| p.run_job("Updating the gateway", vec!["gateway-update".into(), v], cx)).ok();
                }),
            );
        }
        let legacy = Some(get("legacy")).filter(|n| !n.is_empty()).map(|n| div().text_sm().text_color(theme.muted_foreground).child(crate::manager::gateway::legacy_line(&n, "this server")));
        let mut p = ui::panel(&theme).child(
            h_flex()
                .gap_3()
                // A narrow window: the pills go to their own line.
                .flex_wrap()
                .items_center()
                .child(ui::product_logo(ui::Product::Gateway, 24.))
                .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                .child(if !installed { ui::pill(&theme, Tone::Neutral, "Not installed") } else if running { ui::pill(&theme, Tone::Good, "Gateway is running") } else { ui::pill(&theme, Tone::Bad, "Gateway is stopped") })
                .children({
                    let s = match &self.gw_settings {
                        Read::Got(s) => Some(Ok(s.clone())),
                        _ => None,
                    };
                    crate::manager::gateway_settings::connected_pill(&theme, s.as_ref(), running && installed)
                }),
        );
        if !installed {
            return self.gateway_install_panel(&get, busy, cx);
        }
        let kind = get("kind");
        let recorded = get("recorded") == "true";
        let managed = get("managed") == "true";
        let target = crate::manager::gateway::RemoveTarget::of_remote(&get, self.inst.name.clone());
        let runs_as = if kind.is_empty() { "Not known".to_string() } else { crate::manager::gateway::runs_as(&kind, &target.name, managed, recorded) };
        let outside = !managed && !recorded && kind != "windows-service" && !kind.is_empty();
        let compose = get("compose");
        let volumes = get("volumes");
        // A row only when it has a value.
        let host = get("host");
        let version = get("version");
        let page = get("page");
        p = p
            .children(legacy)
            .when(!version.is_empty(), |p| p.child(ui::fact(&theme, "Version", version.clone())))
            .when(!kind.is_empty(), |p| p.child(ui::fact(&theme, "Runs as", runs_as.clone())))
            .when(!host.is_empty(), |p| p.child(ui::fact(&theme, "Connects to", format!("{host}:{}", get("port")))))
            .when(!compose.is_empty(), |p| {
                let proj = get("project");
                p.child(ui::fact(&theme, "Compose", if proj.is_empty() { compose.clone() } else { format!("{proj}, in {compose}") }))
            })
            .when(!volumes.is_empty(), |p| p.child(ui::fact(&theme, "Volumes", volumes.replace(',', ", "))))
            .when(!page.is_empty(), |p| {
                p.child(if page == "off" {
                    ui::fact_help(&theme, "Web page", "Off", crate::manager::gateway::page_off_help(&kind))
                } else {
                    ui::fact(&theme, "Web page", page.clone())
                })
            });
        // A plain process has no service to start it again: no controls.
        let controllable = kind != "process";
        // "Update to <v>": only with a newer release than the server's
        // gateway (this computer's channel), and only where an update can
        // run there; else its reason on the disabled button.
        let status = crate::manager::gateway::status_of(&get);
        let offer = match &self.updates.read(cx).found {
            Some(f) => f.gateway.as_ref().ok().cloned().flatten(),
            None => None,
        };
        let newer = offer.filter(|o| !version.is_empty() && sentient_installer_core::releases::is_newer(&o.version, &version, false));
        let blocker = newer.as_ref().and_then(|o| sentient_installer_core::gateway::offer_blocker(&status, o, crate::remote::is_windows(&self.inst)));
        let this = cx.entity().downgrade();
        let act = |action: &'static str| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
            let this = this.clone();
            std::rc::Rc::new(move |_, cx| {
                this.update(cx, |p, cx| p.run_job("Gateway", vec!["gateway-control".into(), action.into()], cx)).ok();
            })
        };
        let services = if controllable {
            crate::manager::status::service_buttons(&theme, "r-gw-", busy, Some(crate::manager::status::Running { serving: running, any_up: running }), "Stop", act("start"), act("restart"), act("stop"))
        } else {
            Vec::new()
        };
        // The settings card under the status card (crate::manager::gateway_settings).
        let base = self.status.got().and_then(|st| st.http_port).map(|port| format!("http://{}:{port}", self.inst.host));
        let url = crate::manager::gateway_settings::sentient_page(base.as_deref(), Some(host.as_str()).filter(|h| !h.is_empty()), &[self.inst.host.as_str()]);
        let settings = match &self.gw_settings {
            Read::Loading => None,
            Read::Failed(e) => Some(Err(e.clone())),
            Read::Got(s) => Some(Ok(s.clone())),
        };
        self.gw_card.update(cx, |c, _| c.feed(settings, url, busy));
        let card = self.gw_card.clone();
        let panel = p.children(ui::card_actions(
            &theme,
            h_flex()
                .gap(crate::ui::button_gap())
                .flex_wrap()
                // A wrapped line ends at the right too.
                .justify_end()
                .items_center()
                .child(crate::ui::probed(crate::ui::button("r-gw-remove").danger().label("Remove the Gateway").disabled(busy).on_click(cx.listener(move |this, _, _, cx| this.open_gateway_remove(target.clone(), cx)))))
                .when(outside, |r| {
                    r.child(crate::ui::probed(
                        h_flex().gap_1().items_center().child(crate::ui::button("r-gw-adopt").outline().label("Manage it here").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_job("Recording the gateway", vec!["gateway-adopt".into()], cx)))).child(ui::info(
                            "r-gw-adopt-help".into(),
                            "Records this gateway as the one the Platform Manager manages on this server. Nothing about it changes: the container, its compose file and its volumes stay as they are.".into(),
                        )),
                    ))
                })
                .child(crate::ui::probed(crate::manager::status::logs_button("r-gw-", |_, _, cx| {
                    crate::logsource::request(crate::logsource::Source::Gateway);
                    cx.refresh_windows();
                })))
                .children(services.into_iter().map(crate::ui::probed))
                .when_some(newer, |r, o| {
                    let v = o.version.clone();
                    let button = crate::ui::button("r-gw-update")
                        .primary()
                        .label(format!("Update to {}", o.version))
                        .disabled(busy || blocker.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| this.run_job("Updating the gateway", vec!["gateway-update".into(), v.clone()], cx)));
                    r.child(crate::ui::probed(match blocker.clone() {
                        // A disabled button carries its reason as a tooltip.
                        Some(why) => div().id("r-gw-update-why").child(button).tooltip(crate::ui::tooltip(why)).into_any_element(),
                        None => button.into_any_element(),
                    }))
                }),
        ));
        v_flex().flex_1().min_h_0().gap(px(ui::GAP)).child(panel.flex_shrink_0()).child(card)
    }

    /// What the gateway's settings card asks for, on the server: Apply (a
    /// plan with no secret, put in the account's home and given to the
    /// agent's gateway-apply) and the password (the agent's
    /// gateway-ui-password, on its standard input) as the page's job; the
    /// test in the background.
    fn run_gw_work(&mut self, work: crate::manager::gateway_settings::Work, cx: &mut Context<Self>) {
        use crate::manager::gateway_settings::Work;
        use sentient_installer_core::progress::{PlanStep, Progress};
        let inst = self.inst.clone();
        // Screens: a canned server gateway is never changed.
        let canned = cfg!(debug_assertions) && (std::env::var_os("SPM_DEBUG_REMOTE_GATEWAY").is_some() || std::env::var_os("SPM_DEBUG_REMOTE_GATEWAY_SETTINGS").is_some());
        if let Work::Test = work {
            let card = self.gw_card.clone();
            cx.spawn(async move |_, cx| {
                let r = if canned { Ok("Screens: a canned gateway; nothing was tested.".to_string()) } else { cx.background_executor().spawn(async move { remote::gateway_test(&inst) }).await };
                card.update(cx, |c, cx| c.set_test(Some(r), cx));
            })
            .detach();
            return;
        }
        if self.job.read(cx).is_running() {
            let other = self.job_label.clone().map(|l| l.to_string()).unwrap_or_else(|| "another job".into());
            ui::show_error("The gateway's settings cannot be changed yet", format!("{other} is still running on {}. Try again once it has finished.", inst.name), cx);
            return;
        }
        self.shown_job = Some(Kind::Gateway);
        match work {
            Work::Apply(plan) => {
                self.job_label = Some(format!("Applying the gateway's settings on {}", inst.name).into());
                let kind = match &self.gw_settings {
                    Read::Got(s) => s.machine.kind.clone(),
                    _ => String::new(),
                };
                let names = format!("{}. {}", plan.names().join(", "), plan.restart_words(&kind));
                self.job.update(cx, |job, cx| {
                    job.set_plan(vec![crate::jobplan::connect(), PlanStep::new("Applying the settings").detail(names).weight(3.), PlanStep::new("Reading the settings back")]);
                    job.set_label(format!("Applying the gateway's settings on {}", inst.name));
                    job.set_on_page(true);
                    job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
                    job.start(
                        move |sink| {
                            sink(Progress::Step { name: crate::jobplan::CONNECT.into() });
                            if canned {
                                sink(Progress::Done { message: "Screens: a canned gateway; nothing was changed.".into() });
                                return Ok(());
                            }
                            // As run_job: an agent that could not be brought up to
                            // date still runs the job when it knows it.
                            if let Err(e) = remote::deploy(&inst, &sink) {
                                sink(Progress::Log { line: format!("The agent could not be updated: {}", remote::split_failure(&e).0) });
                            }
                            let file = format!("{}/.spm-gateway-apply.json", remote::home(&inst)?);
                            remote::put_text(&inst, &file, &serde_json::to_string(&plan).map_err(|e| e.to_string())?)?;
                            sink(Progress::Step { name: "Applying the settings".into() });
                            let id = remote::start_job(&inst, &["gateway-apply", &file])?;
                            let r = remote::follow(&inst, &id, &sink);
                            remote::remove_file(&inst, &file);
                            r
                        },
                        cx,
                    )
                });
            }
            Work::Password(pw) => {
                self.job_label = Some(format!("Changing the gateway page's password on {}", inst.name).into());
                self.job.update(cx, |job, cx| {
                    job.set_plan(vec![crate::jobplan::connect(), PlanStep::new("Setting the password")]);
                    job.set_label(format!("Changing the gateway page's password on {}", inst.name));
                    job.set_on_page(true);
                    job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
                    job.start(
                        move |sink| {
                            sink(Progress::Step { name: crate::jobplan::CONNECT.into() });
                            if canned {
                                sink(Progress::Done { message: "Screens: a canned gateway; nothing was changed.".into() });
                                return Ok(());
                            }
                            if let Err(e) = remote::deploy(&inst, &sink) {
                                sink(Progress::Log { line: format!("The agent could not be updated: {}", remote::split_failure(&e).0) });
                            }
                            sink(Progress::Step { name: "Setting the password".into() });
                            remote::gateway_ui_password(&inst, &pw).map(|m| sink(Progress::Done { message: m }))
                        },
                        cx,
                    )
                });
            }
            Work::Test => {}
        }
        cx.notify();
    }

    /// No gateway on the server: the install form (crate::manager::gateway::
    /// InstallForm, as on this computer's page), "Runs as" with only what
    /// the server can run, and Install.
    fn gateway_install_panel(&self, get: &dyn Fn(&str) -> String, busy: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let windows = remote::is_windows(&self.inst);
        let docker = match get("docker").as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        let runs = crate::manager::gateway::install_runs_as(windows, docker, &self.inst.name);
        let found = self.updates.read(cx).found.as_ref().map(|f| f.gateway.clone());
        let offer = found.clone().and_then(|r| r.ok()).flatten();
        // Why Install cannot run (its tooltip): nothing the server runs, an
        // image not published yet, a release without its Windows installer.
        let blocker = windows.then(|| WINDOWS_REMOTE_INSTALL.to_string()).or_else(|| runs.clone().err()).or_else(|| {
            offer.as_ref().and_then(|o| {
                if o.image_missing && !windows {
                    Some(sentient_installer_core::dockerhub::not_published(&o.version))
                } else if windows && o.installer.is_none() {
                    Some(format!("Gateway {} has no Windows installer.", o.version))
                } else {
                    None
                }
            })
        });
        let runs_row = match runs {
            Ok(r) => r,
            Err(why) => ("Docker container (Docker is not on this server)".to_string(), why),
        };
        let action = match offer {
            Some(o) => {
                let v = o.version.clone();
                let button = crate::ui::button("r-gw-install")
                    .primary()
                    .label("Install the gateway")
                    .disabled(busy || blocker.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| this.start_gateway_install(v.clone(), cx)));
                // What is installed on the left, Install at the card's end.
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(format!(
                        "Sentient Gateway {}, {}",
                        o.version,
                        if windows { "the installer from the SENTIENT releases site, checked before use" } else { "the quakestring/sentient-gateway-rs image, run by Docker" }
                    )))
                    .child(match blocker.clone() {
                        // A disabled button carries its reason as a tooltip.
                        Some(why) => div().id("r-gw-install-why").flex_shrink_0().child(button).tooltip(crate::ui::tooltip(why)).into_any_element(),
                        None => div().flex_shrink_0().child(button).into_any_element(),
                    })
                    .into_any_element()
            }
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(match &found {
                    Some(Err(e)) => format!("The gateway releases could not be read: {e}"),
                    Some(Ok(_)) => "No gateway release on this channel.".to_string(),
                    None => "Looking for the newest gateway…".to_string(),
                })
                .into_any_element(),
        };
        ui::panel(&theme)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(ui::product_logo(ui::Product::Gateway, 24.))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Sentient Gateway"))
                    .child(ui::pill(&theme, Tone::Neutral, "Not installed"))
                    .child(ui::info("gw-not-installed".into(), format!("Looked for: {}", sentient_installer_core::gateway::CHECKS).into())),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "The gateway reads devices next to this server. In SENTIENT, add a gateway device and copy its access token (Manage credentials).",
            ))
            .child(self.gw_form.fields(&theme, windows, Some(crate::manager::gateway::REMOTE_TOKEN_HELP), Some(runs_row)))
            .children(self.gw_problem.clone().map(|p| ui::outcome(&theme, Tone::Bad, p)))
            .children(ui::card_actions(&theme, action))
    }

    /// Install: the form's values once, the job on the server's agent with
    /// the token and the page's password on its standard input
    /// (crate::remote::gateway_install), never on a command line, in a log
    /// or in this computer's profile.
    fn start_gateway_install(&mut self, version: String, cx: &mut Context<Self>) {
        use sentient_installer_core::progress::{PlanStep, Progress};
        if self.job.read(cx).is_running() {
            let other = self.job_label.clone().map(|l| l.to_string()).unwrap_or_else(|| "another job".into());
            ui::show_error("The gateway cannot be installed yet", format!("{other} is still running on {}. Try again once it has finished.", self.inst.name), cx);
            return;
        }
        let windows = remote::is_windows(&self.inst);
        let settings = match self.gw_form.settings(cx, windows) {
            Ok(s) => s,
            Err(e) => {
                self.gw_problem = Some(e.into());
                cx.notify();
                return;
            }
        };
        self.gw_problem = None;
        let inst = self.inst.clone();
        // Screens: a canned server gateway is never changed.
        let canned = cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_REMOTE_GATEWAY").is_some();
        let args = remote::gateway_install_args(&settings.host, settings.port, &version);
        self.last_args = args;
        self.shown_job = Some(Kind::Gateway);
        let label = format!("Installing the gateway on {}", inst.name);
        self.job_label = Some(label.clone().into());
        let me = cx.entity().downgrade();
        self.job.update(cx, |job, cx| {
            let mut plan = vec![crate::jobplan::connect(), PlanStep::new("Sending the access token")];
            if !windows {
                plan.push(PlanStep::new(format!("Pulling {}:{version}", sentient_installer_core::gateway::IMAGE)).weight(3.));
                plan.push(PlanStep::new(format!("Starting Sentient Gateway {version}")));
            }
            job.set_plan(plan);
            job.set_label(label);
            job.set_on_page(true);
            job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
            // Try again: back to the form, the token still in it (it is
            // cleared only once the install went through).
            job.set_retry(std::rc::Rc::new(move |cx: &mut App| {
                me.update(cx, |p, cx| {
                    p.shown_job = None;
                    p.job.update(cx, |j, cx| j.reset(cx));
                    cx.notify();
                })
                .ok();
            }));
            job.start(
                move |sink| {
                    sink(Progress::Step { name: crate::jobplan::CONNECT.into() });
                    if canned {
                        sink(Progress::Done { message: "Screens: a canned gateway; nothing was installed.".into() });
                        return Ok(());
                    }
                    if let Err(e) = remote::deploy(&inst, &sink) {
                        sink(Progress::Log { line: format!("The agent could not be updated: {}", remote::split_failure(&e).0) });
                    }
                    sink(Progress::Step { name: "Sending the access token".into() });
                    let sentient_installer_core::gateway::Settings { host, port, token, ui_password } = settings;
                    remote::gateway_install(&inst, &host, port, &version, remote::GatewaySecrets { token, ui_password }, &sink)
                },
                cx,
            )
        });
        cx.notify();
    }

    /// "Remove the Gateway…" on the server: the same dialog as here, then
    /// the agent's gateway-remove job (it runs as root there).
    pub(crate) fn open_gateway_remove(&mut self, target: crate::manager::gateway::RemoveTarget, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::manager::gateway::open_remove_dialog(
            target,
            std::rc::Rc::new(move |keep, image, cx: &mut App| {
                let mut args = vec!["gateway-remove".to_string()];
                if !keep {
                    args.push("--delete-data".into());
                }
                if image {
                    args.push("--remove-image".into());
                }
                this.update(cx, |p, cx| p.run_job("Removing the gateway", args, cx)).ok();
            }),
            cx,
        );
    }

    /// One calm panel for a server that is not set up or cannot be reached:
    /// what is the matter and the one thing that solves it. A Platform
    /// Manager installed there (crate::remote::reuse) is offered as the agent
    /// when it does the jobs the same way.
    fn gate_panel(&self, e: String, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let busy = self.job.read(cx).is_running();
        let not_set_up = e == remote::NOT_SET_UP;
        let installed = remote::reuse::is_offer(&e);
        let usable = installed && remote::reuse::found(&self.inst.id).is_some_and(|f| remote::reuse::fit(&f.info, &remote::reuse::AgentInfo::this()) == remote::reuse::Fit::Compatible);
        let title = if installed {
            "Platform Manager installed on this server"
        } else if not_set_up {
            "Server agent not set up"
        } else {
            "The server does not answer"
        };
        let set_up = |primary: bool| {
            let b = crate::ui::button("r-setup").label(if installed { "Set up the agent" } else { "Set up" }).loading(busy).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.set_up(cx)));
            if primary { b.primary() } else { b.outline() }
        };
        ui::panel(&theme)
            .child(div().font_weight(FontWeight::MEDIUM).child(title))
            .child(div().text_sm().text_color(theme.muted_foreground).child(if not_set_up {
                "The Platform Manager works on a server through its server agent. Set up copies the matching agent over SSH and checks it; nothing else on the server changes.".to_string()
            } else {
                e.clone()
            }))
            .child(if usable {
                ui::button_group(vec![
                    crate::ui::button("r-use-installed").primary().label("Use it").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.use_installed(cx))).into_any_element(),
                    set_up(false).into_any_element(),
                ])
            } else if not_set_up || installed {
                h_flex().child(set_up(true))
            } else {
                h_flex().child(crate::ui::button("r-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| {
                    this.ready = Read::Loading;
                    this.reload(cx);
                })))
            })
    }

    /// "Use it anyway": the installed Platform Manager does the jobs.
    fn use_installed(&mut self, cx: &mut Context<Self>) {
        match remote::use_installed(&self.inst) {
            Ok(()) => {
                self.ready = Read::Loading;
                self.reload(cx);
            }
            Err(e) => self.ready = Read::Failed(e),
        }
        cx.notify();
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
            // jobview: short (one step, followed in the progress strip).
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
        self.gw_form.apply_clear(window, cx);
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
        let gateway_only = matches!(&self.status, Read::Got(s) if s.gateway_only());
        let summary = match self.kind {
            Kind::Status if gateway_only => "This server and what runs on it.",
            Kind::Status if matches!(&self.status, Read::Got(s) if !s.installed) => "This server: what runs on it, and what can be installed.",
            Kind::Logs if gateway_only => "The gateway's log and the agent's on this server, newest at the bottom.",
            _ => summary,
        };
        // Set up earlier but nothing added: add a lone SENTIENT found there,
        // once (it changes nothing on the server; the agent asks when the
        // choice is real).
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_REMOTE_STATUS").is_some() {
            self.auto_add_tried = true; // canned screens: no server to add on
        }
        // An install running on the server (started earlier, maybe from a
        // window since closed, or before the connection dropped): shown
        // again, followed where it is.
        if self.install.is_none() && !self.reattach_tried && !self.job.read(cx).is_running() {
            if let Some(id) = self.status.got().and_then(|s| remote::install::running_install(s.running_job.as_deref())).map(str::to_string) {
                self.reattach_tried = true;
                self.auto_add_tried = true;
                self.open_install(Some(id), window, cx);
            }
        }
        // Screens: the flow open on a step (SPM_DEBUG_INSTALL_STEP).
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_INSTALL_STEP").is_some() && self.install.is_none() && !self.reattach_tried && matches!(&self.ready, Read::Got(())) {
            self.reattach_tried = true;
            self.open_install(None, window, cx);
        }
        if let Some(flow) = &self.install {
            return flow.clone().into_any_element();
        }
        if !self.auto_add_tried && matches!(&self.ready, Read::Got(())) && matches!(&self.status, Read::Got(s) if !s.installed && s.running_job.is_none()) && !self.job.read(cx).is_running() {
            self.auto_add_tried = true;
            self.run_job("Looking for SENTIENT on the server", vec!["add-if-one".into()], cx);
        }
        let gate: Option<Div> = match &self.ready {
            Read::Loading => Some(self.skeleton(&theme)),
            // While Set up runs or shows its result, only its panel.
            Read::Failed(_) if self.job_label.is_some() => Some(div()),
            Read::Failed(e) => Some(self.gate_panel(e.clone(), cx)),
            // A gateway-only server: its Logs are the gateway's and the agent's.
            Read::Got(()) if self.kind != Kind::Status && self.kind != Kind::Gateway && matches!(&self.status, Read::Got(s) if !s.installed && !(s.gateway_only() && self.kind == Kind::Logs)) => {
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
        let mut fixed = false;
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
            Kind::Settings => {
                fixed = matches!(self.settings, Read::Got(_));
                self.settings_page(window, cx)
            }
            Kind::Migration => self.migration_page(cx),
            Kind::Repair => self.repair_page(cx),
            Kind::Gateway => self.gateway_page(cx),
        } };
        // An agent too old for this page's jobs: one notice and Update agent,
        // in place of every panel that cannot load.
        let fixed = fixed && !self.agent_old();
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
        let reload = h_flex().gap_3().items_center().min_h(px(32.)).children(self.stale_note(&theme, cx)).child(crate::ui::probed(reload));
        // Platform settings scroll inside their card (its Apply bar stays at
        // the card's bottom), not the page.
        let page = ui::page_with(&theme, title, summary, reload);
        if fixed { page.overflow_y_hidden().child(body) } else { page.child(body) }.into_any_element()
    }
}

/// The window closed (the app runs on): what the pages read stays for
/// the next window ([`Kept`]).
impl Drop for RemotePages {
    fn drop(&mut self) {
        let k = Kept {
            conn: conn_of(&self.inst),
            ready: std::mem::replace(&mut self.ready, Read::Loading),
            status: std::mem::replace(&mut self.status, Read::Loading),
            services: std::mem::replace(&mut self.services, Read::Loading),
            db: std::mem::replace(&mut self.db, Read::Loading),
            backups: std::mem::replace(&mut self.backups, Read::Loading),
            settings: std::mem::replace(&mut self.settings, Read::Loading),
            gateway: std::mem::replace(&mut self.gateway, Read::Loading),
            gw_settings: std::mem::replace(&mut self.gw_settings, Read::Loading),
            offer: std::mem::replace(&mut self.offer, Read::Loading),
            versions: std::mem::replace(&mut self.versions, Read::Loading),
            restores: std::mem::replace(&mut self.restores, Read::Loading),
            updated_at: self.updated_at.take(),
            auto_add_tried: self.auto_add_tried,
        };
        // Nothing read yet: nothing to keep.
        if matches!(k.ready, Read::Loading) {
            return;
        }
        KEPT.lock().unwrap_or_else(|e| e.into_inner()).insert(self.inst.id.clone(), k);
    }
}

/// Whether a page of `kind` shows what it read rather than its loading
/// skeleton (Status: the state, the services and the database).
fn shows_data(kind: Kind, ready: &Read<()>, status: &Read<RemoteStatus>, services: &Read<Vec<RemoteService>>, db: &Read<DbStatus>) -> bool {
    matches!(ready, Read::Got(_)) && status.got().is_some() && (kind != Kind::Status || (services.got().is_some() && db.got().is_some()))
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
        signature: None,
        unsigned: false,
        signing_started: true,
        image_missing: false,
        build_version: None,
        min_glibc: None,
        between: vec![
            n("4.4.0.2", "2026-11-14", "Request a demo or buy a licence from inside SENTIENT, and editor fixes.", vec![
                ("Added", vec!["Licence: Request demo licence and Buy licence open dialogs inside SENTIENT."]),
                ("Fixed", vec!["The fullscreen buttons of the JSON editor no longer submit the form they sit in.", "The licence limits on telemetry and attribute keys count the customer's keys only."]),
            ]),
            n("4.4.0.1", "2026-11-13", "First release of the 4.4 line.", vec![("Added", vec!["CoAP and LwM2M devices (DTLS, OSCORE).", "18 new widgets."])]),
        ],
    }
}

#[cfg(test)]
mod reopen_tests {
    use super::{conn_of, shows_data, take_kept, DbStatus, Instance, Kept, Kind, Read, RemoteStatus};

    fn inst(id: &str, host: &str) -> Instance {
        serde_json::from_value(serde_json::json!({ "id": id, "name": "Plant B", "color": "#6b4fa0", "host": host, "user": "ops" })).unwrap()
    }

    fn kept(inst: &Instance) -> Kept {
        Kept {
            conn: conn_of(inst),
            ready: Read::Got(()),
            status: Read::Got(RemoteStatus { installed: true, ..Default::default() }),
            services: Read::Got(Vec::new()),
            db: Read::Got(DbStatus::default()),
            backups: Read::Loading,
            settings: Read::Loading,
            gateway: Read::Loading,
            gw_settings: Read::Loading,
            offer: Read::Loading,
            versions: Read::Loading,
            restores: Read::Loading,
            updated_at: Some("10:42".into()),
            auto_add_tried: true,
        }
    }

    /// Cold start: nothing read in this run, the loading skeleton.
    #[test]
    fn a_cold_start_has_nothing_to_draw() {
        let mut map = std::collections::HashMap::new();
        assert!(take_kept(&mut map, &inst("b", "10.0.0.2")).is_none());
    }

    /// A window opened again: Status draws what it read, at once.
    #[test]
    fn a_reopened_window_draws_what_was_read() {
        let i = inst("b", "10.0.0.2");
        let mut map = std::collections::HashMap::new();
        map.insert(i.id.clone(), kept(&i));
        let k = take_kept(&mut map, &i).expect("kept for the reopened window");
        assert!(shows_data(Kind::Status, &k.ready, &k.status, &k.services, &k.db));
        // Taken once: the new pages own it.
        assert!(map.is_empty());
    }

    /// The instance was edited meanwhile (another address): read afresh.
    #[test]
    fn an_edited_instance_is_read_afresh() {
        let i = inst("b", "10.0.0.2");
        let mut map = std::collections::HashMap::new();
        map.insert(i.id.clone(), kept(&i));
        assert!(take_kept(&mut map, &inst("b", "10.0.0.9")).is_none());
    }

    /// Before a reading came back the page shows the skeleton.
    #[test]
    fn nothing_read_is_the_skeleton() {
        assert!(!shows_data(Kind::Status, &Read::Loading, &Read::Loading, &Read::Loading, &Read::Loading));
        assert!(!shows_data(Kind::Status, &Read::Got(()), &Read::Got(RemoteStatus::default()), &Read::Loading, &Read::Got(DbStatus::default())));
    }
}
