//! Release manifest: where the SENTIENT payload lives and what it should hash to.
//!
//! The native engine downloads PostgreSQL and TimescaleDB from their vendors,
//! but SENTIENT itself is ours to distribute. Rather than hard-coding a host,
//! the Platform Manager fetches a small JSON manifest and works from that — so
//! moving from a stop-gap host to real infrastructure is a URL change, not a
//! code change.
//!
//! Every artifact carries a SHA-256, and that is load-bearing rather than
//! belt-and-braces. Consumer file hosts fail in ways that look like success:
//! Google Drive returns an HTML share page for a `/file/d/…/view` link, a
//! virus-scan interstitial for anything over ~100 MB, and a rate-limit page
//! when a file gets popular — all of them HTTP 200 with a plausible body. The
//! hash is what turns "you installed an HTML page" into a clear error.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::progress::{Progress, ProgressFn};

/// One downloadable file.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// Name to save it as, and the key the engine looks it up by.
    pub file: String,
    pub url: String,
    /// Lower-case hex SHA-256. Required — see the module note.
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    /// "exe" lands in bin/, "targz" is unpacked into the named directory.
    #[serde(default)]
    pub kind: String,
    /// For "targz": where its contents go, relative to the install directory.
    #[serde(default)]
    pub into: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub product: String,
    pub version: String,
    #[serde(default)]
    pub released: String,
    #[serde(default)]
    pub notes: String,
    pub artifacts: Vec<Artifact>,
}

/// Rewrite a Google Drive share link into something that actually returns bytes.
///
/// A `/file/d/<id>/view` URL serves an HTML page. The `usercontent` download
/// endpoint with `confirm=t` also skips the large-file scan interstitial, which
/// is otherwise hit by anything over roughly 100 MB — i.e. by the server binary
/// every single time.
pub fn direct_url(url: &str) -> String {
    let id = if let Some(rest) = url.split("/file/d/").nth(1) {
        rest.split('/').next().unwrap_or("").to_string()
    } else if let Some(rest) = url.split("id=").nth(1) {
        rest.split('&').next().unwrap_or("").to_string()
    } else {
        return url.to_string();
    };
    if id.is_empty() || !url.contains("drive.google.com") {
        return url.to_string();
    }
    format!("https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t")
}

pub fn fetch(url: &str) -> Result<Manifest, String> {
    let body = ureq::get(url)
        .call()
        .map_err(|e| format!("Could not fetch the release manifest: {e}"))?
        .into_string()
        .map_err(|e| format!("Could not read the release manifest: {e}"))?;
    serde_json::from_str(&body)
        .map_err(|e| format!("The release manifest is not valid JSON: {e}"))
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download one artifact, verifying the hash. A cached file that already
/// matches is reused, so a retry after a failure part-way does not re-pull
/// hundreds of megabytes.
pub fn download(sink: &ProgressFn, art: &Artifact, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let dest = dir.join(&art.file);

    if dest.exists() {
        if let Ok(h) = hash_file(&dest) {
            if h.eq_ignore_ascii_case(&art.sha256) {
                sink(Progress::Log { line: format!("{} already downloaded", art.file) });
                return Ok(dest);
            }
        }
        let _ = std::fs::remove_file(&dest);
    }

    let url = direct_url(&art.url);
    sink(Progress::Log { line: format!("downloading {}", art.file) });
    let resp = ureq::get(&url)
        .call()
        .map_err(|e| format!("download {}: {e}", art.file))?;

    // A file host handing back a web page is the common failure. Catch it here
    // so the error names the real problem instead of "hash mismatch".
    if let Some(ct) = resp.header("content-type") {
        if ct.starts_with("text/html") {
            return Err(format!(
                "{} came back as a web page rather than a file.\n\
                 The link is probably a share page rather than a direct download, \
                 or the host is showing an interstitial.",
                art.file
            ));
        }
    }

    let total = resp
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(art.size);
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download {}: {e}", art.file))?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
        done += n as u64;
        if total > 0 {
            sink(Progress::Percent { value: (done as f32 / total as f32).clamp(0.0, 1.0) });
        }
    }
    drop(file);

    let got = hash_file(&dest)?;
    if !got.eq_ignore_ascii_case(&art.sha256) {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "{} failed verification.\nexpected {}\ngot      {}\n\n\
             The download was incomplete or the file has been altered.",
            art.file, art.sha256, got
        ));
    }
    Ok(dest)
}
