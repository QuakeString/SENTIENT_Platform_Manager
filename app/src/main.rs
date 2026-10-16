// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT Platform Manager — installer, maintenance tool and management app
//! in one GPUI program. See docs/GPUI_PLATFORM_MANAGER_PLAN.md.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod catalog;
mod elevate;
mod installation;
mod installer;
mod jobs;
mod maintenance;
mod manager;
mod mode;
mod secrets;
mod self_install;
mod theme;
mod ui;

use gpui_kit::component::ThemeMode;
use gpui_kit::*;

use mode::AppMode;

fn main() {
    if !elevate::ensure_admin() {
        return;
    }
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
