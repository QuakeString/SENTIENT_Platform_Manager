// Copyright © 2016-2026 The SENTIENT Authors

//! Release signatures: minisign (ed25519) over what the Platform Manager
//! downloads, checked against the release key pinned below.
//!
//! What is signed (scripts/sign-release.sh, run when a release is published):
//! - a SENTIENT release's `manifest.json` → `manifest.json.minisig`. The
//!   manifest lists every file with its SHA-256, so the one signature covers
//!   them all ([`crate::manifest::download`] checks each hash);
//! - each Platform Manager installer → `<installer>.minisig`.
//!
//! GitHub's own SHA-256 says the file arrived as uploaded; the signature says
//! the release key's holder published it.
//!
//! Releases published before signing began carry no signature. The rule
//! ([`check`]), per product (SENTIENT or the Platform Manager):
//! - a signature is there: it must verify with the pinned key, or the file
//!   is refused;
//! - no signature, and signing has begun ([`signing_started`]): refused;
//! - no signature, and signing has not begun: used, with a visible
//!   "unsigned release" warning ([`Trust::Unsigned`]).
//!
//! Signing has begun when the release feed holds at least one release of
//! that product with a signature, or when this computer has seen one before
//! (the caller keeps that memory, so a feed that later hides its signatures
//! does not bring the unsigned path back).
//!
//! The key, its handling and rotation: docs/security/RELEASE_SIGNING.md.

use minisign_verify::{PublicKey, Signature};

/// The release signing key (minisign public key, id 0A8C8F7EFCE2CBA2).
/// Its secret half is kept off every repository, on the release machine.
pub const RELEASE_PUBLIC_KEY: &str = "RWSiy+L8fo+MCtWNaQPLK8KSB3WBwoVVaMaxcLeUE8m8HuvlaJL6p0Ev";

/// The signature file published beside `name`.
pub fn signature_name(name: &str) -> String {
    format!("{name}.minisig")
}

/// How far a file is trusted after [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Signed with the release key; the signature's trusted comment.
    Signed(String),
    /// From before signing began: used, and said so.
    Unsigned,
}

impl Trust {
    /// The warning to show for an unsigned release, if any.
    pub fn warning(&self, what: &str) -> Option<String> {
        matches!(self, Trust::Unsigned).then(|| format!("Unsigned release: {what} was published before releases were signed. Its checksum was checked; its publisher was not."))
    }
}

/// Which product a release belongs to, for [`signing_started`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    Sentient,
    Manager,
}

/// Whether the feed holds a signed release of `product`. SENTIENT's
/// signature is `manifest.json.minisig`; the Platform Manager's, a
/// `.minisig` beside an installer.
pub fn signing_started(all: &[crate::releases::Release], product: Product) -> bool {
    all.iter().filter(|r| !r.draft).any(|r| match product {
        Product::Sentient => crate::releases::is_sentient(r) && r.has_asset(&signature_name("manifest.json")),
        Product::Manager => crate::releases::is_manager(r) && r.assets.iter().any(|a| a.name.ends_with(".minisig")),
    })
}

/// Verify `data` against a minisign signature with the public key `key`
/// (base64, the second line of a minisign .pub file).
pub fn verify_with(key: &str, data: &[u8], signature: &str) -> Result<String, String> {
    let pk = PublicKey::from_base64(key).map_err(|e| format!("the release key is not usable ({e})"))?;
    let sig = Signature::decode(signature).map_err(|_| "the signature file is not a minisign signature".to_string())?;
    pk.verify(data, &sig, false).map_err(|_| "the signature does not match the SENTIENT release key".to_string())?;
    Ok(sig.trusted_comment().to_string())
}

/// The rule of the module note, for `what` (named in the messages).
/// `signature`: the published signature's text, if there is one.
pub fn check(what: &str, data: &[u8], signature: Option<&str>, signing_started: bool) -> Result<Trust, String> {
    check_with(RELEASE_PUBLIC_KEY, what, data, signature, signing_started)
}

fn check_with(key: &str, what: &str, data: &[u8], signature: Option<&str>, signing_started: bool) -> Result<Trust, String> {
    match signature {
        Some(sig) => verify_with(key, data, sig)
            .map(Trust::Signed)
            .map_err(|e| format!("{what} is not used: {e}. It may have been altered. Nothing was changed.")),
        None if signing_started => Err(format!(
            "{what} is not used: it has no signature, and SENTIENT releases are signed. Nothing was changed."
        )),
        None => Ok(Trust::Unsigned),
    }
}

/// Fetch the signature published at `url` (`Ok(None)`: there is none).
pub fn fetch_signature(url: &str) -> Result<Option<String>, String> {
    match crate::net::agent().get(url).set("User-Agent", "SENTIENT-Platform-Manager").call() {
        Ok(r) => r.into_string().map(Some).map_err(|e| format!("could not read the signature: {e}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(format!("could not fetch the signature: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::releases::{Asset, Release};

    /// A test key (not the release key) and a signature made with it over
    /// [`DATA`] by rsign2 (minisign's format, prehashed).
    const TEST_KEY: &str = "RWRhWEPNjjiM6SgyhUOwV/nMxRNko5QCZ80DL0X8gblkX2cLyLDbN+Lo";
    const DATA: &[u8] = b"SENTIENT signing test\n";
    const SIG: &str = "untrusted comment: signature from rsign secret key
RURhWEPNjjiM6dLOzJbqOBezGvir6aXfNP5bHLNVejXPacnNrD2GeAUF5U8xK1HlUX1VQsWqm1XKPZg47IznO7Iexrh/zbiOEgQ=
trusted comment: file:data.bin
hdvYlnXJ9l6PUo4R/EdGIrgOpcCWDN1HQd+jQ7iCxk5zek15asp6uLWB+MbtBYYCnzAi4fYAvvfGQ914bk7PAQ==
";

    /// A file signed with scripts/sign-release.sh verifies with the pinned
    /// key: SPM_SIGNED_FILE=<file> (its .minisig beside it).
    #[test]
    #[ignore]
    fn a_file_signed_by_the_release_script_verifies() {
        let file = std::env::var("SPM_SIGNED_FILE").expect("SPM_SIGNED_FILE");
        let data = std::fs::read(&file).unwrap();
        let sig = std::fs::read_to_string(format!("{file}.minisig")).unwrap();
        let trust = check("the file", &data, Some(&sig), true).unwrap();
        println!("{trust:?}");
    }

    #[test]
    fn the_pinned_key_is_a_minisign_key() {
        assert!(PublicKey::from_base64(RELEASE_PUBLIC_KEY).is_ok());
    }

    #[test]
    fn a_good_signature_is_trusted() {
        assert_eq!(check_with(TEST_KEY, "x", DATA, Some(SIG), true), Ok(Trust::Signed("file:data.bin".into())));
        assert_eq!(check_with(TEST_KEY, "x", DATA, Some(SIG), false), Ok(Trust::Signed("file:data.bin".into())));
    }

    #[test]
    fn a_bad_signature_is_refused() {
        // Other bytes.
        let e = check_with(TEST_KEY, "Platform Manager 0.9.0", b"SENTIENT signing test!\n", Some(SIG), false).unwrap_err();
        assert!(e.starts_with("Platform Manager 0.9.0 is not used: the signature does not match"), "{e}");
        // Another key: the test signature against the release key.
        assert!(check("x", DATA, Some(SIG), false).is_err());
        // Not a signature at all, or a tampered trusted comment.
        assert!(check_with(TEST_KEY, "x", DATA, Some("garbage"), false).unwrap_err().contains("not a minisign signature"));
        let tampered = SIG.replace("file:data.bin", "file:other.bin");
        assert!(check_with(TEST_KEY, "x", DATA, Some(&tampered), false).is_err());
    }

    #[test]
    fn a_missing_signature_counts_only_before_signing_began() {
        // No signed release in the feed yet: used, with the warning.
        let t = check_with(TEST_KEY, "SENTIENT 4.4.0.1", DATA, None, false).unwrap();
        assert_eq!(t, Trust::Unsigned);
        assert!(t.warning("SENTIENT 4.4.0.1").unwrap().starts_with("Unsigned release: SENTIENT 4.4.0.1"));
        assert_eq!(Trust::Signed(String::new()).warning("x"), None);
        // A signed release exists: refused.
        let e = check_with(TEST_KEY, "SENTIENT 4.4.0.1", DATA, None, true).unwrap_err();
        assert!(e.contains("has no signature"), "{e}");
    }

    fn rel(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            name: None,
            body: None,
            html_url: String::new(),
            prerelease: false,
            draft: false,
            published_at: None,
            assets: assets.iter().map(|n| Asset { name: n.to_string(), browser_download_url: String::new(), size: 1, digest: None }).collect(),
        }
    }

    #[test]
    fn signing_begins_per_product_with_the_first_signed_release() {
        let old = vec![rel("4.4.0.1", &["manifest.json"]), rel("spm-v0.8.0", &["SENTIENT-Setup-manager-0.8.0.exe"])];
        assert!(!signing_started(&old, Product::Sentient));
        assert!(!signing_started(&old, Product::Manager));
        let mut feed = old.clone();
        feed.push(rel("4.4.0.2", &["manifest.json", "manifest.json.minisig"]));
        assert!(signing_started(&feed, Product::Sentient));
        assert!(!signing_started(&feed, Product::Manager));
        feed.push(rel("spm-v0.9.0", &["SENTIENT-Setup-manager-0.9.0.exe", "SENTIENT-Setup-manager-0.9.0.exe.minisig"]));
        assert!(signing_started(&feed, Product::Manager));
        // A gateway release's signature says nothing about either.
        let gw = vec![rel("gateway-4.16.1", &["manifest.json", "manifest.json.minisig"])];
        assert!(!signing_started(&gw, Product::Sentient));
    }
}
