// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT Platform Manager — installer, maintenance tool and management app
//! in one GPUI program. See docs/GPUI_PLATFORM_MANAGER_PLAN.md.

// The window build is a GUI program on Windows; the server agent (built
// without the "gui" feature) is a console program, so a job run over SSH
// is waited for and its output read (PowerShell neither waits for nor
// captures a GUI program: "gave no answer", his rc12-22 Windows server).
#![cfg_attr(all(not(debug_assertions), feature = "gui"), windows_subsystem = "windows")]

mod adopt;
// Always built: the menu entries list its actions (crate::self_install).
mod background;
#[cfg(feature = "gui")]
mod bg_app;
#[cfg(feature = "gui")]
mod tray;
#[cfg(all(windows, feature = "gui"))]
mod win_icon;
#[cfg(feature = "gui")]
mod win_frame;
mod agent;
#[cfg(feature = "gui")]
mod assets;
mod authenticode;
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
mod licence;
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
mod movedata;
#[cfg(windows)]
mod movedata_win;
mod mode;
mod notes;
mod online;
mod open_client;
mod remote;
mod replace;
mod restore_flow;
mod restore_switch;
mod rootjob;
mod secrets;
mod secrets_key;
mod self_install;
#[cfg(feature = "gui")]
mod sftp;
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
    // The root helper a job's one password prompt starts (pkexec <this>
    // --root-session): it runs the job's root steps sent over its pipe.
    if let Some(code) = sentient_installer_core::elevation::serve_if_requested(&args) {
        std::process::exit(code);
    }
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
fn window_options(mode: AppMode, cx: &mut gpui_kit::App) -> WindowOptions {
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
    WindowOptions {
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
    }
}

#[cfg(feature = "gui")]
fn window_options_manager(cx: &mut gpui_kit::App) -> WindowOptions {
    let mut o = window_options(AppMode::Manager, cx);
    // Where and how big it was left.
    // (SPM_WINDOW_SIZE, for screens and tests, wins.)
    if std::env::var_os("SPM_WINDOW_SIZE").is_some() {
    } else if let Some(b) = manager::session::window_bounds(cx, gpui_kit::size(gpui_kit::px(560.), gpui_kit::px(480.))) {
        o.window_bounds = Some(b);
    }
    // The translucent menu: the desktop shows blurred behind it (where the
    // desktop can; crate::manager::shell).
    if manager::shell::see_through() {
        o.window_background = gpui_kit::WindowBackgroundAppearance::Blurred;
    }
    o
}

/// The Platform Manager's window (crate::bg_app opens it, again after a close).
#[cfg(feature = "gui")]
pub(crate) fn open_manager_window(options: WindowOptions, cx: &mut gpui_kit::App) -> anyhow::Result<(gpui_kit::AnyWindowHandle, gpui_kit::Entity<manager::Manager>)> {
    let opened = win_frame::open_window(options, cx, |window, cx| {
        #[cfg(windows)]
        win_icon::use_manager_icon(window);
        cx.new(|cx| manager::Manager::new(window, cx))
    })?;
    // The toolkit's root paints the theme's background over the whole
    // window, which hid the translucent menu everywhere: the manager paints
    // its own (opaque unless translucent), so the root stays clear.
    let _ = opened.0.update(cx, |_, window, cx| {
        gpui_kit::base::Root::update(window, cx, |root, _, _| {
            use gpui_kit::Styled as _;
            root.style().background = Some(gpui_kit::transparent_black().into());
        });
    });
    Ok(opened)
}

#[cfg(feature = "gui")]
fn gui_main(args: Vec<String>) {
    let mode = AppMode::detect();
    // The Platform Manager runs once per user: a later start (a menu entry,
    // a taskbar action) hands its request to the running one, before asking
    // for administrator rights again.
    let request = background::request_of(&args);
    if mode == AppMode::Manager && !args.iter().any(|a| a == "--self-update") {
        if background::Prefs::load().show_tray && background::forward(&request) {
            return;
        }
        if request == "action exit" {
            return;
        }
    }
    let background_start = args.iter().any(|a| a == "--background");
    if mode == AppMode::Manager && background_start && !background::Prefs::load().background() {
        // Started at sign-in, but the tray was turned off since.
        return;
    }
    if !elevate::ensure_admin() {
        return;
    }
    // The downloaded Platform Manager replacing the installed one.
    if args.get(1).map(String::as_str) == Some("--self-update") {
        std::process::exit(updates::finish_self_update());
    }
    ui::prefer_native_frame();
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

        if mode == AppMode::Manager {
            bg_app::start(window_options_manager, !background_start, request.clone(), cx);
            return;
        }
        let options = window_options(mode, cx);
        let opened = match mode {
            AppMode::Install => win_frame::open_window(options, cx, |window, cx| {
                cx.new(|cx| installer::Installer::new(window, cx))
            })
            .map(|_| ()),
            _ => win_frame::open_window(options, cx, |window, cx| {
                cx.new(|cx| maintenance::Maintenance::new(window, cx))
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
