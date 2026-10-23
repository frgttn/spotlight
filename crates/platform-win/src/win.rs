//! Raw Win32 window placement for the launcher window.
//!
//! UI frameworks generally have no "hide but keep everything alive" call, so the
//! launcher takes the HWND and drives visibility and placement directly.

use std::{ffi::c_void, mem::size_of};

use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::{
        Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute},
        Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    },
    System::Console::{ATTACH_PARENT_PROCESS, AttachConsole},
    UI::{
        HiDpi::{GetDpiForMonitor, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI},
        WindowsAndMessaging::{
            GetCursorPos, GetWindowRect, HWND_TOPMOST, IsWindowVisible, SET_WINDOW_POS_FLAGS,
            SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE,
            SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow,
        },
    },
};

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
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

/// Shows the window topmost, horizontally centered and a fifth of the way down
/// the work area of the monitor under the mouse cursor, then takes focus.
///
/// `logical_size` (in 96-DPI units) is scaled by that monitor's DPI and applied;
/// pass `None` to keep the window's current size.
pub fn show_on_active_monitor(raw: isize, logical_size: Option<(f32, f32)>) {
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
            let y = work.top + (work.bottom - work.top - height) / 5;
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
pub fn focus(raw: isize) {
    unsafe {
        let _ = SetForegroundWindow(hwnd(raw));
    }
}

pub fn is_visible(raw: isize) -> bool {
    unsafe { IsWindowVisible(hwnd(raw)).as_bool() }
}
