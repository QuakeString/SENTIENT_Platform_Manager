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
    ActiveTheme, Disableable as _, Icon, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

use sentient_installer_core::{native, releases};
use sentient_installer_core::progress::Progress;

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
            Action::Uninstall => "Stop and remove SENTIENT. The Platform Manager stays. You choose whether to keep the data.",
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
    /// The gateway here and its update from the update channel (read in the
    /// background; None: no gateway, or nothing newer).
    gateway: Option<(crate::manager::gateway::GatewayUpdate, sentient_installer_core::gateway::Offer, Option<String>)>,
    tick_gateway: bool,
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
        // The gateway's update: the newest gateway release on the update
        // channel against the gateway found here.
        tasks.push(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let s = crate::manager::gateway::local_status();
                    if !s.installed && s.legacy.is_none() {
                        return None;
                    }
                    #[cfg(debug_assertions)]
                    if let Ok(v) = std::env::var("SPM_DEBUG_GATEWAY_OFFER") {
                        let o = sentient_installer_core::gateway::Offer { version: v, date: "2026-11-17".into(), notes: String::new(), page: String::new(), installer: None, checksum: None };
                        return crate::manager::gateway::gateway_update(&s, Some(&o), cfg!(windows)).map(|u| (u, o, s.version.clone()));
                    }
                    let o = sentient_installer_core::gateway::newest(crate::updates::Prefs::load().channel).ok().flatten()?;
                    crate::manager::gateway::gateway_update(&s, Some(&o), cfg!(windows)).map(|u| (u, o, s.version.clone()))
                })
                .await;
            this.update(cx, |page, cx| {
                page.tick_gateway = found.as_ref().is_some_and(|(u, _, _)| u.why.is_none());
                page.gateway = found;
                cx.notify();
            })
            .ok();
        }));
        Self {
            broken: None,
            gateway: None,
            tick_gateway: false,
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
        // Its steps from the first frame (crate::jobplan).
        let plan = match action {
            Action::Repair => crate::jobplan::repair(crate::jobplan::Shape::of(&record), record.flavor() == sentient_installer_core::stack::Flavor::Clickhouse),
            _ => crate::jobplan::uninstall(keep),
        };
        self.job.update(cx, |job, _| job.set_plan(plan));
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
        // Uninstall: the same dialog as the Platform Manager's (a switch per part).
        if action == Action::Uninstall {
            if let Some(record) = self.record.clone() {
                crate::manager::uninstall_dialog::open_local(
                    record,
                    std::rc::Rc::new(move |cx: &mut App| {
                        this.update(cx, |page, cx| {
                            page.record = InstallationRecord::load();
                            cx.notify();
                        })
                        .ok();
                    }),
                    cx,
                );
            }
            return;
        }
        let keep = self.keep_data;
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            let (title, text, ok) = match action {
                Action::Repair => (
                    "Repair SENTIENT?",
                    "The services are re-registered and SENTIENT is restarted. Data and settings are kept.",
                    "Repair",
                ),
                _ if keep => (
                    "Uninstall SENTIENT?",
                    "SENTIENT is removed. The database is kept. The Platform Manager stays.",
                    "Uninstall",
                ),
                _ => (
                    "Uninstall SENTIENT and delete its data?",
                    "SENTIENT, PostgreSQL and every database are removed. This can't be undone. The Platform Manager stays.",
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
        let gateway = self.gateway_ticked().then(|| self.gateway.as_ref().map(|(_, o, _)| o.clone())).flatten();
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
        if let Some(o) = &gateway {
            planned.push(updating_gateway(&o.version));
        }
        self.job.update(cx, |job, _| job.set_plan(planned.iter().map(|p| sentient_installer_core::progress::PlanStep::new(p.clone())).collect()));
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
                    // The gateway last: SENTIENT is running again by then.
                    if let Some(o) = gateway {
                        step(&sink, &updating_gateway(&o.version));
                        sentient_installer_core::gateway::update(&nested(&sink), &o)?;
                    }
                    Ok(())
                },
                cx,
            )
        });
        cx.notify();
    }

    fn update_label(&self) -> String {
        update_label(self.tick_manager && self.manager_tickable(), self.tick_sentient, self.gateway_ticked())
    }

    /// The gateway's line is ticked and can run.
    fn gateway_ticked(&self) -> bool {
        self.tick_gateway && self.gateway.as_ref().is_some_and(|(u, _, _)| u.why.is_none())
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

/// The Update button's words for what is ticked.
fn update_label(manager: bool, sentient: bool, gateway: bool) -> String {
    match (manager, sentient, gateway) {
        (true, true, true) => "Update all three".into(),
        (true, true, false) | (true, false, true) | (false, true, true) => "Update both".into(),
        (true, false, false) => "Update the Platform Manager".into(),
        (false, true, false) => "Update SENTIENT".into(),
        (false, false, true) => "Update the gateway".into(),
        _ => "Update".into(),
    }
}

/// What the update screen names: the ticked parts, in order.
fn updated_parts(manager: bool, sentient: bool, gateway: bool) -> String {
    let parts: Vec<&str> = [(manager, "the Platform Manager"), (sentient, "SENTIENT"), (gateway, "the Sentient Gateway")].into_iter().filter(|(on, _)| *on).map(|(_, n)| n).collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        _ => parts.join(", "),
    }
}

fn updating_gateway(version: &str) -> String {
    format!("Updating the Sentient Gateway to {version}")
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
        .child(div().when(!enabled, |d| d.opacity(0.6)).child(ui::product_logo(
            match id {
                "line-sentient" => ui::Product::Sentient,
                "line-gateway" => ui::Product::Gateway,
                _ => ui::Product::Manager,
            },
            28.,
        )))
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
        crate::ui::button(id).outline().label("What's new").on_click(cx.listener(|this, _, _, cx| {
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
            .child(h_flex().gap(crate::ui::button_gap()).children(Action::ALL.into_iter().map(|action| {
                crate::ui::button(SharedString::from(format!("other-{}", action.id())))
                    .outline()
                    .label(action.title())
                    .on_click(cx.listener(move |this, _, window, cx| this.confirm(action, window, cx)))
            })))
    }

    /// C7b: the Platform Manager was removed, SENTIENT is still here.
    fn install_manager_screen(&self, theme: &gpui_kit::component::Theme, record: &InstallationRecord, found: &Found, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child("Install the SENTIENT Platform Manager").into_any_element(),
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
                .gap(crate::ui::button_gap())
                .justify_end()
                .child(crate::ui::probed(crate::ui::button("install-manager").primary().label("Install the Platform Manager").on_click(cx.listener(|this, _, _, cx| {
                    this.tick_manager = true;
                    this.tick_sentient = false;
                    this.run_update(cx);
                }))))
                .child(crate::ui::probed(crate::ui::button("close").outline().label("Close").on_click(|_, window, _| window.remove_window())))
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
            div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child(format!("SENTIENT {} is installed on this computer", record.sentient_version)).into_any_element(),
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
        if let Some((u, _, installed)) = &self.gateway {
            let enabled = u.why.is_none();
            let about = match &u.why {
                Some(why) => why.clone(),
                None => u.about.clone(),
            };
            let title = match installed {
                Some(v) if !u.older => format!("Sentient Gateway {v} to {}", u.version),
                _ => format!("Sentient Gateway {}", u.version),
            };
            out.push(
                line(theme, "line-gateway", self.tick_gateway && enabled, enabled, title, about, None)
                    .when(u.older, |l| l.child(ui::info("line-gateway-help".into(), crate::manager::gateway::legacy_update_help().into())))
                    .when(enabled, |l| {
                        l.on_click(cx.listener(|this, _, _, cx| {
                            this.tick_gateway = !this.tick_gateway;
                            cx.notify();
                        }))
                    })
                    .into_any_element(),
            );
        }
        let nothing = !(self.tick_manager && manager_on) && !self.tick_sentient && !self.gateway_ticked();
        out.push(
            h_flex()
                .gap(crate::ui::button_gap())
                .justify_end()
                .child(crate::ui::probed(crate::ui::button("update").primary().label(self.update_label()).disabled(nothing).on_click(cx.listener(|this, _, _, cx| {
                    if !this.manager_tickable() {
                        this.tick_manager = false;
                    }
                    this.run_update(cx);
                }))))
                .child(crate::ui::probed(crate::ui::button("close").outline().label("Close").on_click(|_, window, _| window.remove_window())))
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
            out.push(div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child("A newer Platform Manager is already installed").into_any_element());
            out.push(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("Installed: {}. This setup: {}.", found.installed_manager, found.this_manager))
                    .into_any_element(),
            );
            out.push(ui::outcome(theme, Tone::Warn, "This older setup will not replace the newer program.").into_any_element());
        } else {
            out.push(div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child(format!("SENTIENT {} is installed on this computer", record.sentient_version)).into_any_element());
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
            div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child("What's new in this setup").into_any_element(),
            div()
                .text_color(theme.muted_foreground)
                .child(format!("Platform Manager {} is installed. This setup is {}{carries}.", found.installed_manager_label(), found.this_manager))
                .into_any_element(),
            ui::panel(theme).child(ui::release_notes(theme, &notes)).children(sentient_db.map(|d| div().text_sm().child(d))).into_any_element(),
            h_flex()
                .child(crate::ui::button("back").outline().label("Back").on_click(cx.listener(|this, _, _, cx| {
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
        let gateway = self.gateway_ticked().then(|| self.gateway.clone()).flatten();
        let what = updated_parts(manager, sentient_v.is_some(), gateway.is_some());
        let what = what.as_str();
        let title = match (finished, install_only) {
            (None, true) => "Installing the Platform Manager".to_string(),
            (None, false) => format!("Updating {what}"),
            (Some(true), true) => "The Platform Manager is installed".to_string(),
            (Some(true), false) => format!("{} {} updated", capitalise(what), if what.contains(" and ") { "are" } else { "is" }),
            (Some(false), true) => "The Platform Manager was not installed".to_string(),
            (Some(false), false) => format!("{} was not updated", capitalise(what)),
        };
        let mut versions = match (manager, &sentient_v) {
            (true, Some(v)) => format!("Platform Manager {} to {}, SENTIENT {} to {v}", found.installed_manager_label(), found.this_manager, record.sentient_version),
            (true, None) if install_only => format!("Platform Manager {}", found.this_manager),
            (true, None) => format!("{} to {}", found.installed_manager_label(), found.this_manager),
            (false, Some(v)) => format!("SENTIENT {} to {v}", record.sentient_version),
            _ => String::new(),
        };
        if let Some((u, _, installed)) = &gateway {
            let g = match installed {
                Some(v) if !u.older => format!("Sentient Gateway {v} to {}", u.version),
                _ => format!("the older Python gateway to Sentient Gateway {}", u.version),
            };
            versions = if versions.is_empty() { capitalise(&g) } else { format!("{versions}, {g}") };
        }
        let result = match job.state() {
            JobState::Done(_) => {
                let now = InstallationRecord::load();
                let sentient_now = now.as_ref().map(|r| r.sentient_version.clone()).unwrap_or_else(|| record.sentient_version.clone());
                Some(ui::outcome(theme, Tone::Good, format!("Platform Manager {} is installed. SENTIENT {sentient_now} is running.", if manager { found.this_manager.clone() } else { found.installed_manager_label() })))
            }
            JobState::Failed(m) => {
                let keeps = if manager && !install_only {
                    format!(" Platform Manager {} is still installed and SENTIENT is running.", found.installed_manager_label())
                } else {
                    String::new()
                };
                Some(ui::outcome(theme, Tone::Bad, format!("{m}{keeps}")))
            }
            _ => None,
        };
        // Secondary first, the primary at the right end.
        let acts: Vec<AnyElement> = match finished {
            Some(true) => vec![
                crate::ui::button("close").outline().label("Close").on_click(|_, window, _| window.remove_window()).into_any_element(),
                crate::ui::button("open").primary().label("Open the Platform Manager").on_click(|_, window, _| {
                    if open_manager(&[]) {
                        window.remove_window();
                    }
                }).into_any_element(),
            ],
            Some(false) => vec![
                crate::ui::button("close").outline().label("Close").on_click(|_, window, _| window.remove_window()).into_any_element(),
                crate::ui::button("retry").primary().label("Try again").on_click(cx.listener(|this, _, _, cx| this.run_update(cx))).into_any_element(),
            ],
            None => Vec::new(),
        };
        let mut view = crate::manager::jobview::JobView::new("maint-update", title, &self.job).actions(acts);
        if let Some(r) = result {
            view = view.result(r);
        }
        vec![div().text_color(theme.muted_foreground).child(versions).into_any_element(), view.render(cx).into_any_element()]
    }

    /// Repair and Uninstall, as before.
    fn action_screen(&self, theme: &gpui_kit::component::Theme, action: Action, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let _ = theme;
        let close = crate::ui::button("close").primary().label("Close").on_click(|_, window, _| window.remove_window()).into_any_element();
        vec![crate::manager::jobview::JobView::new("maint-action", action.title(), &self.job).actions([close]).render(cx).into_any_element()]
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Windows 10: rounded corners and the border (crate::win_frame).
        let root = self.render_root(window, cx).into_any_element();
        crate::win_frame::frame(root, window, cx)
    }
}

impl Maintenance {
    fn render_root(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The shared dialogs over the whole window (crate::modal).
        let layer = crate::modal::layer(window, cx, 0., 0.);
        let body = self.body(cx);
        let theme = cx.theme().clone();
        // Windows: the caption over the page's top edge (crate::win_caption),
        // under the dialogs.
        let caption = crate::win_caption::overlay(theme.foreground, theme.muted, window);
        div().size_full().relative().child(body).children(caption).children(layer)
    }
}

impl Maintenance {
    fn body(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let page = v_flex().id("maintenance").size_full().overflow_y_scroll().p_8().gap_4().bg(theme.muted).text_color(theme.foreground);

        let (Some(record), Some(found)) = (self.record.clone(), self.found.clone()) else {
            return page.child(div().text_size(px(19.)).font_weight(FontWeight::MEDIUM).child("SENTIENT Setup")).child(ui::not_installed(&theme));
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
            Screen::Home if !self.manager_tickable() && !self.sentient_tickable() && self.gateway.is_none() => self.choices_screen(&theme, &record, &found, cx),
            Screen::Home => self.tick_screen(&theme, &record, &found, cx),
        };
        page.children(children)
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{update_label, updated_parts, updating_gateway};

    #[test]
    fn the_update_button_and_title_name_what_is_ticked() {
        assert_eq!(update_label(true, false, false), "Update the Platform Manager");
        assert_eq!(update_label(false, false, true), "Update the gateway");
        assert_eq!(update_label(true, false, true), "Update both");
        assert_eq!(update_label(true, true, true), "Update all three");
        assert_eq!(update_label(false, false, false), "Update");
        assert_eq!(updated_parts(true, true, false), "the Platform Manager and SENTIENT");
        assert_eq!(updated_parts(false, false, true), "the Sentient Gateway");
        assert_eq!(updated_parts(true, true, true), "the Platform Manager, SENTIENT and the Sentient Gateway");
        assert_eq!(updating_gateway("4.16.3"), "Updating the Sentient Gateway to 4.16.3");
    }
}
