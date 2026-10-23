//! Windows integration for Spotlight (hotkey, single instance, app discovery
//! and icons, running commands, autostart, window placement, measurements).
//!
//! Nothing in here knows about a UI framework: window handles are passed as raw
//! `isize` HWND values and events are delivered over `async_channel`.

#[cfg(not(windows))]
compile_error!("Spotlight is Windows-only");

pub mod apps;
pub mod autostart;
pub mod commands;
pub mod drives;
pub mod hotkey;
pub mod icons;
pub mod instance;
pub mod links;
pub mod metrics;
pub mod net;
pub mod registry;
pub mod system;
pub mod win;
