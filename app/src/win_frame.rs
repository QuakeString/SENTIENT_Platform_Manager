// Copyright © 2016-2026 The SENTIENT Authors

//! Rounded window corners on Windows, at Windows 11's 8 px (DPI-scaled).
//!
//! Windows 11 (build 22000 and later) rounds a window itself: each window
//! asks for it (DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND) and keeps its
//! native frame and title bar.
//!
//! Windows 10 has no such attribute, and a window region on a window with
//! the native frame makes the system drop to the basic, unthemed frame. So
//! there the window has no frame at all: GPUI's own title-bar mode
//! (`appears_transparent`), the caption drawn by the app ([`frame`]), and a
//! window procedure in front of GPUI's ([`win10`]) that makes the whole
//! window client area, hit-tests the resize edges inside it and clips the
//! window to a rounded region (square when maximized or snapped).
//!
//! On Linux every function here is a pass-through.

use gpui_kit::*;

/// Open a top-level window. The same as `gpui_kit::open_window`, plus the
/// rounded corners on Windows.
pub fn open_window<V: Render>(
    #[allow(unused_mut)] mut options: WindowOptions,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<(AnyWindowHandle, Entity<V>)> {
    #[cfg(windows)]
    {
        if win10::active() {
            // GPUI's own title bar mode: the app draws the caption.
            let title = options.titlebar.as_ref().and_then(|t| t.title.clone());
            options.titlebar = Some(TitlebarOptions { title, appears_transparent: true, ..Default::default() });
            // The window procedure goes in front of GPUI's from the window's
            // very first message (WM_NCCALCSIZE comes before CreateWindowEx
            // returns), so GPUI measures the window frameless from the start.
            let _hook = win10::CreateHook::install();
            return gpui_kit::open_window(options, cx, build);
        }
        return gpui_kit::open_window(options, cx, |window, cx| {
            win11::round_corners(window);
            build(window, cx)
        });
    }
    #[cfg(not(windows))]
    gpui_kit::open_window(options, cx, build)
}

/// The window's root view: on Windows 10 the caption (title, minimize,
/// maximize, close) over `content`, rounded 8 px with a 1 px border unless
/// maximized or snapped. Elsewhere `content` as it is.
pub fn frame(title: &'static str, content: impl IntoElement, window: &mut Window, cx: &mut App) -> AnyElement {
    #[cfg(windows)]
    if win10::active() {
        return win10::frame(title, content.into_any_element(), window, cx);
    }
    let _ = (title, window, cx);
    content.into_any_element()
}

/// The Windows build number (22000 and later is Windows 11).
#[cfg(windows)]
fn windows_build() -> u32 {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| unsafe {
        // RtlGetVersion: GetVersionEx answers what the manifest claims.
        let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        let module = GetModuleHandleW(ntdll.as_ptr());
        if module.is_null() {
            return 0;
        }
        let Some(f) = GetProcAddress(module, c"RtlGetVersion".as_ptr() as *const u8) else { return 0 };
        let f: unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32 = std::mem::transmute(f);
        let mut v: OSVERSIONINFOW = std::mem::zeroed();
        v.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
        if f(&mut v) == 0 { v.dwBuildNumber } else { 0 }
    })
}

#[cfg(windows)]
fn hwnd_of(window: &Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return None };
    Some(w.hwnd.get() as windows_sys::Win32::Foundation::HWND)
}

/// Windows 11: the system's own rounded corners and border.
#[cfg(windows)]
mod win11 {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE};

    pub fn round_corners(window: &gpui_kit::Window) {
        let Some(hwnd) = super::hwnd_of(window) else { return };
        let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        unsafe {
            DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &pref as *const _ as *const _, std::mem::size_of_val(&pref) as u32);
        }
    }
}

/// Windows 10: no frame, the app's caption, a rounded window region.
#[cfg(windows)]
mod win10 {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use gpui_kit::component::{ActiveTheme, TitleBar, h_flex, v_flex};
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::*;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, MonitorFromWindow, SetWindowRgn, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    /// Windows 11's window corner radius at 96 dpi.
    const RADIUS: i32 = 8;

    /// This is Windows 10 (or Server before 2025): the frame is the app's.
    pub fn active() -> bool {
        let build = super::windows_build();
        build != 0 && build < 22000
    }

    // -----------------------------------------------------------------------
    // Installing the window procedure at creation
    // -----------------------------------------------------------------------

    thread_local! {
        static HOOK: Cell<HHOOK> = const { Cell::new(std::ptr::null_mut()) };
        /// Each framed window: GPUI's window procedure, which ours calls on,
        /// and the region it has now (so a region change happens once).
        static WINDOWS: RefCell<HashMap<isize, Framed>> = RefCell::new(HashMap::new());
    }

    #[derive(Clone, Copy)]
    struct Framed {
        next: WNDPROC,
        region: Option<(i32, i32, i32, bool)>,
    }

    /// A CBT hook on this thread for the life of one `open_window`: it puts
    /// our window procedure in front of GPUI's as the window is created.
    pub struct CreateHook;

    impl CreateHook {
        pub fn install() -> Option<Self> {
            let hook = unsafe { SetWindowsHookExW(WH_CBT, Some(cbt), std::ptr::null_mut(), GetCurrentThreadId()) };
            if hook.is_null() {
                return None;
            }
            HOOK.with(|h| h.set(hook));
            Some(Self)
        }
    }

    impl Drop for CreateHook {
        fn drop(&mut self) {
            let hook = HOOK.with(|h| h.replace(std::ptr::null_mut()));
            if !hook.is_null() {
                unsafe { UnhookWindowsHookEx(hook) };
            }
        }
    }

    unsafe extern "system" fn cbt(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HCBT_CREATEWND as i32 {
            let hwnd = wparam as HWND;
            let cs = &*(*(lparam as *const CBT_CREATEWNDW)).lpcs;
            // GPUI's top-level windows only: its class, not a child, not a
            // popup (tool) window.
            let class_is_gpui = (cs.lpszClass as usize) > 0xFFFF && wide_eq(cs.lpszClass, "Zed::Window");
            let top_level = cs.style as u32 & WS_CHILD == 0 && cs.dwExStyle as u32 & WS_EX_TOOLWINDOW == 0;
            if class_is_gpui && top_level {
                subclass(hwnd);
            }
        }
        CallNextHookEx(HOOK.with(|h| h.get()), code, wparam, lparam)
    }

    unsafe fn wide_eq(p: *const u16, s: &str) -> bool {
        let mut i = 0;
        for c in s.encode_utf16() {
            if *p.add(i) != c {
                return false;
            }
            i += 1;
        }
        *p.add(i) == 0
    }

    unsafe fn subclass(hwnd: HWND) {
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, frame_proc as *const () as isize);
        if prev == 0 {
            return;
        }
        let next: WNDPROC = std::mem::transmute(prev);
        WINDOWS.with(|w| w.borrow_mut().insert(hwnd as isize, Framed { next, region: None }));
    }

    // -----------------------------------------------------------------------
    // The window procedure
    // -----------------------------------------------------------------------

    fn framed(hwnd: HWND) -> Option<Framed> {
        // A copy: the map is never borrowed across a call into GPUI, which
        // can re-enter this procedure.
        WINDOWS.with(|w| w.borrow().get(&(hwnd as isize)).copied())
    }

    unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let Some(f) = framed(hwnd) else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
        let next = |msg, wparam, lparam| CallWindowProcW(f.next, hwnd, msg, wparam, lparam);
        match msg {
            // The whole window is client area. Maximized, the window reaches
            // past the screen by its frame: the client stops at the screen.
            WM_NCCALCSIZE if wparam != 0 && has_frame(hwnd) => {
                if IsZoomed(hwnd) != 0 {
                    let params = &mut *(lparam as *mut NCCALCSIZE_PARAMS);
                    let (fx, fy) = frame_thickness(hwnd);
                    let r = &mut params.rgrc[0];
                    r.left += fx;
                    r.right -= fx;
                    r.top += fy;
                    r.bottom -= fy;
                }
                0
            }
            // Activation repaints the frame; there is none, and with a
            // region the system would paint its basic one (lparam -1: no
            // repaint).
            WM_NCACTIVATE => next(msg, wparam, -1),
            WM_NCHITTEST => {
                let inner = next(msg, wparam, lparam);
                resize_hit(hwnd, lparam, inner).unwrap_or(inner)
            }
            WM_SIZE | WM_WINDOWPOSCHANGED | WM_DPICHANGED => {
                let r = next(msg, wparam, lparam);
                update_region(hwnd);
                r
            }
            // The region was drawn for the frame GPUI had before: for a
            // style change (full screen) it is measured again.
            WM_STYLECHANGED => {
                let r = next(msg, wparam, lparam);
                update_region(hwnd);
                r
            }
            WM_NCDESTROY => {
                WINDOWS.with(|w| w.borrow_mut().remove(&(hwnd as isize)));
                SetWindowLongPtrW(hwnd, GWLP_WNDPROC, std::mem::transmute::<WNDPROC, isize>(f.next));
                next(msg, wparam, lparam)
            }
            _ => next(msg, wparam, lparam),
        }
    }

    /// The window has a resizable frame (not full screen, which drops it).
    unsafe fn has_frame(hwnd: HWND) -> bool {
        GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_THICKFRAME != 0
    }

    /// The resize frame Windows would draw outside the window, in pixels at
    /// the window's dpi: GPUI measures the same way.
    unsafe fn frame_thickness(hwnd: HWND) -> (i32, i32) {
        let dpi = GetDpiForWindow(hwnd);
        let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        (GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) + pad, GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + pad)
    }

    /// The resize edges and corners, inside the window. `inner` is GPUI's
    /// answer (drag area, caption buttons): the buttons keep their area but
    /// for the very corner.
    unsafe fn resize_hit(hwnd: HWND, lparam: LPARAM, inner: LRESULT) -> Option<LRESULT> {
        if IsZoomed(hwnd) != 0 || !has_frame(hwnd) {
            return None;
        }
        let pt = POINT { x: (lparam & 0xFFFF) as u16 as i16 as i32, y: ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32 };
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r) == 0 {
            return None;
        }
        let (fx, fy) = frame_thickness(hwnd);
        let (left, right) = (pt.x < r.left + fx, pt.x >= r.right - fx);
        let (top, bottom) = (pt.y < r.top + fy, pt.y >= r.bottom - fy);
        let button = matches!(inner as u32, HTCLOSE | HTMAXBUTTON | HTMINBUTTON);
        if button && !(top && right) {
            return None;
        }
        // The corners take a longer stretch of each edge: easier to catch.
        let (cx, cy) = (fx * 2, fy * 2);
        let (near_left, near_right) = (pt.x < r.left + cx, pt.x >= r.right - cx);
        let (near_top, near_bottom) = (pt.y < r.top + cy, pt.y >= r.bottom - cy);
        let hit = if (top && near_left) || (left && near_top) {
            HTTOPLEFT
        } else if (top && near_right) || (right && near_top) {
            HTTOPRIGHT
        } else if (bottom && near_left) || (left && near_bottom) {
            HTBOTTOMLEFT
        } else if (bottom && near_right) || (right && near_bottom) {
            HTBOTTOMRIGHT
        } else if top {
            HTTOP
        } else if bottom {
            HTBOTTOM
        } else if left {
            HTLEFT
        } else if right {
            HTRIGHT
        } else {
            return None;
        };
        Some(hit as LRESULT)
    }

    /// Clip the window to a rounded rectangle; square (no region) when
    /// maximized, snapped or full screen.
    unsafe fn update_region(hwnd: HWND) {
        let Some(f) = framed(hwnd) else { return };
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r) == 0 || IsIconic(hwnd) != 0 {
            return;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let radius = (RADIUS * GetDpiForWindow(hwnd) as i32 + 48) / 96;
        let square = square(hwnd);
        let key = (w, h, radius, square);
        if f.region == Some(key) {
            return;
        }
        // Noted first: SetWindowRgn sends WM_WINDOWPOSCHANGED back here.
        WINDOWS.with(|m| {
            if let Some(e) = m.borrow_mut().get_mut(&(hwnd as isize)) {
                e.region = Some(key);
            }
        });
        if square {
            SetWindowRgn(hwnd, std::ptr::null_mut(), 1);
        } else {
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2);
            // The system owns the region once it is set.
            if !rgn.is_null() && SetWindowRgn(hwnd, rgn, 1) == 0 {
                DeleteObject(rgn);
            }
        }
    }

    /// Maximized, snapped (Aero Snap: a half, a quadrant, full height) or
    /// full screen: corners square, as Windows 11 draws them then.
    pub(super) unsafe fn square(hwnd: HWND) -> bool {
        IsZoomed(hwnd) != 0 || !has_frame(hwnd) || snapped(hwnd)
    }

    unsafe fn snapped(hwnd: HWND) -> bool {
        // user32 has IsWindowArranged since Windows 7 (documented from
        // Windows 10 build 20348): looked up, so a build without it still
        // starts.
        type Arranged = unsafe extern "system" fn(HWND) -> i32;
        static ARRANGED: std::sync::OnceLock<Option<Arranged>> = std::sync::OnceLock::new();
        let arranged = ARRANGED.get_or_init(|| {
            let user32: Vec<u16> = "user32.dll\0".encode_utf16().collect();
            let module = GetModuleHandleW(user32.as_ptr());
            if module.is_null() {
                return None;
            }
            GetProcAddress(module, c"IsWindowArranged".as_ptr() as *const u8).map(|f| std::mem::transmute::<_, Arranged>(f))
        });
        if let Some(f) = arranged {
            return f(hwnd) != 0;
        }
        // Without it: snapped windows keep their earlier rectangle as the
        // restore rectangle, and fill the work area's height or a corner.
        let mut r: RECT = std::mem::zeroed();
        let mut p: WINDOWPLACEMENT = std::mem::zeroed();
        p.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetWindowRect(hwnd, &mut r) == 0
            || GetWindowPlacement(hwnd, &mut p) == 0
            || GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi) == 0
        {
            return false;
        }
        let work = mi.rcWork;
        // The restore rectangle is in work-area coordinates.
        let (dx, dy) = (work.left - mi.rcMonitor.left, work.top - mi.rcMonitor.top);
        let n = p.rcNormalPosition;
        let moved = (n.left + dx, n.top + dy, n.right + dx, n.bottom + dy) != (r.left, r.top, r.right, r.bottom);
        let sides = [r.left == work.left, r.top == work.top, r.right == work.right, r.bottom == work.bottom].iter().filter(|s| **s).count();
        moved && sides >= 2
    }

    // -----------------------------------------------------------------------
    // The caption and the rounded root
    // -----------------------------------------------------------------------

    pub fn frame(title: &'static str, content: AnyElement, window: &mut Window, cx: &mut App) -> AnyElement {
        let square = super::hwnd_of(window).is_some_and(|h| unsafe { square(h) });
        let theme = cx.theme().clone();
        let dark = theme.mode.is_dark();
        // Windows 11's window border: a dark hairline on light windows, a
        // light one on dark windows (its default, accent colour off).
        let border = if dark { hsla(0., 0., 1., 0.12) } else { hsla(0., 0., 0., 0.20) };
        let active = window.is_window_active();
        let caption = TitleBar::new()
            .bg(theme.background)
            .border_color(theme.border)
            .child(
                h_flex()
                    .h_full()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(if active { theme.foreground } else { theme.muted_foreground })
                    .child(title),
            );
        v_flex()
            .size_full()
            .relative()
            .when(!square, |d| d.rounded(px(RADIUS as f32)))
            .overflow_hidden()
            .child(caption)
            .child(div().flex_1().min_h_0().w_full().relative().child(content))
            // The border over everything (a root's own border would sit
            // under its children's backgrounds); it takes no clicks.
            .when(!square, |d| {
                d.child(div().absolute().inset_0().rounded(px(RADIUS as f32)).border_1().border_color(border))
            })
            .into_any_element()
    }
}
