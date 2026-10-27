// Copyright © 2016-2026 The SENTIENT Authors

//! Manager mode: the app the operator opens from the Start menu after
//! installation. A left navigation of the management screens; each screen is
//! its own view, created the first time it is opened.

mod backup;
mod control;
mod gateway;
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
use gpui_kit::component::Disableable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::theme;
use crate::updates::{Offer, UpdateState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Status,
    Control,
    Logs,
    Update,
    Gateway,
    Backup,
    Migration,
    Settings,
    Repair,
}

impl Page {
    const ALL: [Page; 9] = [
        Page::Status,
        Page::Control,
        Page::Logs,
        Page::Update,
        Page::Gateway,
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
            Page::Gateway => "gateway",
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
            Page::Gateway => "Gateway",
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
            Page::Gateway => Icon::new(Lucide::Network),
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
    gateway: Option<Entity<gateway::GatewayPage>>,
    backup: Option<Entity<backup::BackupPage>>,
    migration: Option<Entity<migration::MigrationPage>>,
    settings: Option<Entity<settings::SettingsPage>>,
    repair: Option<Entity<repair::RepairPage>>,
    updates: Entity<UpdateState>,
    /// Installing a new Platform Manager from the notice.
    self_update: Entity<Job>,
    _subscriptions: Vec<Subscription>,
}

impl Manager {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // `--page migration` (from Setup's Change database).
        let args: Vec<String> = std::env::args().collect();
        let page = args
            .windows(2)
            .find(|w| w[0] == "--page")
            .and_then(|w| Page::ALL.into_iter().find(|p| p.id() == w[1]))
            .unwrap_or(Page::Status);
        let record = InstallationRecord::load().inspect(InstallationRecord::use_pg_tools);
        let installed = record.as_ref().map(|r| r.sentient_version.clone());
        let native = record.as_ref().is_some_and(|r| r.platform().needs_package());
        let updates = cx.new(|cx| UpdateState::new(installed, native, cx));
        let self_update = cx.new(|_| Job::new());
        let subscriptions = vec![
            cx.observe(&updates, |_, _, cx| cx.notify()),
            cx.observe(&self_update, |_, job, cx| {
                // The new program is running and replaces this one.
                if matches!(job.read(cx).state(), JobState::Done(_)) {
                    cx.quit();
                }
                cx.notify();
            }),
        ];
        Self {
            page,
            record,
            status: None,
            control: None,
            logs: None,
            update: None,
            gateway: None,
            backup: None,
            migration: None,
            settings: None,
            repair: None,
            updates,
            self_update,
            _subscriptions: subscriptions,
        }
    }

    /// The notices above every screen: a newer SENTIENT or Platform Manager
    /// on the update channel (crate::updates).
    fn notices(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let (sentient, manager) = self.updates.read(cx).notices();
        let gw = self.updates.read(cx).gateway_notice();
        let job = self.self_update.read(cx);
        let installing = job.is_running();
        let failed = match job.state() {
            JobState::Failed(m) => Some(m.clone()),
            _ => None,
        };
        let step = job.step_with_time();
        if sentient.is_none() && manager.is_none() && gw.is_none() {
            return None;
        }
        let bar = |id: &'static str, text: String| {
            h_flex()
                .id(id)
                .gap_3()
                .px_4()
                .py_2()
                .items_center()
                .bg(theme.accent)
                .text_color(theme.accent_foreground)
                .border_b_1()
                .border_color(theme.border)
                .child(Icon::new(IconName::Info).small())
                .child(div().flex_1().text_sm().child(text))
        };
        let mut col = v_flex();
        if let Some(Offer { version, date, .. }) = sentient {
            let v = version.clone();
            col = col.child(
                bar("notice-sentient", format!("SENTIENT {version} is available{}.", dated(&date)))
                    .child(Button::new("notice-sentient-go").primary().small().label("Update…").on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Update;
                        cx.notify();
                    })))
                    .child(Button::new("notice-sentient-later").ghost().small().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                        let v = v.clone();
                        this.updates.update(cx, |s, cx| s.dismiss_sentient(v, cx));
                    }))),
            );
        }
        if let Some(o) = gw {
            let v = o.version.clone();
            col = col.child(
                bar("notice-gateway", format!("Sentient Gateway {} is available{}.", o.version, dated(&o.date)))
                    .child(Button::new("notice-gateway-go").primary().small().label("Update…").on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Gateway;
                        cx.notify();
                    })))
                    .child(Button::new("notice-gateway-later").ghost().small().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                        let v = v.clone();
                        this.updates.update(cx, |s, cx| s.dismiss_gateway(v, cx));
                    }))),
            );
        }
        if let Some(offer) = manager {
            let v = offer.version.clone();
            let text = match (&failed, installing) {
                (_, true) => format!("Platform Manager {}: {step}", offer.version),
                (Some(e), _) => format!("Platform Manager {} could not be installed: {e}", offer.version),
                _ => format!("Platform Manager {} is available{} (this is {}).", offer.version, dated(&offer.date), crate::updates::manager_version()),
            };
            let page = offer.page.clone();
            col = col.child(
                bar("notice-manager", text)
                    .when(offer.installer.is_some(), |b| {
                        b.child(
                            Button::new("notice-manager-go")
                                .primary()
                                .small()
                                .label(if installing { "Installing…" } else { "Install and restart" })
                                .disabled(installing)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let offer = offer.clone();
                                    this.self_update.update(cx, |job, cx| {
                                        job.start(move |sink| crate::updates::install_manager(&offer, &sink).map(|_| ()), cx)
                                    });
                                })),
                        )
                    })
                    .child(Button::new("notice-manager-page").ghost().small().label("Release notes").on_click(move |_, _, cx| crate::ui::open_url(&page, cx)))
                    .child(Button::new("notice-manager-later").ghost().small().label("Not now").disabled(installing).on_click(cx.listener(move |this, _, _, cx| {
                        let v = v.clone();
                        this.updates.update(cx, |s, cx| s.dismiss_manager(v, cx));
                    }))),
            );
        }
        Some(col.into_any_element())
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

    /// Like `reload`, but the Repair screen keeps showing what it just did.
    fn reload_keeping_repair(&mut self, cx: &mut Context<Self>) {
        let repair = self.repair.take();
        self.reload(cx);
        self.migration = None;
        self.settings = None;
        self.repair = repair;
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
                            .child(format!("v{}", crate::updates::manager_version())),
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
                    let updates = self.updates.clone();
                    let page = cx.new(|cx| status::StatusPage::new(record, updates, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &status::OpenGateway, cx| {
                        this.page = Page::Gateway;
                        cx.notify();
                    }));
                    self.status = Some(page);
                }
                self.status.clone().unwrap().into_any_element()
            }
            Page::Control => self
                .control
                .get_or_insert_with(|| cx.new(|cx| control::ControlPage::new(record, window, cx)))
                .clone()
                .into_any_element(),
            Page::Logs => self
                .logs
                .get_or_insert_with(|| cx.new(|cx| logs::LogsPage::new(record, window, cx)))
                .clone()
                .into_any_element(),
            Page::Update => {
                let updates = self.updates.clone();
                self.update
                    .get_or_insert_with(|| cx.new(|cx| update::UpdatePage::new(record, updates, cx)))
                    .clone()
                    .into_any_element()
            }
            Page::Gateway => {
                let updates = self.updates.clone();
                self.gateway
                    .get_or_insert_with(|| cx.new(|cx| gateway::GatewayPage::new(record, updates, window, cx)))
                    .clone()
                    .into_any_element()
            }
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
            Page::Repair => {
                if self.repair.is_none() {
                    let page = cx.new(|cx| repair::RepairPage::new(record, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload_keeping_repair(cx)));
                    self.repair = Some(page);
                }
                self.repair.clone().unwrap().into_any_element()
            }
        }
    }
}

impl Render for Manager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = self.nav(cx);
        let notices = self.notices(cx);
        let content = self.content(window, cx);
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(nav)
            .child(v_flex().flex_1().h_full().min_w_0().children(notices).child(h_flex().flex_1().min_h_0().w_full().child(content)))
    }
}

/// ", released 2026-10-24", or nothing.
fn dated(date: &str) -> String {
    if date.is_empty() { String::new() } else { format!(", released {date}") }
}
