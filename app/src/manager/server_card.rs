// Copyright © 2016-2026 The SENTIENT Authors

//! The Server card on Statistics (the approved mockup): beside Services,
//! the machine in a few lines (what it is, processor and memory with
//! bars, the disk with SENTIENT's data and any nearly full one, how long
//! it has run, updates), amber Attention lines only when something needs a
//! look, and its actions in the header (terminal, Restart server on a
//! server, the three-dot menu). All details opens everything in a dialog.
//!
//! The reading is crate::server_info (`--job server-info-json` on a
//! server). Restart server, Shut down server, Restart Docker and Install
//! system updates are jobs in the shared job view, their steps declared up
//! front (crate::jobplan). Restart server drives its steps from here: the
//! server is down for most of them; meanwhile the top bar says the server
//! is restarting instead of that it does not answer, and the checker
//! raises no alarm ([`power_state`]); one shut down from here shows as off.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::diagnose::{Diagnosis, Fix};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::dbstatus::bytes_label;
use crate::server_info::{Disk, ServerInfo};
use crate::ui::{self, Tone};

// ---------------------------------------------------------------------------
// Power state: the top bar's and the checker's view while a server restarts,
// shuts down, or is off
// ---------------------------------------------------------------------------

/// A server restarted or shut down from here (for this run, never on disk).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Power {
    /// The restart job runs: not answering is expected (reconnecting).
    Restarting,
    /// The shut down job runs.
    ShuttingDown,
    /// Shut down from here: off, not an error, until it answers again.
    Off,
}

static POWER: Mutex<Option<HashMap<String, Power>>> = Mutex::new(None);

/// What was done to that server's power from here, if anything.
pub fn power_state(id: &str) -> Option<Power> {
    POWER.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(id).copied()))
}

fn set_power(id: &str, p: Option<Power>) {
    if let Ok(mut g) = POWER.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        match p {
            Some(p) => {
                m.insert(id.to_string(), p);
            }
            None => {
                m.remove(id);
            }
        }
    }
}

/// The server answered: one shut down from here is on again. A restart or
/// shut down in progress keeps its state (it answers until it goes down).
pub fn answered(id: &str) {
    if power_state(id) == Some(Power::Off) {
        set_power(id, None);
    }
}

/// Clears Restarting / ShuttingDown however the job ends; Off stays.
struct PowerFlag(String);

impl PowerFlag {
    fn new(id: &str, p: Power) -> Self {
        set_power(id, Some(p));
        Self(id.to_string())
    }
}

impl Drop for PowerFlag {
    fn drop(&mut self) {
        if matches!(power_state(&self.0), Some(Power::Restarting | Power::ShuttingDown)) {
            set_power(&self.0, None);
        }
    }
}

// ---------------------------------------------------------------------------
// The terminal: one entry point (the built-in terminal fills it)
// ---------------------------------------------------------------------------

/// The built-in terminal is there: the header's button is enabled.
pub const TERMINAL_READY: bool = true;

/// Open a terminal on `inst` (None: a local shell on this computer), the
/// mockup's "Terminal: <name>" dialog (crate::terminal). The Server card's
/// "Open terminal" button calls only this.
pub fn open_terminal(inst: Option<&crate::remote::Instance>, window: &mut Window, cx: &mut App) {
    crate::terminal::open(inst, window, cx);
}

// ---------------------------------------------------------------------------
// Reading a server
// ---------------------------------------------------------------------------

/// The server's reading (one agent call).
pub fn read_remote(inst: &crate::remote::Instance) -> Result<ServerInfo, String> {
    #[cfg(debug_assertions)]
    if let Some(i) = crate::server_info::debug_info() {
        return Ok(i);
    }
    let out = crate::remote::agent_job(inst, "server-info-json")?;
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or_else(|| "The server's Platform Manager answered without its details.".to_string())?;
    serde_json::from_str(line).map_err(|e| format!("unexpected answer from the server: {e}"))
}

// ---------------------------------------------------------------------------
// Restart and shut down: the steps, against a host that can be faked
// ---------------------------------------------------------------------------

/// What the restart job asks of the server.
pub trait PowerHost: Send + Sync {
    /// Reach it and bring its agent up to date.
    fn prepare(&self, sink: &ProgressFn) -> Result<(), String>;
    /// Stop SENTIENT cleanly and send the restart (or power off): its start
    /// time before.
    fn send(&self, reboot: bool, sink: &ProgressFn) -> Result<i64, String>;
    /// When it started; Err while it does not answer.
    fn boot(&self) -> Result<i64, String>;
    /// SENTIENT and its database run (true when nothing is installed).
    fn running(&self) -> Result<bool, String>;
    /// SENTIENT answers on its web address.
    fn answers(&self) -> Result<bool, String>;
    /// Wait (the fake one only counts).
    fn wait(&self, d: Duration);
    /// Time since the job started.
    fn elapsed(&self) -> Duration;
}

const DOWN_WITHIN: Duration = Duration::from_secs(180);
const BACK_WITHIN: Duration = Duration::from_secs(600);
const RUNNING_WITHIN: Duration = Duration::from_secs(300);
const ANSWERS_WITHIN: Duration = Duration::from_secs(180);

fn mins(d: Duration) -> String {
    let s = d.as_secs();
    match (s / 60, s % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}

/// A failure with its cause and what to do next (crate::diagnosis shows it).
fn fail(sink: &ProgressFn, id: &str, cause: &str, why: &str, next: &str) -> String {
    let d = Diagnosis { id: id.into(), cause: cause.into(), why: why.into(), fixes: vec![Fix::Steps { text: next.into() }], last_lines: Vec::new(), details: format!("{cause}\n{why}\n{next}") };
    sink(Progress::Log { line: d.encode() });
    crate::remote::failed(cause, next)
}

/// Wait for `ok` until `within` has passed since `from`; a log line each
/// half minute.
fn wait_for(host: &dyn PowerHost, sink: &ProgressFn, every: Duration, within: Duration, what: &str, mut ok: impl FnMut() -> bool) -> bool {
    let from = host.elapsed();
    let mut said = Duration::ZERO;
    loop {
        if ok() {
            return true;
        }
        let gone = host.elapsed().saturating_sub(from);
        if gone >= within {
            return false;
        }
        if gone >= said + Duration::from_secs(30) {
            said = gone;
            sink(Progress::Log { line: format!("{what}: {} so far, gives up after {}", mins(gone), mins(within)) });
        }
        host.wait(every);
    }
}

/// Restart the server and follow it until SENTIENT answers again.
pub fn restart_flow(host: &dyn PowerHost, name: &str, sink: &ProgressFn) -> Result<(), String> {
    use crate::jobplan as jp;
    // The agent brought up to date and the restart sent: one step (the
    // mockup's six steps, all shown up front).
    sink(Progress::Step { name: jp::STEP_RESTART_SENT.into() });
    host.prepare(sink)?;
    // Its start time before, read on its own; the one the restart job
    // answers with is the same reading, taken just before it sends.
    let asked = host.boot().ok();
    let sent = host.send(true, sink)?;
    let before = asked.unwrap_or(sent);
    sink(Progress::Step { name: jp::STEP_WENT_DOWN.into() });
    let mut early: Option<i64> = None;
    let down = wait_for(host, sink, Duration::from_secs(2), DOWN_WITHIN, "Waiting for the server to go down", || match host.boot() {
        Err(_) => true,
        // Back already with a new start time: the drop was missed.
        Ok(b) if b != before => {
            early = Some(b);
            true
        }
        Ok(_) => false,
    });
    if !down {
        return Err(fail(sink, "server-not-down", &format!("{name} did not go down within {}.", mins(DOWN_WITHIN)), "The restart was sent, but the server kept answering with the same start time.", "Look on the server whether a program holds the restart back (an open session asking to save, Windows installing updates), or restart it from its console."));
    }
    sink(Progress::Step { name: jp::STEP_COMING_BACK.into() });
    let mut boot = early;
    let back = wait_for(host, sink, Duration::from_secs(5), BACK_WITHIN, "Reconnecting", || {
        if boot.is_none() {
            boot = host.boot().ok();
        }
        boot.is_some()
    });
    let Some(boot) = boot.filter(|_| back) else {
        return Err(fail(sink, "server-not-back", &format!("{name} did not come back within {}.", mins(BACK_WITHIN)), "It went down for the restart and has not answered since.", "Look at its console (the machine's screen, or the cloud provider's or hypervisor's console): it may wait at a boot prompt, a disk check or an update. Once it answers, SENTIENT starts by itself."));
    };
    sink(Progress::Step { name: jp::STEP_RESTARTED.into() });
    if boot == before {
        return Err(fail(sink, "server-not-restarted", &format!("{name} answers again, but it did not restart."), "Its start time is the same as before: only the connection dropped.", "Restart it again; if it never restarts, look on the server whether the restart is blocked (systemctl status, or Windows' event log)."));
    }
    sink(Progress::Log { line: format!("Started {}", chrono::DateTime::from_timestamp(boot, 0).map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()) });
    sink(Progress::Step { name: jp::STEP_RUNNING.into() });
    if !wait_for(host, sink, Duration::from_secs(5), RUNNING_WITHIN, "Waiting for SENTIENT and its database", || host.running().unwrap_or(false)) {
        return Err(fail(sink, "server-sentient-not-running", &format!("SENTIENT or its database did not start within {} after the restart.", mins(RUNNING_WITHIN)), "The server is back, but its services are not all running.", "Open Status: it shows which one is stopped and its log; Start starts it."));
    }
    sink(Progress::Step { name: jp::STEP_ANSWERS.into() });
    if !wait_for(host, sink, Duration::from_secs(5), ANSWERS_WITHIN, "Waiting for SENTIENT to answer", || host.answers().unwrap_or(false)) {
        return Err(fail(sink, "server-sentient-not-answering", &format!("SENTIENT runs but did not answer within {}.", mins(ANSWERS_WITHIN)), "Its service is up, its web address does not answer yet.", "Open Logs: SENTIENT's log says what it waits for (often the database)."));
    }
    sink(Progress::Done { message: format!("{name} restarted; SENTIENT and its database run again.") });
    Ok(())
}

/// Shut the server down: done once it stops answering.
pub fn shutdown_flow(host: &dyn PowerHost, name: &str, sink: &ProgressFn) -> Result<(), String> {
    sink(Progress::Step { name: crate::jobplan::STEP_SHUTDOWN_SENT.into() });
    host.prepare(sink)?;
    host.send(false, sink)?;
    sink(Progress::Step { name: crate::jobplan::STEP_WENT_DOWN.into() });
    if !wait_for(host, sink, Duration::from_secs(2), DOWN_WITHIN, "Waiting for the server to go down", || host.boot().is_err()) {
        return Err(fail(sink, "server-not-down", &format!("{name} did not go down within {}.", mins(DOWN_WITHIN)), "The shutdown was sent, but the server still answers.", "Look on the server whether a program holds the shutdown back, or shut it down from its console."));
    }
    sink(Progress::Done { message: format!("{name} is off. Platform Manager cannot switch it on again: someone has to, or use the cloud provider's console.") });
    Ok(())
}

/// The real server, through its agent.
struct RemoteHost {
    inst: crate::remote::Instance,
    start: std::time::Instant,
}

fn boot_of(out: &str) -> Option<i64> {
    out.lines().rev().find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok()?.get("boot")?.as_i64())
}

impl PowerHost for RemoteHost {
    fn prepare(&self, sink: &ProgressFn) -> Result<(), String> {
        if let Err(e) = crate::remote::deploy(&self.inst, sink) {
            sink(Progress::Log { line: format!("The agent could not be updated: {}", crate::remote::split_failure(&e).0) });
        }
        Ok(())
    }

    fn send(&self, reboot: bool, sink: &ProgressFn) -> Result<i64, String> {
        let out = crate::remote::agent_job(&self.inst, if reboot { "server-power reboot" } else { "server-power poweroff" })?;
        for l in out.lines().filter(|l| !l.trim_start().starts_with('{') && !l.trim().is_empty()) {
            sink(Progress::Log { line: l.to_string() });
        }
        boot_of(&out).ok_or_else(|| crate::remote::failed("The server did not confirm the restart was sent.", out.trim()))
    }

    fn boot(&self) -> Result<i64, String> {
        crate::remote::agent_job(&self.inst, "server-boot-json").and_then(|o| boot_of(&o).ok_or_else(|| "no start time".to_string()))
    }

    fn running(&self) -> Result<bool, String> {
        match crate::remote::services(&self.inst) {
            Ok(list) => Ok(list.iter().all(|s| s.running)),
            Err(e) if e == crate::remote::NOT_ADDED => Ok(true),
            Err(e) => Err(e),
        }
    }

    fn answers(&self) -> Result<bool, String> {
        match crate::remote::status(&self.inst) {
            Ok(s) => Ok(!s.installed || s.serving),
            Err(e) if e == crate::remote::NOT_ADDED => Ok(true),
            Err(e) => Err(e),
        }
    }

    fn wait(&self, d: Duration) {
        std::thread::sleep(d);
    }

    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

/// A server that is not there (screens, tests): `back_after` seconds down,
/// SENTIENT up `running_after` seconds after that; each wait takes
/// `real` of its length.
pub struct FakeHost {
    pub back_after: Option<u64>,
    pub running_after: u64,
    pub new_boot: bool,
    pub real: f64,
    t: Mutex<Duration>,
    sent_at: Mutex<Option<Duration>>,
}

impl FakeHost {
    pub fn new(back_after: Option<u64>, running_after: u64, new_boot: bool, real: f64) -> Self {
        Self { back_after, running_after, new_boot, real, t: Mutex::new(Duration::ZERO), sent_at: Mutex::new(None) }
    }

    fn since_sent(&self) -> Option<Duration> {
        let t = *self.t.lock().unwrap();
        self.sent_at.lock().unwrap().map(|s| t.saturating_sub(s))
    }
}

impl PowerHost for FakeHost {
    fn prepare(&self, _: &ProgressFn) -> Result<(), String> {
        Ok(())
    }
    fn send(&self, _: bool, sink: &ProgressFn) -> Result<i64, String> {
        sink(Progress::Log { line: "Stopping SENTIENT and its database cleanly".into() });
        *self.sent_at.lock().unwrap() = Some(*self.t.lock().unwrap());
        Ok(1_000)
    }
    fn boot(&self) -> Result<i64, String> {
        match (self.since_sent(), self.back_after) {
            (Some(s), _) if s < Duration::from_secs(6) => Ok(1_000),
            (Some(s), Some(b)) if s >= Duration::from_secs(6 + b) => Ok(if self.new_boot { 2_000 } else { 1_000 }),
            (None, _) => Ok(1_000),
            _ => Err("ssh: connect to host 192.0.2.10 port 22: Connection refused".into()),
        }
    }
    fn running(&self) -> Result<bool, String> {
        Ok(self.since_sent().zip(self.back_after).is_some_and(|(s, b)| s >= Duration::from_secs(6 + b + self.running_after)))
    }
    fn answers(&self) -> Result<bool, String> {
        self.running()
    }
    fn wait(&self, d: Duration) {
        *self.t.lock().unwrap() += d;
        if self.real > 0. {
            std::thread::sleep(d.mul_f64(self.real));
        }
    }
    fn elapsed(&self) -> Duration {
        *self.t.lock().unwrap()
    }
}

// ---------------------------------------------------------------------------
// The jobs, started from the card
// ---------------------------------------------------------------------------

/// Which of the card's jobs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Restart,
    Shutdown,
    Docker,
    Updates,
}

impl Action {
    pub fn title(self, name: &str) -> String {
        match self {
            Action::Restart => format!("Restarting {name}"),
            Action::Shutdown => format!("Shutting down {name}"),
            Action::Docker => format!("Restarting Docker on {name}"),
            Action::Updates => format!("Installing system updates on {name}"),
        }
    }
}

/// Start `action` in `job` (on `inst`, or this computer). `windows`: the
/// machine runs Windows.
pub fn start(job: &Entity<crate::jobs::Job>, action: Action, inst: Option<crate::remote::Instance>, windows: bool, retry: Rc<dyn Fn(&mut App)>, cx: &mut App) {
    let name = inst.as_ref().map(|i| i.name.clone()).unwrap_or_else(|| "this computer".into());
    let docker_sentient = match &inst {
        Some(i) => i.detected.as_ref().is_none_or(|d| d.install.eq_ignore_ascii_case("docker")),
        None => crate::installation::InstallationRecord::load().is_some_and(|r| r.engine == crate::installation::Engine::Docker),
    };
    let mut plan = match action {
        Action::Restart => crate::jobplan::server_restart(windows),
        Action::Shutdown => crate::jobplan::server_shutdown(windows),
        Action::Docker => crate::jobplan::docker_restart(docker_sentient),
        Action::Updates => crate::jobplan::system_updates(),
    };
    if inst.is_some() && matches!(action, Action::Docker | Action::Updates) {
        plan.insert(0, crate::jobplan::connect());
    }
    let label = action.title(&name);
    job.update(cx, |j, cx| {
        j.set_plan(plan);
        j.set_label(label.clone());
        j.set_on_page(true);
        if let Some(i) = &inst {
            j.set_instance(i.id.clone(), i.name.clone(), i.color.clone());
        }
        j.set_retry(retry);
        j.start(
            move |sink| {
                // Screens: a server that is not there (SPM_DEBUG_SERVER_RESTART
                // = ok | waiting | never | same-boot).
                #[cfg(debug_assertions)]
                if let Ok(v) = std::env::var("SPM_DEBUG_SERVER_RESTART") {
                    let host = match v.as_str() {
                        "waiting" => FakeHost::new(Some(100_000), 10, true, 0.2),
                        "never" => FakeHost::new(None, 0, true, 0.002),
                        "same-boot" => FakeHost::new(Some(20), 0, false, 0.05),
                        _ => FakeHost::new(Some(70), 30, true, 0.05),
                    };
                    let id = inst.as_ref().map(|i| i.id.clone()).unwrap_or_default();
                    return match action {
                        Action::Shutdown => {
                            let _flag = PowerFlag::new(&id, Power::ShuttingDown);
                            let r = shutdown_flow(&host, &name, &sink);
                            if r.is_ok() {
                                set_power(&id, Some(Power::Off));
                            }
                            r
                        }
                        _ => {
                            let _flag = PowerFlag::new(&id, Power::Restarting);
                            restart_flow(&host, &name, &sink)
                        }
                    };
                }
                match (action, inst) {
                    (Action::Restart, Some(inst)) => {
                        let _flag = PowerFlag::new(&inst.id, Power::Restarting);
                        restart_flow(&RemoteHost { inst, start: std::time::Instant::now() }, &name, &sink)
                    }
                    (Action::Shutdown, Some(inst)) => {
                        let id = inst.id.clone();
                        let _flag = PowerFlag::new(&id, Power::ShuttingDown);
                        let r = shutdown_flow(&RemoteHost { inst, start: std::time::Instant::now() }, &name, &sink);
                        // Off, not an error, until it answers again.
                        if r.is_ok() {
                            set_power(&id, Some(Power::Off));
                        }
                        r
                    }
                    (Action::Restart | Action::Shutdown, None) => Err("This computer is restarted or shut down from its own system.".into()),
                    (a, Some(inst)) => {
                        sink(Progress::Step { name: crate::jobplan::CONNECT.into() });
                        if let Err(e) = crate::remote::deploy(&inst, &sink) {
                            sink(Progress::Log { line: format!("The agent could not be updated: {}", crate::remote::split_failure(&e).0) });
                        }
                        let args = [if a == Action::Docker { "docker-restart" } else { "system-updates" }];
                        crate::remote::start_job(&inst, &args).and_then(|id| crate::remote::follow(&inst, &id, &sink))
                    }
                    (a, None) => {
                        let job = if a == Action::Docker { "docker-restart" } else { "system-updates" };
                        if cfg!(target_os = "linux") && !crate::rootjob::is_root() {
                            crate::rootjob::run(&[job.to_string()], &sink).map(|_| ())
                        } else {
                            crate::server_ops::job(job, &[job.to_string()], &sink).unwrap_or(Ok(()))
                        }
                    }
                }
            },
            cx,
        )
    });
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn gib(b: u64) -> f32 {
    b as f32 / (1u64 << 30) as f32
}

/// "1 GB", "0.6 GB", "64 GB".
fn gb_label(b: u64) -> String {
    let g = gib(b);
    if g >= 10. || (g - g.round()).abs() < 0.05 { format!("{:.0} GB", g.round()) } else { format!("{g:.1} GB") }
}

/// "41 days", "5 hours", "12 minutes".
pub fn running_for(secs: i64) -> String {
    let s = secs.max(0);
    let (n, unit) = if s >= 2 * 86_400 {
        (s / 86_400, "days")
    } else if s >= 86_400 {
        (1, "day")
    } else if s >= 7200 {
        (s / 3600, "hours")
    } else if s >= 3600 {
        (1, "hour")
    } else {
        ((s / 60).max(1), if s / 60 == 1 { "minute" } else { "minutes" })
    };
    format!("{n} {unit}")
}

fn machine_line(i: &ServerInfo) -> (String, String) {
    if i.machine.is_virtual {
        (match &i.machine.hypervisor {
            Some(h) if h.ends_with("container") => "Container".to_string(),
            _ => "Virtual machine".to_string(),
        }, i.machine.hypervisor.clone().map(|h| h.trim_end_matches(" container").to_string()).unwrap_or_default())
    } else {
        ("Physical machine".into(), i.machine.model.clone().unwrap_or_default())
    }
}

fn warn_color(theme: &gpui_kit::component::Theme) -> Hsla {
    if theme.mode.is_dark() { rgb(0xffb74d).into() } else { rgb(0xb26a00).into() }
}

/// The card's label column.
const LABEL_W: f32 = 118.;

/// A label and its value, a line between rows (none above the first).
fn kv_row(theme: &gpui_kit::component::Theme, first: bool, label: impl Into<SharedString>, label_color: Option<Hsla>, value: AnyElement) -> Div {
    h_flex()
        .items_start()
        .gap_0()
        .when(!first, |r| r.border_t_1().border_color(theme.border).pt(px(9.)))
        .pb(px(9.))
        .text_sm()
        .child(div().w(px(LABEL_W)).flex_shrink_0().text_color(label_color.unwrap_or(theme.muted_foreground)).child(label.into()))
        .child(div().flex_1().min_w_0().child(value))
}

/// Longer than this many characters, a line may be cut: it says its whole
/// text in a tooltip.
const TIP_FROM: usize = 22;

/// One line: the value and a softer note after it. Never wraps; cut with
/// "…" when too long, the whole text on hover.
fn line(theme: &gpui_kit::component::Theme, id: impl Into<SharedString>, value: impl Into<SharedString>, note: impl Into<SharedString>, color: Option<Hsla>) -> Stateful<Div> {
    spans(theme, id, value, note, color, Vec::new())
}

/// A line with spans: value, then the note, each its own element (so a
/// note can sit beside a tag). Never wraps; cut with "…", the whole text
/// in a tooltip.
fn spans(theme: &gpui_kit::component::Theme, id: impl Into<SharedString>, value: impl Into<SharedString>, note: impl Into<SharedString>, color: Option<Hsla>, extra: Vec<AnyElement>) -> Stateful<Div> {
    let value: SharedString = value.into();
    let note: SharedString = note.into();
    let full = full_text(&value, &note);
    let long = full.chars().count() > TIP_FROM;
    let full: SharedString = full.into();
    let id: SharedString = id.into();
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .gap(px(6.))
        .items_center()
        .overflow_hidden()
        .child(div().flex_shrink(0.05).min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().when_some(color, |d, c| d.text_color(c)).child(value))
        .when(!note.is_empty(), |d| d.child(div().flex_shrink(1.).min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(13.)).text_color(theme.muted_foreground).child(note)))
        .children(extra)
        .when(long, |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)))
}

/// A line's whole text, for its tooltip.
fn full_text(value: &str, note: &str) -> String {
    if note.is_empty() { value.to_string() } else { format!("{value}, {note}") }
}

fn bar(theme: &gpui_kit::component::Theme, f: f32, warn: bool) -> Div {
    let c = if warn { warn_color(theme) } else { theme.primary };
    div().w_full().child(ui::segment_bar(theme.secondary, 6., &[(c, f.clamp(0., 1.))]))
}

/// A tag ("SENTIENT data").
fn tag(theme: &gpui_kit::component::Theme, text: &'static str) -> AnyElement {
    div().flex_shrink_0().px(px(8.)).rounded(px(10.)).text_size(px(12.)).line_height(px(20.)).bg(theme.primary.opacity(0.12)).text_color(theme.primary).whitespace_nowrap().child(text).into_any_element()
}

fn health_pill(theme: &gpui_kit::component::Theme, d: &Disk) -> Option<AnyElement> {
    let p = match d.health() {
        crate::server_info::Health::Warning => ui::pill(theme, Tone::Bad, "Health warning"),
        crate::server_info::Health::Worn(w) => ui::pill(theme, Tone::Warn, format!("{w} % worn")),
        crate::server_info::Health::Healthy => ui::pill(theme, Tone::Good, "Healthy"),
        crate::server_info::Health::NotKnown => return None,
    };
    Some(p.flex_shrink_0().text_size(px(12.)).into_any_element())
}

/// A disk: name and type, its bar and the free space (or "Not mounted",
/// greyed, with its size). `id`: unique among the disks shown.
fn disk_block(theme: &gpui_kit::component::Theme, id: &str, d: &Disk, details: bool) -> Div {
    let mut extra = Vec::new();
    if details && d.sentient {
        extra.push(tag(theme, "SENTIENT data"));
    }
    if details {
        extra.extend(health_pill(theme, d));
    }
    let kind = d.kind.clone().unwrap_or_default();
    if !d.mounted() {
        return v_flex()
            .gap(px(4.))
            .min_w_0()
            .child(spans(theme, format!("{id}-name"), d.name.clone(), kind, Some(theme.muted_foreground), extra))
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).whitespace_nowrap().child(format!("Not mounted, {}", bytes_label(d.size))));
    }
    v_flex()
        .gap(px(4.))
        .min_w_0()
        .child(spans(theme, format!("{id}-name"), d.name.clone(), kind, None, extra))
        .child(bar(theme, d.used_fraction(), d.nearly_full()))
        .child(div().text_size(px(13.)).text_color(if d.nearly_full() { warn_color(theme) } else { theme.muted_foreground }).whitespace_nowrap().overflow_hidden().text_ellipsis().child(format!("{} free of {}", bytes_label(d.free.unwrap_or(0)), bytes_label(d.size))))
}

/// The card's actions.
pub struct Actions {
    /// A server (Restart server, Shut down server); false: this computer.
    pub remote: bool,
    pub busy: bool,
    pub on_terminal: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_restart: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_shutdown: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_docker: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_updates: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_details: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_update_agent: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// The three-dot menu's entries, in the mockup's order (Restart Docker
/// only where Docker is installed; Shut down server only on a server).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuEntry {
    Docker,
    Updates,
    Divider,
    Shutdown,
}

pub fn menu_entries(remote: bool, docker: bool) -> Vec<MenuEntry> {
    let mut v = Vec::new();
    if docker {
        v.push(MenuEntry::Docker);
    }
    v.push(MenuEntry::Updates);
    if remote {
        v.push(MenuEntry::Divider);
        v.push(MenuEntry::Shutdown);
    }
    v
}

/// The header: "Server", then the icon buttons (Open terminal, Restart
/// server on a server) and the three-dot menu, each with its tooltip.
fn header(docker: bool, a: &Actions) -> Div {
    let open = a.on_terminal.clone();
    let terminal = div()
        .id("srv-terminal-tip")
        .child(ui::button("srv-terminal").outline().w(px(28.)).h(px(28.)).rounded(px(8.)).icon(Icon::new(Lucide::SquareTerminal).size(px(16.))).disabled(!TERMINAL_READY).on_click(move |_, w, cx| open(w, cx)))
        .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Open terminal").build(w, cx));
    let items: Vec<ui::RowMenuItem> = menu_entries(a.remote, docker)
        .into_iter()
        .map(|e| match e {
            MenuEntry::Docker => {
                let f = a.on_docker.clone();
                ui::RowMenuItem::Item { label: "Restart Docker".into(), icon: Some(IconName::Redo), action: Rc::new(move |w, cx| f(w, cx)) }
            }
            MenuEntry::Updates => {
                let f = a.on_updates.clone();
                ui::RowMenuItem::Item { label: "Install system updates".into(), icon: Some(IconName::ArrowDown), action: Rc::new(move |w, cx| f(w, cx)) }
            }
            MenuEntry::Divider => ui::RowMenuItem::Divider,
            MenuEntry::Shutdown => {
                let f = a.on_shutdown.clone();
                ui::RowMenuItem::Danger { label: "Shut down server".into(), icon: IconName::CircleX, action: Rc::new(move |w, cx| f(w, cx)) }
            }
        })
        .collect();
    let restart = a.on_restart.clone();
    h_flex()
        .gap(px(10.))
        .items_center()
        .mb(px(12.))
        .child(div().flex_shrink_0().font_weight(FontWeight::MEDIUM).child("Server"))
        .child(div().flex_1())
        .child(
            h_flex()
                .flex_shrink_0()
                .gap(ui::button_gap())
                .items_center()
                .child(ui::probed(terminal))
                .when(a.remote, |r| {
                    r.child(ui::probed(ui::button("srv-restart").outline().w(px(28.)).h(px(28.)).rounded(px(8.)).icon(Icon::new(Lucide::RotateCw).size(px(16.))).tooltip("Restart server").disabled(a.busy).on_click(move |_, w, cx| restart(w, cx))))
                })
                .child(ui::probed(ui::row_menu("srv-menu", "More actions", a.busy, items))),
        )
}

/// "1 update waiting", "12 updates waiting", "restart needed" (the card's
/// note after Running for); None: nothing waiting.
pub fn updates_note(updates: Option<u32>, restart_needed: bool) -> Option<String> {
    match updates {
        Some(1) => Some("1 update waiting".to_string()),
        Some(n) if n > 1 => Some(format!("{n} updates waiting")),
        _ => restart_needed.then(|| "restart needed".to_string()),
    }
}

/// The compact card. `info`: None while it is read; `live`: the latest
/// live reading (processor and memory bars).
pub fn card(theme: &gpui_kit::component::Theme, info: Option<&Result<ServerInfo, String>>, live: Option<&crate::stats::Sample>, a: Actions) -> Div {
    let mut c = ui::panel(theme).gap_0().min_w_0().flex().flex_col();
    let info = match info {
        None => {
            return c.child(header(false, &a)).child(v_flex().gap(px(14.)).children((0..5).map(|i| h_flex().gap_3().child(div().w(px(LABEL_W - 12.)).child(ui::skeleton_line(theme, format!("srv-l-{i}"), 70.))).child(div().flex_1().child(ui::skeleton_line(theme, format!("srv-v-{i}"), 150.))))));
        }
        Some(Err(e)) => {
            let old = e == crate::remote::AGENT_OLD;
            let upd = a.on_update_agent.clone();
            return c.child(header(false, &a)).child(
                v_flex()
                    .gap_3()
                    .child(div().text_sm().text_color(theme.muted_foreground).child(if old { "The agent on this server is out of date: update it to see the server's details.".to_string() } else { format!("The server's details could not be read. {}", crate::remote::split_failure(e).0) }))
                    .when(old, |d| d.child(h_flex().justify_end().child(ui::probed(ui::button("srv-agent-update").outline().label("Update agent").disabled(a.busy).on_click(move |_, w, cx| upd(w, cx)))))),
            );
        }
        Some(Ok(i)) => i,
    };
    c = c.child(header(info.docker.is_some(), &a));
    let warn = warn_color(theme);
    let mut rows: Vec<Div> = Vec::new();
    let mut first = true;
    let mut push = |label: SharedString, color: Option<Hsla>, value: AnyElement, rows: &mut Vec<Div>| {
        rows.push(kv_row(theme, first, label, color, value));
        first = false;
    };
    // "Virtual machine" and, small, the system ("Ubuntu 22.04").
    let (m, _) = machine_line(info);
    push("Machine".into(), None, line(theme, "srv-machine", m, info.os.clone(), None).into_any_element(), &mut rows);
    let cpu = live.map(|s| s.cpu).unwrap_or(0.);
    let cores = if info.cpu.cores == 1 { "1 core".to_string() } else { format!("{} cores", info.cpu.cores) };
    push(
        "Processor".into(),
        None,
        v_flex().gap(px(4.)).min_w_0().child(line(theme, "srv-cpu", info.cpu.model.clone(), if live.is_some() { format!("{cores}, {cpu:.0} %") } else { cores }, None)).child(bar(theme, cpu / 100., cpu >= 90.)).into_any_element(),
        &mut rows,
    );
    let (used, total) = live.map(|s| (s.mem_used, s.mem_total)).unwrap_or((0, info.mem_total));
    let mem_f = if total > 0 { used as f32 / total as f32 } else { 0. };
    let swap = if info.swap_total > 0 { format!("+ {} swap", gb_label(info.swap_total)) } else { String::new() };
    push(
        "Memory".into(),
        None,
        v_flex().gap(px(4.)).min_w_0().child(line(theme, "srv-mem", if live.is_some() { format!("{:.1} of {}", gib(used), gb_label(total)) } else { gb_label(total) }, swap, None)).child(bar(theme, mem_f, mem_f >= 0.9)).into_any_element(),
        &mut rows,
    );
    if let Some(d) = info.data_disk() {
        push("Data disk".into(), None, disk_block(theme, "srv-disk-data", d, false).into_any_element(), &mut rows);
    }
    for (k, d) in info.nearly_full_others().into_iter().enumerate() {
        push("Nearly full".into(), Some(warn), disk_block(theme, &format!("srv-disk-full-{k}"), d, false).into_any_element(), &mut rows);
    }
    let up = chrono::Utc::now().timestamp() - info.boot;
    // The card says the count, or that a restart is needed; All details both.
    push("Running for".into(), None, line(theme, "srv-up", running_for(up), updates_note(info.updates, info.restart_needed).unwrap_or_default(), None).into_any_element(), &mut rows);
    // Amber, only when something needs a look.
    for (k, l) in info.card_attention().into_iter().enumerate() {
        push(if k == 0 { "Attention".into() } else { "".into() }, Some(warn), line(theme, format!("srv-att-{k}"), l, "", Some(warn)).into_any_element(), &mut rows);
    }
    let details = a.on_details.clone();
    c.child(v_flex().flex_1().min_w_0().children(rows)).child(h_flex().justify_end().mt(px(14.)).child(ui::probed(ui::button("srv-details").outline().label("All details").on_click(move |_, w, cx| details(w, cx)))))
}

// ---------------------------------------------------------------------------
// All details
// ---------------------------------------------------------------------------

pub struct Details {
    info: ServerInfo,
    scroll: ScrollHandle,
}

/// Open All details for `name` in the shared dialog (only its content scrolls).
pub fn open_details(name: &str, info: ServerInfo, cx: &mut App) {
    let view = cx.new(|_| Details { info, scroll: ScrollHandle::new() });
    let footer = cx.new(|_| CloseFooter);
    let mut shown = name.to_string();
    if let Some(c) = shown.get(..1) {
        shown = format!("{}{}", c.to_uppercase(), &shown[1..]);
    }
    crate::modal::open(crate::modal::Modal { title: format!("Server details: {shown}").into(), body: view.into(), width: 900., icon: Some(Icon::empty().path("icons/server.svg")), on_close: None }, cx);
    crate::modal::set_footer(footer.into(), cx);
    crate::modal::set_list_body(cx);
    crate::modal::offer_full_screen(cx);
}

struct CloseFooter;

impl Render for CloseFooter {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex().justify_end().child(ui::probed(ui::button("srv-details-close").outline().label("Close").on_click(|_, _, cx| crate::modal::close(cx))))
    }
}

/// The Clock row's word and its state: green when synchronised and under a
/// second off, amber when not synchronised or more than a second off.
pub fn clock_word(c: &crate::server_info::Clock) -> (&'static str, Tone) {
    match c.synced {
        Some(false) => ("Not synced", Tone::Warn),
        _ if c.out_of_sync() => ("Out of sync", Tone::Warn),
        Some(true) => ("Synced", Tone::Good),
        None => ("Not known", Tone::Neutral),
    }
}

fn section(theme: &gpui_kit::component::Theme, title: &'static str, rows: Vec<Div>) -> Div {
    v_flex()
        .min_w_0()
        .flex_1()
        .flex_basis(px(0.))
        .child(div().pb(px(8.)).text_size(px(12.)).text_color(theme.muted_foreground).child(title.to_uppercase()))
        .children(rows)
}

impl Render for Details {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let i = &self.info;
        let warn = warn_color(&theme);
        let ok: Hsla = if theme.mode.is_dark() { rgb(0x81c784).into() } else { rgb(0x2e7d32).into() };
        let mut hw: Vec<Div> = Vec::new();
        let mut n = 0usize;
        let mut add = |rows: &mut Vec<Div>, label: &'static str, v: AnyElement| {
            rows.push(kv_row(&theme, rows.is_empty(), label, None, v));
            n += 1;
        };
        let (m, note) = machine_line(i);
        add(&mut hw, "Machine", line(&theme, "sd-machine", m, note, None).into_any_element());
        let mut cpu_note = vec![if i.cpu.cores == 1 { "1 core".to_string() } else { format!("{} cores", i.cpu.cores) }];
        if i.cpu.threads > i.cpu.cores {
            cpu_note.push(format!("{} threads", i.cpu.threads));
        }
        if let Some(t) = i.cpu.temp_c {
            cpu_note.push(format!("{t:.0} °C"));
        }
        add(&mut hw, "Processor", line(&theme, "sd-cpu", i.cpu.model.clone(), cpu_note.join(", "), None).into_any_element());
        let swap = if i.swap_total > 0 { format!("+ {} swap, {} used", gb_label(i.swap_total), gb_label(i.swap_used)) } else { "no swap".into() };
        add(&mut hw, "Memory", line(&theme, "sd-mem", gb_label(i.mem_total), swap, if i.heavy_swap() { Some(warn) } else { None }).into_any_element());
        if i.gpus.is_empty() {
            add(&mut hw, "Graphics", line(&theme, "sd-gpu-none", "None", "", None).into_any_element());
        } else {
            let list = v_flex().gap(px(4.)).children(i.gpus.iter().enumerate().map(|(k, g)| line(&theme, format!("sd-gpu-{k}"), g.name.clone(), g.mem.map(gb_label).unwrap_or_default(), None)));
            add(&mut hw, "Graphics", list.into_any_element());
        }
        let disks = v_flex().gap(px(12.)).min_w_0().children(i.disks.iter().enumerate().map(|(k, d)| disk_block(&theme, &format!("sd-disk-{k}"), d, true)));
        add(&mut hw, "Disks", disks.into_any_element());

        let mut sys: Vec<Div> = Vec::new();
        let mut add2 = |label: &'static str, v: AnyElement| {
            sys.push(kv_row(&theme, sys.is_empty(), label, None, v));
        };
        let kernel = i.kernel.clone().map(|k| if i.windows() { k } else { format!("kernel {k}") }).unwrap_or_default();
        add2("System", line(&theme, "sd-os", i.os.clone(), kernel, None).into_any_element());
        let up = chrono::Utc::now().timestamp() - i.boot;
        add2(
            "Running for",
            line(&theme, "sd-up", running_for(up), match i.clean_restart {
                Some(true) => "after a clean restart",
                Some(false) => "after an unclean stop",
                None => "",
            }, None)
            .into_any_element(),
        );
        let mut clock_note = Vec::new();
        if let Some(o) = i.clock.offset {
            clock_note.push(format!("{} off", crate::server_info::secs_label(o.abs())));
        }
        if let Some(z) = &i.clock.zone {
            clock_note.push(z.clone());
        }
        let (word, state) = clock_word(&i.clock);
        let color = match state {
            Tone::Good => ok,
            Tone::Warn | Tone::Bad => warn,
            Tone::Neutral => theme.muted_foreground,
        };
        add2("Clock", spans(&theme, "sd-clock", word, clock_note.join(", "), Some(color), Vec::new()).into_any_element());
        let nets = if i.nets.is_empty() {
            line(&theme, "sd-net-none", "None", "", None).into_any_element()
        } else {
            v_flex()
                .gap(px(4.))
                .children(i.nets.iter().enumerate().map(|(k, n)| {
                    let speed = n.speed.map(|s| if s >= 1000 && s % 1000 == 0 { format!("{} Gbit/s", s / 1000) } else { format!("{s} Mbit/s") }).unwrap_or_default();
                    line(&theme, format!("sd-net-{k}"), format!("{} {}", n.name, n.ip.clone().unwrap_or_default()), speed, None)
                }))
                .into_any_element()
        };
        add2("Network", nets);
        let upd = match (i.updates, i.restart_needed) {
            (Some(0), false) => line(&theme, "sd-upd", "None waiting", "", None),
            (None, false) => line(&theme, "sd-upd", "Not known", "", None),
            (Some(0), true) | (None, true) => spans(&theme, "sd-upd", "Restart needed", "", Some(warn), Vec::new()),
            (Some(n), r) => spans(&theme, "sd-upd", format!("{n} waiting"), if r { "restart needed" } else { "" }, Some(warn), Vec::new()),
        };
        add2("Updates", upd.into_any_element());
        let docker = match &i.docker {
            Some(d) => line(&theme, "sd-docker", d.version.clone(), match d.on_boot {
                Some(true) => "starts on boot",
                Some(false) => "does not start on boot",
                None => "",
            }, None),
            None => line(&theme, "sd-docker", "Not used", "", None),
        };
        add2("Docker", docker.into_any_element());
        let _ = n;

        // Two columns when the dialog is wide enough, else one under the other.
        let vw = f32::from(window.viewport_size().width);
        let dialog_w = if crate::modal::full_screen(cx) { vw - 32. } else { 900f32.min((vw * 0.7).max(480.)).min(vw - 32.) };
        let cols = dialog_w >= 760.;
        let body = if cols {
            h_flex().items_start().gap(px(40.)).child(section(&theme, "Hardware", hw)).child(section(&theme, "System", sys))
        } else {
            v_flex().gap(px(18.)).child(section(&theme, "Hardware", hw)).child(section(&theme, "System", sys))
        };
        div()
            .id("srv-details-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(body.pb(px(4.)))
    }
}

// ---------------------------------------------------------------------------
// Confirmations
// ---------------------------------------------------------------------------

/// Restart: the mockup's question.
pub fn confirm_restart(name: &str, window: &mut Window, cx: &mut App, go: Rc<dyn Fn(&mut App)>) {
    let name = name.to_string();
    crate::modal::alert(window, cx, move |d, _, _| {
        d.title(format!("Restart {name}?"))
            .description("The whole server restarts. SENTIENT and its database stop and come back by themselves, usually within 2 minutes. Devices keep their data and send it again once SENTIENT is back.\n\nThe connection to this server returns automatically.")
            .ok_text("Restart server")
            .on_ok(move |_, _, cx| {
                go(cx);
                true
            })
    });
}

/// Shut down: the server's name typed to confirm.
pub fn confirm_shutdown(name: &str, window: &mut Window, cx: &mut App, go: Rc<dyn Fn(&mut App)>) {
    let name = name.to_string();
    let view = cx.new(|cx| ShutdownBody::new(name.clone(), window, cx));
    let footer = cx.new(|cx| ShutdownFooter { _o: cx.observe(&view, |_, _, cx| cx.notify()), body: view.clone(), go });
    crate::modal::open(crate::modal::Modal { title: format!("Shut down {name}?").into(), body: view.into(), width: 520., icon: Some(Icon::new(IconName::TriangleAlert)), on_close: None }, cx);
    crate::modal::set_footer(footer.into(), cx);
}

struct ShutdownBody {
    name: String,
    input: Entity<InputState>,
    _s: Subscription,
}

impl ShutdownBody {
    fn new(name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let _s = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        #[cfg(debug_assertions)]
        if let Ok(t) = std::env::var("SPM_DEBUG_SHUTDOWN_TYPED") {
            input.update(cx, |s, cx| s.set_value(t, window, cx));
        }
        Self { name, input, _s }
    }

    fn ok(&self, cx: &App) -> bool {
        name_matches(&self.input.read(cx).value(), &self.name)
    }
}

/// Shut down's field: the server's name exactly (case counts; spaces
/// around it do not), never empty.
pub fn name_matches(typed: &str, name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && typed.trim() == n
}

impl Render for ShutdownBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(theme.muted_foreground).child("SENTIENT stops and the server powers off. Platform Manager cannot switch it on again: someone has to, or use the cloud provider's console."))
            .child(h_flex().flex_wrap().gap_1().text_sm().child("Type").child(div().font_weight(FontWeight::SEMIBOLD).child(self.name.clone())).child("to confirm."))
            .child(Input::new(&self.input))
    }
}

struct ShutdownFooter {
    body: Entity<ShutdownBody>,
    go: Rc<dyn Fn(&mut App)>,
    _o: Subscription,
}

impl Render for ShutdownFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ok = self.body.read(cx).ok(cx);
        let go = self.go.clone();
        h_flex()
            .gap(ui::button_gap())
            .justify_end()
            .child(ui::probed(ui::button("srv-shutdown-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx))))
            // The solid red action once the name is typed (the toolkit's
            // danger variant is a pale tint that reads as disabled).
            .child(ui::probed(if ok {
                crate::modal::danger_button(cx.theme(), "srv-shutdown-go", "Shut down server".into(), move |_, _, cx| {
                    crate::modal::close(cx);
                    go(cx);
                })
                .into_any_element()
            } else {
                ui::button("srv-shutdown-go").danger().label("Shut down server").disabled(true).into_any_element()
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{boot_of, clock_word, gb_label, menu_entries, name_matches, power_state, restart_flow, running_for, set_power, shutdown_flow, updates_note, Diagnosis, FakeHost, MenuEntry, Power, PowerFlag, PowerHost, Progress, ProgressFn};
    use crate::ui::Tone;
    use std::sync::{Arc, Mutex as M};
    use std::time::Duration;

    fn collect() -> (ProgressFn, Arc<M<Vec<Progress>>>) {
        let got: Arc<M<Vec<Progress>>> = Default::default();
        let g = got.clone();
        (Arc::new(move |p| g.lock().unwrap().push(p)), got)
    }

    fn steps(got: &M<Vec<Progress>>) -> Vec<String> {
        got.lock().unwrap().iter().filter_map(|p| if let Progress::Step { name } = p { Some(name.clone()) } else { None }).collect()
    }

    #[test]
    fn a_restart_walks_every_declared_step() {
        let host = FakeHost::new(Some(70), 30, true, 0.);
        let (sink, got) = collect();
        restart_flow(&host, "Plant server", &sink).unwrap();
        let plan: Vec<String> = crate::jobplan::server_restart(false).into_iter().map(|s| s.name).collect();
        assert_eq!(steps(&got), plan);
        assert!(got.lock().unwrap().iter().any(|p| matches!(p, Progress::Done { .. })));
        // Waiting reported along the way, not as a lost connection.
        assert!(got.lock().unwrap().iter().any(|p| matches!(p, Progress::Log { line } if line.starts_with("Reconnecting"))));
    }

    #[test]
    fn a_server_that_never_comes_back_fails_at_that_step() {
        let host = FakeHost::new(None, 0, true, 0.);
        let (sink, got) = collect();
        let e = restart_flow(&host, "Plant server", &sink).unwrap_err();
        assert!(e.contains("did not come back within 10 min"), "{e}");
        assert_eq!(steps(&got).last().map(String::as_str), Some(crate::jobplan::STEP_COMING_BACK));
        // The cause and next step for the error panel.
        assert!(got.lock().unwrap().iter().any(|p| matches!(p, Progress::Log { line } if Diagnosis::decode(line).is_some_and(|d| d.id == "server-not-back"))));
    }

    #[test]
    fn the_same_start_time_is_not_a_restart() {
        let host = FakeHost::new(Some(20), 0, false, 0.);
        let (sink, _) = collect();
        let e = restart_flow(&host, "Plant server", &sink).unwrap_err();
        assert!(e.contains("did not restart"), "{e}");
    }

    #[test]
    fn a_shutdown_ends_once_it_is_down() {
        let host = FakeHost::new(None, 0, true, 0.);
        let (sink, got) = collect();
        shutdown_flow(&host, "Plant server", &sink).unwrap();
        let plan: Vec<String> = crate::jobplan::server_shutdown(false).into_iter().map(|s| s.name).collect();
        assert_eq!(steps(&got), plan);
    }

    #[test]
    fn labels() {
        assert_eq!(running_for(41 * 86_400 + 5), "41 days");
        assert_eq!(running_for(3 * 3600), "3 hours");
        assert_eq!(running_for(90), "1 minute");
        assert_eq!(gb_label(1 << 30), "1 GB");
        assert_eq!(gb_label(644_245_094), "0.6 GB");
        assert_eq!(super::mins(std::time::Duration::from_secs(600)), "10 min");
        assert_eq!(super::mins(std::time::Duration::from_secs(95)), "1 min 35 s");
        assert_eq!(super::mins(std::time::Duration::from_secs(42)), "42 s");
        assert_eq!(boot_of("Stopping\n{\"boot\": 1790000000, \"sent\": true}\n"), Some(1_790_000_000));
    }

    /// A server answering from a script: each `boot` call takes the next
    /// answer (the last one repeats).
    struct Script {
        boots: M<Vec<Result<i64, String>>>,
        t: M<Duration>,
    }

    impl Script {
        fn new(boots: Vec<Result<i64, String>>) -> Self {
            Self { boots: M::new(boots), t: M::new(Duration::ZERO) }
        }
    }

    impl PowerHost for Script {
        fn prepare(&self, _: &ProgressFn) -> Result<(), String> {
            Ok(())
        }
        fn send(&self, _: bool, _: &ProgressFn) -> Result<i64, String> {
            Ok(1_000)
        }
        fn boot(&self) -> Result<i64, String> {
            let mut b = self.boots.lock().unwrap();
            if b.len() > 1 { b.remove(0) } else { b[0].clone() }
        }
        fn running(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn answers(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn wait(&self, d: Duration) {
            *self.t.lock().unwrap() += d;
        }
        fn elapsed(&self) -> Duration {
            *self.t.lock().unwrap()
        }
    }

    #[test]
    fn a_server_that_never_goes_down_fails_at_that_step() {
        let host = Script::new(vec![Ok(1_000)]);
        let (sink, got) = collect();
        let e = restart_flow(&host, "Plant server", &sink).unwrap_err();
        assert!(e.contains("did not go down within 3 min"), "{e}");
        assert_eq!(steps(&got).last().map(String::as_str), Some(crate::jobplan::STEP_WENT_DOWN));
        assert!(got.lock().unwrap().iter().any(|p| matches!(p, Progress::Log { line } if Diagnosis::decode(line).is_some_and(|d| d.id == "server-not-down"))));
    }

    #[test]
    fn a_missed_drop_with_a_new_start_time_is_a_restart() {
        // Before, still up once, then already back with a new start time.
        let host = Script::new(vec![Ok(1_000), Ok(1_000), Ok(2_000)]);
        let (sink, got) = collect();
        restart_flow(&host, "Plant server", &sink).unwrap();
        let plan: Vec<String> = crate::jobplan::server_restart(true).into_iter().map(|s| s.name).collect();
        assert_eq!(steps(&got), plan);
    }

    #[test]
    fn a_restart_waits_through_the_drop() {
        // Up, then down for a while, then back new.
        let mut boots = vec![Ok(1_000), Ok(1_000)];
        boots.extend((0..20).map(|_| Err("ssh: connect to host 192.0.2.10 port 22: Connection refused".to_string())));
        boots.push(Ok(3_000));
        let host = Script::new(boots);
        let (sink, got) = collect();
        restart_flow(&host, "Plant server", &sink).unwrap();
        assert!(got.lock().unwrap().iter().any(|p| matches!(p, Progress::Done { .. })));
        // Same start time after the drop: only the connection dropped.
        let host = Script::new(vec![Ok(1_000), Err("down".into()), Ok(1_000)]);
        let (sink, got) = collect();
        let e = restart_flow(&host, "Plant server", &sink).unwrap_err();
        assert!(e.contains("did not restart"), "{e}");
        assert_eq!(steps(&got).last().map(String::as_str), Some(crate::jobplan::STEP_RESTARTED));
    }

    #[test]
    fn the_plans_are_the_mockups_steps() {
        let r: Vec<String> = crate::jobplan::server_restart(false).into_iter().map(|s| s.name).collect();
        assert_eq!(r, ["Restart sent", "Server went down", "Waiting for the server to come back", "Check it really restarted", "SENTIENT and database running", "SENTIENT answers on its web address"]);
        assert!(crate::jobplan::server_restart(true)[0].detail.contains("shutdown /r"));
        assert!(crate::jobplan::server_restart(false)[0].detail.contains("systemctl reboot"));
        assert!(crate::jobplan::server_shutdown(true)[0].detail.contains("shutdown /s"));
        assert!(crate::jobplan::server_shutdown(false)[0].detail.contains("systemctl poweroff"));
    }

    #[test]
    fn power_states() {
        // Restarting clears however the job ends.
        {
            let _f = PowerFlag::new("test-power-a", Power::Restarting);
            assert_eq!(power_state("test-power-a"), Some(Power::Restarting));
            // It still answers until it goes down: that does not clear it.
            super::answered("test-power-a");
            assert_eq!(power_state("test-power-a"), Some(Power::Restarting));
        }
        assert_eq!(power_state("test-power-a"), None);
        // A shut down that went through stays off until it answers.
        {
            let _f = PowerFlag::new("test-power-b", Power::ShuttingDown);
            set_power("test-power-b", Some(Power::Off));
        }
        assert_eq!(power_state("test-power-b"), Some(Power::Off));
        super::answered("test-power-b");
        assert_eq!(power_state("test-power-b"), None);
        // A failed shut down is not off.
        {
            let _f = PowerFlag::new("test-power-c", Power::ShuttingDown);
        }
        assert_eq!(power_state("test-power-c"), None);
    }

    #[test]
    fn the_menu_in_the_mockups_order() {
        use MenuEntry::*;
        assert_eq!(menu_entries(true, true), [Docker, Updates, Divider, Shutdown]);
        // Native (no Docker): no Restart Docker.
        assert_eq!(menu_entries(true, false), [Updates, Divider, Shutdown]);
        // This computer: never Shut down server.
        assert_eq!(menu_entries(false, true), [Docker, Updates]);
    }

    #[test]
    fn shut_down_needs_the_name() {
        assert!(name_matches("Plant server", "Plant server"));
        assert!(name_matches("  Plant server ", "Plant server"));
        assert!(!name_matches("plant server", "Plant server"));
        assert!(!name_matches("Plant", "Plant server"));
        assert!(!name_matches("", "Plant server"));
        assert!(!name_matches("", " "));
    }

    #[test]
    fn card_notes() {
        assert_eq!(updates_note(Some(12), true).as_deref(), Some("12 updates waiting"));
        assert_eq!(updates_note(Some(1), false).as_deref(), Some("1 update waiting"));
        assert_eq!(updates_note(Some(0), true).as_deref(), Some("restart needed"));
        assert_eq!(updates_note(None, false), None);
        use crate::server_info::Clock;
        let c = |synced, offset| Clock { synced, offset, zone: None };
        assert!(clock_word(&c(Some(true), Some(0.02))) == ("Synced", Tone::Good));
        assert!(clock_word(&c(Some(true), Some(1.5))) == ("Out of sync", Tone::Warn));
        assert!(clock_word(&c(Some(false), None)) == ("Not synced", Tone::Warn));
        assert!(clock_word(&c(None, Some(-2.))) == ("Out of sync", Tone::Warn));
        assert!(clock_word(&c(None, None)) == ("Not known", Tone::Neutral));
    }
}
