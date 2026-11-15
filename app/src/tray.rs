// Copyright © 2016-2026 The SENTIENT Authors

//! The tray icon (crate::background): left click opens or closes the
//! window; its menu has the latest notifications, the instances, this
//! computer's Start / Stop / Restart, Back up now, Check for updates and Exit.
//!
//! One menu model ([`Model`] → [`items`]) drawn twice: on Linux as a
//! StatusNotifierItem over D-Bus (ksni, its own thread; KDE shows it, GNOME
//! with the AppIndicator extension), on Windows in the notification area
//! (tray-icon, on the window's thread, which pumps its messages). Clicks
//! become [`Command`]s on the channel the app reads.

use futures::channel::mpsc::UnboundedSender;

use crate::background::Command;

/// What the menu shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    /// The newest notifications: id, title, instance name, failed, read.
    pub notifications: Vec<(u64, String, String, bool, bool)>,
    pub unread: usize,
    /// The remote instances: id and name.
    pub instances: Vec<(String, String)>,
    /// This computer's SENTIENT: None when it is not installed here, else
    /// whether it is running.
    pub local: Option<bool>,
}

impl Model {
    /// Notifications and instances as saved; `local` from the caller.
    pub fn read(local: Option<bool>) -> Self {
        let events = crate::manager::saved_events();
        let unread = events.iter().filter(|e| !e.read).count();
        Self {
            notifications: events.iter().take(5).map(|e| (e.id, e.title.clone(), e.instance_name.clone(), e.failed, e.read)).collect(),
            unread,
            instances: crate::remote::load().into_iter().map(|i| (i.id, i.name)).collect(),
            local,
        }
    }

    pub fn tooltip(&self) -> String {
        match self.unread {
            0 => "SENTIENT Platform Manager".into(),
            1 => "SENTIENT Platform Manager · 1 new notification".into(),
            n => format!("SENTIENT Platform Manager · {n} new notifications"),
        }
    }
}

/// One menu entry, drawn by each platform.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// `error`: drawn with an error mark (a failed notification).
    Action { label: String, cmd: Command, enabled: bool, error: bool },
    Separator,
    Sub { label: String, items: Vec<Item> },
}

fn act(label: impl Into<String>, cmd: Command) -> Item {
    Item::Action { label: label.into(), cmd, enabled: true, error: false }
}

/// The menu for `m`.
pub fn items(m: &Model) -> Vec<Item> {
    let mut v = vec![act("Open SENTIENT Platform Manager", Command::Show), Item::Separator];
    // Notifications: the latest five, then the panel.
    let mut notes: Vec<Item> = m
        .notifications
        .iter()
        .map(|(id, title, inst, failed, _read)| Item::Action { label: format!("{inst}: {title}"), cmd: Command::Action(format!("notification:{id}")), enabled: true, error: *failed })
        .collect();
    if notes.is_empty() {
        notes.push(Item::Action { label: "No notifications".into(), cmd: Command::Show, enabled: false, error: false });
    }
    notes.push(Item::Separator);
    notes.push(act("Show all notifications", Command::Action("notifications".into())));
    v.push(Item::Sub { label: if m.unread > 0 { format!("Notifications ({})", m.unread) } else { "Notifications".into() }, items: notes });
    // Instances.
    let mut inst = vec![act("This computer", Command::Action("instance:".into()))];
    inst.extend(m.instances.iter().map(|(id, name)| act(name.clone(), Command::Action(format!("instance:{id}")))));
    v.push(Item::Sub { label: "Instances".into(), items: inst });
    // This computer's SENTIENT.
    if let Some(running) = m.local {
        v.push(Item::Sub {
            label: "SENTIENT on this computer".into(),
            items: vec![
                Item::Action { label: "Start".into(), cmd: Command::Action("control:start".into()), enabled: !running, error: false },
                Item::Action { label: "Stop".into(), cmd: Command::Action("control:stop".into()), enabled: running, error: false },
                Item::Action { label: "Restart".into(), cmd: Command::Action("control:restart".into()), enabled: running, error: false },
            ],
        });
        v.push(act("Back up this computer now", Command::Action("backup".into())));
    }
    v.push(act("Statistics", Command::Action("statistics".into())));
    v.push(act("Check for updates", Command::Action("check-updates".into())));
    v.push(Item::Separator);
    v.push(act("Exit", Command::Exit));
    v
}

/// The icon as RGBA (from the packaged PNG).
fn rgba(png_bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return None,
    };
    Some((rgba, info.width, info.height))
}

const ICON_32: &[u8] = include_bytes!("../assets/app-icon/32.png");
const ICON_64: &[u8] = include_bytes!("../assets/app-icon/64.png");
const ICON_22: &[u8] = include_bytes!("../assets/app-icon/22.png");

pub use imp::{spawn, Tray};

// ---------------------------------------------------------------------------
// Linux: StatusNotifierItem
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use ksni::blocking::TrayMethods as _;

    struct Sni {
        model: Model,
        tx: UnboundedSender<Command>,
        icons: Vec<ksni::Icon>,
    }

    fn argb(png: &[u8]) -> Option<ksni::Icon> {
        let (rgba, w, h) = rgba(png)?;
        let data = rgba.chunks(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
        Some(ksni::Icon { width: w as i32, height: h as i32, data })
    }

    fn to_ksni(items: Vec<Item>) -> Vec<ksni::MenuItem<Sni>> {
        items
            .into_iter()
            .map(|i| match i {
                Item::Separator => ksni::MenuItem::Separator,
                Item::Action { label, cmd, enabled, error } => ksni::menu::StandardItem {
                    label: label.replace('_', "__"),
                    enabled,
                    icon_name: if error { "dialog-error".into() } else { String::new() },
                    activate: Box::new(move |t: &mut Sni| {
                        let _ = t.tx.unbounded_send(cmd.clone());
                    }),
                    ..Default::default()
                }
                .into(),
                Item::Sub { label, items } => ksni::menu::SubMenu { label: label.replace('_', "__"), submenu: to_ksni(items), ..Default::default() }.into(),
            })
            .collect()
    }

    impl ksni::Tray for Sni {
        fn id(&self) -> String {
            "sentient-platform-manager".into()
        }
        fn title(&self) -> String {
            "SENTIENT Platform Manager".into()
        }
        fn icon_name(&self) -> String {
            // The installed icon when the theme has it; the pixmap otherwise.
            "sentient-platform-manager".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icons.clone()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: self.model.tooltip(), ..Default::default() }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.unbounded_send(Command::Toggle);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            to_ksni(items(&self.model))
        }
    }

    /// The tray while the app runs.
    pub struct Tray(ksni::blocking::Handle<Sni>);

    impl Tray {
        pub fn update(&self, model: Model) {
            self.0.update(|t| {
                if t.model != model {
                    t.model = model;
                }
            });
        }
    }

    /// Show the tray icon. None: no tray on this desktop (no
    /// StatusNotifierWatcher), then the window behaves as without a tray.
    pub fn spawn(tx: UnboundedSender<Command>, model: Model) -> Option<Tray> {
        let icons = [ICON_22, ICON_32, ICON_64].iter().filter_map(|p| argb(p)).collect();
        match (Sni { model, tx, icons }).spawn() {
            Ok(h) => Some(Tray(h)),
            Err(e) => {
                crate::maintenance_ops::log_line(&format!("No tray on this desktop: {e}"));
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Windows: the notification area
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tray_icon::menu::{IconMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// Menu item id → what it does (rebuilt with the menu).
    type Ids = Arc<Mutex<HashMap<String, Command>>>;

    pub struct Tray {
        icon: TrayIcon,
        ids: Ids,
        model: std::cell::RefCell<Model>,
    }

    enum Built {
        Item(MenuItem),
        Icon(IconMenuItem),
        Sep(PredefinedMenuItem),
        Sub(Submenu),
    }

    impl Built {
        fn as_item(&self) -> &dyn IsMenuItem {
            match self {
                Built::Item(i) => i,
                Built::Icon(i) => i,
                Built::Sep(s) => s,
                Built::Sub(s) => s,
            }
        }
    }

    fn build(items: Vec<Item>, ids: &mut HashMap<String, Command>, n: &mut usize) -> Vec<Built> {
        items
            .into_iter()
            .map(|i| match i {
                Item::Separator => Built::Sep(PredefinedMenuItem::separator()),
                Item::Action { label, cmd, enabled, error } => {
                    *n += 1;
                    let id = format!("spm-{n}");
                    ids.insert(id.clone(), cmd);
                    if error {
                        Built::Icon(IconMenuItem::with_id(id, label.replace('&', "&&"), enabled, red_dot(), None))
                    } else {
                        Built::Item(MenuItem::with_id(id, label.replace('&', "&&"), enabled, None))
                    }
                }
                Item::Sub { label, items } => {
                    let sub = Submenu::new(label.replace('&', "&&"), true);
                    for b in build(items, ids, n) {
                        let _ = sub.append(b.as_item());
                    }
                    Built::Sub(sub)
                }
            })
            .collect()
    }

    /// A failed notification's mark: a red dot, 16 × 16.
    fn red_dot() -> Option<tray_icon::menu::Icon> {
        let mut px = vec![0u8; 16 * 16 * 4];
        for y in 0..16 {
            for x in 0..16 {
                let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
                let a = (5.5 - (dx * dx + dy * dy).sqrt()).clamp(0., 1.);
                let i = (y * 16 + x) * 4;
                px[i..i + 4].copy_from_slice(&[0xd3, 0x2f, 0x2f, (a * 255.) as u8]);
            }
        }
        tray_icon::menu::Icon::from_rgba(px, 16, 16).ok()
    }

    fn menu(model: &Model, ids: &Ids) -> Menu {
        let mut map = HashMap::new();
        let mut n = 0;
        let built = build(items(model), &mut map, &mut n);
        let menu = Menu::new();
        for b in &built {
            let _ = menu.append(b.as_item());
        }
        *ids.lock().unwrap() = map;
        menu
    }

    impl Tray {
        pub fn update(&self, model: Model) {
            if *self.model.borrow() == model {
                return;
            }
            self.icon.set_menu(Some(Box::new(menu(&model, &self.ids))));
            let _ = self.icon.set_tooltip(Some(model.tooltip()));
            *self.model.borrow_mut() = model;
        }
    }

    /// Show the tray icon (on the window's thread).
    pub fn spawn(tx: UnboundedSender<Command>, model: Model) -> Option<Tray> {
        let ids: Ids = Arc::new(Mutex::new(HashMap::new()));
        let (rgba, w, h) = rgba(ICON_32)?;
        let icon = tray_icon::Icon::from_rgba(rgba, w, h).ok()?;
        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip(model.tooltip())
            .with_menu(Box::new(menu(&model, &ids)))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| crate::maintenance_ops::log_line(&format!("No tray: {e}")))
            .ok()?;
        let t = tx.clone();
        let map = ids.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some(cmd) = map.lock().unwrap().get(&e.id.0).cloned() {
                let _ = t.unbounded_send(cmd);
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = tx.unbounded_send(Command::Toggle);
            }
        }));
        Some(Tray { icon: tray, ids, model: std::cell::RefCell::new(model) })
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::*;

    pub struct Tray;

    impl Tray {
        pub fn update(&self, _model: Model) {}
    }

    pub fn spawn(_tx: UnboundedSender<Command>, _model: Model) -> Option<Tray> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Action { label, .. } | Item::Sub { label, .. } => label.clone(),
                Item::Separator => "—".into(),
            })
            .collect()
    }

    #[test]
    fn the_menu_follows_the_model() {
        let mut m = Model { instances: vec![("bsmpl".into(), "BSMPL".into())], ..Default::default() };
        let l = labels(&items(&m));
        assert_eq!(l.first().map(String::as_str), Some("Open SENTIENT Platform Manager"));
        assert_eq!(l.last().map(String::as_str), Some("Exit"));
        // Not installed here: no Start/Stop, no Back up.
        assert!(!l.iter().any(|x| x.contains("this computer")));
        m.local = Some(true);
        m.unread = 2;
        m.notifications = vec![(7, "Backing up failed".into(), "This computer".into(), true, false)];
        let it = items(&m);
        let l = labels(&it);
        assert!(l.contains(&"Notifications (2)".to_string()));
        assert!(l.contains(&"Back up this computer now".to_string()));
        let Some(Item::Sub { items: control, .. }) = it.iter().find(|i| matches!(i, Item::Sub { label, .. } if label == "SENTIENT on this computer")) else { panic!() };
        // Running: Start is off, Stop and Restart are on.
        assert!(matches!(&control[0], Item::Action { enabled: false, .. }));
        assert!(matches!(&control[1], Item::Action { enabled: true, .. }));
        let Some(Item::Sub { items: notes, .. }) = it.iter().find(|i| matches!(i, Item::Sub { label, .. } if label.starts_with("Notifications"))) else { panic!() };
        assert!(matches!(&notes[0], Item::Action { cmd: Command::Action(a), error: true, label, .. } if a == "notification:7" && label == "This computer: Backing up failed"));
    }

    #[test]
    fn the_icon_decodes() {
        let (px, w, h) = rgba(ICON_32).unwrap();
        assert_eq!((w, h, px.len()), (32, 32, 32 * 32 * 4));
    }
}
