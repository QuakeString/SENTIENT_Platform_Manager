// Copyright © 2016-2026 The SENTIENT Authors

//! Assets: GPUI Kit's default icon set plus the few Lucide icons this app
//! needs beyond it (`assets/icons/`, ISC licence in LICENSE-LUCIDE), and the
//! product and brand logos (`assets/logos/`). Embedding
//! only these keeps the binary small; GPUI Kit's `AllAssets` would embed the
//! whole Lucide catalog.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

const EXTRA: &[(&str, &[u8])] = &[
    ("icons/activity.svg", include_bytes!("../assets/icons/activity.svg")),
    ("icons/archive.svg", include_bytes!("../assets/icons/archive.svg")),
    ("icons/arrow-left-right.svg", include_bytes!("../assets/icons/arrow-left-right.svg")),
    ("icons/database.svg", include_bytes!("../assets/icons/database.svg")),
    ("icons/download.svg", include_bytes!("../assets/icons/download.svg")),
    ("icons/hard-drive-download.svg", include_bytes!("../assets/icons/hard-drive-download.svg")),
    ("icons/monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
    ("icons/power.svg", include_bytes!("../assets/icons/power.svg")),
    ("icons/scroll-text.svg", include_bytes!("../assets/icons/scroll-text.svg")),
    ("icons/server.svg", include_bytes!("../assets/icons/server.svg")),
    ("icons/shield-check.svg", include_bytes!("../assets/icons/shield-check.svg")),
    ("icons/trash.svg", include_bytes!("../assets/icons/trash.svg")),
    ("icons/wrench.svg", include_bytes!("../assets/icons/wrench.svg")),
    // Product and brand logos (logos/: simple-icons, CC0, see
    // LICENSE-SIMPLE-ICONS.md; SENTIENT's own marks; Windows from SENTIENT's UI).
    ("logos/almalinux.svg", include_bytes!("../assets/logos/almalinux.svg")),
    ("logos/archlinux.svg", include_bytes!("../assets/logos/archlinux.svg")),
    ("logos/centos.svg", include_bytes!("../assets/logos/centos.svg")),
    ("logos/clickhouse.svg", include_bytes!("../assets/logos/clickhouse.svg")),
    ("logos/debian.svg", include_bytes!("../assets/logos/debian.svg")),
    ("logos/fedora.svg", include_bytes!("../assets/logos/fedora.svg")),
    ("logos/gateway.svg", include_bytes!("../assets/logos/gateway.svg")),
    ("logos/linux.svg", include_bytes!("../assets/logos/linux.svg")),
    ("logos/opensuse.svg", include_bytes!("../assets/logos/opensuse.svg")),
    ("logos/postgresql.svg", include_bytes!("../assets/logos/postgresql.svg")),
    ("logos/raspberrypi.svg", include_bytes!("../assets/logos/raspberrypi.svg")),
    ("logos/redhat.svg", include_bytes!("../assets/logos/redhat.svg")),
    ("logos/rockylinux.svg", include_bytes!("../assets/logos/rockylinux.svg")),
    ("logos/sentient-mark.svg", include_bytes!("../assets/logos/sentient-mark.svg")),
    ("logos/timescale.svg", include_bytes!("../assets/logos/timescale.svg")),
    ("logos/ubuntu.svg", include_bytes!("../assets/logos/ubuntu.svg")),
    ("logos/windows.svg", include_bytes!("../assets/logos/windows.svg")),
    ("logos/spm.png", include_bytes!("../assets/icon-256.png")),
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
