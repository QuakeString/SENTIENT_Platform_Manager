// Copyright © 2016-2026 The SENTIENT Authors

//! The Uninstall dialog (crate::uninstall), here and for a server: one
//! switch per part this installation has, "Keep its data" switches nested
//! under the databases (on by default; turning one off asks for the
//! instance name), the space each part frees, and the "Removes … / Keeps …"
//! lines above Remove. Then the job, then the result: one line per part
//! removed and kept, a "Checked" line, and anything left with the exact
//! command and Copy. The Platform Manager (on a server, its agent) goes
//! last, after the result screen.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputEvent, InputState},
    switch::Switch,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use sentient_installer_core::progress::ProgressFn;
use sentient_installer_core::removal::Check;

use crate::jobs::{Job, JobState};
use crate::uninstall::{self, Choice, Facts, Key, Row};

/// Reads what the computer (or server) has. Blocking; run in the background.
pub type LoadFacts = Arc<dyn Fn() -> Result<Facts, String> + Send + Sync>;
/// Runs `--job <args>` there (`args` starts with `uninstall`).
pub type RunUninstall = Arc<dyn Fn(Vec<String>, ProgressFn) -> Result<(), String> + Send + Sync>;

pub struct Opts {
    /// The instance name to type before data is deleted.
    pub name: String,
    pub load: LoadFacts,
    pub run: RunUninstall,
    /// A server: its rows say so, the agent is removed by the job itself,
    /// and "Remove from Instances" is offered.
    pub remote: bool,
    /// The job ended (the page reloads its record).
    pub on_done: Rc<dyn Fn(&mut App)>,
    /// The result screen was closed: remove the Platform Manager (this
    /// computer), forget the server, restart Windows.
    pub on_close: Rc<dyn Fn(After, &mut App)>,
}

/// What the result screen's button does as it closes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct After {
    pub remove_manager: bool,
    pub forget_instance: bool,
    pub restart: bool,
}

/// Open it (deferred: a click is in the middle of the window's update).
pub fn open(opts: Opts, cx: &mut App) {
    cx.defer(move |cx| {
        let Some(handle) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
        let _ = handle.update(cx, |_, window, cx| {
            let view = cx.new(|cx| UninstallDialog::new(opts, window, cx));
            crate::modal::open(crate::modal::Modal { title: "Uninstall".into(), body: view.into(), width: 640., icon: None, on_close: None }, cx);
        });
    });
}

enum Phase {
    Choose,
    Running,
    Done,
}

pub struct UninstallDialog {
    opts: Opts,
    facts: Option<Result<Facts, String>>,
    rows: Vec<Row>,
    choice: Choice,
    /// A server: forget it here once it is uninstalled.
    forget: bool,
    confirm: Entity<InputState>,
    job: Entity<Job>,
    phase: Phase,
    /// The effective choice the job ran with.
    ran: Choice,
    copied: Option<usize>,
    _subs: Vec<Subscription>,
    _task: Option<Task<()>>,
}

impl UninstallDialog {
    fn new(opts: Opts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let confirm = cx.new(|cx| InputState::new(window, cx).placeholder(opts.name.clone()));
        let job = cx.new(|_| Job::new());
        let subs = vec![
            cx.subscribe_in(&confirm, window, |_, _, e, _, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe(&job, |d: &mut Self, job, cx| {
                if matches!(d.phase, Phase::Running) && matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                    d.phase = Phase::Done;
                    crate::modal::set_title(if d.ran.on.iter().any(|k| !k.is_keep()) { "Removed" } else { "Nothing removed" }, cx);
                    (d.opts.on_done)(cx);
                }
                cx.notify();
            }),
        ];
        let mut d = Self {
            opts,
            facts: None,
            rows: Vec::new(),
            choice: Choice::default(),
            forget: true,
            confirm,
            job,
            phase: Phase::Choose,
            ran: Choice::default(),
            copied: None,
            _subs: subs,
            _task: None,
        };
        d.load(cx);
        d
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let load = self.opts.load.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            // Opening the dialog is the user's action: listing PostgreSQL's
            // databases may ask for the password (once: the job reuses it).
            let got = cx
                .background_executor()
                .spawn(async move {
                    let _action = sentient_installer_core::elevation::user_action();
                    load()
                })
                .await;
            let _ = this.update(cx, |d, cx| {
                if let Ok(f) = &got {
                    d.rows = rows_for(f, d.opts.remote);
                    d.choice = uninstall::default_choice(&d.rows);
                }
                d.facts = Some(got);
                // Screens: the canned job at once (open_fixture's `:done`).
                if cfg!(debug_assertions) && std::env::var_os(AUTORUN).is_some() && d.facts.as_ref().is_some_and(|f| f.is_ok()) {
                    d.choice.set(Key::KeepPgData, false);
                    d.start(cx);
                }
                cx.notify();
            });
        }));
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let e = uninstall::effective(&self.choice, &self.rows);
        let mut run_choice = e.clone();
        // This computer: the Platform Manager goes after the result screen.
        if !self.opts.remote {
            run_choice.set(Key::Manager, false);
        }
        let args = uninstall::to_args(&run_choice, &self.rows);
        self.ran = e;
        self.phase = Phase::Running;
        let run = self.opts.run.clone();
        self.job.update(cx, |job, cx| {
            job.set_label("Uninstalling");
            job.set_on_page(true);
            job.start(move |sink| run(args, sink), cx)
        });
        cx.notify();
    }

    fn after(&self) -> After {
        After { remove_manager: self.ran.has(Key::Manager) && !self.opts.remote, forget_instance: self.opts.remote && self.forget && self.ran.has(Key::Sentient), restart: self.ran.has(Key::Wsl) }
    }
}

/// The rows, with a server's own words.
fn rows_for(f: &Facts, remote: bool) -> Vec<Row> {
    let mut rows = uninstall::rows(f);
    if remote {
        for r in &mut rows {
            if r.key == Key::Manager {
                r.title = "Manager agent on the server".into();
                r.detail = "The Platform Manager there; removed last".into();
                r.name = "Manager agent".into();
                r.default_on = true;
            }
        }
    }
    rows
}

const AUTORUN: &str = "SPM_DEBUG_UNINSTALL_AUTORUN";
const ROW_SWITCH_W: f32 = 44.;
const ROW_SIZE_W: f32 = 72.;
const SUB_INDENT: f32 = 44.;

impl UninstallDialog {
    fn row(&self, theme: &gpui_kit::component::Theme, i: usize, r: &Row, cx: &mut Context<Self>) -> Stateful<Div> {
        let why = uninstall::rule(r.key, &self.choice, &self.rows);
        let enabled = why.is_none();
        let on = enabled && self.choice.has(r.key);
        let key = r.key;
        let size = uninstall::size_text(r, &self.choice, &self.rows);
        let sub = r.parent.is_some();
        let dim = |d: Div| if enabled { d } else { d.opacity(0.55) };
        let mut text = v_flex()
            .flex_1()
            .min_w_0()
            .gap_0p5()
            .child(dim(h_flex().gap_1().items_center().child(div().font_weight(FontWeight::MEDIUM).child(r.title.clone())).when_some(r.help.clone(), |h, help| h.child(crate::ui::info(format!("un-help-{i}").into(), help.into())))))
            .child(dim(div().text_xs().text_color(theme.muted_foreground).child(r.detail.clone())));
        if let Some(w) = why.filter(|w| !w.is_empty()) {
            text = text.child(div().text_xs().text_color(theme.warning_foreground).child(w));
        }
        if let Some(x) = &r.extra {
            text = text.child(dim(div().text_xs().text_color(theme.muted_foreground).child(x.clone())));
        }
        h_flex()
            .id(SharedString::from(format!("un-row-{i}")))
            .items_start()
            .gap_3()
            .py(px(10.))
            .px_3()
            .when(sub, |d| d.pl(px(12. + SUB_INDENT)).bg(theme.secondary))
            .when(i > 0, |d| d.border_t_1().border_color(theme.border))
            .child(
                div().w(px(ROW_SWITCH_W)).flex_shrink_0().pt(px(2.)).child(
                    Switch::new(SharedString::from(format!("un-sw-{}", key.id())))
                        .checked(on)
                        .disabled(!enabled || !matches!(self.phase, Phase::Choose))
                        .on_click(cx.listener(move |d, v: &bool, _, cx| {
                            d.choice.set(key, *v);
                            cx.notify();
                        })),
                ),
            )
            .child(text)
            .child(dim(div().w(px(ROW_SIZE_W)).flex_shrink_0().pt(px(2.)).text_sm().text_color(theme.muted_foreground).flex().justify_end().child(size)))
    }

    fn choose(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let rows: Vec<Row> = self.rows.clone();
        let mut list = v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden();
        for (i, r) in rows.iter().enumerate() {
            list = list.child(self.row(theme, i, r, cx));
        }
        if self.opts.remote {
            let on = self.forget;
            list = list.child(
                h_flex()
                    .items_start()
                    .gap_3()
                    .py(px(10.))
                    .px_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().w(px(ROW_SWITCH_W)).flex_shrink_0().pt(px(2.)).child(Switch::new("un-sw-forget").checked(on).on_click(cx.listener(|d, v: &bool, _, cx| {
                        d.forget = *v;
                        cx.notify();
                    }))))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().font_weight(FontWeight::MEDIUM).child("Remove from Instances"))
                            .child(div().text_xs().text_color(theme.muted_foreground).child("Forget this server and its SSH key here")),
                    )
                    .child(div().w(px(ROW_SIZE_W)).flex_shrink_0()),
            );
        }
        let (removes, keeps) = uninstall::summary(&self.choice, &self.rows);
        let e = uninstall::effective(&self.choice, &self.rows);
        let anything = e.on.iter().any(|k| !k.is_keep());
        let deletes = uninstall::deletes_data(&self.choice, &self.rows);
        let typed = self.confirm.read(cx).value().trim().to_string();
        let confirmed = !deletes || typed == self.opts.name;
        let restart = e.has(Key::Wsl);
        let label: SharedString = if restart { "Remove and restart".into() } else { "Remove".into() };
        let ok_enabled = anything && confirmed;
        v_flex()
            .gap_3()
            .child(list)
            .child(
                v_flex()
                    .id("un-summary")
                    .gap_1()
                    .px_3()
                    .py(px(10.))
                    .rounded(px(8.))
                    .bg(theme.secondary)
                    .text_sm()
                    .child(div().text_color(theme.danger).child(removes))
                    .child(div().text_color(theme.success_foreground).child(keeps)),
            )
            .when(deletes, |c| {
                c.child(
                    v_flex()
                        .gap_1()
                        .text_sm()
                        .child(h_flex().gap_1().items_center().child(format!("Type {} to delete the data", self.opts.name)).child(crate::ui::info("un-confirm-help".into(), "A Keep its data switch is off, or a database is picked: its data cannot be brought back. Typing the instance name confirms it.".into())))
                        .child(Input::new(&self.confirm)),
                )
            })
            .child(
                h_flex()
                    .gap(crate::ui::button_gap())
                    .justify_end()
                    .child(crate::ui::probed(crate::ui::button("un-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx))))
                    .child(crate::ui::probed(if ok_enabled {
                        crate::modal::danger_button(theme, "un-remove", label, cx.listener(|d, _, _, cx| d.start(cx))).into_any_element()
                    } else {
                        crate::ui::button("un-remove").danger().label(label).disabled(true).into_any_element()
                    })),
            )
    }

    fn running(&self, theme: &gpui_kit::component::Theme, cx: &App) -> Div {
        let job = self.job.read(cx);
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(crate::ui::skeleton_block(theme, "un-run", px(14.).into(), 14.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(job.step_with_time())),
            )
            .child(crate::ui::steps(theme, &crate::ui::mark_steps(&[], job.steps(), None)))
    }

    fn done(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let job = self.job.read(cx);
        let log: Vec<String> = job.log().iter().map(|s| s.to_string()).collect();
        let checks: Vec<Check> = uninstall::checks_in(&log);
        let failed = match job.state() {
            JobState::Failed(m) => Some(m.clone()),
            _ => None,
        };
        let (gone, kept, _) = uninstall::gone_and_kept(&self.ran, &self.rows);
        let line = |id: String, mark: &'static str, fg: Hsla, bg: Hsla, text: SharedString| {
            h_flex()
                .id(SharedString::from(id))
                .gap(px(10.))
                .items_start()
                .py(px(8.))
                .px_3()
                .text_sm()
                .child(div().size(px(20.)).flex_shrink_0().rounded_full().bg(bg).text_color(fg).text_xs().font_weight(FontWeight::BOLD).flex().items_center().justify_center().child(mark))
                .child(div().flex_1().min_w_0().pt(px(1.)).child(text))
        };
        let (ok_fg, ok_bg) = (theme.success_foreground, theme.success);
        let (kp_fg, kp_bg) = (theme.primary, theme.primary.opacity(0.12));
        let (bad_fg, bad_bg) = (theme.danger, theme.danger.opacity(0.14));
        let mut list = v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden();
        let mut n = 0;
        let mut push = |list: Div, el: Stateful<Div>| {
            n += 1;
            list.child(el.when(n > 1, |d| d.border_t_1().border_color(theme.border)))
        };
        if let Some(m) = &failed {
            list = push(list, line("un-res-fail".into(), "!", bad_fg, bad_bg, m.clone()));
        } else {
            for (i, g) in gone.iter().enumerate() {
                list = push(list, line(format!("un-res-g{i}"), "✓", ok_fg, ok_bg, format!("{} removed", capital(g)).into()));
            }
            if let Some(c) = uninstall::checked_line(&checks) {
                list = push(list, line("un-res-checked".into(), "✓", ok_fg, ok_bg, c.into()));
            }
            for (i, k) in kept.iter().enumerate() {
                list = push(list, line(format!("un-res-k{i}"), "•", kp_fg, kp_bg, format!("{} kept", capital(k)).into()));
            }
        }
        for (i, c) in checks.iter().enumerate().filter(|(_, c)| !c.ok) {
            let el = line(format!("un-res-b{i}"), "!", bad_fg, bad_bg, c.what.clone().into());
            let el = match c.fix.clone() {
                None => el,
                Some(fix) => {
                    let copied = self.copied == Some(i);
                    let f2 = fix.clone();
                    v_flex().id(SharedString::from(format!("un-res-bb{i}"))).child(el).child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .pl(px(12. + 30.))
                            .pr_3()
                            .pb(px(8.))
                            .child(div().flex_1().min_w_0().px(px(10.)).py(px(8.)).rounded(px(8.)).border_1().border_color(theme.border).bg(theme.secondary).font_family("monospace").text_xs().overflow_hidden().child(fix))
                            .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("un-copy-{i}"))).outline().small().label(if copied { "Copied" } else { "Copy" }).on_click(cx.listener(move |d, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(f2.clone()));
                                d.copied = Some(i);
                                cx.notify();
                            })))),
                    )
                }
            };
            list = push(list, el);
        }
        if self.ran.has(Key::Wsl) && failed.is_none() {
            list = push(list, line("un-res-wsl".into(), "!", bad_fg, bad_bg, "WSL is off after Windows restarts.".into()));
        }
        let after = self.after();
        let label: SharedString = if after.remove_manager {
            "Close and remove Platform Manager".into()
        } else if after.restart {
            "Restart Windows".into()
        } else {
            "Close".into()
        };
        let on_close = self.opts.on_close.clone();
        v_flex().gap_3().child(list).child(
            h_flex().gap(crate::ui::button_gap()).justify_end().child(crate::ui::probed(crate::ui::button("un-close").primary().label(label).on_click(move |_, _, cx| {
                crate::modal::close(cx);
                on_close(after, cx);
            }))),
        )
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

impl Render for UninstallDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let body = match (&self.facts, &self.phase) {
            (None, _) => v_flex()
                .gap_2()
                .child(h_flex().gap_2().items_center().text_sm().text_color(theme.muted_foreground).child(crate::ui::skeleton_block(&theme, "un-load", px(14.).into(), 14.)).child("Looking at what is installed…"))
                .children(crate::ui::skeleton_service_rows(&theme, "un-load-rows", 5)),
            (Some(Err(e)), _) => v_flex()
                .gap_3()
                .child(crate::ui::outcome(&theme, crate::ui::Tone::Bad, format!("What is installed could not be read: {e}")))
                .child(h_flex().justify_end().child(crate::ui::probed(crate::ui::button("un-cancel").outline().label("Close").on_click(|_, _, cx| crate::modal::close(cx))))),
            (Some(Ok(_)), Phase::Choose) => self.choose(&theme, cx),
            (Some(Ok(_)), Phase::Running) => self.running(&theme, cx),
            (Some(Ok(_)), Phase::Done) => self.done(&theme, cx),
        };
        div().child(body)
    }
}

/// Screens: the dialog for one canned deployment (crate::uninstall::FIXTURES;
/// `<name>:done` runs a canned job and shows its result).
pub fn open_fixture(spec: &str, cx: &mut App) {
    let (name, done) = match spec.split_once(':') {
        Some((n, d)) => (n.to_string(), d == "done"),
        None => (spec.to_string(), false),
    };
    let Some(facts) = uninstall::fixture(&name) else { return };
    let remote = name == "remote";
    let f2 = facts.clone();
    let run: RunUninstall = Arc::new(move |_args, sink| {
        use sentient_installer_core::progress::Progress;
        let lines = [
            Check { what: "No PostgreSQL process running".into(), ok: true, left: None, fix: None },
            Check { what: "Port 5432 (PostgreSQL) is free".into(), ok: true, left: None, fix: None },
            Check { what: "No postgresql.service service".into(), ok: true, left: None, fix: None },
            Check { what: "/etc/postgresql/17 is still there".into(), ok: false, left: Some("/etc/postgresql/17".into()), fix: Some("sudo rm -r /etc/postgresql/17".into()) },
        ];
        for c in lines {
            sink(Progress::Log { line: c.encode() });
        }
        sink(Progress::Done { message: "Removed.".into() });
        Ok(())
    });
    let opts = Opts {
        name: if remote { "plant-b".into() } else { "st-dev".into() },
        load: Arc::new(move || Ok(f2.clone())),
        run,
        remote,
        on_done: Rc::new(|_| {}),
        on_close: Rc::new(|_, _| {}),
    };
    if done {
        std::env::set_var(AUTORUN, "1");
    }
    open(opts, cx);
    let _ = facts;
}

/// This computer's result screen closed: the Platform Manager goes last
/// (the program quits; its files go once it has), or Windows restarts to
/// finish turning WSL off.
pub fn finish_local(after: After, cx: &mut App) {
    if after.remove_manager {
        cx.spawn(async move |cx| {
            let sink = sentient_installer_core::progress::noop();
            cx.background_spawn(async move { uninstall::remove_manager_last(&sink) }).await;
            if after.restart {
                restart_windows();
            }
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    } else if after.restart {
        restart_windows();
    }
}

fn restart_windows() {
    #[cfg(windows)]
    let _ = std::process::Command::new("shutdown.exe").args(["/r", "/t", "5", "/c", "Restarting to finish turning WSL off"]).spawn();
}

/// The dialog for this computer's installation (the Platform Manager's
/// Repair page and Setup's maintenance screen). On native Linux the job is
/// one root job (one password); the shortcuts in this account's home go as
/// this account after it.
pub fn open_local(record: crate::installation::InstallationRecord, on_done: Rc<dyn Fn(&mut App)>, cx: &mut App) {
    let r1 = record.clone();
    let load: LoadFacts = Arc::new(move || Ok(uninstall::facts_full(&r1)));
    let r2 = record;
    let run: RunUninstall = Arc::new(move |args: Vec<String>, sink| {
        if crate::rootjob::needed(&r2) {
            crate::rootjob::run(&args, &sink)?;
            if args.iter().any(|a| a.split(',').any(|p| p == "shortcuts")) {
                crate::shortcuts::remove(crate::shortcuts::Owner::Sentient, &r2.shortcuts, &sink);
            }
            Ok(())
        } else {
            let choice = uninstall::from_args(&args[1..])?;
            let mut facts = uninstall::facts_quick(&r2);
            facts.pg_others = Some(uninstall::pg_others(&r2));
            uninstall::run(r2.clone(), choice, facts, sink)
        }
    });
    open(Opts { name: uninstall::computer_name(), load, run, remote: false, on_done, on_close: Rc::new(finish_local) }, cx);
}
