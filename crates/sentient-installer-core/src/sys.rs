//! Small cross-platform command helpers shared by the checks and provisioning
//! modules.

use std::process::Command;

/// A `Command` that never flashes a console window on Windows (the GUI app
/// spawns console tools like wsl.exe/powershell). No-op elsewhere.
pub(crate) fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// Run a command; returns (success, stdout, stderr). None if it couldn't spawn.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn output(program: &str, args: &[&str]) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    let o = command(program).args(args).output().ok()?;
    Some((o.status.success(), o.stdout, o.stderr))
}

/// Like `output`, but registers the child PID with the cancellation module for
/// the duration of the call so a Cancel request can kill this (possibly slow)
/// process. Use for long provisioning commands, not quick probes.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn output_tracked(program: &str, args: &[&str]) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    use std::process::Stdio;
    let child = command(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    crate::cancel::register_pid(child.id());
    let o = child.wait_with_output();
    crate::cancel::clear_pid();
    let o = o.ok()?;
    Some((o.status.success(), o.stdout, o.stderr))
}

/// `wsl.exe` writes UTF-16LE on many Windows builds; decode either encoding.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn decode(bytes: &[u8]) -> String {
    let nulls = bytes.iter().skip(1).step_by(2).take(8).filter(|&&b| b == 0).count();
    if bytes.len() >= 2 && nulls >= 3 {
        let u16s: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}
