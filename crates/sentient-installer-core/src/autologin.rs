// Copyright © 2016-2026 The SENTIENT Authors

//! Automatic sign-in for operator screens: after a reboot the computer signs
//! in to one account by itself, so the sign-in entry (kiosk::set_open_at_signin)
//! can open SENTIENT with nobody at the keyboard. Off unless asked for.
//!
//! Windows: Winlogon's AutoAdminLogon, with the password in the LSA secret
//! store (`DefaultPassword`, readable by administrators only), never in the
//! registry. The password is checked with LogonUser first, so a typo cannot
//! leave the screen stuck at a failed sign-in after the next reboot.
//!
//! Linux: the display manager's own setting (GDM, LightDM or SDDM). No
//! password is needed there.

use crate::progress::{Progress, ProgressFn};

/// The account the computer signs in to by itself, if it does.
pub fn current() -> Option<String> {
    imp::current()
}

/// Whether the Platform Manager turned automatic sign-in on (uninstall
/// turns off only what it turned on).
pub fn set_by_manager() -> bool {
    imp::set_by_manager()
}

/// Sign in to `user` automatically (`Some((user, password))`; the password is
/// ignored on Linux), or stop (`None`). Needs administrator rights.
pub fn set(sink: &ProgressFn, account: Option<(&str, &str)>) -> Result<(), String> {
    match account {
        Some((user, password)) => {
            let user = user.trim();
            if user.is_empty() {
                return Err("Enter the account that signs in automatically.".into());
            }
            imp::enable(sink, user, password)?;
            sink(Progress::Done { message: format!("This computer signs in as {user} by itself after a restart.") });
        }
        None => {
            imp::disable(sink)?;
            sink(Progress::Done { message: "This computer no longer signs in by itself.".into() });
        }
    }
    Ok(())
}

/// The account signed in now, as a starting value for the account field.
pub fn this_user() -> String {
    let vars: &[&str] = if cfg!(windows) { &["USERNAME"] } else { &["USER", "LOGNAME"] };
    vars.iter().find_map(|v| std::env::var(v).ok().filter(|u| !u.is_empty())).unwrap_or_default()
}

/// Set (`Some`) or remove (`None`) keys in one `[section]` of an INI-style
/// file, keeping everything else as it is. The section is added when missing.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn ini_set(text: &str, section: &str, keys: &[(&str, Option<&str>)]) -> String {
    let header = format!("[{section}]");
    let mut out: Vec<String> = Vec::new();
    let mut inside = false;
    let mut found = false;
    let mut pending: Vec<(&str, Option<&str>)> = keys.to_vec();
    let flush = |out: &mut Vec<String>, pending: &mut Vec<(&str, Option<&str>)>| {
        // Insert after the section's last non-blank line.
        let at = out.iter().rposition(|l| !l.trim().is_empty()).map(|i| i + 1).unwrap_or(out.len());
        let new: Vec<String> = pending.drain(..).filter_map(|(k, v)| v.map(|v| format!("{k}={v}"))).collect();
        out.splice(at..at, new);
    };
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if inside {
                flush(&mut out, &mut pending);
            }
            inside = t == header;
            found |= inside;
            out.push(line.to_string());
            continue;
        }
        if inside && !t.starts_with('#') && !t.starts_with(';') {
            if let Some((k, _)) = t.split_once('=') {
                if let Some(i) = pending.iter().position(|(key, _)| *key == k.trim()) {
                    let (key, value) = pending.remove(i);
                    if let Some(v) = value {
                        out.push(format!("{key}={v}"));
                    }
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if inside {
        flush(&mut out, &mut pending);
    } else if !found && pending.iter().any(|(_, v)| v.is_some()) {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(header);
        flush(&mut out, &mut pending);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// The value of `key` in `[section]`, if set.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn ini_get(text: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t == header;
        } else if inside && !t.starts_with('#') && !t.starts_with(';') {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::sys;

    const WINLOGON: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
    /// Windows 11's "only Windows Hello sign-in" hides and blocks automatic
    /// sign-in while it is 2.
    const PASSWORDLESS: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\PasswordLess\Device";

    fn reg_get(key: &str, name: &str) -> Option<String> {
        let (ok, out, _) = sys::output("reg", &["query", key, "/v", name])?;
        if !ok {
            return None;
        }
        sys::decode(&out).lines().find_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix(name)?.trim_start();
            let (_, value) = rest.split_once("REG_")?;
            Some(value.split_once(char::is_whitespace).map(|(_, v)| v.trim()).unwrap_or("").to_string())
        })
    }

    fn reg_set(key: &str, name: &str, kind: &str, value: &str) -> Result<(), String> {
        match sys::output("reg", &["add", key, "/v", name, "/t", kind, "/d", value, "/f"]) {
            Some((true, _, _)) => Ok(()),
            Some((false, _, e)) => Err(format!("Could not set {name}: {}", sys::decode(&e).trim())),
            None => Err("Could not run reg.exe.".into()),
        }
    }

    fn reg_delete(key: &str, name: &str) {
        let _ = sys::output("reg", &["delete", key, "/v", name, "/f"]);
    }

    /// Our mark beside Winlogon's own values.
    const MARK: &str = "SentientAutoSignin";

    pub fn set_by_manager() -> bool {
        current().is_some() && reg_get(WINLOGON, MARK).is_some()
    }

    pub fn current() -> Option<String> {
        (reg_get(WINLOGON, "AutoAdminLogon")?.trim() == "1")
            .then(|| reg_get(WINLOGON, "DefaultUserName"))
            .flatten()
            .filter(|u| !u.is_empty())
    }

    /// (user name, domain) as Winlogon wants them: `DOMAIN\user`, a
    /// Microsoft account's e-mail address, or a local account.
    fn split(user: &str) -> (String, String) {
        if let Some((domain, name)) = user.split_once('\\') {
            (name.to_string(), domain.to_string())
        } else if user.contains('@') {
            (user.to_string(), "MicrosoftAccount".to_string())
        } else {
            (user.to_string(), std::env::var("COMPUTERNAME").unwrap_or_else(|_| ".".into()))
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Whether Windows accepts this account and password.
    fn check_password(user: &str, password: &str) -> Result<(), String> {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
        use windows_sys::Win32::Security::{LogonUserW, LOGON32_LOGON_INTERACTIVE, LOGON32_PROVIDER_DEFAULT};
        let (name, domain) = if let Some((d, n)) = user.split_once('\\') {
            (n.to_string(), Some(d.to_string()))
        } else if user.contains('@') {
            (user.to_string(), None)
        } else {
            (user.to_string(), Some(".".to_string()))
        };
        let name = wide(&name);
        let domain = domain.map(|d| wide(&d));
        let password = wide(password);
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that lives
        // across the call; the token is closed below.
        let ok = unsafe {
            LogonUserW(
                name.as_ptr(),
                domain.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                password.as_ptr(),
                LOGON32_LOGON_INTERACTIVE,
                LOGON32_PROVIDER_DEFAULT,
                &mut token,
            )
        };
        if ok != 0 {
            unsafe { CloseHandle(token) };
            return Ok(());
        }
        match unsafe { GetLastError() } {
            // A blank password is refused here by policy but accepted at the
            // console, which is where automatic sign-in happens.
            1327 if password.len() == 1 => Ok(()),
            1326 => Err("Windows did not accept that account and password. Check both and try again.".into()),
            1385 => Err("That account is not allowed to sign in on this computer.".into()),
            code => Err(format!("Windows refused the sign-in check (error {code}).")),
        }
    }

    /// Store (`Some`) or delete (`None`) the `DefaultPassword` LSA secret.
    fn store_secret(password: Option<&str>) -> Result<(), String> {
        use windows_sys::Win32::Security::Authentication::Identity::{
            LsaClose, LsaNtStatusToWinError, LsaOpenPolicy, LsaStorePrivateData, LSA_HANDLE, LSA_OBJECT_ATTRIBUTES,
            LSA_UNICODE_STRING, POLICY_CREATE_SECRET,
        };
        fn lsa_string(buf: &mut [u16]) -> LSA_UNICODE_STRING {
            let bytes = (buf.len() * 2) as u16;
            LSA_UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: buf.as_mut_ptr() }
        }
        let mut key: Vec<u16> = "DefaultPassword".encode_utf16().collect();
        let mut value: Vec<u16> = password.map(|p| p.encode_utf16().collect()).unwrap_or_default();
        let key_s = lsa_string(&mut key);
        let value_s = lsa_string(&mut value);
        // SAFETY: zeroed object attributes are what LsaOpenPolicy documents;
        // the string buffers outlive the calls and the handle is closed.
        unsafe {
            let attrs: LSA_OBJECT_ATTRIBUTES = std::mem::zeroed();
            let mut policy: LSA_HANDLE = 0;
            let status = LsaOpenPolicy(std::ptr::null(), &attrs, POLICY_CREATE_SECRET as u32, &mut policy);
            if status != 0 {
                return Err(format!(
                    "Could not open the Windows secret store (error {}).",
                    LsaNtStatusToWinError(status)
                ));
            }
            let data = if password.is_some() { &value_s as *const _ } else { std::ptr::null() };
            let status = LsaStorePrivateData(policy, &key_s, data);
            LsaClose(policy);
            // Deleting a secret that is not there is fine.
            if status != 0 && !(password.is_none() && LsaNtStatusToWinError(status) == 2) {
                return Err(format!(
                    "Could not save the password in the Windows secret store (error {}).",
                    LsaNtStatusToWinError(status)
                ));
            }
        }
        Ok(())
    }

    pub fn enable(sink: &ProgressFn, user: &str, password: &str) -> Result<(), String> {
        sink(Progress::Step { name: "Turning on automatic sign-in".into() });
        check_password(user, password)?;
        store_secret(Some(password))?;
        let (name, domain) = split(user);
        reg_set(WINLOGON, "DefaultUserName", "REG_SZ", &name)?;
        reg_set(WINLOGON, "DefaultDomainName", "REG_SZ", &domain)?;
        // A plain-text copy in the registry would win over the secret store.
        reg_delete(WINLOGON, "DefaultPassword");
        reg_delete(WINLOGON, "AutoLogonCount");
        reg_set(WINLOGON, "AutoAdminLogon", "REG_SZ", "1")?;
        reg_set(WINLOGON, MARK, "REG_SZ", "1")?;
        if reg_get(PASSWORDLESS, "DevicePasswordLessBuildVersion").is_some_and(|v| v != "0x0") {
            reg_set(PASSWORDLESS, "DevicePasswordLessBuildVersion", "REG_DWORD", "0")?;
            sink(Progress::Log {
                line: "Turned off \"only allow Windows Hello sign-in\", which blocks automatic sign-in.".into(),
            });
        }
        sink(Progress::Log { line: format!("Signs in as {domain}\\{name}; the password is in the Windows secret store.") });
        Ok(())
    }

    pub fn disable(sink: &ProgressFn) -> Result<(), String> {
        sink(Progress::Step { name: "Turning off automatic sign-in".into() });
        reg_set(WINLOGON, "AutoAdminLogon", "REG_SZ", "0")?;
        reg_delete(WINLOGON, "DefaultPassword");
        reg_delete(WINLOGON, MARK);
        store_secret(None)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use crate::exec::{run_privileged, shq};

    const LIGHTDM_DROPIN: &str = "/etc/lightdm/lightdm.conf.d/50-sentient-autologin.conf";
    const SDDM_DROPIN: &str = "/etc/sddm.conf.d/50-sentient-autologin.conf";
    /// Our mark (GDM's file is shared with other settings).
    const MARK: &str = "/var/lib/sentient-platform-manager/autologin";

    pub fn set_by_manager() -> bool {
        current().is_some() && std::path::Path::new(MARK).exists()
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Dm {
        Gdm,
        Lightdm,
        Sddm,
    }

    /// The display manager systemd starts, as its unit name.
    fn display_manager() -> Option<String> {
        let target = std::fs::read_link("/etc/systemd/system/display-manager.service").ok()?;
        Some(target.file_name()?.to_string_lossy().trim_end_matches(".service").to_string())
    }

    fn dm() -> Result<Dm, String> {
        match display_manager().as_deref() {
            Some("gdm") | Some("gdm3") => Ok(Dm::Gdm),
            Some("lightdm") => Ok(Dm::Lightdm),
            Some("sddm") => Ok(Dm::Sddm),
            Some(other) => Err(format!(
                "Automatic sign-in can be set here for GDM, LightDM and SDDM; this computer uses {other}. Turn it on in {other}'s own settings."
            )),
            None => Err("No graphical sign-in screen was found on this computer.".into()),
        }
    }

    fn gdm_conf() -> &'static str {
        ["/etc/gdm3/custom.conf", "/etc/gdm3/daemon.conf", "/etc/gdm/custom.conf"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .unwrap_or(if std::path::Path::new("/etc/gdm3").is_dir() { "/etc/gdm3/custom.conf" } else { "/etc/gdm/custom.conf" })
    }

    pub fn current() -> Option<String> {
        let read = |p: &str| std::fs::read_to_string(p).ok();
        let user = match dm().ok()? {
            Dm::Gdm => {
                let text = read(gdm_conf())?;
                ini_get(&text, "daemon", "AutomaticLoginEnable")
                    .filter(|v| v.eq_ignore_ascii_case("true") || v == "1")
                    .and_then(|_| ini_get(&text, "daemon", "AutomaticLogin"))
            }
            Dm::Lightdm => ini_get(&read(LIGHTDM_DROPIN)?, "Seat:*", "autologin-user"),
            Dm::Sddm => ini_get(&read(SDDM_DROPIN)?, "Autologin", "User"),
        };
        user.filter(|u| !u.is_empty())
    }

    /// The desktop session SDDM starts: Plasma when installed, else the first.
    fn sddm_session() -> Option<String> {
        let mut names: Vec<String> = ["/usr/share/wayland-sessions", "/usr/share/xsessions"]
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".desktop").map(str::to_string))
            .collect();
        names.sort();
        names.iter().find(|n| n.starts_with("plasma")).or(names.first()).cloned()
    }

    fn write_file(path: &str, text: &str) -> String {
        let dir = std::path::Path::new(path).parent().map(|d| d.display().to_string()).unwrap_or_default();
        format!("mkdir -p {}\ncat > {} <<'SENTIENTEOF'\n{text}SENTIENTEOF\nchmod 644 {}\n", shq(&dir), shq(path), shq(path))
    }

    pub fn enable(sink: &ProgressFn, user: &str, _password: &str) -> Result<(), String> {
        let dm = dm()?;
        match std::process::Command::new("id").args(["-u", user]).output() {
            Ok(o) if o.status.success() => {
                if String::from_utf8_lossy(&o.stdout).trim() == "0" {
                    return Err("Use an ordinary account for the operator screen, not root.".into());
                }
            }
            _ => return Err(format!("There is no account named {user} on this computer.")),
        }
        sink(Progress::Step { name: "Turning on automatic sign-in".into() });
        let script = match dm {
            Dm::Gdm => {
                let path = gdm_conf();
                let text = std::fs::read_to_string(path).unwrap_or_default();
                let new = ini_set(&text, "daemon", &[("AutomaticLoginEnable", Some("True")), ("AutomaticLogin", Some(user))]);
                write_file(path, &new)
            }
            Dm::Lightdm => {
                // Some distributions (Arch) only let members of "autologin"
                // sign in without a password.
                let conf = format!("[Seat:*]\nautologin-user={user}\nautologin-user-timeout=0\n");
                format!(
                    "{}getent group autologin >/dev/null || groupadd -r autologin\ngpasswd -a {} autologin >/dev/null\n",
                    write_file(LIGHTDM_DROPIN, &conf),
                    shq(user)
                )
            }
            Dm::Sddm => {
                let session = sddm_session().ok_or("No desktop session was found for SDDM to start.")?;
                write_file(SDDM_DROPIN, &format!("[Autologin]\nUser={user}\nSession={session}\nRelogin=false\n"))
            }
        };
        let script = format!("{script}mkdir -p /var/lib/sentient-platform-manager\necho {} > {MARK}\n", shq(user));
        run_privileged(sink, &script)?;
        let name = match dm { Dm::Gdm => "GDM", Dm::Lightdm => "LightDM", Dm::Sddm => "SDDM" };
        sink(Progress::Log { line: format!("{name} signs in as {user} after a restart.") });
        Ok(())
    }

    pub fn disable(sink: &ProgressFn) -> Result<(), String> {
        let dm = dm()?;
        sink(Progress::Step { name: "Turning off automatic sign-in".into() });
        let script = match dm {
            Dm::Gdm => {
                let path = gdm_conf();
                let Ok(text) = std::fs::read_to_string(path) else { return Ok(()) };
                write_file(path, &ini_set(&text, "daemon", &[("AutomaticLoginEnable", Some("False")), ("AutomaticLogin", None)]))
            }
            Dm::Lightdm => format!("rm -f {}\n", shq(LIGHTDM_DROPIN)),
            Dm::Sddm => format!("rm -f {}\n", shq(SDDM_DROPIN)),
        };
        run_privileged(sink, &format!("{script}rm -f {MARK}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_keys_in_existing_section() {
        let gdm = "# GDM configuration\n[daemon]\n# Uncomment the line below to force the login screen to use Xorg\n#WaylandEnable=false\n\n[security]\n";
        let out = ini_set(gdm, "daemon", &[("AutomaticLoginEnable", Some("True")), ("AutomaticLogin", Some("op"))]);
        assert_eq!(
            out,
            "# GDM configuration\n[daemon]\n# Uncomment the line below to force the login screen to use Xorg\n#WaylandEnable=false\nAutomaticLoginEnable=True\nAutomaticLogin=op\n\n[security]\n"
        );
        assert_eq!(ini_get(&out, "daemon", "AutomaticLogin").as_deref(), Some("op"));
        assert_eq!(ini_get(&out, "security", "AutomaticLogin"), None);
    }

    #[test]
    fn replaces_and_removes_keys() {
        let text = "[daemon]\nAutomaticLoginEnable=True\nAutomaticLogin=old\nWaylandEnable=false\n";
        let out = ini_set(text, "daemon", &[("AutomaticLoginEnable", Some("False")), ("AutomaticLogin", None)]);
        assert_eq!(out, "[daemon]\nAutomaticLoginEnable=False\nWaylandEnable=false\n");
    }

    #[test]
    fn adds_missing_section() {
        assert_eq!(ini_set("", "daemon", &[("AutomaticLogin", Some("op"))]), "[daemon]\nAutomaticLogin=op\n");
        assert_eq!(ini_set("[x]\na=1\n", "daemon", &[("k", Some("v"))]), "[x]\na=1\n\n[daemon]\nk=v\n");
        // Removing from a file without the section leaves it alone.
        assert_eq!(ini_set("[x]\na=1\n", "daemon", &[("k", None)]), "[x]\na=1\n");
    }
}
