//! Read-only preflight checks. Nothing here changes the system — it only reports
//! whether each prerequisite for installing SENTIENT (WSL2 + Docker Engine) is
//! satisfied, will be set up by the installer, or is a blocker the user must fix.

use serde::Serialize;

#[cfg(windows)]
use crate::sys::{decode, output};

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Satisfied.
    Pass,
    /// Not satisfied, but the installer will set it up (WSL, distro, Docker…).
    Setup,
    /// A blocker the user must resolve externally (BIOS, Windows version…).
    Fail,
    /// Couldn't determine (e.g. running the dev build off-Windows).
    Unknown,
}

#[derive(Serialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub status: Status,
    pub detail: String,
}

fn check(id: &str, label: &str, status: Status, detail: impl Into<String>) -> Check {
    Check { id: id.into(), label: label.into(), status, detail: detail.into() }
}

/// Run every preflight check, in display order.
pub fn run_all() -> Vec<Check> {
    vec![
        windows_version(),
        administrator(),
        virtualization(),
        wsl_installed(),
        distro_present(),
        docker_ready(),
        disk_space(),
        internet(),
    ]
}

// ---- command helper ----------------------------------------------------------

#[cfg(windows)]
fn ps(script: &str) -> String {
    output("powershell", &["-NoProfile", "-NonInteractive", "-Command", script])
        .map(|(_, out, _)| decode(&out).trim().to_string())
        .unwrap_or_default()
}

// ---- Windows checks ----------------------------------------------------------

fn windows_version() -> Check {
    #[cfg(windows)]
    {
        let build: u32 = ps("[System.Environment]::OSVersion.Version.Build").parse().unwrap_or(0);
        if build == 0 {
            check("windows", "Windows version", Status::Unknown, "Could not read the Windows build.")
        } else if build >= 19041 {
            check("windows", "Windows version", Status::Pass, format!("Build {build} supports WSL2."))
        } else {
            check("windows", "Windows version", Status::Fail,
                  format!("Build {build}; WSL2 needs Windows 10 2004 (19041) or newer."))
        }
    }
    #[cfg(not(windows))]
    check("windows", "Windows version", Status::Unknown, "This installer targets Windows.")
}

fn administrator() -> Check {
    #[cfg(windows)]
    {
        let admin = ps("([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)");
        if admin.eq_ignore_ascii_case("true") {
            check("admin", "Administrator rights", Status::Pass, "Running elevated.")
        } else {
            check("admin", "Administrator rights", Status::Setup,
                  "Not elevated — the installer will request administrator rights when needed.")
        }
    }
    #[cfg(not(windows))]
    check("admin", "Administrator rights", Status::Unknown, "Windows only.")
}

fn virtualization() -> Check {
    #[cfg(windows)]
    {
        let v = ps("$hv=(Get-CimInstance Win32_ComputerSystem).HypervisorPresent; $vf=(Get-CimInstance Win32_Processor | Select-Object -First 1).VirtualizationFirmwareEnabled; if($hv -or $vf){'enabled'}else{'disabled'}");
        match v.as_str() {
            "enabled" => check("virt", "CPU virtualization", Status::Pass, "Enabled in firmware."),
            "disabled" => check("virt", "CPU virtualization", Status::Fail,
                "Disabled — enable virtualization (VT-x / AMD-V, SVM) in your BIOS/UEFI. Required for WSL2."),
            _ => check("virt", "CPU virtualization", Status::Unknown, "Could not determine virtualization state."),
        }
    }
    #[cfg(not(windows))]
    check("virt", "CPU virtualization", Status::Unknown, "Windows only.")
}

fn wsl_installed() -> Check {
    #[cfg(windows)]
    {
        match output("wsl.exe", &["--status"]) {
            Some((true, out, _)) => {
                let s = decode(&out);
                let v2 = s.contains('2');
                check("wsl", "WSL2", Status::Pass,
                      if v2 { "Installed.".into() } else { "Installed (will ensure default version 2).".to_string() })
            }
            _ => check("wsl", "WSL2", Status::Setup, "Not installed — the installer will install and update WSL2."),
        }
    }
    #[cfg(not(windows))]
    check("wsl", "WSL2", Status::Unknown, "Windows only.")
}

fn distro_present() -> Check {
    #[cfg(windows)]
    {
        let present = output("wsl.exe", &["-l", "-q"])
            .map(|(_, out, _)| decode(&out).lines().any(|l| l.trim().eq_ignore_ascii_case("sentient")))
            .unwrap_or(false);
        if present {
            check("distro", "SENTIENT WSL distro", Status::Pass, "The 'sentient' distro exists.")
        } else {
            check("distro", "SENTIENT WSL distro", Status::Setup, "Not present — the installer will create it.")
        }
    }
    #[cfg(not(windows))]
    check("distro", "SENTIENT WSL distro", Status::Unknown, "Windows only.")
}

fn docker_ready() -> Check {
    #[cfg(windows)]
    {
        match output("wsl.exe", &["-d", "sentient", "--", "docker", "version"]) {
            Some((true, _, _)) => check("docker", "Docker Engine", Status::Pass, "Docker is running in the distro."),
            _ => check("docker", "Docker Engine", Status::Setup, "Not set up yet — the installer will install Docker Engine."),
        }
    }
    #[cfg(not(windows))]
    check("docker", "Docker Engine", Status::Unknown, "Windows only.")
}

// ---- cross-platform checks ---------------------------------------------------

fn disk_space() -> Check {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let want_win = cfg!(windows);
    let avail = disks
        .list()
        .iter()
        .filter(|d| {
            let m = d.mount_point().to_string_lossy().to_string();
            if want_win { m.starts_with("C:") } else { m == "/" }
        })
        .map(|d| d.available_space())
        .max()
        .or_else(|| disks.list().iter().map(|d| d.available_space()).max());
    match avail {
        Some(bytes) => {
            let gb = bytes as f64 / 1e9;
            if gb >= 10.0 {
                check("disk", "Disk space", Status::Pass, format!("{gb:.0} GB free."))
            } else {
                check("disk", "Disk space", Status::Fail,
                      format!("{gb:.1} GB free — 10 GB+ recommended for the images and data."))
            }
        }
        None => check("disk", "Disk space", Status::Unknown, "Could not read free disk space."),
    }
}

fn internet() -> Check {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let ok = "registry-1.docker.io:443"
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
        .unwrap_or(false);
    if ok {
        check("internet", "Internet access", Status::Pass, "Docker registry is reachable.")
    } else {
        check("internet", "Internet access", Status::Fail, "Can't reach the Docker registry (registry-1.docker.io:443).")
    }
}
