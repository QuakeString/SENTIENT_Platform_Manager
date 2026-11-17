// Copyright © 2016-2026 The SENTIENT Authors

//! The built-in terminal (the approved mockup's "Terminal: <name>"): a
//! shell on a server over the connection details SPM already has (its key
//! or kept password, its known_hosts: no extra sign-in), or on this
//! computer. Linux: the account's login shell (bash gets SPM's prompt for
//! the session); Windows: PowerShell. Nothing is written on the server.
//!
//! - [`prompt`]: what the session runs (first line, prompt, theme check).
//! - [`keys`]: keys to terminal bytes.
//! - [`session`]: the pseudo terminal, the program and the screen grid.
//! - [`view`]: the dialog.

pub mod keys;
pub mod prompt;
pub mod session;
pub mod view;

use std::ffi::OsString;

use gpui_kit::{App, Window};

use crate::remote::Instance;
use session::Spawn;

/// Register MesloLGS NF with the text system (once).
pub fn load_fonts(cx: &mut App) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    let fonts = crate::assets::TERMINAL_FONTS.iter().map(|b| std::borrow::Cow::Borrowed(*b)).collect();
    // VERIFY: TextSystem::add_fonts takes Vec<Cow<'static, [u8]>> (gpui 0.3.9: yes).
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("terminal font: {e}");
    }
}

/// What a session on `inst` runs: ssh with a terminal, then the shell for
/// its system.
pub fn remote_target(inst: &Instance) -> view::Target {
    let windows = crate::remote::is_windows(inst);
    let system = inst.detected.as_ref().map(|d| d.os_name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| if windows { "Windows".into() } else { "Linux".into() });
    let remote = if windows { prompt::windows_remote_command(&inst.host, &system) } else { prompt::linux_remote_command(&inst.host, &system) };
    let (argv, env) = crate::remote::terminal_command(inst, &remote);
    view::Target { spawn: Spawn { argv, env, cwd: None }, shell: windows.then(|| "PowerShell".to_string()) }
}

/// A shell on this computer: the user's shell (Linux), PowerShell
/// (Windows), in the home folder.
pub fn local_target() -> view::Target {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "this computer".into());
    let system = sysinfo::System::long_os_version().or_else(sysinfo::System::name).unwrap_or_default();
    let mut env: Vec<(String, OsString)> = vec![("TERM".into(), "xterm-256color".into()), ("COLORTERM".into(), "truecolor".into())];
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from);
    if cfg!(windows) {
        let argv = prompt::powershell_argv("powershell.exe", &host, &system);
        return view::Target { spawn: Spawn { argv, env, cwd: home }, shell: Some("PowerShell".into()) };
    }
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let name = shell.rsplit('/').next().unwrap_or("sh").to_string();
    env.push(("SHELL".into(), shell.into()));
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), prompt::linux_script(&host, &system)];
    view::Target { spawn: Spawn { argv, env, cwd: home }, shell: Some(name) }
}

/// Open the Terminal dialog on `inst` (None: this computer).
pub fn open(inst: Option<&Instance>, window: &mut Window, cx: &mut App) {
    let (name, target) = match inst {
        Some(i) => (i.name.clone(), remote_target(i)),
        None => ("this computer".to_string(), local_target()),
    };
    view::open(&name, target, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(auth: crate::remote::Auth) -> Instance {
        let key = auth == crate::remote::Auth::Key;
        Instance {
            id: "srv-test-1".into(),
            name: "Plant A".into(),
            color: crate::remote::PALETTE[0].into(),
            os: crate::remote::Os::Linux,
            host: "203.0.113.20".into(),
            port: 2222,
            user: "ops".into(),
            auth,
            key_file: key.then(|| "/home/ops/.ssh/id_ed25519".into()),
            key_has_passphrase: false,
            host_key: None,
            detected: None,
        }
    }

    #[test]
    fn the_ssh_command_asks_for_a_terminal() {
        for auth in [crate::remote::Auth::Key, crate::remote::Auth::Password] {
            let t = remote_target(&inst(auth));
            let argv = &t.spawn.argv;
            assert_eq!(argv[0], "ssh");
            assert_eq!(argv[1], "-tt", "{argv:?}");
            assert!(argv.windows(2).any(|w| w == ["-p", "2222"]), "{argv:?}");
            assert!(argv.iter().any(|a| a == "StrictHostKeyChecking=yes"), "{argv:?}");
            assert!(argv.iter().any(|a| a.starts_with("UserKnownHostsFile=")), "{argv:?}");
            assert_eq!(argv[argv.len() - 2], "ops@203.0.113.20");
            assert!(argv.last().unwrap().starts_with("exec /bin/sh -c "), "{argv:?}");
            if auth == crate::remote::Auth::Key {
                assert!(argv.windows(2).any(|w| w == ["-i", "/home/ops/.ssh/id_ed25519"]), "{argv:?}");
            }
            assert!(t.spawn.env.iter().any(|(k, v)| k == "TERM" && v == "xterm-256color"));
        }
    }

    #[test]
    fn a_windows_server_gets_powershell() {
        let mut i = inst(crate::remote::Auth::Key);
        i.os = crate::remote::Os::Windows;
        let t = remote_target(&i);
        let last = t.spawn.argv.last().unwrap();
        assert!(last.starts_with("powershell -NoLogo -NoExit -ExecutionPolicy Bypass -EncodedCommand "), "{last}");
        assert_eq!(t.shell.as_deref(), Some("PowerShell"));
    }
}
