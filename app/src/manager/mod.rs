// Copyright © 2016-2026 The SENTIENT Authors

//! Manager mode: the app the operator opens from the Start menu after
//! installation. A left navigation of the management screens; each screen is
//! its own view, created the first time it is opened.

mod about;
mod backup;
mod control;
mod gateway;
mod instances;
mod logs;
pub(crate) mod migration;
mod remote_pages;
mod repair;
mod settings;
pub(crate) mod setting_field;
mod status;
pub(crate) mod update;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::Selectable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::installation::InstallationRecord;
use crate::theme;
use crate::ui;
use crate::sysmon::Monitor;
use crate::updates::UpdateState;

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
    /// This computer and remote instances (below the divider).
    Instances,
    /// About and preferences of the Platform Manager (the (i) at the
    /// bottom of the menu, not a menu entry).
    About,
}

impl Page {
    /// The menu, in its order: Platform settings last.
    const ALL: [Page; 9] = [
        Page::Status,
        Page::Control,
        Page::Logs,
        Page::Update,
        Page::Gateway,
        Page::Backup,
        Page::Migration,
        Page::Repair,
        Page::Settings,
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
            Page::Instances => "instances",
            Page::About => "about",
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
            Page::Settings => "Platform settings",
            Page::Repair => "Repair and uninstall",
            Page::Instances => "Instances",
            Page::About => "About and preferences",
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
            Page::Instances => Icon::new(Lucide::Server),
            Page::About => Icon::new(IconName::Info),
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
    about: Option<Entity<about::AboutPage>>,
    instances: Option<Entity<instances::InstancesPage>>,
    /// The remote instances (crate::remote), for the switcher.
    remotes: Vec<crate::remote::Instance>,
    /// The remote instance that is open (None: this computer).
    open_remote: Option<String>,
    /// That instance's processor, memory and disk (None until read, or
    /// when it does not answer).
    remote_monitor: Option<Monitor>,
    /// The pages of the open remote instance.
    remote_pages: Option<Entity<remote_pages::RemotePages>>,
    /// Takes the keyboard while a dialog is open (Escape closes it).
    modal_focus: FocusHandle,
    /// The open instance's processor, memory and disk (top bar).
    monitor: Option<Monitor>,
    updates: Entity<UpdateState>,
    /// This run follows a self-update from that version (`--updated-from`):
    /// say so once.
    updated_from: Option<String>,
    /// Setup installed this Platform Manager beside a SENTIENT it found:
    /// say so once.
    found_notice: bool,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl Manager {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // `--page migration` (from Setup's Change database).
        let args: Vec<String> = std::env::args().collect();
        let page = args
            .windows(2)
            .find(|w| w[0] == "--page")
            .and_then(|w| Page::ALL.into_iter().chain([Page::Instances, Page::About]).find(|p| p.id() == w[1]))
            .unwrap_or(Page::Status);
        let record = InstallationRecord::load().inspect(InstallationRecord::use_pg_tools);
        let installed = record.as_ref().map(|r| r.sentient_version.clone());
        let native = record.as_ref().is_some_and(|r| r.platform().needs_package());
        let updates = cx.new(|cx| UpdateState::new(installed, native, cx));
        let subscriptions = vec![cx.observe(&updates, |_, _, cx| cx.notify())];
        let updated_from = args.windows(2).find(|w| w[0] == "--updated-from").map(|w| w[1].clone());
        let found_notice = record.is_some() && crate::updates::take_found_installation();
        let mut manager = Self {
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
            about: None,
            instances: None,
            remotes: crate::remote::load(),
            open_remote: None,
            remote_monitor: None,
            remote_pages: None,
            modal_focus: cx.focus_handle(),
            monitor: None,
            updates,
            updated_from,
            found_notice,
            _subscriptions: subscriptions,
            _tasks: Vec::new(),
        };
        manager.start_monitor(cx);
        // Screenshots: `--instance <id>` opens a remote instance on `--page`.
        #[cfg(debug_assertions)]
        if let Some(id) = args.windows(2).find(|w| w[0] == "--instance").map(|w| w[1].clone()) {
            manager.open_instance(Some(id), cx);
            manager.page = page;
        }
        manager
    }

    /// Read the open instance's processor, memory and disk every few seconds.
    fn start_monitor(&mut self, cx: &mut Context<Self>) {
        let dir = self
            .record
            .as_ref()
            .map(|r| if r.pg_dir.as_os_str().is_empty() { r.install_dir.clone() } else { r.pg_dir.clone() })
            .unwrap_or_else(|| std::path::PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }));
        let task = cx.spawn(async move |this, cx| loop {
            let d = dir.clone();
            let m = cx.background_executor().spawn(async move { crate::sysmon::sample(&d) }).await;
            if this
                .update(cx, |page, cx| {
                    page.monitor = Some(m);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
        });
        self._tasks.push(task);
    }

    /// What needs attention, for the bell: (text, page it opens).
    fn attention(&self, cx: &App) -> Vec<(String, Page)> {
        let mut items = Vec::new();
        let (sentient, manager) = self.updates.read(cx).notices();
        if let Some(o) = manager {
            items.push((format!("Platform Manager {} is available", o.version), Page::Update));
        }
        if let Some(o) = sentient {
            items.push((format!("SENTIENT {} is available", o.version), Page::Update));
        }
        if let Some(o) = self.updates.read(cx).gateway_notice() {
            items.push((format!("Sentient Gateway {} is available", o.version), Page::Gateway));
        }
        let pending = self.record.as_ref().map(|r| crate::changes::pending(r).len()).unwrap_or(0);
        if pending > 0 {
            items.push((format!("{pending} change{} for this installation waiting", if pending == 1 { "" } else { "s" }), Page::Update));
        }
        let shown = if self.open_remote.is_some() { self.remote_monitor } else { self.monitor };
        if let Some(m) = shown.filter(|m| m.disk_low()) {
            items.push((format!("Disk almost full: {} free", crate::dbstatus::bytes_label(m.disk_free)), Page::Status));
        }
        items
    }

    /// The top bar: the Platform Manager's icon, the instance that is
    /// open, its processor / memory / disk, and what needs attention. It
    /// takes the open instance's colour (SENTIENT teal for this computer).
    fn top_bar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = theme::is_dark(cx);
        let remote = self.open_remote.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned();
        let (bg, fg): (Hsla, Hsla) = match &remote {
            Some(r) => {
                let c = crate::remote::parse_hex(&r.color).unwrap_or(0x187074);
                let bg: Hsla = rgb(c).into();
                (if dark { bg.blend(gpui_kit::black().opacity(0.25)) } else { bg }, rgb(crate::remote::text_on(&r.color)).into())
            }
            None => (if dark { rgb(0x155e61).into() } else { rgb(0x187074).into() }, rgb(0xffffff).into()),
        };
        let dim = fg.opacity(0.75);
        let amber: Hsla = rgb(0xfbbf24).into();
        let instance = match (&remote, &self.record) {
            (Some(r), _) => format!("{} · {}", r.name, r.host),
            (None, Some(r)) => format!("This computer · SENTIENT {}", r.sentient_version),
            (None, None) => "This computer".to_string(),
        };
        let meter = |id: &'static str, icon: Icon, label: &'static str, value: String, fraction: f32, high: bool| {
            h_flex()
                .id(id)
                .gap_1p5()
                .items_center()
                .text_size(px(13.))
                .text_color(if high { amber } else { fg })
                .child(icon.size_4())
                .child(div().child(label))
                .child(
                    div()
                        .w(px(36.))
                        .h(px(5.))
                        .rounded_full()
                        .bg(fg.opacity(0.25))
                        .child(div().h_full().rounded_full().w(relative(fraction.clamp(0.0, 1.0))).bg(if high { amber } else { fg })),
                )
                .child(div().whitespace_nowrap().child(value))
        };
        // What fits: a rough width per piece (about 7 px a character at
        // 13-14 px), kept in this order: CPU, memory, disk, the product name.
        let text_w = |s: &str| s.chars().count() as f32 * 7.2;
        let mut room = width - 24. - (22. + 12.) - (text_w(&instance) + 40. + 12.) - if remote.is_some() { 72. } else { 0. } - 48. - 12.;
        let mut fits = |need: f32| {
            let ok = room >= need;
            if ok {
                room -= need;
            }
            ok
        };
        let shown = if remote.is_some() { self.remote_monitor } else { self.monitor };
        let (show_cpu, show_title, show_mem, show_disk) = match shown {
            Some(_) => {
                let cpu = fits(16. + 122. + 12.);
                let mem = cpu && fits(16. + 196.);
                let disk = mem && fits(16. + 186.);
                let title = fits(text_w("SENTIENT Platform Manager") + 12.);
                (cpu, title, mem, disk)
            }
            None => (false, fits(text_w("SENTIENT Platform Manager") + 12.), false, false),
        };
        let monitor = shown.map(|m| {
            let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
            h_flex()
                .id("monitor")
                .gap_4()
                .px_2()
                .py_1()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|d| d.bg(fg.opacity(0.1)))
                .when(show_cpu, |r| r.child(meter("m-cpu", Icon::new(IconName::Cpu), "CPU", format!("{:.0}%", m.cpu), m.cpu / 100.0, m.cpu_high())))
                .when(show_mem, |r| r.child(meter(
                    "m-mem",
                    Icon::new(IconName::MemoryStick),
                    "Memory",
                    format!("{:.1} / {:.0} GB", gb(m.mem_used), gb(m.mem_total)),
                    if m.mem_total > 0 { m.mem_used as f32 / m.mem_total as f32 } else { 0.0 },
                    m.mem_high(),
                )))
                .when(show_disk, |r| r.child(meter(
                    "m-disk",
                    Icon::new(IconName::HardDrive),
                    "Disk",
                    format!("{} free", crate::dbstatus::bytes_label(m.disk_free)),
                    if m.disk_total > 0 { 1.0 - m.disk_free as f32 / m.disk_total as f32 } else { 0.0 },
                    m.disk_low(),
                )))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.page = Page::Status;
                    cx.notify();
                }))
        });
        let items = self.attention(cx);
        let count = items.len();
        let bell = Button::new("bell")
            .ghost()
            .icon(Icon::new(IconName::Bell).text_color(fg))
            .when(count > 0, |b| b.label(count.to_string()))
            .text_color(fg)
            .tooltip(if count == 0 { "Nothing needs attention" } else { "Needs attention" })
            .dropdown_menu({
                let this = cx.entity().downgrade();
                move |menu, _, _| {
                    let mut menu = menu;
                    if items.is_empty() {
                        menu = menu.item(PopupMenuItem::new("Nothing needs attention").disabled(true));
                    }
                    for (text, page) in &items {
                        let this = this.clone();
                        let page = *page;
                        menu = menu.item(PopupMenuItem::new(text.clone()).on_click(move |_, _, cx| {
                            this.update(cx, |m, cx| {
                                m.page = page;
                                cx.notify();
                            })
                            .ok();
                        }));
                    }
                    menu
                }
            });
        h_flex()
            .id("top-bar")
            .h(px(42.))
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .items_center()
            .bg(bg)
            .text_color(fg)
            .child(ui::product_logo(ui::Product::Manager, 22.))
            .when(show_title, |b| b.child(div().text_sm().font_weight(FontWeight::MEDIUM).whitespace_nowrap().child("SENTIENT Platform Manager")))
            .child(
                Button::new("switcher")
                    .ghost()
                    .label(instance)
                    .icon(Icon::new(IconName::ChevronDown).size_3p5().text_color(dim))
                    .text_color(fg)
                    .bg(fg.opacity(0.14))
                    .dropdown_menu({
                        let this = cx.entity().downgrade();
                        let remotes = self.remotes.clone();
                        let open = self.open_remote.clone();
                        move |menu, _, _| {
                            let t = this.clone();
                            let mut menu = menu.item(PopupMenuItem::new("This computer").when(open.is_some(), |i| i.icon(Lucide::Monitor)).checked(open.is_none()).on_click(move |_, _, cx| {
                                t.update(cx, |m, cx| m.open_instance(None, cx)).ok();
                            }));
                            if !remotes.is_empty() {
                                menu = menu.separator();
                            }
                            for r in &remotes {
                                let (t, id) = (this.clone(), r.id.clone());
                                menu = menu.item(PopupMenuItem::new(format!("{} · {}", r.name, r.host)).when(open.as_deref() != Some(r.id.as_str()), |i| i.icon(Lucide::Server)).checked(open.as_deref() == Some(r.id.as_str())).on_click(move |_, _, cx| {
                                    t.update(cx, |m, cx| m.open_instance(Some(id.clone()), cx)).ok();
                                }));
                            }
                            let t = this.clone();
                            menu.separator().item(PopupMenuItem::new("Manage instances…").icon(Lucide::Settings).on_click(move |_, _, cx| {
                                t.update(cx, |m, cx| {
                                    m.page = Page::Instances;
                                    cx.notify();
                                })
                                .ok();
                            }))
                        }
                    }),
            )
            .when(remote.is_some(), |b| {
                b.child(div().px_1p5().py_0p5().rounded(px(4.)).border_1().border_color(fg.opacity(0.6)).text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Remote"))
            })
            .child(div().flex_1())
            .children(monitor)
            .child(bell)
    }

    /// Switch to an instance (None: this computer). Remote instances are
    /// worked on from their card on Instances for now.
    fn open_instance(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if id.is_some() {
            self.page = Page::Status;
        }
        self.open_remote = id.clone();
        self.remote_monitor = None;
        self.remote_pages = None;
        if let Some(inst) = id.as_ref().and_then(|id| self.remotes.iter().find(|r| &r.id == id)).cloned() {
            // Read it every few seconds while it stays open.
            let task = cx.spawn(async move |this, cx| loop {
                let i = inst.clone();
                let m = cx.background_executor().spawn(async move { crate::remote::monitor(&i).ok() }).await;
                let still_open = this
                    .update(cx, |page, cx| {
                        let open = page.open_remote.as_deref() == Some(inst.id.as_str());
                        if open {
                            page.remote_monitor = m;
                            cx.notify();
                        }
                        open
                    })
                    .unwrap_or(false);
                if !still_open {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
            });
            self._tasks.push(task);
        }
        if let Some(page) = &self.instances {
            page.update(cx, |p, cx| {
                p.open = id;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// The Update screen, made when first needed.
    fn update_page(&mut self, cx: &mut Context<Self>) -> Entity<update::UpdatePage> {
        let record = self.record.clone();
        let updates = self.updates.clone();
        self.update.get_or_insert_with(|| cx.new(|cx| update::UpdatePage::new(record, updates, cx))).clone()
    }

    /// Open the Update screen on a product's release notes.
    fn whats_new(&mut self, product: update::Product, cx: &mut Context<Self>) {
        let page = self.update_page(cx);
        page.update(cx, |p, cx| p.show_notes(product, cx));
        self.page = Page::Update;
        cx.notify();
    }

    /// The notices above every screen: what this run follows (a self-update,
    /// a Setup that found SENTIENT) and a newer SENTIENT, Platform Manager
    /// or gateway on the update channel (crate::updates).
    fn notices(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let (sentient, manager) = self.updates.read(cx).notices();
        let gw = self.updates.read(cx).gateway_notice();
        let pending = self.record.as_ref().map(|r| crate::changes::pending(r).len()).unwrap_or(0);
        if sentient.is_none() && manager.is_none() && gw.is_none() && self.updated_from.is_none() && !self.found_notice {
            return None;
        }
        let bar = |id: &'static str, tone: crate::ui::Tone, text: String| {
            let (bg, fg) = match tone {
                crate::ui::Tone::Good => (theme.success, theme.success_foreground),
                _ => (theme.accent, theme.accent_foreground),
            };
            h_flex()
                .id(id)
                .gap_3()
                .px_4()
                .py_2()
                .items_center()
                .bg(bg)
                .text_color(fg)
                .border_b_1()
                .border_color(theme.border)
                .child(Icon::new(if matches!(tone, crate::ui::Tone::Good) { IconName::CircleCheck } else { IconName::Info }).small())
                .child(div().flex_1().text_sm().child(text))
        };
        let mut col = v_flex();
        // B3/B4: this run follows a self-update.
        if let Some(from) = self.updated_from.clone() {
            let this = crate::updates::manager_version();
            let text = if pending > 0 {
                format!("Platform Manager updated to {this} (from {from}). {pending} change{} for this installation are waiting on the Update screen.", if pending == 1 { "" } else { "s" })
            } else {
                format!("Platform Manager updated to {this}.")
            };
            col = col.child(
                bar("notice-updated", crate::ui::Tone::Good, text)
                    .when(pending > 0, |b| {
                        b.child(Button::new("notice-updated-changes").primary().label("Show the changes").on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Update;
                            cx.notify();
                        })))
                    })
                    .child(Button::new("notice-updated-close").ghost().label("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.updated_from = None;
                        cx.notify();
                    }))),
            );
        }
        // C7c: Setup installed this Platform Manager beside a SENTIENT.
        if self.found_notice {
            if let Some(r) = &self.record {
                col = col.child(
                    bar(
                        "notice-found",
                        crate::ui::Tone::Good,
                        format!("Found SENTIENT {} on this computer with its database, settings and backups. Nothing was changed.", r.sentient_version),
                    )
                    .child(Button::new("notice-found-close").ghost().label("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.found_notice = false;
                        cx.notify();
                    }))),
                );
            }
        }
        match (&manager, &sentient) {
            // A4: both; the Platform Manager goes first.
            (Some(m), Some(s)) => {
                let (mv, sv) = (m.version.clone(), s.version.clone());
                col = col.child(
                    bar("notice-both", crate::ui::Tone::Neutral, format!("Platform Manager {} and SENTIENT {} are available.", m.version, s.version))
                        .when(m.installer.is_some(), |b| {
                            b.child(Button::new("notice-both-go").primary().label("Update Platform Manager").on_click(cx.listener(|this, _, _, cx| {
                                let page = this.update_page(cx);
                                page.update(cx, |p, cx| p.start_manager_update(cx));
                                this.page = Page::Update;
                                cx.notify();
                            })))
                        })
                        .child(Button::new("notice-both-notes").ghost().label("What's new").on_click(cx.listener(|this, _, _, cx| this.whats_new(update::Product::Manager, cx))))
                        .child(Button::new("notice-both-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                            let (mv, sv) = (mv.clone(), sv.clone());
                            this.updates.update(cx, |s, cx| {
                                s.dismiss_manager(mv, cx);
                                s.dismiss_sentient(sv, cx);
                            });
                        }))),
                );
            }
            // A2: a new Platform Manager.
            (Some(m), None) => {
                let v = m.version.clone();
                col = col.child(
                    bar("notice-manager", crate::ui::Tone::Neutral, format!("Platform Manager {} is available.", m.version))
                        .when(m.installer.is_some(), |b| {
                            b.child(Button::new("notice-manager-go").primary().label("Update and restart").on_click(cx.listener(|this, _, _, cx| {
                                let page = this.update_page(cx);
                                page.update(cx, |p, cx| p.start_manager_update(cx));
                                this.page = Page::Update;
                                cx.notify();
                            })))
                        })
                        .child(Button::new("notice-manager-notes").ghost().label("What's new").on_click(cx.listener(|this, _, _, cx| this.whats_new(update::Product::Manager, cx))))
                        .child(Button::new("notice-manager-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                            let v = v.clone();
                            this.updates.update(cx, |s, cx| s.dismiss_manager(v, cx));
                        }))),
                );
            }
            // A3: a new SENTIENT.
            (None, Some(s)) => {
                let v = s.version.clone();
                col = col.child(
                    bar("notice-sentient", crate::ui::Tone::Neutral, format!("SENTIENT {} is available{}.", s.version, dated(&s.date)))
                        .child(Button::new("notice-sentient-go").primary().label("Update…").on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Update;
                            cx.notify();
                        })))
                        .child(Button::new("notice-sentient-notes").ghost().label("What's new").on_click(cx.listener(|this, _, _, cx| this.whats_new(update::Product::Sentient, cx))))
                        .child(Button::new("notice-sentient-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                            let v = v.clone();
                            this.updates.update(cx, |s, cx| s.dismiss_sentient(v, cx));
                        }))),
                );
            }
            (None, None) => {}
        }
        if let Some(o) = gw {
            let v = o.version.clone();
            col = col.child(
                bar("notice-gateway", crate::ui::Tone::Neutral, format!("Sentient Gateway {} is available{}.", o.version, dated(&o.date)))
                    .child(Button::new("notice-gateway-go").primary().label("Update…").on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Gateway;
                        cx.notify();
                    })))
                    .child(Button::new("notice-gateway-later").ghost().label("Not now").on_click(cx.listener(move |this, _, _, cx| {
                        let v = v.clone();
                        this.updates.update(cx, |s, cx| s.dismiss_gateway(v, cx));
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

    fn nav(&self, rail: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let items = Page::ALL.into_iter().map(|page| {
            let active = page == self.page;
            h_flex()
                .id(page.id())
                .gap_3()
                .when(rail, |r| r.justify_center().px_2())
                .when(!rail, |r| r.px_4())
                .py_2()
                .rounded(theme.radius)
                .cursor_pointer()
                .text_sm()
                .when(active, |row| {
                    row.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground)
                })
                .when(!active, |row| row.hover(|row| row.bg(theme.list_hover)))
                .child(page.icon().size(px(20.)))
                .when(!rail, |r| r.child(page.title()))
                .when(rail, |r| r.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(page.title()).build(window, cx)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = page;
                    cx.notify();
                }))
        });
        let choice = theme::Choice::saved();
        v_flex()
            .when(rail, |n| n.w(px(64.)))
            .when(!rail, |n| n.w_56())
            .h_full()
            .flex_shrink_0()
            .p_3()
            .gap_1()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                // The menu scrolls when the window is short; the bottom group stays.
                v_flex()
                    .id("nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(items)
                    .child(div().my_1().h(px(1.)).flex_shrink_0().bg(theme.sidebar_border))
                    .child({
                let active = self.page == Page::Instances;
                h_flex()
                    .id("instances")
                    .gap_3()
                    .when(rail, |r| r.justify_center().px_2())
                    .when(!rail, |r| r.px_4())
                    .py_2()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .text_sm()
                    .when(active, |row| row.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                    .when(!active, |row| row.hover(|row| row.bg(theme.list_hover)))
                    .child(Page::Instances.icon().size(px(20.)))
                    .when(!rail, |r| r.child(Page::Instances.title()))
                    .when(rail, |r| r.tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Instances").build(window, cx)))
                    .when(!self.remotes.is_empty() && !rail, |r| r.child(div().flex_1()).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child((self.remotes.len() + 1).to_string())))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Instances;
                        cx.notify();
                    }))
                    }),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .when(rail, |r| r.flex_col().px_0())
                    .when(!rail, |r| r.px_2())
                    .pt_2()
                    .gap_1()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .when(!rail, |r| r.child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(format!("v{}", crate::updates::manager_version())),
                    ))
                    .child({
                        let b = Button::new("about").ghost().icon(IconName::Info).tooltip("About and preferences").on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::About;
                            cx.notify();
                        }));
                        if self.page == Page::About { b.selected(true) } else { b }
                    })
                    .child(
                        Button::new("theme")
                            .ghost()
                            .icon(match choice {
                                theme::Choice::Light => Icon::new(IconName::Sun),
                                theme::Choice::Dark => Icon::new(IconName::Moon),
                                theme::Choice::System => Icon::new(Lucide::Monitor),
                            })
                            .tooltip("Theme")
                            .dropdown_menu(move |menu, _, _| {
                                let mut menu = menu;
                                for c in theme::Choice::ALL {
                                    menu = menu.item(PopupMenuItem::new(c.label())
                                            .when(c != choice, |i| {
                                                i.icon(match c {
                                                    theme::Choice::Light => Lucide::Sun,
                                                    theme::Choice::Dark => Lucide::Moon,
                                                    theme::Choice::System => Lucide::Monitor,
                                                })
                                            })
                                            .checked(c == choice).on_click(move |_, window, cx| theme::choose(c, Some(window), cx)));
                                }
                                menu
                            }),
                    ),
            )
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let record = self.record.clone();
        if let Some(r) = self.open_remote.as_ref().and_then(|id| self.remotes.iter().find(|i| &i.id == id)).cloned() {
            let kind = match self.page {
                Page::Status => Some(remote_pages::Kind::Status),
                Page::Control => Some(remote_pages::Kind::Control),
                Page::Logs => Some(remote_pages::Kind::Logs),
                Page::Update => Some(remote_pages::Kind::Update),
                Page::Backup => Some(remote_pages::Kind::Backup),
                Page::Settings => Some(remote_pages::Kind::Settings),
                Page::Migration => Some(remote_pages::Kind::Migration),
                Page::Repair => Some(remote_pages::Kind::Repair),
                Page::Gateway => Some(remote_pages::Kind::Gateway),
                _ => None,
            };
            if let Some(kind) = kind {
                let page = match &self.remote_pages {
                    Some(p) => {
                        p.update(cx, |p, cx| p.show(kind, cx));
                        p.clone()
                    }
                    None => {
                        let updates = self.updates.clone();
                        let p = cx.new(|cx| remote_pages::RemotePages::new(r.clone(), kind, updates, window, cx));
                        self.remote_pages = Some(p.clone());
                        p
                    }
                };
                return page.into_any_element();
            }
            if !matches!(self.page, Page::Instances | Page::About) {
                let theme = cx.theme().clone();
                return ui::page(&theme, self.page.title(), "")
                    .child(
                        ui::panel(&theme)
                            .child(div().child(format!("{} is open. This page works on this computer only for now; Status, Control, Logs, Update and Backup and restore work on {}.", r.name, r.name)))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Button::new("to-instances").primary().label(format!("Go to {}", r.name)).on_click(cx.listener(|this, _, _, cx| {
                                        this.page = Page::Instances;
                                        cx.notify();
                                    })))
                                    .child(Button::new("to-local").ghost().label("Back to this computer").on_click(cx.listener(|this, _, _, cx| this.open_instance(None, cx)))),
                            ),
                    )
                    .into_any_element();
            }
        }
        match self.page {
            Page::Instances => {
                if self.instances.is_none() {
                    let open = self.open_remote.clone();
                    let page = cx.new(|cx| {
                        let mut p = instances::InstancesPage::new(record, cx);
                        // An instance opened before this page was first shown.
                        p.open = open;
                        p
                    });
                    self._subscriptions.push(cx.subscribe(&page, |this, _, e: &instances::OpenInstance, cx| {
                        this.open_instance(e.0.clone(), cx);
                    }));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &instances::ListChanged, cx| {
                        this.remotes = crate::remote::load();
                        if this.open_remote.as_ref().is_some_and(|id| !this.remotes.iter().any(|r| &r.id == id)) {
                            this.open_remote = None;
                        }
                        cx.notify();
                    }));
                    self.instances = Some(page);
                }
                self.instances.clone().unwrap().into_any_element()
            }
            Page::Status => {
                if self.status.is_none() {
                    let updates = self.updates.clone();
                    let page = cx.new(|cx| status::StatusPage::new(record, updates, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &migration::RecordChanged, cx| this.reload(cx)));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &status::OpenGateway, cx| {
                        this.page = Page::Gateway;
                        cx.notify();
                    }));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &status::OpenUpdate, cx| {
                        this.page = Page::Update;
                        cx.notify();
                    }));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, go: &status::Go, cx| {
                        this.page = match go {
                            status::Go::Backup => Page::Backup,
                            status::Go::Settings => Page::Settings,
                            status::Go::Migration => Page::Migration,
                        };
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
            Page::Update => self.update_page(cx).into_any_element(),
            Page::Gateway => {
                let updates = self.updates.clone();
                self.gateway
                    .get_or_insert_with(|| cx.new(|cx| gateway::GatewayPage::new(record, updates, window, cx)))
                    .clone()
                    .into_any_element()
            }
            Page::Backup => {
                if self.backup.is_none() {
                    let page = cx.new(|cx| backup::BackupPage::new(record, window, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, go: &backup::Go, cx| {
                        this.page = match go {
                            backup::Go::Update => Page::Update,
                            backup::Go::Settings => Page::Settings,
                        };
                        cx.notify();
                    }));
                    self.backup = Some(page);
                }
                self.backup.clone().unwrap().into_any_element()
            }
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
            Page::About => {
                if self.about.is_none() {
                    let updates = self.updates.clone();
                    let page = cx.new(|cx| about::AboutPage::new(updates, cx));
                    self._subscriptions.push(cx.subscribe(&page, |this, _, _: &about::ShowWhatsNew, cx| this.whats_new(update::Product::Manager, cx)));
                    self.about = Some(page);
                }
                self.about.clone().unwrap().into_any_element()
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
        // The layout follows the window: an icon rail below RAIL_BELOW px,
        // fewer monitors in the top bar as it narrows.
        let width = f32::from(window.viewport_size().width);
        let top = self.top_bar(width, cx);
        let nav = self.nav(width < RAIL_BELOW, cx);
        let notices = self.notices(cx);
        let content = self.content(window, cx);
        let rail = width < RAIL_BELOW;
        let layer = crate::modal::layer(window, cx, if rail { 64. } else { 224. }, 42.);
        if layer.is_some() && window.focused(cx).is_none() {
            window.focus(&self.modal_focus, cx);
        }
        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.modal_focus)
            // Escape closes the open dialog, from anywhere in the window.
            .on_key_down(|e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && cx.try_global::<crate::modal::ModalSlot>().is_some_and(|s| s.0.is_some()) {
                    crate::modal::dismiss(window, cx);
                }
            })
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(top)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(nav)
                    .child(v_flex().flex_1().h_full().min_w_0().children(notices).child(crate::ui::appear(h_flex().flex_1().min_h_0().w_full().child(content), SharedString::from(format!("page-{}", self.page.id())), 140))),
            )
            .children(layer)
    }
}

/// Below this window width the menu becomes an icon rail.
pub(crate) const RAIL_BELOW: f32 = 960.;

/// ", released 2026-10-24", or nothing.
fn dated(date: &str) -> String {
    if date.is_empty() { String::new() } else { format!(", released {date}") }
}
