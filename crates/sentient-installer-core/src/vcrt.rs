// Copyright © 2016-2026 The SENTIENT Authors

//! Visual C++ runtime checks and Windows crash-code explanations.
//!
//! PostgreSQL 18.6's Windows binaries ship ICU (icuuc77.dll, icuin77.dll)
//! built with MSVC 14.44 (Visual Studio 2022 17.14). Those DLLs import
//! `_Mtx_lock` from msvcp140.dll but do not ship msvcp140.dll, so they use
//! whatever copy is in System32. Since VS 2022 17.10 (runtime 14.40) the
//! STL's `std::mutex` constructor is constexpr; code built that way crashes
//! with an access violation (0xC0000005) inside an older msvcp140.dll. initdb
//! first loads ICU during "performing post-bootstrap initialization", which
//! is where the crash shows.
//!
//! So the runtime being *present* is not enough: it has to be at least as
//! new as the toolset the binaries were built with. This module reads the
//! version out of the DLL itself (the registry can disagree with the file)
//! and turns raw NTSTATUS codes into a sentence a user can act on.

/// Oldest msvcp140.dll / vcruntime140.dll that the shipped PostgreSQL and
/// ICU binaries run with: the linker version stamped into icuuc77.dll and
/// postgres.exe of postgresql-18.6-1 (14.44). Raise it together with
/// `native::PG_VERSION` when a newer PostgreSQL zip is built with a newer
/// toolset: check with the PE header's MajorLinkerVersion/MinorLinkerVersion.
pub const MIN_VC_RUNTIME: (u16, u16) = (14, 44);

/// The runtime DLLs whose version decides it. msvcp140.dll is the one that
/// crashes; vcruntime140_1.dll may be missing entirely on a fresh Windows.
pub const RUNTIME_DLLS: &[&str] = &["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll"];

/// File version (major, minor, build, revision) from a PE file's
/// VS_FIXEDFILEINFO block, found by its 0xFEEF04BD signature.
pub fn pe_file_version(bytes: &[u8]) -> Option<(u16, u16, u16, u16)> {
    const SIG: [u8; 4] = [0xBD, 0x04, 0xEF, 0xFE];
    let at = bytes.windows(4).position(|w| w == SIG)?;
    let u32_at = |o: usize| -> Option<u32> {
        let b = bytes.get(at + o..at + o + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    // dwSignature, dwStrucVersion, dwFileVersionMS, dwFileVersionLS
    let ms = u32_at(8)?;
    let ls = u32_at(12)?;
    Some(((ms >> 16) as u16, ms as u16, (ls >> 16) as u16, ls as u16))
}

/// Why the runtime in `system32` is not good enough, or None when it is.
pub fn runtime_problem(system32: &std::path::Path) -> Option<String> {
    for name in RUNTIME_DLLS {
        let path = system32.join(name);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Some(format!("{name} is missing")),
        };
        match pe_file_version(&bytes) {
            Some((maj, min, build, _)) if (maj, min) < MIN_VC_RUNTIME => {
                return Some(format!(
                    "{name} is version {maj}.{min}.{build}; PostgreSQL needs {}.{} or newer",
                    MIN_VC_RUNTIME.0, MIN_VC_RUNTIME.1
                ))
            }
            Some(_) => {}
            // Unreadable version: do not block on it, the crash check after
            // initdb still catches a bad runtime.
            None => {}
        }
    }
    None
}

/// A crash code in a child process's output that points at the Visual C++
/// runtime, which a runtime repair can fix.
pub fn is_runtime_crash(text: &str) -> bool {
    let t = text.to_ascii_uppercase();
    ["0XC0000005", "0XC0000135", "0XC000007B", "0XC0000139"]
        .iter()
        .any(|c| t.contains(c))
}

/// A plain-language explanation for a Windows NTSTATUS crash code found in
/// `text`, for the install error box.
pub fn crash_hint(text: &str) -> Option<&'static str> {
    let t = text.to_ascii_uppercase();
    let hints: &[(&str, &str)] = &[
        (
            "0XC0000005",
            "0xC0000005 is an access violation. With PostgreSQL on Windows this almost always \
             means the Microsoft Visual C++ runtime on this computer is older than the one \
             PostgreSQL was built with. Install the latest \"Visual C++ 2015-2022 \
             Redistributable (x64)\" (https://aka.ms/vs/17/release/vc_redist.x64.exe, choose \
             Repair if it is already installed) and press Retry. If it still fails, an \
             antivirus program may be injecting into postgres.exe: add an exclusion for the \
             PostgreSQL folder.",
        ),
        (
            "0XC0000135",
            "0xC0000135 means a DLL that PostgreSQL needs could not be found, usually the \
             Microsoft Visual C++ runtime. Install the latest \"Visual C++ 2015-2022 \
             Redistributable (x64)\" (https://aka.ms/vs/17/release/vc_redist.x64.exe) and press \
             Retry.",
        ),
        (
            "0XC0000139",
            "0xC0000139 means a DLL PostgreSQL loaded is too old to have a function it needs, \
             usually an old Microsoft Visual C++ runtime. Install the latest \"Visual C++ \
             2015-2022 Redistributable (x64)\" (https://aka.ms/vs/17/release/vc_redist.x64.exe) \
             and press Retry.",
        ),
        (
            "0XC000007B",
            "0xC000007B means a 32-bit DLL was loaded into 64-bit PostgreSQL: another program \
             has put its own DLLs on the system PATH. Install the latest \"Visual C++ 2015-2022 \
             Redistributable (x64)\" and remove the other program's folder from PATH, then \
             press Retry.",
        ),
        (
            "0XC0000142",
            "0xC0000142 means PostgreSQL could not start a process, often because Windows is \
             low on desktop heap or an antivirus program blocked it. Restart the computer and \
             press Retry.",
        ),
    ];
    hints.iter().find(|(code, _)| t.contains(code)).map(|(_, h)| *h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_info(ms: u32, ls: u32) -> Vec<u8> {
        let mut v = vec![0u8; 37]; // junk before, like a real resource section
        v.extend_from_slice(&0xFEEF04BDu32.to_le_bytes());
        v.extend_from_slice(&0x0001_0000u32.to_le_bytes());
        v.extend_from_slice(&ms.to_le_bytes());
        v.extend_from_slice(&ls.to_le_bytes());
        v.extend_from_slice(&[0u8; 40]);
        v
    }

    #[test]
    fn reads_version() {
        let b = fixed_info((14 << 16) | 29, (30139 << 16) | 0);
        assert_eq!(pe_file_version(&b), Some((14, 29, 30139, 0)));
        assert_eq!(pe_file_version(&[1, 2, 3]), None);
    }

    #[test]
    fn old_runtime_is_a_problem_new_is_not() {
        let dir = std::env::temp_dir().join(format!("vcrt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in RUNTIME_DLLS {
            std::fs::write(dir.join(n), fixed_info((14 << 16) | 44, 35211 << 16)).unwrap();
        }
        assert_eq!(runtime_problem(&dir), None);
        std::fs::write(dir.join("msvcp140.dll"), fixed_info((14 << 16) | 29, 30139 << 16)).unwrap();
        let p = runtime_problem(&dir).unwrap();
        assert!(p.contains("msvcp140.dll is version 14.29.30139"), "{p}");
        std::fs::remove_file(dir.join("vcruntime140_1.dll")).unwrap();
        std::fs::write(dir.join("msvcp140.dll"), fixed_info((14 << 16) | 50, 0)).unwrap();
        assert_eq!(runtime_problem(&dir).as_deref(), Some("vcruntime140_1.dll is missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explains_crash_codes() {
        let msg = "initdb failed: child process was terminated by exception 0xC0000005";
        assert!(is_runtime_crash(msg));
        assert!(crash_hint(msg).unwrap().contains("Visual C++"));
        assert!(crash_hint("exception 0xc0000135").unwrap().contains("could not be found"));
        assert!(!is_runtime_crash("could not change permissions"));
        assert_eq!(crash_hint("Permission denied"), None);
    }
}
