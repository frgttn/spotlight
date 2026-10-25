//! Raw Win32 window placement for the launcher window, and the apps that
//! have windows open.
//!
//! UI frameworks generally have no "hide but keep everything alive" call, so the
//! launcher takes the HWND and drives visibility and placement directly.

use std::{ffi::c_void, mem::size_of};

use spotlight_core::clip::IgnoredApp;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT},
        Globalization::GetUserDefaultLocaleName,
        Graphics::{
            Dwm::{
                DWMWA_BORDER_COLOR, DWMWA_CLOAKED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
                DwmGetWindowAttribute, DwmSetWindowAttribute,
            },
            Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, COLORONCOLOR, CreateCompatibleBitmap,
                CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits,
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
                ReleaseDC, SRCCOPY, SelectObject, SetStretchBltMode, StretchBlt,
            },
        },
        System::{
            Console::{ATTACH_PARENT_PROCESS, AttachConsole},
            Threading::GetCurrentProcessId,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{
                EnumWindows, GW_OWNER, GWL_EXSTYLE, GetCursorPos, GetWindow, GetWindowLongPtrW,
                GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId, HWND_TOPMOST,
                IsWindowVisible, SET_WINDOW_POS_FLAGS, SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SW_HIDE,
                SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
                SetForegroundWindow, SetWindowPos, ShowWindow, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::BOOL,
};

use crate::clipboard;

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

/// The apps with a window on screen (as the taskbar would show them), each
/// once, by name; not Spotlight. For choosing apps in Settings.
pub fn apps_with_windows() -> Vec<IgnoredApp> {
    let windows = app_windows();
    let own = unsafe { GetCurrentProcessId() };
    let mut apps: Vec<IgnoredApp> = Vec::new();
    for window in windows {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd(window), Some(&mut pid)) };
        if pid == own {
            continue;
        }
        let Some(path) = clipboard::window_program(window) else {
            continue;
        };
        if !IgnoredApp::contains(&apps, &path) {
            let name = clipboard::app_name(&path);
            apps.push(IgnoredApp { path, name });
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// The windows a user would call an app's window (as the taskbar shows
/// them), front to back, as raw HWNDs. Includes Spotlight's own.
pub(crate) fn app_windows() -> Vec<isize> {
    let mut windows: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(&mut windows as *mut Vec<isize> as isize),
        );
    }
    windows
}

/// `EnumWindows` callback: keeps windows a user would call an app's window.
unsafe extern "system" fn collect_window(window: HWND, windows: LPARAM) -> BOOL {
    unsafe {
        let ex_style = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        );
        let shown = IsWindowVisible(window).as_bool()
            && GetWindow(window, GW_OWNER).is_err()
            && ex_style & WS_EX_TOOLWINDOW.0 == 0
            && GetWindowTextLengthW(window) > 0
            // Hidden by DWM: suspended Store apps, other virtual desktops.
            && cloaked == 0;
        if shown {
            (*(windows.0 as *mut Vec<isize>)).push(window.0 as isize);
        }
        true.into()
    }
}

/// Asks DWM for Windows 11 rounded corners (frameless popups don't get them by default).
pub fn round_corners(raw: isize) {
    unsafe {
        let preference = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd(raw),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const c_void,
            size_of::<i32>() as u32,
        );
    }
}

/// Sets the colour (`0xRRGGBB`) of the thin border Windows 11 draws around
/// the window, along its rounded corners.
pub fn set_border_color(raw: isize, rgb: u32) {
    // COLORREF is 0x00BBGGRR.
    let colorref = ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd(raw),
            DWMWA_BORDER_COLOR,
            &colorref as *const _ as *const c_void,
            size_of::<u32>() as u32,
        );
    }
}

/// Shows the window topmost, centered (as Raycast places it) in
/// the work area of the monitor under the mouse cursor, then takes focus.
///
/// `logical_size` (in 96-DPI units) is scaled by that monitor's DPI and applied;
/// pass `None` to keep the window's current size. With `client_height`, the
/// window is placed as if it had `logical_size` but is only that tall inside
/// (a collapsed launcher), so growing it later keeps its top edge.
///
/// GPUI misses a size change made here while it updates the window (see
/// [`resize`]), so the window should already have the size asked for.
///
/// Returns how bright the screen is where the window now is (0 black, 1
/// white), read just before it showed, so the window can stay dark over a
/// light background.
pub fn show_on_active_monitor(
    raw: isize,
    logical_size: Option<(f32, f32)>,
    client_height: Option<f32>,
) -> Option<f32> {
    let mut behind = None;
    unsafe {
        let hwnd = hwnd(raw);
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut rect = RECT::default();

        if GetMonitorInfoW(monitor, &mut info).as_bool() && GetWindowRect(hwnd, &mut rect).is_ok() {
            let (width, height, size_flag) = match logical_size {
                Some((width, height)) => {
                    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
                    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                    let scale = dpi_x as f32 / 96.;
                    (
                        (width * scale).round() as i32,
                        (height * scale).round() as i32,
                        SET_WINDOW_POS_FLAGS(0),
                    )
                }
                None => (rect.right - rect.left, rect.bottom - rect.top, SWP_NOSIZE),
            };
            let work = info.rcWork;
            let x = work.left + (work.right - work.left - width) / 2;
            let y = work.top + (work.bottom - work.top - height) / 2;
            let height = match client_height {
                Some(client_height) => {
                    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
                    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
                    window_height_for_client(client_height, dpi_x)
                }
                None => height,
            };
            // The whole window as it will be, not just a collapsed search row.
            behind = brightness(x, y, width, height.max(rect.bottom - rect.top));
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                size_flag | SWP_SHOWWINDOW,
            );
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        // Allowed because our process just received the hotkey input event.
        let _ = SetForegroundWindow(hwnd);
    }
    behind
}

/// The mean brightness (0 black, 1 white) of the screen in a rectangle of
/// physical pixels, from a small sample of it.
fn brightness(x: i32, y: i32, width: i32, height: i32) -> Option<f32> {
    const SAMPLE: (i32, i32) = (48, 32);
    if width <= 0 || height <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, SAMPLE.0, SAMPLE.1);
        let old = SelectObject(memory, bitmap.into());
        // Point sampling: a few hundred pixels say enough, and fast.
        SetStretchBltMode(memory, COLORONCOLOR);
        let copied = StretchBlt(
            memory,
            0,
            0,
            SAMPLE.0,
            SAMPLE.1,
            Some(screen),
            x,
            y,
            width,
            height,
            SRCCOPY,
        )
        .as_bool();
        SelectObject(memory, old);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SAMPLE.0,
                biHeight: -SAMPLE.1,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (SAMPLE.0 * SAMPLE.1 * 4) as usize];
        let rows = copied.then(|| {
            GetDIBits(
                memory,
                bitmap,
                0,
                SAMPLE.1 as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            )
        });
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        if rows.unwrap_or(0) == 0 {
            return None;
        }
        Some(mean_brightness(&bgra))
    }
}

/// The mean luma of BGRA pixels, 0 to 1.
fn mean_brightness(bgra: &[u8]) -> f32 {
    let (pixels, _) = bgra.as_chunks::<4>();
    let count = pixels.len().max(1) as f32;
    let sum: f32 = pixels
        .iter()
        .map(|pixel| 0.0722 * pixel[0] as f32 + 0.7152 * pixel[1] as f32 + 0.2126 * pixel[2] as f32)
        .sum();
    sum / count / 255.
}

/// Changes the window's size, keeping its top-left corner and z-order.
/// `logical_size` and `client_height` are as for [`show_on_active_monitor`].
///
/// GPUI learns the new size from WM_SIZE, which it can only take while no
/// update of the window is running: call this from a spawned task, as GPUI's
/// own resize does, or its layout keeps the old size.
pub fn resize(raw: isize, logical_size: (f32, f32), client_height: Option<f32>) {
    unsafe {
        let hwnd = hwnd(raw);
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = dpi as f32 / 96.;
        let width = (logical_size.0 * scale).round() as i32;
        let height = match client_height {
            Some(client_height) => window_height_for_client(client_height, dpi),
            None => (logical_size.1 * scale).round() as i32,
        };
        if (width, height) == (rect.right - rect.left, rect.bottom - rect.top) {
            return;
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// The window height whose client area is `logical_height` tall: GPUI insets
/// it by the resize frame at the bottom and a pixel at the top.
fn window_height_for_client(logical_height: f32, dpi: u32) -> i32 {
    let scale = dpi as f32 / 96.;
    let frame = unsafe {
        GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    };
    (logical_height * scale).round() as i32 + scale.round() as i32 + frame
}

/// Shows the window topmost, horizontally centered near the bottom of the work
/// area of the monitor under the mouse cursor, without activating it: focus
/// stays with whatever window has it. `logical_size` is in 96-DPI units.
pub fn show_without_focus_near_bottom(raw: isize, logical_size: (f32, f32)) {
    unsafe {
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x as f32 / 96.;
        // GPUI's client area is inset from the window rect (resize frame on the
        // left, right and bottom, a pixel on top), so grow the window by that to
        // get a client of exactly `logical_size`; otherwise the content sits low.
        let frame = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi_x)
            + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi_x);
        let top = scale.round() as i32;
        let width = (logical_size.0 * scale).round() as i32;
        let height = (logical_size.1 * scale).round() as i32;
        let (win_width, win_height) = (width + 2 * frame, height + top + frame);
        let work = info.rcWork;
        let x = work.left + (work.right - work.left - win_width) / 2;
        let y = work.bottom - win_height + frame - (64. * scale).round() as i32;
        let _ = SetWindowPos(
            hwnd(raw),
            Some(HWND_TOPMOST),
            x,
            y,
            win_width,
            win_height,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );
    }
}

/// Lets a GUI-subsystem process print to the console it was started from, if
/// any. Output that is redirected (to a file or a pipe) already works without it.
pub fn attach_parent_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

pub fn hide(raw: isize) {
    unsafe {
        let _ = ShowWindow(hwnd(raw), SW_HIDE);
    }
}

/// Brings an already visible window to the front.
/// Brings our window to the front when Windows wouldn't let us (no hotkey
/// or click of ours came just before): a no-op mouse input makes our
/// process the last to send input, which allows it.
pub fn take_foreground(raw: isize) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput};
    unsafe {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            ..Default::default()
        };
        SendInput(&[input], size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(hwnd(raw));
    }
}

pub fn focus(raw: isize) {
    unsafe {
        let _ = SetForegroundWindow(hwnd(raw));
    }
}

/// The user's language as a two-letter code (`"ru"`), from their Windows
/// regional format; `"en"` when it can't be told.
pub fn user_language() -> String {
    let mut name = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(&mut name) };
    let name = String::from_utf16_lossy(&name[..(len.max(1) - 1) as usize]);
    match name.split('-').next() {
        Some(language) if language.len() >= 2 => language.to_lowercase(),
        _ => "en".into(),
    }
}

pub fn is_visible(raw: isize) -> bool {
    unsafe { IsWindowVisible(hwnd(raw)).as_bool() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_brightness() {
        let white = [255u8; 8];
        assert!((mean_brightness(&white) - 1.).abs() < 1e-3);
        // One black pixel and one pure green (BGRA).
        let mixed = [0, 0, 0, 255, 0, 255, 0, 255];
        assert!((mean_brightness(&mixed) - 0.3576).abs() < 1e-3);
    }
}
