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
use std::time::Duration;

use gpui_kit::component::{ActiveTheme, Disableable as _, button::ButtonVariants as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::dbstatus::bytes_label;
use crate::installation::InstallationRecord;
use crate::stats::Sample;
use crate::ui;

/// Live: one reading every this many seconds, the last LIVE_KEEP kept (about
/// five minutes).
const LIVE_EVERY: Duration = Duration::from_secs(3);
const LIVE_KEEP: usize = 100;
/// The longer ranges read their history again this often while shown.
const HISTORY_EVERY: Duration = Duration::from_secs(60);

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
    fn live(&self) -> Result<Sample, String> {
        match self {
            Source::Local(r) => {
                #[cfg(debug_assertions)]
                if let Some(d) = std::env::var_os("SPM_DEBUG_STATS") {
                    return crate::stats::debug_live(std::path::Path::new(&d));
                }
                Ok(crate::stats::read(r.as_ref(), Duration::from_secs(1), true))
            }
            Source::Remote(i) => crate::remote::stats_live(i),
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
    live: Vec<Sample>,
    hist: Option<Result<Vec<Sample>, String>>,
    /// The last 24 hours, for the database's growth and "Disk full in".
    day: Vec<Sample>,
    live_err: Option<String>,
    /// (chart, point) under the pointer.
    hover: Option<(usize, usize)>,
    /// Each chart's plot bounds, for the pointer.
    plots: Vec<Rc<Cell<Bounds<Pixels>>>>,
    /// Update agent (a server whose agent is too old for these readings).
    job: Entity<crate::jobs::Job>,
    _watch: Option<Subscription>,
    _poll: Option<Task<()>>,
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
        let mut p = Self {
            src,
            range,
            live: Vec::new(),
            hist: None,
            day: Vec::new(),
            live_err: None,
            hover: None,
            plots: (0..6).map(|_| Rc::new(Cell::new(Bounds::default()))).collect(),
            job: cx.new(|_| crate::jobs::Job::new()),
            _watch: None,
            _poll: None,
            _tasks: Vec::new(),
        };
        p.start(cx);
        p.read_day(cx);
        p
    }

    /// Read now and then on the range's rhythm, while the page is shown.
    fn start(&mut self, cx: &mut Context<Self>) {
        let src = self.src.clone();
        let range = self.range;
        if range != Range::Live {
            self.hist = None;
        }
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
        self._poll = Some(cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                if first || OPEN.load(Ordering::Relaxed) {
                    first = false;
                    let s = src.clone();
                    if range == Range::Live {
                        let got = cx.background_executor().spawn(async move { s.live() }).await;
                        let ok = this
                            .update(cx, |p, cx| {
                                match got {
                                    Ok(sample) => {
                                        p.live.push(sample);
                                        if p.live.len() > LIVE_KEEP {
                                            p.live.remove(0);
                                        }
                                        p.live_err = None;
                                    }
                                    Err(e) => p.live_err = Some(e),
                                }
                                cx.notify();
                            })
                            .is_ok();
                        if !ok {
                            break;
                        }
                    } else {
                        // The history, and one live reading for the figures,
                        // Services and Database above and below the charts.
                        let s2 = s.clone();
                        let got = cx.background_executor().spawn(async move { s.history(range.secs()) }).await;
                        let now = cx.background_executor().spawn(async move { s2.live() }).await;
                        if this.update(cx, |p, cx| {
                            p.hist = Some(got);
                            if let Ok(sample) = now {
                                p.live.push(sample);
                                if p.live.len() > LIVE_KEEP {
                                    p.live.remove(0);
                                }
                            }
                            cx.notify();
                        })
                        .is_err()
                        {
                            break;
                        }
                    }
                }
                cx.background_executor().timer(if range == Range::Live { LIVE_EVERY } else { HISTORY_EVERY }).await;
            }
        }));
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
                p.read_day(cx);
            }
            cx.notify();
        });
        let _ = me2;
        self._watch = Some(watch);
        cx.notify();
    }

    fn read_day(&mut self, cx: &mut Context<Self>) {
        let src = self.src.clone();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { src.history(86_400) }).await;
            this.update(cx, |p, cx| {
                p.day = got.unwrap_or_default();
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
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
                    .gap(px(6.))
                    .items_start()
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
        let refresh = crate::ui::refresh_button("stats-refresh", false, window, move |_, cx| {
            me.update(cx, |p, cx| {
                p.start(cx);
                p.read_day(cx);
            })
            .ok();
        });
        // Title only; the range and Read again at its right, under it when narrow.
        let header = h_flex()
            .flex_wrap()
            .gap_3()
            .items_center()
            .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Statistics"))
            .child(div().flex_1())
            .child(h_flex().gap_2().items_center().child(seg).child(refresh));

        let latest = self.live.last().cloned().or_else(|| self.points().last().cloned());
        let mut col = v_flex().gap(px(crate::ui::GAP)).child(header);
        let page = |col: Div| v_flex().id("stats-page").flex_1().min_w_0().h_full().overflow_y_scroll().p(px(crate::ui::PAGE_PAD)).bg(theme.muted).child(col);
        // Nothing read: one notice in the page and nothing else (no empty
        // boxes, no endless "Reading…").
        if latest.is_none() {
            let busy = self.job.read(cx).is_running();
            col = col.child(match &self.live_err {
                Some(e) if e == crate::remote::AGENT_OLD => ui::notice_with(
                    &theme,
                    ui::Tone::Warn,
                    "The agent on this server is out of date.\nUpdate it to read the server's statistics. Nothing else on the server changes.",
                    Some(crate::ui::button("stats-agent-update").primary().label("Update agent").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.update_agent(cx))).into_any_element()),
                ),
                Some(e) => ui::notice_with(
                    &theme,
                    ui::Tone::Bad,
                    format!("The statistics could not be read.\n{e}"),
                    Some(crate::ui::button("stats-retry").outline().label("Retry").on_click(cx.listener(|p, _, _, cx| {
                        p.live_err = None;
                        p.start(cx);
                        p.read_day(cx);
                        cx.notify();
                    })).into_any_element()),
                ),
                // Loading: the page's final layout with placeholders.
                None => {
                    let fig = |i: usize, name: &'static str| {
                        ui::panel(&theme)
                            .flex_1()
                            .gap_0()
                            .child(div().text_sm().text_color(theme.muted_foreground).child(name))
                            .child(div().h(px(30.)).flex().items_center().child(ui::skeleton_block(&theme, format!("fig-{i}"), px(70.).into(), 18.)))
                            .child(div().mt(px(7.)).child(ui::skeleton_block(&theme, format!("figbar-{i}"), relative(1.).into(), 5.)))
                            .child(div().mt(px(5.)).child(ui::skeleton_line(&theme, format!("figline-{i}"), 110.)))
                            .into_any_element()
                    };
                    let figs = ["Processor", "Memory", "Disk", "Running for"].into_iter().enumerate().map(|(i, n)| fig(i, n)).collect();
                    let chart = |i: usize, title: &'static str| {
                        ui::panel(&theme)
                            .flex_1()
                            .gap_1()
                            .child(div().font_weight(FontWeight::MEDIUM).child(title))
                            .child(ui::skeleton_block(&theme, format!("plot-{i}"), relative(1.).into(), 96.))
                            .child(div().h(px(15.)))
                            .into_any_element()
                    };
                    let charts = ["Processor", "Memory", "Processor load", "Network received", "Network sent", "Disk activity"].into_iter().enumerate().map(|(i, t)| chart(i, t)).collect();
                    v_flex()
                        .gap(px(crate::ui::GAP))
                        .child(grid(fig_cols, figs))
                        .child(grid(chart_cols, charts))
                        .child(ui::skeleton_card(&theme, "stats-svc", Some("Services"), &["", "", ""]))
                }
            });
            return page(col);
        }
        if let Some(Err(e)) = &self.hist {
            if self.range != Range::Live {
                col = col.child(ui::notice(&theme, ui::Tone::Warn, format!("The history could not be read.\n{e}")));
            }
        }

        // The four figures.
        if let Some(s) = &latest {
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
            col = col.child(grid(fig_cols, figs.into_iter().map(|f| f.flex_1().into_any_element()).collect()));
        }

        // The six charts.
        let points = self.points().to_vec();
        let times: Vec<i64> = points.iter().map(|s| s.t).collect();
        let charts: Vec<AnyElement> = charts(&points).into_iter().enumerate().map(|(i, c)| self.chart(i, c, &times, cx)).collect();
        col = col.child(grid(chart_cols, charts));

        // Services and Database: each only with something in it.
        let services = latest.as_ref().filter(|s| !s.services.is_empty()).map(|s| services_panel(&theme, s));
        let db = latest.as_ref().filter(|s| s.db.is_some()).map(|_| database_panel(&theme, latest.as_ref(), &self.day));
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

fn services_panel(theme: &gpui_kit::component::Theme, s: &Sample) -> Div {
    let mem_total = s.mem_total.max(1) as f32;
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
    let mut list = s.services.clone();
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
    p.child(row(div().child("Everything else on the server"), (s.cpu - cpu_sum).max(0.), s.mem_used.saturating_sub(mem_sum), None, n))
}

fn database_panel(theme: &gpui_kit::component::Theme, latest: Option<&Sample>, day: &[Sample]) -> Div {
    let mut p = ui::panel(theme).gap(px(6.)).min_w_0().child(div().pb_1().font_weight(FontWeight::MEDIUM).child("Database"));
    let fact = |label: &'static str, value: String| {
        h_flex().gap_3().text_sm().child(div().w(px(118.)).flex_shrink_0().text_color(theme.muted_foreground).child(label)).child(div().min_w_0().child(value))
    };
    let Some(db) = latest.and_then(|s| s.db.clone()) else {
        return p.child(div().text_sm().text_color(theme.muted_foreground).child("Reading the database…"));
    };
    // The oldest reading of the last 24 hours that has the database's sizes.
    let then = day.iter().find(|s| s.db.is_some());
    let growth = |now: u64, before: Option<u64>| match before {
        Some(b) if now >= b => format!(", +{} in 24 hours", bytes_label(now - b)),
        Some(b) => format!(", -{} in 24 hours", bytes_label(b - now)),
        None => String::new(),
    };
    p = p.child(fact("Connections", format!("{} open of {} allowed", db.connections, db.max_connections))).child(fact(
        if db.timescale { "TimescaleDB size" } else { "PostgreSQL size" },
        format!("{}{}", bytes_label(db.pg_bytes), growth(db.pg_bytes, then.and_then(|s| s.db.as_ref().map(|d| d.pg_bytes)))),
    ));
    if let Some(ch) = db.ch_bytes {
        p = p.child(fact("ClickHouse size", format!("{}{}", bytes_label(ch), growth(ch, then.and_then(|s| s.db.as_ref().and_then(|d| d.ch_bytes))))));
    }
    let full = match (latest, day.first()) {
        (Some(now), Some(first)) if now.t - first.t >= 3600 => {
            let used_per_day = (first.disk_free as f64 - now.disk_free as f64) / ((now.t - first.t) as f64 / 86_400.);
            if used_per_day <= 0. {
                "not filling up".to_string()
            } else {
                let days = now.disk_free as f64 / used_per_day;
                if days > 365. { "more than a year at this rate".into() } else { format!("about {} days at this rate", days.round() as i64) }
            }
        }
        _ => "known after an hour of history".into(),
    };
    p = p.child(fact("Disk full in", full));
    let last = match &db.last_backup {
        Some((at, bytes)) => {
            let when = chrono::DateTime::parse_from_rfc3339(at).map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|_| at.clone());
            match bytes {
                Some(b) => format!("{when}, {}", bytes_label(*b)),
                None => when,
            }
        }
        None => "None".into(),
    };
    p.child(fact("Last backup", last))
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
