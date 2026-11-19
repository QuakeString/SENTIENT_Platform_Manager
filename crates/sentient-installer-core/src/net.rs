// Copyright © 2016-2026 The SENTIENT Authors

//! HTTPS for downloads (PostgreSQL, TimescaleDB, the WSL image, release
//! manifests and payloads).
//!
//! Certificates are verified by the operating system, not against a list of
//! root CAs built into this program: Windows' certificate store (kept
//! current by Windows Update, which also fetches roots on demand), macOS's
//! keychain, or the distribution's CA bundle (ca-certificates) on Linux. An
//! organisation's own CAs — a TLS-inspecting proxy, an internal mirror —
//! work once they are in that store, and nothing here needs updating when
//! roots change. `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` are honoured.

use std::sync::{Arc, OnceLock};

use rustls_platform_verifier::BuilderVerifierExt;

/// The TLS configuration with the platform verifier, built once.
fn tls_config() -> Option<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .ok()?
                .with_platform_verifier()
                .ok()?
                .with_no_client_auth();
            Some(Arc::new(config))
        })
        .clone()
}

/// A GET to the internet, refused in offline mode (crate::offline): every
/// download goes through here, so an install from an offline pack never
/// opens a connection.
pub fn get(url: &str) -> Result<ureq::Request, String> {
    crate::offline::refuse_network(&format!("a download from {}", host_of(url)))?;
    Ok(agent().get(url))
}

/// `https://cloud-images.ubuntu.com/wsl/…` → `cloud-images.ubuntu.com`.
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// An agent for requests to the internet. Only [`get`] uses it: it says no
/// in offline mode.
fn agent() -> ureq::Agent {
    let builder = ureq::AgentBuilder::new().try_proxy_from_env(true);
    match tls_config() {
        Some(config) => builder.tls_config(config),
        // Only if the platform verifier cannot start at all: ureq's own roots.
        None => builder,
    }
    .build()
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_verifier_starts() {
        assert!(super::tls_config().is_some());
    }

    #[test]
    fn the_host_of_a_download_is_named() {
        assert_eq!(super::host_of("https://cloud-images.ubuntu.com/wsl/releases/24.04/x.tar.gz"), "cloud-images.ubuntu.com");
        assert_eq!(super::host_of("https://aka.ms?x=1"), "aka.ms");
        assert_eq!(super::host_of("example.test/a"), "example.test");
    }
}
