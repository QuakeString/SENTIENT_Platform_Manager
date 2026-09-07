//! `sinative` — drive the native Windows deployment engine headlessly.
//!
//! Companion to `sicheck`: the same "exercise the core without the GUI" idea,
//! but for [`native`] rather than the preflight checks. It is a test harness,
//! not a shipped tool — the wizard calls the same functions through Tauri.
//!
//! Usage:
//!   sinative stage    <root>   lay out a package tree from an existing build
//!   sinative setup    <root>   install PostgreSQL + TimescaleDB
//!   sinative deploy   <root>   create the DB, install the schema, run as a service
//!   sinative status   <root>
//!   sinative control  <root> <start|stop|restart>
//!   sinative logs     <root>
//!   sinative cleanup  <root>   remove EVERYTHING (destructive)
//!
//! `<root>` is a scratch directory; the harness derives the install dir, the
//! PostgreSQL dir and the state dir beneath it so a test never touches a real
//! installation.

use std::path::PathBuf;
use std::sync::Arc;

use sentient_installer_core::native::{self, ArtifactSource, NativeConfig};
use sentient_installer_core::progress::Progress;

/// Production defaults, matching `NativeOptions::default` in the Tauri layer so
/// the harness exercises exactly the layout the wizard will install.
fn cfg_default() -> NativeConfig {
    let mut c = cfg_for("C:\\_unused");
    c.install_dir = PathBuf::from(r"C:\Program Files\SENTIENT");
    c.pg_dir = PathBuf::from(r"C:\PostgreSQL\18");
    c.state_dir = PathBuf::from(r"C:\ProgramData\SENTIENT");
    c.http_port = 8080;
    c.mqtt_port = 1883;
    c.coap_port = 5683;
    c.pg_port = 5432;
    c
}

fn cfg_for(root: &str) -> NativeConfig {
    let root = PathBuf::from(root);
    NativeConfig {
        install_dir: root.join("app"),
        pg_dir: root.join("pg"),
        state_dir: root.join("state"),
        db_name: "sentient".into(),
        db_user: "sentient".into(),
        db_password: "sentient".into(),
        pg_superuser_password: "sentient".into(),
        // Deliberately off the defaults so a test run cannot collide with a
        // real install on the same machine.
        http_port: 8081,
        mqtt_port: 1884,
        coap_port: 5684,
        pg_port: 5433,
        load_demo: true,
        source: ArtifactSource::Remote { base_url: String::new() },
        jwt_secret: "sinative-harness-secret-key-not-for-production-0123".into(),
    }
}

fn sink() -> sentient_installer_core::progress::ProgressFn {
    Arc::new(|p: Progress| match p {
        Progress::Step { name } => println!("\n==> {name}"),
        Progress::Log { line } => println!("    {line}"),
        Progress::Percent { value } => println!("    {:.0}%", value * 100.0),
        Progress::Done { message } => println!("\n[done] {message}"),
        Progress::Error { message } => eprintln!("\n[error] {message}"),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: sinative <stage|setup|deploy|status|control|logs|cleanup> <root|-> [arg]");
        eprintln!("       '-' as <root> uses the production default layout");
        std::process::exit(2);
    }
    let (cmd, root) = (args[1].as_str(), args[2].as_str());
    // `-` means "the real default layout" — what the wizard installs.
    let cfg = if root == "-" { cfg_default() } else { cfg_for(root) };

    let result = match cmd {
        "stage" => stage(&cfg),
        "setup" => native::setup(sink(), &cfg),
        "deploy" => native::deploy(sink(), &cfg),
        "status" => {
            let s = native::status(&cfg);
            println!("installed: {}", s.installed);
            println!("running:   {}", s.running);
            for svc in s.services {
                println!("  {:<24} {:<10} {}", svc.name, svc.state, svc.status);
            }
            Ok(())
        }
        "control" => {
            let action = args.get(3).map(|s| s.as_str()).unwrap_or("restart");
            native::control(sink(), action)
        }
        "logs" => {
            println!("{}", native::logs(&cfg, 40));
            Ok(())
        }
        "cleanup" => native::cleanup(sink(), &cfg),
        other => Err(format!("unknown command: {other}")),
    };

    match result {
        Ok(()) => println!("\nOK"),
        Err(e) => {
            eprintln!("\nFAILED: {e}");
            std::process::exit(1);
        }
    }
}

/// Lay out the package tree the real installer bundle would ship, from
/// artifacts staged by the caller under `<root>/payload`.
///
/// Mirrors what IPM's packaging step must produce, so `deploy` is exercised
/// against the same layout it will see in production.
fn stage(cfg: &NativeConfig) -> Result<(), String> {
    let payload = cfg.state_dir.parent().unwrap().join("payload");
    if !payload.exists() {
        return Err(format!(
            "expected pre-staged artifacts in {} (bin/, ui/, data/, services/)",
            payload.display()
        ));
    }
    for item in ["bin", "ui", "data", "services"] {
        let from = payload.join(item);
        if !from.exists() {
            return Err(format!("missing {} in the payload", item));
        }
        let to = cfg.install_dir.join(item);
        copy_tree(&from, &to)?;
        println!("staged {item}");
    }
    Ok(())
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}
