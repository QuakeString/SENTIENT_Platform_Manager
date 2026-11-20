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
    // The WSL package needs a Windows that runs it; an older Windows 10
    // keeps its inbox WSL and gets Microsoft's kernel for it instead.
    let packaged = os_build().is_none_or(|(build, ubr)| packaged_supported(build, ubr));
    if packaged {
        sink(Progress::Step { name: "Downloading the WSL package from Microsoft".into() });
        if let Err(e) = install_package(sink) {
            sink(Progress::Log { line: format!("The WSL package was not installed: {e}") });
        }
        if kernel_present() {
            return;
        }
    } else if let Some((build, ubr)) = os_build() {
        sink(Progress::Log { line: format!("Windows 10.0.{build}.{ubr} cannot run the WSL package: its inbox WSL gets Microsoft's WSL 2 kernel") });
    }
    sink(Progress::Step { name: "Downloading the WSL 2 kernel from Microsoft".into() });
    if let Err(e) = install_signed_msi(sink, if cfg!(target_arch = "aarch64") { KERNEL_MSI_ARM64 } else { KERNEL_MSI_X64 }, "sentient-wsl-kernel.msi") {
        sink(Progress::Log { line: format!("The WSL 2 kernel was not installed: {e}") });
    }
}

/// The program of Microsoft's "Welcome to Windows Subsystem for Linux"
/// window, which the WSL package opens after it is installed.
pub const WELCOME_EXE: &str = "wslsettings.exe";

/// Of `processes` (process id, program name, start in seconds since 1970),
/// the welcome windows that opened at `since` or later: the ones the
/// install's own WSL step caused. One the user opened earlier stays.
pub fn welcome_windows(processes: &[(u32, String, u64)], since: u64) -> Vec<u32> {
    processes.iter().filter(|(_, name, start)| name.eq_ignore_ascii_case(WELCOME_EXE) && *start >= since).map(|(pid, _, _)| *pid).collect()
}

/// Close the welcome window the WSL package opened during an install that
/// began at `since` (seconds since 1970). Only that program, by its exact
/// name; nothing else is touched.
#[cfg(windows)]
pub fn close_welcome_window(sink: &ProgressFn, since: u64) {
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let list: Vec<(u32, String, u64)> = system.processes().iter().map(|(pid, p)| (pid.as_u32(), p.name().to_string_lossy().to_string(), p.start_time())).collect();
    let found = welcome_windows(&list, since);
    for pid in &found {
        if let Some(p) = system.process(sysinfo::Pid::from_u32(*pid)) {
            p.kill();
        }
    }
    if !found.is_empty() {
        sink(Progress::Log { line: "Closed Microsoft's WSL welcome window that the WSL install opened".into() });
    }
}

#[cfg(not(windows))]
pub fn close_welcome_window(_sink: &ProgressFn, _since: u64) {}

/// Microsoft's WSL 2 kernel for the inbox WSL (Windows 10 before the WSL
/// package runs there).
pub const KERNEL_MSI_X64: &str = "https://wslstorestorage.blob.core.windows.net/wslblob/wsl_update_x64.msi";
pub const KERNEL_MSI_ARM64: &str = "https://wslstorestorage.blob.core.windows.net/wslblob/wsl_update_arm64.msi";

/// Whether Windows `build`.`ubr` runs the WSL package: Windows 11, or
/// Windows 10 21H2/22H2 with the November 2022 update (KB5020030, .2311)
/// or later. Older builds answer it with WSL_E_OS_NOT_SUPPORTED.
pub fn packaged_supported(build: u32, ubr: u32) -> bool {
    build >= 22000 || ((build == 19044 || build == 19045) && ubr >= 2311)
}

/// This Windows' build and update revision.
#[cfg(windows)]
fn os_build() -> Option<(u32, u32)> {
    let script = "[Environment]::OSVersion.Version.Build; (Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion').UBR";
    let (_, out, _) = sys::output("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", script])?;
    let text = sys::decode(&out);
    let mut n = text.lines().filter_map(|l| l.trim().parse::<u32>().ok());
    Some((n.next()?, n.next()?))
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

/// The release page GitHub sends `…/releases/latest` to (not rate-limited
/// like its API, which refuses an address that asked too often).
pub const PACKAGE_LATEST: &str = "https://github.com/microsoft/WSL/releases/latest";

/// The tag in a release page's address (`…/releases/tag/3.0.5`).
pub fn tag_from_release_url(url: &str) -> Option<String> {
    let tag = url.split("/releases/tag/").nth(1)?.split(['?', '#', '/']).next()?.trim();
    (!tag.is_empty() && tag.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| tag.to_string())
}

/// The MSI of release `tag`, named as Microsoft names it (`wsl.3.0.5.0.x64.msi`).
pub fn package_url_for_tag(tag: &str, arm: bool) -> String {
    let version = if tag.split('.').count() >= 4 { tag.to_string() } else { format!("{tag}.0") };
    format!("https://github.com/microsoft/WSL/releases/download/{tag}/wsl.{version}.{}.msi", if arm { "arm64" } else { "x64" })
}

/// The newest WSL package's address: from the releases API, else from
/// where the latest release page lives.
#[cfg(windows)]
fn latest_package_url() -> Result<String, String> {
    let arm = cfg!(target_arch = "aarch64");
    let api = crate::net::agent()
        .get(PACKAGE_RELEASES)
        .set("User-Agent", "sentient-setup")
        .call()
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .ok()
        .and_then(|text| package_url(&text, arm));
    if let Some(url) = api {
        return Ok(url);
    }
    let page = crate::net::agent().get(PACKAGE_LATEST).set("User-Agent", "sentient-setup").call().map_err(|e| e.to_string())?;
    let tag = tag_from_release_url(page.get_url()).ok_or_else(|| format!("no release tag in {}", page.get_url()))?;
    Ok(package_url_for_tag(&tag, arm))
}

#[cfg(windows)]
fn install_package(sink: &ProgressFn) -> Result<(), String> {
    let url = latest_package_url()?;
    install_signed_msi(sink, &url, "sentient-wsl.msi")
}

/// Download an MSI, check that Microsoft signed it, install it quietly.
#[cfg(windows)]
fn install_signed_msi(sink: &ProgressFn, url: &str, file: &str) -> Result<(), String> {
    let msi = std::env::temp_dir().join(file);
    sink(Progress::Log { line: format!("Downloading {url}") });
    crate::distro::download(url, &msi, sink)?;
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
        return Err(format!("the download is not signed by Microsoft ({signed})"));
    }
    sink(Progress::Step { name: format!("Installing {}", if file.contains("kernel") { "the WSL 2 kernel" } else { "the WSL package" }) });
    let path = msi.display().to_string();
    let result = sys::output_tracked_code("msiexec.exe", &["/i", &path, "/quiet", "/norestart"]);
    let _ = std::fs::remove_file(&msi);
    match result {
        Some((code, out, err)) => msiexec_result(code, &format!("{}{}", sys::decode(&out), sys::decode(&err))),
        None => Err("could not run msiexec.exe".into()),
    }
}

/// What an `msiexec /quiet` exit code means. It prints nothing itself, so
/// the code is the reason. 3010 and 1641: installed, a restart finishes it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn msiexec_result(code: Option<i32>, output: &str) -> Result<(), String> {
    let meaning = match code {
        Some(0 | 3010 | 1641) => return Ok(()),
        Some(1601) => "the Windows Installer service could not be reached",
        Some(1602) => "the installation was cancelled",
        Some(1603) => "a fatal error occurred during installation; this update may need Windows to restart first, or WSL to be turned on",
        Some(1618) => "another installation is in progress; try again when it has finished",
        Some(1619 | 1620) => "the installation package could not be opened",
        Some(1625) => "a system policy does not allow this installation",
        Some(1633) => "this installation package is not supported on this computer",
        Some(1638) => "another version of this product is already installed",
        Some(1639) => "the command line was not accepted",
        Some(_) => "",
        None => "it was stopped before it finished",
    };
    let mut m = match code {
        Some(c) if meaning.is_empty() => format!("msiexec ended with code {c}"),
        Some(c) => format!("msiexec ended with code {c}: {meaning}"),
        None => format!("msiexec failed: {meaning}"),
    };
    let output = output.trim();
    if !output.is_empty() {
        m.push_str(&format!(" ({output})"));
    }
    Err(m)
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

/// The `memory=` of a `.wslconfig`'s `[wsl2]` section, in MB: "8GB",
/// "4096MB", "6G"; a bare number is bytes, as WSL reads it. None when it
/// is not set (or cannot be read).
pub fn wslconfig_memory_mb(text: &str) -> Option<u64> {
    let mut in_wsl2 = false;
    let mut found = None;
    for line in text.lines() {
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_wsl2 = line.trim_matches(['[', ']']).trim().eq_ignore_ascii_case("wsl2");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if !in_wsl2 || !key.trim().eq_ignore_ascii_case("memory") {
            continue;
        }
        let v = value.trim().trim_matches('"').to_ascii_uppercase();
        let digits: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let n: f64 = digits.parse().ok()?;
        let unit = v[digits.len()..].trim().trim_end_matches('B');
        let mb = match unit {
            "T" => n * 1024.0 * 1024.0,
            "G" => n * 1024.0,
            "M" => n,
            "K" => n / 1024.0,
            "" => n / 1024.0 / 1024.0,
            _ => return None,
        };
        found = Some(mb as u64);
    }
    found.filter(|mb| *mb > 0)
}

/// The memory a WSL 2 VM gets on a computer with `host_mb`: `.wslconfig`'s
/// `memory=` when it is set (never more than the computer has), else half
/// the computer's memory, WSL 2's default.
pub fn vm_memory_mb(host_mb: u64, wslconfig: Option<&str>) -> u64 {
    match wslconfig.and_then(wslconfig_memory_mb) {
        Some(mb) => mb.min(host_mb),
        None => host_mb / 2,
    }
}

/// This account's `.wslconfig` (`%USERPROFILE%\.wslconfig`), the one its
/// WSL 2 VM reads; None when there is none.
pub fn account_wslconfig() -> Option<String> {
    let home = std::env::var_os("USERPROFILE")?;
    std::fs::read_to_string(std::path::Path::new(&home).join(".wslconfig")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_welcome_window_opened_during_the_install_is_closed() {
        let list = vec![
            (10, "wslsettings.exe".to_string(), 1_000),
            (11, "WslSettings.exe".to_string(), 2_000),
            (12, "wsl.exe".to_string(), 2_000),
            (13, "wslsettings.exe.bak".to_string(), 2_000),
            (14, "wslsettings.exe".to_string(), 1_500),
        ];
        assert_eq!(welcome_windows(&list, 1_500), vec![11, 14]);
        assert!(welcome_windows(&list, 3_000).is_empty());
    }

    #[test]
    fn msiexec_says_what_its_code_means() {
        assert_eq!(msiexec_result(Some(0), ""), Ok(()));
        assert_eq!(msiexec_result(Some(3010), ""), Ok(()));
        assert_eq!(msiexec_result(Some(1641), ""), Ok(()));
        let e = msiexec_result(Some(1603), "").unwrap_err();
        assert!(e.starts_with("msiexec ended with code 1603: a fatal error"), "{e}");
        assert_eq!(msiexec_result(Some(1618), "").unwrap_err(), "msiexec ended with code 1618: another installation is in progress; try again when it has finished");
        assert_eq!(msiexec_result(Some(42), "").unwrap_err(), "msiexec ended with code 42");
        assert_eq!(msiexec_result(Some(42), " oops \n").unwrap_err(), "msiexec ended with code 42 (oops)");
        assert!(msiexec_result(None, "").unwrap_err().contains("stopped before it finished"));
    }

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

    #[test]
    fn the_wsl_package_needs_windows_11_or_an_updated_windows_10() {
        assert!(!packaged_supported(19044, 1288)); // LTSC 2021 as released
        assert!(packaged_supported(19044, 2311));
        assert!(packaged_supported(19045, 5737));
        assert!(!packaged_supported(19041, 9999));
        assert!(packaged_supported(22631, 1));
    }

    #[test]
    fn without_the_api_the_package_comes_from_the_latest_release_page() {
        let tag = tag_from_release_url("https://github.com/microsoft/WSL/releases/tag/3.0.5").unwrap();
        assert_eq!(package_url_for_tag(&tag, false), "https://github.com/microsoft/WSL/releases/download/3.0.5/wsl.3.0.5.0.x64.msi");
        assert_eq!(package_url_for_tag("2.6.1", true), "https://github.com/microsoft/WSL/releases/download/2.6.1/wsl.2.6.1.0.arm64.msi");
        assert_eq!(tag_from_release_url("https://github.com/microsoft/WSL/releases"), None);
        assert_eq!(tag_from_release_url("https://github.com/microsoft/WSL/releases/tag/evil;rm"), None);
    }
}

#[cfg(test)]
mod vm_memory_tests {
    use super::{vm_memory_mb, wslconfig_memory_mb};

    #[test]
    fn wslconfig_memory_is_read_from_the_wsl2_section() {
        assert_eq!(wslconfig_memory_mb("[wsl2]\r\nmemory=8GB\r\n"), Some(8192));
        assert_eq!(wslconfig_memory_mb("[wsl2]\nmemory = 4096MB"), Some(4096));
        assert_eq!(wslconfig_memory_mb("[WSL2]\nMemory=6g # six"), Some(6144));
        assert_eq!(wslconfig_memory_mb("[wsl2]\nmemory=1.5GB"), Some(1536));
        assert_eq!(wslconfig_memory_mb("[wsl2]\nmemory=2147483648"), Some(2048));
        // Another section's, a commented one, none, nonsense.
        assert_eq!(wslconfig_memory_mb("[experimental]\nmemory=8GB"), None);
        assert_eq!(wslconfig_memory_mb("[wsl2]\n# memory=8GB\nguiApplications=false"), None);
        assert_eq!(wslconfig_memory_mb(""), None);
        assert_eq!(wslconfig_memory_mb("[wsl2]\nmemory=lots"), None);
    }

    #[test]
    fn the_vm_gets_half_unless_wslconfig_says_otherwise() {
        assert_eq!(vm_memory_mb(16384, None), 8192);
        assert_eq!(vm_memory_mb(16384, Some("[wsl2]\nguiApplications=false\n")), 8192);
        assert_eq!(vm_memory_mb(16384, Some("[wsl2]\nmemory=12GB")), 12288);
        // Never more than the computer has.
        assert_eq!(vm_memory_mb(8192, Some("[wsl2]\nmemory=64GB")), 8192);
    }
}
