//! The launcher's window on macOS: showing it centred on the screen under the
//! mouse, hiding it, resizing it, and the HUD near the bottom. GPUI has no
//! "hide but keep everything alive" call, so this drives the `NSWindow`
//! behind the view GPUI made, as `loupe_platform_win::win` drives the HWND.
//!
//! The handle is the window's `NSView` pointer as an `isize`
//! ([`native_handle`]). AppKit wants all of this on the main thread, which
//! is where GPUI runs the UI.
//!
//! The app stays out of the Dock and Cmd+Tab ([`hide_from_dock`]).
//!
//! Windows open at GPUI's pop-up level, above floating panels. Their layer
//! gives them rounded corners and a border, as Windows' DWM does there, and
//! their blurred background blurs ([`round_corners`]).
//!
//! Not yet: the list of apps with windows (Settings) is empty.

use loupe_core::clip::IgnoredApp;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The OS handle of a window the UI made (its `NSView`), for the other calls
/// in this crate. `None` if it isn't an AppKit window.
pub fn native_handle(window: &impl HasWindowHandle) -> Option<isize> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::AppKit(handle) => Some(handle.ns_view.as_ptr() as isize),
        _ => None,
    }
}

/// The apps with a window on screen, each once, by name; not Loupe. For
/// choosing apps in Settings. Not yet on macOS.
pub fn apps_with_windows() -> Vec<IgnoredApp> {
    Vec::new()
}

/// Rounded corners for the frameless window. Each window calls this once as
/// it's made, so it also makes the blurred background blur.
pub fn round_corners(raw: isize) {
    #[cfg(target_os = "macos")]
    {
        appkit::blur_behind(raw);
        appkit::round_corners(raw);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = raw;
}

/// Clips the window to the top `logical_height` of its content. Not yet on
/// macOS.
pub fn clip_height(_raw: isize, _logical_height: Option<f32>) {}

/// The colour (`0xRRGGBB`) of a border around the window: one pixel, inside
/// its rounded edge.
pub fn set_border_color(raw: isize, rgb: u32) {
    #[cfg(target_os = "macos")]
    appkit::set_border_color(raw, rgb);
    #[cfg(not(target_os = "macos"))]
    let _ = (raw, rgb);
}

/// Lets a GUI process print to the terminal it was started from: on macOS it
/// always can.
pub fn attach_parent_console() {}

/// The user's language as a two-letter code (`"ru"`); `"en"` when it can't
/// be told.
pub fn user_language() -> String {
    // `LANG=ru_RU.UTF-8` → `ru`, until macOS's own preferred languages are read.
    std::env::var("LANG")
        .ok()
        .and_then(|lang| lang.get(..2).map(str::to_lowercase))
        .filter(|code| code.chars().all(|ch| ch.is_ascii_lowercase()))
        .unwrap_or_else(|| "en".to_owned())
}

#[cfg(target_os = "macos")]
pub use appkit::*;

/// Without AppKit (building on Windows with `--cfg loupe_mac_stub`): nothing
/// to show, so these do nothing.
#[cfg(not(target_os = "macos"))]
mod no_appkit {
    pub fn show_on_active_monitor(
        _raw: isize,
        _logical_size: Option<(f32, f32)>,
        _client_height: Option<f32>,
    ) {
    }
    pub fn resize(_raw: isize, _logical_size: (f32, f32), _client_height: Option<f32>) {}
    pub fn show_without_focus_near_bottom(_raw: isize, _logical_size: (f32, f32)) {}
    pub fn hide(_raw: isize) {}
    pub fn hide_from_dock() {}
    pub fn take_foreground(_raw: isize) {}
    pub fn focus(_raw: isize) {}
    pub fn is_visible(_raw: isize) -> bool {
        false
    }
}
#[cfg(not(target_os = "macos"))]
pub use no_appkit::*;

// Some of these AppKit calls are `unsafe` in objc2 and some aren't; one
// `unsafe` block around each keeps it the same either way.
#[cfg(target_os = "macos")]
#[allow(unused_unsafe)]
mod appkit {
    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSEvent, NSScreen, NSView,
        NSVisualEffectMaterial, NSVisualEffectView, NSWindow,
    };
    use objc2_core_graphics::CGColor;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use objc2_quartz_core::CALayer;

    /// The windows' corner radius in points: Windows 11's, which the
    /// launcher draws its own background with too (`WINDOW_RADIUS`).
    const CORNER_RADIUS: f64 = 8.;
    /// How far above the bottom of the screen's usable area the HUD sits.
    const HUD_MARGIN: f64 = 64.;

    /// The window holding the view `raw` points at.
    fn window(raw: isize) -> Option<Retained<NSWindow>> {
        if raw == 0 {
            return None;
        }
        // SAFETY: `raw` is the `NSView` pointer `native_handle` returned, for
        // a window GPUI keeps alive for the app's lifetime.
        let view = unsafe { &*(raw as *const NSView) };
        unsafe { view.window() }
    }

    /// Makes GPUI's blur behind the window actually blur. For a `Blurred`
    /// window GPUI puts an `NSVisualEffectView` under its own view with the
    /// `Selection` material, which on current macOS is a flat colour with no
    /// backdrop layer: what's behind showed through sharp. A window material
    /// blurs. GPUI makes that view once, with the window.
    pub(super) fn blur_behind(raw: isize) {
        let (Some(_), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        let Some(content) = (unsafe { window.contentView() }) else {
            return;
        };
        let subviews = unsafe { content.subviews() };
        for ix in 0..unsafe { subviews.count() } {
            let view = unsafe { subviews.objectAtIndex(ix) };
            if let Ok(effect) = view.downcast::<NSVisualEffectView>() {
                unsafe { effect.setMaterial(NSVisualEffectMaterial::HUDWindow) };
            }
        }
    }

    /// The layer of the window's content view, which holds GPUI's view and
    /// the blur under it: rounding and bordering it does both.
    fn content_layer(window: &NSWindow) -> Option<Retained<CALayer>> {
        let content = unsafe { window.contentView() }?;
        unsafe {
            content.setWantsLayer(true);
            content.layer()
        }
    }

    /// Clips the window, blur included, to rounded corners. The shadow
    /// follows what's drawn, so it's worked out again.
    pub(super) fn round_corners(raw: isize) {
        let (Some(_), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        let Some(layer) = content_layer(&window) else {
            return;
        };
        unsafe {
            layer.setCornerRadius(CORNER_RADIUS);
            layer.setMasksToBounds(true);
            window.invalidateShadow();
        }
    }

    /// A one-pixel border in `rgb` (`0xRRGGBB`), along the rounded edge.
    pub(super) fn set_border_color(raw: isize, rgb: u32) {
        let (Some(_), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        let Some(layer) = content_layer(&window) else {
            return;
        };
        let channel = |shift: u32| f64::from((rgb >> shift) & 0xff) / 255.;
        let color = CGColor::new_srgb(channel(16), channel(8), channel(0), 1.);
        let scale = unsafe { window.backingScaleFactor() };
        unsafe {
            layer.setBorderWidth(1. / scale);
            layer.setBorderColor(Some(&color));
        }
    }

    fn main_thread() -> Option<MainThreadMarker> {
        let marker = MainThreadMarker::new();
        if marker.is_none() {
            eprintln!("[win] called off the main thread; AppKit needs the main one");
        }
        marker
    }

    /// The usable area (without the menu bar and the Dock) of the screen under
    /// the mouse, else the main screen's.
    fn active_screen_area(mtm: MainThreadMarker) -> Option<NSRect> {
        let mouse = unsafe { NSEvent::mouseLocation() };
        let screens = unsafe { NSScreen::screens(mtm) };
        let count = unsafe { screens.count() };
        let mut first = None;
        for ix in 0..count {
            let screen = unsafe { screens.objectAtIndex(ix) };
            let frame = unsafe { screen.frame() };
            let area = unsafe { screen.visibleFrame() };
            first.get_or_insert(area);
            let inside_x = mouse.x >= frame.origin.x && mouse.x < frame.origin.x + frame.size.width;
            let inside_y =
                mouse.y >= frame.origin.y && mouse.y < frame.origin.y + frame.size.height;
            if inside_x && inside_y {
                return Some(area);
            }
        }
        first
    }

    fn app(mtm: MainThreadMarker) -> Retained<NSApplication> {
        NSApplication::sharedApplication(mtm)
    }

    /// Shows the window above others, centred (as Raycast places it) in the
    /// usable area of the screen under the mouse, then takes focus.
    ///
    /// `logical_size` is in points; `None` keeps the window's size. With
    /// `client_height`, the window is placed as if it had `logical_size` but
    /// is only that tall (a collapsed launcher), so growing it later keeps
    /// its top edge.
    pub fn show_on_active_monitor(
        raw: isize,
        logical_size: Option<(f32, f32)>,
        client_height: Option<f32>,
    ) {
        let (Some(mtm), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        let current = unsafe { window.frame() };
        let (width, full_height) = match logical_size {
            Some((width, height)) => (f64::from(width), f64::from(height)),
            None => (current.size.width, current.size.height),
        };
        let height = client_height.map_or(full_height, f64::from);
        if let Some(area) = active_screen_area(mtm) {
            let x = area.origin.x + (area.size.width - width) / 2.;
            // AppKit counts y up from the bottom: place the top edge where a
            // window of the full height would have it.
            let top = area.origin.y + area.size.height - (area.size.height - full_height) / 2.;
            let frame = NSRect::new(NSPoint::new(x, top - height), NSSize::new(width, height));
            unsafe {
                window.setFrame_display(frame, true);
                window.invalidateShadow();
            }
        }
        unsafe {
            app(mtm).activate();
            window.makeKeyAndOrderFront(None);
        }
    }

    /// Changes the window's size, keeping its top-left corner.
    /// `logical_size` and `client_height` are as for [`show_on_active_monitor`].
    pub fn resize(raw: isize, logical_size: (f32, f32), client_height: Option<f32>) {
        let (Some(_), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        let current = unsafe { window.frame() };
        let top = current.origin.y + current.size.height;
        let width = f64::from(logical_size.0);
        let height = f64::from(client_height.unwrap_or(logical_size.1));
        let frame = NSRect::new(
            NSPoint::new(current.origin.x, top - height),
            NSSize::new(width, height),
        );
        unsafe {
            window.setFrame_display(frame, true);
            // The shadow is worked out from what's drawn: else it keeps the
            // old height.
            window.invalidateShadow();
        }
    }

    /// Shows the window above others, horizontally centred near the bottom of
    /// the usable area of the screen under the mouse, without taking focus.
    /// `logical_size` is in points.
    pub fn show_without_focus_near_bottom(raw: isize, logical_size: (f32, f32)) {
        let (Some(mtm), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        let Some(area) = active_screen_area(mtm) else {
            return;
        };
        let (width, height) = (f64::from(logical_size.0), f64::from(logical_size.1));
        let x = area.origin.x + (area.size.width - width) / 2.;
        let frame = NSRect::new(
            NSPoint::new(x, area.origin.y + HUD_MARGIN),
            NSSize::new(width, height),
        );
        unsafe {
            window.setFrame_display(frame, true);
            window.invalidateShadow();
            window.orderFrontRegardless();
        }
    }

    /// Hides the window. With no other window of ours left on screen, the
    /// app hides too, so the app that was in front before gets focus back.
    /// Only windows that can take focus count: the tray icon's status bar
    /// window is always on screen.
    pub fn hide(raw: isize) {
        let (Some(mtm), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        unsafe { window.orderOut(None) };
        let app = app(mtm);
        let windows = unsafe { app.windows() };
        let others_visible = (0..unsafe { windows.count() }).any(|ix| {
            let other = unsafe { windows.objectAtIndex(ix) };
            unsafe { other.isVisible() && other.canBecomeKeyWindow() }
        });
        if !others_visible {
            unsafe { app.hide(None) };
        }
    }

    /// Keeps the app out of the Dock and Cmd+Tab, as a menu-bar app: it shows
    /// windows and takes focus, but has no Dock icon or menu bar of its own.
    /// Call once the UI has started: GPUI makes the app a regular one when it
    /// finishes launching. (The bundle will say so too, with `LSUIElement`.)
    pub fn hide_from_dock() {
        let Some(mtm) = main_thread() else {
            return;
        };
        let app = app(mtm);
        if !unsafe { app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) } {
            eprintln!("[win] could not keep the app out of the Dock");
        }
    }

    /// Brings our window to the front and gives it focus.
    pub fn take_foreground(raw: isize) {
        focus(raw);
    }

    /// Brings an already visible window to the front and gives it focus.
    pub fn focus(raw: isize) {
        let (Some(mtm), Some(window)) = (main_thread(), window(raw)) else {
            return;
        };
        unsafe {
            app(mtm).activate();
            window.makeKeyAndOrderFront(None);
        }
    }

    pub fn is_visible(raw: isize) -> bool {
        window(raw).is_some_and(|window| unsafe { window.isVisible() })
    }
}
