// Copyright © 2016-2026 The SENTIENT Authors

//! The Platform Manager's own icon on its window (Windows). The program is
//! one file for Setup and the Platform Manager: icon 1 (build.rs) is the
//! Setup icon, which GPUI gives every window; the Platform Manager's window
//! takes icon 2 here, so its taskbar button and title bar show the new icon.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, SM_CXICON, SM_CXSMICON, WM_SETICON};

/// Give the window the Platform Manager's icon (resource 2).
pub fn use_manager_icon(window: &gpui_kit::Window) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return };
    let hwnd = w.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        for (which, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let size = GetSystemMetrics(metric);
            let icon = LoadImageW(module, 2usize as *const u16, IMAGE_ICON, size, size, 0);
            if !icon.is_null() {
                SendMessageW(hwnd, WM_SETICON, which as usize, icon as isize);
            }
        }
    }
}
