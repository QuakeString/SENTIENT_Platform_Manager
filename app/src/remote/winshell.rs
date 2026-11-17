// Copyright © 2016-2026 The SENTIENT Authors

//! The shell OpenSSH Server runs a Windows server's commands in.
//!
//! OpenSSH for Windows runs every command it is given in its default
//! shell: cmd.exe unless `HKLM\SOFTWARE\OpenSSH DefaultShell` names
//! another, often PowerShell. A program path with a space in it is started
//! differently in the two: cmd takes `"C:\a b\x.exe" --job serve`, while
//! PowerShell reads that as a string followed by unexpected words and
//! refuses it (it needs `& 'C:\a b\x.exe' --job serve`).
//!
//! - Commands that can go through PowerShell's `-EncodedCommand` do
//!   ([`encoded`]): that line has no quoting at all, so it reads the same in
//!   either shell.
//! - The rest (the connection's agent, crate::remote::conn, whose standard
//!   input and output must be the program's own) are written for the
//!   server's shell ([`program`]), found once per server with [`PROBE`] and
//!   remembered while its address and account stay the same.

use std::collections::HashMap;
use std::sync::Mutex;

use super::Instance;

/// The default shell of OpenSSH Server on a Windows server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Cmd,
    PowerShell,
}

/// Prints `Windows_NT` in cmd (which expands `%OS%`) and `%OS%` itself in
/// PowerShell (where `%…%` is no variable). The same words in both.
pub const PROBE: &str = "echo %OS%";

/// The shell from what [`PROBE`] printed. Anything else (another shell, no
/// answer) is taken as cmd, OpenSSH's own default.
pub fn parse_probe(out: &str) -> Shell {
    let said = |w: &str| out.lines().any(|l| l.trim().eq_ignore_ascii_case(w));
    if said("Windows_NT") {
        Shell::Cmd
    } else if said("%OS%") {
        Shell::PowerShell
    } else {
        Shell::Cmd
    }
}

/// A program and its arguments as a command line for `shell`. `args` is
/// plain words (no quoting of its own; crate::remote::conn passes
/// `--job serve`).
pub fn program(shell: Shell, exe: &str, args: &str) -> String {
    match shell {
        Shell::Cmd => format!("\"{}\" {args}", exe.replace('"', "")),
        Shell::PowerShell => format!("& '{}' {args}", exe.replace('\'', "''")),
    }
}

/// A PowerShell script as one command line that reads the same in cmd and
/// in PowerShell: base64 of its UTF-16 text, nothing for either to expand.
pub fn encoded(script: &str) -> String {
    format!("powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}", encode(script))
}

/// A PowerShell script as `-EncodedCommand` takes it (progress bars off).
pub fn encode(script: &str) -> String {
    use base64::Engine as _;
    let utf16: Vec<u8> = format!("$ProgressPreference='SilentlyContinue'; {script}").encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// What each server's shell was found to be, by address and account (an
/// edited instance is asked again).
static FOUND: Mutex<Option<HashMap<String, Shell>>> = Mutex::new(None);

fn key(inst: &Instance) -> String {
    format!("{}|{}|{}|{}", inst.id, inst.host, inst.port, inst.user)
}

/// The server's default shell: asked once (one ssh call), then
/// remembered. A server that does not answer is taken as cmd for now and
/// asked again next time.
pub fn of(inst: &Instance) -> Shell {
    let k = key(inst);
    if let Some(s) = FOUND.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&k).copied()) {
        return s;
    }
    // No lock held during the call. On Windows `run` makes its own ssh
    // call (never the open connection), so this cannot wait on itself.
    match super::run(inst, PROBE, None) {
        Ok(out) => {
            let s = parse_probe(&out);
            FOUND.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(k, s);
            s
        }
        Err(_) => Shell::Cmd,
    }
}

/// Forget a server's shell (removed, or its connection failed in a way a
/// changed shell would explain): asked again on the next use.
pub fn forget(id: &str) {
    if let Some(m) = FOUND.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.retain(|k, _| k.split('|').next() != Some(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\ProgramData\SENTIENT\Platform Manager\remote\sentient-platform-manager.exe";

    #[test]
    fn the_probe_tells_cmd_from_powershell() {
        assert_eq!(parse_probe("Windows_NT\r\n"), Shell::Cmd);
        assert_eq!(parse_probe("%OS%\r\n"), Shell::PowerShell);
        // pwsh and Windows PowerShell print the same; another shell or no
        // answer stays with OpenSSH's own default.
        assert_eq!(parse_probe(""), Shell::Cmd);
        assert_eq!(parse_probe("something else"), Shell::Cmd);
    }

    #[test]
    fn a_program_with_spaces_is_quoted_for_each_shell() {
        assert_eq!(program(Shell::Cmd, EXE, "--job serve"), format!("\"{EXE}\" --job serve"));
        assert_eq!(program(Shell::PowerShell, EXE, "--job serve"), format!("& '{EXE}' --job serve"));
    }

    #[test]
    fn quotes_in_a_path_cannot_end_the_quoting() {
        let odd = r"C:\Users\o'neil\x.exe";
        assert_eq!(program(Shell::PowerShell, odd, "--job serve"), r"& 'C:\Users\o''neil\x.exe' --job serve");
        assert_eq!(program(Shell::Cmd, r#"C:\a"b\x.exe"#, "--job serve"), r#""C:\ab\x.exe" --job serve"#);
    }

    #[test]
    fn an_encoded_command_has_nothing_either_shell_expands() {
        let line = encoded("(Get-CimInstance Win32_OperatingSystem).Caption; $env:PROCESSOR_ARCHITECTURE; 'a \"b\" %OS%'");
        let (head, b64) = line.rsplit_once(' ').unwrap();
        assert_eq!(head, "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand");
        // Only base64 after the switch: no quote, $, %, &, ; or space for
        // cmd or PowerShell to read.
        assert!(b64.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')));
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16(&units).unwrap();
        assert!(text.ends_with("$env:PROCESSOR_ARCHITECTURE; 'a \"b\" %OS%'"));
    }
}
