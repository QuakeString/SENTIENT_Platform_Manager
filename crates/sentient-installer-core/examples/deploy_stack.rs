// Copyright © 2016-2026 The SENTIENT Authors

//! Deploy a Docker stack from a DeployConfig JSON file, without the wizard.
//! For testing the Docker engine: `cargo run --example deploy_stack -- stack.json`.

use std::sync::Arc;

use sentient_installer_core::distro::{self, DeployConfig};
use sentient_installer_core::progress::Progress;

fn main() {
    let path = std::env::args().nth(1).expect("usage: deploy_stack <config.json>");
    let text = std::fs::read_to_string(&path).expect("read the config");
    let cfg: DeployConfig = serde_json::from_str(&text).expect("parse the config");
    let sink = Arc::new(|p: Progress| match p {
        Progress::Step { name } => println!("== {name}"),
        Progress::Log { line } => println!("   {line}"),
        Progress::Done { message } => println!("✓ {message}"),
        Progress::Error { message } => eprintln!("✗ {message}"),
        Progress::Percent { .. } => {}
    });
    if let Err(e) = distro::deploy(sink, &cfg) {
        eprintln!("✗ {e}");
        std::process::exit(1);
    }
}
