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
    ("icons/chart-line.svg", include_bytes!("../assets/icons/chart-line.svg")),
    ("icons/clipboard.svg", include_bytes!("../assets/icons/clipboard.svg")),
    ("icons/contrast.svg", include_bytes!("../assets/icons/contrast.svg")),
    ("icons/database.svg", include_bytes!("../assets/icons/database.svg")),
    ("icons/download.svg", include_bytes!("../assets/icons/download.svg")),
    ("icons/folder-fill.svg", include_bytes!("../assets/icons/folder-fill.svg")),
    ("icons/folder-plus.svg", include_bytes!("../assets/icons/folder-plus.svg")),
    ("icons/history.svg", include_bytes!("../assets/icons/history.svg")),
    ("icons/upload.svg", include_bytes!("../assets/icons/upload.svg")),
    ("icons/laptop.svg", include_bytes!("../assets/icons/laptop.svg")),
    ("icons/calendar.svg", include_bytes!("../assets/icons/calendar.svg")),
    ("icons/move-disk.svg", include_bytes!("../assets/icons/move-disk.svg")),
    ("icons/log-file.svg", include_bytes!("../assets/icons/log-file.svg")),
    ("icons/house.svg", include_bytes!("../assets/icons/house.svg")),
    ("icons/link.svg", include_bytes!("../assets/icons/link.svg")),
    ("icons/lock.svg", include_bytes!("../assets/icons/lock.svg")),
    ("icons/hard-drive-download.svg", include_bytes!("../assets/icons/hard-drive-download.svg")),
    ("icons/key-round.svg", include_bytes!("../assets/icons/key-round.svg")),
    ("icons/monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
    ("icons/pencil.svg", include_bytes!("../assets/icons/pencil.svg")),
    ("icons/power.svg", include_bytes!("../assets/icons/power.svg")),
    ("icons/scroll-text.svg", include_bytes!("../assets/icons/scroll-text.svg")),
    ("icons/server.svg", include_bytes!("../assets/icons/server.svg")),
    ("icons/shield-check.svg", include_bytes!("../assets/icons/shield-check.svg")),
    ("icons/sliders-horizontal.svg", include_bytes!("../assets/icons/sliders-horizontal.svg")),
    ("icons/trash.svg", include_bytes!("../assets/icons/trash.svg")),
    ("icons/wrench.svg", include_bytes!("../assets/icons/wrench.svg")),
    // The Licence page (crate::manager::licence): the mockup's Lucide icons.
    ("icons/licence/cart.svg", include_bytes!("../assets/icons/licence/cart.svg")),
    ("icons/licence/chart.svg", include_bytes!("../assets/icons/licence/chart.svg")),
    ("icons/licence/file.svg", include_bytes!("../assets/icons/licence/file.svg")),
    ("icons/licence/key.svg", include_bytes!("../assets/icons/licence/key.svg")),
    ("icons/licence/mail.svg", include_bytes!("../assets/icons/licence/mail.svg")),
    ("icons/licence/shield.svg", include_bytes!("../assets/icons/licence/shield.svg")),
    ("icons/licence/trash.svg", include_bytes!("../assets/icons/licence/trash.svg")),
    ("icons/licence/unlink.svg", include_bytes!("../assets/icons/licence/unlink.svg")),
    ("icons/licence/user.svg", include_bytes!("../assets/icons/licence/user.svg")),
    // The menu's icons, drawn heavier (stroke 2.75 for 2) to sit with the
    // labels' weight.
    ("icons/menu/activity.svg", include_bytes!("../assets/icons/menu/activity.svg")),
    ("icons/menu/archive.svg", include_bytes!("../assets/icons/menu/archive.svg")),
    ("icons/menu/arrow-left-right.svg", include_bytes!("../assets/icons/menu/arrow-left-right.svg")),
    ("icons/menu/chart-line.svg", include_bytes!("../assets/icons/menu/chart-line.svg")),
    ("icons/menu/key.svg", include_bytes!("../assets/icons/menu/key.svg")),
    ("icons/menu/folder.svg", include_bytes!("../assets/icons/menu/folder.svg")),
    ("icons/menu/info.svg", include_bytes!("../assets/icons/menu/info.svg")),
    ("icons/menu/network.svg", include_bytes!("../assets/icons/menu/network.svg")),
    ("icons/menu/panel-left-close.svg", include_bytes!("../assets/icons/menu/panel-left-close.svg")),
    ("icons/menu/panel-left-open.svg", include_bytes!("../assets/icons/menu/panel-left-open.svg")),
    ("icons/menu/power.svg", include_bytes!("../assets/icons/menu/power.svg")),
    ("icons/menu/refresh-cw.svg", include_bytes!("../assets/icons/menu/refresh-cw.svg")),
    ("icons/menu/server.svg", include_bytes!("../assets/icons/menu/server.svg")),
    ("icons/menu/settings.svg", include_bytes!("../assets/icons/menu/settings.svg")),
    ("icons/menu/sliders-horizontal.svg", include_bytes!("../assets/icons/menu/sliders-horizontal.svg")),
    ("icons/menu/square-terminal.svg", include_bytes!("../assets/icons/menu/square-terminal.svg")),
    ("icons/menu/wrench.svg", include_bytes!("../assets/icons/menu/wrench.svg")),
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
    // The Platform Manager's icon (assets/app-icon): the full design, and the
    // simplified one for small sizes (crate::ui::product_logo).
    ("logos/spm.png", include_bytes!("../assets/app-icon/128.png")),
    ("logos/spm-small.png", include_bytes!("../assets/app-icon/small-64.png")),
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
