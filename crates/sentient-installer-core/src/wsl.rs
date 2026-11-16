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
    match sys::output_tracked(program, args) {
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
        sink(Progress::Log {
            line: "Windows is provisioning WSL2. These steps are handled by Windows itself and can be quiet for a minute or two (features, kernel download). Please wait.".into(),
        });
        step(&sink, "Installing WSL2 (features + kernel)", "wsl.exe", &["--install", "--no-distribution"]);
        step(&sink, "Updating the WSL kernel", "wsl.exe", &["--update"]);
        step(&sink, "Setting the default version to 2", "wsl.exe", &["--set-default-version", "2"]);
        let ready = is_ready();
        if ready {
            sink(Progress::Done { message: "WSL2 is installed and ready.".into() });
        } else {
            sink(Progress::Log {
                line: "WSL isn't active yet. A restart is required to finish enabling it.".into(),
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

/// The WSL distributions this account has, without SENTIENT's own
/// (`sentient`, crate::distro::DISTRO): what keeps WSL from being removed.
pub fn other_distros() -> Vec<String> {
    #[cfg(windows)]
    {
        sys::output("wsl.exe", &["-l", "-q"])
            .filter(|(ok, _, _)| *ok)
            .map(|(_, out, _)| crate::removal::wsl_distros(&sys::decode(&out)))
            .unwrap_or_default()
            .into_iter()
            .filter(|d| !d.eq_ignore_ascii_case(crate::distro::DISTRO))
            .collect()
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// Turn WSL off: its package (`wsl --uninstall`) and the two Windows
/// features Setup's `wsl --install` turned on. Finished by a restart.
pub fn remove(sink: &ProgressFn) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = sys::output("wsl.exe", &["--shutdown"]);
        step(sink, "Removing WSL", "wsl.exe", &["--uninstall"]);
        let mut failed = Vec::new();
        for feature in ["Microsoft-Windows-Subsystem-Linux", "VirtualMachinePlatform"] {
            sink(Progress::Step { name: format!("Turning off the Windows feature {feature}") });
            let arg = format!("/featurename:{feature}");
            match sys::output_tracked("dism.exe", &["/online", "/disable-feature", &arg, "/norestart"]) {
                // 3010: done, a restart finishes it.
                Some((ok, out, _)) if ok || sys::decode(&out).contains("3010") => {}
                Some((_, out, err)) => failed.push(format!("{feature}: {}", sys::decode(&[out, err].concat()).trim().lines().last().unwrap_or("").to_string())),
                None => failed.push(format!("{feature}: dism.exe did not run")),
            }
        }
        if failed.is_empty() {
            sink(Progress::Log { line: "WSL is turned off. Windows finishes it at the next restart.".into() });
            Ok(())
        } else {
            Err(format!("WSL was not turned off: {}", failed.join("; ")))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = sink;
        Err("WSL exists on Windows only.".into())
    }
}
