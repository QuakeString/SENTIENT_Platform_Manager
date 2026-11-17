// Copyright © 2016-2026 The SENTIENT Authors

//! Statistics: the open instance's processor, memory, disk, network and
//! services, live or over 1 hour, 24 hours or 7 days (crate::stats reads
//! them; on a server through its agent). Opened from the top bar's monitors;
//! no menu entry. The same view for this computer and a server.
//!
//! Live reads every few seconds, only while this page is shown (`set_open`).
//! The longer ranges come from the sampler's history, one reading a minute.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme, Disableable as _, button::ButtonVariants as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::dbstatus::bytes_label;
use crate::installation::InstallationRecord;
use crate::stats::{DbRead, Part, Sample};
use crate::ui;

/// Live: one reading every this many seconds, the last LIVE_KEEP kept (about
/// five minutes).
const LIVE_EVERY: Duration = Duration::from_secs(3);
const LIVE_KEEP: usize = 100;
/// The longer ranges read their history again this often while shown.
const HISTORY_EVERY: Duration = Duration::from_secs(60);
/// Live: the second reading this soon after the first, so the charts have
/// a line within about a second.
const SECOND_AFTER: Duration = Duration::from_secs(1);
/// The Server card's reading again this often while shown.
const SERVER_EVERY: Duration = Duration::from_secs(300);

static OPEN: AtomicBool = AtomicBool::new(false);

/// The page is shown (the manager sets it each frame): reading stops when not.
pub fn set_open(open: bool) {
    OPEN.store(open, Ordering::Relaxed);
}

#[derive(Clone)]
pub enum Source {
    Local(Option<InstallationRecord>),
    Remote(crate::remote::Instance),
}

impl Source {
    /// Processor, memory, disk, network, uptime: /proc and sysinfo here,
    /// never a service manager, a database or a password; on a server one
    /// agent call. An agent from before the parts answers the whole
    /// reading (services and database in it).
    fn system(&self) -> Result<Sample, String> {
        match self {
            Source::Local(r) => {
                #[cfg(debug_assertions)]
                if let Some(d) = std::env::var_os("SPM_DEBUG_STATS") {
                    return crate::stats::debug_part(std::path::Path::new(&d), Part::System);
                }
                Ok(crate::stats::local_read(r.as_ref(), crate::stats::FIRST_WINDOW))
            }
            Source::Remote(i) => crate::remote::stats_live_part(i, Part::System),
        }
    }

    /// Whether there are services and a database to read at all.
    fn has_install(&self) -> bool {
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_STATS").is_some() {
            return true;
        }
        match self {
            Source::Local(r) => r.is_some(),
            Source::Remote(_) => true,
        }
    }

    fn services(&self) -> Result<Vec<crate::stats::Service>, String> {
        match self {
            Source::Local(r) => {
                #[cfg(debug_assertions)]
                if let Some(d) = std::env::var_os("SPM_DEBUG_STATS") {
                    return crate::stats::debug_part(std::path::Path::new(&d), Part::Services).map(|s| s.services);
                }
                Ok(crate::stats::services_over(r.as_ref(), Duration::from_secs(1)))
            }
            Source::Remote(i) => crate::remote::stats_live_part(i, Part::Services).map(|s| s.services),
        }
    }

    /// The Server card's reading (crate::server_info): in this process
    /// here, one agent call on a server.
    fn server_info(&self) -> Result<crate::server_info::ServerInfo, String> {
        match self {
            Source::Local(r) => Ok(crate::server_info::gather(r.as_ref())),
            Source::Remote(i) => crate::manager::server_card::read_remote(i),
        }
    }

    fn history(&self, secs: i64) -> Result<Vec<Sample>, String> {
        match self {
            Source::Local(_) => {
                #[cfg(debug_assertions)]
                if let Some(d) = std::env::var_os("SPM_DEBUG_STATS") {
                    return Ok(crate::stats::debug_history(std::path::Path::new(&d), secs));
                }
                Ok(crate::stats::history(&crate::stats::history_path(), secs, chrono::Utc::now().timestamp()))
            }
            Source::Remote(i) => crate::remote::stats_history(i, secs),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Range {
    Live,
    Hour,
    Day,
    Week,
}

impl Range {
    const ALL: [Range; 4] = [Range::Live, Range::Hour, Range::Day, Range::Week];

    fn label(self) -> &'static str {
        match self {
            Range::Live => "Live",
            Range::Hour => "1 hour",
            Range::Day => "24 hours",
            Range::Week => "7 days",
        }
    }

    fn secs(self) -> i64 {
        match self {
            Range::Live => 300,
            Range::Hour => 3600,
            Range::Day => 86_400,
            Range::Week => 7 * 86_400,
        }
    }
}

pub struct StatsPage {
    src: Source,
    range: Range,
    /// Processor, memory, disk, network, uptime: the figures and the live charts.
    live: Vec<Sample>,
    hist: Option<Result<Vec<Sample>, String>>,
    live_err: Option<String>,
    /// Its instance is not the open one (a kept page): no reading meanwhile.
    paused: bool,
    /// A read asked for (shown again, Refresh) has not come back yet: the
    /// Refresh icon turns.
    refreshing: bool,
    /// Per part: the last re-read failed while what was read before still
    /// shows (the first one is said, softly, in the title row).
    stale: Vec<(&'static str, String)>,
    /// When a reading last came back (HH:MM).
    updated_at: Option<String>,
    /// Each source fills its own card: None while it is being read.
    services: Option<Result<Vec<crate::stats::Service>, String>>,
    db: Option<DbRead>,
    /// The server's agent answers whole readings (from before the parts):
    /// services and database come with the processor reading.
    whole: Arc<AtomicBool>,
    /// Debug builds: when the page opened, for SPM_DEBUG_STATS_TIMING.
    opened: Instant,
    timed: Vec<&'static str>,
    /// (chart, point) under the pointer.
    hover: Option<(usize, usize)>,
    /// Each chart's plot bounds, for the pointer.
    plots: Vec<Rc<Cell<Bounds<Pixels>>>>,
    /// Update agent (a server whose agent is too old for these readings).
    job: Entity<crate::jobs::Job>,
    /// The Server card's reading (crate::manager::server_card): None while
    /// it is read the first time.
    server: Option<Result<crate::server_info::ServerInfo, String>>,
    /// The card's jobs (restart, shut down, Docker, updates), shown in the
    /// shared job view in place of the page until "Back to Statistics".
    server_job: Entity<crate::jobs::Job>,
    server_shown: bool,
    /// The job view was left with Hide while the job runs: read the card
    /// again when it ends.
    server_hidden: bool,
    server_job_label: String,
    /// Debug builds: the dialog SPM_DEBUG_SERVER_DIALOG asks for was opened.
    debug_opened: bool,
    _server_poll: Option<Task<()>>,
    _server_watch: Option<Subscription>,
    _watch: Option<Subscription>,
    _poll: Vec<Task<()>>,
    _tasks: Vec<Task<()>>,
}

impl StatsPage {
    pub fn new(src: Source, cx: &mut Context<Self>) -> Self {
        #[allow(unused_mut)]
        let mut range = Range::Live;
        #[cfg(debug_assertions)]
        if let Ok(r) = std::env::var("SPM_STATS_RANGE") {
            range = match r.as_str() {
                "hour" => Range::Hour,
                "day" => Range::Day,
                "week" => Range::Week,
                _ => Range::Live,
            };
        }
        // Nothing installed here: no Services and no Database to wait for.
        let none = !src.has_install();
        let mut p = Self {
            src,
            range,
            live: Vec::new(),
            hist: None,
            live_err: None,
            paused: false,
            refreshing: false,
            stale: Vec::new(),
            updated_at: None,
            services: none.then(|| Ok(Vec::new())),
            db: none.then_some(DbRead::None),
            whole: Arc::new(AtomicBool::new(false)),
            opened: Instant::now(),
            timed: Vec::new(),
            hover: None,
            plots: (0..6).map(|_| Rc::new(Cell::new(Bounds::default()))).collect(),
            job: cx.new(|_| crate::jobs::Job::new()),
            server: None,
            server_job: cx.new(|_| crate::jobs::Job::new()),
            server_shown: false,
            server_hidden: false,
            server_job_label: String::new(),
            debug_opened: false,
            _server_poll: None,
            _server_watch: None,
            _watch: None,
            _poll: Vec::new(),
            _tasks: Vec::new(),
        };
        p.start(cx);
        p.poll_server(cx);
        let w = cx.observe(&p.server_job, |p, job, cx| {
            // A job that ended changes what the card shows: read it again.
            if !job.read(cx).is_running() && (p.server_shown || p.server_hidden) {
                p.server_hidden = false;
                p.read_server(cx);
            }
            cx.notify();
        });
        p._server_watch = Some(w);
        p
    }

    /// The Server card: read on open, then every few minutes while shown
    /// (nothing on the live ticks; the processor and memory bars use the
    /// live reading).
    fn poll_server(&mut self, cx: &mut Context<Self>) {
        let src = self.src.clone();
        self._server_poll = Some(cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let Ok(paused) = this.update(cx, |p, _| p.paused) else { break };
                if first || (OPEN.load(Ordering::Relaxed) && !paused) {
                    first = false;
                    let s = src.clone();
                    let got = cx.background_executor().spawn(async move { s.server_info() }).await;
                    if this.update(cx, |p, cx| p.got_server(got, cx)).is_err() {
                        break;
                    }
                }
                cx.background_executor().timer(SERVER_EVERY).await;
            }
        }));
    }

    /// Read the Server card once now (Refresh, shown again, a job ended).
    fn read_server(&mut self, cx: &mut Context<Self>) {
        let src = self.src.clone();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { src.server_info() }).await;
            this.update(cx, |p, cx| p.got_server(got, cx)).ok();
        });
        self._tasks.push(task);
    }

    fn got_server(&mut self, got: Result<crate::server_info::ServerInfo, String>, cx: &mut Context<Self>) {
        match got {
            // A failed re-read keeps what was read.
            Err(e) if matches!(self.server, Some(Ok(_))) => self.set_stale("server", Some(e)),
            got => {
                if got.is_ok() {
                    self.set_stale("server", None);
                    // A server shut down from here is on again.
                    if let Source::Remote(i) = &self.src {
                        crate::manager::server_card::answered(&i.id);
                    }
                }
                self.server = Some(got);
            }
        }
        self.timing("server");
        cx.notify();
    }

    fn remote_inst(&self) -> Option<crate::remote::Instance> {
        match &self.src {
            Source::Remote(i) => Some(i.clone()),
            Source::Local(_) => None,
        }
    }

    /// Screens: SPM_DEBUG_SERVER_DIALOG=details|restart|shutdown opens that
    /// once the reading is in; =job:<action> starts that job (with
    /// SPM_DEBUG_SERVER_RESTART, a server that is not there).
    #[cfg(debug_assertions)]
    fn debug_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::manager::server_card::{self as sc, Action};
        let Ok(v) = std::env::var("SPM_DEBUG_SERVER_DIALOG") else { return };
        let Some(Ok(info)) = self.server.clone() else { return };
        if self.debug_opened {
            return;
        }
        self.debug_opened = true;
        let name = self.server_name();
        cx.defer_in(window, move |p, window, cx| match v.as_str() {
            "details" => sc::open_details(&name, info, cx),
            "restart" => sc::confirm_restart(&name, window, cx, Rc::new(|_| {})),
            "shutdown" => sc::confirm_shutdown(&name, window, cx, Rc::new(|_| {})),
            j => {
                let a = match j.strip_prefix("job:").unwrap_or("") {
                    "shutdown" => Action::Shutdown,
                    "docker" => Action::Docker,
                    "updates" => Action::Updates,
                    _ => Action::Restart,
                };
                p.server_action(a, cx);
            }
        });
    }

    /// Start one of the card's jobs (asked first where it says so).
    fn server_action(&mut self, action: crate::manager::server_card::Action, cx: &mut Context<Self>) {
        if self.server_job.read(cx).is_running() {
            return;
        }
        let windows = match &self.server {
            Some(Ok(i)) => i.windows(),
            _ => self.remote_inst().is_some_and(|i| crate::remote::is_windows(&i)),
        };
        let me = cx.entity().downgrade();
        let retry: Rc<dyn Fn(&mut App)> = Rc::new(move |cx: &mut App| {
            me.update(cx, |p, cx| p.server_action(action, cx)).ok();
        });
        self.server_shown = true;
        self.server_hidden = false;
        self.server_job_label = action.title(&self.server_name());
        crate::manager::server_card::start(&self.server_job, action, self.remote_inst(), windows, retry, cx);
        cx.notify();
    }

    fn server_name(&self) -> String {
        match &self.src {
            Source::Remote(i) => i.name.clone(),
            Source::Local(_) => "this computer".into(),
        }
    }

    /// The card's actions, each asking first where it should.
    fn server_actions(&self, cx: &mut Context<Self>) -> crate::manager::server_card::Actions {
        use crate::manager::server_card::{self as sc, Action};
        let me = cx.entity().downgrade();
        let name = self.server_name();
        let run = move |a: Action| {
            let me = me.clone();
            Rc::new(move |cx: &mut App| {
                me.update(cx, |p, cx| p.server_action(a, cx)).ok();
            }) as Rc<dyn Fn(&mut App)>
        };
        let (n1, n2) = (name.clone(), name.clone());
        let (r1, r2, r3, r4) = (run(Action::Restart), run(Action::Shutdown), run(Action::Docker), run(Action::Updates));
        let details_info = self.server.as_ref().and_then(|s| s.as_ref().ok()).cloned();
        let n3 = name.clone();
        let me2 = cx.entity().downgrade();
        let term_inst = self.remote_inst();
        sc::Actions {
            remote: matches!(self.src, Source::Remote(_)),
            busy: self.server_job.read(cx).is_running(),
            on_terminal: Rc::new(move |w, cx| sc::open_terminal(term_inst.as_ref(), w, cx)),
            on_restart: Rc::new(move |w, cx| sc::confirm_restart(&n1, w, cx, r1.clone())),
            on_shutdown: Rc::new(move |w, cx| sc::confirm_shutdown(&n2, w, cx, r2.clone())),
            on_docker: Rc::new(move |_, cx| r3(cx)),
            on_updates: Rc::new(move |_, cx| r4(cx)),
            on_details: Rc::new(move |_, cx| {
                if let Some(i) = details_info.clone() {
                    sc::open_details(&n3, i, cx);
                }
            }),
            on_update_agent: Rc::new(move |_, cx| {
                me2.update(cx, |p, cx| p.update_agent(cx)).ok();
            }),
        }
    }

    /// Debug builds with SPM_DEBUG_STATS_TIMING: when each source first
    /// came back, after the page opened.
    fn timing(&mut self, what: &'static str) {
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_STATS_TIMING").is_some() && !self.timed.contains(&what) {
            self.timed.push(what);
            eprintln!("stats timing: {what} after {} ms", self.opened.elapsed().as_millis());
        }
        #[cfg(not(debug_assertions))]
        let _ = what;
    }

    /// Hidden behind another instance: no reading until shown again.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Shown again after another instance was open: what was read stays
    /// and every part is read again behind it.
    pub fn reshow(&mut self, cx: &mut Context<Self>) {
        self.paused = false;
        self.read_again(cx);
    }

    /// Read every part again now, keeping what shows (the Refresh icon turns).
    fn read_again(&mut self, cx: &mut Context<Self>) {
        self.refreshing = true;
        self.restart(false, cx);
        self.read_server(cx);
        cx.notify();
    }

    /// A part came back: `Some(why)` when it failed while its last reading
    /// still shows.
    fn set_stale(&mut self, part: &'static str, why: Option<String>) {
        self.stale.retain(|(p, _)| *p != part);
        match why {
            Some(e) => self.stale.push((part, e)),
            None => self.updated_at = Some(chrono::Local::now().format("%H:%M").to_string()),
        }
    }

    fn push_live(&mut self, sample: Sample) {
        // An agent from before the parts: its reading carries the rest.
        if !sample.services.is_empty() || sample.db.is_some() {
            self.whole.store(true, Ordering::Relaxed);
        }
        if self.whole.load(Ordering::Relaxed) {
            self.services = Some(Ok(sample.services.clone()));
            self.db = Some(sample.db.clone().map(DbRead::Read).unwrap_or(DbRead::None));
            self.timing("services");
            self.timing("db");
        }
        self.live.push(sample);
        if self.live.len() > LIVE_KEEP {
            self.live.remove(0);
        }
        self.live_err = None;
        self.set_stale("system", None);
        self.timing("system");
    }

    /// Read now and then on the range's rhythm, while the page is shown:
    /// each source on its own, so a slow one never holds the others back.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.restart(true, cx);
    }

    /// `clear`: a new range, whose history is not read yet.
    fn restart(&mut self, clear: bool, cx: &mut Context<Self>) {
        let range = self.range;
        if range != Range::Live && clear {
            self.hist = None;
        }
        self._poll.clear();
        // 20 seconds without a first reading: say so, with Retry.
        let timeout = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(20)).await;
            this.update(cx, |p, cx| {
                if p.live.is_empty() && p.live_err.is_none() {
                    p.live_err = Some("The server did not answer within 20 seconds.".into());
                    cx.notify();
                }
            })
            .ok();
        });
        self._tasks.push(timeout);
        let every = if range == Range::Live { LIVE_EVERY } else { HISTORY_EVERY };

        // Processor, memory, disk, network, uptime.
        let src = self.src.clone();
        self._poll.push(cx.spawn(async move |this, cx| {
            let mut round = 0u32;
            loop {
                let Ok(paused) = this.update(cx, |p, _| p.paused) else { break };
                if round == 0 || (OPEN.load(Ordering::Relaxed) && !paused) {
                    let s = src.clone();
                    let got = cx.background_executor().spawn(async move { s.system() }).await;
                    let ok = this
                        .update(cx, |p, cx| {
                            if range == Range::Live {
                                p.refreshing = false;
                            }
                            match got {
                                Ok(sample) => p.push_live(sample),
                                // With readings shown: they stay, said
                                // softly to be stale.
                                Err(e) if !p.live.is_empty() => p.set_stale("system", Some(e)),
                                Err(e) => p.live_err = Some(e),
                            }
                            cx.notify();
                        })
                        .is_ok();
                    if !ok {
                        break;
                    }
                }
                round += 1;
                cx.background_executor().timer(if round == 1 && range == Range::Live { SECOND_AFTER } else { every }).await;
            }
        }));

        // Each SENTIENT service (systemctl or docker stats: a second or two).
        if self.src.has_install() {
            let (src, whole) = (self.src.clone(), self.whole.clone());
            self._poll.push(cx.spawn(async move |this, cx| {
                let mut first = true;
                loop {
                    if whole.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(paused) = this.update(cx, |p, _| p.paused) else { break };
                    if first || (OPEN.load(Ordering::Relaxed) && !paused) {
                        first = false;
                        let s = src.clone();
                        let got = cx.background_executor().spawn(async move { s.services() }).await;
                        if this
                            .update(cx, |p, cx| {
                                if !p.whole.load(Ordering::Relaxed) {
                                    match got {
                                        // A failed re-read keeps what was read.
                                        Err(e) if matches!(p.services, Some(Ok(_))) => p.set_stale("services", Some(e)),
                                        got => {
                                            if got.is_ok() {
                                                p.set_stale("services", None);
                                            }
                                            p.services = Some(got);
                                        }
                                    }
                                    p.timing("services");
                                    cx.notify();
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    cx.background_executor().timer(every).await;
                }
            }));
        }

        // The longer ranges' history.
        if range != Range::Live {
            let src = self.src.clone();
            self._poll.push(cx.spawn(async move |this, cx| {
                let mut first = true;
                loop {
                    let Ok(paused) = this.update(cx, |p, _| p.paused) else { break };
                    if first || (OPEN.load(Ordering::Relaxed) && !paused) {
                        first = false;
                        let s = src.clone();
                        let got = cx.background_executor().spawn(async move { s.history(range.secs()) }).await;
                        if this
                            .update(cx, |p, cx| {
                                p.refreshing = false;
                                match got {
                                    // A failed re-read keeps the history shown.
                                    Err(e) if matches!(p.hist, Some(Ok(_))) => p.set_stale("history", Some(e)),
                                    got => {
                                        if got.is_ok() {
                                            p.set_stale("history", None);
                                        }
                                        p.hist = Some(got);
                                    }
                                }
                                p.timing("history");
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    cx.background_executor().timer(HISTORY_EVERY).await;
                }
            }));
        }
    }

    /// Update the server's agent (the progress chip; Retry on failure),
    /// then read again.
    fn update_agent(&mut self, cx: &mut Context<Self>) {
        let Source::Remote(inst) = self.src.clone() else { return };
        let me = cx.entity().downgrade();
        let me2 = me.clone();
        self.job.update(cx, |job, cx| {
            job.set_label(format!("Updating the server agent on {}", inst.name));
            job.set_retry(Rc::new(move |cx: &mut App| {
                me.update(cx, |p, cx| p.update_agent(cx)).ok();
            }));
            job.start(move |sink| crate::remote::deploy(&inst, &sink), cx)
        });
        let watch = cx.observe(&self.job, move |p, job, cx| {
            if matches!(job.read(cx).state(), crate::jobs::JobState::Done(_)) {
                p.live_err = None;
                p.start(cx);
                p.read_server(cx);
            }
            cx.notify();
        });
        let _ = me2;
        self._watch = Some(watch);
        cx.notify();
    }

    fn pick(&mut self, range: Range, cx: &mut Context<Self>) {
        if self.range != range {
            self.range = range;
            self.hover = None;
            self.start(cx);
            cx.notify();
        }
    }

    /// The points the charts draw.
    fn points(&self) -> &[Sample] {
        match (&self.range, &self.hist) {
            (Range::Live, _) => &self.live,
            (_, Some(Ok(h))) => h,
            _ => &[],
        }
    }
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

struct Chart {
    title: &'static str,
    values: Vec<f32>,
    max: f32,
    fmt: fn(f32) -> String,
    axis: fn(f32) -> String,
    high: bool,
}

/// A round top for a chart's scale: 1, 2 or 5 times a power of ten.
/// A chart's top: the smallest of 1, 2, 4, 5 × 10^n at or above `v`, so
/// its half (the middle grid line) is round too (0.5, 1, 2, 2.5).
fn nice(v: f32) -> f32 {
    if v <= 0. {
        return 1.;
    }
    let p = 10f32.powf(v.log10().floor());
    [1., 2., 4., 5., 10.].iter().map(|m| m * p).find(|c| *c >= v * 0.9999).unwrap_or(10. * p)
}

/// Memory's top: the total rounded up to a power of two (31 GB → 32, 7 → 8).
fn nice_memory(total: f32) -> f32 {
    let mut t = 1.;
    while t < total * 0.999 {
        t *= 2.;
    }
    t
}

/// A scale label: as many decimals as the middle line needs, so the three
/// labels are always distinct (0, 0.25, 0.5; 0, 1, 2).
pub(crate) fn tick(v: f32, max: f32) -> String {
    if v == 0. {
        return "0".into();
    }
    let mid = max / 2.;
    let decimals = (0..=2).find(|d| {
        let f = 10f32.powi(*d);
        ((mid * f).round() - mid * f).abs() < 1e-3
    });
    format!("{v:.*}", decimals.unwrap_or(2) as usize)
}

fn mb(v: f64) -> f32 {
    (v / 1_000_000.) as f32
}

fn gb(v: u64) -> f32 {
    v as f32 / (1u64 << 30) as f32
}

fn charts(points: &[Sample]) -> Vec<Chart> {
    let last = points.last();
    let col = |f: &dyn Fn(&Sample) -> f32| points.iter().map(f).collect::<Vec<f32>>();
    let peak = |v: &[f32]| v.iter().cloned().fold(0f32, f32::max);
    let mem_total = last.map(|s| gb(s.mem_total)).unwrap_or(1.).max(0.1);
    let cores = last.map(|s| s.cores as f32).unwrap_or(1.).max(1.);
    let load = col(&|s| s.load.unwrap_or(0.));
    let rx = col(&|s| mb(s.rx));
    let tx = col(&|s| mb(s.tx));
    let io = col(&|s| mb(s.io.unwrap_or(0.)));
    // Rates keep a floor of 1 MB/s, so a quiet line is not stretched.
    let io_max = nice(peak(&io).max(1.));
    vec![
        Chart { title: "Processor", values: col(&|s| s.cpu), max: 100., fmt: |v| format!("{v:.0} %"), axis: |v| format!("{v:.0}"), high: last.is_some_and(|s| s.cpu_high()) },
        Chart { title: "Memory", values: col(&|s| gb(s.mem_used)), max: nice_memory(mem_total), fmt: |v| format!("{v:.1} GB"), axis: |v| format!("{v}"), high: last.is_some_and(|s| s.mem_high()) },
        Chart { title: "Processor load", max: nice(peak(&load).max(cores)), values: load, fmt: |v| format!("{v:.2}"), axis: |v| if v.fract() == 0. { format!("{v:.0}") } else { format!("{v:.1}") }, high: last.is_some_and(|s| s.load.unwrap_or(0.) > s.cores as f32) },
        Chart { title: "Network received", max: nice(peak(&rx).max(1.)), values: rx, fmt: |v| format!("{v:.1} MB/s"), axis: |v| if v == 0. || (v >= 1. && v.fract() == 0.) { format!("{v:.0}") } else { format!("{v:.1}") }, high: false },
        Chart { title: "Network sent", max: nice(peak(&tx).max(1.)), values: tx, fmt: |v| format!("{v:.1} MB/s"), axis: |v| if v == 0. || (v >= 1. && v.fract() == 0.) { format!("{v:.0}") } else { format!("{v:.1}") }, high: false },
        // Disk activity is high when it runs near its own peak for the
        // range and that peak is real work (over 50 MB/s).
        Chart { title: "Disk activity", high: io.last().is_some_and(|v| *v >= 50. && *v >= io_max * 0.8), max: io_max, values: io, fmt: |v| format!("{v:.0} MB/s"), axis: |v| format!("{v:.0}") },
    ]
}

fn time_label(t: i64, range: Range) -> String {
    let Some(dt) = chrono::DateTime::from_timestamp(t, 0) else { return String::new() };
    let local = dt.with_timezone(&chrono::Local);
    match range {
        Range::Week => local.format("%a %H:%M").to_string(),
        Range::Live => local.format("%H:%M:%S").to_string(),
        _ => local.format("%H:%M").to_string(),
    }
}

impl StatsPage {
    fn chart(&self, i: usize, c: Chart, times: &[i64], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let warn: Hsla = if theme.mode.is_dark() { rgb(0xf3b744).into() } else { rgb(0xb97400).into() };
        let line: Hsla = if c.high { warn } else { theme.primary };
        let n = c.values.len();
        let now = c.values.last().map(|v| (c.fmt)(*v)).unwrap_or_else(|| "–".into());
        let hover = self.hover.filter(|(h, _)| *h == i).map(|(_, p)| p).filter(|p| *p < n);
        let values = c.values.clone();
        let max = c.max.max(0.0001);
        let bounds = self.plots[i].clone();
        let grid = theme.border;
        let cross = theme.muted_foreground;
        const PLOT_H: f32 = 96.;
        let plot = canvas(
            {
                let bounds = bounds.clone();
                move |b, _, _| bounds.set(b)
            },
            move |b, _, window, _| {
                let (x0, y0, w, h) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
                for f in [0., 0.5, 1.] {
                    let y = y0 + h - f * h;
                    window.paint_quad(fill(Bounds::new(point(px(x0), px(y.round())), size(px(w), px(1.))), grid));
                }
                if n < 2 {
                    return;
                }
                let pt = |k: usize, v: f32| point(px(x0 + k as f32 * w / (n - 1) as f32), px(y0 + h - (v.min(max) / max) * h));
                let mut area = PathBuilder::fill();
                area.move_to(point(px(x0), px(y0 + h)));
                for (k, v) in values.iter().enumerate() {
                    area.line_to(pt(k, *v));
                }
                area.line_to(point(px(x0 + w), px(y0 + h)));
                area.close();
                if let Ok(p) = area.build() {
                    window.paint_path(p, line.opacity(0.13));
                }
                let mut stroke = PathBuilder::stroke(px(2.));
                stroke.move_to(pt(0, values[0]));
                for (k, v) in values.iter().enumerate().skip(1) {
                    stroke.line_to(pt(k, *v));
                }
                if let Ok(p) = stroke.build() {
                    window.paint_path(p, line);
                }
                if let Some(k) = hover {
                    let x = x0 + k as f32 * w / (n - 1) as f32;
                    window.paint_quad(fill(Bounds::new(point(px(x.round()), px(y0)), size(px(1.), px(h))), cross));
                }
            },
        )
        .size_full();
        let axis = |f: f32| div().text_size(px(10.5)).text_color(theme.muted_foreground).font_features(ui::tabular()).child(tick(c.max * f, c.max));
        let tip = hover.map(|k| {
            let at = if self.range == Range::Live {
                let ago = times.last().copied().unwrap_or(0) - times.get(k).copied().unwrap_or(0);
                if ago <= 0 { "now".to_string() } else { format!("{ago} s ago") }
            } else {
                time_label(times.get(k).copied().unwrap_or(0), self.range)
            };
            let f = if n > 1 { k as f32 / (n - 1) as f32 } else { 0. };
            div()
                .absolute()
                .top(px(-6.))
                .left(relative(f))
                .child(
                    div()
                        .ml(px(-60.))
                        .w(px(120.))
                        .flex()
                        .justify_center()
                        .child(div().px(px(7.)).py(px(2.)).rounded(px(6.)).bg(theme.foreground).text_color(theme.background).text_size(px(11.5)).whitespace_nowrap().child(format!("{} · {at}", (c.fmt)(values_at(&c.values, k))))),
                )
        });
        let first_t = times.first().map(|t| time_label(*t, self.range)).unwrap_or_default();
        let last_t = times.last().map(|t| time_label(*t, self.range)).unwrap_or_default();
        let empty = n < 2;
        let plot_bounds = bounds;
        ui::panel(&theme)
            .flex_1()
            .gap_1()
            .min_w_0()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().font_weight(FontWeight::MEDIUM).child(c.title))
                    .when(c.high, |r| r.child(ui::pill(&theme, ui::Tone::Warn, "High").text_size(px(11.5))))
                    .child(div().flex_1())
                    .child(div().text_size(px(12.)).text_color(theme.muted_foreground).font_features(ui::tabular()).whitespace_nowrap().child(format!("now {now}"))),
            )
            .child(
                h_flex()
                    .relative()
                    .gap(px(6.))
                    .items_start()
                    // Layout tests: the scale and plot are not row dividers.
                    .child(ui::layout_probe("chart"))
                    // The scale: its widest label decides the column, for every chart alike.
                    .child(v_flex().w(px(26.)).h(px(PLOT_H)).justify_between().items_end().when(!empty, |c| c.child(axis(1.)).child(axis(0.5)).child(axis(0.))))
                    .child(
                        div()
                            .id(SharedString::from(format!("plot-{i}")))
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h(px(PLOT_H))
                            .child(plot)
                            .children(tip)
                            .when(empty, |d| {
                                d.child(
                                    div().absolute().inset_0().px_2().flex().items_center().justify_center().child(div().w_full().text_center().text_size(px(12.5)).text_color(theme.muted_foreground).child(if self.range == Range::Live {
                                        "Reading…"
                                    } else {
                                        "No history yet. It fills up, one reading a minute."
                                    })),
                                )
                            })
                            .on_mouse_move(cx.listener(move |p, e: &MouseMoveEvent, _, cx| {
                                let b = plot_bounds.get();
                                let w = f32::from(b.size.width);
                                if n < 2 || w <= 0. {
                                    return;
                                }
                                let f = ((f32::from(e.position.x) - f32::from(b.origin.x)) / w).clamp(0., 1.);
                                let k = (f * (n - 1) as f32).round() as usize;
                                if p.hover != Some((i, k)) {
                                    p.hover = Some((i, k));
                                    cx.notify();
                                }
                            }))
                            .on_hover(cx.listener(move |p, over: &bool, _, cx| {
                                if !*over && p.hover.is_some_and(|(h, _)| h == i) {
                                    p.hover = None;
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(
                h_flex()
                    .pl(px(32.))
                    .text_size(px(10.5))
                    .text_color(theme.muted_foreground)
                    .child(first_t)
                    .child(div().flex_1())
                    .child(last_t),
            )
            .into_any_element()
    }
}

fn values_at(v: &[f32], k: usize) -> f32 {
    v.get(k).copied().unwrap_or(0.)
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// A figure: its name, a big number with its unit, a bar, a line under it.
fn figure(theme: &gpui_kit::component::Theme, name: &'static str, big: String, unit: String, bar: Option<(f32, bool)>, line: String) -> Div {
    let warn: Hsla = if theme.mode.is_dark() { rgb(0xf3b744).into() } else { rgb(0xb97400).into() };
    ui::panel(theme)
        .gap_0()
        .min_w_0()
        .child(div().text_sm().text_color(theme.muted_foreground).child(name))
        .child(
            h_flex()
                .items_baseline()
                .gap_1()
                .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).font_features(ui::tabular()).child(big))
                .child(div().text_sm().text_color(theme.muted_foreground).whitespace_nowrap().child(unit)),
        )
        .child(
            div().mt(px(7.)).child(ui::segment_bar(theme.secondary, 5., &bar.map(|(f, high)| vec![(if high { warn } else { theme.primary }, f.clamp(0., 1.))]).unwrap_or_default())).when(bar.is_none(), |d| d.invisible()),
        )
        .child(div().mt(px(5.)).text_size(px(12.)).text_color(theme.muted_foreground).overflow_hidden().whitespace_nowrap().text_ellipsis().child(line))
}

/// A figure while its reading comes.
fn skeleton_figure(theme: &gpui_kit::component::Theme, i: usize, name: &'static str) -> AnyElement {
    ui::panel(theme)
        .flex_1()
        .gap_0()
        .child(div().text_sm().text_color(theme.muted_foreground).child(name))
        .child(div().h(px(30.)).flex().items_center().child(ui::skeleton_block(theme, format!("fig-{i}"), px(70.).into(), 18.)))
        .child(div().mt(px(7.)).child(ui::skeleton_block(theme, format!("figbar-{i}"), relative(1.).into(), 5.)))
        .child(div().mt(px(5.)).child(ui::skeleton_line(theme, format!("figline-{i}"), 110.)))
        .into_any_element()
}

/// A chart while its points come.
fn skeleton_chart(theme: &gpui_kit::component::Theme, i: usize, title: &'static str) -> AnyElement {
    ui::panel(theme)
        .flex_1()
        .gap_1()
        .child(div().font_weight(FontWeight::MEDIUM).child(title))
        .child(ui::skeleton_block(theme, format!("plot-{i}"), relative(1.).into(), 96.))
        .child(div().h(px(15.)))
        .into_any_element()
}

/// The Services table's number columns.
const PROC_W: f32 = 118.;
const MEM_W: f32 = 124.;
const RESTART_W: f32 = 58.;

fn mini_bar(theme: &gpui_kit::component::Theme, f: f32) -> Div {
    div().w(px(56.)).flex_shrink_0().child(ui::segment_bar(theme.secondary, 5., &[(theme.primary, f.clamp(0., 1.))]))
}

impl Render for StatsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // The page's own width decides the layout (figures 4 → 2, charts
        // 3 → 2 → 1, Services and Database side by side or stacked).
        let vw = f32::from(window.viewport_size().width);
        let content = vw - if vw < crate::manager::RAIL_BELOW { 64. } else { crate::manager::NAV_W } - 32.;
        let fig_cols = if content >= 760. { 4 } else { 2 };
        let chart_cols = if content >= 760. { 3 } else if content >= 480. { 2 } else { 1 };
        let side_by_side = content >= 820.;

        let mut seg = ui::seg_track(&theme);
        for r in Range::ALL {
            seg = seg.child(ui::seg_item(&theme, SharedString::from(format!("range-{}", r.label())), r.label(), self.range == r).on_click(cx.listener(move |p, _, _, cx| p.pick(r, cx))));
        }
        let me = cx.entity().downgrade();
        let refresh = crate::ui::refresh_button("stats-refresh", self.refreshing, window, move |_, cx| {
            me.update(cx, |p, cx| p.read_again(cx)).ok();
        });
        let dbg = ui::debug_stale().filter(|_| !self.live.is_empty());
        let stale = self.stale.first().map(|(_, e)| e.as_str()).or(dbg.as_deref()).filter(|_| !self.refreshing).map(|e| ui::stale_note(&theme, "stats-stale", e, self.updated_at.as_deref()));
        // Title only; the range and Read again at its right, under it when narrow.
        let header = h_flex()
            .flex_wrap()
            .gap_3()
            .items_center()
            .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Statistics"))
            .child(div().flex_1())
            // The stale note wraps on its own: the range and Refresh never move.
            .children(stale.map(|n| div().max_w_full().child(n)))
            .child(h_flex().gap_2().items_center().child(seg).child(refresh));

        let latest = self.live.last().cloned().or_else(|| self.points().last().cloned());
        let mut col = v_flex().gap(px(crate::ui::GAP)).child(header);
        let page = |col: Div| v_flex().id("stats-page").flex_1().min_w_0().h_full().overflow_y_scroll().p(px(crate::ui::PAGE_PAD)).bg(theme.muted).child(col);
        // One of the Server card's jobs, in the shared job view, until
        // "Back to Statistics".
        if self.server_shown && !matches!(self.server_job.read(cx).state(), crate::jobs::JobState::Idle) {
            let me = cx.entity().downgrade();
            let back = crate::manager::jobview::back_button("srv-job-back", "Statistics", move |_, cx| {
                me.update(cx, |p, cx| {
                    p.server_shown = false;
                    p.server_job.update(cx, |j, cx| j.reset(cx));
                    p.read_server(cx);
                })
                .ok();
            });
            let job = self.server_job.read(cx);
            let retry = match job.state() {
                crate::jobs::JobState::Failed(_) => job.retry().map(|r| crate::ui::button("srv-job-retry").primary().label("Try again").on_click(move |_, _, cx| r(cx)).into_any_element()),
                _ => None,
            };
            let label = self.server_job_label.clone();
            // Nothing to stop half-way (the server restarts by itself once
            // sent): Hide leaves the view, the job goes on in the progress
            // chip, and a failure then opens its details.
            let me = cx.entity().downgrade();
            let hide = crate::manager::jobview::Cancel::Hide(Rc::new(move |_, cx| {
                me.update(cx, |p, cx| {
                    p.server_shown = false;
                    p.server_hidden = true;
                    p.server_job.update(cx, |j, _| j.set_on_page(false));
                    cx.notify();
                })
                .ok();
            }));
            col = col.child(crate::manager::jobview::JobView::new("srv-job", label, &self.server_job).actions(std::iter::once(back).chain(retry)).cancel(hide).render(cx));
            return page(col);
        }
        // Shut down from here: off, said calmly (not as an error), until
        // it answers again.
        if let Source::Remote(i) = &self.src {
            if crate::manager::server_card::power_state(&i.id) == Some(crate::manager::server_card::Power::Off) {
                col = col.child(ui::notice_with(
                    &theme,
                    ui::Tone::Neutral,
                    format!("{} is off.\nIt was shut down from here. Platform Manager cannot switch it on again: someone has to, or use the cloud provider's console.", i.name),
                    Some(crate::ui::button("stats-off-check").outline().label("Check again").on_click(cx.listener(|p, _, _, cx| p.read_again(cx))).into_any_element()),
                ));
                return page(col);
            }
        }
        // Nothing could be read: one notice in the page and nothing else (no
        // empty boxes, no endless "Reading…").
        if latest.is_none() {
            if let Some(e) = &self.live_err {
                let busy = self.job.read(cx).is_running();
                col = col.child(if e == crate::remote::AGENT_OLD {
                    ui::notice_with(
                        &theme,
                        ui::Tone::Warn,
                        "The agent on this server is out of date.\nUpdate it to read the server's statistics. Nothing else on the server changes.",
                        Some(crate::ui::button("stats-agent-update").primary().label("Update agent").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.update_agent(cx))).into_any_element()),
                    )
                } else {
                    ui::notice_with(
                        &theme,
                        ui::Tone::Bad,
                        format!("The statistics could not be read.\n{e}"),
                        Some(crate::ui::button("stats-retry").outline().label("Retry").on_click(cx.listener(|p, _, _, cx| {
                            p.live_err = None;
                            p.start(cx);
                            cx.notify();
                        })).into_any_element()),
                    )
                });
                return page(col);
            }
        }
        if let Some(Err(e)) = &self.hist {
            if self.range != Range::Live {
                col = col.child(ui::notice(&theme, ui::Tone::Warn, format!("The history could not be read.\n{e}")));
            }
        }

        // The four figures: each fills in place as the reading comes.
        match &latest {
            Some(s) => {
                let used = s.disk_total.saturating_sub(s.disk_free);
                let up = (s.t - s.boot).max(0);
                let (n, unit) = if up >= 86_400 { (up / 86_400, if up / 86_400 == 1 { "day" } else { "days" }) } else { (up / 3600, if up / 3600 == 1 { "hour" } else { "hours" }) };
                let started = chrono::DateTime::from_timestamp(s.boot, 0).map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                let figs = [
                    figure(&theme, "Processor", format!("{:.0}", s.cpu), "%".into(), Some((s.cpu / 100., s.cpu_high())), match s.load {
                        Some(l) => format!("{} cores · load {l:.2}", s.cores),
                        None => format!("{} cores", s.cores),
                    }),
                    figure(
                        &theme,
                        "Memory",
                        format!("{:.1}", gb(s.mem_used)),
                        format!("of {:.0} GB", gb(s.mem_total)),
                        Some((if s.mem_total > 0 { s.mem_used as f32 / s.mem_total as f32 } else { 0. }, s.mem_high())),
                        if s.swap_total > 0 { format!("Swap {:.1} of {:.0} GB", gb(s.swap_used), gb(s.swap_total)) } else { "No swap".into() },
                    ),
                    {
                        let free = bytes_label(s.disk_free);
                        let (num, unit) = free.split_once(' ').map(|(a, b)| (a.to_string(), format!("{b} free"))).unwrap_or((free.clone(), "free".into()));
                        figure(&theme, "Disk", num, unit, Some((if s.disk_total > 0 { used as f32 / s.disk_total as f32 } else { 0. }, s.disk_low())), format!("{} of {} used", bytes_label(used), bytes_label(s.disk_total)))
                    },
                    figure(&theme, "Running for", n.to_string(), unit.into(), None, format!("Started {started}")),
                ];
                self.timing("figures drawn");
                col = col.child(grid(fig_cols, figs.into_iter().map(|f| f.flex_1().into_any_element()).collect()));
            }
            None => {
                let figs = ["Processor", "Memory", "Disk", "Running for"].into_iter().enumerate().map(|(i, n)| skeleton_figure(&theme, i, n)).collect();
                col = col.child(grid(fig_cols, figs));
            }
        }

        // The six charts: placeholders until their points come.
        let loading = match self.range {
            Range::Live => self.live.is_empty(),
            _ => self.hist.is_none(),
        };
        let charts: Vec<AnyElement> = if loading {
            ["Processor", "Memory", "Processor load", "Network received", "Network sent", "Disk activity"].into_iter().enumerate().map(|(i, t)| skeleton_chart(&theme, i, t)).collect()
        } else {
            let points = self.points().to_vec();
            let times: Vec<i64> = points.iter().map(|s| s.t).collect();
            charts(&points).into_iter().enumerate().map(|(i, c)| self.chart(i, c, &times, cx)).collect()
        };
        col = col.child(grid(chart_cols, charts));

        // Services and Database: each its own source, a placeholder while
        // it is read, left out when there is nothing in it.
        let services = match &self.services {
            None => Some(ui::skeleton_card(&theme, "stats-svc", Some("Services"), &["", "", ""])),
            Some(Ok(list)) if !list.is_empty() => Some(services_panel(&theme, list, self.live.last())),
            Some(_) => None,
        };
        // The Server card in the Database card's place (the approved mockup).
        let actions = self.server_actions(cx);
        let db = Some(crate::manager::server_card::card(&theme, self.server.as_ref(), self.live.last(), actions));
        #[cfg(debug_assertions)]
        self.debug_dialog(window, cx);
        col = col.child(match (services, db) {
            (Some(services), Some(db)) if side_by_side => {
            // Siblings in a row share its height.
            h_flex()
                .gap(px(crate::ui::GAP))
                .items_stretch()
                .child(div().flex_grow(1.6).flex_basis(px(0.)).min_w_0().flex().flex_col().child(services.flex_1()))
                .child(div().flex_grow(1.).flex_basis(px(0.)).min_w_0().flex().flex_col().child(db.flex_1()))
                .into_any_element()
            }
            (services, db) => v_flex().gap(px(crate::ui::GAP)).children(services).children(db).into_any_element(),
        });

        page(col)
    }
}

/// Items in `cols` equal columns, rows as tall as their tallest item.
fn grid(cols: usize, items: Vec<AnyElement>) -> AnyElement {
    let mut rows = v_flex().gap(px(crate::ui::GAP));
    let mut it = items.into_iter().peekable();
    while it.peek().is_some() {
        let mut row = h_flex().gap(px(crate::ui::GAP)).items_stretch();
        for _ in 0..cols {
            row = row.child(match it.next() {
                Some(el) => div().flex_1().flex_basis(px(0.)).min_w_0().flex().flex_col().child(el).into_any_element(),
                None => div().flex_1().flex_basis(px(0.)).into_any_element(),
            });
        }
        rows = rows.child(row);
    }
    rows.into_any_element()
}

/// `list`: the services; `s`: the computer's reading, for "Everything
/// else" (left out until it comes).
fn services_panel(theme: &gpui_kit::component::Theme, list: &[crate::stats::Service], s: Option<&Sample>) -> Div {
    let mem_total = s.map(|s| s.mem_total as f32).filter(|t| *t > 0.).unwrap_or(f32::MAX);
    let head = |t: &'static str, w: Option<f32>, right: bool| {
        div().when_some(w, |d, w| d.w(px(w)).flex_shrink_0()).when(w.is_none(), |d| d.flex_1().min_w_0()).when(right, |d| d.text_right()).text_size(px(12.)).text_color(theme.muted_foreground).child(t)
    };
    let row = |name: Div, cpu: f32, mem: u64, restarts: Option<u32>, i: usize| {
        h_flex()
            .gap_3()
            .items_center()
            .py(px(6.))
            .when(i > 0, |r| r.border_t_1())
            .border_color(theme.border)
            .text_sm()
            .child(name.flex_1().min_w_0())
            .child(h_flex().w(px(PROC_W)).flex_shrink_0().gap(px(7.)).items_center().child(mini_bar(theme, cpu / 100.)).child(div().font_features(ui::tabular()).whitespace_nowrap().child(format!("{cpu:.1} %"))))
            .child(h_flex().w(px(MEM_W)).flex_shrink_0().gap(px(7.)).items_center().child(mini_bar(theme, mem as f32 / mem_total)).child(div().font_features(ui::tabular()).whitespace_nowrap().child(format!("{:.1} GB", gb(mem)))))
            .child(div().w(px(RESTART_W)).flex_shrink_0().text_right().font_features(ui::tabular()).child(restarts.map(|r| r.to_string()).unwrap_or_default()))
    };
    let mut p = ui::panel(theme)
        .gap_0()
        .min_w_0()
        .child(div().pb(px(7.)).font_weight(FontWeight::MEDIUM).child("Services"))
        .child(h_flex().gap_3().pb(px(5.)).child(head("Service", None, false)).child(head("Processor", Some(PROC_W), false)).child(head("Memory", Some(MEM_W), false)).child(head("Restarts", Some(RESTART_W), true)));
    let mut list = list.to_vec();
    list.sort_by_key(|v| match ui::service_label(&v.name) {
        Some("SENTIENT") => 0,
        Some("Database (PostgreSQL)") => 1,
        Some("ClickHouse") => 2,
        _ => 3,
    });
    let mut cpu_sum = 0.;
    let mut mem_sum = 0u64;
    for (i, v) in list.iter().enumerate() {
        cpu_sum += v.cpu;
        mem_sum += v.mem;
        let plain = ui::service_label(&v.name);
        // The plain name, the container or unit name beside it, or under it
        // when the column is narrow: wrapped, never cut.
        let name = h_flex()
            .flex_wrap()
            .gap_x(px(6.))
            .child(div().child(SharedString::from(plain.unwrap_or(&v.name).to_string())))
            .when(plain.is_some(), |r| r.child(div().text_size(px(11.5)).font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(SharedString::from(v.name.clone()))));
        p = p.child(row(name, v.cpu, v.mem, v.restarts, i));
    }
    let n = list.len();
    match s {
        Some(s) => p.child(row(div().child("Everything else on the server"), (s.cpu - cpu_sum).max(0.), s.mem_used.saturating_sub(mem_sum), None, n)),
        None => p,
    }
}

#[cfg(test)]
mod tests {
    use super::{nice, nice_memory, tick};

    #[test]
    fn scales_are_round_and_labels_distinct() {
        assert_eq!(nice_memory(31.), 32.);
        assert_eq!(nice_memory(7.), 8.);
        assert_eq!(nice(0.3), 0.4);
        assert_eq!(nice(1.), 1.);
        assert_eq!(nice(130.), 200.);
        let labels = |max: f32| [tick(0., max), tick(max / 2., max), tick(max, max)];
        assert_eq!(labels(1.), ["0", "0.5", "1.0"]);
        assert_eq!(labels(32.), ["0", "16", "32"]);
        assert_eq!(labels(5.), ["0", "2.5", "5.0"]);
        assert_eq!(labels(0.4), ["0", "0.2", "0.4"]);
        assert_eq!(labels(100.), ["0", "50", "100"]);
    }
}
