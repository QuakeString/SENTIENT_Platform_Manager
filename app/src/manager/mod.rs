// Copyright © 2016-2026 The SENTIENT Authors

//! Manager mode: the app the operator opens from the Start menu after
//! installation. A left navigation of the management screens; each screen is
//! its own view, created the first time it is opened.

mod backup;
mod control;
mod logs;
pub(crate) mod migration;
mod repair;
mod settings;
mod status;
mod update;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Status,
    Control,
    Logs,
    Update,
    Backup,
    Migration,
    Settings,
    Repair,
}

impl Page {
    const ALL: [Page; 8] = [
        Page::Status,
        Page::Control,
        Page::Logs,
        Page::Update,
        Page::Backup,
        Page::Migration,
        Page::Settings,
        Page::Repair,
    ];

    fn id(self) -> &'static str {
        match self {
            Page::Status => "status",
            Page::Control => "control",
            Page::Logs => "logs",
            Page::Update => "update",
            Page::Backup => "backup",
            Page::Migration => "migration",
            Page::Settings => "settings",
            Page::Repair => "repair",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Status => "Status",
            Page::Control => "Control",
            Page::Logs => "Logs",
            Page::Update => "Update",
            Page::Backup => "Backup and restore",
            Page::Migration => "Migration",
            Page::Settings => "Settings",
            Page::Repair => "Repair and uninstall",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Page::Status => Icon::new(Lucide::Activity),
            Page::Control => Icon::new(Lucide::Power),
            Page::Logs => Icon::new(IconName::SquareTerminal),
            Page::Update => Icon::new(IconName::RefreshCw),
            Page::Backup => Icon::new(Lucide::Archive),
            Page::Migration => Icon::new(Lucide::ArrowLeftRight),
            Page::Settings => Icon::new(IconName::Settings),
            Page::Repair => Icon::new(Lucide::Wrench),
        }
    }
}

pub struct Manager {
    page: Page,
    record: Option<InstallationRecord>,
    status: Option<Entity<status::StatusPage>>,
    control: Option<Entity<control::ControlPage>>,
    logs: Option<Entity<logs::LogsPage>>,
    update: Option<Entity<update::UpdatePage>>,
    backup: Option<Entity<backup::BackupPage>>,
    migration: Option<Entity<migration::MigrationPage>>,
    settings: Option<Entity<settings::SettingsPage>>,
    repair: Option<Entity<repair::RepairPage>>,
    _subscriptions: Vec<Subscription>,
}

impl Manager {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        // `--page migration` (from Setup's Change database).
        let args: Vec<String> = std::env::args().collect();
        let page = args
            .windows(2)
            .find(|w| w[0] == "--page")
            .and_then(|w| Page::ALL.into_iter().find(|p| p.id() == w[1]))
            .unwrap_or(Page::Status);
        Self {
            page,
            record: InstallationRecord::load().inspect(InstallationRecord::use_pg_tools),
            status: None,
            control: None,
            logs: None,
            update: None,
            backup: None,
            migration: None,
            settings: None,
            repair: None,
            _subscriptions: Vec::new(),
        }
    }

    /// The record changed (a migration): every screen starts again from it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let was_installed = self.record.is_some();
        self.record = InstallationRecord::load().inspect(InstallationRecord::use_pg_tools);
        if !was_installed {
            // Adopted: every screen was showing "not installed".
            self.migration = None;
            self.settings = None;
        }
        self.status = None;
        self.control = None;
        self.logs = None;
        self.update = None;
        self.backup = None;
        self.repair = None;
        // Migration and Settings reload their own record and keep showing
        // the result of what just ran.
        cx.notify();
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let items = Page::ALL.into_iter().map(|page| {
            let active = page == self.page;
            h_flex()
                .id(page.id())
                .gap_3()
                .px_4()
                .py_2()
                .rounded(theme.radius)
                .cursor_pointer()
                .text_sm()
                .when(active, |row| {
                    row.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground)
                })
                .when(!active, |row| row.hover(|row| row.bg(theme.list_hover)))
                .child(page.icon().small())
                .child(page.title())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = page;
                    cx.notify();
                }))
        });
        let dark = theme::is_dark(cx);
        v_flex()
            .w_56()
            .h_full()
            .flex_shrink_0()
            .p_3()
            .gap_1()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                v_flex()
                    .px_4()
                    .pt_2()
                    .pb_4()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).text_color(theme.primary).child("SENTIENT"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child("Platform Manager")),
            )
            .children(items)
            .child(div().flex_1())
            .child(
                h_flex()
                    .px_4()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(
                        Button::new("theme")
                            .ghost()
                            .small()
                            .icon(if dark { IconName::Sun } else { IconName::Moon })
                            .tooltip(if dark { "Light theme" } else { "Dark theme" })
                            .on_click(|_, _, cx| theme::toggle(cx)),
                    ),
            )
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let record = self.record.clone();
        match self.page {
            Page::Status => {
                if self.status.is_none() {
                    let page = cx.new(|cx| status::StatusPage::new(record, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self.status = Some(page);
                }
                self.status.clone().unwrap().into_any_element()
            }
            Page::Control => self
                .control
                .get_or_insert_with(|| cx.new(|cx| control::ControlPage::new(record, cx)))
                .clone()
                .into_any_element(),
            Page::Logs => self
                .logs
                .get_or_insert_with(|| cx.new(|cx| logs::LogsPage::new(record, window, cx)))
                .clone()
                .into_any_element(),
            Page::Update => self
                .update
                .get_or_insert_with(|| cx.new(|cx| update::UpdatePage::new(record, cx)))
                .clone()
                .into_any_element(),
            Page::Backup => self
                .backup
                .get_or_insert_with(|| cx.new(|cx| backup::BackupPage::new(record, window, cx)))
                .clone()
                .into_any_element(),
            Page::Migration => {
                if self.migration.is_none() {
                    let page = cx.new(|cx| migration::MigrationPage::new(record, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self.migration = Some(page);
                }
                self.migration.clone().unwrap().into_any_element()
            }
            Page::Settings => {
                if self.settings.is_none() {
                    let page = cx.new(|cx| settings::SettingsPage::new(record, window, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self.settings = Some(page);
                }
                self.settings.clone().unwrap().into_any_element()
            }
            Page::Repair => self
                .repair
                .get_or_insert_with(|| cx.new(|cx| repair::RepairPage::new(record, cx)))
                .clone()
                .into_any_element(),
        }
    }
}

impl Render for Manager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = self.nav(cx);
        let content = self.content(window, cx);
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(nav)
            .child(content)
    }
}
