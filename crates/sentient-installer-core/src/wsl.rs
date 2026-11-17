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
        sys::output("wsl.exe", &["--status"])
            .map(|(ok, out, err)| {
                let text = format!("{}{}", sys::decode(&out), sys::decode(&err));
                ok && !is_usage_text(&text) && !kernel_missing(&text)
            })
            .unwrap_or(false)
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

/// The Windows features WSL 2 needs, turned on with DISM where `wsl.exe`
/// cannot do it itself (Windows 10 builds whose inbox `wsl.exe` knows no
/// `--no-distribution`).
pub const FEATURES: [&str; 2] = ["Microsoft-Windows-Subsystem-Linux", "VirtualMachinePlatform"];

/// Where the WSL package comes from on a Windows without the Microsoft
/// Store (LTSC, older builds): its GitHub releases, signed by Microsoft.
pub const PACKAGE_RELEASES: &str = "https://api.github.com/repos/microsoft/WSL/releases/latest";

/// Install / enable WSL2. Requires administrator privileges. Idempotent — safe
/// to re-run (after the restart it finishes what needs WSL itself).
///
/// `wsl --install --no-distribution` first. An inbox `wsl.exe` that does not
/// know that flag prints its usage and fails: then the two features are
/// turned on with DISM. A kernel that is still missing (`wsl --status`
/// says so) comes from `wsl --update`, and failing that from the WSL
/// package of Microsoft's releases.
pub fn install(sink: ProgressFn) -> WslOutcome {
    #[cfg(windows)]
    {
        sink(Progress::Log {
            line: "Windows is turning on WSL 2. These steps are Windows' own and can be quiet for a few minutes.".into(),
        });
        let mut restart = false;
        let known = run_logged(&sink, "Turning on WSL 2", "wsl.exe", &["--install", "--no-distribution"]);
        match known {
            Some((true, out)) => restart |= mentions_restart(&out),
            _ => {
                sink(Progress::Log { line: "This wsl.exe does not know --no-distribution: turning the Windows features on with DISM".into() });
                for feature in FEATURES {
                    sink(Progress::Step { name: format!("Turning on the Windows feature {feature}") });
                    let arg = format!("/featurename:{feature}");
                    match sys::output_tracked("dism.exe", &["/online", "/enable-feature", &arg, "/all", "/norestart"]) {
                        // 3010: turned on, a restart finishes it.
                        Some((ok, out, _)) => {
                            let text = sys::decode(&out);
                            if text.contains("3010") || text.to_lowercase().contains("restart") {
                                restart = true;
                            }
                            if !ok && !text.contains("3010") {
                                sink(Progress::Log { line: format!("DISM: {}", text.trim().lines().last().unwrap_or("")) });
                            }
                        }
                        None => sink(Progress::Log { line: "could not run dism.exe".into() }),
                    }
                }
                // DISM's exit code 3010 also reads as a failure above; the
                // feature state is the truth.
                restart |= features_pending();
            }
        }
        if !restart && !is_ready() {
            ensure_kernel(&sink);
        }
        let _ = sys::output("wsl.exe", &["--set-default-version", "2"]);
        let ready = is_ready();
        if ready {
            sink(Progress::Done { message: "WSL 2 is turned on.".into() });
        } else {
            // A kernel needs the features running: fetch it now so the run
            // after the restart has less to do.
            if restart {
                ensure_kernel(&sink);
            }
            sink(Progress::Log { line: "WSL 2 is not active yet: Windows must restart to finish turning it on.".into() });
        }
        WslOutcome { ready, reboot_required: !ready }
    }
    #[cfg(not(windows))]
    {
        sink(Progress::Error { message: "WSL setup is Windows-only.".into() });
        WslOutcome { ready: false, reboot_required: false }
    }
}

/// Run a program, log its output, and return (success, output).
#[cfg(windows)]
fn run_logged(sink: &ProgressFn, step_name: &str, program: &str, args: &[&str]) -> Option<(bool, String)> {
    sink(Progress::Step { name: step_name.into() });
    let (ok, out, err) = sys::output_tracked(program, args)?;
    let text = format!("{}\n{}", sys::decode(&out), sys::decode(&err));
    // An inbox wsl.exe answers an unknown flag with its whole usage text:
    // not worth the log.
    if !is_usage_text(&text) {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            sink(Progress::Log { line: line.into() });
        }
    }
    Some((ok && !is_usage_text(&text), text))
}

/// `wsl.exe`'s usage text, printed for a flag it does not know.
pub fn is_usage_text(text: &str) -> bool {
    text.contains("Usage: wsl.exe")
}

/// `wsl --install` says a restart finishes it.
pub fn mentions_restart(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("reboot") || t.contains("restart")
}

/// A WSL feature is turned on but waits for a restart.
#[cfg(windows)]
fn features_pending() -> bool {
    let script = format!(
        "@({}) | ForEach-Object {{ (Get-WindowsOptionalFeature -Online -FeatureName $_).State }}",
        FEATURES.iter().map(|f| format!("'{f}'")).collect::<Vec<_>>().join(",")
    );
    sys::output("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &script])
        .map(|(_, out, _)| sys::decode(&out).lines().any(|l| l.trim().eq_ignore_ascii_case("EnablePending")))
        .unwrap_or(false)
}

/// The WSL 2 kernel (and with it a WSL that runs systemd): `wsl --update`,
/// else Microsoft's WSL package from its releases, checked for Microsoft's
/// signature before it is installed.
#[cfg(windows)]
fn ensure_kernel(sink: &ProgressFn) {
    if kernel_present() {
        return;
    }
    let _ = run_logged(sink, "Updating the WSL kernel", "wsl.exe", &["--update"]);
    if kernel_present() {
        return;
    }
    sink(Progress::Step { name: "Downloading the WSL package from Microsoft".into() });
    if let Err(e) = install_package(sink) {
        sink(Progress::Log { line: format!("The WSL package was not installed: {e}") });
    }
}

/// `wsl --status` no longer says the kernel is missing, or `wsl --version`
/// answers (the WSL package carries its own kernel).
#[cfg(windows)]
fn kernel_present() -> bool {
    if sys::output("wsl.exe", &["--version"]).is_some_and(|(ok, out, _)| ok && !is_usage_text(&sys::decode(&out))) {
        return true;
    }
    match sys::output("wsl.exe", &["--status"]) {
        Some((ok, out, err)) => ok && !kernel_missing(&format!("{}{}", sys::decode(&out), sys::decode(&err))),
        None => false,
    }
}

/// `wsl --status` text saying the WSL 2 kernel is not installed.
pub fn kernel_missing(status: &str) -> bool {
    status.contains("kernel file is not found")
}

/// The x64 (or ARM64) MSI of a release listing (GitHub's JSON).
pub fn package_url(release_json: &str, arm: bool) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(release_json).ok()?;
    let want = if arm { ".arm64.msi" } else { ".x64.msi" };
    v.get("assets")?.as_array()?.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?;
        (name.starts_with("wsl.") && name.ends_with(want)).then(|| a.get("browser_download_url")?.as_str().map(String::from)).flatten()
    })
}

#[cfg(windows)]
fn install_package(sink: &ProgressFn) -> Result<(), String> {
    let listing = crate::net::agent().get(PACKAGE_RELEASES).set("User-Agent", "sentient-setup").call().map_err(|e| e.to_string())?;
    let text = listing.into_string().map_err(|e| e.to_string())?;
    let url = package_url(&text, cfg!(target_arch = "aarch64")).ok_or("no WSL package in Microsoft's latest release")?;
    let msi = std::env::temp_dir().join("sentient-wsl.msi");
    sink(Progress::Log { line: format!("Downloading {url}") });
    crate::distro::download(&url, &msi, sink)?;
    // Microsoft's signature, or nothing is installed.
    let check = format!(
        "$s = Get-AuthenticodeSignature -LiteralPath '{}'; if ($s.Status -eq 'Valid' -and $s.SignerCertificate.Subject -match 'O=Microsoft Corporation') {{ 'signed' }} else {{ $s.Status }}",
        msi.display()
    );
    let signed = sys::output("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &check])
        .map(|(_, out, _)| sys::decode(&out).trim().to_string())
        .unwrap_or_default();
    if signed != "signed" {
        let _ = std::fs::remove_file(&msi);
        return Err(format!("the downloaded package is not signed by Microsoft ({signed})"));
    }
    sink(Progress::Step { name: "Installing the WSL package".into() });
    let path = msi.display().to_string();
    let result = sys::output_tracked("msiexec.exe", &["/i", &path, "/quiet", "/norestart"]);
    let _ = std::fs::remove_file(&msi);
    match result {
        Some((true, _, _)) => Ok(()),
        // 3010: installed, a restart finishes it.
        Some((false, out, err)) => {
            let t = format!("{}{}", sys::decode(&out), sys::decode(&err));
            if t.contains("3010") { Ok(()) } else { Err(format!("msiexec failed {}", t.trim())) }
        }
        None => Err("could not run msiexec.exe".into()),
    }
}

/// What Setup knows about this computer's WSL 2, read before the engine
/// cards show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// `wsl --status` succeeds.
    pub ready: bool,
    /// What `wsl --status` printed.
    pub status: String,
    /// Win32_Processor.VirtualizationFirmwareEnabled.
    pub virt_firmware: Option<bool>,
    /// Win32_ComputerSystem.HypervisorPresent (Hyper-V running hides the
    /// firmware flag).
    pub hypervisor: Option<bool>,
}

/// Whether Docker in WSL 2 can be chosen.
#[derive(Clone, Debug, PartialEq)]
pub enum DockerChoice {
    /// WSL 2 runs here.
    Ready,
    /// WSL 2 is not turned on: Setup turns it on, Windows restarts once.
    TurnOnWsl,
    /// It cannot run here: `reason` for the card, `detail` for its (i).
    Unavailable { reason: String, detail: String },
}

/// The words in a WSL error that mean its virtual machine cannot start.
const NO_VM: [&str; 4] = ["0x80370102", "HCS_E_HYPERV_NOT_INSTALLED", "virtualization is not enabled", "enable the Virtual Machine Platform"];

/// The WSL error saying its virtual machine cannot start, if `text` has one.
pub fn vm_error(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| NO_VM.iter().any(|w| l.contains(w)))?;
    Some(line.to_string())
}

/// [`DockerChoice`] from what was read.
pub fn docker_choice(f: &Facts) -> DockerChoice {
    if let Some(error) = vm_error(&f.status) {
        return DockerChoice::Unavailable {
            reason: "WSL 2 cannot start its virtual machine on this computer.".into(),
            detail: format!("Windows says: {error} Turn on virtualization (Intel VT-x or AMD-V) in the firmware settings and the Virtual Machine Platform feature, then open Setup again."),
        };
    }
    if f.ready {
        return DockerChoice::Ready;
    }
    if f.virt_firmware == Some(false) && f.hypervisor != Some(true) {
        return DockerChoice::Unavailable {
            reason: "Virtualization is turned off in this computer's firmware.".into(),
            detail: "WSL 2 runs a small virtual machine. Turn on Intel VT-x or AMD-V (SVM) in the BIOS or UEFI settings, then open Setup again.".into(),
        };
    }
    DockerChoice::TurnOnWsl
}

/// Read [`Facts`] (Windows; elsewhere nothing is known).
pub fn facts() -> Facts {
    #[cfg(windows)]
    {
        let (ready, status) = match sys::output("wsl.exe", &["--status"]) {
            Some((ok, out, err)) => {
                let text = format!("{}{}", sys::decode(&out), sys::decode(&err));
                (ok && !is_usage_text(&text) && !kernel_missing(&text), text)
            }
            None => (false, String::new()),
        };
        let script = "(Get-CimInstance Win32_Processor | Select-Object -First 1).VirtualizationFirmwareEnabled; (Get-CimInstance Win32_ComputerSystem).HypervisorPresent";
        let lines: Vec<Option<bool>> = sys::output("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", script])
            .map(|(_, out, _)| sys::decode(&out).lines().map(|l| l.trim().parse::<bool>().ok().or_else(|| match l.trim() { "True" => Some(true), "False" => Some(false), _ => None })).collect())
            .unwrap_or_default();
        Facts { ready, status, virt_firmware: lines.first().copied().flatten(), hypervisor: lines.get(1).copied().flatten() }
    }
    #[cfg(not(windows))]
    Facts::default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_that_runs_is_ready() {
        assert_eq!(docker_choice(&Facts { ready: true, status: "Default Version: 2".into(), ..Default::default() }), DockerChoice::Ready);
    }

    #[test]
    fn no_wsl_with_virtualization_means_turning_it_on() {
        let f = Facts { ready: false, virt_firmware: Some(true), hypervisor: Some(true), ..Default::default() };
        assert_eq!(docker_choice(&f), DockerChoice::TurnOnWsl);
        // Unknown firmware state: offered, the restart tells.
        assert_eq!(docker_choice(&Facts::default()), DockerChoice::TurnOnWsl);
        // Hyper-V running hides the firmware flag.
        assert_eq!(docker_choice(&Facts { virt_firmware: Some(false), hypervisor: Some(true), ..Default::default() }), DockerChoice::TurnOnWsl);
    }

    #[test]
    fn virtualization_off_greys_the_card_with_the_reason() {
        let f = Facts { virt_firmware: Some(false), hypervisor: Some(false), ..Default::default() };
        match docker_choice(&f) {
            DockerChoice::Unavailable { reason, detail } => {
                assert!(reason.contains("Virtualization is turned off"));
                assert!(detail.contains("VT-x"));
                assert!(!reason.contains("..."));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_vm_that_cannot_start_is_unavailable_even_when_wsl_is_installed() {
        let status = "Default Version: 2\nWSL 2 requires an update.\nError: 0x80370102 The virtual machine could not be started because a required feature is not installed.";
        let f = Facts { ready: true, status: status.into(), virt_firmware: Some(true), hypervisor: Some(true) };
        match docker_choice(&f) {
            DockerChoice::Unavailable { detail, .. } => assert!(detail.contains("0x80370102")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_inbox_wsl_usage_and_a_missing_kernel_are_recognised() {
        assert!(is_usage_text("Copyright (c) Microsoft Corporation.\nUsage: wsl.exe [Argument]\nArguments:"));
        assert!(!is_usage_text("The requested operation is successful."));
        assert!(kernel_missing("Default Version: 2\nThe WSL 2 kernel file is not found. To update or restore the kernel please run 'wsl.exe --update'."));
        assert!(mentions_restart("The requested operation is successful. Changes will not be effective until the system is rebooted."));
    }

    #[test]
    fn the_package_is_the_msi_for_this_processor() {
        let json = r#"{"assets":[
            {"name":"Microsoft.WSL_3.0.5.0_x64_ARM64.msixbundle","browser_download_url":"https://example.test/bundle"},
            {"name":"wsl.3.0.5.0.arm64.msi","browser_download_url":"https://example.test/arm64.msi"},
            {"name":"wsl.3.0.5.0.x64.msi","browser_download_url":"https://example.test/x64.msi"}]}"#;
        assert_eq!(package_url(json, false).as_deref(), Some("https://example.test/x64.msi"));
        assert_eq!(package_url(json, true).as_deref(), Some("https://example.test/arm64.msi"));
        assert_eq!(package_url("{}", false), None);
    }
}
