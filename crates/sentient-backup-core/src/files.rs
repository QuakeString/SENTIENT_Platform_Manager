//! File-store handling: the on-disk directories SENTIENT keeps outside the
//! database (`vc-repos`, `reports`). These are only reachable when the app runs
//! on the same machine and can read the path; otherwise the category is offered
//! but disabled.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::categories::{catalog, CategoryKind, FileStore};
use crate::error::{Error, Result};
use crate::util::HashingWriter;

/// Runtime status of a file store for the UI: resolved path + whether it's
/// readable from here.
#[derive(Debug, Clone, Serialize)]
pub struct FileStoreStatus {
    pub id: String,
    /// Category this store belongs to.
    pub category_id: String,
    pub path: String,
    pub reachable: bool,
}

/// Resolve a file store's path: `SBR_<ENVVAR>` override, else the env var
/// SENTIENT itself uses, else the documented default.
pub fn resolve_path(fs: &FileStore) -> PathBuf {
    if let Ok(p) = std::env::var(format!("SBR_{}", fs.env_var)) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var(fs.env_var) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from(fs.default_path)
}

/// Readable directory?
pub fn reachable(path: &Path) -> bool {
    path.is_dir() && std::fs::read_dir(path).is_ok()
}

/// Status of every file-store category (for the connection/backup screen).
pub fn statuses() -> Vec<FileStoreStatus> {
    catalog()
        .iter()
        .filter(|c| c.kind == CategoryKind::FileStore)
        .filter_map(|c| {
            c.file_store.map(|fs| {
                let path = resolve_path(&fs);
                FileStoreStatus {
                    id: fs.id.to_string(),
                    category_id: c.id.to_string(),
                    path: path.display().to_string(),
                    reachable: reachable(&path),
                }
            })
        })
        .collect()
}

/// tar + zstd a directory's contents into `dest_file`, hashing the compressed
/// output. Returns (sha256, bytes).
pub fn archive_dir(dir: &Path, dest_file: &Path, level: i32) -> Result<(String, u64)> {
    let out = std::fs::File::create(dest_file)?;
    let mut hw = HashingWriter::new(out);
    {
        let enc = zstd::stream::Encoder::new(&mut hw, level)
            .map_err(|e| Error::msg(format!("zstd: {e}")))?
            .auto_finish();
        let mut tar = tar::Builder::new(enc);
        tar.follow_symlinks(false);
        tar.append_dir_all(".", dir)
            .map_err(|e| Error::msg(format!("archiving {}: {e}", dir.display())))?;
        tar.finish().map_err(|e| Error::msg(e.to_string()))?;
    }
    Ok(hw.finish())
}

/// Extract a tar.zst file into `dest` (created if needed).
pub fn extract_dir(src_targz: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let f = std::fs::File::open(src_targz)?;
    let dec = zstd::stream::Decoder::new(f).map_err(|e| Error::msg(format!("zstd: {e}")))?;
    let mut ar = tar::Archive::new(dec);
    ar.unpack(dest)
        .map_err(|e| Error::msg(format!("extracting to {}: {e}", dest.display())))?;
    Ok(())
}
