// Copyright © 2016-2026 The SENTIENT Authors

//! Windows: reading SENTIENT's state never starts its WSL distro.
//!
//! Any `wsl.exe -d <distro> …` starts that distro when it is stopped, and
//! with it the whole stack (Docker brings the containers up): a status page
//! that polls every few seconds would start SENTIENT a minute after the
//! operator turned "Start with Windows" off. So the periodic readers (the
//! Status page, the tray's checks, the statistics and their sampler, the
//! instance status the Platform Manager on another computer asks for) run
//! as reads ([`reading`], or [`reading_process`] for a whole agent job):
//! a command for a distro that is not running is not run (it fails, as a
//! stopped stack's commands do), and [`super::status`] reports a stopped
//! distro as SENTIENT stopped without asking it anything. What the operator
//! starts (Start, Restart, Repair, an update) still starts it.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local!(static READING: Cell<bool> = const { Cell::new(false) });
static READING_PROCESS: AtomicBool = AtomicBool::new(false);

/// Run `f` as a read on this thread: no stopped distro is started by it.
pub fn reading<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            READING.with(|r| r.set(self.0));
        }
    }
    let _restore = Restore(READING.with(|r| r.replace(true)));
    f()
}

/// [`reading`] when `on` (a reader's own threads carry it on).
pub fn reading_if<T>(on: bool, f: impl FnOnce() -> T) -> T {
    if on { reading(f) } else { f() }
}

/// This whole process only reads (an agent job such as `status-json`, on
/// every thread it starts).
pub fn reading_process() {
    READING_PROCESS.store(true, Ordering::SeqCst);
}

/// Whether the work on this thread is a read.
pub fn is_reading() -> bool {
    READING_PROCESS.load(Ordering::SeqCst) || READING.with(|r| r.get())
}

/// Whether a `wsl.exe` command (its arguments) is left out: a read, for a
/// distro (`-d <name>`) that is not running.
pub fn skip(reading: bool, args: &[&str], running: impl FnOnce(&str) -> bool) -> bool {
    if !reading {
        return false;
    }
    match args {
        [flag, name, ..] if flag.eq_ignore_ascii_case("-d") || flag.eq_ignore_ascii_case("--distribution") => !running(name),
        _ => false,
    }
}

/// Whether `name` is among the running distros, from `wsl.exe -l
/// --running -q` (decoded; with no distro running it prints a sentence in
/// Windows' language instead, and exits with an error).
pub fn listed_running(decoded: &str, name: &str) -> bool {
    super::hosts::parse_list_quiet(decoded).iter().any(|l| l.eq_ignore_ascii_case(name.trim()))
}

/// Whether the WSL distro `name` runs now. Asking does not start it. A
/// second ask within two seconds gets the same answer (one reading asks
/// several times).
#[cfg(windows)]
pub fn running(name: &str) -> bool {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static LAST: Mutex<Option<(String, Instant, bool)>> = Mutex::new(None);
    if let Some((n, at, up)) = LAST.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if n.eq_ignore_ascii_case(name) && at.elapsed() < Duration::from_secs(2) {
            return up;
        }
    }
    let up = crate::sys::output_limited("wsl.exe", &["-l", "--running", "-q"], Duration::from_secs(20))
        .map(|(_, out, _)| listed_running(&crate::sys::decode(&out), name))
        .unwrap_or(false);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((name.to_string(), Instant::now(), up));
    up
}

/// Linux: no distro to start.
#[cfg(not(windows))]
pub fn running(_name: &str) -> bool {
    true
}

/// How long changing the boot task (its PowerShell) may take: on a busy
/// computer the task scheduler's module alone can take a minute to load.
pub const BOOT_TASK_LIMIT: std::time::Duration = std::time::Duration::from_secs(180);

/// Whether a PowerShell result is a run stopped at its time limit
/// (crate::sys::output_limited), rather than its own failure.
pub fn timed_out(err: &str) -> bool {
    err.contains("did not finish within")
}

/// Run `attempt` (a PowerShell run, as crate::sys::output_limited answers)
/// once more when it was stopped at its time limit; `what` names what it
/// does, for the error.
pub fn twice(what: &str, limit: std::time::Duration, mut attempt: impl FnMut() -> Option<(bool, String, String)>) -> Result<(), String> {
    for n in 0..2 {
        match attempt() {
            Some((true, _, _)) => return Ok(()),
            Some((false, _, err)) if timed_out(&err) && n == 0 => continue,
            Some((false, _, err)) if timed_out(&err) => {
                return Err(format!("{what} did not finish within {} s, also when tried again. Windows may be busy: try again in a few minutes.", limit.as_secs()));
            }
            Some((false, out, err)) => return Err(format!("{what} failed: {} {}", err.trim(), out.trim()).trim().to_string()),
            None => return Err("could not run powershell.exe".into()),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_read_leaves_a_stopped_distro_alone() {
        let args = ["-d", "sentient", "-u", "root", "--", "bash", "-lc", "docker ps"];
        assert!(skip(true, &args, |_| false));
        assert!(!skip(true, &args, |_| true), "a running distro is asked");
        assert!(!skip(false, &args, |_| false), "Start, Repair: started as before");
        // The name asked about is the one after -d.
        let mut asked = String::new();
        skip(true, &["-d", "Ubuntu", "--", "curl"], |n| {
            asked = n.to_string();
            true
        });
        assert_eq!(asked, "Ubuntu");
        // Commands about WSL itself start no distro.
        assert!(!skip(true, &["-l", "--running", "-q"], |_| false));
        assert!(!skip(true, &["--terminate", "sentient"], |_| false));
    }

    #[test]
    fn the_running_list_is_read_in_either_encoding() {
        assert!(listed_running("sentient\r\nUbuntu\r\n", "sentient"));
        assert!(listed_running("Ubuntu\r\nSENTIENT\r\n", "sentient"));
        assert!(!listed_running("Ubuntu\r\n", "sentient"));
        // Nothing running: a sentence, in Windows' language.
        assert!(!listed_running("There are no running distributions.\r\n", "sentient"));
        assert!(!listed_running("Es werden keine Distributionen ausgeführt.\r\n", "sentient"));
        // UTF-16 output (wsl.exe on many builds), decoded.
        let utf16: Vec<u8> = "sentient\r\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert!(listed_running(&crate::sys::decode(&utf16), "sentient"));
        assert!(listed_running("\u{feff}sentient\0\r\n", "sentient"));
    }

    #[test]
    fn reading_is_scoped_to_the_thread_and_restored() {
        assert!(!READING.with(|r| r.get()));
        reading(|| {
            assert!(READING.with(|r| r.get()));
            reading(|| {});
            assert!(READING.with(|r| r.get()), "a nested read keeps the outer one");
            std::thread::spawn(|| assert!(!READING.with(|r| r.get()), "another thread is not a read")).join().unwrap();
            reading_if(true, || assert!(READING.with(|r| r.get())));
        });
        assert!(!READING.with(|r| r.get()));
        reading_if(false, || assert!(!READING.with(|r| r.get())));
    }

    #[test]
    fn the_boot_task_is_tried_twice_on_a_timeout_only() {
        let stopped = || Some((false, String::new(), "powershell.exe did not finish within 180 s and was stopped".to_string()));
        let what = "Changing SENTIENT's start at boot (scheduled task)";
        // Timed out, then done.
        let mut n = 0;
        assert_eq!(
            twice(what, BOOT_TASK_LIMIT, || {
                n += 1;
                if n == 1 { stopped() } else { Some((true, String::new(), String::new())) }
            }),
            Ok(())
        );
        assert_eq!(n, 2);
        // Timed out twice: says what timed out, and after how long.
        let mut n = 0;
        let e = twice(what, BOOT_TASK_LIMIT, || {
            n += 1;
            stopped()
        })
        .unwrap_err();
        assert_eq!(n, 2);
        assert!(e.starts_with("Changing SENTIENT's start at boot (scheduled task) did not finish within 180 s"), "{e}");
        // Its own failure: not tried again.
        let mut n = 0;
        let e = twice(what, Duration::from_secs(180), || {
            n += 1;
            Some((false, String::new(), "Access is denied.".to_string()))
        })
        .unwrap_err();
        assert_eq!(n, 1);
        assert!(e.contains("Access is denied."), "{e}");
        assert_eq!(twice(what, BOOT_TASK_LIMIT, || Some((true, String::new(), String::new()))), Ok(()));
    }
}
