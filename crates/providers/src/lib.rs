//! Built-in root-search providers. Each one only describes results; the app
//! runs their actions (see `spotlight_core::item::Command`).

pub mod apps;
pub mod calc;
pub mod system;
pub mod web;

use std::sync::Arc;

use spotlight_core::provider::Provider;

/// Symbols from Segoe Fluent Icons (Windows 11), at the same code points in
/// Segoe MDL2 Assets (Windows 10).
mod glyph {
    pub const CALCULATOR: char = '\u{E8EF}';
    pub const GLOBE: char = '\u{E774}';
    pub const SEARCH: char = '\u{E721}';
    pub const LOCK: char = '\u{E72E}';
    pub const SLEEP: char = '\u{E708}';
    pub const RESTART: char = '\u{E777}';
    pub const POWER: char = '\u{E7E8}';
    pub const SIGN_OUT: char = '\u{F3B1}';
    pub const DELETE: char = '\u{E74D}';
    pub const BRIGHTNESS: char = '\u{E793}';
    pub const MUTE: char = '\u{E74F}';
}

/// Every built-in provider, in the order their answers and catalogs are listed.
pub fn all() -> Vec<Arc<dyn Provider>> {
    vec![
        Arc::new(apps::Apps),
        Arc::new(system::System),
        Arc::new(calc::Calculator),
        Arc::new(web::Web),
    ]
}
