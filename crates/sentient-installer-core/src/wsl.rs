//! WSL2 provisioning (Windows). Enables the WSL + VirtualMachinePlatform
//! features, installs the kernel, and sets the default version to 2. If the
//! features were newly enabled the system must reboot before WSL works — this is
//! reported so the installer can drive the reboot-and-resume flow.

use crate::progress::{Progress, ProgressFn};
#[cfg(windows)]
use crate::sys;

pub struct WslOutcome {
    pub ready: bool,
    pub reboot_required: bool,
}

/// Is WSL installed and functional right now?
pub fn is_ready() -> bool {
    #[cfg(windows)]
    {
        sys::output("wsl.exe", &["--status"]).map(|(ok, _, _)| ok).unwrap_or(false)
    }
    #[cfg(not(windows))]
    false
}

#[cfg(windows)]
fn step(sink: &ProgressFn, name: &str, program: &str, args: &[&str]) {
    sink(Progress::Step { name: name.into() });
    match sys::output(program, args) {
        Some((_, out, err)) => {
            for bytes in [out, err] {
                for line in sys::decode(&bytes).lines() {
                    let l = line.trim();
                    if !l.is_empty() {
                        sink(Progress::Log { line: l.into() });
                    }
                }
            }
        }
        None => sink(Progress::Log { line: format!("could not run {program}") }),
    }
}

/// Install / enable WSL2. Requires administrator privileges. Idempotent — safe
/// to re-run (e.g. after a reboot to finish the kernel update).
pub fn install(sink: ProgressFn) -> WslOutcome {
    #[cfg(windows)]
    {
        step(&sink, "Installing WSL2 (features + kernel)", "wsl.exe", &["--install", "--no-distribution"]);
        step(&sink, "Updating the WSL kernel", "wsl.exe", &["--update"]);
        step(&sink, "Setting the default version to 2", "wsl.exe", &["--set-default-version", "2"]);
        let ready = is_ready();
        if ready {
            sink(Progress::Done { message: "WSL2 is installed and ready.".into() });
        } else {
            sink(Progress::Log {
                line: "WSL isn't active yet — a restart is required to finish enabling it.".into(),
            });
        }
        WslOutcome { ready, reboot_required: !ready }
    }
    #[cfg(not(windows))]
    {
        sink(Progress::Error { message: "WSL setup is Windows-only.".into() });
        WslOutcome { ready: false, reboot_required: false }
    }
}
