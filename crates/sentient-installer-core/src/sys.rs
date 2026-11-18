//! Small cross-platform command helpers shared by the checks and provisioning
//! modules.

use std::ffi::{OsStr, OsString};
use std::process::Command;

/// A `Command` that never flashes a console window on Windows (the GUI app
/// spawns console tools like wsl.exe/powershell), and that runs on the
/// computer itself when this program runs as a Flatpak ([`host_command`]).
pub(crate) fn command(program: &str) -> Command {
    host_command(program)
}

/// This program runs inside a Flatpak sandbox (Flatpak sets `FLATPAK_ID`
/// in every app it starts).
pub fn in_flatpak() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("FLATPAK_ID").is_some_and(|v| !v.is_empty())
}

/// The program and the leading arguments that run `program` on the
/// computer itself. Inside a Flatpak the tools the Platform Manager drives
/// (docker, systemctl, pkexec, journalctl, the package managers...) are the
/// host's, not the sandbox's: `flatpak-spawn --host <program>` (the
/// org.freedesktop.Flatpak portal) runs them there, with this process's
/// standard input, output and error. `env_file`, when given, is a file of
/// `KEY='value'` lines the host shell loads, then deletes, before it starts
/// `program`: flatpak-spawn does not pass this process's environment on,
/// and `--env=KEY=VALUE` would put a password on a command line others can
/// read.
pub fn host_argv(flatpak: bool, program: &OsStr, env_file: Option<&OsStr>) -> (OsString, Vec<OsString>) {
    if !flatpak {
        return (program.to_os_string(), Vec::new());
    }
    let mut args: Vec<OsString> = vec!["--host".into()];
    if let Some(file) = env_file {
        args.extend(["sh".into(), "-c".into(), ENV_LOADER.into(), "spm-env".into(), file.to_os_string()]);
    }
    args.push(program.to_os_string());
    (OsString::from("flatpak-spawn"), args)
}

/// Load the variables in `$1`, delete it, run the rest of the arguments.
const ENV_LOADER: &str = r#"f=$1; shift; set -a; . "$f"; set +a; rm -f "$f"; exec "$@""#;

/// `Command::new(program)` for a program of the computer itself: through
/// `flatpak-spawn --host` inside a Flatpak ([`host_argv`]), unchanged
/// elsewhere. Never for this program's own executable (the host cannot run
/// a file inside the sandbox) or tools the sandbox carries itself (ssh).
/// Variables set on the returned command with `.env()` reach flatpak-spawn
/// only: use [`host_command_env`] for those.
pub fn host_command(program: impl AsRef<OsStr>) -> Command {
    host_command_env(program, &[])
}

/// [`host_command`] with variables for the program.
pub fn host_command_env(program: impl AsRef<OsStr>, env: &[(&str, &str)]) -> Command {
    let flatpak = in_flatpak();
    let env_file = if flatpak && !env.is_empty() { write_env_file(env) } else { None };
    let (prog, lead) = host_argv(flatpak, program.as_ref(), env_file.as_deref().map(OsStr::new));
    #[allow(unused_mut)]
    let mut c = Command::new(prog);
    c.args(lead);
    if !flatpak || env_file.is_none() {
        // Outside a Flatpak, or when the file could not be written (the
        // variables then reach flatpak-spawn only, and the step says what
        // it misses).
        c.envs(env.iter().copied());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        // A folder that surely exists to run in: never the one this
        // process was started from, which the elevated session may not see
        // (a mapped network drive, a removed USB stick, a deleted folder).
        // cmd then fails with "The current directory is invalid", and
        // initdb's `postgres -V` (run through cmd) returned nothing, so
        // initdb said postgres.exe "was not found".
        c.current_dir(safe_dir(program.as_ref()));
    }
    c
}

/// A folder to run `program` in that exists: the program's own folder when
/// it is given with its path, else the temporary folder, else Windows'.
pub fn safe_dir(program: &OsStr) -> std::path::PathBuf {
    let p = std::path::Path::new(program);
    if p.is_absolute() {
        if let Some(dir) = p.parent().filter(|d| d.is_dir()) {
            return dir.to_path_buf();
        }
    }
    let tmp = std::env::temp_dir();
    if tmp.is_dir() {
        return tmp;
    }
    std::env::var_os("SystemRoot").map(std::path::PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
}

/// Make this process's own working folder one that exists (started from a
/// folder that is gone or that this session cannot reach): every program it
/// starts, and every relative path, would otherwise fail. Call it first.
pub fn ensure_valid_cwd() {
    let fine = std::env::current_dir().map(|d| d.is_dir()).unwrap_or(false);
    if fine {
        return;
    }
    let to = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    let _ = std::env::set_current_dir(to);
}

/// `KEY='value'` lines for the host shell, in a file only this account can
/// read, in the temporary folder (a folder the host sees at the same path
/// inside a Flatpak, see [`flatpak_init`]).
#[cfg(unix)]
fn env_file_text(env: &[(&str, &str)]) -> String {
    env.iter()
        .filter(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(|(k, v)| format!("{k}={}\n", crate::exec::shq(v)))
        .collect()
}

#[cfg(unix)]
fn write_env_file(env: &[(&str, &str)]) -> Option<String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir();
    let path = dir.join(format!("spm-env-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).ok()?;
    f.write_all(env_file_text(env).as_bytes()).ok()?;
    Some(path.display().to_string())
}

#[cfg(not(unix))]
fn write_env_file(_env: &[(&str, &str)]) -> Option<String> {
    None
}

/// Inside a Flatpak, make the temporary folder one the host sees at the
/// same path: the sandbox's /tmp is its own, so a script, an icon or a
/// download put there for a host command (or for pkexec) would not be
/// found. `$XDG_CACHE_HOME` is `~/.var/app/<id>/cache`, which the host
/// has at that same path. Call first thing in main, before any thread.
pub fn flatpak_init() {
    if !in_flatpak() || std::env::var_os("SPM_FLATPAK_TMP_SET").is_some() {
        return;
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".var/app").join(std::env::var_os("FLATPAK_ID").unwrap_or_default()).join("cache")));
    if let Some(cache) = cache {
        let tmp = cache.join("tmp");
        if std::fs::create_dir_all(&tmp).is_ok() {
            std::env::set_var("TMPDIR", &tmp);
            std::env::set_var("SPM_FLATPAK_TMP_SET", "1");
        }
    }
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

/// The Windows account this process runs as (DOMAIN\\user), for messages.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn account_name() -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    }
}

/// [`output_limited`] for `wsl.exe` and the commands run through it: a
/// WSL that does not answer (as SYSTEM, or as an account the distro does
/// not belong to, wsl.exe can wait for ever) becomes a failure that says
/// which account it ran as. Elsewhere and for other programs, the same
/// limit without the note.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn output_wsl(program: &str, args: &[&str], limit: std::time::Duration) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    let r = output_limited(program, args, limit);
    match r {
        Some((false, out, mut err)) if String::from_utf8_lossy(&err).contains("did not finish within") => {
            if program.eq_ignore_ascii_case("wsl.exe") {
                err.extend_from_slice(
                    format!(
                        ". WSL did not answer. WSL distros belong to the Windows account that installed them; this ran as {}. Sign in to that server as the account that owns the distro, or start WSL there once by hand.",
                        account_name()
                    )
                    .as_bytes(),
                );
            }
            Some((false, out, err))
        }
        other => other,
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
    let child = host_command_env(program, env)
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
pub fn decode(bytes: &[u8]) -> String {
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
    let mut child = host_command_env(program, env)
        .args(args)
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

#[cfg(test)]
mod host_tests {
    use super::*;

    fn strs(p: OsString, a: Vec<OsString>) -> Vec<String> {
        std::iter::once(p).chain(a).map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn outside_a_flatpak_the_program_runs_as_it_is() {
        let (p, a) = host_argv(false, OsStr::new("docker"), None);
        assert_eq!(strs(p, a), ["docker"]);
        let (p, a) = host_argv(false, OsStr::new("psql"), Some(OsStr::new("/x/env")));
        assert_eq!(strs(p, a), ["psql"]);
    }

    #[test]
    fn inside_a_flatpak_the_host_runs_it() {
        let (p, a) = host_argv(true, OsStr::new("systemctl"), None);
        assert_eq!(strs(p, a), ["flatpak-spawn", "--host", "systemctl"]);
        // The arguments the caller adds come after the program.
        let (p, a) = host_argv(true, OsStr::new("docker"), None);
        let mut c = Command::new(p);
        c.args(a).args(["ps", "-a"]);
        let argv: Vec<String> = c.get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(c.get_program(), "flatpak-spawn");
        assert_eq!(argv, ["--host", "docker", "ps", "-a"]);
    }

    #[test]
    fn variables_go_through_a_file_not_the_command_line() {
        let (p, a) = host_argv(true, OsStr::new("psql"), Some(OsStr::new("/home/u/.var/app/x/cache/tmp/spm-env-1-0")));
        let v = strs(p, a);
        assert_eq!(&v[..4], ["flatpak-spawn", "--host", "sh", "-c"]);
        assert_eq!(&v[5..], ["spm-env", "/home/u/.var/app/x/cache/tmp/spm-env-1-0", "psql"]);
        assert!(!v.iter().any(|s| s.contains("PGPASSWORD")));
    }

    #[cfg(unix)]
    #[test]
    fn the_env_file_quotes_values_and_drops_bad_names() {
        let t = env_file_text(&[("PGPASSWORD", "it's $x"), ("LC_ALL", "C"), ("BAD NAME", "1"), ("", "2")]);
        assert_eq!(t, "PGPASSWORD='it'\\''s $x'\nLC_ALL='C'\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_loader_sets_the_variables_deletes_the_file_and_runs_the_program() {
        let dir = std::env::temp_dir().join(format!("spm-env-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("env");
        std::fs::write(&file, env_file_text(&[("SPM_T", "a b'c")])).unwrap();
        let out = Command::new("sh")
            .args(["-c", ENV_LOADER, "spm-env"])
            .arg(&file)
            .args(["sh", "-c", "printf %s \"$SPM_T\""])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a b'c");
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod safe_dir_tests {
    use super::*;

    #[test]
    fn a_program_with_its_path_runs_in_its_own_folder() {
        let tmp = std::env::temp_dir();
        let prog = tmp.join("spm-safe-dir-test.exe");
        assert_eq!(safe_dir(prog.as_os_str()), tmp);
    }

    #[test]
    fn a_bare_program_or_a_missing_folder_runs_in_the_temporary_folder() {
        assert_eq!(safe_dir(OsStr::new("sc.exe")), std::env::temp_dir());
        let gone = std::env::temp_dir().join("spm-no-such-folder-9f3c").join("initdb.exe");
        assert_eq!(safe_dir(gone.as_os_str()), std::env::temp_dir());
    }
}
