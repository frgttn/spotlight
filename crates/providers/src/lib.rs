//! Built-in root-search providers. Each one only describes results; the app
//! runs their actions (see `spotlight_core::item::Command`).

pub mod apps;
pub mod calc;
pub mod rates;
pub mod spotlight;
pub mod system;
pub mod web;

use std::{path::PathBuf, sync::Arc};

use spotlight_core::{provider::Provider, web::SearchEngine};

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
    pub const VOLUME: char = '\u{E767}';
    pub const VOLUME_DOWN: char = '\u{E993}';
    pub const VOLUME_UP: char = '\u{E995}';
    pub const BLUETOOTH: char = '\u{E702}';
    pub const DISPLAY: char = '\u{E7F4}';
    pub const NIGHT_LIGHT: char = '\u{EA80}';
    pub const USB: char = '\u{E88E}';
    pub const APPS: char = '\u{E71D}';
    pub const KEYBOARD: char = '\u{E765}';
    pub const FORM: char = '\u{E70F}';
}

/// Every built-in provider, in the order their answers and catalogs are
/// listed, and the web provider again, to change its search engine later.
/// `data_dir` is where providers keep caches (exchange rates); `None` keeps
/// nothing between runs.
pub fn all(
    engine: SearchEngine,
    data_dir: Option<PathBuf>,
) -> (Vec<Arc<dyn Provider>>, Arc<web::Web>) {
    let web = Arc::new(web::Web::new(engine));
    let providers: Vec<Arc<dyn Provider>> = vec![
        Arc::new(apps::Apps),
        Arc::new(system::System),
        Arc::new(calc::Calculator::new(rates::Rates::new(data_dir))),
        web.clone(),
        Arc::new(spotlight::Spotlight),
    ];
    (providers, web)
}
