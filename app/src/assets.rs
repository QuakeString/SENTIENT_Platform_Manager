// Copyright © 2016-2026 The SENTIENT Authors

//! Assets: GPUI Kit's default icon set plus the few Lucide icons this app
//! needs beyond it (`assets/icons/`, ISC licence in LICENSE-LUCIDE). Embedding
//! only these keeps the binary small; GPUI Kit's `AllAssets` would embed the
//! whole Lucide catalog.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

const EXTRA: &[(&str, &[u8])] = &[
    ("icons/activity.svg", include_bytes!("../assets/icons/activity.svg")),
    ("icons/archive.svg", include_bytes!("../assets/icons/archive.svg")),
    ("icons/arrow-left-right.svg", include_bytes!("../assets/icons/arrow-left-right.svg")),
    ("icons/database.svg", include_bytes!("../assets/icons/database.svg")),
    ("icons/hard-drive-download.svg", include_bytes!("../assets/icons/hard-drive-download.svg")),
    ("icons/power.svg", include_bytes!("../assets/icons/power.svg")),
    ("icons/server.svg", include_bytes!("../assets/icons/server.svg")),
    ("icons/shield-check.svg", include_bytes!("../assets/icons/shield-check.svg")),
    ("icons/trash.svg", include_bytes!("../assets/icons/trash.svg")),
    ("icons/wrench.svg", include_bytes!("../assets/icons/wrench.svg")),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match EXTRA.iter().find(|(name, _)| *name == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = gpui_kit::assets::Assets.list(path)?;
        names.extend(
            EXTRA
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(names)
    }
}
