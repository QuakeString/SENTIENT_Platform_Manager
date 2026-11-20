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
//!
//! Pinned publishers ([`PINNED_SIGNERS`]): our own self-signed code-signing
//! certificate ("INVENIA SYSTEMS") is in no computer's trusted roots, so
//! WinVerifyTrust answers "untrusted root" for a program it signed. That one
//! answer passes when the program's signer is exactly a pinned certificate
//! (the SHA-256 of its DER encoding), the signer has no other error and the
//! certificate is self-signed (the chain is that one certificate). Every
//! other failure (changed after signing, revoked, expired, distrusted,
//! another signer) stays refused. A certificate from a public CA later needs
//! no pin: it passes as trusted.

// The rule is used by check() on Windows only; elsewhere by its tests.
#![cfg_attr(not(windows), allow(dead_code))]

/// Refuse a program without any Authenticode signature.
pub const REQUIRE_SIGNATURE: bool = false;

/// SHA-256 of the DER encoding of each pinned self-signed code-signing
/// certificate. INVENIA SYSTEMS, self-signed, RSA 3072, valid 2026-11-19 to
/// 2031-11-19 (SHA-1 thumbprint C7DC8013F1ECD97ED20193F04E56359B17B24CBD).
/// The public certificate: packaging/windows/INVENIA-SYSTEMS-codesign.cer.
pub const PINNED_SIGNERS: &[&str] = &["64107a06d2803230e2255888045600326e552740dd65f2a711a7e9ea8ff376b2"];

/// What the program's signer was, for the pin: the SHA-256 (hex) of the
/// signer certificate's DER, whether the signer itself had an error, and the
/// length of its certificate chain.
#[derive(Debug, Clone)]
pub struct Signer {
    pub cert_sha256: String,
    pub signer_error: u32,
    pub chain_len: u32,
}

/// [`verdict`], with the pin: "untrusted root" passes for a pinned signer.
pub fn verdict_with_signer(code: i32, require_signature: bool, signer: Option<&Signer>, pins: &[&str]) -> Verdict {
    if code == CERT_E_UNTRUSTEDROOT {
        if let Some(s) = signer {
            if s.signer_error == 0 && s.chain_len == 1 && pins.iter().any(|p| p.eq_ignore_ascii_case(&s.cert_sha256)) {
                return Verdict::Trusted;
            }
        }
    }
    verdict(code, require_signature)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

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
    // The signer, only where the pin can matter, read before the state is
    // freed.
    let signer = if code == CERT_E_UNTRUSTEDROOT { unsafe { signer_of(data.hWVTStateData) } } else { None };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void) };
    match verdict_with_signer(code, REQUIRE_SIGNATURE, signer.as_ref(), PINNED_SIGNERS) {
        Verdict::Trusted => Ok(None),
        Verdict::Unsigned(w) => Ok(Some(w)),
        Verdict::Refused(e) => Err(e),
    }
}

/// The first signer of a verified file, from WinVerifyTrust's state.
///
/// SAFETY: `state` is the hWVTStateData of a WTD_STATEACTION_VERIFY call that
/// has not been closed yet.
#[cfg(windows)]
unsafe fn signer_of(state: windows_sys::Win32::Foundation::HANDLE) -> Option<Signer> {
    use windows_sys::Win32::Security::WinTrust::{WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData};
    if state.is_null() {
        return None;
    }
    let prov = unsafe { WTHelperProvDataFromStateData(state) };
    if prov.is_null() {
        return None;
    }
    let sgnr = unsafe { WTHelperGetProvSignerFromChain(prov, 0, 0, 0) };
    if sgnr.is_null() {
        return None;
    }
    let sgnr = unsafe { &*sgnr };
    if sgnr.csCertChain == 0 || sgnr.pasCertChain.is_null() {
        return None;
    }
    let leaf = unsafe { &*sgnr.pasCertChain };
    if leaf.pCert.is_null() {
        return None;
    }
    let cert = unsafe { &*leaf.pCert };
    if cert.pbCertEncoded.is_null() || cert.cbCertEncoded == 0 {
        return None;
    }
    let der = unsafe { std::slice::from_raw_parts(cert.pbCertEncoded, cert.cbCertEncoded as usize) };
    Some(Signer { cert_sha256: sha256_hex(der), signer_error: sgnr.dwError, chain_len: sgnr.csCertChain })
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

    const PIN: &str = "64107a06d2803230e2255888045600326e552740dd65f2a711a7e9ea8ff376b2";

    fn signer(sha: &str, err: u32, len: u32) -> Signer {
        Signer { cert_sha256: sha.into(), signer_error: err, chain_len: len }
    }

    #[test]
    fn the_pinned_self_signed_signer_passes_an_untrusted_root() {
        assert_eq!(PINNED_SIGNERS, &[PIN]);
        let ours = signer(PIN, 0, 1);
        assert_eq!(verdict_with_signer(CERT_E_UNTRUSTEDROOT, false, Some(&ours), PINNED_SIGNERS), Verdict::Trusted);
        assert_eq!(verdict_with_signer(CERT_E_UNTRUSTEDROOT, true, Some(&signer(&PIN.to_uppercase(), 0, 1)), PINNED_SIGNERS), Verdict::Trusted);
    }

    #[test]
    fn the_pin_allows_nothing_else() {
        let ours = signer(PIN, 0, 1);
        // Another self-signed certificate.
        let other = signer(&"ab".repeat(32), 0, 1);
        assert!(matches!(verdict_with_signer(CERT_E_UNTRUSTEDROOT, false, Some(&other), PINNED_SIGNERS), Verdict::Refused(_)));
        // No signer read.
        assert!(matches!(verdict_with_signer(CERT_E_UNTRUSTEDROOT, false, None, PINNED_SIGNERS), Verdict::Refused(_)));
        // Our certificate, but the signer has its own error or a longer chain.
        assert!(matches!(verdict_with_signer(CERT_E_UNTRUSTEDROOT, false, Some(&signer(PIN, 0x80096010, 1)), PINNED_SIGNERS), Verdict::Refused(_)));
        assert!(matches!(verdict_with_signer(CERT_E_UNTRUSTEDROOT, false, Some(&signer(PIN, 0, 2)), PINNED_SIGNERS), Verdict::Refused(_)));
        // Every other failure stays refused for our certificate too.
        for code in [TRUST_E_BAD_DIGEST, CERT_E_REVOKED, CERT_E_EXPIRED, TRUST_E_EXPLICIT_DISTRUST, TRUST_E_SUBJECT_NOT_TRUSTED, CRYPT_E_SECURITY_SETTINGS] {
            assert!(matches!(verdict_with_signer(code, false, Some(&ours), PINNED_SIGNERS), Verdict::Refused(_)), "{code:#x}");
        }
        // Unsigned is still the plain rule.
        assert!(matches!(verdict_with_signer(TRUST_E_NOSIGNATURE, false, Some(&ours), PINNED_SIGNERS), Verdict::Unsigned(_)));
        assert_eq!(verdict_with_signer(0, false, None, PINNED_SIGNERS), Verdict::Trusted);
    }

    #[test]
    fn the_pin_is_the_published_certificate() {
        let der = include_bytes!("../../packaging/windows/INVENIA-SYSTEMS-codesign.cer");
        assert_eq!(sha256_hex(der), PIN);
    }

    #[test]
    fn no_signature_is_a_warning_until_signatures_are_required() {
        assert!(matches!(verdict(TRUST_E_NOSIGNATURE, false), Verdict::Unsigned(_)));
        assert!(matches!(verdict(TRUST_E_NOSIGNATURE, true), Verdict::Refused(_)));
    }
}
