// Copyright © 2016-2026 The SENTIENT Authors

//! Instances: this computer and SENTIENT on other computers (crate::remote).
//! One card each, with its state and the everyday actions; adding a server
//! checks its key fingerprint before anything is sent to it.

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
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
}

pub struct InstancesPage {
    local: Option<InstallationRecord>,
    list: Vec<Instance>,
    statuses: HashMap<String, Result<RemoteStatus, String>>,
    /// Read again once the refresh under way ends.
    refresh_again: HashSet<String>,
    refreshing: HashSet<String>,
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
    /// (instance id, text) of the log view.
    logs: Option<(String, SharedString)>,
    confirm_remove: Option<String>,
    /// The instance open in the whole Platform Manager (None: this computer).
    pub open: Option<String>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OpenInstance> for InstancesPage {}
impl EventEmitter<ListChanged> for InstancesPage {}

impl InstancesPage {
    pub fn new(local: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
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
            refresh_again: HashSet::new(),
            form: None,
            job,
            job_for: None,
            retry: None,
            show_details: false,
            form_dialog: false,
            job_dialog: false,
            logs: None,
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
        });
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
        self.logs = None;
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    remote::deploy(&inst, &sink)?;
                    sink(sentient_installer_core::progress::Progress::Step { name: label.to_string() });
                    let job_id = remote::start_job(&inst, &args)?;
                    remote::follow(&inst, &job_id, &sink)
                },
                cx,
            )
        });
        cx.notify();
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
            job.start(
                move |sink| {
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
        self.job.update(cx, |job, cx| job.start(move |sink| remote::follow(&inst, &job_id, &sink), cx));
        cx.notify();
    }

    fn show_logs(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(inst) = self.instance(id) else { return };
        let id = id.to_string();
        self.logs = Some((id.clone(), "Reading the log…".into()));
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { remote::logs(&inst, 400) }).await;
            this.update(cx, |page, cx| {
                if page.logs.as_ref().is_some_and(|(i, _)| *i == id) {
                    page.logs = Some((id, ui::strip_ansi(&got.unwrap_or_else(|e| e)).into()));
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    // -- rendering ----------------------------------------------------------

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
            open_now_tag(&theme).into_any_element()
        } else {
            Button::new("open-local").primary().label("Open").on_click(cx.listener(|_, _, _, cx| cx.emit(OpenInstance(None)))).into_any_element()
        };
        let more = Button::new("more-local").ghost().small().icon(IconName::Ellipsis).tooltip("More").dropdown_menu({
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
            h_flex().gap_2().items_center().child(open_btn).child(div().flex_1()).child(more),
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
        let (tone, state) = state_of(status.as_ref(), refreshing);
        let can_work = matches!(&status, Some(Ok(s)) if s.installed) && !busy;
        let can_read = matches!(&status, Some(Ok(s)) if s.installed);
        let not_set_up = matches!(&status, Some(Err(e)) if e == remote::NOT_SET_UP);
        let this = cx.entity().downgrade();
        let more = Button::new(SharedString::from(format!("more-{id}"))).ghost().small().icon(IconName::Ellipsis).tooltip("Work on this instance").dropdown_menu({
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
                    item(icon, label, can_work, Box::new(move |p, _, cx| p.run_job(&id, title, vec!["control", action], cx)))
                };
                menu.item(ctl(Lucide::Play, "Start SENTIENT", "start", "Starting SENTIENT"))
                    .item(ctl(Lucide::Square, "Stop SENTIENT", "stop", "Stopping SENTIENT"))
                    .item(ctl(Lucide::RotateCw, "Restart SENTIENT", "restart", "Restarting SENTIENT"))
                    .separator()
                    .item(item(Lucide::Download, "Update SENTIENT", can_work, Box::new(move |p, _, cx| p.run_job(&i1, "Updating SENTIENT", vec!["update", "--latest"], cx))))
                    .item(item(Lucide::Archive, "Back up now", can_work, Box::new(move |p, _, cx| p.run_job(&i2, "Backing up", vec!["backup-now"], cx))))
                    .item(item(Lucide::ScrollText, "Logs", can_read, Box::new(move |p, _, cx| p.show_logs(&i3, cx))))
                    .separator()
                    .item(item(Lucide::RefreshCw, "Check again", true, Box::new(move |p, _, cx| p.refresh(&i4, cx))))
            }
        });
        let unreachable = matches!(&status, Some(Err(e)) if e != remote::NOT_SET_UP && e != remote::FORGOTTEN && !e.contains("administrator rights"));
        let current = self.open.as_deref() == Some(id.as_str());
        let primary: AnyElement = if unreachable {
            Button::new(SharedString::from(format!("retry-{id}"))).primary().label("Try again").loading(refreshing).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.refresh(&id, cx)
            })).into_any_element()
        } else if not_set_up {
            Button::new(SharedString::from(format!("setup-{id}"))).primary().label("Set up").disabled(busy).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.set_up(&id, cx)
            })).into_any_element()
        } else if current {
            open_now_tag(&theme).into_any_element()
        } else {
            Button::new(SharedString::from(format!("open-{id}"))).primary().label("Open").on_click(cx.listener({
                let id = id.clone();
                move |_, _, _, cx| cx.emit(OpenInstance(Some(id.clone())))
            })).into_any_element()
        };
        let footer = h_flex()
            .gap_2()
            .items_center()
            .child(primary)
            .child(Button::new(SharedString::from(format!("edit-{id}"))).outline().label("Edit").disabled(busy).on_click(cx.listener({
                let inst = inst.clone();
                move |this, _, window, cx| this.open_form(Some(inst.clone()), window, cx)
            })))
            .child(Button::new(SharedString::from(format!("remove-{id}"))).outline().label("Remove").disabled(busy).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    this.confirm_remove = Some(id.clone());
                    cx.notify();
                }
            })))
            .child(div().flex_1())
            .child(more);
        let mut sub = format!("{} over SSH", inst.host);
        if !det.os_name.is_empty() {
            sub.push_str(&format!(" · {}", det.os_name));
        }
        if !det.install.is_empty() {
            sub.push_str(&format!(", {}", det.install));
        }
        let mut facts = quick_facts(&theme, status.as_ref(), Some(&det));
        let following = self.job.read(cx).is_running() && self.job_for.as_ref().is_some_and(|(i, _)| *i == id);
        if let (Some(Ok(s)), false) = (&status, following) {
            if let Some(job_id) = s.running_job.clone() {
                facts = facts.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.accent)
                        .child(div().flex_1().text_sm().child("A job is running on this server."))
                        .child(Button::new(SharedString::from(format!("follow-{id}"))).outline().label("Follow").disabled(busy).on_click(cx.listener({
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
                            .gap_2()
                            .child(Button::new("remove-yes").danger().label("Remove").on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| this.remove(&id, cx)
                            })))
                            .child(Button::new("remove-no").ghost().label("Keep").on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_remove = None;
                                cx.notify();
                            }))),
                    ),
            );
        }
        c.into_any_element()
    }

    fn form_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.form.as_ref()?;
        let theme = cx.theme().clone();
        let help = |id: &'static str, text: &'static str| Button::new(id).ghost().xsmall().icon(IconName::Info).tooltip(text);
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
        let row = |label: &'static str, help_id: &'static str, help_text: Option<&'static str>, control: AnyElement| {
            h_flex()
                .gap_3()
                .items_center()
                .min_h(px(36.))
                .child(
                    h_flex()
                        .w(px(150.))
                        .flex_shrink_0()
                        .gap_1()
                        .items_center()
                        .child(div().text_sm().child(label))
                        .when_some(help_text, |l, t| l.child(help(help_id, t))),
                )
                .child(div().flex_1().min_w_0().child(control))
        };
        let note = |text: String| h_flex().child(div().w(px(150.)).flex_shrink_0()).child(div().pl_3().text_size(px(13.)).text_color(theme.muted_foreground).child(text));
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
                "Address",
                "",
                None,
                h_flex().gap_2().items_center().child(div().flex_1().min_w_0().child(Input::new(&f.host))).child(div().text_sm().child("Port")).child(div().w(px(84.)).child(Input::new(&f.port))).into_any_element(),
            ))
            .child(row(
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
                    .child(Button::new("os-guide").ghost().small().label("Setup guide").icon(IconName::ExternalLink).on_click({
                        let anchor = if f.os == Os::Windows { "windows-servers" } else { "linux-servers" };
                        move |_, _, cx| ui::open_url(&format!("https://sentient.invenia.in/docs/installation/remote-instances/#{anchor}"), cx)
                    }))
                    .into_any_element(),
            ))
            .child(note(found))
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
                        .child(Button::new("key-choose").outline().label("Choose…").on_click(cx.listener(|this, _, _, cx| this.choose_key(cx))))
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
            .child(row(
                "Colour",
                "help-colour",
                Some("The top bar takes this colour while the instance is open, so it is clear which computer you are working on. This computer is always SENTIENT teal."),
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(swatches)
                    .child(div().w(px(1.)).h(px(22.)).bg(theme.border))
                    .child(ColorPicker::new(&f.color_picker).featured_colors(remote::PALETTE.iter().map(|c| rgb(remote::parse_hex(c).unwrap_or(0)).into()).collect()))
                    .into_any_element(),
            ));
        if let Some(e) = &f.error {
            panel = panel.child(ui::outcome(&theme, Tone::Bad, e.clone()));
        }
        if let Stage::Fingerprint { fingerprint, keys } = &f.stage {
            let keys = keys.clone();
            let fp = fingerprint.clone();
            panel = panel.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.warning)
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Is this the server's key?"))
                    .child(div().text_sm().font_family(theme.mono_font_family.clone()).child(fingerprint.clone()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Compare it with what the server shows for: ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub. After you accept it, a different key is refused.",
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("trust").primary().label("It matches: connect").on_click(cx.listener(move |this, _, _, cx| this.connect(Some((keys.clone(), fp.clone())), cx))))
                            .child(Button::new("distrust").ghost().label("It does not match").on_click(cx.listener(|this, _, _, cx| this.set_error("Not connected: the key was not accepted.", cx)))),
                    ),
            );
        }
        panel = panel.child(
            h_flex()
                .mt_2()
                .pt_3()
                .border_t_1()
                .border_color(theme.border)
                .gap_2()
                .items_center()
                .child(div().flex_1().text_sm().text_color(theme.muted_foreground).child(match f.stage {
                    Stage::Checking => "Asking the server for its key…",
                    Stage::Connecting => "Signing in and looking at the system…",
                    _ => "",
                }))
                .child(Button::new("cancel").outline().label("Cancel").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                    this.form = None;
                    cx.notify();
                })))
                .when(!matches!(f.stage, Stage::Fingerprint { .. }), |r| {
                    r.child(
                        Button::new("check")
                            .primary()
                            .label(if f.editing.is_some() { "Save and connect" } else { "Connect" })
                            .loading(busy)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                    )
                })
        );
        Some(panel.into_any_element())
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
                    width: 540.,
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

    fn job_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, label) = self.job_for.clone()?;
        let theme = cx.theme().clone();
        let job = self.job.read(cx);
        let log: Vec<SharedString> = job.log().iter().rev().take(14).rev().cloned().collect();
        let inst = self.instance(&id);
        // Which server this is about: its colour dot, name and address, as on
        // the page headers.
        let mut p = v_flex().gap_3().when_some(inst.clone(), |p, i| {
            p.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(div().size(px(10.)).rounded_full().bg(rgb(remote::parse_hex(&i.color).unwrap_or(0x187074))))
                    .child(div().font_weight(FontWeight::MEDIUM).child(i.name.clone()))
                    .child(div().text_color(theme.muted_foreground).child(i.host.clone())),
            )
        });
        let _ = &label;
        p = match job.state() {
            JobState::Running => p.child(div().text_sm().child(job.step_with_time())),
            JobState::Done(m) => p.child(ui::outcome(&theme, Tone::Good, m.clone())),
            JobState::Failed(m) => {
                let (headline, details) = match m.split_once(remote::DETAILS) {
                    Some((h, d)) => (h.to_string(), Some(d.to_string())),
                    None => (m.to_string(), None),
                };
                let shown = self.show_details;
                p.child(ui::outcome(&theme, Tone::Bad, headline)).when_some(details, |p, d| {
                    p.child(Button::new("job-details").ghost().label(if shown { "Hide details" } else { "Show details" }).on_click(cx.listener(|this, _, _, cx| {
                        this.show_details = !this.show_details;
                        cx.notify();
                    })))
                    .when(shown, |p| p.child(div().p_2().rounded(theme.radius).bg(theme.muted).text_size(px(13.)).font_family(theme.mono_font_family.clone()).child(d)))
                })
            }
            JobState::Idle => p,
        };
        let done = !job.is_running();
        let failed = matches!(job.state(), JobState::Failed(_));
        Some(
            p.when(!log.is_empty(), |p| {
                p.child(
                    v_flex()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_size(px(13.))
                        .font_family(theme.mono_font_family.clone())
                        .children(log.into_iter().map(|l| div().child(l))),
                )
            })
            .when(done, |p| {
                p.child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .when(failed && self.retry.is_some(), |r| {
                            r.child(Button::new("job-retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| {
                                this.job.update(cx, |j, cx| j.reset(cx));
                                match this.retry.clone() {
                                    Some(Retry::SetUp(id)) => this.set_up(&id, cx),
                                    Some(Retry::Job(id, label, args)) => this.run_job(&id, label, args, cx),
                                    None => {}
                                }
                            })))
                        })
                        .when(!failed, |r| {
                            let id = id.clone();
                            r.child(Button::new("job-open").primary().label("Open").on_click(cx.listener(move |this, _, _, cx| {
                                this.job_for = None;
                                this.job.update(cx, |j, cx| j.reset(cx));
                                cx.emit(OpenInstance(Some(id.clone())));
                                cx.notify();
                            })))
                        })
                        .child(Button::new("job-close").outline().label("Close").on_click(cx.listener(|this, _, _, cx| {
                            this.job_for = None;
                            this.job.update(cx, |j, cx| j.reset(cx));
                            cx.notify();
                        }))),
                )
            })
            .into_any_element(),
        )
    }

    fn logs_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, text) = self.logs.clone()?;
        let theme = cx.theme().clone();
        let name = self.instance(&id).map(|i| i.name).unwrap_or_default();
        Some(
            ui::panel(&theme)
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(format!("SENTIENT log on {name}")))
                        .child(Button::new("logs-again").ghost().small().icon(IconName::RefreshCw).tooltip("Read again").on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| this.show_logs(&id, cx)
                        })))
                        .child(Button::new("logs-close").ghost().label("Close").on_click(cx.listener(|this, _, _, cx| {
                            this.logs = None;
                            cx.notify();
                        }))),
                )
                .child(
                    div()
                        .id("remote-log")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_size(px(13.))
                        .font_family(theme.mono_font_family.clone())
                        .children(text.lines().map(|l| div().child(SharedString::from(l.to_string()))).collect::<Vec<_>>()),
                )
                .into_any_element(),
        )
    }
}

impl Render for InstancesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        self.sync_dialogs(window, cx);
        let local = self.local_card(cx);
        let cards: Vec<AnyElement> = self.list.clone().iter().map(|i| self.remote_card(i, cx)).collect();
        let logs = self.logs_panel(cx);
        let all: Vec<Option<&Result<RemoteStatus, String>>> = std::iter::once(self.statuses.get(LOCAL)).chain(self.list.iter().map(|i| self.statuses.get(&i.id))).collect();
        // Named states, in this order, each only when there is one.
        let mut named: Vec<(&str, usize)> = vec![("running", 0), ("stopped", 0), ("without SENTIENT", 0), ("not set up", 0), ("not reachable", 0), ("waiting for sign-in", 0), ("being checked", 0)];
        for st in &all {
            let k = match st {
                Some(Ok(s)) if s.installed && s.serving => 0,
                Some(Ok(s)) if s.installed => 1,
                Some(Ok(_)) => 2,
                Some(Err(e)) if e == remote::NOT_SET_UP => 3,
                Some(Err(e)) if e == remote::FORGOTTEN || e.contains("administrator rights") => 5,
                Some(Err(_)) => 4,
                None => 6,
            };
            named[k].1 += 1;
        }
        let parts: Vec<String> = named.iter().filter(|(_, c)| *c > 0).map(|(l, c)| format!("{c} {l}")).collect();
        let n = all.len();
        let summary = format!("Every SENTIENT this program looks after. {n} instance{}: {}.", if n == 1 { "" } else { "s" }, parts.join(", "));
        v_flex()
            .id("page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_8()
            .gap_4()
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(v_flex().flex_1().min_w_0().gap_1().child(div().text_2xl().child("Instances")).child(div().text_color(theme.muted_foreground).child(summary)))
                    .when(self.form.is_none(), |r| {
                        r.child(Button::new("add").primary().icon(IconName::Plus).label("Add a remote instance").on_click(cx.listener(|this, _, window, cx| this.open_form(None, window, cx))))
                    }),
            )
            .children(logs)
            .child({
                // The cards fill the page: as many columns as fit (one on
                // narrow windows), all the same width.
                let width = f32::from(window.viewport_size().width);
                let rail = width < crate::manager::RAIL_BELOW;
                let inner = width - if rail { 64. } else { 224. } - 64.;
                let cols = if rail { 1. } else { ((inner + 16.) / (CARD_MIN_W + 16.)).floor().max(1.) };
                let card_w = ((inner - 16. * (cols - 1.)) / cols).floor();
                h_flex()
                    .flex_wrap()
                    .gap_4()
                    .items_stretch()
                    .children(std::iter::once(local).chain(cards).map(|c| v_flex().w(px(card_w)).flex_shrink_0().child(c)))
            })
    }
}

/// The key for this computer's status.
const LOCAL: &str = "@local";
/// The narrowest a card gets before the grid drops a column.
const CARD_MIN_W: f32 = 320.;
/// Cards in a row stretch to the tallest; none is shorter than this.
const CARD_MIN_H: f32 = 268.;

/// The state label of a card.
fn state_of(status: Option<&Result<RemoteStatus, String>>, refreshing: bool) -> (Tone, String) {
    let (t, s) = match status {
        None if refreshing => (Tone::Neutral, "Checking…"),
        None => (Tone::Neutral, "Not checked"),
        Some(Err(e)) if e == remote::NOT_SET_UP => (Tone::Warn, "Not set up"),
        Some(Err(e)) if e == remote::FORGOTTEN => (Tone::Warn, "Sign-in needed"),
        Some(Err(e)) if e.contains("administrator rights") => (Tone::Warn, "Needs sudo"),
        Some(Err(_)) => (Tone::Bad, "Not reachable"),
        Some(Ok(s)) if !s.installed => (Tone::Neutral, "No SENTIENT"),
        Some(Ok(s)) if s.serving => (Tone::Good, "Running"),
        Some(Ok(_)) => (Tone::Bad, "Stopped"),
    };
    (t, s.to_string())
}

/// The quick status: SENTIENT, database, size, disk, last backup.
fn quick_facts(theme: &gpui_kit::component::Theme, status: Option<&Result<RemoteStatus, String>>, det: Option<&Detected>) -> Div {
    let row = |label: &'static str, value: String, warn: bool| {
        h_flex()
            .gap_3()
            .text_sm()
            .child(div().w(px(96.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
            .child(div().flex_1().min_w_0().text_ellipsis().when(warn, |d| d.text_color(theme.warning_foreground)).child(value))
    };
    let mut col = v_flex().gap_1();
    match status {
        Some(Ok(s)) if s.installed => {
            let db = match s.database_bytes {
                Some(b) => format!("{}, {}", s.database, crate::dbstatus::bytes_label(b)),
                None => s.database.clone(),
            };
            col = col
                .child(row("SENTIENT", s.sentient_version.clone(), false))
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
        Some(Err(e)) => col = col.child(div().text_sm().text_color(if e == remote::NOT_SET_UP || e == remote::FORGOTTEN { theme.foreground } else { theme.danger }).child(e.clone())),
        None => {
            col = col.child(div().text_sm().text_color(theme.muted_foreground).child(match det {
                Some(d) if !d.arch.is_empty() => format!("{} · {}", d.arch, if d.install.is_empty() { "install not found".to_string() } else { d.install.clone() }),
                _ => "Reading its state…".to_string(),
            }))
        }
    }
    col
}

/// "3 days", "5 hours", "12 minutes".
fn uptime_label(secs: u64) -> String {
    let (n, unit) = if secs >= 86_400 { (secs / 86_400, "day") } else if secs >= 3_600 { (secs / 3_600, "hour") } else { (secs / 60, "minute") };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// One instance as a card: its colour along the top, logo, name, state,
/// the quick status and the actions.
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
    // One rounded box. Its own background is the instance colour, so the
    // colour band's top corners are the card's corners (GPUI does not clip
    // children to a radius); the white body sits below the band and rounds
    // only its bottom corners, inside the border. The open card changes only
    // the border's colour: same width, same box.
    let inner = ui::PANEL_RADIUS - 1.;
    v_flex()
        .w_full()
        .flex_1()
        .min_h(px(CARD_MIN_H))
        .rounded(px(ui::PANEL_RADIUS))
        .bg(rgb(remote::parse_hex(color).unwrap_or(0x187074)))
        .border_1()
        // Opaque, so the colour under the border does not show through it.
        .border_color(if current { theme.primary } else { theme.background.blend(theme.border) })
        .pt(px(5.))
        .child(
            v_flex()
                .flex_1()
                .gap_3()
                .p_4()
                .bg(theme.background)
                .rounded_b(px(inner))
                .child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(os_tile(det, 36.))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).text_ellipsis().child(name))
                                .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(sub)),
                        )
                        .child(ui::pill(theme, tone, state)),
                )
                .child(facts.flex_1())
                .child(div().h(px(1.)).bg(theme.border))
                .child(footer),
        )
}

/// On the open instance's card, in place of its Open button.
fn open_now_tag(theme: &gpui_kit::component::Theme) -> Div {
    h_flex()
        .gap_1p5()
        .items_center()
        .px_2()
        .h(px(32.))
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(gpui_kit::component::Icon::new(IconName::Check).size_4().text_color(theme.success))
        .child("Open now")
}

/// The system's logo on a tile.
pub fn os_tile(d: &Detected, size: f32) -> impl IntoElement {
    let (name, color, dark_tile) = match (d.os, d.distro.as_str()) {
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
    };
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(8.))
        .bg(if dark_tile { rgb(0x1f1f1f) } else { rgba(color << 8 | 0x1f) })
        .flex()
        .items_center()
        .justify_center()
        .child(ui::brand_mark(name, rgb(color).into(), size * 0.6))
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

