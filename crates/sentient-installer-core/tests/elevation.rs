// Copyright © 2016-2026 The SENTIENT Authors

//! The one root helper per job (sentient_installer_core::elevation), with a
//! stand-in pkexec on PATH that counts how often it is asked and then runs
//! the helper as this user. Counts the password prompts a job causes.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sentient_installer_core::elevation::{self, ElevationError};
use sentient_installer_core::progress::{Progress, ProgressFn};

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spm-elevation-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    dir
}

/// pkexec that counts, refuses while `cancel` exists (as a closed dialog
/// does: 126), else runs the command.
fn fake_pkexec(dir: &Path) {
    let p = dir.join("bin/pkexec");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\necho asked >> {count}\n[ -f {cancel} ] && exit 126\nexec \"$@\"\n",
            count = dir.join("count").display(),
            cancel = dir.join("cancel").display()
        ),
    )
    .unwrap();
    Command::new("chmod").arg("755").arg(&p).status().unwrap();
}

fn asked(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("count")).map(|t| t.lines().count()).unwrap_or(0)
}

fn collecting() -> (ProgressFn, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let l = lines.clone();
    let sink: ProgressFn = Arc::new(move |p| {
        if let Progress::Log { line } = p {
            l.lock().unwrap().push(line);
        }
    });
    (sink, lines)
}

fn run(script: &str) -> Result<(), String> {
    let (sink, _) = collecting();
    sentient_installer_core::privileged::run(&sink, script)
}

fn is_root() -> bool {
    Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0").unwrap_or(false)
}

/// One test: the session is process-wide, so the cases run in order.
#[test]
fn one_prompt_per_job() {
    if is_root() {
        eprintln!("skipped: as root nothing is elevated");
        return;
    }
    let dir = scratch();
    fake_pkexec(&dir);
    let path = format!("{}:{}", dir.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", path);
    elevation::set_helper(vec![env!("CARGO_BIN_EXE_sinative").to_string(), elevation::SESSION_ARG.to_string()]);

    // A page view with no helper alive: refused, nothing asked.
    let e = run("true").unwrap_err();
    assert_eq!(ElevationError::from_message(&e), Some(ElevationError::NotNow), "{e}");
    assert_eq!(asked(&dir), 0);

    // A job of five root steps (setup, files, units, ports, a probe): one prompt.
    {
        let _job = elevation::user_action();
        let (sink, lines) = collecting();
        for step in ["echo packages", "echo initdb", "echo units", "echo ports >&2"] {
            sentient_installer_core::privileged::run(&sink, step).unwrap();
        }
        let (ok, out, _) = elevation::output(&["bash".into(), "-c".into(), "printf 'a\\nb\\n'".into()]).unwrap();
        assert!(ok);
        assert_eq!(out, "a\nb\n");
        let got = lines.lock().unwrap().clone();
        for want in ["packages", "initdb", "units", "ports"] {
            assert!(got.iter().any(|l| l == want), "{want} in {got:?}");
        }
        // A failing step reports its code; the helper stays.
        let e = run("echo before; exit 43").unwrap_err();
        assert_eq!(e, "bash exited with 43");
        run("true").unwrap();
    }
    assert_eq!(asked(&dir), 1, "one password prompt for the whole job");
    assert_eq!(elevation::prompts(), 1);

    // A page view while the helper lives: read through it, still no prompt.
    run("true").unwrap();
    assert_eq!(asked(&dir), 1);

    // A second job (Retry) while the helper lives: reused.
    {
        let _job = elevation::user_action();
        run("true").unwrap();
    }
    assert_eq!(asked(&dir), 1, "retry reuses the live helper");

    // Cancel during a root step: it stops at once, the helper stays.
    {
        let _job = elevation::user_action();
        let started = Instant::now();
        let t = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(600));
            sentient_installer_core::cancel::request();
        });
        let e = run("echo partial; sleep 30; echo never").unwrap_err();
        t.join().unwrap();
        assert_eq!(e, "Cancelled.");
        assert!(started.elapsed() < Duration::from_secs(8), "cancel took {:?}", started.elapsed());
        sentient_installer_core::cancel::reset();
        run("true").unwrap();
    }
    assert_eq!(asked(&dir), 1);

    // The helper gone (idle timeout, killed): the next job asks once more.
    elevation::end();
    assert!(!elevation::alive());
    assert!(run("true").is_err(), "a page view does not bring it back");
    {
        let _job = elevation::user_action();
        run("true").unwrap();
        run("true").unwrap();
    }
    assert_eq!(asked(&dir), 2);

    // A closed password prompt: a typed Cancelled, then "Ask again" works.
    elevation::end();
    std::fs::write(dir.join("cancel"), "").unwrap();
    {
        let _job = elevation::user_action();
        let e = run("true").unwrap_err();
        assert_eq!(ElevationError::from_message(&e), Some(ElevationError::Cancelled), "{e}");
        assert_eq!(e, ElevationError::CANCELLED_MESSAGE);
    }
    std::fs::remove_file(dir.join("cancel")).unwrap();
    {
        let _job = elevation::user_action();
        run("true").unwrap();
        run("true").unwrap();
    }
    assert_eq!(asked(&dir), 4, "the closed prompt and the one Ask again");
    elevation::end();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The window goes away mid-step: the helper finishes that step (Setup's
/// Cancel promises "stops after the current task") and then ends.
#[test]
fn helper_ends_with_its_window() {
    let dir = std::env::temp_dir().join(format!("spm-elevation-eof-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mark = dir.join("finished");
    let mut helper = Command::new(env!("CARGO_BIN_EXE_sinative"))
        .arg(elevation::SESSION_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(helper.stdout.take().unwrap());
    let mut first = String::new();
    out.read_line(&mut first).unwrap();
    assert_eq!(first.trim(), "\"ready\"");
    let mut stdin = helper.stdin.take().unwrap();
    let step = serde_json::json!({"run": {"id": 1, "argv": ["bash", "-c", format!("sleep 1; touch {}", mark.display())], "input": null}});
    writeln!(stdin, "{step}").unwrap();
    drop(stdin);
    let started = Instant::now();
    let status = helper.wait().unwrap();
    assert!(status.success());
    assert!(mark.exists(), "the step in progress finished");
    assert!(started.elapsed() < Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}
