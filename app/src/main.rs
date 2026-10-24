// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT Platform Manager — installer, maintenance tool and management app
//! in one GPUI program. See docs/GPUI_PLATFORM_MANAGER_PLAN.md.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adopt;
mod agent;
mod assets;
mod backup_jobs;
mod catalog;
mod elevate;
mod headless;
mod installation;
mod installer;
mod jobs;
mod kept;
mod maintenance;
mod manager;
mod migration;
mod mode;
mod online;
mod open_client;
mod restore_switch;
mod rootjob;
mod secrets;
mod secrets_key;
mod self_install;
mod settings;
mod standalone;
mod theme;
mod ui;
mod updates;
mod updating;

use gpui_kit::component::ThemeMode;
use gpui_kit::*;

use mode::AppMode;

fn main() {
    // Agent mode runs as the hidden ClickHouse account: no window, no
    // elevation (that account is not an administrator).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--clickhouse-agent") {
        std::process::exit(agent::run(args.get(2).map(String::as_str).unwrap_or("")));
    }
    // The operator screen's sign-in entry: no window, no elevation.
    if args.get(1).map(String::as_str) == Some("--open-sentient") {
        std::process::exit(open_client::run(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some("--job") {
        std::process::exit(headless::run(&args[2..]));
    }
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

        let size = match mode {
            AppMode::Install => size(px(980.), px(680.)),
            AppMode::Maintenance => size(px(820.), px(600.)),
            AppMode::Manager => size(px(1100.), px(720.)),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size, cx)),
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
