// Copyright © 2016-2026 The SENTIENT Authors

//! The Microsoft Store build (MSIX). The same program as the GitHub
//! download, told apart at run time, never by a build switch:
//!
//! - **Packaged**: this process runs from the package (Windows gives it a
//!   package identity, or its program is under `C:\Program Files\WindowsApps`).
//!   It only hands over: the package folder is read-only, a process with
//!   the package's identity has its writes to the user's registry and
//!   AppData redirected into a private store that services, scheduled
//!   tasks and the RunOnce entry never see, and the program's path changes
//!   with every Store update.
//! - **Helper**: a copy of this program in `%ProgramData%\SENTIENT\Platform
//!   Manager\app`, a folder only SYSTEM and the Administrators may change
//!   (every user may read and run it). It runs outside the package, so it
//!   works exactly as the downloaded Platform Manager does: the window
//!   (elevated with the normal UAC prompt), the tray, services, scheduled
//!   tasks, the operator screen and the RunOnce entry that continues Setup
//!   after the WSL restart all start this one, at a path that never changes.
//! - **Staged**: a packaged start whose helper is missing or older copies
//!   itself to a staging folder in the user's own profile (no other user
//!   can change it there; ProgramData would let any user swap the file
//!   before it runs as administrator) and starts that copy as administrator
//!   (`--store-stage`). The staged copy puts itself in the helper folder,
//!   starts the helper with the original arguments and ends.
//!
//! In the Store build the Platform Manager does not update itself (the Store
//! does), does not install itself into Program Files, has no Apps & features
//! entry or Start menu shortcut of its own (the package has them), and its
//! "Remove the Platform Manager" removes only what it put on the computer
//! outside the package; the app itself is uninstalled from Start or
//! Settings > Apps.

#![cfg_attr(not(all(windows, feature = "gui")), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The app execution alias the package declares (AppxManifest.xml): the
/// packaged program by name, from a command prompt or the Run box.
#[allow(dead_code)]
pub const ALIAS: &str = "sentient-platform-manager.exe";

/// The program's name inside the package (AppxManifest.xml).
#[allow(dead_code)]
pub const PACKAGED_EXE: &str = "sentient-platform-manager.exe";

/// The helper's file name: the same as the downloaded Platform Manager's,
/// so the replace-and-restart code (crate::self_install) treats it alike.
pub const HELPER_EXE: &str = "SENTIENT Platform Manager.exe";

/// The staged copy's name: a different image name, so the helper's
/// replacement (which ends every running "SENTIENT Platform Manager.exe")
/// does not end the copy doing it.
pub const STAGE_EXE: &str = "spm-store-stage.exe";

/// The staged copy's first argument.
pub const STAGE_ARG: &str = "--store-stage";

/// The file beside the helper that names the build it is.
pub const MARKER: &str = "helper-build.txt";

/// The helper's folder under `program_data`.
pub fn helper_dir_in(program_data: &Path) -> PathBuf {
    program_data.join("SENTIENT").join("Platform Manager").join("app")
}

/// The staging folder in the user's profile `home`: only this user, SYSTEM
/// and the Administrators may change it, and writes there are not
/// redirected for a packaged app (only AppData is).
pub fn staging_dir_in(home: &Path) -> PathBuf {
    home.join(".sentient-platform-manager").join("store-staging")
}

/// `%ProgramData%`.
pub fn program_data() -> PathBuf {
    std::env::var_os("ProgramData").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

pub fn helper_dir() -> PathBuf {
    helper_dir_in(&program_data())
}

pub fn helper_exe() -> PathBuf {
    helper_dir().join(HELPER_EXE)
}

pub fn staging_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(std::env::temp_dir);
    staging_dir_in(&home)
}

/// Windows paths compared as Windows does: case and slash direction ignored.
fn norm(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// `path` is `dir` or inside it.
pub fn is_under(path: &str, dir: &str) -> bool {
    let (p, d) = (norm(path), norm(dir));
    !d.is_empty() && (p == d || p.starts_with(&format!("{d}\\")))
}

/// The program at `exe` is a package's (an MSIX install location). Packages
/// live under a `WindowsApps` folder on every package volume
/// (`C:\Program Files\WindowsApps`, `D:\WindowsApps`).
pub fn path_in_package(exe: &str) -> bool {
    norm(exe).split('\\').any(|part| part == "windowsapps")
}

/// Which build this run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// The GitHub download (or a development build).
    Direct,
    /// The Store build, running from the package.
    Packaged,
    /// The Store build's helper copy.
    Helper,
    /// The Store build's staged copy (`spm-store-stage.exe --store-stage`).
    Staged,
}

impl Flavour {
    pub fn is_store(self) -> bool {
        self != Flavour::Direct
    }
}

/// The flavour of a program at `exe`, with or without a package identity,
/// started with `first_arg`. The staged copy is known by its name and
/// argument, not its folder: an administrator who is another account (a
/// standard user typing an administrator's password) has another profile.
pub fn flavour_of(exe: &str, has_identity: bool, program_data: &Path, first_arg: Option<&str>) -> Flavour {
    let name = norm(exe).rsplit('\\').next().unwrap_or_default().to_string();
    if has_identity || path_in_package(exe) {
        Flavour::Packaged
    } else if is_under(exe, &helper_dir_in(program_data).to_string_lossy()) {
        Flavour::Helper
    } else if first_arg == Some(STAGE_ARG) && name == STAGE_EXE {
        Flavour::Staged
    } else {
        Flavour::Direct
    }
}

/// This run's flavour (decided once).
pub fn flavour() -> Flavour {
    static F: std::sync::OnceLock<Flavour> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        // Screens and tests of the Store build's pages on any computer.
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_STORE").is_some() {
            return Flavour::Helper;
        }
        if !cfg!(windows) {
            return Flavour::Direct;
        }
        let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_default();
        flavour_of(&exe, package_full_name().is_some(), &program_data(), std::env::args().nth(1).as_deref())
    })
}

/// The Store build, whichever copy of it this is.
pub fn is_store() -> bool {
    flavour().is_store()
}

/// The helper's marker: which build the helper is.
pub fn marker_text(version: &str, commit: &str, len: u64) -> String {
    format!("{version} {commit} {len}\n")
}

/// The helper with `marker` is this build (`version`, `commit`, its
/// program `len` bytes): nothing to copy.
pub fn helper_current(marker: Option<&str>, helper_len: Option<u64>, version: &str, commit: &str, len: u64) -> bool {
    marker.is_some_and(|m| m == marker_text(version, commit, len)) && helper_len == Some(len)
}

/// What a packaged start does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandOver {
    /// Give the request to the copy already running (the tray or the
    /// window); no UAC prompt.
    Forward,
    /// Start the helper as administrator with this start's arguments.
    Helper,
    /// Copy this build to the staging folder and start that copy as
    /// administrator: it replaces the helper first.
    Stage,
}

/// `running`: a copy runs (crate::background::running), and whether it is
/// elevated. `current`: the helper is this build.
pub fn hand_over_plan(running: Option<bool>, current: bool) -> HandOver {
    match (running, current) {
        (Some(_), true) => HandOver::Forward,
        // An elevated window may be in the middle of a job: replacing the
        // helper would end it. It takes this request; the helper is
        // brought up to date at a later start.
        (Some(true), false) => HandOver::Forward,
        // Only the tray runs: replacing the helper ends it, and the elevated
        // window that follows takes its place.
        (Some(false), false) => HandOver::Stage,
        (None, true) => HandOver::Helper,
        (None, false) => HandOver::Stage,
    }
}

// ---------------------------------------------------------------------------
// Package metadata (scripts/package-msix.ps1 through --job msix-manifest)
// ---------------------------------------------------------------------------

/// The package version from the program's: MSIX needs four numbers, each
/// 0 to 65535, and the Store keeps the fourth for itself (it must be 0).
/// Release candidates and nightlies are not offered in the Store.
pub fn msix_version(version: &str) -> Result<String, String> {
    let v = version.trim().trim_start_matches('v');
    if v.contains('-') || v.contains('+') {
        return Err(format!("{version} is a pre-release: only final releases (like 0.9.4) go to the Microsoft Store."));
    }
    let parts: Vec<&str> = v.split('.').collect();
    if !(3..=4).contains(&parts.len()) {
        return Err(format!("{version} is not a version of three numbers (like 0.9.4)."));
    }
    let mut nums = Vec::new();
    for p in &parts {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("{version} is not a version of numbers only (like 0.9.4)."));
        }
        let n: u32 = p.parse().map_err(|_| format!("{version}: {p} is too large."))?;
        if n > 65535 {
            return Err(format!("{version}: {p} is larger than 65535, the largest number a package version may have."));
        }
        nums.push(n);
    }
    if nums.len() == 4 && nums[3] != 0 {
        return Err(format!("{version}: the fourth number must be 0 (the Microsoft Store keeps it for itself)."));
    }
    nums.truncate(3);
    if nums[0] == 0 && nums[1] == 0 && nums[2] == 0 {
        return Err("0.0.0 is not a package version.".into());
    }
    Ok(format!("{}.{}.{}.0", nums[0], nums[1], nums[2]))
}

/// The package identity's name as Partner Center gives it (letters,
/// digits, dots and hyphens, 3 to 50 characters).
pub fn check_identity_name(name: &str) -> Result<(), String> {
    let ok = (3..=50).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if ok { Ok(()) } else { Err(format!("\"{name}\" is not a package name (3 to 50 letters, digits, dots or hyphens, as Partner Center shows it).")) }
}

/// The publisher: a certificate subject with a common name. Partner Center
/// gives `CN=…`; a sideload test package carries its certificate's subject
/// as Windows writes it (`C=IN, O=INVENIA SYSTEMS, CN=INVENIA SYSTEMS`).
pub fn check_publisher(publisher: &str) -> Result<(), String> {
    let has_cn = publisher.split(',').any(|part| part.trim_start().starts_with("CN=") && part.trim().len() > 3);
    if has_cn && publisher.len() <= 8192 {
        Ok(())
    } else {
        Err(format!("\"{publisher}\" is not a publisher: it needs a CN= part (Partner Center, Product identity, Package/Identity/Publisher)."))
    }
}

/// Text made safe inside an XML attribute or element.
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The manifest template with every `{{NAME}}` replaced by its value (XML
/// escaped). A placeholder without a value, or a value no placeholder
/// uses, is an error: a misspelt name would otherwise reach the package.
pub fn fill_manifest(template: &str, values: &[(&str, &str)]) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut used = vec![false; values.len()];
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or("A {{ without its }} in the manifest template.")?;
        let name = after[..end].trim();
        match values.iter().position(|(k, _)| *k == name) {
            Some(i) => {
                out.push_str(&xml_escape(values[i].1));
                used[i] = true;
            }
            None => return Err(format!("The manifest template has {{{{{name}}}}}, which has no value.")),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    if let Some(i) = used.iter().position(|u| !u) {
        return Err(format!("No {{{{{}}}}} in the manifest template.", values[i].0));
    }
    Ok(out)
}

/// `--job msix-manifest --template <file> --out <file> --version <v>
/// --identity-name <name> --publisher <CN=…> --publisher-display-name <name>`:
/// the package's AppxManifest.xml (scripts/package-msix.ps1).
pub fn manifest_job(args: &[String]) -> Result<String, String> {
    let get = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone()).ok_or(format!("{flag} is missing"));
    let template = std::fs::read_to_string(get("--template")?).map_err(|e| format!("read the template: {e}"))?;
    let version = msix_version(&get("--version")?)?;
    let name = get("--identity-name")?;
    check_identity_name(&name)?;
    let publisher = get("--publisher")?;
    check_publisher(&publisher)?;
    let display = get("--publisher-display-name")?;
    let filled = fill_manifest(
        &template,
        &[
            ("IDENTITY_NAME", &name),
            ("PUBLISHER", &publisher),
            ("PUBLISHER_DISPLAY_NAME", &display),
            ("VERSION", &version),
        ],
    )?;
    let out = get("--out")?;
    std::fs::write(&out, filled).map_err(|e| format!("write {out}: {e}"))?;
    Ok(version)
}

// ---------------------------------------------------------------------------
// Windows: identity, the hand-over and the helper
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentPackageFullName(len: *mut u32, name: *mut u16) -> i32;
        fn GetCurrentPackageFamilyName(len: *mut u32, name: *mut u16) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    pub fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// GetCurrentPackageFullName or GetCurrentPackageFamilyName: the name, or
    /// None without a package identity (APPMODEL_ERROR_NO_PACKAGE).
    fn ask(f: unsafe extern "system" fn(*mut u32, *mut u16) -> i32) -> Option<String> {
        let mut len: u32 = 0;
        // SAFETY: a zero length with a null buffer asks for the length
        // (ERROR_INSUFFICIENT_BUFFER, 122); without a package the call
        // answers APPMODEL_ERROR_NO_PACKAGE (15700).
        let first = unsafe { f(&mut len, std::ptr::null_mut()) };
        if first != 122 || len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds `len` UTF-16 units, as the call asked for.
        let r = unsafe { f(&mut len, buf.as_mut_ptr()) };
        if r != 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    pub fn package_full_name() -> Option<String> {
        ask(GetCurrentPackageFullName)
    }

    pub fn package_family_name() -> Option<String> {
        ask(GetCurrentPackageFamilyName)
    }

    /// A message box: the packaged start has no window of its own.
    pub fn tell(text: &str) {
        let t = wide(std::ffi::OsStr::new(text));
        let c = wide(std::ffi::OsStr::new("SENTIENT Platform Manager"));
        // SAFETY: both NUL-terminated buffers live until the call returns;
        // MB_OK | MB_ICONERROR.
        unsafe {
            MessageBoxW(0, t.as_ptr(), c.as_ptr(), 0x10);
        }
    }
}

#[cfg(windows)]
pub use win::{package_family_name, package_full_name};

#[cfg(not(windows))]
pub fn package_full_name() -> Option<String> {
    None
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn package_family_name() -> Option<String> {
    None
}

fn this_len() -> u64 {
    std::env::current_exe().ok().and_then(|e| std::fs::metadata(e).ok()).map(|m| m.len()).unwrap_or(0)
}

/// The helper is this build.
pub fn helper_is_current() -> bool {
    let marker = std::fs::read_to_string(helper_dir().join(MARKER)).ok();
    let helper_len = std::fs::metadata(helper_exe()).ok().map(|m| m.len());
    helper_current(marker.as_deref(), helper_len, crate::updates::manager_version(), crate::updates::build_commit(), this_len())
}

/// A start from the package: hand over to the helper (or to the copy that
/// already runs) and end. Only file copies, the loopback hand-over and the
/// UAC prompt happen here: no command prompt or PowerShell is started from
/// inside the package.
#[cfg(windows)]
pub fn hand_over(args: &[String]) {
    let request = crate::background::request_of(args);
    let running = crate::background::running();
    let plan = hand_over_plan(running, helper_is_current());
    if plan == HandOver::Forward && crate::background::forward(&request) {
        return;
    }
    // Forward failed (a stale lock): start the helper, or stage when it is
    // not this build and nothing elevated runs.
    let stage = match plan {
        HandOver::Stage => true,
        HandOver::Forward => !helper_is_current() && running != Some(true),
        HandOver::Helper => false,
    };
    let (exe, mut argv) = if stage {
        match stage_copy() {
            Ok(staged) => (staged, vec![STAGE_ARG.to_string()]),
            Err(e) => {
                win::tell(&format!("The Platform Manager could not prepare its administrator copy: {e}"));
                return;
            }
        }
    } else if helper_exe().is_file() {
        (helper_exe(), Vec::new())
    } else {
        win::tell("The Platform Manager's administrator copy is missing. Start the Platform Manager again.");
        return;
    };
    argv.extend(args.iter().cloned());
    // Declined at the UAC prompt: nothing happens, as for the downloaded
    // Platform Manager.
    let _ = crate::elevate::runas(&exe, &argv);
}

/// Copy this program to a new folder under the staging folder.
#[cfg(windows)]
fn stage_copy() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| format!("locate this program: {e}"))?;
    // Earlier staged copies (each in its own folder; one still running is
    // left for the next time).
    if let Ok(entries) = std::fs::read_dir(staging_dir()) {
        for e in entries.flatten() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let dir = staging_dir().join(format!("{}-{}", std::process::id(), chrono::Local::now().format("%Y%m%d%H%M%S")));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let to = dir.join(STAGE_EXE);
    std::fs::copy(&me, &to).map_err(|e| format!("copy to {}: {e}", to.display()))?;
    Ok(to)
}

/// `--store-stage <args…>`, run as administrator from the staging folder:
/// put this build in the helper folder, start the helper with `args` (it
/// inherits this process's administrator rights) and end.
#[cfg(windows)]
pub fn finish_stage(args: &[String]) -> i32 {
    if !crate::elevate::is_admin() {
        win::tell("The Platform Manager's administrator copy needs administrator rights.");
        return 2;
    }
    match crate::self_install::install_manager() {
        Ok(helper) => match crate::self_install::manager_command(&helper).args(args).spawn() {
            Ok(_) => 0,
            Err(e) => {
                win::tell(&format!("The Platform Manager could not start ({}): {e}", helper.display()));
                1
            }
        },
        Err(e) => {
            win::tell(&format!("The Platform Manager could not put its administrator copy in place: {e}"));
            1
        }
    }
}

/// The helper folder made SYSTEM's and the Administrators' to change, every
/// user's to read and run (the ClickHouse account and the operator screen
/// run the helper too). ProgramData lets every user create folders, and a
/// service runs this program as LocalSystem, so a folder someone else made
/// first is taken over completely: not a link to elsewhere, owned by the
/// Administrators, its inherited access gone, and nothing in it but the
/// helper and its marker (a planted DLL beside it would load into it).
#[cfg(windows)]
pub fn lock_helper_dir(dir: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    for d in [Some(dir), dir.parent()].into_iter().flatten() {
        if std::fs::symlink_metadata(d).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a link to another folder; remove it and start the Platform Manager again", d.display()));
        }
    }
    let icacls = |args: Vec<String>| -> Result<(), String> {
        let out = std::process::Command::new("icacls.exe")
            .args(args)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .map_err(|e| format!("could not run icacls.exe: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "restrict access to {}: {} {}",
                dir.display(),
                String::from_utf8_lossy(&out.stderr).trim(),
                String::from_utf8_lossy(&out.stdout).trim()
            ))
        }
    };
    icacls(helper_owner_args(&dir.to_string_lossy()))?;
    icacls(helper_acl_args(&dir.to_string_lossy()))?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if !helper_dir_keeps(&e.file_name().to_string_lossy()) {
                let p = e.path();
                let gone = if p.is_dir() && !e.file_type().is_ok_and(|t| t.is_symlink()) { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p).or_else(|_| std::fs::remove_dir(&p)) };
                gone.map_err(|err| format!("remove {} from the helper folder: {err}", p.display()))?;
            }
        }
    }
    Ok(())
}

/// What the helper folder may hold: the helper, the copy set aside while it
/// is replaced, and the marker.
pub fn helper_dir_keeps(name: &str) -> bool {
    let n = name.to_lowercase();
    n == HELPER_EXE.to_lowercase() || n == format!("{}.old", HELPER_EXE.to_lowercase()) || n == MARKER.to_lowercase()
}

/// icacls' arguments that make the Administrators own the helper folder
/// and everything in it.
pub fn helper_owner_args(dir: &str) -> Vec<String> {
    vec![dir.to_string(), "/setowner".into(), sentient_installer_core::private_acl::ADMINISTRATORS_SID.into(), "/T".into(), "/C".into(), "/Q".into()]
}

/// icacls' arguments for [`lock_helper_dir`] (well-known SIDs: the group
/// names differ by Windows language).
pub fn helper_acl_args(dir: &str) -> Vec<String> {
    use sentient_installer_core::private_acl::{ADMINISTRATORS_SID, SYSTEM_SID};
    vec![
        dir.to_string(),
        "/inheritance:r".into(),
        "/grant:r".into(),
        format!("{SYSTEM_SID}:(OI)(CI)F"),
        format!("{ADMINISTRATORS_SID}:(OI)(CI)F"),
        // BUILTIN\Users: read and run.
        "*S-1-5-32-545:(OI)(CI)RX".into(),
        "/T".into(),
        "/Q".into(),
    ]
}

/// After the helper is in place: its marker, and the staging folder's
/// copies that have ended.
pub fn after_helper_installed(helper: &Path) {
    let len = std::fs::metadata(helper).map(|m| m.len()).unwrap_or(0);
    if let Some(dir) = helper.parent() {
        let _ = std::fs::write(dir.join(MARKER), marker_text(crate::updates::manager_version(), crate::updates::build_commit(), len));
    }
}

/// Staged copies left behind (the helper removes them once they ended).
pub fn clean_staging() {
    if flavour() != Flavour::Helper {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(staging_dir()) {
        for e in entries.flatten() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

// ---------------------------------------------------------------------------
// --job package-identity: what Windows does with this process's writes
// ---------------------------------------------------------------------------

/// `--job package-identity [<label>]`: whether this process has a package
/// identity, and whether its writes to ProgramData, HKLM and HKCU reach
/// the real places or a package's private copy. Each run first shows what
/// the run before it left (from inside the package, or from the helper),
/// so two runs side by side tell whether the other one's writes landed.
/// The report goes to standard output and to
/// `%ProgramData%\SENTIENT\msix-probe-report-<pid>.txt`.
pub fn probe_job(args: &[String]) -> i32 {
    let label = args.get(1).cloned().unwrap_or_else(|| format!("{:?}", flavour()).to_lowercase());
    let mut r = Vec::<String>::new();
    let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_default();
    r.push(format!("label: {label}"));
    r.push(format!("program: {exe}"));
    r.push(format!("flavour: {:?}", flavour()));
    r.push(format!("version: {} ({})", crate::updates::manager_version(), crate::updates::build_commit()));
    r.push(format!("administrator: {}", crate::elevate::is_admin()));
    r.push(format!("package full name: {}", package_full_name().unwrap_or_else(|| "none (no package identity)".into())));
    r.push(format!("package family name: {}", package_family_name().unwrap_or_else(|| "none".into())));
    r.push(format!("in a WindowsApps folder: {}", path_in_package(&exe)));
    let stamp = format!("{label} pid={} at {}", std::process::id(), chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    probe_platform(&mut r, &stamp);
    let text = r.join("\n") + "\n";
    print!("{text}");
    let report = program_data().join("SENTIENT").join(format!("msix-probe-report-{}.txt", std::process::id()));
    if let Some(d) = report.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    match std::fs::write(&report, &text) {
        Ok(()) => println!("report: {}", report.display()),
        Err(e) => println!("report not written ({}): {e}", report.display()),
    }
    0
}

#[cfg(not(windows))]
fn probe_platform(r: &mut Vec<String>, _stamp: &str) {
    r.push("Windows only: nothing else to check here.".into());
}

#[cfg(windows)]
fn probe_platform(r: &mut Vec<String>, stamp: &str) {
    use std::os::windows::io::AsRawHandle;
    // The file.
    let file = program_data().join("SENTIENT").join("msix-probe.txt");
    r.push(format!("file: {}", file.display()));
    r.push(format!("  before: {}", std::fs::read_to_string(&file).map(|t| t.trim().to_string()).unwrap_or_else(|e| format!("not readable ({e})"))));
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
    match std::fs::write(&file, format!("{stamp}\n")) {
        Ok(()) => r.push("  written".into()),
        Err(e) => r.push(format!("  write failed: {e}")),
    }
    r.push(format!("  read back: {}", std::fs::read_to_string(&file).map(|t| t.trim().to_string()).unwrap_or_else(|e| format!("not readable ({e})"))));
    if let Ok(f) = std::fs::File::open(&file) {
        let real = reg::final_path(f.as_raw_handle() as isize).unwrap_or_else(|| "unknown".into());
        let redirected = real.to_lowercase().contains("\\packages\\") || real.to_lowercase().contains("\\localcache\\");
        r.push(format!("  really at: {real}{}", if redirected { "  (REDIRECTED into the package's private folder)" } else { "" }));
    }
    // The package's private folders, when there is a package family.
    if let (Some(pfn), Some(local)) = (package_family_name(), std::env::var_os("LOCALAPPDATA")) {
        let base = PathBuf::from(local).join("Packages").join(&pfn);
        for sub in ["LocalCache", "SystemAppData\\Helium"] {
            let p = base.join(sub);
            r.push(format!("private {sub}: {} ({})", p.display(), if p.exists() { "exists" } else { "absent" }));
        }
        let hits = find_named(&base.join("LocalCache"), "msix-probe.txt", 8);
        r.push(format!("  msix-probe.txt copies under LocalCache: {}", if hits.is_empty() { "none".into() } else { hits.join("; ") }));
    }
    // The registry.
    for (hive, name) in [(reg::HKLM, "HKLM"), (reg::HKCU, "HKCU")] {
        let key = r"SOFTWARE\SENTIENT";
        r.push(format!("registry: {name}\\{key} MsixProbe"));
        r.push(format!("  before: {}", reg::get(hive, key, "MsixProbe").unwrap_or_else(|e| format!("not readable ({e})"))));
        match reg::set(hive, key, "MsixProbe", stamp) {
            Ok(real) => {
                r.push("  written".into());
                let private = real.to_uppercase().starts_with(r"\REGISTRY\A\");
                r.push(format!("  key really at: {real}{}", if private { "  (VIRTUALISED: a private hive)" } else { "" }));
            }
            Err(e) => r.push(format!("  write failed: {e}")),
        }
        r.push(format!("  read back: {}", reg::get(hive, key, "MsixProbe").unwrap_or_else(|e| format!("not readable ({e})"))));
    }
}

#[cfg(windows)]
fn find_named(dir: &Path, name: &str, depth: u32) -> Vec<String> {
    let mut out = Vec::new();
    if depth == 0 {
        return out;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(find_named(&p, name, depth - 1));
            } else if p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name)) {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

/// The registry and handle calls the probe needs, straight from the
/// Windows API (no reg.exe from inside the package).
#[cfg(windows)]
mod reg {
    use super::win::wide;
    use std::ffi::OsStr;

    pub const HKLM: isize = 0x8000_0002u32 as i32 as isize;
    pub const HKCU: isize = 0x8000_0001u32 as i32 as isize;
    const KEY_READ_WRITE: u32 = 0x2001F;
    const KEY_WOW64_64KEY: u32 = 0x0100;
    const REG_SZ: u32 = 1;
    const RRF_RT_REG_SZ: u32 = 0x2;
    const RRF_SUBKEY_WOW6464KEY: u32 = 0x10000;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegCreateKeyExW(key: isize, sub: *const u16, reserved: u32, class: *mut u16, options: u32, sam: u32, sa: *const core::ffi::c_void, result: *mut isize, disposition: *mut u32) -> i32;
        fn RegSetValueExW(key: isize, name: *const u16, reserved: u32, kind: u32, data: *const u8, len: u32) -> i32;
        fn RegGetValueW(key: isize, sub: *const u16, name: *const u16, flags: u32, kind: *mut u32, data: *mut core::ffi::c_void, len: *mut u32) -> i32;
        fn RegCloseKey(key: isize) -> i32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryKey(key: isize, class: u32, info: *mut core::ffi::c_void, len: u32, result: *mut u32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFinalPathNameByHandleW(file: isize, buf: *mut u16, len: u32, flags: u32) -> u32;
    }

    fn err(code: i32) -> String {
        std::io::Error::from_raw_os_error(code).to_string()
    }

    /// Write `value`; the key's real name in the kernel's registry (a
    /// private hive shows as \REGISTRY\A\…).
    pub fn set(hive: isize, key: &str, name: &str, value: &str) -> Result<String, String> {
        let k = wide(OsStr::new(key));
        let mut h: isize = 0;
        // SAFETY: NUL-terminated key name; `h` receives the opened key.
        let rc = unsafe { RegCreateKeyExW(hive, k.as_ptr(), 0, std::ptr::null_mut(), 0, KEY_READ_WRITE | KEY_WOW64_64KEY, std::ptr::null(), &mut h, std::ptr::null_mut()) };
        if rc != 0 {
            return Err(err(rc));
        }
        let n = wide(OsStr::new(name));
        let v = wide(OsStr::new(value));
        // SAFETY: `v` is the value with its NUL, its length in bytes.
        let rc = unsafe { RegSetValueExW(h, n.as_ptr(), 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32) };
        let real = key_name(h).unwrap_or_else(|| "unknown".into());
        // SAFETY: `h` was opened above.
        unsafe { RegCloseKey(h) };
        if rc != 0 { Err(err(rc)) } else { Ok(real) }
    }

    pub fn get(hive: isize, key: &str, name: &str) -> Result<String, String> {
        let k = wide(OsStr::new(key));
        let n = wide(OsStr::new(name));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: `buf` is `len` bytes long.
        let rc = unsafe { RegGetValueW(hive, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _, &mut len) };
        if rc != 0 {
            return Err(err(rc));
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(String::from_utf16_lossy(&buf[..end]))
    }

    /// KeyNameInformation (3): the key's full kernel name.
    fn key_name(h: isize) -> Option<String> {
        let mut buf = vec![0u8; 4096];
        let mut got: u32 = 0;
        // SAFETY: `buf` is large enough for KEY_NAME_INFORMATION
        // { ULONG NameLength; WCHAR Name[] } of any key name.
        let st = unsafe { NtQueryKey(h, 3, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut got) };
        if st < 0 || got < 4 {
            return None;
        }
        let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let units: Vec<u16> = buf[4..(4 + n).min(buf.len())].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// The file's real path (after any redirection).
    pub fn final_path(file: isize) -> Option<String> {
        let mut buf = vec![0u16; 2048];
        // SAFETY: `buf` holds `buf.len()` UTF-16 units; flags 0 = the
        // normalised DOS path.
        let n = unsafe { GetFinalPathNameByHandleW(file, buf.as_mut_ptr(), buf.len() as u32, 0) } as usize;
        (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_version_has_four_numbers_and_the_last_is_zero() {
        assert_eq!(msix_version("0.9.4").as_deref(), Ok("0.9.4.0"));
        assert_eq!(msix_version("v1.2.3").as_deref(), Ok("1.2.3.0"));
        assert_eq!(msix_version("1.2.3.0").as_deref(), Ok("1.2.3.0"));
        assert_eq!(msix_version(" 10.0.65535 ").as_deref(), Ok("10.0.65535.0"));
    }

    #[test]
    fn pre_releases_and_odd_versions_are_refused() {
        for v in ["0.9.4-rc1", "0.9.5-nightly.20261120", "0.9.4+build", "0.9", "0.9.4.1", "0.9.x", "0.9.70000", "1.2.3.4.5", "", "0.0.0", "0..4"] {
            assert!(msix_version(v).is_err(), "{v}");
        }
        assert!(msix_version("0.9.4-rc1").unwrap_err().contains("pre-release"));
    }

    #[test]
    fn the_manifest_is_filled_and_escaped() {
        let t = r#"<Identity Name="{{IDENTITY_NAME}}" Publisher="{{ PUBLISHER }}" Version="{{VERSION}}"/>"#;
        let out = fill_manifest(t, &[("IDENTITY_NAME", "INVENIA.SPM"), ("PUBLISHER", "CN=A & B, O=\"X\""), ("VERSION", "0.9.4.0")]).unwrap();
        assert_eq!(out, r#"<Identity Name="INVENIA.SPM" Publisher="CN=A &amp; B, O=&quot;X&quot;" Version="0.9.4.0"/>"#);
    }

    #[test]
    fn a_missing_or_unused_placeholder_is_an_error() {
        assert!(fill_manifest("{{NAME}} {{OTHER}}", &[("NAME", "x")]).unwrap_err().contains("OTHER"));
        assert!(fill_manifest("{{NAME}}", &[("NAME", "x"), ("EXTRA", "y")]).unwrap_err().contains("EXTRA"));
        assert!(fill_manifest("{{NAME", &[("NAME", "x")]).is_err());
        assert_eq!(fill_manifest("no placeholders", &[]).unwrap(), "no placeholders");
    }

    #[test]
    fn the_shipped_template_fills_completely() {
        let t = include_str!("../../packaging/msix/AppxManifest.xml");
        let out = fill_manifest(
            t,
            &[("IDENTITY_NAME", "INVENIASYSTEMS.SENTIENTPlatformManager"), ("PUBLISHER", "CN=INVENIA SYSTEMS, O=INVENIA SYSTEMS, C=IN"), ("PUBLISHER_DISPLAY_NAME", "INVENIA SYSTEMS"), ("VERSION", "0.9.4.0")],
        )
        .unwrap();
        assert!(!out.contains("{{"));
        assert!(out.contains(r#"Version="0.9.4.0""#));
        // The one capability the Store build declares.
        assert!(out.contains(r#"<rescap:Capability Name="runFullTrust"/>"#));
        for refused in ["allowElevation", "packagedServices", "localSystemServices", "unvirtualizedResources"] {
            assert!(!out.contains(refused), "{refused}");
        }
        assert!(out.contains(r#"MinVersion="10.0.19041.0""#));
        assert!(out.contains(&format!(r#"Alias="{ALIAS}""#)));
        assert!(out.contains(&format!(r#"Executable="{PACKAGED_EXE}""#)));
        assert_eq!(out.matches("<rescap:Capability ").count(), 1);
    }

    #[test]
    fn identity_values_are_checked() {
        assert!(check_identity_name("INVENIASYSTEMS.SENTIENTPlatformManager").is_ok());
        assert!(check_identity_name("a b").is_err());
        assert!(check_identity_name("ab").is_err());
        assert!(check_publisher("CN=1A2B3C4D-0000-1111-2222-333344445555").is_ok());
        assert!(check_publisher("INVENIA").is_err());
        // A sideload test package: the certificate subject as Windows writes it.
        assert!(check_publisher("C=IN, O=INVENIA SYSTEMS, CN=INVENIA SYSTEMS").is_ok());
        assert!(check_publisher("O=INVENIA SYSTEMS, C=IN").is_err());
        assert!(check_publisher("O=X, CN=").is_err());
    }

    #[test]
    fn a_program_under_windowsapps_is_packaged() {
        assert!(path_in_package(r"C:\Program Files\WindowsApps\INVENIA.SPM_0.9.4.0_x64__abc\SENTIENT Platform Manager.exe"));
        assert!(path_in_package(r"D:\WindowsApps\INVENIA.SPM_0.9.4.0_x64__abc\SENTIENT Platform Manager.exe"));
        assert!(!path_in_package(r"C:\Program Files\SENTIENT Platform Manager\SENTIENT Platform Manager.exe"));
        assert!(!path_in_package(r"C:\Users\op\Downloads\MyWindowsAppsTool\x.exe"));
    }

    #[test]
    fn each_copy_knows_its_flavour() {
        let pd = Path::new(r"C:\ProgramData");
        assert_eq!(flavour_of(r"C:\Program Files\WindowsApps\P_0.9.4.0_x64__h\sentient-platform-manager.exe", false, pd, None), Flavour::Packaged);
        assert_eq!(flavour_of(r"C:\somewhere\x.exe", true, pd, None), Flavour::Packaged);
        assert_eq!(flavour_of(r"c:\programdata\sentient\platform manager\app\SENTIENT Platform Manager.exe", false, pd, Some("--resume")), Flavour::Helper);
        let staged = r"C:\Users\op\.sentient-platform-manager\store-staging\12-2026\spm-store-stage.exe";
        assert_eq!(flavour_of(staged, false, pd, Some(STAGE_ARG)), Flavour::Staged);
        // The name alone, or the argument alone, is not the staged copy.
        assert_eq!(flavour_of(staged, false, pd, None), Flavour::Direct);
        assert_eq!(flavour_of(r"C:\Users\op\Downloads\SENTIENT-Setup-manager-0.9.4.exe", false, pd, Some(STAGE_ARG)), Flavour::Direct);
        assert_eq!(flavour_of(r"C:\Program Files\SENTIENT Platform Manager\SENTIENT Platform Manager.exe", false, pd, None), Flavour::Direct);
        // The remote agent's folder beside it is not the helper.
        assert_eq!(flavour_of(r"C:\ProgramData\SENTIENT\Platform Manager\remote\sentient-platform-manager.exe", false, pd, None), Flavour::Direct);
        assert!(!Flavour::Direct.is_store() && Flavour::Helper.is_store() && Flavour::Packaged.is_store() && Flavour::Staged.is_store());
    }

    #[test]
    fn the_helper_is_current_only_for_the_same_build() {
        let m = marker_text("0.9.4", "4e97dff", 1000);
        assert!(helper_current(Some(&m), Some(1000), "0.9.4", "4e97dff", 1000));
        assert!(!helper_current(Some(&m), Some(1000), "0.9.5", "1234567", 1000));
        assert!(!helper_current(Some(&m), Some(999), "0.9.4", "4e97dff", 1000));
        assert!(!helper_current(None, Some(1000), "0.9.4", "4e97dff", 1000));
        assert!(!helper_current(Some(&m), None, "0.9.4", "4e97dff", 1000));
    }

    #[test]
    fn a_packaged_start_never_replaces_the_helper_under_a_running_window() {
        assert_eq!(hand_over_plan(None, true), HandOver::Helper);
        assert_eq!(hand_over_plan(None, false), HandOver::Stage);
        assert_eq!(hand_over_plan(Some(true), true), HandOver::Forward);
        assert_eq!(hand_over_plan(Some(false), true), HandOver::Forward);
        assert_eq!(hand_over_plan(Some(true), false), HandOver::Forward);
        assert_eq!(hand_over_plan(Some(false), false), HandOver::Stage);
    }

    #[test]
    fn the_helper_folder_is_changed_only_by_system_and_administrators() {
        let a = helper_acl_args(r"C:\ProgramData\SENTIENT\Platform Manager\app");
        assert_eq!(a[1], "/inheritance:r");
        assert!(a.contains(&"*S-1-5-18:(OI)(CI)F".to_string()));
        assert!(a.contains(&"*S-1-5-32-544:(OI)(CI)F".to_string()));
        assert!(a.contains(&"*S-1-5-32-545:(OI)(CI)RX".to_string()));
        assert_eq!(a.iter().filter(|x| x.ends_with(":(OI)(CI)F")).count(), 2);
    }

    #[test]
    fn the_helper_folder_holds_only_the_helper() {
        assert!(helper_dir_keeps("SENTIENT Platform Manager.exe"));
        assert!(helper_dir_keeps("sentient platform manager.exe"));
        assert!(helper_dir_keeps("SENTIENT Platform Manager.exe.old"));
        assert!(helper_dir_keeps(MARKER));
        for planted in ["version.dll", "SENTIENT Platform Manager.exe.local", "x.exe", "sub"] {
            assert!(!helper_dir_keeps(planted), "{planted}");
        }
        assert_eq!(helper_owner_args(r"C:\d"), [r"C:\d", "/setowner", "*S-1-5-32-544", "/T", "/C", "/Q"]);
    }

    #[test]
    fn the_folders_are_under_program_data() {
        let pd = Path::new(r"C:\ProgramData");
        assert!(is_under(&helper_dir_in(pd).to_string_lossy(), r"C:\ProgramData\SENTIENT"));
        assert!(is_under(&staging_dir_in(Path::new(r"C:\Users\op")).to_string_lossy(), r"C:\Users\op"));
        assert!(!is_under(r"C:\ProgramData\SENTIENTX\a", r"C:\ProgramData\SENTIENT"));
        assert!(is_under(r"C:/ProgramData/SENTIENT/x", r"c:\programdata\sentient\"));
    }
}
