// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT Platform Manager — installer, maintenance tool and management app
//! in one GPUI program. See docs/GPUI_PLATFORM_MANAGER_PLAN.md.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adopt;
#[cfg(all(windows, feature = "gui"))]
mod win_icon;
mod agent;
#[cfg(feature = "gui")]
mod assets;
mod backup_jobs;
mod backups;
mod catalog;
mod changes;
mod dbstatus;
#[cfg(feature = "gui")]
mod earlier;
mod elevate;
mod headless;
mod installation;
#[cfg(feature = "gui")]
mod installer;
/// The server agent runs unattended installs from a plan too (no window).
#[cfg(not(feature = "gui"))]
mod installer {
    #[path = "plan.rs"]
    pub mod plan;
    pub(crate) use plan::{run as run_install, *};
    #[cfg(windows)]
    pub use plan::provision_clickhouse_wsl;
}
#[cfg(feature = "gui")]
mod jobs;
mod log_level;
mod logsource;
mod kept;
mod leftovers;
#[cfg(feature = "gui")]
mod maintenance;
#[cfg(feature = "gui")]
mod manager;
#[cfg(feature = "gui")]
mod modal;
#[cfg(feature = "gui")]
mod chooser;
mod maintenance_ops;
mod migration;
mod mode;
mod notes;
mod online;
mod open_client;
mod remote;
mod replace;
mod restore_switch;
mod rootjob;
mod secrets;
mod secrets_key;
mod self_install;
mod sentient_admin;
mod settings;
#[cfg(feature = "gui")]
mod standalone;
mod support;
mod stats;
mod sysmon;
#[cfg(feature = "gui")]
mod theme;
#[cfg(feature = "gui")]
mod ui;
mod updates;
mod updating;

#[cfg(feature = "gui")]
use gpui_kit::component::ThemeMode;
#[cfg(feature = "gui")]
use gpui_kit::*;

#[cfg(feature = "gui")]
use mode::AppMode;

fn main() {
    // Agent mode runs as the hidden ClickHouse account: no window, no
    // elevation (that account is not an administrator).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--clickhouse-agent") {
        std::process::exit(agent::run(args.get(2).map(String::as_str).unwrap_or("")));
    }
    // The ClickHouse shutdown guard, started by Windows as a service.
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some("--clickhouse-guard") {
        let port = args.get(2).and_then(|p| p.parse().ok()).unwrap_or(8123);
        std::process::exit(sentient_installer_core::clickhouse_wsl::run_guard(port));
    }
    // The operator screen's sign-in entry: no window, no elevation.
    if args.get(1).map(String::as_str) == Some("--open-sentient") {
        std::process::exit(open_client::run(&args[2..]));
    }
    // ssh asks for a remote instance's passphrase or password (crate::remote):
    // it runs this program with only its prompt as the argument, and
    // crate::remote::ssh marks the call with SPM_ASKPASS_INSTANCE.
    if std::env::var_os("SPM_ASKPASS_INSTANCE").is_some() || args.get(1).map(String::as_str) == Some("--askpass") {
        std::process::exit(remote::askpass());
    }
    if args.get(1).map(String::as_str) == Some("--job") {
        std::process::exit(headless::run(&args[2..]));
    }
    #[cfg(not(feature = "gui"))]
    {
        eprintln!("This is the SENTIENT Platform Manager's server agent: run it with --job <job> (see --job help).");
        std::process::exit(2);
    }
    #[cfg(feature = "gui")]
    gui_main(args);
}

#[cfg(feature = "gui")]
fn gui_main(args: Vec<String>) {
    if !elevate::ensure_admin() {
        return;
    }
    // The downloaded Platform Manager replacing the installed one.
    if args.get(1).map(String::as_str) == Some("--self-update") {
        std::process::exit(updates::finish_self_update());
    }
    ui::prefer_native_frame();
    let mode = AppMode::detect();
    gpui_kit::application().with_assets(assets::AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        theme::init(ThemeMode::Light, cx);
        // Development aid: open in a given theme (screenshots of both).
        let forced = cfg!(debug_assertions).then(|| std::env::var("SPM_THEME").ok()).flatten();
        match forced.as_deref() {
            Some("dark") => theme::apply(theme::Choice::Dark, None, cx),
            Some("light") => theme::apply(theme::Choice::Light, None, cx),
            _ => {
                theme::apply(theme::Choice::saved(), None, cx);
                // "Follow system" switches while the window is open.
                theme::follow_system(cx);
            }
        }

        let size = match mode {
            AppMode::Install => size(px(980.), px(680.)),
            AppMode::Maintenance => size(px(820.), px(600.)),
            AppMode::Manager => size(px(1100.), px(720.)),
        };
        // Development aid: open at a given size (layout renders).
        #[cfg(debug_assertions)]
        let size = std::env::var("SPM_WINDOW_SIZE")
            .ok()
            .and_then(|v| v.split_once('x').and_then(|(w, h)| Some(gpui_kit::size(px(w.parse().ok()?), px(h.parse().ok()?)))))
            .unwrap_or(size);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size, cx)),
            // Small enough for a half screen, large enough that nothing breaks.
            window_min_size: Some(gpui_kit::size(px(560.), px(480.))),
            titlebar: Some(TitlebarOptions {
                title: Some(mode.window_title().into()),
                ..Default::default()
            }),
            // Matches the application-menu entry, so the desktop shows its icon.
            app_id: Some("sentient-platform-manager".into()),
            ..Default::default()
        };
        let opened = match mode {
            AppMode::Install => gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| installer::Installer::new(window, cx))
            })
            .map(|_| ()),
            AppMode::Maintenance => gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| maintenance::Maintenance::new(window, cx))
            })
            .map(|_| ()),
            AppMode::Manager => gpui_kit::open_window(options, cx, |window, cx| {
                #[cfg(windows)]
                win_icon::use_manager_icon(window);
                cx.new(|cx| manager::Manager::new(window, cx))
            })
            .map(|_| ()),
        };
        opened.expect("open the main window");
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
