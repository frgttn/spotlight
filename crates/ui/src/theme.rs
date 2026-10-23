//! Colours shared by the launcher and settings windows (RGBA, `0xRRGGBBAA`).

/// Background over GPUI's `Blurred` window. GPUI's accent-policy blur barely
/// blurs on Windows 11, so the tint has to hide what's behind on its own.
/// (The DWM acrylic system backdrop was tried: light grey, and the window lost
/// focus and hid itself after a few seconds.)
pub const BACKGROUND_TINT: u32 = 0x1c1c1ef7;
pub const TEXT: u32 = 0xecececff;
pub const TEXT_MUTED: u32 = 0xffffff80;
pub const TEXT_FAINT: u32 = 0xffffff60;
pub const HAIRLINE: u32 = 0xffffff14;
/// Raised surfaces (cards, key caps) on the window background.
pub const SURFACE: u32 = 0xffffff0a;
pub const SURFACE_HOVER: u32 = 0xffffff1a;
pub const SURFACE_BORDER: u32 = 0xffffff14;
pub const ACCENT: u32 = 0x4f7cffff;
pub const ACCENT_SOFT: u32 = 0x4f7cff2e;
pub const SUCCESS: u32 = 0x7fd4a0ff;
pub const ERROR: u32 = 0xff8a7aff;
