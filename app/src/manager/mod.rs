// Copyright © 2016-2026 The SENTIENT Authors

//! Manager mode: the app the operator opens from the Start menu after
//! installation. A left navigation of the management screens; each screen is
//! its own view, created the first time it is opened.

mod backup;
mod control;
mod logs;
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
use crate::{theme, ui};

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

    /// For the screens not built yet.
    fn summary(self) -> &'static str {
        match self {
            Page::Backup => "Full backups of the database and all telemetry, scheduled or on demand, and restore.",
            Page::Migration => "Change the database type, verified before switching over.",
            Page::Settings => "Every option chosen at install time; shows which services restart.",
            Page::Repair => "Re-register services and restore missing files, or uninstall.",
            _ => "",
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
}

impl Manager {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            page: Page::Status,
            record: InstallationRecord::load(),
            status: None,
            control: None,
            logs: None,
            update: None,
            backup: None,
        }
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
            Page::Status => self
                .status
                .get_or_insert_with(|| cx.new(|cx| status::StatusPage::new(record, cx)))
                .clone()
                .into_any_element(),
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
            page => {
                let theme = cx.theme().clone();
                ui::page(&theme, page.title(), page.summary())
                    .child(
                        ui::panel(&theme).child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("This screen is part of a later phase of the GPUI port."),
                        ),
                    )
                    .into_any_element()
            }
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
