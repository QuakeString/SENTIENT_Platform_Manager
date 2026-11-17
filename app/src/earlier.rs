// Copyright © 2016-2026 The SENTIENT Authors

//! An earlier SENTIENT install that is broken or was half removed, found by
//! Setup with or without an installation record. One screen with the three
//! ways out, each saying what happens to the data:
//!
//! - Remove it and install fresh: everything listed goes (a backup first
//!   when the database can still be read), the ports are checked free, then
//!   the normal install runs.
//! - Repair it: keeps the databases; puts the program files and services
//!   back and starts SENTIENT (an install without a record is adopted
//!   first). When that fails, the screen says why and offers the fresh
//!   install.
//! - Install fresh, keep the old database: the new install uses the
//!   database that is already here.
//!
//! A port is asked about only when another program holds it (the install
//! wizard names that program); ports held by the earlier install itself go
//! with it.

use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::checks;
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

/// What was found. Blocking to make: build it off the UI thread.
#[derive(Clone)]
pub struct Found {
    /// One line per thing found, with its state.
    pub items: Vec<String>,
    /// The installation: its record, or one found without a record (adopt).
    record: Option<InstallationRecord>,
    candidate: Option<crate::adopt::Candidate>,
    /// A PostgreSQL database is there to keep.
    pub has_database: bool,
    /// Why no backup can be made first, when it cannot.
    no_backup: Option<String>,
    pg_dir: PathBuf,
}

impl Found {
    /// Look at what an earlier install left. `record` is the installation
    /// record when there is one; `pg_dir` the PostgreSQL folder Setup would
    /// use by default.
    pub fn look(record: Option<InstallationRecord>, pg_dir: PathBuf) -> Found {
        let pg_dir = record.as_ref().map(|r| r.pg_dir.clone()).filter(|p| !p.as_os_str().is_empty()).unwrap_or(pg_dir);
        let items = crate::leftovers::find(&pg_dir);
        let candidate = if record.is_none() { crate::adopt::find().into_iter().find(|c| c.problem.is_none()) } else { None };
        let usable = record.clone().or_else(|| candidate.as_ref().map(|c| c.record.clone()));
        let has_database = items.iter().any(|i| i.starts_with("PostgreSQL data")) || record.is_some();
        let no_backup = match &usable {
            None => Some("Not possible: no SENTIENT database could be found to copy.".to_string()),
            Some(r) => {
                let port = if r.stack.pg_port == 0 { 5432 } else { r.stack.pg_port };
                if !checks::port_busy(port) {
                    Some("Not possible: PostgreSQL does not run, so the database cannot be read.".to_string())
                } else if crate::migration::Setup::current(r).is_err() {
                    Some("Not possible: the database password is not known on this computer.".to_string())
                } else {
                    None
                }
            }
        };
        Found { items, record, candidate, has_database, no_backup, pg_dir }
    }

    /// Screens (SPM_DEBUG_EARLIER): what rc2's clean-VM run found, the
    /// database not readable.
    #[cfg(debug_assertions)]
    pub fn debug() -> Found {
        Found {
            items: vec!["PostgreSQL service (sentient-postgresql): stopped".into(), r"PostgreSQL data in C:\SENTIENT\data (1.1 GB)".into()],
            record: None,
            candidate: None,
            has_database: true,
            no_backup: Some("Not possible: no SENTIENT database could be found to copy.".into()),
            pg_dir: PathBuf::new(),
        }
    }

    /// Something is there.
    pub fn any(&self) -> bool {
        !self.items.is_empty()
    }

    fn can_repair(&self) -> bool {
        self.record.is_some() || self.candidate.is_some()
    }
}

fn backup_dir() -> PathBuf {
    #[cfg(windows)]
    return PathBuf::from(r"C:\SENTIENT-backups");
    #[cfg(not(windows))]
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/var/backups")).join("SENTIENT-backups")
}

/// The ports a new install needs from the earlier one.
fn default_ports() -> Vec<(u16, String)> {
    let d = sentient_installer_core::distro::DeployConfig::default();
    vec![
        (d.http_port, "HTTP".into()),
        (d.pg_port, "PostgreSQL".into()),
        (d.ch_http_port, "ClickHouse HTTP".into()),
        (d.ch_native_port, "ClickHouse native".into()),
    ]
}

fn ports_line() -> String {
    let ports: Vec<String> = default_ports().iter().map(|(p, _)| p.to_string()).collect();
    let (last, rest) = ports.split_last().map(|(l, r)| (l.clone(), r.join(", "))).unwrap_or_default();
    format!("Checking that ports {rest} and {last} are free")
}

const BACKUP: &str = "Saving a backup";
const REMOVE: &str = "Removing the earlier install";
const CONTINUE: &str = "Continuing with the install";
const REPAIR: &str = "Repairing";
const ADOPT: &str = "Finding the installation's settings";
const KEEP: &str = "Removing the earlier program, keeping its database";

/// Remove everything found (a backup first when asked), then make sure the
/// default ports are free.
fn remove_fresh(found: Found, backup: bool, sink: ProgressFn) -> Result<(), String> {
    if backup {
        sink(Progress::Step { name: BACKUP.into() });
        let r = found.record.clone().or_else(|| found.candidate.as_ref().map(|c| c.record.clone())).ok_or("no database to copy")?;
        let dir = backup_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let file = dir.join(format!("sentient-before-reinstall-{}.sentient-backup", chrono::Local::now().format("%Y-%m-%d_%H%M")));
        crate::backup_jobs::backup(&r, &file, nested(&sink))?;
        sink(Progress::Log { line: format!("Backup saved: {}", file.display()) });
    }
    sink(Progress::Step { name: REMOVE.into() });
    match &found.record {
        Some(r) => crate::maintenance_ops::run_uninstall(r.clone(), false, nested(&sink))?,
        None => crate::leftovers::remove(found.pg_dir.clone(), nested(&sink))?,
    }
    let check = ports_line();
    sink(Progress::Step { name: check });
    let busy: Vec<String> = default_ports()
        .into_iter()
        .filter(|(p, _)| checks::port_busy(*p))
        .map(|(p, what)| match checks::port_owner(p) {
            Some(o) => format!("{p} ({what}, used by {})", o.describe()),
            None => format!("{p} ({what})"),
        })
        .collect();
    if !busy.is_empty() {
        sink(Progress::Log { line: format!("Still in use: {}. The install asks for other ports for these.", busy.join(", ")) });
    }
    sink(Progress::Step { name: CONTINUE.into() });
    Ok(())
}

fn repair(found: Found, sink: ProgressFn) -> Result<(), String> {
    let record = match found.record {
        Some(r) => r,
        None => {
            sink(Progress::Step { name: ADOPT.into() });
            let c = found.candidate.ok_or("No SENTIENT service is left to repair.")?;
            crate::adopt::adopt(&c, None, nested(&sink))?
        }
    };
    sink(Progress::Step { name: REPAIR.into() });
    crate::maintenance_ops::run_repair(record, nested(&sink))
}

fn keep_database(found: Found, sink: ProgressFn) -> Result<(), String> {
    if let Some(r) = found.record {
        sink(Progress::Step { name: KEEP.into() });
        crate::maintenance_ops::run_uninstall(r, true, nested(&sink))?;
    }
    sink(Progress::Step { name: CONTINUE.into() });
    Ok(())
}

/// Steps of a nested engine call as log lines, so the checklist keeps the
/// steps this screen planned.
fn nested(sink: &ProgressFn) -> ProgressFn {
    let sink = sink.clone();
    std::sync::Arc::new(move |p| match p {
        Progress::Step { name } => sink(Progress::Log { line: format!(">> {name}") }),
        Progress::Done { message } => sink(Progress::Log { line: message }),
        other => sink(other),
    })
}

/// Go on with the normal install.
pub struct Continue;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    Remove,
    Repair,
    Keep,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Choices,
    ConfirmRemove,
    Running(Run),
}

pub struct EarlierPage {
    found: Found,
    screen: Screen,
    backup_first: bool,
    understood: bool,
    planned: Vec<String>,
    show_log: bool,
    job: Entity<Job>,
    _observe: Subscription,
}

impl EventEmitter<Continue> for EarlierPage {}

impl EarlierPage {
    pub fn new(found: Found, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |page: &mut Self, job, cx| {
            if let (JobState::Done(_), Screen::Running(run)) = (job.read(cx).state(), page.screen) {
                if run != Run::Repair {
                    cx.emit(Continue);
                }
            }
            cx.notify();
        });
        let backup_first = found.no_backup.is_none();
        // Screens: SPM_DEBUG_EARLIER=confirm opens on the confirm screen.
        #[cfg(debug_assertions)]
        let screen = if std::env::var("SPM_DEBUG_EARLIER").as_deref() == Ok("confirm") { Screen::ConfirmRemove } else { Screen::Choices };
        #[cfg(not(debug_assertions))]
        let screen = Screen::Choices;
        Self { found, screen, backup_first, understood: false, planned: Vec::new(), show_log: false, job, _observe }
    }

    fn start(&mut self, run: Run, cx: &mut Context<Self>) {
        let found = self.found.clone();
        let backup = self.backup_first && found.no_backup.is_none();
        let planned = match run {
            Run::Remove => {
                let mut p = Vec::new();
                if backup {
                    p.push(BACKUP.to_string());
                }
                p.extend([REMOVE.to_string(), ports_line(), CONTINUE.to_string()]);
                p
            }
            Run::Repair if found.record.is_none() => vec![ADOPT.into(), REPAIR.into()],
            Run::Repair => vec![REPAIR.into()],
            Run::Keep if found.record.is_some() => vec![KEEP.into(), CONTINUE.into()],
            Run::Keep => vec![CONTINUE.into()],
        };
        self.planned = planned.clone();
        // Its steps from the first frame (the shared job view).
        self.job.update(cx, |job, _| job.set_plan(planned.into_iter().map(sentient_installer_core::progress::PlanStep::new).collect()));
        self.screen = Screen::Running(run);
        self.show_log = false;
        self.job.update(cx, |job, cx| match run {
            Run::Remove => job.start(move |sink| remove_fresh(found, backup, sink), cx),
            Run::Repair => job.start(move |sink| repair(found, sink), cx),
            Run::Keep => job.start(move |sink| keep_database(found, sink), cx),
        });
        cx.notify();
    }

    fn found_list(&self, theme: &gpui_kit::component::Theme) -> Div {
        ui::panel(theme)
            .gap_1()
            .children(self.found.items.iter().map(|i| h_flex().gap_2().text_sm().child("•").child(div().min_w_0().child(i.clone()))))
    }

    fn choice(theme: &gpui_kit::component::Theme, id: &'static str, icon: Icon, title: &'static str, about: String, enabled: bool) -> Stateful<Div> {
        h_flex()
            .id(id)
            .gap_4()
            .p_4()
            .rounded(theme.radius)
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .when(enabled, |c| c.cursor_pointer().hover(|c| c.border_color(theme.primary)))
            .when(!enabled, |c| c.opacity(0.55))
            .child(icon.text_color(theme.primary))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(about)),
            )
    }

    fn choices(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let repair_ok = self.found.can_repair();
        let keep_ok = self.found.has_database;
        vec![
            div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child("An earlier SENTIENT install is on this computer").into_any_element(),
            div().text_color(theme.muted_foreground).child("It is not working. This is what was found:").into_any_element(),
            self.found_list(theme).into_any_element(),
            Self::choice(
                theme,
                "earlier-remove",
                Icon::new(Lucide::Trash),
                "Remove it and install fresh",
                "Deletes the old services, PostgreSQL, ClickHouse and every database in them. Frees their ports. Then installs SENTIENT new.".into(),
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.screen = Screen::ConfirmRemove;
                this.understood = false;
                cx.notify();
            }))
            .into_any_element(),
            Self::choice(
                theme,
                "earlier-repair",
                Icon::new(Lucide::Wrench),
                "Repair it",
                if repair_ok {
                    "Keeps the databases. Puts the program files and services back and starts SENTIENT.".into()
                } else {
                    "Not possible: no SENTIENT service is left to repair.".into()
                },
                repair_ok,
            )
            .when(repair_ok, |c| c.on_click(cx.listener(|this, _, _, cx| this.start(Run::Repair, cx))))
            .into_any_element(),
            Self::choice(
                theme,
                "earlier-keep",
                Icon::new(Lucide::Database),
                "Install fresh, keep the old database",
                if keep_ok {
                    "Installs SENTIENT new and connects it to the existing database.".into()
                } else {
                    "Not possible: there is no database to keep.".into()
                },
                keep_ok,
            )
            .when(keep_ok, |c| c.on_click(cx.listener(|this, _, _, cx| this.start(Run::Keep, cx))))
            .into_any_element(),
        ]
    }

    fn confirm(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let can_backup = self.found.no_backup.is_none();
        vec![
            div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child("Remove the earlier install?").into_any_element(),
            div().text_color(theme.muted_foreground).child("This will be deleted from this computer and cannot be undone:").into_any_element(),
            self.found_list(theme).into_any_element(),
            v_flex()
                .gap_1()
                .child(
                    Checkbox::new("backup-first")
                        .label("Save a backup first")
                        .checked(self.backup_first && can_backup)
                        .disabled(!can_backup)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.backup_first = *v;
                            cx.notify();
                        })),
                )
                .child(div().pl_6().text_sm().text_color(theme.muted_foreground).child(match &self.found.no_backup {
                    None => format!("To {}. Possible because PostgreSQL still runs.", backup_dir().display()),
                    Some(why) => why.clone(),
                }))
                .into_any_element(),
            Checkbox::new("understood")
                .label("I understand that every database listed above is deleted")
                .checked(self.understood)
                .on_change(cx.listener(|this, v: &bool, _, cx| {
                    this.understood = *v;
                    cx.notify();
                }))
                .into_any_element(),
        ]
    }

    /// The confirm screen's commit buttons, in a bar under the page as on
    /// every Setup page (Installer::footer): Back, then the removal at the
    /// right end, its purpose (docs/DESIGN_NOTES.md, "Where buttons go").
    fn confirm_footer(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        h_flex()
            .flex_shrink_0()
            .px(px(ui::PAGE_PAD))
            .py(px(14.))
            .gap(crate::ui::button_gap())
            .items_center()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(div().flex_1())
            .child(crate::ui::probed(crate::ui::button("back").outline().h(px(crate::theme::BAR_CTRL_H)).px_4().label("Back").on_click(cx.listener(|this, _, _, cx| {
                this.screen = Screen::Choices;
                cx.notify();
            }))))
            .child(crate::ui::probed(
                crate::ui::button("remove-fresh")
                    .danger()
                    .h(px(crate::theme::BAR_CTRL_H))
                    .px_5()
                    .label("Remove and install fresh")
                    .disabled(!self.understood)
                    .on_click(cx.listener(|this, _, _, cx| this.start(Run::Remove, cx))),
            ))
    }

    fn running(&self, theme: &gpui_kit::component::Theme, run: Run, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let job = self.job.read(cx);
        let finished = match job.state() {
            JobState::Done(_) => Some(true),
            JobState::Failed(_) => Some(false),
            _ => None,
        };
        let title = match (run, finished) {
            (Run::Remove, Some(false)) => "The earlier install was not removed",
            (Run::Remove, _) => "Removing the earlier install",
            (Run::Repair, None) => "Repairing the earlier install",
            (Run::Repair, Some(true)) => "Repaired: SENTIENT is running",
            (Run::Repair, Some(false)) => "The earlier install could not be repaired",
            (Run::Keep, Some(false)) => "The earlier program was not removed",
            (Run::Keep, _) => "Keeping the old database",
        };
        let result = match job.state() {
            JobState::Failed(m) => {
                let tail = if run == Run::Repair { " Nothing was deleted." } else { "" };
                Some(ui::outcome(theme, Tone::Bad, format!("{m}{tail}")))
            }
            _ => None,
        };
        // Secondary first, the primary at the right end.
        let close = || crate::ui::button("close").outline().label("Close").on_click(|_, window, _| window.remove_window()).into_any_element();
        let acts: Vec<AnyElement> = match finished {
            Some(false) if run == Run::Repair => vec![
                close(),
                crate::ui::button("fresh-after-repair").primary().label("Remove it and install fresh").on_click(cx.listener(|this, _, _, cx| {
                    this.screen = Screen::ConfirmRemove;
                    this.understood = false;
                    cx.notify();
                })).into_any_element(),
            ],
            Some(false) => vec![close(), crate::ui::button("retry").primary().label("Try again").on_click(cx.listener(move |this, _, _, cx| this.start(run, cx))).into_any_element()],
            Some(true) if run == Run::Repair => vec![
                close(),
                crate::ui::button("open").primary().label("Open the Platform Manager").on_click(|_, window, _| {
                    let exe = crate::self_install::manager_exe();
                    let exe = if exe.exists() { exe } else { std::env::current_exe().unwrap_or(exe) };
                    if crate::self_install::manager_command(&exe).arg("--manager").spawn().is_ok() {
                        window.remove_window();
                    }
                }).into_any_element(),
            ],
            _ => Vec::new(),
        };
        let mut view = crate::manager::jobview::JobView::new("earlier-run", title, &self.job).actions(acts);
        if let Some(r) = result {
            view = view.result(r);
        }
        vec![view.render(cx).into_any_element()]
    }
}

impl Render for EarlierPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let children = match self.screen {
            Screen::Choices => self.choices(&theme, cx),
            Screen::ConfirmRemove => self.confirm(&theme, cx),
            Screen::Running(run) => self.running(&theme, run, cx),
        };
        let footer = matches!(self.screen, Screen::ConfirmRemove).then(|| self.confirm_footer(&theme, cx));
        v_flex()
            .size_full()
            .bg(theme.muted)
            .text_color(theme.foreground)
            .child(v_flex().id("earlier").flex_1().min_h_0().overflow_y_scroll().p_8().gap_4().children(children))
            .children(footer)
    }
}
