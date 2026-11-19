// Copyright © 2016-2026 The SENTIENT Authors

//! The Authenticode check of a downloaded Windows program before it runs
//! (WinVerifyTrust, the check Explorer and SmartScreen make), on top of its
//! release signature (crate::signing).
//!
//! The rule ([`verdict`]): a valid signature passes; a signature that is
//! there but does not hold (the file changed after signing, an untrusted,
//! revoked or distrusted certificate) is refused. A program with no
//! Authenticode signature at all passes with a warning while
//! [`REQUIRE_SIGNATURE`] is false: Platform Manager releases are not yet
//! code-signed (scripts/sign-windows.ps1 leaves them unsigned without a
//! certificate), and refusing them would stop every update. Set it to true
//! once the releases carry a certificate.

// The rule is used by check() on Windows only; elsewhere by its tests.
#![cfg_attr(not(windows), allow(dead_code))]

/// Refuse a program without any Authenticode signature.
pub const REQUIRE_SIGNATURE: bool = false;

/// What a WinVerifyTrust result means for running the file.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Trusted,
    /// No signature at all: allowed (with this warning) or refused, by
    /// [`REQUIRE_SIGNATURE`].
    Unsigned(String),
    Refused(String),
}

// The HRESULTs of WinVerifyTrust (winerror.h), here so the rule can be
// tested on every system.
const TRUST_E_NOSIGNATURE: i32 = 0x800B0100_u32 as i32;
const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B0003_u32 as i32;
const TRUST_E_PROVIDER_UNKNOWN: i32 = 0x800B0001_u32 as i32;
const TRUST_E_BAD_DIGEST: i32 = 0x80096010_u32 as i32;
const TRUST_E_EXPLICIT_DISTRUST: i32 = 0x800B0111_u32 as i32;
const TRUST_E_SUBJECT_NOT_TRUSTED: i32 = 0x800B0004_u32 as i32;
const CERT_E_UNTRUSTEDROOT: i32 = 0x800B0109_u32 as i32;
const CERT_E_REVOKED: i32 = 0x800B010C_u32 as i32;
const CERT_E_EXPIRED: i32 = 0x800B0101_u32 as i32;
const CRYPT_E_SECURITY_SETTINGS: i32 = 0x80092026_u32 as i32;

pub fn verdict(code: i32, require_signature: bool) -> Verdict {
    let why = match code {
        0 => return Verdict::Trusted,
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => {
            let w = t!("it has no Authenticode signature").to_string();
            return if require_signature { Verdict::Refused(w) } else { Verdict::Unsigned(w) };
        }
        TRUST_E_BAD_DIGEST => t!("its Authenticode signature does not match the file (it was changed after signing)"),
        TRUST_E_EXPLICIT_DISTRUST => t!("its Authenticode certificate is marked as not trusted on this computer"),
        CERT_E_REVOKED => t!("its Authenticode certificate was revoked"),
        CERT_E_EXPIRED => t!("its Authenticode certificate has expired"),
        CERT_E_UNTRUSTEDROOT | TRUST_E_SUBJECT_NOT_TRUSTED => t!("its Authenticode signature is not from a trusted publisher"),
        CRYPT_E_SECURITY_SETTINGS => t!("this computer's security settings do not allow it"),
        _ => return Verdict::Refused(tf!("its Authenticode signature could not be checked (0x{:08X})", code as u32)),
    };
    Verdict::Refused(why.into())
}

/// Check `path` before it runs: `Ok(Some(warning))` for an unsigned program
/// that is allowed, `Err` for one that is refused.
#[cfg(windows)]
pub fn check(path: &std::path::Path) -> Result<Option<String>, String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN,
        WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    data.Anonymous.pFile = &mut file;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` and `file` (and the path they point at) outlive both
    // calls; the second one frees the state the first one made.
    let code = unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void) };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void) };
    match verdict(code, REQUIRE_SIGNATURE) {
        Verdict::Trusted => Ok(None),
        Verdict::Unsigned(w) => Ok(Some(w)),
        Verdict::Refused(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_signature_passes_and_a_broken_one_is_refused() {
        assert_eq!(verdict(0, true), Verdict::Trusted);
        assert!(matches!(verdict(TRUST_E_BAD_DIGEST, false), Verdict::Refused(e) if e.contains("changed after signing")));
        assert!(matches!(verdict(CERT_E_REVOKED, false), Verdict::Refused(_)));
        assert!(matches!(verdict(CERT_E_UNTRUSTEDROOT, false), Verdict::Refused(_)));
        assert!(matches!(verdict(0x80004005_u32 as i32, false), Verdict::Refused(e) if e.contains("0x80004005")));
    }

    #[test]
    fn no_signature_is_a_warning_until_signatures_are_required() {
        assert!(matches!(verdict(TRUST_E_NOSIGNATURE, false), Verdict::Unsigned(_)));
        assert!(matches!(verdict(TRUST_E_NOSIGNATURE, true), Verdict::Refused(_)));
    }
}
