# gpui-pre-linux 0.3.9, patched for SENTIENT Platform Manager

This is crates.io `gpui-pre-linux` 0.3.9 with the changes below, used through
`[patch.crates-io]` in `app/Cargo.toml`. Drop the copy once upstream ships
the same.

1. **ext-background-effect-v1** (`src/linux/wayland/client.rs`, `window.rs`):
   binds `ext_background_effect_manager_v1`. A `Blurred` window gets a
   background-effect object and a blur region covering the whole surface.
   KWin 6.8 offers only this protocol, not `org_kde_kwin_blur_manager`, so
   the stock crate showed no blur on current Plasma. The KDE protocol stays
   as the fallback, now with an explicit whole-surface region.
2. **KDE blur on X11** (`src/linux/x11/window.rs`): `set_background_appearance`
   sets `_KDE_NET_WM_BLUR_BEHIND_REGION` (an empty region means the whole
   window) for `Blurred` and removes it otherwise.
3. `SPM_DEBUG_BLUR=1` prints which blur protocol the compositor offers.
