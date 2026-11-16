// Copyright © 2016-2026 The SENTIENT Authors

//! Move data to another disk: the M lane's flow, built by another worker.
//! This stub is what the restore flow calls (X3, B2); worker A replaces it
//! with the real flow when the branches merge.

use gpui_kit::*;

/// Open "Move data to another disk" for the instance the folders are on.
// TODO(worker A): replace with the M lane's movedata::open when merging.
pub fn open(target: crate::manager::folder_chooser::Target, _window: &mut Window, cx: &mut App) {
    let _ = target;
    crate::manager::restore::go("settings", cx);
}
