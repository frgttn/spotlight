//! Built-in root-search providers. Each one only describes results; the app
//! runs their actions (see `spotlight_core::item::Command`).

pub mod apps;
pub mod calc;
pub mod clipboard;
pub mod control_panel;
mod currency;
pub mod files;
pub mod games;
pub mod qr;
pub mod quicklinks;
pub mod rates;
pub mod run;
pub mod selection;
pub mod settings_pages;
pub mod snippets;
pub mod spotlight;
pub mod switcher;
pub mod system;
pub mod web;
pub mod windows;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use spotlight_core::{clip::Retention, provider::Provider, web::SearchEngine};
use spotlight_store::Store;

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
    pub const GAME: char = '\u{E7FC}';
    pub const LINK: char = '\u{E71B}';
    pub const ADD: char = '\u{E710}';
    pub const PASTE: char = '\u{E77F}';
    pub const TEXT: char = '\u{E8D2}';
    pub const COLOR: char = '\u{E790}';
    pub const IMAGE: char = '\u{EB9F}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const TRANSLATE: char = '\u{F2B7}';
    pub const SNIPPET: char = '\u{E70B}';
    pub const IMPORT: char = '\u{E8B5}';
    pub const EXPORT: char = '\u{EDE1}';
    pub const DOCK_LEFT: char = '\u{E90C}';
    pub const DOCK_RIGHT: char = '\u{E90D}';
    pub const DOCK_BOTTOM: char = '\u{E90E}';
    pub const MAXIMIZE: char = '\u{E922}';
    pub const RESTORE_WINDOW: char = '\u{E923}';
    pub const MINIMIZE: char = '\u{E921}';
    pub const PIN: char = '\u{E718}';
    pub const SWITCH: char = '\u{E8AB}';
    pub const WINDOWS: char = '\u{E7C4}';
    pub const PROCESSES: char = '\u{E9D9}';
}

/// The built-in providers, and the ones the app talks to again later.
pub struct Builtins {
    /// In the order their answers and catalogs are listed.
    pub providers: Vec<Arc<dyn Provider>>,
    /// To change its search engine.
    pub web: Arc<web::Web>,
    /// To feed it what is copied, and change how much it keeps.
    pub clipboard: Arc<clipboard::ClipboardHistory>,
    /// To start and stop indexing, and change what it covers.
    pub files: Arc<files::Files>,
    /// To change its prefix.
    pub run: Arc<run::Run>,
}

/// Every built-in provider. `data_dir` is where providers keep caches
/// (exchange rates) and files (clipboard images); `None` keeps nothing
/// between runs. `store` holds what the user saves (quicklinks, clipboard
/// history, snippets). File Search starts indexing only when the app asks.
pub fn all(
    engine: SearchEngine,
    data_dir: Option<PathBuf>,
    store: Option<Arc<Mutex<Store>>>,
    retention: Retention,
    files: spotlight_core::file_search::Config,
    run_prefix: String,
) -> Builtins {
    let run = Arc::new(run::Run::new(store.clone(), run_prefix));
    let files = Arc::new(files::Files::new(files, store.clone()));
    let web = Arc::new(web::Web::new(engine, store.clone()));
    let clipboard = Arc::new(clipboard::ClipboardHistory::new(
        store.clone(),
        data_dir.clone(),
        retention,
    ));
    let providers: Vec<Arc<dyn Provider>> = vec![
        Arc::new(apps::Apps),
        Arc::new(settings_pages::SettingsPages),
        Arc::new(control_panel::ControlPanel),
        Arc::new(games::Games),
        Arc::new(system::System),
        Arc::new(windows::WindowManagement),
        Arc::new(switcher::Switcher),
        files.clone(),
        clipboard.clone(),
        Arc::new(calc::Calculator::new(rates::Rates::new(data_dir.clone()))),
        Arc::new(quicklinks::Quicklinks::new(store.clone())),
        Arc::new(snippets::Snippets::new(store)),
        Arc::new(qr::Qr),
        run.clone(),
        web.clone(),
        Arc::new(selection::Selection::new(
            web.clone(),
            spotlight_platform_win::win::user_language(),
        )),
        Arc::new(spotlight::Spotlight),
    ];
    Builtins {
        providers,
        web,
        clipboard,
        files,
        run,
    }
}
