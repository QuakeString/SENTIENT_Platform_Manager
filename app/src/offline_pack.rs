// Copyright © 2016-2026 The SENTIENT Authors

//! What the Platform Manager keeps of the last offline pack it installed
//! or updated from (sentient_installer_core::pack): `pack.json`, its
//! signature and the gateway's installer, in the installation's folder.
//! The pack itself may have been on a USB drive that is gone; with this
//! the Gateway screen can still install the gateway it carried when the
//! release list cannot be reached.

use std::path::PathBuf;

use sentient_installer_core::pack::{self, Kind, Opened};

/// `%ProgramData%\SENTIENT\offline-pack` (beside installation.json).
pub fn kept_dir() -> PathBuf {
    crate::installation::record_path().with_file_name("offline-pack")
}

/// Keep `pack`'s list, signature and gateway installer (best effort: the
/// install or update has already succeeded).
pub fn remember(pack: &Opened) -> Result<(), String> {
    let dir = kept_dir();
    if dir == pack.dir {
        return Ok(());
    }
    let _ = std::fs::remove_dir_all(&dir);
    pack.copy_part(&dir, &|f| f.kind == Kind::Gateway).map(|_| ())
}

/// The kept pack, its signature checked again.
pub fn kept() -> Option<Opened> {
    pack::open(&kept_dir()).ok()
}

/// The gateway the kept pack carries, its installer on disk.
pub fn gateway_offer() -> Option<sentient_installer_core::gateway::Offer> {
    let kept = kept()?;
    let offer = sentient_installer_core::gateway::offer_from_pack(&kept)?;
    offer.pack_file.as_ref().is_some_and(|p| p.is_file()).then_some(offer)
}
