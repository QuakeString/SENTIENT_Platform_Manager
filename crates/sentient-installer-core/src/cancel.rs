//! Cooperative cancellation for the long-running install steps (WSL2 / Docker /
//! deploy). The frontend calls `request()` from a separate command while a step
//! is running on a `spawn_blocking` thread. We flip a flag the step's loops
//! check, and kill the currently-registered child process tree so a blocking
//! `wait()` returns promptly.
//!
//! Only one step runs at a time, so a single global slot for the child PID is
//! enough. Quick helper commands don't register, so `request()` only ever kills
//! the genuinely long-running child.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static CHILD_PID: AtomicU32 = AtomicU32::new(0);

/// Clear the flag at the start of a step (a previous run may have set it).
pub fn reset() {
    CANCELLED.store(false, Ordering::SeqCst);
    CHILD_PID.store(0, Ordering::SeqCst);
}

/// Has the user asked to cancel the current step?
pub fn is_cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

/// Register the child currently doing the long work, so `request()` can kill it.
pub fn register_pid(pid: u32) {
    CHILD_PID.store(pid, Ordering::SeqCst);
}

/// The registered child has exited normally — stop tracking it.
pub fn clear_pid() {
    CHILD_PID.store(0, Ordering::SeqCst);
}

/// Request cancellation: set the flag and kill the tracked child process tree.
pub fn request() {
    CANCELLED.store(true, Ordering::SeqCst);
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid != 0 {
        kill_tree(pid);
    }
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    // /T kills the whole tree (wsl.exe + its children), /F forces it.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
}

#[cfg(not(windows))]
fn kill_tree(_pid: u32) {}
