// Copyright © 2016-2026 The SENTIENT Authors

//! Instances: this computer and SENTIENT on other computers (crate::remote).
//! One card each, with its state and the everyday actions; adding a server
//! checks its key fingerprint before anything is sent to it.

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::remote::{self, Auth, Detected, Instance, Os, RemoteStatus};
use crate::ui::{self, Tone};

#[derive(Clone)]
enum Retry {
    SetUp(String),
    Job(String, &'static str, Vec<&'static str>),
}

/// Open an instance (None: this computer).
pub struct OpenInstance(pub Option<String>);

/// The list changed (the switcher reads it again).
pub struct ListChanged;

enum Stage {
    Edit,
    Checking,
    /// The server's key, waiting for the operator to accept it.
    Fingerprint { keys: String, fingerprint: String },
    Connecting,
}

struct Form {
    editing: Option<String>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    key_file: Entity<InputState>,
    secret: Entity<InputState>,
    /// sudo's password, for a key sign-in that is not root.
    sudo_secret: Entity<InputState>,
    color_picker: Entity<ColorPickerState>,
    auth: Auth,
    /// Linux or Windows: what the help shows; connecting checks it.
    os: Os,
    color: String,
    stage: Stage,
    error: Option<SharedString>,
    /// The Setup guide beside the form (the mockup's G1).
    guide: bool,
    guide_scroll: ScrollHandle,
    /// The command copied from the guide last, and when (its tick).
    copied: Option<(String, std::time::Instant)>,
    /// Enter moving through the fields (dropped with the form).
    enter: Vec<Subscription>,
}

pub struct InstancesPage {
    local: Option<InstallationRecord>,
    list: Vec<Instance>,
    statuses: HashMap<String, Result<RemoteStatus, String>>,
    /// Read again once the refresh under way ends.
    refresh_again: HashSet<String>,
    refreshing: HashSet<String>,
    /// The tile grid's own width as laid out (the last frame's), so the
    /// tiles fill exactly the width the title row has.
    grid_w: std::rc::Rc<std::cell::Cell<f32>>,
    form: Option<Form>,
    job: Entity<Job>,
    /// (instance id, what runs) of the job shown.
    job_for: Option<(String, SharedString)>,
    /// What "Try again" repeats after a job failed.
    retry: Option<Retry>,
    /// The failed job's technical details are shown.
    show_details: bool,
    /// The add/edit dialog and the job dialog are open.
    form_dialog: bool,
    job_dialog: bool,
    /// The instance whose removal is being confirmed.
    confirm_remove: Option<String>,
    /// The instance open in the whole Platform Manager (None: this computer).
    pub open: Option<String>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OpenInstance> for InstancesPage {}
impl EventEmitter<ListChanged> for InstancesPage {}

impl InstancesPage {
    /// The list again from the saved instances (one was removed elsewhere).
    pub fn reload_list(&mut self, cx: &mut Context<Self>) {
        self.list = remote::load();
        self.statuses.retain(|id, _| id == LOCAL || self.list.iter().any(|i| &i.id == id));
        if self.open.as_ref().is_some_and(|id| !self.list.iter().any(|i| &i.id == id)) {
            self.open = None;
        }
        cx.notify();
    }

    /// A server's state as its card shows it (the switcher's dots), from
    /// the cached status; None before it was first read.
    pub fn tone_of(&self, id: &str) -> Option<Tone> {
        let st = self.statuses.get(id)?;
        Some(state_of(Some(st), false).0)
    }

    pub fn new(local: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        // The error panel's "Ask for the password again": Try again.
        let me = cx.entity().downgrade();
        job.update(cx, |j, _| {
            j.set_fix_handler(std::rc::Rc::new(move |action, _, cx| {
                if *action != sentient_installer_core::diagnose::FixAction::AskAgain {
                    return false;
                }
                me.update(cx, |this, cx| {
                    let Some(retry) = this.retry.clone() else { return false };
                    this.job.update(cx, |j, cx| j.reset(cx));
                    match retry {
                        Retry::SetUp(id) => this.set_up(&id, cx),
                        Retry::Job(id, label, args) => this.run_job(&id, label, args, cx),
                    }
                    true
                })
                .unwrap_or(false)
            }), false)
        });
        let subs = vec![cx.observe(&job, |this, job, cx| {
            // A finished job changes what the card shows.
            if !job.read(cx).is_running() {
                if let Some((id, _)) = this.job_for.clone() {
                    this.refresh(&id, cx);
                    // The dialog's title says how it ended.
                    let name = this.instance(&id).map(|i| i.name).unwrap_or_default();
                    match job.read(cx).state() {
                        JobState::Done(_) if this.job_dialog => crate::modal::set_title(format!("Finished on {name}"), cx),
                        JobState::Failed(_) if this.job_dialog => crate::modal::set_title(format!("Did not finish on {name}"), cx),
                        _ => {}
                    }
                }
            }
            cx.notify();
        })];
        let mut page = Self {
            local,
            list: remote::load(),
            statuses: HashMap::new(),
            refreshing: HashSet::new(),
            grid_w: Default::default(),
            refresh_again: HashSet::new(),
            form: None,
            job,
            job_for: None,
            retry: None,
            show_details: false,
            form_dialog: false,
            job_dialog: false,
            confirm_remove: None,
            open: None,
            _tasks: Vec::new(),
            _subscriptions: subs,
        };
        let ids: Vec<String> = page.list.iter().map(|i| i.id.clone()).collect();
        for id in ids {
            page.refresh(&id, cx);
        }
        if page.local.is_some() {
            page.refresh_local(cx);
        }
        // Screens: the job dialog running, finished or failed on the first
        // remote instance (SPM_DEBUG_JOB=running|done|failed).
        #[cfg(debug_assertions)]
        if let (Ok(kind), Some(inst)) = (std::env::var("SPM_DEBUG_JOB"), page.list.first().cloned()) {
            use sentient_installer_core::progress::Progress;
            page.job_for = Some((inst.id.clone(), format!("Setting up {}", inst.name).into()));
            page.retry = Some(Retry::SetUp(inst.id.clone()));
            let name = inst.name.clone();
            page.job.update(cx, |job, cx| {
                job.set_plan(vec![
                    crate::jobplan::connect().when(&["Copying the Platform Manager"]),
                    sentient_installer_core::progress::PlanStep::new("Looking for SENTIENT on the server"),
                ]);
                job.start(
                    move |sink| {
                        sink(Progress::Step { name: "Copying the Platform Manager to the server".into() });
                        sink(Progress::Log { line: "sentient-platform-manager-agent-linux-x86_64: 11.3 MB".into() });
                        match kind.as_str() {
                            "running" => {
                                std::thread::sleep(std::time::Duration::from_secs(120));
                                Ok(())
                            }
                            "failed" => Err(remote::failed("The server refused the administrator password.", "sudo: 1 incorrect password attempt")),
                            _ => {
                                sink(Progress::Step { name: "Looking for SENTIENT on the server".into() });
                                sink(Progress::Done { message: format!("The Platform Manager is set up on {name}. SENTIENT 4.4.0.2 (Docker) was found and added. Nothing on it was changed.") });
                                Ok(())
                            }
                        }
                    },
                    cx,
                )
            });
        }
        // The cards refresh every minute.
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(60)).await;
            let alive = this
                .update(cx, |page, cx| {
                    let ids: Vec<String> = page.list.iter().map(|i| i.id.clone()).collect();
                    for id in ids {
                        page.refresh(&id, cx);
                    }
                    if page.local.is_some() {
                        page.refresh_local(cx);
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        });
        page._tasks.push(tick);
        page
    }

    fn instance(&self, id: &str) -> Option<Instance> {
        self.list.iter().find(|i| i.id == id).cloned()
    }

    fn refresh(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(inst) = self.instance(id) else { return };
        if !self.refreshing.insert(id.to_string()) {
            // One is under way and may have started before a job changed the
            // server: read again when it ends.
            self.refresh_again.insert(id.to_string());
            return;
        }
        let id = id.to_string();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let i = inst.clone();
            let got = cx.background_executor().spawn(async move { remote::status(&i) }).await;
            this.update(cx, |page, cx| {
                page.refreshing.remove(&id);
                let again = page.refresh_again.remove(&id);
                page.statuses.insert(id.clone(), got);
                if again {
                    page.refresh(&id, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    // -- the add / edit form ------------------------------------------------

    fn open_form(&mut self, editing: Option<Instance>, window: &mut Window, cx: &mut Context<Self>) {
        let input = |cx: &mut Context<Self>, window: &mut Window, value: String, placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).default_value(value).placeholder(placeholder))
        };
        let e = editing.clone();
        let used: Vec<String> = self.list.iter().map(|i| i.color.to_lowercase()).collect();
        let color = e
            .as_ref()
            .map(|i| i.color.clone())
            .unwrap_or_else(|| remote::PALETTE.iter().find(|c| !used.contains(&c.to_string())).unwrap_or(&remote::PALETTE[0]).to_string());
        self.form = Some(Form {
            editing: e.as_ref().map(|i| i.id.clone()),
            name: input(cx, window, e.as_ref().map(|i| i.name.clone()).unwrap_or_default(), "Plant B server"),
            host: input(cx, window, e.as_ref().map(|i| i.host.clone()).unwrap_or_default(), "192.168.1.20 or plantb.local"),
            port: input(cx, window, e.as_ref().map(|i| i.port.to_string()).unwrap_or_else(|| "22".into()), "22"),
            user: input(cx, window, e.as_ref().map(|i| i.user.clone()).unwrap_or_default(), "admin"),
            key_file: input(
                cx,
                window,
                e.as_ref().and_then(|i| i.key_file.as_ref()).map(|k| k.display().to_string()).unwrap_or_else(default_key),
                "~/.ssh/id_ed25519",
            ),
            secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(if editing.is_some() { "Leave empty to keep the saved one" } else { "Key passphrase or password (if any)" })
            }),
            sudo_secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(if editing.is_some() { "Leave empty to keep the saved one" } else { "Only when sudo asks for one" })
            }),
            color_picker: {
                let start = remote::parse_hex(&color).unwrap_or(0x6b4fa0);
                let picker = cx.new(|cx| ColorPickerState::new(window, cx).default_value(rgb(start)));
                self._subscriptions.push(cx.subscribe(&picker, |this, _, e: &ColorPickerEvent, cx| {
                    let ColorPickerEvent::Change(Some(c)) = e else { return };
                    if let Some(f) = &mut this.form {
                        f.color = hex_of(*c);
                    }
                    cx.notify();
                }));
                picker
            },
            auth: e.as_ref().map(|i| i.auth).unwrap_or_default(),
            os: e.as_ref().map(|i| i.os).unwrap_or_default(),
            color,
            stage: Stage::Edit,
            error: None,
            guide: false,
            guide_scroll: ScrollHandle::new(),
            copied: None,
            enter: Vec::new(),
        });
        // Enter: the next field shown, and on the last one Connect.
        if let Some(f) = &self.form {
            let fields = [f.name.clone(), f.host.clone(), f.port.clone(), f.user.clone(), f.key_file.clone(), f.secret.clone(), f.sudo_secret.clone()];
            let subs = crate::ui::enter_moves_on(
                &fields,
                window,
                cx,
                |this: &Self, _| {
                    let Some(f) = &this.form else { return Vec::new() };
                    let mut v = vec![f.name.clone(), f.host.clone(), f.port.clone(), f.user.clone()];
                    if f.auth == Auth::Key {
                        v.push(f.key_file.clone());
                    }
                    v.push(f.secret.clone());
                    if f.auth == Auth::Key && f.os == Os::Linux {
                        v.push(f.sudo_secret.clone());
                    }
                    v
                },
                |this: &mut Self, _, cx| {
                    let f = this.form.as_ref()?;
                    if this.job.read(cx).is_running() {
                        return None;
                    }
                    let empty = |e: &Entity<InputState>| e.read(cx).value().trim().is_empty();
                    let first_bad = [&f.name, &f.host, &f.user].into_iter().find(|e| empty(e)).cloned();
                    this.check(cx);
                    first_bad
                },
            );
            if let Some(f) = &mut self.form {
                f.enter = subs;
            }
        }
        cx.notify();
    }

    /// The instance as the form describes it.
    fn from_form(&self, cx: &App) -> Result<Instance, String> {
        let f = self.form.as_ref().ok_or("no form")?;
        let v = |s: &Entity<InputState>| s.read(cx).value().trim().to_string();
        let name = v(&f.name);
        let host = v(&f.host);
        let user = v(&f.user);
        if name.is_empty() || host.is_empty() || user.is_empty() {
            return Err("Name, address and user are needed.".into());
        }
        let port: u16 = v(&f.port).parse().map_err(|_| "The port is a number, usually 22.")?;
        let key = v(&f.key_file);
        if f.auth == Auth::Key && key.is_empty() {
            return Err("Choose the key file to sign in with.".into());
        }
        let old = f.editing.as_ref().and_then(|id| self.instance(id));
        Ok(Instance {
            id: f.editing.clone().unwrap_or_else(remote::new_id),
            name,
            color: f.color.clone(),
            os: f.os,
            host,
            port,
            user,
            auth: f.auth,
            key_file: (f.auth == Auth::Key).then(|| expand_home(&key)),
            key_has_passphrase: !v(&f.secret).is_empty() || old.as_ref().is_some_and(|o| o.key_has_passphrase),
            host_key: old.as_ref().and_then(|o| o.host_key.clone()),
            detected: old.and_then(|o| o.detected),
        })
    }

    fn set_error(&mut self, e: impl Into<SharedString>, cx: &mut Context<Self>) {
        if let Some(f) = &mut self.form {
            f.error = Some(e.into());
            f.stage = Stage::Edit;
        }
        cx.notify();
    }

    /// Step 1: the server's key (or straight to connecting when the address
    /// is the one already trusted).
    fn check(&mut self, cx: &mut Context<Self>) {
        let inst = match self.from_form(cx) {
            Ok(i) => i,
            Err(e) => return self.set_error(e, cx),
        };
        let same_server = self.instance(&inst.id).is_some_and(|o| o.host == inst.host && o.port == inst.port && o.host_key.is_some());
        if same_server {
            return self.connect(None, cx);
        }
        if let Some(f) = &mut self.form {
            f.stage = Stage::Checking;
            f.error = None;
        }
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let (h, p) = (inst.host.clone(), inst.port);
            let got = cx.background_executor().spawn(async move { remote::scan(&h, p) }).await;
            this.update(cx, |page, cx| match got {
                Ok((keys, fingerprint)) => {
                    if let Some(f) = &mut page.form {
                        f.stage = Stage::Fingerprint { keys, fingerprint };
                    }
                    cx.notify();
                }
                Err(e) => page.set_error(e, cx),
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Step 2: trust the key (when new), sign in, find out the system, save.
    fn connect(&mut self, keys: Option<(String, String)>, cx: &mut Context<Self>) {
        let mut inst = match self.from_form(cx) {
            Ok(i) => i,
            Err(e) => return self.set_error(e, cx),
        };
        let secret = self.form.as_ref().map(|f| f.secret.read(cx).value().to_string()).unwrap_or_default();
        let sudo_secret = self.form.as_ref().filter(|f| f.auth == Auth::Key).map(|f| f.sudo_secret.read(cx).value().to_string()).unwrap_or_default();
        if let Some(f) = &mut self.form {
            f.stage = Stage::Connecting;
            f.error = None;
        }
        cx.notify();
        let old = self.instance(&inst.id);
        let task = cx.spawn(async move |this, cx| {
            let work = cx.background_executor().spawn(async move {
                if let Some((k, fp)) = &keys {
                    if let Some(o) = &old {
                        remote::untrust(&o.host, o.port);
                    }
                    remote::trust(k)?;
                    inst.host_key = Some(fp.clone());
                }
                let once = (!secret.is_empty()).then_some(secret.as_str());
                match remote::detect(&inst, once) {
                    Ok(d) => {
                        inst.os = d.os;
                        inst.detected = Some(d);
                        if !secret.is_empty() {
                            remote::set_secret(&inst.id, &secret);
                        }
                        if !sudo_secret.is_empty() {
                            remote::set_secret(&remote::sudo_key(&inst.id), &sudo_secret);
                        }
                        Ok(inst)
                    }
                    Err(e) => {
                        if keys.is_some() {
                            remote::untrust(&inst.host, inst.port);
                        }
                        Err(e)
                    }
                }
            });
            let got = work.await;
            this.update(cx, |page, cx| match got {
                Ok(inst) => {
                    let id = inst.id.clone();
                    match page.list.iter_mut().find(|i| i.id == id) {
                        Some(slot) => *slot = inst,
                        None => page.list.push(inst),
                    }
                    if let Err(e) = remote::save(&page.list) {
                        return page.set_error(e, cx);
                    }
                    page.form = None;
                    cx.emit(ListChanged);
                    page.refresh(&id, cx);
                }
                Err(e) => page.set_error(e, cx),
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(inst) = self.instance(id) {
            remote::untrust(&inst.host, inst.port);
            remote::forget_secret(id);
            remote::reuse::forget(id);
        }
        self.list.retain(|i| i.id != id);
        self.statuses.remove(id);
        let _ = remote::save(&self.list);
        self.confirm_remove = None;
        cx.emit(ListChanged);
        cx.notify();
    }

    // -- work on a server ---------------------------------------------------

    fn run_job(&mut self, id: &str, label: &'static str, args: Vec<&'static str>, cx: &mut Context<Self>) {
        self.retry = Some(Retry::Job(id.to_string(), label, args.clone()));
        let Some(inst) = self.instance(id) else { return };
        if self.job.read(cx).is_running() {
            return;
        }
        self.job_for = Some((id.to_string(), format!("{label} on {}", inst.name).into()));
        // Its steps from the first frame (crate::jobplan); the agent's own
        // list replaces them when it answers.
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let docker = inst.detected.as_ref().is_none_or(|d| d.install.eq_ignore_ascii_case("docker"));
        let mut plan = crate::jobplan::guess(&owned, docker, remote::is_windows(&inst));
        let generic = plan.len() == 1;
        if generic {
            plan.push(sentient_installer_core::progress::PlanStep::new(label).weight(3.));
        }
        self.job.update(cx, |job, cx| {
            job.set_plan(plan);
            job.start(
                move |sink| {
                    sink(sentient_installer_core::progress::Progress::Step { name: crate::jobplan::CONNECT.into() });
                    remote::deploy(&inst, &sink)?;
                    if generic {
                        sink(sentient_installer_core::progress::Progress::Step { name: label.to_string() });
                    }
                    let job_id = remote::start_job(&inst, &args)?;
                    remote::follow(&inst, &job_id, &sink)
                },
                cx,
            )
        });
        cx.notify();
    }

    /// "Use it anyway": the Platform Manager installed on the server does
    /// the jobs; then check it again.
    fn use_installed(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(inst) = self.instance(id) else { return };
        match remote::use_installed(&inst) {
            Ok(()) => self.refresh(id, cx),
            Err(e) => {
                self.statuses.insert(id.to_string(), Err(e));
                cx.notify();
            }
        }
    }

    /// Copy the Platform Manager to the server (the first time).
    fn set_up(&mut self, id: &str, cx: &mut Context<Self>) {
        self.retry = Some(Retry::SetUp(id.to_string()));
        let Some(inst) = self.instance(id) else { return };
        if self.job.read(cx).is_running() {
            return;
        }
        self.job_for = Some((id.to_string(), format!("Setting up {}", inst.name).into()));
        self.job.update(cx, |job, cx| {
            job.set_plan(vec![
                crate::jobplan::connect().detail("SSH, and the Platform Manager copied to the server"),
                sentient_installer_core::progress::PlanStep::new("Looking for SENTIENT on the server").detail("A SENTIENT found there is added; nothing on it changes"),
            ]);
            job.start(
                move |sink| {
                    sink(sentient_installer_core::progress::Progress::Step { name: crate::jobplan::CONNECT.into() });
                    remote::deploy(&inst, &sink)?;
                    // A lone SENTIENT already there is added in the same job
                    // (the agent says what it found and did).
                    sink(sentient_installer_core::progress::Progress::Step { name: "Looking for SENTIENT on the server".into() });
                    let job_id = remote::start_job(&inst, &["add-if-one"])?;
                    remote::follow(&inst, &job_id, &sink)
                },
                cx,
            )
        });
        cx.notify();
    }

    /// Follow a job already running on the server (started before this
    /// window, or before the connection was lost).
    fn follow_job(&mut self, id: &str, job_id: String, cx: &mut Context<Self>) {
        let Some(inst) = self.instance(id) else { return };
        if self.job.read(cx).is_running() {
            return;
        }
        self.job_for = Some((id.to_string(), format!("The job running on {}", inst.name).into()));
        self.job.update(cx, |job, cx| {
            // A server's job: its agent diagnoses a failure there.
            job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
            // jobview: its plan comes from the agent (the job's log starts with it).
            job.start(move |sink| remote::follow(&inst, &job_id, &sink), cx)
        });
        cx.notify();
    }

    /// The server's logs in a dialog: the same view as the Logs page.
    fn show_logs(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inst) = self.instance(id) else { return };
        let title = format!("Logs on {}", inst.name);
        let view = cx.new(|cx| crate::manager::logs::LogView::remote(inst, window, cx));
        crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: 1000., icon: None, on_close: None }, cx);
    }

    // -- rendering ----------------------------------------------------------

    /// A job ended there: its card reads its state again.
    pub fn after_job(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        match id {
            Some(id) => self.refresh(id, cx),
            None => {
                self.local = InstallationRecord::load();
                self.refresh_local(cx);
            }
        }
    }

    /// This computer's quick status, read in the background.
    fn refresh_local(&mut self, cx: &mut Context<Self>) {
        if !self.refreshing.insert(LOCAL.to_string()) {
            return;
        }
        let record = self.local.clone();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { crate::headless::status_json(record.as_ref()) }).await;
            this.update(cx, |page, cx| {
                page.refreshing.remove(LOCAL);
                page.statuses.insert(LOCAL.to_string(), Ok(got));
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn local_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let os = if cfg!(windows) { Os::Windows } else { Os::Linux };
        let det = Detected {
            os,
            os_name: format!("{} {}", sysinfo::System::name().unwrap_or_default(), sysinfo::System::os_version().unwrap_or_default()).trim().to_string(),
            distro: sysinfo::System::distribution_id(),
            arch: std::env::consts::ARCH.to_string(),
            ..Default::default()
        };
        let status = if self.local.is_none() { Some(Ok(RemoteStatus::default())) } else { self.statuses.get(LOCAL).cloned() };
        let (tone, state) = state_of(status.as_ref(), self.refreshing.contains(LOCAL));
        let current = self.open.is_none();
        let open_btn: AnyElement = if current {
            open_now_tag(remote::LOCAL_COLOR).into_any_element()
        } else {
            colour_button("open-local".into(), "Open", remote::LOCAL_COLOR, cx.listener(|_, _, _, cx| cx.emit(OpenInstance(None)))).into_any_element()
        };
        let more = crate::ui::button("more-local").outline().w(px(28.)).h(px(28.)).icon(gpui_kit::component::Icon::new(IconName::Ellipsis).size(px(18.)).text_color(cx.theme().foreground)).tooltip("More").dropdown_menu({
            let this = cx.entity().downgrade();
            move |menu, _, _| {
                let t = this.clone();
                menu.item(PopupMenuItem::new("Check again").on_click(move |_, _, cx| {
                    t.update(cx, |p, cx| p.refresh_local(cx)).ok();
                }))
            }
        });
        grid_card(
            &theme,
            remote::LOCAL_COLOR,
            &det,
            "This computer".into(),
            format!("{} · localhost", det.os_name),
            current,
            (tone, state),
            quick_facts(&theme, status.as_ref(), None),
            h_flex().gap(crate::ui::button_gap()).items_center().child(open_btn).child(div().flex_1().min_w_0()).child(div().flex_shrink_0().child(more)),
        )
        .into_any_element()
    }

    fn remote_card(&self, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = inst.id.clone();
        let det = inst.detected.clone().unwrap_or(Detected { os: inst.os, ..Default::default() });
        let busy = self.job.read(cx).is_running();
        let refreshing = self.refreshing.contains(&id);
        let status = self.statuses.get(&id).cloned();
        let (mut tone, mut state) = state_of(status.as_ref(), refreshing);
        let can_work = matches!(&status, Some(Ok(s)) if s.installed) && !busy;
        let can_read = matches!(&status, Some(Ok(s)) if s.installed);
        let not_set_up = matches!(&status, Some(Err(e)) if remote::needs_setup(e));
        // An installed Platform Manager there that works the same way: the
        // card offers to use it (the server's pages offer both choices).
        let use_installed = matches!(&status, Some(Err(e)) if remote::reuse::is_offer(e))
            && remote::reuse::found(&id).is_some_and(|f| remote::reuse::fit(&f.info, &remote::reuse::AgentInfo::this()) == remote::reuse::Fit::Compatible);
        // Found and usable: say so, not "Not set up" (the older/newer note
        // is the line under it).
        if use_installed {
            if let Some(f) = remote::reuse::found(&id) {
                let v = if f.info.version.is_empty() { String::new() } else { format!(" {}", f.info.version) };
                state = format!("Platform Manager{v} found");
                tone = Tone::Neutral;
            }
        }
        let this = cx.entity().downgrade();
        let more = crate::ui::button(SharedString::from(format!("more-{id}"))).outline().w(px(28.)).h(px(28.)).icon(gpui_kit::component::Icon::new(IconName::Ellipsis).size(px(18.)).text_color(cx.theme().foreground)).tooltip("Work on this instance").dropdown_menu({
            let (this, id) = (this.clone(), id.clone());
            move |menu, _, _| {
                let item = |icon: Lucide, label: &'static str, enabled: bool, f: Box<dyn Fn(&mut InstancesPage, &mut Window, &mut Context<InstancesPage>)>| {
                    let this = this.clone();
                    PopupMenuItem::new(label).icon(icon).disabled(!enabled).on_click(move |_, window, cx| {
                        this.update(cx, |p, cx| f(p, window, cx)).ok();
                    })
                };
                let (i1, i2, i3, i4) = (id.clone(), id.clone(), id.clone(), id.clone());
                let ctl = |icon: Lucide, label: &'static str, action: &'static str, title: &'static str| {
                    let id = id.clone();
                    item(icon, label, can_work, Box::new(move |p, _, cx| {
                        p.run_job(&id, title, vec!["control", action], cx)
                    }))
                };
                menu.item(ctl(Lucide::Play, "Start SENTIENT", "start", "Starting SENTIENT"))
                    .item(ctl(Lucide::Square, "Stop SENTIENT", "stop", "Stopping SENTIENT"))
                    .item(ctl(Lucide::RotateCw, "Restart SENTIENT", "restart", "Restarting SENTIENT"))
                    .separator()
                    .item(item(Lucide::Download, "Update SENTIENT", can_work, Box::new(move |p, _, cx| p.run_job(&i1, "Updating SENTIENT", vec!["update", "--latest"], cx))))
                    .item(item(Lucide::Archive, "Back up now", can_work, Box::new(move |p, _, cx| p.run_job(&i2, "Backing up", vec!["backup-now"], cx))))
                    .item(item(Lucide::ScrollText, "Logs", can_read, Box::new(move |p, window, cx| p.show_logs(&i3, window, cx))))
                    .separator()
                    .item(item(Lucide::RefreshCw, "Check again", true, Box::new(move |p, _, cx| p.refresh(&i4, cx))))
            }
        });
        let unreachable = matches!(&status, Some(Err(e)) if !remote::needs_setup(e) && e != remote::FORGOTTEN && !e.contains("administrator rights"));
        let current = self.open.as_deref() == Some(id.as_str());
        let primary: AnyElement = if unreachable {
            crate::ui::button(SharedString::from(format!("retry-{id}"))).primary().h(px(28.)).label("Try again").loading(refreshing).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.refresh(&id, cx)
            })).into_any_element()
        } else if use_installed {
            crate::ui::button(SharedString::from(format!("use-installed-{id}"))).primary().h(px(28.)).label("Use it").disabled(busy).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.use_installed(&id, cx)
            })).into_any_element()
        } else if not_set_up {
            crate::ui::button(SharedString::from(format!("setup-{id}"))).primary().h(px(28.)).label("Set up").disabled(busy).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.set_up(&id, cx)
            })).into_any_element()
        } else if current {
            open_now_tag(&inst.color).into_any_element()
        } else {
            colour_button(SharedString::from(format!("open-{id}")), "Open", &inst.color, cx.listener({
                let id = id.clone();
                move |_, _, _, cx| cx.emit(OpenInstance(Some(id.clone())))
            })).into_any_element()
        };
        let footer = h_flex()
            .gap(crate::ui::button_gap())
            .items_center()
            .child(crate::ui::probed(primary))
            .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("edit-{id}"))).outline().h(px(28.)).px(px(10.)).text_size(px(13.)).label("Edit").disabled(busy).on_click(cx.listener({
                let inst = inst.clone();
                move |this, _, window, cx| this.open_form(Some(inst.clone()), window, cx)
            }))))
            .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("remove-{id}"))).outline().h(px(28.)).px(px(10.)).text_size(px(13.)).label("Remove").disabled(busy).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    this.confirm_remove = Some(id.clone());
                    cx.notify();
                }
            }))))
            .child(div().flex_1().min_w_0())
            .child(div().flex_shrink_0().child(more));
        let mut sub = format!("{} over SSH", inst.host);
        if !det.os_name.is_empty() {
            sub.push_str(&format!(" · {}", det.os_name));
        }
        // "none": no SENTIENT found there, nothing to name.
        if !det.install.is_empty() && !det.install.eq_ignore_ascii_case("none") {
            sub.push_str(&format!(", {}", det.install));
        }
        let mut facts = quick_facts(&theme, status.as_ref(), Some(&det));
        let following = self.job.read(cx).is_running() && self.job_for.as_ref().is_some_and(|(i, _)| *i == id);
        if let (Some(Ok(s)), false) = (&status, following) {
            if let Some(job_id) = s.running_job.clone() {
                facts = facts.child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.accent)
                        .child(div().flex_1().text_sm().child("A job is running on this server."))
                        .child(crate::ui::button(SharedString::from(format!("follow-{id}"))).outline().label("Follow").disabled(busy).on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| this.follow_job(&id, job_id.clone(), cx)
                        }))),
                );
            }
        }
        let mut c = grid_card(&theme, &inst.color, &det, inst.name.clone(), sub, current, (tone, state), facts, footer);
        if self.confirm_remove.as_deref() == Some(id.as_str()) {
            c = c.child(
                v_flex()
                    .gap_2()
                    .px_4()
                    .pb_4()
                    .child(div().text_sm().child(format!("Remove {} from this list? Nothing on the server changes.", inst.name)))
                    .child(
                        h_flex()
                            .gap(crate::ui::button_gap())
                            .child(crate::ui::probed(crate::ui::button("remove-yes").danger().label("Remove").on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| this.remove(&id, cx)
                            }))))
                            .child(crate::ui::probed(crate::ui::button("remove-no").ghost().label("Keep").on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_remove = None;
                                cx.notify();
                            })))),
                    ),
            );
        }
        c.into_any_element()
    }

    fn form_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.form.as_ref()?;
        let theme = cx.theme().clone();
        let help = |id: &'static str, text: &'static str| crate::ui::button(id).ghost().xsmall().icon(IconName::Info).tooltip(text);
        let busy = matches!(f.stage, Stage::Checking | Stage::Connecting);
        let swatches = h_flex().gap_2().items_center().children(remote::PALETTE.iter().map(|c| {
            let selected = f.color.eq_ignore_ascii_case(c);
            let hex = c.to_string();
            div()
                .id(SharedString::from(format!("swatch-{c}")))
                .size(px(24.))
                .rounded_full()
                .bg(rgb(remote::parse_hex(c).unwrap_or(0)))
                .border_2()
                .border_color(if selected { theme.primary } else { theme.background })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(f) = &mut this.form {
                        f.color = hex.clone();
                        let c = rgb(remote::parse_hex(&hex).unwrap_or(0));
                        f.color_picker.update(cx, |s, cx| s.set_value(c, window, cx));
                    }
                    cx.notify();
                }))
        }));
        // One grid for every row: label (with its help) | control, so all
        // controls share the same left and right edges.
        // The mockup's form row (K2): the label on the left, the control in a
        // right-hand column of one width; rows divided by a line.
        let row = |label: &'static str, help_id: &'static str, help_text: Option<&'static str>, control: AnyElement| {
            h_flex()
                .gap_3()
                .items_center()
                .min_h(px(40.))
                .py_1()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .items_center()
                        .child(div().text_sm().child(label))
                        .when_some(help_text, |l, t| l.child(help(help_id, t))),
                )
                .child(h_flex().w(px(290.)).flex_shrink_0().justify_end().child(div().w_full().child(control)))
        };
        let os_help = if f.os == Os::Windows {
            "Windows 10/11 or Windows Server 2019 and later, with OpenSSH Server turned on:\n1. Settings, System, Optional features, Add a feature: OpenSSH Server (or in PowerShell as administrator: Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0).\n2. Start it and start it with Windows: Start-Service sshd; Set-Service sshd -StartupType Automatic.\n3. Allow TCP port 22 in Windows Defender Firewall (the feature adds the rule).\n4. Sign in with an account in the Administrators group. For a key: put its public key in C:\\ProgramData\\ssh\\administrators_authorized_keys."
        } else {
            "Ubuntu, Debian, Red Hat, Rocky, Alma, Fedora, openSUSE and others, with an SSH server (openssh-server):\n1. Install it if missing: sudo apt install openssh-server (Debian, Ubuntu) or sudo dnf install openssh-server (Red Hat family).\n2. Start it with the system: sudo systemctl enable --now ssh (sshd on Red Hat).\n3. Allow TCP port 22 in the firewall if one is on.\n4. Sign in as root or a user that may use sudo. For a key: add its public key to ~/.ssh/authorized_keys of that user."
        };
        let found = f
            .editing
            .as_ref()
            .and_then(|id| self.instance(id))
            .and_then(|i| i.detected)
            .map(|d| format!("Found when connecting: {} {}", d.os_name, d.arch))
            .unwrap_or_else(|| "Connecting checks the system and keeps what it finds.".into());
        let mut panel = v_flex()
            .gap_2()
            .child(div().pb_1().text_sm().text_color(theme.muted_foreground).child("A computer running SENTIENT that this Platform Manager reaches over SSH."))
            .child(row("Name", "", None, Input::new(&f.name).into_any_element()))
            .child(row(
                "Colour",
                "help-colour",
                Some("The top bar takes this colour while the instance is open, so it is clear which computer you are working on. This computer is always SENTIENT teal."),
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(swatches)
                    .child(div().w(px(1.)).h(px(22.)).bg(theme.border))
                    .child(ColorPicker::new(&f.color_picker).icon(IconName::Plus).featured_colors(remote::PALETTE.iter().map(|c| rgb(remote::parse_hex(c).unwrap_or(0)).into()).collect()))
                    .into_any_element(),
            ))

            .child(v_flex().child(row(
                "System",
                "help-os",
                Some(os_help),
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        ui::seg_track(&theme)
                            .child(ui::seg_item(&theme, "os-linux", "Linux", f.os == Os::Linux).on_click(cx.listener(|this, _, _, cx| {
                                if let Some(f) = &mut this.form {
                                    f.os = Os::Linux;
                                }
                                cx.notify();
                            })))
                            .child(ui::seg_item(&theme, "os-windows", "Windows", f.os == Os::Windows).on_click(cx.listener(|this, _, _, cx| {
                                if let Some(f) = &mut this.form {
                                    f.os = Os::Windows;
                                }
                                cx.notify();
                            }))),
                    )
                    // The mockup's teal ghost button: tint on hover; tint and a
                    // thin border while the guide is open.
                    .child(
                        h_flex()
                            .id("os-guide")
                            .h(px(30.))
                            .px(px(10.))
                            .gap(px(6.))
                            .items_center()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if f.guide { theme.border } else { gpui_kit::transparent_black() })
                            .when(f.guide, |b| b.bg(theme.sidebar_accent))
                            .when(!f.guide, |b| b.hover(|b| b.bg(theme.sidebar_accent)))
                            .cursor_pointer()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.primary)
                            .child(gpui_kit::component::Icon::new(IconName::BookOpen).size(px(16.)))
                            .child("Setup guide")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_guide(cx))),
                    )
                    .into_any_element(),
            )).child(
                // What connecting found (or will check) sits inside the
                // System row, under its control.
                h_flex().justify_end().pb_1().child(div().w(px(290.)).text_size(px(13.)).text_color(theme.muted_foreground).child(found)),
            ))
            .child(row(
                "Address",
                "",
                None,
                h_flex().gap_2().items_center().child(div().flex_1().min_w_0().child(Input::new(&f.host))).child(div().text_sm().child("Port")).child(div().w(px(84.)).child(Input::new(&f.port))).into_any_element(),
            ))
            .child(row("User", "help-user", Some("An account on the server that may use sudo, or root. The Platform Manager runs its work on the server as an administrator."), Input::new(&f.user).into_any_element()))
            .child(row(
                "Sign in with",
                "",
                None,
                h_flex()
                    .child(
                        ui::seg_track(&theme)
                            .child(ui::seg_item(&theme, "auth-key", "Key file", f.auth == Auth::Key).on_click(cx.listener(|this, _, _, cx| {
                                if let Some(f) = &mut this.form {
                                    f.auth = Auth::Key;
                                }
                                cx.notify();
                            })))
                            .child(ui::seg_item(&theme, "auth-password", "Password", f.auth == Auth::Password).on_click(cx.listener(|this, _, _, cx| {
                                if let Some(f) = &mut this.form {
                                    f.auth = Auth::Password;
                                }
                                cx.notify();
                            }))),
                    )
                    .into_any_element(),
            ))
            .when(f.auth == Auth::Key, |p| {
                p.child(row(
                    "Key file",
                    "",
                    None,
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&f.key_file)))
                        .child(crate::ui::button("key-choose").outline().label("Choose…").on_click(cx.listener(|this, _, _, cx| this.choose_key(cx))))
                        .into_any_element(),
                ))
            })
            .child(row(
                if f.auth == Auth::Key { "Key passphrase" } else { "Password" },
                "help-secret",
                Some("Kept in this computer's secret store (Windows: sealed to this computer; Linux: the desktop keyring, else until the Platform Manager closes). It is never written to the instance list."),
                Input::new(&f.secret).mask_toggle().into_any_element(),
            ))
            .when(f.auth == Auth::Key && f.os == Os::Linux, |p| {
                p.child(row(
                    "sudo password",
                    "help-sudo",
                    Some("The password sudo asks for on the server, when the user is not root and sudo is not set up without one. Kept in the secret store like the passphrase."),
                    Input::new(&f.sudo_secret).mask_toggle().into_any_element(),
                ))
            })
;
        if let Some(e) = &f.error {
            panel = panel.child(ui::outcome(&theme, Tone::Bad, e.clone()));
        }
        // The mockup's K3: the trust step on its own, before anything else.
        if let Stage::Fingerprint { fingerprint, keys } = &f.stage {
            let keys = keys.clone();
            let fp = fingerprint.clone();
            let host = format!("{}{}", self.form.as_ref().map(|f| f.host.read(cx).value().to_string()).unwrap_or_default(), "");
            return Some(
                v_flex()
                    .gap_3()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Is this the right computer?"))
                    .child(ui::fact(&theme, "Address", host))
                    .child(ui::fact(&theme, "Its fingerprint", fingerprint.clone()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Compare it with the fingerprint shown on that server (ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub). It is asked once and remembered; if it ever changes, the connection is refused.",
                    ))
                    .child(
                        h_flex()
                            .mt_2()
                            .pt_3()
                            .border_t_1()
                            .border_color(theme.border)
                            .gap(crate::ui::button_gap())
                            .justify_end()
                            .child(crate::ui::probed(crate::ui::button("distrust").outline().label("Cancel").on_click(cx.listener(|this, _, _, cx| this.set_error("Not connected: the fingerprint was not accepted.", cx)))))
                            .child(crate::ui::probed(crate::ui::button("trust").primary().label("Trust and add").on_click(cx.listener(move |this, _, _, cx| this.connect(Some((keys.clone(), fp.clone())), cx))))),
                    )
                    .into_any_element(),
            );
        }
        panel = panel.child(
            h_flex()
                .mt_2()
                .pt_3()
                .border_t_1()
                .border_color(theme.border)
                .gap(crate::ui::button_gap())
                .items_center()
                .child(div().flex_1().text_sm().text_color(theme.muted_foreground).child(match f.stage {
                    Stage::Checking => "Asking the server for its key…",
                    Stage::Connecting => "Signing in and looking at the system…",
                    _ => "",
                }))
                .child(crate::ui::probed(crate::ui::button("cancel").outline().label("Cancel").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                    this.form = None;
                    cx.notify();
                }))))
                .when(!matches!(f.stage, Stage::Fingerprint { .. }), |r| {
                    r.child(crate::ui::probed(
                        crate::ui::button("check")
                            .primary()
                            .label(if f.editing.is_some() { "Save and connect" } else { "Connect" })
                            .loading(busy)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                    ))
                })
        );
        // With the guide open: the form as it is, the guide beside it at the
        // form's height, scrolling on its own.
        if !f.guide {
            return Some(panel.into_any_element());
        }
        let this = cx.entity().downgrade();
        let this2 = this.clone();
        let ctx = crate::manager::setup_guide::Ctx {
            os: f.os,
            host: f.host.read(cx).value().to_string(),
            port: f.port.read(cx).value().to_string(),
            user: f.user.read(cx).value().to_string(),
            copied: f.copied.as_ref().filter(|(_, t)| t.elapsed() < std::time::Duration::from_millis(1200)).map(|(c, _)| c.clone()),
            scroll: f.guide_scroll.clone(),
            copy: std::rc::Rc::new(move |text, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                this.update(cx, |p, cx| {
                    if let Some(f) = &mut p.form {
                        f.copied = Some((text, std::time::Instant::now()));
                    }
                    cx.notify();
                    // The tick goes after 1.2 s.
                    let t = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(1250)).await;
                        this.update(cx, |_, cx| cx.notify()).ok();
                    });
                    p._tasks.push(t);
                })
                .ok();
            }),
            close: std::rc::Rc::new(move |cx| {
                this2.update(cx, |p, cx| p.toggle_guide(cx)).ok();
            }),
        };
        let guide = crate::manager::setup_guide::pane(&ctx, cx);
        Some(
            h_flex()
                .items_stretch()
                .child(div().w(px(FORM_W)).flex_shrink_0().pr(px(20.)).child(panel))
                .child(div().relative().flex_1().min_w_0().child(div().absolute().inset_0().child(guide)))
                .into_any_element(),
        )
    }

    /// Open or close the Setup guide: the dialog widens or narrows to it.
    fn toggle_guide(&mut self, cx: &mut Context<Self>) {
        let Some(f) = &mut self.form else { return };
        f.guide = !f.guide;
        let w = if f.guide { FORM_DIALOG_W + crate::manager::setup_guide::PANE_W } else { FORM_DIALOG_W };
        crate::modal::set_width(w, cx);
        cx.notify();
    }

    /// Pick the key file with the system's file chooser.
    fn choose_key(&mut self, cx: &mut Context<Self>) {
        // Opens where the key in the field is, else in ~/.ssh.
        let now = self.form.as_ref().map(|f| f.key_file.read(cx).value().to_string()).unwrap_or_default();
        let ssh = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).map(|h| h.join(".ssh"));
        let start = crate::chooser::start_for(&expand_home(&now).display().to_string(), ssh);
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose the SSH key file", start: start.as_deref(), directory: false },
            PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use this key".into()) },
        );
        let task = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = cx.update(|cx| {
                if let Some(window) = cx.active_window() {
                    let _ = window.update(cx, |_, window, cx| {
                        this.update(cx, |p, cx| {
                            if let Some(f) = &p.form {
                                f.key_file.update(cx, |s, cx| s.set_value(path.display().to_string(), window, cx));
                            }
                            cx.notify();
                        })
                        .ok();
                    });
                }
            });
        });
        self._tasks.push(task);
    }

    /// The add/edit form and a job's progress show as dialogs over the
    /// page, opened and closed with their state.
    fn sync_dialogs(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.entity();
        if self.form.is_some() && !self.form_dialog && !self.job_dialog {
            self.form_dialog = true;
            let title = if self.form.as_ref().is_some_and(|f| f.editing.is_some()) { "Edit instance" } else { "Add a remote instance" };
            let slot = cx.new(|cx| Slot::new(page.clone(), SlotKind::Form, cx));
            let p2 = page.clone();
            crate::modal::open(
                crate::modal::Modal {
                    title: title.into(),
                    body: slot.into(),
                    width: FORM_DIALOG_W,
                    icon: None,
                    on_close: Some(std::rc::Rc::new(move |_, cx| {
                        p2.update(cx, |p, cx| {
                            p.form = None;
                            p.form_dialog = false;
                            cx.notify();
                        });
                    })),
                },
                cx,
            );
        } else if self.form.is_none() && self.form_dialog {
            self.form_dialog = false;
            crate::modal::close(cx);
        }
        if let Some((_, label)) = self.job_for.clone() {
            if !self.job_dialog && !self.form_dialog {
                self.job_dialog = true;
                let slot = cx.new(|cx| Slot::new(page.clone(), SlotKind::Job, cx));
                let p2 = page.clone();
                crate::modal::open(
                    crate::modal::Modal {
                        title: label,
                        body: slot.into(),
                        width: 560.,
                        icon: None,
                        on_close: Some(std::rc::Rc::new(move |_, cx| {
                            p2.update(cx, |p, cx| {
                                p.job_dialog = false;
                                if !p.job.read(cx).is_running() {
                                    p.job_for = None;
                                    p.job.update(cx, |j, cx| j.reset(cx));
                                }
                                cx.notify();
                            });
                        })),
                    },
                    cx,
                );
            }
        } else if self.job_dialog {
            self.job_dialog = false;
            crate::modal::close(cx);
        }
    }

    /// The job on a server, in the shared job view (crate::manager::jobview).
    fn job_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, label) = self.job_for.clone()?;
        let theme = cx.theme().clone();
        let job = self.job.read(cx);
        let inst = self.instance(&id);
        let done = !job.is_running();
        let failed = matches!(job.state(), JobState::Failed(_));
        // Secondary first, the primary at the right end.
        let mut acts: Vec<AnyElement> = vec![crate::ui::button("job-close").outline().label("Close").on_click(cx.listener(|this, _, _, cx| {
            this.job_for = None;
            this.job.update(cx, |j, cx| j.reset(cx));
            cx.notify();
        })).into_any_element()];
        if done && failed && self.retry.is_some() {
            acts.push(crate::ui::button("job-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| {
                this.job.update(cx, |j, cx| j.reset(cx));
                match this.retry.clone() {
                    Some(Retry::SetUp(id)) => this.set_up(&id, cx),
                    Some(Retry::Job(id, label, args)) => this.run_job(&id, label, args, cx),
                    None => {}
                }
            })).into_any_element());
        } else if done && !failed {
            let id = id.clone();
            acts.push(crate::ui::button("job-open").primary().label("Open").on_click(cx.listener(move |this, _, _, cx| {
                this.job_for = None;
                this.job.update(cx, |j, cx| j.reset(cx));
                cx.emit(OpenInstance(Some(id.clone())));
                cx.notify();
            })).into_any_element());
        }
        // Which server this is about: its colour dot, name and address, as on
        // the page headers.
        Some(
            v_flex()
                .gap_3()
                .when_some(inst, |p, i| {
                    p.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .child(div().size(px(10.)).rounded_full().bg(rgb(remote::parse_hex(&i.color).unwrap_or(0x187074))))
                            .child(div().font_weight(FontWeight::MEDIUM).child(i.name.clone()))
                            .child(div().text_color(theme.muted_foreground).child(i.host.clone())),
                    )
                })
                .child(crate::manager::jobview::JobView::new("inst-job", label.to_string(), &self.job).actions(acts).render(cx))
                .into_any_element(),
        )
    }
}

impl Render for InstancesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Screens and the layout test: Add instance open with its Setup
        // guide (SPM_DEBUG_ADD_GUIDE=linux|windows).
        #[cfg(debug_assertions)]
        {
            static OPENED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if let Ok(os) = std::env::var("SPM_DEBUG_ADD_GUIDE") {
                if !OPENED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    self.open_form(None, window, cx);
                    if let Some(f) = &mut self.form {
                        f.os = if os == "windows" { Os::Windows } else { Os::Linux };
                    }
                    self.sync_dialogs(window, cx);
                    self.toggle_guide(cx);
                }
            }
        }
        // Screens: the trust step open (SPM_DEBUG_TRUST).
        #[cfg(debug_assertions)]
        {
            static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if std::env::var_os("SPM_DEBUG_TRUST").is_some() && !SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.open_form(None, window, cx);
                if let Some(f) = &mut self.form {
                    f.host.update(cx, |s, cx| s.set_value("10.0.4.12", window, cx));
                    f.stage = Stage::Fingerprint { keys: String::new(), fingerprint: "SHA256:k3Vd0FqW8nE2xTz1mR4cYpL7sB9aH6uJ5gKo0Qm7x8".into() };
                }
            }
        }
        // Screens: an instance's logs dialog (SPM_DEBUG_INSTANCE_LOGS=<id>).
        #[cfg(debug_assertions)]
        {
            static LOGS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if let Ok(id) = std::env::var("SPM_DEBUG_INSTANCE_LOGS") {
                if !LOGS.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    self.show_logs(&id, window, cx);
                }
            }
        }
        self.sync_dialogs(window, cx);
        let local = self.local_card(cx);
        let cards: Vec<AnyElement> = self.list.clone().iter().map(|i| self.remote_card(i, cx)).collect();
        let all: Vec<Option<&Result<RemoteStatus, String>>> = std::iter::once(self.statuses.get(LOCAL)).chain(self.list.iter().map(|i| self.statuses.get(&i.id))).collect();
        // Named states, in this order, each only when there is one.
        let mut named: Vec<(&str, usize)> = vec![("running", 0), ("stopped", 0), ("without SENTIENT", 0), ("not set up", 0), ("not reachable", 0), ("waiting for sign-in", 0), ("being checked", 0)];
        for st in &all {
            let k = match st {
                Some(Ok(s)) if s.installed && s.serving => 0,
                Some(Ok(s)) if s.installed => 1,
                Some(Ok(_)) => 2,
                Some(Err(e)) if remote::needs_setup(e) => 3,
                Some(Err(e)) if e == remote::FORGOTTEN || e.contains("administrator rights") => 5,
                Some(Err(_)) => 4,
                None => 6,
            };
            named[k].1 += 1;
        }
        let parts: Vec<String> = named.iter().filter(|(_, c)| *c > 0).map(|(l, c)| format!("{c} {l}")).collect();
        let n = all.len();
        // The title row's width, measured: the grid takes exactly it, so the
        // button's right edge and the last tile's are one.
        let grid_w = self.grid_w.clone();
        let probe = canvas(
            move |bounds, window, _| {
                let w = f32::from(bounds.size.width);
                if (grid_w.get() - w).abs() > 0.5 {
                    grid_w.set(w);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(1.));
        let summary = format!("Every SENTIENT this program looks after. {n} instance{}: {}.", if n == 1 { "" } else { "s" }, parts.join(", "));
        v_flex()
            .id("page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p(px(crate::ui::PAGE_PAD))
            .gap(px(crate::ui::GAP))
            .bg(theme.muted)
            .child(
                h_flex()
                    .relative()
                    .child(probe)
                    .child(ui::layout_probe("header-actions"))
                    .gap_3()
                    .items_center()
                    .child(v_flex().flex_1().min_w_0().gap_0p5().child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Instances")).child(div().text_sm().text_color(theme.muted_foreground).child(summary)))
                    .when(self.form.is_none(), |r| {
                        r.child(crate::ui::button("add").primary().icon(IconName::Plus).label("Add instance").on_click(cx.listener(|this, _, window, cx| this.open_form(None, window, cx))))
                    }),
            )
            .child({
                // The cards fill the page: as many columns as fit at
                // CARD_MIN_W (one on a narrow window), stretched equally so a
                // row spans the full width with the same 12 px gaps. The width
                // is the grid's own, measured, so its right edge is the title
                // row's (an estimate from the window is used for the first
                // frame only).
                let width = f32::from(window.viewport_size().width);
                let rail = width < crate::manager::RAIL_BELOW;
                let measured = self.grid_w.get();
                let inner = if measured > 0. { measured } else { width - if rail { 64. } else { crate::manager::NAV_W } - 32. };
                let cols = ((inner + 12.) / (CARD_MIN_W + 12.)).floor().max(1.);
                // Explicit rows of `cols` tiles sharing the row equally (no
                // wrapping, so no rounding can push a tile to the next row); a
                // short last row keeps the same tile width with empty slots.
                let cols = cols as usize;
                let mut tiles: Vec<AnyElement> = std::iter::once(local).chain(cards).collect();
                let mut rows = v_flex().gap(px(crate::ui::GAP));
                while !tiles.is_empty() {
                    let rest = tiles.split_off(cols.min(tiles.len()));
                    let n = tiles.len();
                    let mut row = h_flex().w_full().gap(px(crate::ui::GAP)).items_stretch();
                    for t in tiles {
                        row = row.child(v_flex().flex_1().flex_basis(px(0.)).min_w_0().child(t));
                    }
                    for _ in n..cols {
                        row = row.child(div().flex_1().flex_basis(px(0.)).min_w_0());
                    }
                    rows = rows.child(row);
                    tiles = rest;
                }
                rows
            })
    }
}

/// The Add instance dialog's width, and its form's within it (the dialog
/// body keeps 20 px either side).
const FORM_DIALOG_W: f32 = 540.;
const FORM_W: f32 = 500.;

/// The key for this computer's status.
const LOCAL: &str = "@local";
/// The narrowest a card gets before the grid drops a column.
const CARD_MIN_W: f32 = 340.;
/// "Open" and "Open now" share one width, so Edit and Remove stay put.
const OPEN_W: f32 = 78.;

/// The state label of a card.
fn state_of(status: Option<&Result<RemoteStatus, String>>, refreshing: bool) -> (Tone, String) {
    let (t, s) = match status {
        None if refreshing => (Tone::Neutral, "Checking…"),
        None => (Tone::Neutral, "Not checked"),
        Some(Err(e)) if remote::needs_setup(e) => (Tone::Warn, "Not set up"),
        Some(Err(e)) if e == remote::FORGOTTEN => (Tone::Warn, "Sign-in needed"),
        Some(Err(e)) if e.contains("administrator rights") => (Tone::Warn, "Needs sudo"),
        Some(Err(e)) if remote::is_not_admin(e) => (Tone::Warn, "Not an administrator"),
        Some(Err(_)) => (Tone::Bad, "Not reachable"),
        Some(Ok(s)) if !s.installed => (Tone::Neutral, "No SENTIENT"),
        Some(Ok(s)) if s.serving => (Tone::Good, "Running"),
        Some(Ok(_)) => (Tone::Bad, "Stopped"),
    };
    (t, s.to_string())
}

/// The quick status: SENTIENT, database, size, disk, last backup.
fn quick_facts(theme: &gpui_kit::component::Theme, status: Option<&Result<RemoteStatus, String>>, _det: Option<&Detected>) -> Div {
    let row = |label: &'static str, value: String, warn: bool| {
        h_flex()
            .gap_0()
            .text_size(px(13.))
            .items_start()
            .child(div().w(px(84.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
            // A value that needs attention is a pill, as in the mockup; long
            // values wrap inside the card.
            .child(if warn {
                h_flex().flex_1().min_w_0().child(ui::pill(theme, Tone::Warn, value))
            } else {
                h_flex().flex_1().min_w_0().child(div().flex_1().min_w_0().child(value))
            })
    };
    let mut col = v_flex().gap(px(5.));
    match status {
        Some(Ok(s)) if s.installed => {
            let db = match s.database_bytes {
                Some(b) => format!("{}, {}", s.database, crate::dbstatus::bytes_label(b)),
                None => s.database.clone(),
            };
            col = col
                .child(row("SENTIENT", s.sentient_version.clone(), false))
                // The licence in one word, read-only from the database.
                .when_some(s.licence, |c, l| {
                    use crate::licence::TileLicence as L;
                    let tone = match l {
                        L::Active => Tone::Good,
                        L::EndsIn(_) => Tone::Warn,
                        L::Expired | L::OverLimit => Tone::Bad,
                        L::None => Tone::Neutral,
                    };
                    c.child(
                        h_flex()
                            .text_size(px(13.))
                            .items_center()
                            .child(div().w(px(84.)).flex_shrink_0().text_color(theme.muted_foreground).child("Licence"))
                            .child(h_flex().flex_1().min_w_0().child(ui::pill(theme, tone, l.label()).text_size(px(11.5)).px(px(8.)).py(px(0.)))),
                    )
                })
                .child(row("Database", db, false))
                .child(row(
                    "Disk",
                    s.disk_free.map(|d| format!("{} free", crate::dbstatus::bytes_label(d))).unwrap_or_else(|| "Not known".into()),
                    s.disk_free.is_some_and(|d| d < crate::dbstatus::LOW_DISK),
                ))
                .child(row("Last backup", s.last_backup.clone().unwrap_or_else(|| "None".into()), s.last_backup.is_none()))
                .when_some(s.uptime_secs, |c, u| c.child(row("Up for", uptime_label(u), false)));
        }
        Some(Ok(s)) => {
            col = col
                .child(row("Connection", "OK".into(), false))
                .child(row("SENTIENT", "Not found".into(), false))
                .when_some(s.disk_free, |c, d| c.child(row("Disk", format!("{} free", crate::dbstatus::bytes_label(d)), false)));
        }
        Some(Err(e)) => col = col.child(div().text_sm().text_color(if remote::needs_setup(e) || e == remote::FORGOTTEN { theme.foreground } else { theme.danger }).child(e.clone())),
        // Still reading: the rows it will show, values as placeholders.
        None => {
            for (i, label) in ["SENTIENT", "Licence", "Database", "Disk", "Last backup", "Up for"].into_iter().enumerate() {
                col = col.child(
                    h_flex()
                        .text_size(px(13.))
                        .child(div().w(px(84.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
                        .child(ui::skeleton_line(theme, format!("tile-{label}"), [70., 54., 150., 90., 110., 60.][i])),
                );
            }
        }
    }
    col
}

/// "3 days", "5 hours", "12 minutes".
fn uptime_label(secs: u64) -> String {
    let (n, unit) = if secs >= 86_400 { (secs / 86_400, "day") } else if secs >= 3_600 { (secs / 3_600, "hour") } else { (secs / 60, "minute") };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// `a` mixed into `b`: `t` of `a` (CSS color-mix in sRGB).
fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba { r: a.r * t + b.r * (1. - t), g: a.g * t + b.g * (1. - t), b: a.b * t + b.b * (1. - t), a: 1. }
}

/// Dark or white text on a colour, by its luminance.
fn text_on(c: Rgba) -> Hsla {
    let l = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    if l > 0.6 { rgb(0x1c2326).into() } else { rgb(0xffffff).into() }
}

/// The Open button in the instance's own colour (the mockup's option C).
fn colour_button(id: SharedString, label: &'static str, color: &str, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    let c = rgb(remote::parse_hex(color).unwrap_or(0x187074));
    h_flex()
        .id(id)
        .min_w(px(OPEN_W))
        .justify_center()
        .h(px(28.))
        .px(px(11.))
        .rounded(px(8.))
        .bg(c)
        .text_color(text_on(c))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .items_center()
        .cursor_pointer()
        .hover(|d| d.opacity(0.9))
        .child(label)
        .on_click(on_click)
}

/// One instance as a tile (the mockup's option C): one rounded box with a
/// thin muted border; a header band tinted with the instance colour (logo
/// tile in the full colour, name, address and system, state), then a body in
/// a fainter tint with the facts and the actions. Each part rounds its own
/// corners, so no colour reaches past the box's corners at any scale. The
/// open instance's tile has a 2px border in its own colour.
#[allow(clippy::too_many_arguments)]
fn grid_card(
    theme: &gpui_kit::component::Theme,
    color: &str,
    det: &Detected,
    name: String,
    sub: String,
    current: bool,
    (tone, state): (Tone, String),
    facts: Div,
    footer: Div,
) -> Div {
    let dark = theme.mode.is_dark();
    let c = rgb(remote::parse_hex(color).unwrap_or(0x187074));
    let card = theme.background.to_rgb();
    let head = mix(c, card, crate::theme::bar_tint(dark));
    // The body a third of the header's tint or less, close to the plain
    // card, so a tile reads as a coloured header over a card (dark was
    // 0.13 of 0.18: one solid block, his rc12-23 note). tint.py holds the
    // body to half the header's colour difference in both themes.
    let body = mix(c, card, if dark { 0.055 } else { 0.07 });
    if cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_LAYOUT").is_some() {
        // The plain card colour, for tint.py's comparison.
        eprintln!("LAYOUT inst-card-surface {:.0} {:.0} {:.0}", card.r * 255., card.g * 255., card.b * 255.);
    }
    let fg = theme.foreground.to_rgb();
    // The state keeps its meaning (green, red, amber) but in the tile's
    // softer form; a neutral state takes the instance colour.
    let pill = match tone {
        Tone::Neutral => {
            let mut bg = c;
            bg.a = 0.30;
            h_flex()
                .px(px(8.))
                .py(px(2.))
                .rounded_full()
                .bg(bg)
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(mix(c, fg, 0.35))
                .whitespace_nowrap()
                .child(state)
        }
        _ => ui::pill(theme, tone, state).text_size(px(11.5)).px(px(8.)).py(px(2.)),
    };
    let border_w = if current { 2. } else { 1. };
    let inner = ui::PANEL_RADIUS - border_w;
    v_flex()
        .relative()
        .w_full()
        .flex_1()
        .rounded(px(ui::PANEL_RADIUS))
        .bg(body)
        .child(ui::layout_probe("inst-card"))
        .border(px(border_w))
        .border_color(if current { Hsla::from(c) } else { theme.background.blend(theme.border) })
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., if dark { 0.35 } else { 0.10 }), offset: point(px(0.), px(1.)), blur_radius: px(3.), spread_radius: px(0.), inset: false }])
        .child(
            h_flex()
                .relative()
                .gap(px(9.))
                .items_center()
                .px(px(12.))
                .py(px(9.))
                .bg(head)
                .rounded_t(px(inner))
                .child(ui::layout_probe("inst-card-head"))
                .child(os_tile_in(det, 26., c))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(px(14.5)).font_weight(FontWeight::SEMIBOLD).whitespace_nowrap().overflow_hidden().text_ellipsis().child(name))
                        .child(div().text_size(px(11.5)).text_color(theme.muted_foreground).whitespace_nowrap().overflow_hidden().text_ellipsis().child(sub)),
                )
                .child(pill),
        )
        .child(
            v_flex()
                .flex_1()
                .gap(px(9.))
                .px(px(12.))
                .pt(px(10.))
                .pb(px(12.))
                .rounded_b(px(inner))
                .child(facts.flex_1())
                .child(footer),
        )
}

/// On the open instance's card, in place of its Open button: the same
/// coloured button, not clickable (it is open already).
fn open_now_tag(color: &str) -> Div {
    let c = rgb(remote::parse_hex(color).unwrap_or(0x187074));
    h_flex()
        .min_w(px(OPEN_W))
        .justify_center()
        .items_center()
        .px(px(10.))
        .h(px(28.))
        .flex_shrink_0()
        .rounded(px(8.))
        .bg(c)
        .text_color(text_on(c))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .child("Open now")
}

/// The system's logo, white, on a tile in the instance's colour.
fn os_tile_in(d: &Detected, size: f32, c: Rgba) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(7.))
        .bg(c)
        .flex()
        .items_center()
        .justify_center()
        .child(ui::brand_mark(os_brand(d).0, text_on(c), size * 0.62))
}

fn os_brand(d: &Detected) -> (&'static str, u32, bool) {
    match (d.os, d.distro.as_str()) {
        (Os::Windows, _) => ("windows", 0x0078d4, false),
        (_, "ubuntu") => ("ubuntu", 0xe95420, false),
        (_, "debian") => ("debian", 0xa81d33, false),
        (_, "rhel") => ("redhat", 0xee0000, false),
        (_, "fedora") => ("fedora", 0x51a2da, false),
        (_, "centos") => ("centos", 0x262577, false),
        (_, "rocky") => ("rockylinux", 0x10b981, false),
        (_, "almalinux") => ("almalinux", 0x0f4266, false),
        (_, "arch") => ("archlinux", 0x1793d1, false),
        (_, id) if id.starts_with("opensuse") => ("opensuse", 0x73ba25, false),
        (_, "raspbian") => ("raspberrypi", 0xa22846, false),
        _ => ("linux", 0xfcc624, true),
    }
}


fn default_key() -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(std::path::PathBuf::from).unwrap_or_default();
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn expand_home(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(std::path::PathBuf::from).unwrap_or_default().join(rest),
        None => std::path::PathBuf::from(path),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum SlotKind {
    Form,
    Job,
}

/// A dialog's content: one part of the Instances page, drawn with the page's
/// own state and handlers, redrawn whenever the page changes.
struct Slot {
    page: Entity<InstancesPage>,
    kind: SlotKind,
    _sub: Subscription,
}

impl Slot {
    fn new(page: Entity<InstancesPage>, kind: SlotKind, cx: &mut Context<Self>) -> Self {
        let _sub = cx.observe(&page, |_, _, cx| cx.notify());
        Self { page, kind, _sub }
    }
}

impl Render for Slot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        let body = self.page.update(cx, |p, cx| match kind {
            SlotKind::Form => p.form_panel(cx),
            SlotKind::Job => p.job_panel(cx),
        });
        let _ = window;
        div().id("slot").children(body)
    }
}

/// "#rrggbb" of a colour.
fn hex_of(c: Hsla) -> String {
    let r: Rgba = c.into();
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(r.r), b(r.g), b(r.b))
}

