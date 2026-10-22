//! UI-independent Windows integration for the launcher spike (hotkey, app
//! discovery, window placement, fuzzy search, measurements).
//!
//! Nothing in here knows about a UI framework: window handles are passed as raw
//! `isize` HWND values and events are delivered over `async_channel`.

#[cfg(not(windows))]
compile_error!("the launcher spikes are Windows-only");

pub mod apps;
pub mod hotkey;
pub mod metrics;
pub mod search;
pub mod win;
