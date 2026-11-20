// Copyright © 2016-2026 The SENTIENT Authors

//! Run as administrator. Setup and the manager change services, Program Files
//! and the registry, so a run without elevation relaunches itself through the
//! UAC prompt and exits. (A manifest cannot ask for this: GPUI embeds its own
//! `asInvoker` manifest and two manifests do not link.)

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> i32;
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn is_admin() -> bool {
        // SAFETY: no arguments; reads the process token.
        unsafe { IsUserAnAdmin() != 0 }
    }

    /// True when this process may continue; false when it has handed over
    /// to an elevated copy (or the operator declined) and should exit.
    #[cfg(feature = "gui")]
    pub fn ensure_admin() -> bool {
        // SAFETY: no arguments; reads the process token.
        if unsafe { IsUserAnAdmin() } != 0 {
            return true;
        }
        let args: Vec<String> = std::env::args().skip(1).collect();
        let _ = relaunch_elevated(&args);
        false
    }

    /// Start this program again as administrator (the UAC prompt) with
    /// `args`. False when it did not start (the prompt was declined).
    /// From the Microsoft Store package the helper copy outside the
    /// package is started instead (crate::store).
    #[cfg(feature = "gui")]
    pub fn relaunch_elevated(args: &[String]) -> bool {
        if crate::store::flavour() == crate::store::Flavour::Packaged {
            crate::store::hand_over(args);
            return true;
        }
        let Ok(exe) = std::env::current_exe() else { return false };
        runas(&exe, args)
    }

    /// Start `exe` as administrator (the UAC prompt) with `args`, in its
    /// own folder. False when it did not start (the prompt was declined).
    pub fn runas(exe: &std::path::Path, args: &[String]) -> bool {
        let args: Vec<String> = args.iter().map(|a| if a.contains(' ') || a.is_empty() { format!("\"{a}\"") } else { a.clone() }).collect();
        let file = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(&args.join(" ")));
        let verb = wide(std::ffi::OsStr::new("runas"));
        let dir = exe.parent().map(|d| wide(d.as_os_str()));
        let dir_ptr = dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
        // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that lives
        // until the call returns (or null for no folder); SW_SHOWNORMAL = 1.
        // A result above 32 is success.
        unsafe { ShellExecuteW(0, verb.as_ptr(), file.as_ptr(), params.as_ptr(), dir_ptr, 1) > 32 }
    }
}

#[cfg(windows)]
pub use imp::{is_admin, runas};
// Only the window asks to be elevated; the server agent does not.
#[cfg(all(windows, feature = "gui"))]
pub use imp::{ensure_admin, relaunch_elevated};

/// Linux elevates per step (polkit): never a whole relaunch.
#[cfg(not(windows))]
#[allow(dead_code)]
pub fn relaunch_elevated(_args: &[String]) -> bool {
    false
}

#[cfg(not(windows))]
pub fn ensure_admin() -> bool {
    true
}

/// Headless jobs do their privileged steps through polkit on Linux, so only
/// Windows needs an elevated start.
#[cfg(not(windows))]
pub fn is_admin() -> bool {
    true
}
