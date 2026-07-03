//! `sicheck` — run the SENTIENT installer preflight checks headlessly and print
//! them. Useful for testing the core without the GUI.

use sentient_installer_core::checks::{run_all, Status};

fn main() {
    println!("SENTIENT installer — preflight checks\n");
    for c in run_all() {
        let icon = match c.status {
            Status::Pass => "[ ok ]",
            Status::Setup => "[setup]",
            Status::Fail => "[FAIL]",
            Status::Unknown => "[ ?? ]",
        };
        println!("  {icon}  {:<22} {}", c.label, c.detail);
    }
}
