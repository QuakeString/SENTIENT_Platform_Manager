// Copyright © 2016-2026 The SENTIENT Authors

//! What a terminal session runs: the shell for the system, the first dim
//! line ("Signed in as …"), and the Platform Manager's own segmented prompt
//! for that session only.
//!
//! Nothing is written on the server. On Linux the session runs a small
//! POSIX script (passed base64-encoded, so no quoting passes through the
//! account's login shell, whatever it is): bash gets the login files and
//! then the prompt through `--rcfile /dev/fd/3` (a here-document), any
//! other shell starts as a login shell with its own prompt. On Windows
//! PowerShell runs `-NoExit -EncodedCommand` with a `prompt` function. A
//! prompt theme the account already uses (Powerlevel10k, Oh My Posh,
//! Starship, …) is kept: the check runs after the account's own files.

use base64::Engine as _;

/// The prompt's colours (the approved mockup).
pub const SEG_USER: (u8, u8, u8) = (0x5c, 0x6b, 0x70);
pub const SEG_DIR_LINUX: (u8, u8, u8) = (0x18, 0x70, 0x74);
pub const SEG_DIR_POWERSHELL: (u8, u8, u8) = (0x2f, 0x5f, 0x9e);
pub const SEG_TEXT: (u8, u8, u8) = (0xd8, 0xde, 0xe0);

/// Nerd Font glyphs (MesloLGS NF, crate::assets).
pub const GLYPH_LINUX: char = '\u{f17c}';
pub const GLYPH_WINDOWS: char = '\u{f17a}';
pub const GLYPH_FOLDER: char = '\u{f07c}';
/// Powerline: the arrow joining two segments, the rounded ends.
pub const GLYPH_ARROW: char = '\u{e0b0}';
pub const GLYPH_ROUND_LEFT: char = '\u{e0b6}';
pub const GLYPH_ROUND_RIGHT: char = '\u{e0b4}';

/// Signs that a prompt theme is set up, in a shell's start-up file text.
/// The same names are checked at run time (variables and the prompt hook,
/// [`bash_theme_check`], [`powershell_theme_check`]).
const THEME_MARKERS: &[(&str, &str)] = &[
    ("powerlevel10k", "Powerlevel10k"),
    ("p10k", "Powerlevel10k"),
    ("POWERLEVEL9K", "Powerlevel10k"),
    ("oh-my-posh", "Oh My Posh"),
    ("oh_my_posh", "Oh My Posh"),
    ("POSH_THEME", "Oh My Posh"),
    ("starship", "Starship"),
    ("Invoke-Starship", "Starship"),
    ("oh-my-bash", "Oh My Bash"),
    ("OSH_THEME", "Oh My Bash"),
    ("bash-it", "Bash-it"),
    ("BASH_IT_THEME", "Bash-it"),
    ("liquidprompt", "Liquid Prompt"),
];

/// The prompt theme a start-up file sets up, if any (a commented-out line
/// does not count).
pub fn detect_theme(rc_text: &str) -> Option<&'static str> {
    rc_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .find_map(|l| THEME_MARKERS.iter().find(|(m, _)| l.contains(m)).map(|(_, name)| *name))
}

fn sgr_fg((r, g, b): (u8, u8, u8)) -> String {
    format!("38;2;{r};{g};{b}")
}

fn sgr_bg((r, g, b): (u8, u8, u8)) -> String {
    format!("48;2;{r};{g};{b}")
}

/// bash's PS1: "<penguin> user" on grey, the arrow, "<folder> cwd" on teal,
/// rounded ends. Escapes inside `\[ \]` so readline counts only the text.
pub fn bash_ps1() -> String {
    let e = |codes: String| format!("\\[\\e[{codes}m\\]");
    let user = SEG_USER;
    let dir = SEG_DIR_LINUX;
    let text = SEG_TEXT;
    format!(
        "{}{GLYPH_ROUND_LEFT}{} {GLYPH_LINUX} \\u {}{GLYPH_ARROW}{} {GLYPH_FOLDER} \\w {}{GLYPH_ROUND_RIGHT}{} ",
        e(format!("0;{}", sgr_fg(user))),
        e(format!("{};{}", sgr_bg(user), sgr_fg(text))),
        e(format!("{};{}", sgr_fg(user), sgr_bg(dir))),
        e(sgr_fg(text)),
        e(format!("0;{}", sgr_fg(dir))),
        e("0".into()),
    )
}

/// The bash test, after the account's files ran: a theme in use keeps its
/// prompt. Exits 0 when one is in use.
pub fn bash_theme_check() -> &'static str {
    r#"[ -n "${STARSHIP_SHELL:-}${POSH_THEME:-}${POSH_PID:-}${POSH_SHELL:-}${OSH_THEME:-}${BASH_IT_THEME:-}${LP_VERSION:-}" ] || case "${PROMPT_COMMAND:-}" in *starship*|*_omp_*|*posh*|*_lp_*) true ;; *) false ;; esac"#
}

/// bash's start-up for a session: the login files as `bash -l` reads them,
/// then SPM's prompt unless a theme is in use.
pub fn bash_rc() -> String {
    format!(
        "[ -r /etc/profile ] && . /etc/profile\n\
         for __spm_f in \"$HOME/.bash_profile\" \"$HOME/.bash_login\" \"$HOME/.profile\"; do\n\
         \x20 if [ -r \"$__spm_f\" ]; then . \"$__spm_f\"; break; fi\n\
         done\n\
         unset __spm_f\n\
         if ! {{ {check}; }}; then PS1='{ps1}'; fi\n",
        check = bash_theme_check(),
        ps1 = bash_ps1(),
    )
}

/// `s` as one POSIX shell word.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `s` as one PowerShell single-quoted string.
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The POSIX script a Linux session runs (as the signed-in account): the
/// shell's name for the status bar (a title SPM:shell=…, caught by the
/// terminal), the dim first line, then the shell.
pub fn linux_script(host: &str, system: &str) -> String {
    format!(
        "__spm_sh=${{SHELL:-/bin/sh}}\n\
         [ -x \"$__spm_sh\" ] || __spm_sh=/bin/sh\n\
         __spm_name=${{__spm_sh##*/}}\n\
         printf '\\033]0;SPM:shell=%s\\007' \"$__spm_name\"\n\
         printf '\\033[2mSigned in as %s on %s, %s, %s\\033[0m\\r\\n' \"$(id -un)\" {host} {system} \"$__spm_name\"\n\
         if [ \"$__spm_name\" = bash ]; then\n\
         exec \"$__spm_sh\" --rcfile /dev/fd/3 -i 3<<'SPM_RC_END'\n\
         {rc}SPM_RC_END\n\
         fi\n\
         exec \"$__spm_sh\" -l\n",
        host = sh_quote(host),
        system = sh_quote(system),
        rc = bash_rc(),
    )
}

/// What ssh asks the server's login shell to run for a Linux session: the
/// script decoded and run by sh. Only `'`-free text inside single quotes,
/// so bash, zsh, fish and csh all pass it as it is.
pub fn linux_remote_command(host: &str, system: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(linux_script(host, system));
    format!("exec /bin/sh -c 'eval \"$(printf %s {b64} | base64 -d)\"'")
}

/// The PowerShell test, after the profile ran: a theme in use keeps its
/// prompt.
pub fn powershell_theme_check() -> &'static str {
    "($env:POSH_THEME -or $env:POSH_SHELL -or $env:POSH_PID -or $env:STARSHIP_SHELL -or ((Get-Command prompt -ErrorAction SilentlyContinue).ScriptBlock -match 'oh-my-posh|posh|starship'))"
}

/// PowerShell's `prompt` function: "<windows> user" on grey, the arrow,
/// "<folder> cwd" on blue, rounded ends. The home folder reads `~`.
pub fn powershell_prompt_fn() -> String {
    let c = |ch: char| format!("$([char]0x{:x})", ch as u32);
    let user = SEG_USER;
    let dir = SEG_DIR_POWERSHELL;
    let text = SEG_TEXT;
    format!(
        "function global:prompt {{ $e = [char]27; $p = $executionContext.SessionState.Path.CurrentLocation.Path; if ($HOME -and $p.StartsWith($HOME, [StringComparison]::OrdinalIgnoreCase)) {{ $p = '~' + $p.Substring($HOME.Length) }}; \
         \"$e[0;{fu}m{l}$e[{bu};{ft}m {win} $env:USERNAME $e[{fu};{bd}m{arrow}$e[{ft}m {folder} $p $e[0;{fd}m{r}$e[0m \" }}",
        fu = sgr_fg(user),
        bu = sgr_bg(user),
        ft = sgr_fg(text),
        bd = sgr_bg(dir),
        fd = sgr_fg(dir),
        l = c(GLYPH_ROUND_LEFT),
        r = c(GLYPH_ROUND_RIGHT),
        win = c(GLYPH_WINDOWS),
        arrow = c(GLYPH_ARROW),
        folder = c(GLYPH_FOLDER),
    )
}

/// The script PowerShell runs after its profile: the dim first line, then
/// the prompt unless a theme is in use.
pub fn powershell_script(host: &str, system: &str) -> String {
    format!(
        "$e = [char]27; Write-Host \"$e[2mSigned in as $env:USERNAME on $({host}), $({system}), PowerShell$e[0m\"; if (-not {check}) {{ {prompt} }}",
        host = ps_quote(host),
        system = ps_quote(system),
        check = powershell_theme_check(),
        prompt = powershell_prompt_fn(),
    )
}

/// `script` for `-EncodedCommand` (UTF-16LE, base64).
pub fn ps_encode(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// PowerShell's arguments for a session (program first).
pub fn powershell_argv(program: &str, host: &str, system: &str) -> Vec<String> {
    vec![
        program.to_string(),
        "-NoLogo".into(),
        "-NoExit".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-EncodedCommand".into(),
        ps_encode(&powershell_script(host, system)),
    ]
}

/// What ssh asks a Windows server to run (cmd or PowerShell as its shell
/// both start it the same way).
pub fn windows_remote_command(host: &str, system: &str) -> String {
    powershell_argv("powershell", host, system).join(" ")
}

/// The shell's name from the title a Linux session sets first.
pub fn shell_from_title(title: &str) -> Option<String> {
    title.strip_prefix("SPM:shell=").map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bash_prompt_has_both_segments_and_counts_only_text() {
        let ps1 = bash_ps1();
        assert!(ps1.contains(" \\u "), "{ps1}");
        assert!(ps1.contains(" \\w "), "{ps1}");
        for g in [GLYPH_ROUND_LEFT, GLYPH_LINUX, GLYPH_ARROW, GLYPH_FOLDER, GLYPH_ROUND_RIGHT] {
            assert!(ps1.contains(g), "{g:?} missing in {ps1}");
        }
        assert!(ps1.contains("48;2;92;107;112"), "grey user segment: {ps1}");
        assert!(ps1.contains("48;2;24;112;116"), "teal folder segment: {ps1}");
        // Every escape sits inside \[ \] (readline must not count it).
        let outside: String = ps1.split("\\[").map(|p| p.split_once("\\]").map(|(_, rest)| rest).unwrap_or(p)).collect();
        assert!(!outside.contains("\\e["), "{outside}");
        // It goes inside single quotes in the rc file.
        assert!(!ps1.contains('\''));
        assert!(ps1.ends_with("\\[\\e[0m\\] "));
    }

    #[test]
    fn the_powershell_prompt_is_blue_and_says_the_windows_logo() {
        let f = powershell_prompt_fn();
        assert!(f.starts_with("function global:prompt {"), "{f}");
        assert!(f.contains("48;2;47;95;158"), "blue folder segment: {f}");
        assert!(f.contains("48;2;92;107;112"), "grey user segment: {f}");
        assert!(f.contains("$([char]0xf17a)"), "windows logo: {f}");
        assert!(f.contains("$([char]0xe0b0)"), "arrow: {f}");
        assert!(f.contains("$env:USERNAME"));
    }

    #[test]
    fn a_powershell_session_keeps_a_theme_and_quotes_names() {
        let s = powershell_script("srv-a.example", "Windows Server 2022 O'Neil");
        assert!(s.contains("if (-not ($env:POSH_THEME"), "{s}");
        assert!(s.contains("'Windows Server 2022 O''Neil'"), "{s}");
        let argv = powershell_argv("powershell", "h", "s");
        assert_eq!(&argv[..6], ["powershell", "-NoLogo", "-NoExit", "-ExecutionPolicy", "Bypass", "-EncodedCommand"]);
        // UTF-16LE base64 decodes back to the script.
        let bytes = base64::engine::general_purpose::STANDARD.decode(&argv[6]).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), powershell_script("h", "s"));
    }

    #[test]
    fn themes_are_found_in_start_up_files() {
        assert_eq!(detect_theme("eval \"$(starship init bash)\"\n"), Some("Starship"));
        assert_eq!(detect_theme("eval \"$(oh-my-posh init bash --config ~/x.json)\""), Some("Oh My Posh"));
        assert_eq!(detect_theme("oh-my-posh init pwsh | Invoke-Expression"), Some("Oh My Posh"));
        assert_eq!(detect_theme("source ~/powerlevel10k/powerlevel10k.zsh-theme"), Some("Powerlevel10k"));
        assert_eq!(detect_theme("[[ ! -f ~/.p10k.zsh ]] || source ~/.p10k.zsh"), Some("Powerlevel10k"));
        assert_eq!(detect_theme("Invoke-Expression (&starship init powershell)"), Some("Starship"));
        assert_eq!(detect_theme("OSH_THEME=\"font\""), Some("Oh My Bash"));
        // Commented out, or none.
        assert_eq!(detect_theme("# eval \"$(starship init bash)\"\nalias ll='ls -l'\n"), None);
        assert_eq!(detect_theme("PS1='\\u@\\h:\\w\\$ '\n"), None);
    }

    #[test]
    fn the_linux_command_survives_any_login_shell() {
        let c = linux_remote_command("203.0.113.7", "Ubuntu 22.04");
        // Only the outer pair of single quotes, no backslash: fish and csh
        // read it as bash does.
        assert_eq!(c.matches('\'').count(), 2, "{c}");
        assert!(!c.contains('\\'), "{c}");
        assert!(c.starts_with("exec /bin/sh -c 'eval"));
        let b64 = c.split_whitespace().find(|w| w.len() > 40).unwrap();
        let script = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
        assert_eq!(script, linux_script("203.0.113.7", "Ubuntu 22.04"));
        assert!(script.contains("'203.0.113.7' 'Ubuntu 22.04'"), "{script}");
        assert!(script.contains("--rcfile /dev/fd/3"));
    }

    #[test]
    fn names_are_quoted_for_sh() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("$(reboot)"), "'$(reboot)'");
    }

    #[test]
    fn the_shell_name_comes_from_the_first_title() {
        assert_eq!(shell_from_title("SPM:shell=bash"), Some("bash".into()));
        assert_eq!(shell_from_title("ubuntu@srv: ~"), None);
        assert_eq!(shell_from_title("SPM:shell="), None);
    }

    /// The script really sets SPM's prompt in bash, and keeps a theme's
    /// (bash runs it with a stand-in home; skipped where there is no bash).
    #[cfg(unix)]
    #[test]
    fn bash_gets_the_prompt_unless_a_theme_is_in_use() {
        use std::process::Command;
        if Command::new("bash").arg("--version").output().is_err() {
            return;
        }
        let home = std::env::temp_dir().join(format!("spm-term-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let rc = home.join("rc");
        std::fs::write(&rc, bash_rc()).unwrap();
        let ps1_with = |profile: &str| {
            std::fs::write(home.join(".profile"), profile).unwrap();
            let out = Command::new("bash")
                .arg("--rcfile")
                .arg(&rc)
                .args(["-i", "-c", "printf %s \"$PS1\""])
                .env_clear()
                .env("HOME", &home)
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        assert_eq!(ps1_with("PS1='plain$ '\n"), bash_ps1());
        assert_eq!(ps1_with("PS1='themed$ '\nexport STARSHIP_SHELL=bash\n"), "themed$ ");
        assert_eq!(ps1_with("PS1='themed$ '\nPROMPT_COMMAND='_omp_hook'\n"), "themed$ ");
        let _ = std::fs::remove_dir_all(&home);
    }
}
