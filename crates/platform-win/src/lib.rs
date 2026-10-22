//! Windows integration for Spotlight (hotkey, app discovery, window placement,
//! measurements).
//!
//! Nothing in here knows about a UI framework: window handles are passed as raw
//! `isize` HWND values and events are delivered over `async_channel`.

#[cfg(not(windows))]
compile_error!("Spotlight is Windows-only");

pub mod apps;
pub mod hotkey;
pub mod metrics;
pub mod win;
