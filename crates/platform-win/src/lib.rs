//! Windows integration for Spotlight (hotkey, clipboard, single instance, app discovery
//! and icons, running commands, autostart, window placement, measurements).
//!
//! Nothing in here knows about a UI framework: window handles are passed as raw
//! `isize` HWND values and events are delivered over `async_channel`.

#[cfg(not(windows))]
compile_error!("Spotlight is Windows-only");

pub mod apps;
pub mod autostart;
pub mod clipboard;
pub mod commands;
pub mod dialog;
pub mod drives;
pub mod expander;
pub mod favicon;
pub mod fswalk;
pub mod fswatch;
pub mod hotkey;
pub mod icons;
pub mod index_service;
pub mod instance;
pub mod links;
pub mod metrics;
pub mod net;
pub mod ntfs;
pub mod protocol;
pub mod registry;
pub mod selection;
pub mod system;
pub mod vault;
pub mod win;
pub mod wm;
