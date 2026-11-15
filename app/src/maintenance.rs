// Copyright © 2016-2026 The SENTIENT Authors

//! Maintenance mode: setup run again on a machine where SENTIENT is already
//! installed (or "Uninstall" in Apps & features).
//!
//! The screen shows a tick box for each thing this Setup can update: the
//! Platform Manager when this Setup is newer, SENTIENT when it carries a
//! newer package. A line that cannot be updated is greyed with the reason;
//! with nothing to update the choices are Open, Repair, Change database and
//! Uninstall. An older Setup never replaces a newer Platform Manager, and a
//! Setup run where the Platform Manager was removed installs only the
//! Platform Manager (SENTIENT is detected, not touched).
//!
//! Updating the Platform Manager never touches SENTIENT's version or data:
//! it replaces the program and applies only the listed changes for this
//! installation (crate::changes), never Repair. Repair re-registers the
//! services and restores the program files; Uninstall removes SENTIENT and,
//! if asked, its data. Change database opens the Platform Manager on its
//! Migration screen.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, WindowExt as _, h_flex, v_flex,
    Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    progress::Progress as ProgressBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

use sentient_installer_core::{distro, native, releases};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};
use crate::jobs::{Job, JobState};
use crate::self_install;
use crate::ui::{self, Tone};
#[allow(unused_imports)]
use crate::maintenance_ops::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Repair,
    ChangeDatabase,
    Uninstall,
}

impl Action {
    const ALL: [Action; 3] = [Action::Repair, Action::ChangeDatabase, Action::Uninstall];

    fn id(self) -> &'static str {
        match self {
            Action::Repair => "repair",
            Action::ChangeDatabase => "change-database",
            Action::Uninstall => "uninstall",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Action::Repair => "Repair",
            Action::ChangeDatabase => "Change database",
            Action::Uninstall => "Uninstall",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Action::Repair => "Re-register the services and restore missing program files. Data and settings are kept.",
            Action::ChangeDatabase => "Move to another database type, history included. Opens the Platform Manager's Migration screen.",
            Action::Uninstall => "Stop and remove SENTIENT and the Platform Manager. You choose whether to keep the data.",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Action::Repair => Icon::new(Lucide::Wrench),
            Action::ChangeDatabase => Icon::new(Lucide::ArrowLeftRight),
            Action::Uninstall => Icon::new(Lucide::Trash),
        }
    }
}

/// What this Setup finds and can do. Read once when it opens.
#[derive(Clone)]
struct Found {
    /// The Platform Manager installed here, if its program is there.
    manager_installed: bool,
    installed_manager: String,
    this_manager: String,
    /// This Setup against the installed Platform Manager.
    manager_move: releases::Move,
    /// The SENTIENT package next to this Setup and its version.
    package: Option<(PathBuf, String)>,
}

impl Found {
    fn read(record: &InstallationRecord) -> Found {
        let installed_manager = record.manager_version.clone();
        let this_manager = crate::updates::manager_version().to_string();
        // A record without a version is from a Platform Manager before the
        // field was kept: older than this one.
        let manager_move = if installed_manager.is_empty() { releases::Move::Newer } else { releases::movement(&this_manager, &installed_manager) };
        let mut manager_installed = self_install::manager_exe().exists();
        #[cfg(debug_assertions)]
        if let Ok(v) = std::env::var("SPM_DEBUG_MANAGER_INSTALLED") {
            manager_installed = v == "1";
        }
        let mut package = (record.engine == Engine::Native)
            .then(native::bundled_payload_dir)
            .flatten()
            .and_then(|dir| crate::updating::package_version(&dir).map(|v| (dir, v)));
        #[cfg(debug_assertions)]
        if let Ok(v) = std::env::var("SPM_DEBUG_PACKAGE_VERSION") {
            let dir = std::env::var_os("SPM_DEBUG_PACKAGE_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("spm-debug-package"));
            package = (!v.is_empty()).then(|| (dir, v));
        }
        Found { manager_installed, installed_manager, this_manager, manager_move, package }
    }

    fn installed_manager_label(&self) -> String {
        if self.installed_manager.is_empty() { "an earlier version".into() } else { self.installed_manager.clone() }
    }

    fn sentient_move(&self, record: &InstallationRecord) -> Option<releases::Move> {
        self.package.as_ref().map(|(_, v)| releases::movement(v, &record.sentient_version))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// What fits what was found: tick boxes, the choices, or a refusal.
    Home,
    /// The release notes this Setup carries.
    WhatsNew,
    /// Updating or installing the Platform Manager and/or SENTIENT.
    Update,
    /// Repair or Uninstall.
    Action(Action),
}

pub struct Maintenance {
    record: Option<InstallationRecord>,
    found: Option<Found>,
    screen: Screen,
    tick_manager: bool,
    tick_sentient: bool,
    /// The steps the update run shows.
    planned: Vec<String>,
    show_log: bool,
    keep_data: bool,
    job: Entity<Job>,
    /// The installation is not working: the screen for a broken earlier
    /// install (crate::earlier) comes first.
    broken: Option<Entity<crate::earlier::EarlierPage>>,
    _observe: Subscription,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Maintenance {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |_, _, cx| cx.notify());
        let record = InstallationRecord::load();
        let found = record.as_ref().map(Found::read);
        let (tick_manager, tick_sentient) = match (&record, &found) {
            (Some(r), Some(f)) => (f.manager_move == releases::Move::Newer, f.sentient_move(r) == Some(releases::Move::Newer)),
            _ => (false, false),
        };
        // Not working (nothing runs, nothing answers): offer the ways out of a
        // broken install before anything else.
        let mut tasks = Vec::new();
        if let Some(r) = record.clone() {
            tasks.push(cx.spawn(async move |this, cx| {
                let found = cx
                    .background_executor()
                    .spawn(async move {
                        let snap = r.platform().snapshot();
                        let mut broken = !snap.serving && !snap.services.iter().any(|s| s.running || s.starting);
                        #[cfg(debug_assertions)]
                        if let Ok(v) = std::env::var("SPM_DEBUG_BROKEN") {
                            broken = v == "1";
                        }
                        broken.then(|| crate::earlier::Found::look(Some(r.clone()), r.pg_dir.clone()))
                    })
                    .await;
                let Some(found) = found else { return };
                this.update(cx, |page, cx| {
                    let view = cx.new(|cx| crate::earlier::EarlierPage::new(found, cx));
                    // Remove-and-install-fresh or keep-the-database: the
                    // install wizard takes over.
                    page._subs.push(cx.subscribe(&view, |_, _, _: &crate::earlier::Continue, cx| {
                        if let Ok(me) = std::env::current_exe() {
                            if std::process::Command::new(me).arg("--install").spawn().is_ok() {
                                cx.quit();
                            }
                        }
                    }));
                    page.broken = Some(view);
                    cx.notify();
                })
                .ok();
            }));
        }
        Self {
            broken: None,
            _tasks: tasks,
            _subs: Vec::new(),
            record,
            found,
            screen: Screen::Home,
            tick_manager,
            tick_sentient,
            planned: Vec::new(),
            show_log: false,
            keep_data: true,
            job,
            _observe,
        }
    }

    fn start(&mut self, action: Action, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.screen = Screen::Action(action);
        let keep = self.keep_data;
        self.job.update(cx, |job, cx| match action {
            Action::Repair => job.start(move |sink| run_repair(record, sink), cx),
            Action::Uninstall => job.start(move |sink| run_uninstall(record, keep, sink), cx),
            Action::ChangeDatabase => {}
        });
        cx.notify();
    }

    fn confirm(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if action == Action::ChangeDatabase {
            if open_manager(&["--page", "migration"]) {
                window.remove_window();
            }
            return;
        }
        let this = cx.entity().downgrade();
        let keep = self.keep_data;
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let (title, text, ok) = match action {
                Action::Repair => (
                    "Repair SENTIENT?",
                    "The services are re-registered and SENTIENT is restarted. Data and settings are kept.",
                    "Repair",
                ),
                _ if keep => (
                    "Uninstall SENTIENT?",
                    "SENTIENT and the Platform Manager are removed. The database is kept.",
                    "Uninstall",
                ),
                _ => (
                    "Uninstall SENTIENT and delete its data?",
                    "SENTIENT, the Platform Manager, PostgreSQL and every database are removed. This can't be undone.",
                    "Delete everything",
                ),
            };
            dialog.title(title).description(text).confirm().ok_text(ok).on_ok(move |_, _, cx| {
                this.update(cx, |page, cx| page.start(action, cx)).ok();
                true
            })
        });
    }

    /// Update (or, where it was removed, install) what is ticked: the
    /// Platform Manager first, so SENTIENT is updated by the newest updater.
    fn run_update(&mut self, cx: &mut Context<Self>) {
        let (Some(record), Some(found)) = (self.record.clone(), self.found.clone()) else { return };
        let manager = self.tick_manager;
        let sentient = self.tick_sentient.then(|| found.package.clone()).flatten();
        let install_only = !found.manager_installed;
        let mut planned: Vec<String> = Vec::new();
        if install_only {
            planned.extend([INSTALL_MANAGER.to_string(), SHORTCUTS.to_string()]);
        } else if manager {
            planned.extend([CLOSE_MANAGER.to_string(), REPLACE_MANAGER.to_string()]);
            planned.extend(crate::changes::planned_steps(&crate::changes::pending(&record)));
        }
        if let Some((_, v)) = &sentient {
            planned.push(updating_sentient(v));
        }
        self.planned = planned;
        self.screen = Screen::Update;
        self.show_log = false;
        self.job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    let mut record = record;
                    if install_only {
                        step(&sink, INSTALL_MANAGER);
                        let path = self_install::install_manager()?;
                        step(&sink, SHORTCUTS);
                        let shortcuts = self_install::Shortcuts {
                            autostart: false,
                            desktop_sentient: false,
                            desktop_manager: true,
                            start_menu: true,
                            kiosk: false,
                            open_at_signin: false,
                            auto_signin: String::new(),
                            start_delay: 0,
                        };
                        if let Err(e) = self_install::create_manager_shortcuts(&path, &shortcuts) {
                            sink(Progress::Log { line: format!("note: {e}") });
                        }
                        self_install::refresh_operator_screen(&sink);
                        record.manager_version = crate::updates::manager_version().into();
                        save_record(&record)?;
                        // The new Platform Manager says once what it found.
                        crate::updates::mark_found_installation();
                    } else if manager {
                        step(&sink, CLOSE_MANAGER);
                        step(&sink, REPLACE_MANAGER);
                        let path = self_install::install_manager()?;
                        #[cfg(windows)]
                        self_install::restore_menu_entry(&path);
                        let _ = path;
                        self_install::refresh_operator_screen(&sink);
                        record.manager_version = crate::updates::manager_version().into();
                        save_record(&record)?;
                        record = crate::changes::apply(record, &sink)?;
                    }
                    if let Some((dir, v)) = sentient {
                        step(&sink, &updating_sentient(&v));
                        let mut updated = crate::updating::run(record, Some(&dir), false, nested(&sink))?;
                        updated.manager_version = crate::updates::manager_version().into();
                        save_record(&updated)?;
                    }
                    Ok(())
                },
                cx,
            )
        });
        cx.notify();
    }

    fn update_label(&self) -> String {
        match (self.tick_manager, self.tick_sentient) {
            (true, true) => "Update both".into(),
            (true, false) if self.sentient_tickable() => "Update the Platform Manager".into(),
            (false, true) if self.manager_tickable() => "Update SENTIENT".into(),
            (false, true) => "Update SENTIENT".into(),
            _ => "Update".into(),
        }
    }

    fn manager_tickable(&self) -> bool {
        self.found.as_ref().is_some_and(|f| f.manager_move == releases::Move::Newer)
    }

    fn sentient_tickable(&self) -> bool {
        match (&self.record, &self.found) {
            (Some(r), Some(f)) => f.sentient_move(r) == Some(releases::Move::Newer),
            _ => false,
        }
    }
}

/// Start the installed Platform Manager with `args`.
fn open_manager(args: &[&str]) -> bool {
    let exe = self_install::manager_exe();
    let exe = if exe.exists() { exe } else { std::env::current_exe().unwrap_or(exe) };
    std::process::Command::new(&exe).arg("--manager").args(args).spawn().is_ok()
}

/// A tick box line: box, title and what it does, and its "What's new".
fn line(
    theme: &gpui_kit::component::Theme,
    id: &'static str,
    ticked: bool,
    enabled: bool,
    title: impl Into<SharedString>,
    about: impl Into<SharedString>,
    pill: Option<&'static str>,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .gap_3()
        .p_3()
        .items_start()
        .rounded(theme.radius)
        .bg(theme.background)
        .border_1()
        .border_color(if ticked && enabled { theme.primary } else { theme.border })
        .child(Checkbox::new(SharedString::from(format!("{id}-box"))).checked(ticked).disabled(!enabled))
        .child(div().when(!enabled, |d| d.opacity(0.6)).child(ui::product_logo(if id == "line-sentient" { ui::Product::Sentient } else { ui::Product::Manager }, 28.)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .when(!enabled, |c| c.opacity(0.65))
                .child(div().font_weight(FontWeight::MEDIUM).child(title.into()))
                .child(div().text_sm().text_color(theme.muted_foreground).child(about.into())),
        )
        .children(pill.map(|p| ui::pill(theme, Tone::Neutral, p)))
}

impl Maintenance {
    /// The "What's new" button of a line: the notes this Setup carries.
    fn notes_button(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        Button::new(id).outline().small().label("What's new").on_click(cx.listener(|this, _, _, cx| {
            this.screen = Screen::WhatsNew;
            cx.notify();
        }))
    }
}

/// A card for one of the choices.
fn card(theme: &gpui_kit::component::Theme, id: &'static str, icon: Icon, title: &'static str, about: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    h_flex()
        .id(id)
        .gap_4()
        .p_4()
        .rounded(theme.radius)
        .bg(if primary { theme.accent } else { theme.background })
        .border_1()
        .border_color(if primary { theme.primary } else { theme.border })
        .cursor_pointer()
        .hover(|c| c.border_color(theme.primary))
        .child(icon.text_color(theme.primary))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_sm().text_color(theme.muted_foreground).child(about.into())),
        )
}

impl Maintenance {
    fn other_choices(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        v_flex()
            .gap_2()
            .child(div().text_sm().text_color(theme.muted_foreground).child("Other choices"))
            .child(h_flex().gap_2().children(Action::ALL.into_iter().map(|action| {
                Button::new(SharedString::from(format!("other-{}", action.id())))
                    .outline()
                    .small()
                    .label(action.title())
                    .on_click(cx.listener(move |this, _, window, cx| this.confirm(action, window, cx)))
            })))
            .child(
                Checkbox::new("keep-data")
                    .label("When uninstalling, keep the database")
                    .checked(self.keep_data)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.keep_data = *value;
                        cx.notify();
                    })),
            )
    }

    /// C7b: the Platform Manager was removed, SENTIENT is still here.
    fn install_manager_screen(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            div().text_2xl().child("Install the SENTIENT Platform Manager").into_any_element(),
            div().text_color(theme.muted_foreground).child("This setup found SENTIENT already on this computer.").into_any_element(),
            line(
                theme,
                "line-sentient",
                false,
                false,
                "SENTIENT",
                format!(
                    "SENTIENT {} installation detected ({}). It is not installed again and not touched.",
                    record.sentient_version,
                    crate::installation::flavor_label(record.flavor())
                ),
                Some("Detected"),
            )
            .into_any_element(),
            line(theme, "line-manager", true, true, "SENTIENT Platform Manager", format!("Install {}.", found.this_manager), None)
                .child(self.notes_button("manager-notes", cx))
                .into_any_element(),
            h_flex()
                .gap_2()
                .justify_end()
                .child(Button::new("install-manager").primary().label("Install the Platform Manager").on_click(cx.listener(|this, _, _, cx| {
                    this.tick_manager = true;
                    this.tick_sentient = false;
                    this.run_update(cx);
                })))
                .child(Button::new("close").outline().label("Close").on_click(|_, window, _| window.remove_window()))
                .into_any_element(),
        ]
    }

    /// C2, C5, C6, C7a: a tick box for each thing this Setup can update.
    fn tick_screen(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let manager_about = match found.manager_move {
            releases::Move::Newer => {
                if found.package.is_none() || !self.sentient_tickable() {
                    format!("{} to {}. SENTIENT is not touched; data and settings are kept.", found.installed_manager_label(), found.this_manager)
                } else {
                    format!("{} to {}.", found.installed_manager_label(), found.this_manager)
                }
            }
            _ => format!("{} is installed. This setup is the same version.", found.installed_manager_label()),
        };
        let mut out: Vec<AnyElement> = vec![
            div().text_2xl().child(format!("SENTIENT {} is installed on this computer", record.sentient_version)).into_any_element(),
            div().text_color(theme.muted_foreground).child("Tick what this setup should update.").into_any_element(),
        ];
        let manager_on = self.manager_tickable();
        out.push(
            line(theme, "line-manager", self.tick_manager && manager_on, manager_on, "Platform Manager", manager_about, None)
                .when(manager_on, |l| {
                    l.child(self.notes_button("manager-notes", cx)).on_click(cx.listener(|this, _, _, cx| {
                        this.tick_manager = !this.tick_manager;
                        cx.notify();
                    }))
                })
                .into_any_element(),
        );
        match (&found.package, found.sentient_move(record)) {
            (Some((_, v)), Some(releases::Move::Newer)) => out.push(
                line(
                    theme,
                    "line-sentient",
                    self.tick_sentient,
                    true,
                    "SENTIENT",
                    format!("{} to {v}. Stopped for about 2 minutes. The database is copied first and put back if the update fails.", record.sentient_version),
                    None,
                )
                .child(self.notes_button("sentient-notes", cx))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.tick_sentient = !this.tick_sentient;
                    cx.notify();
                }))
                .into_any_element(),
            ),
            (Some((_, v)), Some(m)) => out.push(
                line(
                    theme,
                    "line-sentient",
                    false,
                    false,
                    "SENTIENT",
                    if m == releases::Move::Same {
                        format!("{v} is already installed. Nothing to update.")
                    } else {
                        format!("This setup carries {v}, older than the installed {}. Nothing to update.", record.sentient_version)
                    },
                    None,
                )
                .into_any_element(),
            ),
            _ => out.push(
                div()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .bg(theme.accent)
                    .text_sm()
                    .child(if record.engine == Engine::Docker {
                        "To update SENTIENT, use Update inside the Platform Manager."
                    } else {
                        "This setup carries no SENTIENT package. To update SENTIENT, use Update inside the Platform Manager or the offline setup."
                    })
                    .into_any_element(),
            ),
        }
        let nothing = !(self.tick_manager && manager_on) && !self.tick_sentient;
        out.push(
            h_flex()
                .gap_2()
                .justify_end()
                .child(Button::new("update").primary().label(self.update_label()).disabled(nothing).on_click(cx.listener(|this, _, _, cx| {
                    if !this.manager_tickable() {
                        this.tick_manager = false;
                    }
                    this.run_update(cx);
                })))
                .child(Button::new("close").outline().label("Close").on_click(|_, window, _| window.remove_window()))
                .into_any_element(),
        );
        out.push(self.other_choices(theme, cx).into_any_element());
        out
    }

    /// C3 (same version, nothing to update) and C4 (this Setup is older).
    fn choices_screen(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let older = found.manager_move == releases::Move::Older;
        let mut out: Vec<AnyElement> = Vec::new();
        if older {
            out.push(div().text_2xl().child("A newer Platform Manager is already installed").into_any_element());
            out.push(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("Installed: {}. This setup: {}.", found.installed_manager, found.this_manager))
                    .into_any_element(),
            );
            out.push(ui::outcome(theme, Tone::Warn, "This older setup will not replace the newer program.").into_any_element());
        } else {
            out.push(div().text_2xl().child(format!("SENTIENT {} is installed on this computer", record.sentient_version)).into_any_element());
            out.push(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("Platform Manager {} is installed. This setup is the same version.", found.installed_manager_label()))
                    .into_any_element(),
            );
        }
        out.push(
            card(
                theme,
                "open-manager",
                Icon::new(Lucide::Play),
                "Open the Platform Manager",
                if older { "Use the installed version for repair, database change or uninstall." } else { "Status, control, updates, backup and logs." },
                true,
            )
            .on_click(cx.listener(|_, _, window, _| {
                if open_manager(&[]) {
                    window.remove_window();
                }
            }))
            .into_any_element(),
        );
        if !older {
            for action in Action::ALL {
                out.push(
                    card(theme, action.id(), action.icon(), action.title(), action.describe(), false)
                        .on_click(cx.listener(move |this, _, window, cx| this.confirm(action, window, cx)))
                        .into_any_element(),
                );
            }
            out.push(
                Checkbox::new("keep-data")
                    .label("When uninstalling, keep the database")
                    .checked(self.keep_data)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.keep_data = *value;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        out
    }

    /// E5: the notes this Setup carries, for what it can update.
    fn whats_new_screen(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut notes = Vec::new();
        if found.manager_move == releases::Move::Newer {
            notes.extend(crate::notes::own(&found.installed_manager).into_iter().map(|mut n| {
                n.version = format!("Platform Manager {}", n.version);
                n
            }));
        }
        let mut sentient_db = None;
        if let (Some((dir, v)), Some(releases::Move::Newer)) = (&found.package, found.sentient_move(record)) {
            let s = crate::notes::bundled(dir, &record.sentient_version, v);
            sentient_db = Some(if s.is_empty() { "This package carries no release notes for SENTIENT.".to_string() } else { crate::notes::database_line(&s) });
            notes.extend(s.into_iter().map(|mut n| {
                n.version = format!("SENTIENT {}", n.version);
                n
            }));
        }
        let carries = match &found.package {
            Some((_, v)) => format!(" and carries SENTIENT {v}"),
            None => String::new(),
        };
        vec![
            div().text_2xl().child("What's new in this setup").into_any_element(),
            div()
                .text_color(theme.muted_foreground)
                .child(format!("Platform Manager {} is installed. This setup is {}{carries}.", found.installed_manager_label(), found.this_manager))
                .into_any_element(),
            ui::panel(theme).child(ui::release_notes(theme, &notes)).children(sentient_db.map(|d| div().text_sm().child(d))).into_any_element(),
            h_flex()
                .child(Button::new("back").outline().label("Back").on_click(cx.listener(|this, _, _, cx| {
                    this.screen = Screen::Home;
                    cx.notify();
                })))
                .into_any_element(),
        ]
    }

    /// C8–C10: the update running, finished or failed.
    fn update_screen(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let job = self.job.read(cx);
        let finished = match job.state() {
            JobState::Done(_) => Some(true),
            JobState::Failed(_) => Some(false),
            _ => None,
        };
        let install_only = !found.manager_installed;
        let manager = self.tick_manager || install_only;
        let sentient_v = self.tick_sentient.then(|| found.package.as_ref().map(|(_, v)| v.clone())).flatten();
        let what = match (manager, &sentient_v) {
            (true, Some(_)) => "the Platform Manager and SENTIENT",
            (true, None) => "the Platform Manager",
            _ => "SENTIENT",
        };
        let title = match (finished, install_only) {
            (None, true) => "Installing the Platform Manager".to_string(),
            (None, false) => format!("Updating {what}"),
            (Some(true), true) => "The Platform Manager is installed".to_string(),
            (Some(true), false) => match (manager, &sentient_v) {
                (true, Some(_)) => "The Platform Manager and SENTIENT are updated".to_string(),
                (true, None) => "The Platform Manager is updated".to_string(),
                _ => "SENTIENT is updated".to_string(),
            },
            (Some(false), true) => "The Platform Manager was not installed".to_string(),
            (Some(false), false) => format!("{} was not updated", capitalise(what)),
        };
        let versions = match (manager, &sentient_v) {
            (true, Some(v)) => format!("Platform Manager {} to {}, SENTIENT {} to {v}", found.installed_manager_label(), found.this_manager, record.sentient_version),
            (true, None) if install_only => format!("Platform Manager {}", found.this_manager),
            (true, None) => format!("{} to {}", found.installed_manager_label(), found.this_manager),
            (false, Some(v)) => format!("SENTIENT {} to {v}", record.sentient_version),
            _ => String::new(),
        };
        let marked = ui::mark_steps(&self.planned, job.steps(), finished);
        let log: Vec<SharedString> = job.log().iter().rev().take(14).rev().cloned().collect();
        let mut out: Vec<AnyElement> = vec![
            div().text_2xl().child(title).into_any_element(),
            div().text_color(theme.muted_foreground).child(versions).into_any_element(),
        ];
        match job.state() {
            JobState::Done(_) => {
                let now = InstallationRecord::load();
                let sentient_now = now.as_ref().map(|r| r.sentient_version.clone()).unwrap_or_else(|| record.sentient_version.clone());
                out.push(
                    ui::outcome(theme, Tone::Good, format!("Platform Manager {} is installed. SENTIENT {sentient_now} is running.", if manager { found.this_manager.clone() } else { found.installed_manager_label() }))
                        .into_any_element(),
                );
            }
            JobState::Failed(m) => {
                let keeps = if manager && !install_only {
                    format!(" Platform Manager {} is still installed and SENTIENT is running.", found.installed_manager_label())
                } else {
                    String::new()
                };
                out.push(ui::outcome(theme, Tone::Bad, format!("{m}{keeps}")).into_any_element());
            }
            _ => {}
        }
        out.push(ui::steps(theme, &marked).into_any_element());
        if finished.is_none() {
            if let Some(p) = job.percent() {
                out.push(ProgressBar::new("maint").value(p).into_any_element());
            }
        }
        if (self.show_log || finished.is_none()) && !log.is_empty() {
            out.push(
                ui::panel(theme)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .children(log.into_iter().map(|l| div().child(l)))
                    .into_any_element(),
            );
        }
        match finished {
            Some(true) => out.push(
                h_flex()
                    .gap_2()
                    .child(Button::new("open").primary().label("Open the Platform Manager").on_click(|_, window, _| {
                        if open_manager(&[]) {
                            window.remove_window();
                        }
                    }))
                    .child(Button::new("close").outline().label("Close").on_click(|_, window, _| window.remove_window()))
                    .into_any_element(),
            ),
            Some(false) => out.push(
                h_flex()
                    .gap_2()
                    .child(Button::new("retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| this.run_update(cx))))
                    .child(Button::new("log").outline().label(if self.show_log { "Hide log" } else { "Show log" }).on_click(cx.listener(|this, _, _, cx| {
                        this.show_log = !this.show_log;
                        cx.notify();
                    })))
                    .child(Button::new("close").outline().label("Close").on_click(|_, window, _| window.remove_window()))
                    .into_any_element(),
            ),
            None => {}
        }
        out
    }

    /// Repair and Uninstall, as before.
    fn action_screen(&self, theme: &gpui_kit::component::Theme, action: Action, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let job = self.job.read(cx);
        let busy = job.is_running();
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let log: Vec<SharedString> = job.log().iter().rev().take(12).rev().cloned().collect();
        let mut out = vec![
            div().text_2xl().child(action.title()).into_any_element(),
            div().text_sm().child(job.step_with_time()).into_any_element(),
        ];
        if busy {
            if let Some(v) = job.percent() {
                out.push(ProgressBar::new("maint").value(v).into_any_element());
            }
        }
        out.extend(outcome.map(|(tone, text)| ui::outcome(theme, tone, text).into_any_element()));
        out.push(
            ui::panel(theme)
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .children(log.into_iter().map(|l| div().child(l)))
                .into_any_element(),
        );
        if !busy {
            out.push(h_flex().child(Button::new("close").primary().label("Close").on_click(|_, window, _| window.remove_window())).into_any_element());
        }
        let _ = cx;
        out
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

impl Render for Maintenance {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = v_flex().id("maintenance").size_full().overflow_y_scroll().p_8().gap_4().bg(theme.muted).text_color(theme.foreground);

        let (Some(record), Some(found)) = (self.record.clone(), self.found.clone()) else {
            return page.child(div().text_2xl().child("SENTIENT Setup")).child(ui::not_installed(&theme));
        };
        if let (Some(broken), Screen::Home) = (&self.broken, self.screen) {
            return page.p_0().child(div().size_full().child(broken.clone()));
        }
        let children = match self.screen {
            Screen::Action(action) => self.action_screen(&theme, action, cx),
            Screen::Update => self.update_screen(&theme, &record, &found, cx),
            Screen::WhatsNew => self.whats_new_screen(&theme, &record, &found, cx),
            Screen::Home if !found.manager_installed => self.install_manager_screen(&theme, &record, &found, cx),
            Screen::Home if found.manager_move == releases::Move::Older => self.choices_screen(&theme, &record, &found, cx),
            Screen::Home if !self.manager_tickable() && !self.sentient_tickable() => self.choices_screen(&theme, &record, &found, cx),
            Screen::Home => self.tick_screen(&theme, &record, &found, cx),
        };
        page.children(children)
    }
}
