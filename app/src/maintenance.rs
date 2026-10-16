// Copyright © 2016-2026 The SENTIENT Authors

//! Maintenance mode: setup run again on a machine where SENTIENT is already
//! installed (or "Uninstall" in Apps & features). Update installs the
//! SENTIENT in this package; Repair re-registers the services and restores
//! the program files; Uninstall removes SENTIENT and, if asked, its data.
//! Change database opens the Platform Manager on its Migration screen.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    progress::Progress as ProgressBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::{distro, native};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};
use crate::jobs::{Job, JobState};
use crate::self_install;
use crate::ui::{self, Tone};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Update,
    Repair,
    ChangeDatabase,
    Uninstall,
}

impl Action {
    const ALL: [Action; 4] = [Action::Update, Action::Repair, Action::ChangeDatabase, Action::Uninstall];

    fn id(self) -> &'static str {
        match self {
            Action::Update => "update",
            Action::Repair => "repair",
            Action::ChangeDatabase => "change-database",
            Action::Uninstall => "uninstall",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Action::Update => "Update",
            Action::Repair => "Repair",
            Action::ChangeDatabase => "Change database",
            Action::Uninstall => "Uninstall",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Action::Update => "Install the SENTIENT version in this package. The database is copied first and everything is put back if the update fails.",
            Action::Repair => "Re-register the services and restore missing program files. Data and settings are kept.",
            Action::ChangeDatabase => "Move to another database type, history included. Opens the Platform Manager's Migration screen.",
            Action::Uninstall => "Stop and remove SENTIENT and the Platform Manager. You choose whether to keep the data.",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Action::Update => Icon::new(IconName::RefreshCw),
            Action::Repair => Icon::new(Lucide::Wrench),
            Action::ChangeDatabase => Icon::new(Lucide::ArrowLeftRight),
            Action::Uninstall => Icon::new(Lucide::Trash),
        }
    }
}

pub struct Maintenance {
    record: Option<InstallationRecord>,
    running: Option<Action>,
    keep_data: bool,
    job: Entity<Job>,
    _observe: Subscription,
}

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

fn run_update(record: InstallationRecord, sink: ProgressFn) -> Result<(), String> {
    let bundle = native::bundled_payload_dir();
    if record.engine == Engine::Native && bundle.is_none() {
        return Err("This copy of setup has no SENTIENT package next to it (a payload folder).".into());
    }
    let mut updated = crate::updating::run(record, bundle.as_deref(), false, sink.clone())?;
    step(&sink, "Updating the Platform Manager");
    self_install::install_manager()?;
    updated.manager_version = env!("CARGO_PKG_VERSION").into();
    save_record(&updated)?;
    Ok(())
}

fn run_repair(record: InstallationRecord, sink: ProgressFn) -> Result<(), String> {
    step(&sink, "Restoring the Platform Manager");
    let manager = self_install::install_manager()?;
    match record.engine {
        // Bring the stack up again from its compose file.
        Engine::Docker => distro::control(sink.clone(), &record.stack, "start")?,
        Engine::Native if cfg!(windows) => {
            // deploy is idempotent: it keeps the database, re-registers the
            // service if it is missing, and starts SENTIENT.
            native::deploy(sink.clone(), &record.native_config_with_secrets()?)?;
        }
        // Linux keeps the unit and its environment in /etc; restart them.
        Engine::Native => native::control(sink.clone(), &record.native_config(), "restart")?,
    }
    self_install::restore_menu_entry(&manager);
    sink(Progress::Done { message: "Repaired: SENTIENT is running.".into() });
    Ok(())
}

fn run_uninstall(record: InstallationRecord, keep_data: bool, sink: ProgressFn) -> Result<(), String> {
    match (record.engine, keep_data) {
        (Engine::Docker, true) => distro::uninstall(sink.clone(), &record.stack)?,
        (Engine::Docker, false) => distro::cleanup(sink.clone(), &record.stack)?,
        (Engine::Native, true) => native::uninstall(sink.clone(), &record.native_config())?,
        (Engine::Native, false) => native::cleanup(sink.clone(), &record.native_config())?,
    }
    // Windows hybrid: ClickHouse's distro belongs to its own account. Keeping
    // the data keeps both; otherwise the account deletes its distro, then
    // the account and its boot task go.
    #[cfg(windows)]
    if record.engine == Engine::Native
        && record.flavor() == sentient_installer_core::stack::Flavor::Clickhouse
        && !keep_data
    {
        use sentient_installer_core::clickhouse_wsl;
        if let Some(password) = crate::secrets::get(crate::secrets::Secret::ClickhouseAccount) {
            let _ = clickhouse_wsl::run_as_account(&sink, &password, &self_install::manager_exe(), "unregister");
        }
        clickhouse_wsl::remove_account(&sink);
    }
    if !keep_data {
        crate::secrets::forget_all();
    }
    step(&sink, "Removing shortcuts and the Platform Manager");
    self_install::remove_shortcuts();
    self_install::remove_record(&crate::installation::record_path());
    self_install::remove_manager_later();
    sink(Progress::Done {
        message: if keep_data {
            "SENTIENT is removed. The database was kept and can be reused by a new install.".into()
        } else {
            "SENTIENT and its data are removed.".into()
        },
    });
    Ok(())
}

fn save_record(record: &InstallationRecord) -> Result<(), String> {
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    self_install::write_record(&crate::installation::record_path(), &json)
}

impl Maintenance {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let job = cx.new(|_| Job::new());
        let _observe = cx.observe(&job, |_, _, cx| cx.notify());
        Self { record: InstallationRecord::load(), running: None, keep_data: true, job, _observe }
    }

    fn start(&mut self, action: Action, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        self.running = Some(action);
        let keep = self.keep_data;
        self.job.update(cx, |job, cx| match action {
            Action::Update => job.start(move |sink| run_update(record, sink), cx),
            Action::Repair => job.start(move |sink| run_repair(record, sink), cx),
            Action::Uninstall => job.start(move |sink| run_uninstall(record, keep, sink), cx),
            Action::ChangeDatabase => {}
        });
        cx.notify();
    }

    fn confirm(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if action == Action::ChangeDatabase {
            let exe = self_install::manager_exe();
            let exe = if exe.exists() { exe } else { std::env::current_exe().unwrap_or(exe) };
            if std::process::Command::new(&exe).args(["--manager", "--page", "migration"]).spawn().is_ok() {
                window.remove_window();
            }
            return;
        }
        let this = cx.entity().downgrade();
        let keep = self.keep_data;
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let (title, text, ok) = match action {
                Action::Update => (
                    "Update SENTIENT?",
                    "SENTIENT stops, its database is copied, and the new version's database changes are tried before they are made. If anything fails, the previous version and its database are put back and started.",
                    "Update",
                ),
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
}

impl Render for Maintenance {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = v_flex().size_full().p_8().gap_4().bg(theme.muted).text_color(theme.foreground);

        let Some(record) = &self.record else {
            return page.child(div().text_2xl().child("SENTIENT Setup")).child(ui::not_installed(&theme));
        };
        let job = self.job.read(cx);
        let busy = job.is_running();
        let header = format!("SENTIENT {} is installed on this computer", record.sentient_version);

        if let Some(action) = self.running {
            let outcome = match job.state() {
                JobState::Done(m) => Some((Tone::Good, m.clone())),
                JobState::Failed(m) => Some((Tone::Bad, m.clone())),
                _ => None,
            };
            let log: Vec<SharedString> = job.log().iter().rev().take(12).rev().cloned().collect();
            return page
                .child(div().text_2xl().child(action.title()))
                .child(div().text_sm().child(job.step().clone()))
                .when(busy, |p| p.when_some(job.percent(), |p, v| p.child(ProgressBar::new("maint").value(v))))
                .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                .child(
                    ui::panel(&theme)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .children(log.into_iter().map(|l| div().child(l))),
                )
                .when(!busy, |p| {
                    p.child(h_flex().child(
                        Button::new("close").primary().label("Close").on_click(|_, window, _| window.remove_window()),
                    ))
                });
        }

        let cards = Action::ALL.into_iter().map(|action| {
            let available = true;
            h_flex()
                .id(action.id())
                .gap_4()
                .p_4()
                .rounded(theme.radius)
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .when(available, |c| c.cursor_pointer().hover(|c| c.border_color(theme.primary)))
                .when(!available, |c| c.opacity(0.6))
                .child(action.icon().text_color(theme.primary))
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1()
                        .child(div().font_weight(FontWeight::MEDIUM).child(action.title()))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(action.describe())),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.confirm(action, window, cx)))
        });
        page.child(div().text_2xl().child(header))
            .child(div().text_color(theme.muted_foreground).child("Choose what to do with the existing installation."))
            .children(cards)
            .child(
                Checkbox::new("keep-data")
                    .label("When uninstalling, keep the database")
                    .checked(self.keep_data)
                    .disabled(busy)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.keep_data = *value;
                        cx.notify();
                    })),
            )
    }
}
