// Copyright © 2016-2026 The SENTIENT Authors

//! `--open-sentient [--kiosk]`: what the sign-in entry runs on operator
//! screens (kiosk::set_open_at_signin). After a reboot SENTIENT's services
//! are usually still starting, and a browser opened at once shows an error
//! page that a kiosk cannot leave; so wait until SENTIENT answers, then open
//! it. Runs as the signed-in user: no window of its own, no elevation.

use std::time::{Duration, Instant};

use sentient_installer_core::kiosk;

use crate::installation::InstallationRecord;

/// How long to wait for SENTIENT before opening it anyway.
const WAIT: Duration = Duration::from_secs(15 * 60);

fn answers(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if s.write_all(b"GET /login HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return false;
    }
    let mut head = [0u8; 12];
    s.read_exact(&mut head).is_ok() && head.starts_with(b"HTTP/") && (head[9] == b'2' || head[9] == b'3')
}

pub fn run(args: &[String]) -> i32 {
    let kiosk_mode = args.iter().any(|a| a == "--kiosk");
    let port = InstallationRecord::load().map(|r| r.stack.http_port).unwrap_or(8080);
    let start = Instant::now();
    while !answers(port) && start.elapsed() < WAIT {
        std::thread::sleep(Duration::from_secs(3));
    }
    let url = format!("http://localhost:{port}");
    let spawned = match kiosk::detect() {
        Some((browser, exe)) => std::process::Command::new(exe).args(browser.launch_args(port, kiosk_mode)).spawn().is_ok(),
        None if cfg!(windows) => std::process::Command::new("cmd").args(["/c", "start", "", &url]).spawn().is_ok(),
        None => std::process::Command::new("xdg-open").arg(&url).spawn().is_ok(),
    };
    if spawned { 0 } else { 1 }
}
