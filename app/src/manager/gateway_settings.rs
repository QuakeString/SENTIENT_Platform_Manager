// Copyright © 2016-2026 The SENTIENT Authors

//! The Gateway page's settings card, here and on a server: seven tabs in
//! the look of Platform settings (sentient_installer_core::gateway_settings
//! reads and applies them).
//!
//! - Connection to SENTIENT: host, port, credential, TLS; locked while the
//!   gateway is connected (SENTIENT owns it then). Test connection checks
//!   that SENTIENT's MQTT answers (TCP, and TLS when used), not yet with
//!   the gateway's credentials.
//! - Local web page: on or off, where it listens, its port, its extra host
//!   names, and the administrator password (a dialog; the password goes to
//!   the gateway's own command on its standard input).
//! - Connectors, Storage, Statistics: SENTIENT's, shown locked.
//! - Logging: SENTIENT's levels locked; the level before SENTIENT
//!   connects (RUST_LOG) is ours.
//! - Advanced: restart after a failure, serial ports, environment
//!   overrides; remote shell and rate limits locked.
//!
//! The card holds the values and the edits; its page reads the settings
//! and runs what Apply, the password and the test ask for ([`Work`]), here
//! or through the server's agent.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use sentient_installer_core::gateway_settings::{self as gs, ApplyPlan, GwSettings};

use crate::ui::{self, SettingRow, Tone};

/// What the card asks its page to run.
pub enum Work {
    Apply(ApplyPlan),
    Password(String),
    Test,
}

pub type RunFn = Rc<dyn Fn(Work, &mut Window, &mut App)>;

pub const TITLES: [&str; 7] = ["Connection to SENTIENT", "Local web page", "Connectors", "Storage", "Logging", "Statistics", "Advanced"];
/// The settings card's least height: its tabs, three rows and the Apply
/// bar, so that under the status card the bar is in view on the smallest
/// window (760 px tall); below that the page scrolls.
const CARD_MIN_H: f32 = 290.;
const LEVEL_WORDS: [&str; 3] = ["Errors", "Normal", "Detailed"];
const LISTEN_WORDS: [&str; 3] = ["This computer", "All interfaces", "One address"];

/// The values the card edits, as read or as changed.
#[derive(Clone, Debug, PartialEq, Default)]
struct Edit {
    ui_on: bool,
    listen_mode: usize,
    restart_on: bool,
    serial_group: bool,
    serial_devices: Vec<String>,
    level: Option<usize>,
}

/// One environment override row: its name and value fields (a secret has
/// no value field: it is kept as it is).
struct EnvRow {
    name: Entity<InputState>,
    value: Option<Entity<InputState>>,
    editable: bool,
    origin: String,
    /// As read (None: added here).
    was: Option<(String, Option<String>)>,
}

pub struct GatewaySettingsCard {
    settings: Option<Result<GwSettings, String>>,
    /// The settings the fields were filled from.
    filled_from: Option<GwSettings>,
    /// SENTIENT's Gateways page, when its address is known.
    platform_url: Option<String>,
    /// The server's name, for a server's card (None: this computer).
    server: Option<String>,
    tab: usize,
    base: Edit,
    edit: Edit,
    addr: Option<Entity<InputState>>,
    port: Option<Entity<InputState>>,
    base_addr: String,
    base_port: String,
    env: Vec<EnvRow>,
    removed: usize,
    new_name: Option<Entity<InputState>>,
    new_value: Option<Entity<InputState>>,
    test: Option<(Tone, String)>,
    testing: bool,
    busy: bool,
    run: RunFn,
    _subs: Vec<Subscription>,
}

impl GatewaySettingsCard {
    pub fn new(server: Option<String>, run: RunFn) -> Self {
        Self {
            settings: None,
            filled_from: None,
            platform_url: None,
            server,
            // Screens: SPM_DEBUG_GATEWAY_TAB opens a tab (0 to 6).
            tab: if cfg!(debug_assertions) { std::env::var("SPM_DEBUG_GATEWAY_TAB").ok().and_then(|t| t.parse().ok()).filter(|t: &usize| *t < 7).unwrap_or(0) } else { 0 },
            base: Edit::default(),
            edit: Edit::default(),
            addr: None,
            port: None,
            base_addr: String::new(),
            base_port: String::new(),
            env: Vec::new(),
            removed: 0,
            new_name: None,
            new_value: None,
            test: None,
            testing: false,
            busy: false,
            run,
            _subs: Vec::new(),
        }
    }

    /// New settings from the page's read (no notify: the page renders next).
    pub fn feed(&mut self, s: Option<Result<GwSettings, String>>, platform_url: Option<String>, busy: bool) {
        self.settings = s;
        self.platform_url = platform_url;
        self.busy = busy;
    }

    /// Fill the fields again from what is read next (after Apply, Discard).
    pub fn reset(&mut self) {
        self.filled_from = None;
    }

    pub fn set_test(&mut self, r: Option<Result<String, String>>, cx: &mut Context<Self>) {
        self.testing = r.is_none();
        self.test = r.map(|r| match r {
            Ok(m) => (Tone::Good, m),
            Err(e) => (Tone::Bad, e),
        });
        cx.notify();
    }

    fn input(&mut self, value: String, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let p: SharedString = placeholder.to_string().into();
        let i = cx.new(|cx| InputState::new(window, cx).default_value(value).placeholder(p));
        self._subs.push(cx.subscribe(&i, |_, _, _: &InputEvent, cx| cx.notify()));
        i
    }

    /// The fields from `s`, unless changes are waiting.
    fn fill(&mut self, s: &GwSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.filled_from.as_ref() == Some(s) || (self.filled_from.is_some() && self.changes(cx) > 0) {
            return;
        }
        let (mode, addr, port) = gs::listen_parts(&s.ui.listen);
        self.base = Edit {
            ui_on: s.ui.enabled,
            listen_mode: mode,
            restart_on: s.machine.restart_always.unwrap_or(false),
            serial_group: s.machine.serial.in_group,
            serial_devices: s.machine.serial.chosen.clone(),
            level: gs::level_choice(s.machine.rust_log.as_deref()),
        };
        self.edit = self.base.clone();
        self._subs.clear();
        self.base_addr = addr.clone();
        self.base_port = port.to_string();
        self.addr = Some(self.input(addr, "192.168.1.20", window, cx));
        self.port = Some(self.input(port.to_string(), "8642", window, cx));
        self.env = Vec::new();
        for e in &s.machine.env {
            let name = self.input(e.name.clone(), "", window, cx);
            let value = match &e.value {
                Some(v) => Some(self.input(v.clone(), "", window, cx)),
                None => None,
            };
            self.env.push(EnvRow { name, value, editable: e.editable, origin: e.origin.clone(), was: Some((e.name.clone(), e.value.clone())) });
        }
        self.removed = 0;
        self.new_name = Some(self.input(String::new(), "ST_GW_…", window, cx));
        self.new_value = Some(self.input(String::new(), "value", window, cx));
        let first = self.filled_from.is_none() && self.edit == self.base;
        self.filled_from = Some(s.clone());
        // Screens: SPM_DEBUG_GATEWAY_PENDING=1 changes three settings (the
        // Apply bar); =apply also applies them (the job view; a canned
        // gateway is never changed).
        if cfg!(debug_assertions) && first && s.machine.not_editable.is_none() {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if let Ok(v) = std::env::var("SPM_DEBUG_GATEWAY_PENDING") {
                if !DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    self.edit.ui_on = !self.base.ui_on;
                    self.edit.restart_on = !self.base.restart_on;
                    self.edit.level = Some(2);
                    if v == "apply" {
                        cx.on_next_frame(window, |c, window, cx| c.apply(window, cx));
                    }
                }
            }
        }
    }

    fn text(i: &Option<Entity<InputState>>, cx: &App) -> String {
        i.as_ref().map(|i| i.read(cx).value().trim().to_string()).unwrap_or_default()
    }

    fn env_now(&self, cx: &App) -> Vec<(String, String)> {
        self.env
            .iter()
            .filter(|r| r.editable)
            .filter_map(|r| Some((r.name.read(cx).value().trim().to_string(), r.value.as_ref()?.read(cx).value().to_string())))
            .collect()
    }

    fn env_changed(&self, cx: &App) -> bool {
        self.removed > 0
            || self.env.iter().any(|r| match (&r.was, &r.value) {
                (None, _) => true,
                (Some((n, Some(v))), Some(i)) => r.name.read(cx).value().trim() != n || i.read(cx).value() != v.as_str(),
                _ => false,
            })
    }

    /// Changes per tab: (Local web page, Logging, Advanced).
    fn counts(&self, cx: &App) -> (usize, usize, usize) {
        let (b, e) = (&self.base, &self.edit);
        let page = (e.ui_on != b.ui_on) as usize
            + (e.ui_on && (e.listen_mode != b.listen_mode || (e.listen_mode == 2 && Self::text(&self.addr, cx) != self.base_addr))) as usize
            + (e.ui_on && Self::text(&self.port, cx) != self.base_port) as usize;
        let log = (e.level != b.level) as usize;
        let adv = (e.restart_on != b.restart_on) as usize + (e.serial_group != b.serial_group || e.serial_devices != b.serial_devices) as usize + self.env_changed(cx) as usize;
        (page, log, adv)
    }

    fn changes(&self, cx: &App) -> usize {
        let (a, b, c) = self.counts(cx);
        a + b + c
    }

    /// What is not valid: (tab, the bar's words).
    fn invalid(&self, cx: &App) -> Option<(usize, String)> {
        if self.edit.ui_on {
            let port = Self::text(&self.port, cx);
            if !port.parse::<u16>().is_ok_and(|p| p > 0) {
                return Some((1, "The page's port needs a number from 1 to 65535.".into()));
            }
            if self.edit.listen_mode == 2 && Self::text(&self.addr, cx).is_empty() {
                return Some((1, "Name the address the page listens on.".into()));
            }
        }
        for (n, _) in self.env_now(cx) {
            if !valid_name(&n) {
                return Some((6, format!("{n}: not a usable variable name (letters, digits and _).")));
            }
        }
        None
    }

    fn plan(&self, cx: &App) -> ApplyPlan {
        let (b, e) = (&self.base, &self.edit);
        let mut p = ApplyPlan::default();
        if e.ui_on != b.ui_on {
            p.ui_enabled = Some(e.ui_on);
        }
        let port: u16 = Self::text(&self.port, cx).parse().unwrap_or(sentient_installer_core::gateway::UI_PORT);
        let listen = gs::listen_of(e.listen_mode, &Self::text(&self.addr, cx), port);
        let base_listen = gs::listen_of(b.listen_mode, &self.base_addr, self.base_port.parse().unwrap_or(sentient_installer_core::gateway::UI_PORT));
        if e.ui_on && listen != base_listen {
            p.ui_listen = Some(listen);
        }
        if e.restart_on != b.restart_on {
            p.restart_always = Some(e.restart_on);
        }
        if e.serial_group != b.serial_group {
            p.serial_group = Some(e.serial_group);
        }
        if e.serial_devices != b.serial_devices {
            p.serial_devices = Some(e.serial_devices.clone());
        }
        if e.level != b.level {
            p.rust_log = Some(e.level.map(|i| gs::LEVELS[i].to_string()).unwrap_or_default());
        }
        if self.env_changed(cx) {
            p.env = Some(self.env_now(cx));
        }
        p
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        self.filled_from = None;
        self.edit = self.base.clone();
        cx.notify();
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.invalid(cx).is_some() {
            return;
        }
        let plan = self.plan(cx);
        if plan.is_empty() {
            return;
        }
        self.filled_from = None;
        (self.run)(Work::Apply(plan), window, cx);
    }

    fn open_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let run = self.run.clone();
        let server = self.server.clone();
        let view = cx.new(|cx| PasswordDialog::new(server, run, window, cx));
        crate::modal::open(crate::modal::Modal { title: "Change the web page's password".into(), body: view.into(), width: 460., icon: None, on_close: None }, cx);
    }
}

fn valid_name(n: &str) -> bool {
    let mut c = n.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// The status card's pill for the connection to SENTIENT (None: not known).
pub fn connected_pill(theme: &gpui_kit::component::Theme, s: Option<&Result<GwSettings, String>>, running: bool) -> Option<Div> {
    let c = s.and_then(|s| s.as_ref().ok()).and_then(|s| s.connected)?;
    running.then(|| if c { ui::pill(theme, Tone::Good, "Connected to SENTIENT") } else { ui::pill(theme, Tone::Bad, "Not connected to SENTIENT") })
}

/// Whether the connection is SENTIENT's now: connected, or not known.
pub fn connection_locked(s: &GwSettings) -> bool {
    s.connected != Some(false)
}

/// SENTIENT's Gateways page for a gateway that sends to `host`, when that
/// is the SENTIENT at `base` (`base` its address, `same`: the host names
/// that machine).
pub fn sentient_page(base: Option<&str>, host: Option<&str>, same: &[&str]) -> Option<String> {
    let base = base?.trim_end_matches('/');
    let h = host?.trim();
    let local = h == "127.0.0.1" || h == "localhost" || h == "::1" || same.iter().any(|s| s.eq_ignore_ascii_case(h));
    local.then(|| format!("{base}/entities/gateways"))
}

/// A value shown with the lock and its reason, in the field column.
fn locked(theme: &gpui_kit::component::Theme, id: &str, label: &str, value: impl Into<SharedString>, why: &str) -> SettingRow {
    let id: SharedString = format!("lock-{id}").into();
    SettingRow {
        label: label.to_string().into(),
        help: None,
        desc: None,
        restart: false,
        changed: false,
        control: h_flex()
            .w(px(ui::FIELD_W))
            .gap(px(8.))
            .items_center()
            .child(ui::lock_tip(id, why.to_string().into()))
            .child(ui::row_text(value).text_color(theme.foreground).min_w_0().overflow_hidden().text_ellipsis())
            .into_any_element(),
        wide: false,
        current: None,
        first: false,
    }
}

/// A plain value (not locked, not editable here).
fn plain(theme: &gpui_kit::component::Theme, label: &str, help: Option<String>, value: impl Into<SharedString>) -> SettingRow {
    let mut r = SettingRow::wide(label.to_string(), help, ui::row_text(value).text_color(theme.foreground));
    r.wide = true;
    r
}

/// The line on top of a tab SENTIENT owns: the lock, what it says, the
/// way to SENTIENT when its address is known.
fn notice(theme: &gpui_kit::component::Theme, id: &str, text: &str, why: &str, url: Option<&str>) -> Div {
    h_flex()
        .mx(px(18.))
        .mt(px(14.))
        .px(px(12.))
        .py(px(8.))
        .gap(px(8.))
        .items_center()
        .rounded(theme.radius)
        .bg(theme.secondary)
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(ui::lock_tip(format!("notice-lock-{id}").into(), why.to_string().into()))
        .child(div().flex_1().min_w_0().child(text.to_string()))
        .when_some(url.map(str::to_string), |d, url| {
            d.child(
                div()
                    .id(SharedString::from(format!("open-sentient-{id}")))
                    .text_color(theme.primary)
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|d| d.underline())
                    .child("Open in SENTIENT")
                    .on_click(move |_, _, cx| ui::open_url(&url, cx)),
            )
        })
}

/// An informational line on top of a tab (not a lock).
fn info_line(theme: &gpui_kit::component::Theme, id: &str, text: &str, why: Option<&str>) -> Div {
    h_flex()
        .mx(px(18.))
        .mt(px(14.))
        .px(px(12.))
        .py(px(8.))
        .gap(px(8.))
        .items_center()
        .rounded(theme.radius)
        .bg(theme.secondary)
        .text_sm()
        .text_color(theme.muted_foreground)
        .when_some(why.map(str::to_string), |d, w| d.child(ui::info(format!("info-line-{id}").into(), w.into())))
        .child(div().flex_1().min_w_0().child(text.to_string()))
}

const OWNED: &str = "SENTIENT manages this for the gateway and sends it again on every connection; a change made on this computer would be undone. Change it in SENTIENT › Gateways › this gateway › Configuration.";

impl Render for GatewaySettingsCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        ui::set_stacked(f32::from(window.viewport_size().width) < ui::ROW_STACK_W);
        let shell = |body: AnyElement| {
            v_flex()
                .relative()
                .min_w_0()
                .rounded(px(ui::PANEL_RADIUS))
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(ui::layout_probe("panel"))
                .child(ui::layout_probe("gw-settings-card"))
                .child(body)
        };
        let s = match &self.settings {
            None => return shell(v_flex().p(px(18.)).gap_2().child(ui::skeleton_line(&theme, "gw-set-1", 220.)).child(ui::skeleton_line(&theme, "gw-set-2", 320.)).into_any_element()).into_any_element(),
            Some(Err(e)) => return shell(div().p(px(18.)).text_sm().text_color(theme.muted_foreground).child(format!("Its settings: {e}")).into_any_element()).into_any_element(),
            Some(Ok(s)) => s.clone(),
        };
        self.fill(&s, window, cx);
        let (page_n, log_n, adv_n) = self.counts(cx);
        let invalid = self.invalid(cx);
        let mut badges = vec![(0usize, false); 7];
        badges[1] = (page_n, invalid.as_ref().is_some_and(|(t, _)| *t == 1));
        badges[4] = (log_n, false);
        badges[6] = (adv_n, invalid.as_ref().is_some_and(|(t, _)| *t == 6));
        let conn_locked = connection_locked(&s);
        let locks = [conn_locked, false, true, true, false, true, false];
        let this = cx.entity().downgrade();
        let tabs = ui::card_tabs_locked(&theme, "gw-", &TITLES, self.tab, &badges, &locks, Rc::new(move |i, cx| {
            this.update(cx, |c, cx| {
                c.tab = i;
                cx.notify();
            })
            .ok();
        }));
        let body: Vec<AnyElement> = match self.tab {
            0 => self.connection(&theme, &s, cx),
            1 => self.page(&theme, &s, window, cx),
            2 => self.connectors(&theme, &s),
            3 => self.owned(&theme, &s, "storage", "SENTIENT manages storage for this gateway. A change there takes effect at once, with no restart.", "Buffer", &s.storage, cx),
            4 => self.logging(&theme, &s, cx),
            5 => self.owned(&theme, &s, "statistics", "SENTIENT manages statistics for this gateway.", "Sent to SENTIENT", &s.statistics, cx),
            _ => self.advanced(&theme, &s, window, cx),
        };
        let changes = page_n + log_n + adv_n;
        let editable = s.machine.not_editable.is_none();
        let plan = self.plan(cx);
        let restarts = plan.restarts(&s.machine.kind);
        let line = match &invalid {
            Some((_, why)) => why.clone(),
            None => {
                let mut names = plan.names().join(", ");
                if !names.is_empty() {
                    names.push_str(". ");
                }
                names.push_str(plan.restart_words(&s.machine.kind));
                if self.server.is_some() {
                    names.push_str(" Applied by the server's agent.");
                }
                names
            }
        };
        let bar = ui::pending_bar(
            &theme,
            "gw-pending",
            changes,
            invalid.is_some() as usize,
            line,
            restarts,
            !self.busy && editable,
            cx.listener(|c, _, _, cx| c.discard(cx)),
            cx.listener(|c, _, window, cx| c.apply(window, cx)),
        );
        let mut top: Vec<AnyElement> = Vec::new();
        if let Some(n) = s.note.clone().filter(|_| s.source == "none") {
            top.push(info_line(&theme, "note", &n, None).into_any_element());
        }
        // As Platform settings: the card fills what the page has left (at
        // least CARD_MIN_H), its rows scroll inside it and the Apply bar
        // stays at its bottom edge, in view.
        div()
            .flex_1()
            .min_h(px(CARD_MIN_H))
            .flex()
            .flex_col()
            .min_w_0()
            .child(ui::settings_card(&theme, "gw-settings-body", tabs, v_flex().pb(px(12.)).children(top).children(body).child(ui::layout_probe("gw-settings-card")), bar))
            .into_any_element()
    }
}

impl GatewaySettingsCard {
    fn connection(&mut self, theme: &gpui_kit::component::Theme, s: &GwSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = &s.conn;
        let locked_now = connection_locked(s);
        let host = c.host.clone().unwrap_or_else(|| "Not commissioned".into());
        let port = c.port.map(|p| p.to_string()).unwrap_or_else(|| "—".into());
        let cred = c.credential_words();
        let tls = if c.tls { "On" } else { "Off" };
        let mut out: Vec<AnyElement> = Vec::new();
        let rows: Vec<SettingRow> = if locked_now {
            let why = match s.connected {
                Some(true) => "Connected: SENTIENT owns the connection now. Change it in SENTIENT › Gateways › this gateway › Configuration › General. The gateway takes a change made on this computer only while it is not connected.".to_string(),
                _ => "Whether the gateway is connected is not known (it does not report it), so the connection is treated as SENTIENT's.".to_string(),
            };
            out.push(
                notice(theme, "conn", if s.connected == Some(true) { "Connected: SENTIENT owns the connection now." } else { "Not known whether it is connected: treated as SENTIENT's." }, &why, self.platform_url.as_deref())
                    .into_any_element(),
            );
            vec![locked(theme, "host", "Host", host, &why), locked(theme, "port", "MQTT port", port, &why), locked(theme, "cred", "Credential", cred, &why), locked(theme, "tls", "TLS", tls, &why)]
        } else {
            let how = s.connected_how.clone().map(|h| format!(" (from {h})")).unwrap_or_default();
            out.push(
                info_line(
                    theme,
                    "conn",
                    &format!("Not connected to SENTIENT{how}."),
                    Some("The gateway lets its connection be changed only while it is not connected. Changing it from the Platform Manager comes in a later version; until then use the gateway's own page or its --install command."),
                )
                .into_any_element(),
            );
            let env = if c.from_env { Some("Set in the container's environment: it is created again to change it.".to_string()) } else { None };
            vec![plain(theme, "Host", env.clone(), host), plain(theme, "MQTT port", env, port), plain(theme, "Credential", Some("Never shown in full: \"set\" or its last four characters at most.".into()), cred), plain(theme, "TLS", None, tls)]
        };
        out.push(ui::group(theme, "Platform", rows, vec![], cx).into_any_element());
        let busy = self.busy || self.testing;
        let button = crate::ui::probed(
            crate::ui::button("gw-test")
                .outline()
                .label("Test")
                .loading(self.testing)
                .disabled(busy || c.host.is_none())
                .on_click(cx.listener(|c, _, window, cx| {
                    c.testing = true;
                    c.test = None;
                    (c.run)(Work::Test, window, cx);
                    cx.notify();
                })),
        );
        let mut test = SettingRow::wide(
            "Test connection",
            Some("Checks that SENTIENT's MQTT answers where the gateway connects: a TCP connection, and a TLS handshake when it uses TLS. The gateway's credentials are not tried yet, so its own connection is never disturbed.".into()),
            ui::on_control_line(button),
        );
        test.first = true;
        out.push(ui::group(theme, "Check", vec![test], self.test.clone().map(|(t, m)| ui::outcome(theme, t, m).into_any_element()).into_iter().collect(), cx).into_any_element());
        out
    }

    fn page(&mut self, theme: &gpui_kit::component::Theme, s: &GwSettings, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        let editable = s.machine.not_editable.is_none();
        if let Some(why) = &s.machine.not_editable {
            out.push(info_line(theme, "page", "Shown as it is: these settings are not changed from here.", Some(why)).into_any_element());
        } else if !s.caps.ui_listen && !s.machine.kind.contains("docker") {
            out.push(info_line(theme, "page-old", "This gateway is older than 4.12: its page is set on its own command line only.", None).into_any_element());
        }
        let can_page = editable && (s.caps.ui_listen || s.machine.kind == "docker");
        let (b, e) = (self.base.clone(), self.edit.clone());
        let mut rows = Vec::new();
        let this = cx.entity().downgrade();
        let page_help = "The gateway's own page for commissioning and looking at values. Off: no web listener at all.".to_string();
        rows.push(if can_page {
            SettingRow {
                label: "Web page".into(),
                help: Some(page_help),
                desc: None,
                restart: e.ui_on != b.ui_on,
                changed: e.ui_on != b.ui_on,
                control: ui::toggle(theme, "gw-ui-on", e.ui_on, None, cx.listener(|c, v: &bool, _, cx| {
                    c.edit.ui_on = *v;
                    cx.notify();
                }))
                .into_any_element(),
                wide: false,
                current: Some(if b.ui_on { "On".into() } else { "Off".into() }),
                first: false,
            }
        } else {
            plain(theme, "Web page", Some(page_help), if s.ui.enabled { "On" } else { "Off" })
        });
        if can_page && e.ui_on {
            let pick: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(move |i, _, cx| {
                this.update(cx, |c, cx| {
                    c.edit.listen_mode = i;
                    cx.notify();
                })
                .ok();
            });
            let mode_changed = e.listen_mode != b.listen_mode;
            rows.push(SettingRow {
                label: "Listen on".into(),
                help: Some("This computer: 127.0.0.1, plain HTTP. All interfaces or one address: reachable from the network, served over HTTPS only.".into()),
                desc: None,
                restart: mode_changed,
                changed: mode_changed,
                control: ui::chips(theme, "gw-listen", &LISTEN_WORDS, Some(e.listen_mode), pick).into_any_element(),
                wide: false,
                current: Some(LISTEN_WORDS[b.listen_mode].to_string()),
                first: false,
            });
            if e.listen_mode == 2 {
                if let Some(addr) = self.addr.clone() {
                    let changed = Self::text(&self.addr, cx) != self.base_addr;
                    rows.push(SettingRow {
                        label: "Address".into(),
                        help: Some("One of this computer's own addresses.".into()),
                        desc: None,
                        restart: changed,
                        changed,
                        control: ui::field_input(theme, &addr, None, None, false, window, cx).into_any_element(),
                        wide: false,
                        current: Some(if self.base_addr.is_empty() { "none".into() } else { self.base_addr.clone() }),
                        first: false,
                    });
                }
            }
            if let Some(port) = self.port.clone() {
                let now = Self::text(&self.port, cx);
                let changed = now != self.base_port;
                let bad = (!now.parse::<u16>().is_ok_and(|p| p > 0)).then(|| "A number from 1 to 65535".to_string());
                rows.push(SettingRow {
                    label: "Port".into(),
                    help: None,
                    desc: None,
                    restart: changed,
                    changed,
                    control: ui::field_input(theme, &port, Some(ui::Slot::Unit("port".into())), bad, false, window, cx).into_any_element(),
                    wide: false,
                    current: Some(self.base_port.clone()),
                    first: false,
                });
            }
        } else if !can_page && s.ui.enabled {
            rows.push(plain(theme, "Listen on", None, s.ui.listen.clone()));
        }
        let names = if s.ui.hostnames.is_empty() { "none".to_string() } else { s.ui.hostnames.join(", ") };
        rows.push(plain(
            theme,
            "Extra host names",
            Some(if s.caps.ui_hostname {
                "Names besides the computer's own, written into the page's certificate.".into()
            } else {
                "Names besides the computer's own, written into the page's certificate. This gateway sets them in its file only (localUi.hostnames); the Platform Manager changes them once the gateway's command can.".into()
            }),
            names,
        ));
        out.push(ui::group(theme, "Page", rows, vec![], cx).into_any_element());
        let pw_word = match s.ui.password_set {
            Some(true) => "set",
            Some(false) => "not set",
            None => "not known",
        };
        let can_pw = editable && s.caps.ui_password && !self.busy;
        let mut pw = SettingRow {
            label: "Administrator password".into(),
            help: Some("Set through the gateway's own command (--ui-password, the password on its standard input; never on a command line or in a log). Every browser signed in to the page is signed out. No restart needed.".into()),
            desc: None,
            restart: false,
            changed: false,
            control: h_flex()
                .w(px(ui::FIELD_W))
                .child(crate::ui::probed(crate::ui::button("gw-pw").outline().label("Change password").disabled(!can_pw).on_click(cx.listener(|c, _, window, cx| c.open_password(window, cx)))))
                .into_any_element(),
            wide: false,
            current: Some(pw_word.to_string()),
            first: false,
        };
        pw.first = true;
        out.push(ui::group(theme, "Sign-in", vec![pw], vec![], cx).into_any_element());
        out
    }

    fn connectors(&self, theme: &gpui_kit::component::Theme, s: &GwSettings) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = vec![notice(theme, "connectors", "SENTIENT manages the connectors for this gateway.", OWNED, self.platform_url.as_deref()).into_any_element()];
        let cell = |t: String, w: f32| div().w(px(w)).flex_shrink_0().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(t);
        let head = h_flex()
            .px(px(18.))
            .pt(px(14.))
            .pb(px(6.))
            .gap(px(12.))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().child("CONNECTOR"))
            .child(cell("TYPE".into(), 130.))
            .child(cell("STATE".into(), 110.))
            .child(cell("DEVICES".into(), 70.))
            .child(div().w(px(22.)));
        out.push(head.into_any_element());
        if s.connectors.is_empty() {
            out.push(div().px(px(18.)).py(px(10.)).text_sm().text_color(theme.muted_foreground).child("No connectors yet.").into_any_element());
        }
        for (i, c) in s.connectors.iter().enumerate() {
            let state = c.state.clone().unwrap_or_else(|| match c.enabled {
                Some(false) => "Off".into(),
                _ => "On".into(),
            });
            let on = !matches!(state.to_ascii_lowercase().as_str(), "off" | "stopped" | "failed" | "disabled");
            out.push(
                h_flex()
                    .px(px(18.))
                    .py(px(8.))
                    .gap(px(12.))
                    .items_center()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_weight(FontWeight::MEDIUM).child(c.name.clone()))
                    .child(cell(c.kind.clone(), 130.))
                    .child(
                        h_flex()
                            .w(px(110.))
                            .flex_shrink_0()
                            .gap(px(6.))
                            .items_center()
                            .child(div().size(px(8.)).rounded_full().bg(if on { gpui_kit::rgb(ui::STATUS_GREEN).into() } else { Hsla::from(gpui_kit::rgb(ui::STATUS_GREY)) }))
                            .child(state),
                    )
                    .child(cell(c.devices.map(|d| d.to_string()).unwrap_or_else(|| "—".into()), 70.))
                    .child(ui::lock_tip(format!("conn-lock-{i}").into(), OWNED.into()))
                    .into_any_element(),
            );
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn owned(&self, theme: &gpui_kit::component::Theme, _s: &GwSettings, id: &str, text: &str, group: &str, facts: &[gs::Fact], cx: &App) -> Vec<AnyElement> {
        let rows: Vec<SettingRow> = facts.iter().map(|f| locked(theme, &format!("{id}-{}", f.label), &f.label, f.value.clone(), OWNED)).collect();
        vec![notice(theme, id, text, OWNED, self.platform_url.as_deref()).into_any_element(), ui::group(theme, group, rows, vec![], cx).into_any_element()]
    }

    fn logging(&mut self, theme: &gpui_kit::component::Theme, s: &GwSettings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let levels: Vec<SettingRow> = if s.logging.is_empty() {
            vec![locked(theme, "log-none", "Levels", "The gateway's own (no logs.json)", OWNED)]
        } else {
            s.logging.iter().map(|f| locked(theme, &format!("log-{}", f.label), &f.label, f.value.clone(), OWNED)).collect()
        };
        let mut out = vec![
            notice(theme, "logging", "SENTIENT sets these levels (logs_configuration, RemoteLoggingLevel).", OWNED, self.platform_url.as_deref()).into_any_element(),
            ui::group(theme, "Levels from SENTIENT", levels, vec![], cx).into_any_element(),
        ];
        let editable = s.machine.not_editable.is_none();
        let (b, e) = (self.base.level, self.edit.level);
        let current = s.machine.rust_log.clone().unwrap_or_else(|| "info (the gateway's own)".into());
        let help = "RUST_LOG for the service or the container: how much the gateway writes until SENTIENT's levels take over. Errors: error. Normal: info. Detailed: debug.".to_string();
        let mut row = if editable {
            let this = cx.entity().downgrade();
            let pick: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(move |i, _, cx| {
                this.update(cx, |c, cx| {
                    c.edit.level = Some(i);
                    cx.notify();
                })
                .ok();
            });
            SettingRow { label: "Level before SENTIENT connects".into(), help: Some(help), desc: None, restart: e != b, changed: e != b, control: ui::chips(theme, "gw-level", &LEVEL_WORDS, e, pick).into_any_element(), wide: false, current: Some(current), first: false }
        } else {
            plain(theme, "Level before SENTIENT connects", Some(help), current)
        };
        row.first = true;
        let files = plain(theme, "Log files", Some("The gateway's rotating log files; the Logs button shows them.".into()), s.log_files.clone().unwrap_or_else(|| "In its data folder".into()));
        out.push(ui::group(theme, "This computer", vec![row, files], vec![], cx).into_any_element());
        out
    }

    fn advanced(&mut self, theme: &gpui_kit::component::Theme, s: &GwSettings, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        let editable = s.machine.not_editable.is_none();
        if let Some(why) = &s.machine.not_editable {
            out.push(info_line(theme, "adv", "Shown as it is: these settings are not changed from here.", Some(why)).into_any_element());
        }
        let (b, e) = (self.base.clone(), self.edit.clone());
        let mut service: Vec<SettingRow> = Vec::new();
        let restart_help = match s.machine.kind.as_str() {
            "docker" => "The container's restart policy: unless-stopped when on, no when off. The container is created again with the same volumes.",
            "windows-service" => "What Windows does when the service fails: start it again after 5 seconds.",
            _ => "systemd Restart=always, RestartSec=5, in a drop-in of the Platform Manager's (the unit file is not changed).",
        };
        let restart_word = s.machine.restart_word.clone().unwrap_or_else(|| "not known".into());
        service.push(if editable && s.machine.restart_always.is_some() {
            SettingRow {
                label: "Restart after a failure".into(),
                help: Some(restart_help.into()),
                desc: None,
                restart: e.restart_on != b.restart_on && s.machine.kind != "windows-service",
                changed: e.restart_on != b.restart_on,
                control: ui::toggle(theme, "gw-restart", e.restart_on, None, cx.listener(|c, v: &bool, _, cx| {
                    c.edit.restart_on = *v;
                    cx.notify();
                }))
                .into_any_element(),
                wide: false,
                current: Some(restart_word),
                first: false,
            }
        } else {
            plain(theme, "Restart after a failure", Some(restart_help.into()), restart_word)
        });
        let serial = &s.machine.serial;
        match serial.mode.as_str() {
            "group" => {
                let g = serial.group.clone().unwrap_or_else(|| "dialout".into());
                let acc = serial.account.clone().unwrap_or_default();
                let help = format!("The service's account {acc} joins the group {g}, which may open serial ports (Modbus RTU and other serial connectors). Found here: {}.", if serial.available.is_empty() { "no serial adapter".to_string() } else { serial.available.join(", ") });
                service.push(if editable && serial.group.is_some() {
                    SettingRow {
                        label: "Serial ports".into(),
                        help: Some(help),
                        desc: None,
                        restart: e.serial_group != b.serial_group,
                        changed: e.serial_group != b.serial_group,
                        control: ui::toggle(theme, "gw-serial", e.serial_group, None, cx.listener(|c, v: &bool, _, cx| {
                            c.edit.serial_group = *v;
                            cx.notify();
                        }))
                        .into_any_element(),
                        wide: false,
                        current: Some(if b.serial_group { format!("in {g}") } else { format!("not in {g}") }),
                        first: false,
                    }
                } else {
                    plain(theme, "Serial ports", Some(help), if serial.in_group { format!("in {g}") } else { "no serial group".into() })
                });
            }
            "devices" => {
                use gpui_kit::component::checkbox::Checkbox;
                let mut all = serial.available.clone();
                for c in &serial.chosen {
                    if !all.contains(c) {
                        all.push(c.clone());
                    }
                }
                let help = "Each chosen adapter is given to the container at the same path (its /dev/serial/by-id name stays the one a connector names), with its group added. The container is created again with the same volumes.".to_string();
                let changed = e.serial_devices != b.serial_devices;
                let list = if all.is_empty() {
                    ui::row_text("No serial adapter on this computer").text_color(theme.muted_foreground).into_any_element()
                } else {
                    v_flex()
                        .gap(px(6.))
                        .children(all.iter().enumerate().map(|(i, d)| {
                            let on = e.serial_devices.contains(d);
                            let d2 = d.clone();
                            let name = d.rsplit('/').next().unwrap_or(d).to_string();
                            Checkbox::new(SharedString::from(format!("gw-dev-{i}")))
                                .label(name)
                                .checked(on)
                                .disabled(!editable)
                                .on_change(cx.listener(move |c, v: &bool, _, cx| {
                                    c.edit.serial_devices.retain(|x| x != &d2);
                                    if *v {
                                        c.edit.serial_devices.push(d2.clone());
                                        c.edit.serial_devices.sort();
                                    }
                                    cx.notify();
                                }))
                        }))
                        .into_any_element()
                };
                let mut r = SettingRow::wide("Serial ports", Some(help), list);
                r.changed = changed;
                r.restart = changed;
                service.push(r);
            }
            _ => {}
        }
        out.push(ui::group(theme, "Service", service, vec![], cx).into_any_element());
        // Environment overrides.
        let mut env_rows: Vec<SettingRow> = Vec::new();
        let mut remove: Option<usize> = None;
        let _ = &mut remove;
        for (i, r) in self.env.iter().enumerate() {
            let name = r.name.read(cx).value().to_string();
            match (&r.value, r.editable && editable) {
                (Some(v), true) => {
                    let changed = match &r.was {
                        None => true,
                        Some((n, Some(old))) => name.trim() != n || v.read(cx).value() != old.as_str(),
                        _ => false,
                    };
                    let bad = (!valid_name(name.trim())).then(|| "Letters, digits and _".to_string());
                    let control = h_flex()
                        .gap(px(8.))
                        .items_center()
                        .child(ui::field_input(theme, v, None, bad, false, window, cx))
                        .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("gw-env-rm-{i}"))).ghost().label("Remove").on_click(cx.listener(move |c, _, _, cx| {
                            if i < c.env.len() {
                                if c.env[i].was.is_some() {
                                    c.removed += 1;
                                }
                                c.env.remove(i);
                            }
                            cx.notify();
                        }))));
                    env_rows.push(SettingRow { label: name.into(), help: None, desc: None, restart: changed, changed, control: control.into_any_element(), wide: true, current: None, first: false });
                }
                (None, _) => env_rows.push(plain(theme, &name, Some("A secret: kept as it is, never shown.".into()), "set")),
                (Some(v), false) => env_rows.push(plain(theme, &name, Some(format!("From {}; not changed here.", r.origin)), v.read(cx).value().to_string())),
            }
        }
        let mut extra: Vec<AnyElement> = Vec::new();
        if editable {
            if let (Some(n), Some(v)) = (self.new_name.clone(), self.new_value.clone()) {
                let name = n.read(cx).value().trim().to_string();
                let ok = valid_name(&name) && !self.env.iter().any(|r| r.name.read(cx).value().trim() == name) && !matches!(name.as_str(), "ST_GW_UI" | "ST_GW_UI_LISTEN" | "RUST_LOG");
                extra.push(
                    h_flex()
                        .gap(px(8.))
                        .items_center()
                        .flex_wrap()
                        .child(div().w(px(200.)).child(Input::new(&n)))
                        .child(div().w(px(200.)).child(Input::new(&v)))
                        .child(crate::ui::probed(crate::ui::button("gw-env-add").outline().label("Add").disabled(!ok).on_click(cx.listener(|c, _, window, cx| {
                            let name = Self::text(&c.new_name, cx);
                            let value = c.new_value.as_ref().map(|v| v.read(cx).value().to_string()).unwrap_or_default();
                            let n = c.input(name, "", window, cx);
                            let v = c.input(value, "", window, cx);
                            c.env.push(EnvRow { name: n, value: Some(v), editable: true, origin: String::new(), was: None });
                            c.new_name = Some(c.input(String::new(), "ST_GW_…", window, cx));
                            c.new_value = Some(c.input(String::new(), "value", window, cx));
                            cx.notify();
                        }))))
                        .child(ui::info("gw-env-help".into(), env_help(&s.machine.kind).into()))
                        .into_any_element(),
                );
            }
        }
        if env_rows.is_empty() && extra.is_empty() {
            env_rows.push(plain(theme, "Environment overrides", Some(env_help(&s.machine.kind).into()), "none"));
        }
        out.push(ui::group(theme, "Environment overrides", env_rows, extra, cx).into_any_element());
        // SENTIENT's.
        let mut platform: Vec<SettingRow> = s
            .platform
            .iter()
            .map(|f| {
                let why = if f.label == "Remote shell" { "SENTIENT turns remote shell on or off (general_configuration › remoteShell). It is off by default." } else { OWNED };
                locked(theme, &format!("adv-{}", f.label), &f.label, f.value.clone(), why)
            })
            .collect();
        if platform.is_empty() {
            platform.push(locked(theme, "adv-none", "Remote shell", "Off", OWNED));
        }
        out.push(ui::group(theme, "From SENTIENT", platform, vec![], cx).into_any_element());
        out
    }
}

fn env_help(kind: &str) -> &'static str {
    match kind {
        "docker" => "Variables for the gateway's container (ST_GW_* override its file). The container is created again with them; its connection and secrets stay as they are.",
        "windows-service" => "Variables in the service's Environment (ST_GW_* override its file). The service restarts with them.",
        _ => "Variables in the Platform Manager's drop-in for the service (ST_GW_* override its file); those in the unit file are shown but not changed. The service restarts with them.",
    }
}

/// "Change password": twice, 8 characters or more.
struct PasswordDialog {
    a: Entity<InputState>,
    b: Entity<InputState>,
    server: Option<String>,
    problem: Option<String>,
    run: RunFn,
    _subs: Vec<Subscription>,
}

impl PasswordDialog {
    fn new(server: Option<String>, run: RunFn, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let a = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("8 characters or more"));
        let b = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Again"));
        let subs = vec![cx.subscribe(&a, |_, _, _: &InputEvent, cx| cx.notify()), cx.subscribe(&b, |_, _, _: &InputEvent, cx| cx.notify())];
        Self { a, b, server, problem: None, run, _subs: subs }
    }
}

impl Render for PasswordDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let field = |label: &'static str, input: &Entity<InputState>| v_flex().gap_1p5().child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(label)).child(Input::new(input).mask_toggle());
        let place = self.server.as_ref().map(|s| format!(" It goes to the gateway on {s} over the server's agent, on its standard input.")).unwrap_or_default();
        v_flex()
            .gap_4()
            .child(div().text_sm().text_color(theme.muted_foreground).child(format!("The gateway's own command sets it (--ui-password); every browser signed in to the page is signed out.{place}")))
            .child(field("New password", &self.a))
            .child(field("Again", &self.b))
            .children(self.problem.clone().map(|p| ui::outcome(&theme, Tone::Bad, p)))
            .child(
                h_flex()
                    .gap(crate::ui::button_gap())
                    .justify_end()
                    .child(crate::ui::probed(crate::ui::button("gw-pw-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx))))
                    .child(crate::ui::probed(crate::ui::button("gw-pw-ok").primary().label("Change password").on_click(cx.listener(|d, _, window, cx| {
                        let (a, b) = (d.a.read(cx).value().to_string(), d.b.read(cx).value().to_string());
                        if a.chars().count() < 8 {
                            d.problem = Some("The password needs 8 characters or more.".into());
                        } else if a != b {
                            d.problem = Some("The two passwords differ.".into());
                        } else if a.contains('\n') {
                            d.problem = Some("The password cannot hold a line break.".into());
                        } else {
                            crate::modal::close(cx);
                            (d.run)(Work::Password(a), window, cx);
                            return;
                        }
                        cx.notify();
                    })))),
            )
    }
}

/// Screens: canned settings by name (fictional names only).
#[cfg(debug_assertions)]
pub fn debug_settings(name: &str) -> Option<GwSettings> {
    let file = r#"{"sentient":{"host":"sentient.example.local","port":1883,"remoteShell":false,
        "security":{"type":"accessToken","accessToken":"Ex4mpleT0kenAbcd3f9a"},
        "statistics":{"enable":true,"statsSendPeriodInSeconds":3600,"enableCustom":false},
        "rateLimits":"DEFAULT_TELEMETRY_RATE_LIMIT","maxPayloadSizeBytes":8196},
        "storage":{"type":"sqlite","data_file_path":"/var/lib/sentient-gateway/data/","messages_ttl_in_days":7,"size_limit":1.0,"max_db_amount":10},
        "localUi":{"enabled":true,"listen":"127.0.0.1:8642"},
        "connectors":[{"name":"line-1-modbus","type":"modbus"},{"name":"line-2-s7","type":"s7commplus"},{"name":"kiln-opcua","type":"opcua"},{"name":"dosing-rtu","type":"modbus","enabled":false}]}"#;
    let logs = r#"{"loggers":{"service":{"level":"INFO"},"connector":{"level":"WARNING"},"tb_connection":{"level":"INFO"}},"remoteLoggingLevel":"ERROR"}"#;
    let mut s = gs::from_files(file, Some(logs));
    s.ui.password_set = Some(true);
    s.caps = gs::Caps { status_json: false, ui_password: true, ui_listen: true, ui_hostname: false };
    s.log_files = Some("/var/lib/sentient-gateway/logs".into());
    s.machine.restart_always = Some(true);
    s.machine.restart_word = Some("always".into());
    match name {
        "native" | "native-connected" | "native-hand" => {
            s.config_path = Some("/etc/sentient-gateway/st_gateway.json".into());
            s.connected = Some(true);
            s.connected_how = Some("the gateway's open connections".into());
            s.machine.kind = "service".into();
            s.machine.serial = gs::Serial { mode: "group".into(), group: Some("dialout".into()), account: Some("sentient-gateway".into()), in_group: true, chosen: vec![], available: vec!["/dev/serial/by-id/usb-Example_RS485_A1-if00-port0".into()] };
            s.machine.env = vec![
                gs::EnvVar { name: "ST_GW_STATE_INTERVAL".into(), value: Some("30".into()), editable: true, origin: "the unit file".into() },
                gs::EnvVar { name: "ST_GW_DATA_DIR".into(), value: Some("/var/lib/sentient-gateway".into()), editable: false, origin: "the unit file".into() },
            ];
        }
        "windows" => {
            s.config_path = Some(r"C:\ProgramData\Sentient Gateway\config\st_gateway.json".into());
            s.connected = Some(true);
            s.machine.kind = "windows-service".into();
            s.machine.restart_word = Some("restart after a failure".into());
            s.log_files = Some(r"C:\ProgramData\Sentient Gateway\data\logs".into());
        }
        "docker" | "docker-disconnected" | "docker-legacy" => {
            s.conn.host = Some("127.0.0.1".into());
            s.conn.from_env = true;
            s.connected = Some(false);
            s.connected_how = Some("the gateway's open connections".into());
            s.ui.enabled = false;
            s.ui.listen = "0.0.0.0:8642".into();
            s.ui.password_set = Some(false);
            s.machine.kind = "docker".into();
            s.machine.restart_word = Some("unless-stopped".into());
            s.machine.serial = gs::Serial { mode: "devices".into(), chosen: vec![], available: vec!["/dev/serial/by-id/usb-Example_RS485_A1-if00-port0".into(), "/dev/ttyACM0".into()], ..gs::Serial::default() };
            s.machine.env = vec![gs::EnvVar { name: "TB_GW_STORAGE_PASSWORD".into(), value: None, editable: true, origin: "container".into() }];
        }
        "compose" => {
            s.machine.kind = "docker".into();
            s.connected = Some(true);
            s.machine.not_editable = Some("This container comes from a compose file in /srv/example-site/gateway: change its settings in that file and run docker compose up -d.".into());
        }
        "unreadable" | "native-unreadable" => return Some(GwSettings { source: "none".into(), note: Some("Readable by an administrator only: Read its settings reads them once with your password.".into()), ..GwSettings::default() }),
        _ => return None,
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::{connection_locked, sentient_page, valid_name};
    use sentient_installer_core::gateway_settings::GwSettings;

    #[test]
    fn the_connection_is_sentients_unless_known_offline() {
        let mut s = GwSettings::default();
        assert!(connection_locked(&s));
        s.connected = Some(true);
        assert!(connection_locked(&s));
        s.connected = Some(false);
        assert!(!connection_locked(&s));
    }

    #[test]
    fn the_sentient_link_only_when_it_is_that_sentient() {
        assert_eq!(sentient_page(Some("http://127.0.0.1:8080/"), Some("127.0.0.1"), &[]).as_deref(), Some("http://127.0.0.1:8080/entities/gateways"));
        assert_eq!(sentient_page(Some("http://plant-srv:8080"), Some("plant-srv"), &["plant-srv"]).as_deref(), Some("http://plant-srv:8080/entities/gateways"));
        assert_eq!(sentient_page(Some("http://127.0.0.1:8080"), Some("10.0.4.12"), &[]), None);
        assert_eq!(sentient_page(None, Some("127.0.0.1"), &[]), None);
    }

    #[test]
    fn variable_names() {
        assert!(valid_name("ST_GW_X1") && valid_name("_A"));
        assert!(!valid_name("1A") && !valid_name("A-B") && !valid_name(""));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn fixtures_carry_no_token() {
        for n in ["native", "docker", "compose", "unreadable"] {
            let s = super::debug_settings(n).unwrap();
            assert!(!serde_json::to_string(&s).unwrap().contains("Ex4mpleT0ken"), "{n}");
        }
    }
}
