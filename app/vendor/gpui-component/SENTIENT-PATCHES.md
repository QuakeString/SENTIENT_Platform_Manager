# gpui-component 0.7.3, patched for SENTIENT Platform Manager

This is crates.io `gpui-component` 0.7.3 with the changes below, used through
`[patch.crates-io]` in `app/Cargo.toml`. The crate's `tests/` folder is left out.

1. **Tooltips wrap** (`src/tooltip.rs`): every tooltip is at most 320 px wide,
   with a 19 px line height, so long help wraps instead of running as one
   line. Padding, radius and shadow are as before.
2. **Popup menus open with a motion** (`src/menu/popup_menu.rs`): every
   `PopupMenu` (dropdown menus, context menus, submenus) fades in over
   140 ms, rising 4 px, its shadow ramping with the fade, the Platform
   Manager's menu motion (`app/src/ui.rs` `menu_appear`). Upstream opens
   them with none. Under `App::reduce_motion` GPUI draws the end state.
