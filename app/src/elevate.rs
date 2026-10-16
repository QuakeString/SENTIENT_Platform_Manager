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
    pub fn ensure_admin() -> bool {
        // SAFETY: no arguments; reads the process token.
        if unsafe { IsUserAnAdmin() } != 0 {
            return true;
        }
        let Ok(exe) = std::env::current_exe() else { return true };
        let args: Vec<String> = std::env::args()
            .skip(1)
            .map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a })
            .collect();
        let file = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(&args.join(" ")));
        let verb = wide(std::ffi::OsStr::new("runas"));
        // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that lives
        // until the call returns; SW_SHOWNORMAL = 1.
        unsafe {
            ShellExecuteW(0, verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), 1);
        }
        false
    }
}

#[cfg(windows)]
pub use imp::{ensure_admin, is_admin};

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
