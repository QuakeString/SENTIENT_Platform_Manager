//! `sicheck` — run the SENTIENT installer preflight checks headlessly and print
//! them. Useful for testing the core without the GUI.

//!
//!   sicheck [windows-docker|windows-native|linux-docker|linux-native] [--offline]

use sentient_installer_core::checks::{run_all, run_for, Status};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let offline = args.iter().any(|a| a == "--offline");
    let mode = args.iter().find(|a| !a.starts_with("--")).cloned();
    println!("SENTIENT installer — preflight checks{}\n", mode.as_deref().map(|m| format!(" ({m})")).unwrap_or_default());
    let list = match mode { Some(m) => run_for(&m, offline, None), None => run_all() };
    for c in list {
        let icon = match c.status {
            Status::Pass => "[ ok ]",
            Status::Setup => "[setup]",
            Status::Fail => "[FAIL]",
            Status::Unknown => "[ ?? ]",
        };
        println!("  {icon}  {:<22} {}", c.label, c.detail);
    }
}
