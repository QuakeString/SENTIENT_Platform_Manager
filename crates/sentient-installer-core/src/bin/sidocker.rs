//! `sidocker` — drive the Docker deployment engine headlessly, the way
//! `sinative` drives the native one. A test harness, not a shipped tool: the
//! wizard (app/) calls the same functions.
//!
//! Usage:
//!   sidocker compose   <config.json>        print the generated compose file
//!   sidocker setup                          make Docker reachable (Linux: install if missing)
//!   sidocker deploy    <config.json>
//!   sidocker status    <config.json>
//!   sidocker logs      <config.json> [tail]
//!   sidocker control   <config.json> <start|stop|restart>
//!   sidocker update    <config.json>
//!   sidocker cleanup   <config.json>        remove containers + volumes (retry-clean)
//!   sidocker uninstall <config.json>        remove everything of this stack
//!
//! `<config.json>` is a (possibly partial) `DeployConfig`; missing keys take
//! the reference defaults, exactly as the wizard's payload does.

use std::sync::Arc;

use sentient_installer_core::distro::{self, DeployConfig};
use sentient_installer_core::progress::Progress;

fn sink() -> sentient_installer_core::progress::ProgressFn {
    Arc::new(|p: Progress| match p {
        Progress::Step { name } => println!("\n==> {name}"),
        Progress::Log { line } => println!("    {line}"),
        Progress::Percent { value } => println!("    {:.0}%", value * 100.0),
        Progress::Done { message } => println!("\n[done] {message}"),
        Progress::Error { message } => eprintln!("\n[error] {message}"),
    })
}

fn load(path: Option<&String>) -> Result<DeployConfig, String> {
    let Some(path) = path else { return Ok(DeployConfig::default()) };
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {path}: {e}"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: sidocker <compose|setup|deploy|status|logs|control|update|cleanup|uninstall> [config.json] [arg]");
        std::process::exit(2);
    }
    let cfg = match load(args.get(2)) {
        Ok(c) => c,
        Err(e) => { eprintln!("{e}"); std::process::exit(2); }
    };
    let result = match args[1].as_str() {
        "compose" => { print!("{}", distro::compose(&cfg)); cfg.validate() }
        "setup" => distro::setup(sink(), std::path::Path::new("/tmp/sentient-spm")),
        "deploy" => distro::deploy(sink(), &cfg),
        "status" => {
            let s = distro::status(&cfg);
            println!("installed: {}\nrunning:   {}", s.installed, s.running);
            for c in s.containers {
                println!("  {:<28} {:<10} {}", c.name, c.state, c.status);
            }
            Ok(())
        }
        "logs" => {
            let tail = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(40);
            println!("{}", distro::logs(&cfg, tail));
            Ok(())
        }
        "control" => distro::control(sink(), &cfg, args.get(3).map(|s| s.as_str()).unwrap_or("restart")),
        "update" => distro::update(sink(), &cfg),
        "cleanup" => distro::cleanup(sink(), &cfg),
        "uninstall" => distro::uninstall(sink(), &cfg),
        other => Err(format!("unknown command: {other}")),
    };
    match result {
        Ok(()) => println!("\nOK"),
        Err(e) => { eprintln!("\nFAILED: {e}"); std::process::exit(1); }
    }
}
