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

/// [`output`] that gives up after `limit`: the child is killed and the
/// answer is a failure saying so. For steps an uninstall must not hang on.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn output_limited(program: &str, args: &[&str], limit: std::time::Duration) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = command(program).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let reader_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let reader_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < limit => std::thread::sleep(std::time::Duration::from_millis(200)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = reader_out.join().unwrap_or_default();
    let mut stderr = reader_err.join().unwrap_or_default();
    match status {
        Some(s) => Some((s.success(), stdout, stderr)),
        None => {
            stderr.extend_from_slice(format!("{program} did not finish within {} s and was stopped", limit.as_secs()).as_bytes());
            Some((false, stdout, stderr))
        }
    }
}

/// [`output_tracked`] with extra environment variables for the child.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn output_tracked_env(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    use std::process::Stdio;
    let child = command(program)
        .args(args)
        .envs(env.iter().copied())
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

/// Like `output_tracked_env`, but every output line also goes to `sink` as
/// it comes (rate-limited like `exec::stream`), so a long step shows what it
/// is doing instead of nothing until it ends.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn output_streamed_env(
    sink: &crate::progress::ProgressFn,
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let mut child = command(program)
        .args(args)
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    crate::cancel::register_pid(child.id());
    // Each reader copies what it reads into a buffer and on to the sink.
    let tee = |mut r: Box<dyn Read + Send>, sink: crate::progress::ProgressFn| {
        let kept = Arc::new(Mutex::new(Vec::new()));
        let k = kept.clone();
        let handle = std::thread::spawn(move || {
            struct Tee<R> {
                inner: R,
                kept: Arc<Mutex<Vec<u8>>>,
            }
            impl<R: Read> Read for Tee<R> {
                fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                    let n = self.inner.read(buf)?;
                    if let Ok(mut k) = self.kept.lock() {
                        k.extend_from_slice(&buf[..n]);
                    }
                    Ok(n)
                }
            }
            crate::exec::throttle_lines(Tee { inner: &mut r, kept: k }, &sink);
        });
        (handle, kept)
    };
    let (t1, out) = tee(Box::new(child.stdout.take()?), sink.clone());
    let (t2, err) = tee(Box::new(child.stderr.take()?), sink.clone());
    let status = child.wait();
    let _ = t1.join();
    let _ = t2.join();
    crate::cancel::clear_pid();
    let take = |m: Arc<Mutex<Vec<u8>>>| m.lock().map(|v| v.clone()).unwrap_or_default();
    Some((status.ok()?.success(), take(out), take(err)))
}
