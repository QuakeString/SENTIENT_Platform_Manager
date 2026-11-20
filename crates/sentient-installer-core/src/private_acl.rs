// Copyright © 2016-2026 The SENTIENT Authors

//! A secret's file or folder on Windows, readable only by SYSTEM, the
//! Administrators and this account: its inherited access removed (what
//! mode 0600 is on Linux). A file under ProgramData otherwise inherits
//! read access for every user of the computer.
//!
//! The groups by their well-known SIDs (their names differ by Windows
//! language), the account by its own name (`whoami`).

/// LocalSystem.
pub const SYSTEM_SID: &str = "*S-1-5-18";
/// BUILTIN\Administrators.
pub const ADMINISTRATORS_SID: &str = "*S-1-5-32-544";

/// icacls' arguments that leave `path` to SYSTEM, the Administrators and
/// `me` (the account itself: a process whose Administrators membership is
/// filtered still reads what it wrote). A folder's grants pass to what is
/// created in it.
pub fn icacls_args(path: &str, me: Option<&str>, folder: bool) -> Vec<String> {
    let rights = if folder { "(OI)(CI)F" } else { "F" };
    let mut a = vec![
        path.to_string(),
        "/inheritance:r".to_string(),
        "/grant:r".to_string(),
        format!("{SYSTEM_SID}:{rights}"),
        format!("{ADMINISTRATORS_SID}:{rights}"),
    ];
    if let Some(me) = me.map(str::trim).filter(|m| !m.is_empty() && !m.contains(':') && !m.starts_with('*')) {
        a.push(format!("{me}:{rights}"));
    }
    a.push("/Q".to_string());
    a
}

/// Restrict `path` as [`icacls_args`] says. Err when icacls did not.
#[cfg(windows)]
pub fn make_private(path: &std::path::Path, folder: bool) -> Result<(), String> {
    let me = crate::sys::output("whoami.exe", &[]).filter(|(ok, _, _)| *ok).map(|(_, out, _)| crate::sys::decode(&out).trim().to_string());
    let args = icacls_args(&path.to_string_lossy(), me.as_deref(), folder);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match crate::sys::output("icacls.exe", &refs) {
        Some((true, _, _)) => Ok(()),
        Some((false, out, err)) => Err(format!("restrict access to {}: {} {}", path.display(), crate::sys::decode(&err).trim(), crate::sys::decode(&out).trim())),
        None => Err("could not run icacls.exe".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_left_to_system_the_administrators_and_its_account() {
        let a = icacls_args(r"C:\ProgramData\SENTIENT\gateway-install.secret", Some("plant-w\\ops\r\n"), false);
        assert_eq!(
            a,
            [r"C:\ProgramData\SENTIENT\gateway-install.secret", "/inheritance:r", "/grant:r", "*S-1-5-18:F", "*S-1-5-32-544:F", "plant-w\\ops:F", "/Q"]
        );
    }

    #[test]
    fn a_folder_passes_its_grants_on() {
        let a = icacls_args(r"C:\Windows\SystemTemp\sentient-gateway-4.16.3", None, true);
        assert_eq!(a[3..], ["*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F", "/Q"]);
    }

    /// An odd answer from whoami adds no grant of its own making.
    #[test]
    fn no_odd_account_is_granted() {
        for me in ["", "  ", "*S-1-1-0", "Everyone:(F)"] {
            assert_eq!(icacls_args("x", Some(me), false).len(), 6, "{me:?}");
        }
    }
}
