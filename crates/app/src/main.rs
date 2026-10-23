//! Spotlight: wiring for the launcher window. See `README.md`.
//!
//! Environment:
//! - `SPOTLIGHT_SYNTHETIC=10000` adds that many filler rows for the list test.
//! - `SPOTLIGHT_SHOW_ON_START=1` shows the window immediately (handy while developing).
//! - `SPOTLIGHT_SELFTEST=1` runs the scripted measurement in `selftest.rs` and quits.
//! - `SPOTLIGHT_DATA_DIR=<dir>` keeps the database there instead of `%APPDATA%\spotlight`.
//!
//! `spotlight.exe --settings` opens the settings window (of the running
//! instance, if there is one).

// A GUI-subsystem binary, so starting at login or from the Start menu opens no
// console window. Logs still reach a terminal it was started from; see `main`.
#![windows_subsystem = "windows"]

mod selftest;
mod tray;

use std::{
    cell::{Cell, RefCell},
    env,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    App, Application, Bounds, SharedString, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, prelude::*, px, size,
};
use spotlight_core::{
    clip::{IGNORED_APPS_SETTING, IgnoredApp, Retention},
    snippet,
    web::SearchEngine,
};
use spotlight_platform_win::{
    clipboard,
    hotkey::{self, Hotkey},
    instance::{self, Instance},
    metrics, vault, win,
};
use spotlight_providers::{Builtins, clipboard::ClipboardHistory, web::Web};
use spotlight_store::{Key, Store};
use spotlight_ui::{
    launcher::{self, Launcher, OpenSettings, Quit},
    settings::{self, LauncherReader, LauncherState, Settings, SettingsEvent},
    text_input,
};

use crate::tray::{Command, Tray};

/// `settings` key holding the launcher hotkey, in `Hotkey`'s display form.
const HOTKEY_SETTING: &str = "hotkey";
/// `settings` key: `"true"` or `"false"`; missing means on.
const SMOOTH_SCROLLING_SETTING: &str = "smooth_scrolling";
/// `settings` key: `SearchEngine`'s setting form; missing means Google.
const SEARCH_ENGINE_SETTING: &str = "search_engine";
/// `settings` key: ids of the turned-off search providers, comma-separated;
/// missing means all on.
const DISABLED_PROVIDERS_SETTING: &str = "disabled_providers";
/// `settings` key: how much clipboard history to keep, `Retention`'s setting
/// form; missing means the default.
const CLIPBOARD_RETENTION_SETTING: &str = "clipboard_retention";
/// The clipboard history provider's id: turning it off stops recording.
const CLIPBOARD_PROVIDER: &str = "clipboard";
/// `settings` key: `"true"` when Win+V opens the clipboard history; missing
/// means off (Windows keeps it).
const WIN_V_SETTING: &str = "clipboard_win_v";
/// `settings` key: `"false"` when reading the selected text mustn't use
/// Ctrl+C; missing means it may.
const SELECTION_COPY_SETTING: &str = "selection_copy";
/// The item Win+V runs: the Clipboard History command.
const CLIPBOARD_HISTORY_ITEM: &str = "clipboard:history";

/// What the tray, the settings window and app-level actions share.
struct AppState {
    launcher: WindowHandle<Launcher>,
    hotkeys: hotkey::Service,
    hotkey: Cell<Option<Hotkey>>,
    smooth_scrolling: Cell<bool>,
    search_engine: RefCell<SearchEngine>,
    /// The web provider, to switch its search engine.
    web: Arc<Web>,
    /// The clipboard history, to change how much it keeps and whether it records.
    clipboard: Arc<ClipboardHistory>,
    win_v: Cell<bool>,
    selection_copy: Cell<bool>,
    snippet_expansion: Cell<bool>,
    store: Option<Arc<Mutex<Store>>>,
    data_path: SharedString,
    tray: Option<Tray>,
    settings: RefCell<Option<WindowHandle<Settings>>>,
}

fn main() {
    win::attach_parent_console();
    let self_test = env::var_os("SPOTLIGHT_SELFTEST").is_some();
    let settings_requested = env::args().skip(1).any(|arg| arg == "--settings");

    // Before anything else, so a second launch doesn't grab a fallback hotkey.
    // The self-test runs alongside a normal instance, so it skips this.
    let _instance_guard = if self_test {
        None
    } else {
        match instance::acquire() {
            Instance::Primary(guard) => Some(guard),
            Instance::Secondary => {
                let message = if settings_requested {
                    instance::SETTINGS
                } else {
                    instance::SHOW
                };
                if let Err(err) = instance::send(message) {
                    eprintln!("[spotlight] already running, but could not reach it: {err}");
                }
                return;
            }
        }
    };
    let (messages_tx, messages_rx) = async_channel::unbounded::<String>();
    if !self_test {
        instance::listen(messages_tx);
    }

    let synthetic = env::var("SPOTLIGHT_SYNTHETIC")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0usize);
    let show_on_start = env::var_os("SPOTLIGHT_SHOW_ON_START").is_some();
    let data_dir = data_dir();
    let (store, data_path) = open_store(data_dir.as_deref());
    let store = store.map(|store| Arc::new(Mutex::new(store)));

    let setting = |key: &str| {
        store
            .as_ref()
            .and_then(|store| store.lock().ok()?.setting(key).ok().flatten())
    };
    let smooth_scrolling = setting(SMOOTH_SCROLLING_SETTING).is_none_or(|value| value != "false");
    let search_engine = setting(SEARCH_ENGINE_SETTING)
        .and_then(|text| {
            let engine = SearchEngine::from_setting(&text);
            if engine.is_none() {
                eprintln!("[spotlight] ignoring unknown search engine {text:?}");
            }
            engine
        })
        .unwrap_or_default();
    let disabled_providers: Vec<String> = setting(DISABLED_PROVIDERS_SETTING)
        .map(|ids| {
            ids.split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let retention = setting(CLIPBOARD_RETENTION_SETTING)
        .and_then(|text| {
            let retention = Retention::from_setting(&text);
            if retention.is_none() {
                eprintln!("[spotlight] ignoring unreadable clipboard retention {text:?}");
            }
            retention
        })
        .unwrap_or_default();
    let saved_hotkey = setting(HOTKEY_SETTING).and_then(|text| {
        let hotkey = Hotkey::parse(&text);
        if hotkey.is_none() {
            eprintln!("[spotlight] ignoring unreadable saved hotkey {text:?}");
        }
        hotkey
    });
    let (presses_tx, presses_rx) = async_channel::unbounded::<Instant>();
    let (item_presses_tx, item_presses_rx) = async_channel::unbounded::<String>();
    let (hotkeys, registered) = hotkey::spawn(saved_hotkey, presses_tx.clone(), item_presses_tx)
        .expect("failed to start the hotkey thread");
    let win_v = setting(WIN_V_SETTING).is_some_and(|value| value == "true");
    let selection_copy = setting(SELECTION_COPY_SETTING).is_none_or(|value| value != "false");
    let snippet_expansion =
        setting(snippet::EXPANSION_SETTING).is_none_or(|value| value != "false");
    if win_v && !self_test {
        set_win_v(&hotkeys, true);
    }
    let hotkey = match registered {
        Ok(hotkey) => {
            if saved_hotkey.is_some_and(|saved| saved != hotkey) {
                eprintln!("[spotlight] saved hotkey is taken; using {hotkey} for now");
            }
            eprintln!("[spotlight] hotkey registered: {hotkey}");
            Some(hotkey)
        }
        Err(err) => {
            eprintln!("[spotlight] {err}; use SPOTLIGHT_SHOW_ON_START=1 to test without a hotkey");
            None
        }
    };
    let hotkey_label = hotkey_label(hotkey);

    Application::new().run(move |cx: &mut App| {
        text_input::bind_keys(cx);
        launcher::bind_keys(cx);
        settings::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let (width, height) = launcher::WINDOW_SIZE;
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        let launcher_store = store.clone();
        let launcher_hotkeys = hotkeys.clone();
        let label = hotkey_label.clone();
        let Builtins {
            providers,
            web,
            clipboard: clipboard_history,
        } = spotlight_providers::all(
            search_engine.clone(),
            data_dir.clone(),
            store.clone(),
            retention,
        );
        clipboard_history
            .set_recording(!disabled_providers.iter().any(|id| id == CLIPBOARD_PROVIDER));
        // The self-test runs beside a normal instance, which keeps the history.
        if !self_test {
            record_clipboard(clipboard_history.clone());
        }
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: None,
                    focus: false,
                    // Prewarmed: created once at startup, then only shown/hidden.
                    show: false,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    window_background: WindowBackgroundAppearance::Blurred,
                    app_id: Some("spotlight".into()),
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        Launcher::new(
                            label,
                            launcher_hotkeys,
                            providers,
                            &disabled_providers,
                            synthetic,
                            launcher_store,
                            smooth_scrolling,
                            window,
                            cx,
                        )
                    })
                },
            )
            .expect("failed to open the launcher window");
        window
            .update(cx, |launcher, _, _| {
                launcher.set_selection_copy(selection_copy);
                launcher.set_snippet_expansion(snippet_expansion);
                // The self-test runs beside a normal instance, which expands them.
                if !self_test {
                    launcher.start_expander();
                }
            })
            .ok();

        cx.spawn(async move |cx| {
            while let Ok(pressed_at) = presses_rx.recv().await {
                window
                    .update(cx, |launcher, window, cx| {
                        launcher.toggle(pressed_at, window, cx)
                    })
                    .ok();
            }
        })
        .detach();

        // Item hotkeys run their item's primary action.
        cx.spawn(async move |cx| {
            while let Ok(item_id) = item_presses_rx.recv().await {
                window
                    .update(cx, |launcher, window, cx| {
                        launcher.run_by_id(&item_id, window, cx)
                    })
                    .ok();
            }
        })
        .detach();

        let (commands_tx, commands_rx) = async_channel::unbounded();
        let tray = Tray::create(&hotkey_label, commands_tx)
            .inspect_err(|err| eprintln!("[spotlight] could not create the tray icon: {err}"))
            .ok();
        let state = Rc::new(AppState {
            launcher: window,
            hotkeys: hotkeys.clone(),
            hotkey: Cell::new(hotkey),
            smooth_scrolling: Cell::new(smooth_scrolling),
            search_engine: RefCell::new(search_engine.clone()),
            web,
            clipboard: clipboard_history,
            win_v: Cell::new(win_v),
            selection_copy: Cell::new(selection_copy),
            snippet_expansion: Cell::new(snippet_expansion),
            store: store.clone(),
            data_path: data_path.clone(),
            tray,
            settings: RefCell::new(None),
        });

        let settings_state = state.clone();
        cx.on_action(move |_: &OpenSettings, cx| open_settings(&settings_state, cx));

        // Later launches of the executable (Start menu, a shortcut) open this instance.
        let messages_state = state.clone();
        cx.spawn(async move |cx| {
            while let Ok(message) = messages_rx.recv().await {
                let state = &messages_state;
                cx.update(|cx| match message.trim() {
                    instance::SHOW => {
                        state
                            .launcher
                            .update(cx, |launcher, window, cx| launcher.open(window, cx))
                            .ok();
                    }
                    instance::SETTINGS => open_settings(state, cx),
                    other => eprintln!("[spotlight] ignoring unknown message {other:?}"),
                })
                .ok();
            }
        })
        .detach();

        if settings_requested {
            open_settings(&state, cx);
        }

        let tray_state = state.clone();
        cx.spawn(async move |cx| {
            while let Ok(command) = commands_rx.recv().await {
                let state = &tray_state;
                cx.update(|cx| match command {
                    Command::Open => {
                        state
                            .launcher
                            .update(cx, |launcher, window, cx| launcher.open(window, cx))
                            .ok();
                    }
                    Command::Settings => open_settings(state, cx),
                    Command::ToggleAutostart => {
                        if let Some(tray) = &state.tray {
                            tray.toggle_autostart();
                        }
                        let settings = *state.settings.borrow();
                        if let Some(settings) = settings {
                            settings
                                .update(cx, |settings, _, cx| settings.refresh_autostart(cx))
                                .ok();
                        }
                    }
                    Command::Quit => cx.quit(),
                })
                .ok();
            }
        })
        .detach();

        // Idle-memory target: log RAM periodically, including while hidden.
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(10))
                    .await;
                let memory = metrics::memory();
                eprintln!(
                    "[spotlight] RAM working set {:.1} MB, private {:.1} MB",
                    memory.working_set_mb, memory.private_mb
                );
            }
        })
        .detach();

        if self_test {
            cx.spawn(async move |cx| selftest::run(window, cx).await)
                .detach();
        } else if show_on_start {
            presses_tx.send_blocking(Instant::now()).ok();
        }
    });
}

/// Focuses the settings window, opening it if it isn't open.
fn open_settings(state: &Rc<AppState>, cx: &mut App) {
    let existing = *state.settings.borrow();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }

    let (width, height) = settings::WINDOW_SIZE;
    let (min_width, min_height) = settings::WINDOW_MIN_SIZE;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        // Frameless: the view draws its own header with a drag area and close button.
        titlebar: None,
        // The edges still resize it: GPUI leaves them to Windows.
        is_resizable: true,
        window_min_size: Some(size(px(min_width), px(min_height))),
        window_background: WindowBackgroundAppearance::Blurred,
        app_id: Some("spotlight".into()),
        ..Default::default()
    };
    let view_state = state.clone();
    let opened = cx.open_window(options, move |window, cx| {
        let view = cx.new(|cx| {
            Settings::new(
                view_state.hotkeys.clone(),
                view_state.hotkey.get(),
                view_state.smooth_scrolling.get(),
                view_state.search_engine.borrow().clone(),
                view_state.clipboard.retention(),
                view_state.win_v.get(),
                view_state.selection_copy.get(),
                view_state.snippet_expansion.get(),
                env!("SPOTLIGHT_VERSION"),
                view_state.data_path.clone(),
                launcher_reader(view_state.launcher),
                window,
                cx,
            )
        });
        cx.subscribe(&view, move |_, event, cx| {
            settings_changed(&view_state, event, cx)
        })
        .detach();
        view
    });
    match opened {
        Ok(handle) => *state.settings.borrow_mut() = Some(handle),
        Err(err) => eprintln!("[spotlight] could not open settings: {err}"),
    }
}

fn settings_changed(state: &AppState, event: &SettingsEvent, cx: &mut App) {
    match event {
        SettingsEvent::HotkeyChanged(hotkey) => {
            state.hotkey.set(Some(*hotkey));
            let label = hotkey_label(Some(*hotkey));
            state
                .launcher
                .update(cx, |launcher, _, cx| {
                    launcher.set_hotkey_label(label.clone(), cx)
                })
                .ok();
            if let Some(tray) = &state.tray {
                tray.set_hotkey(&label);
            }
            save_setting(state, HOTKEY_SETTING, hotkey.to_string(), cx);
        }
        SettingsEvent::SearchEngineChanged(engine) => {
            state.web.set_engine(engine.clone());
            // Their items name the engine and carry its URL.
            state
                .launcher
                .update(cx, |launcher, window, cx| {
                    launcher.reload_provider("web", window, cx);
                    launcher.reload_provider("selection", window, cx);
                })
                .ok();
            *state.search_engine.borrow_mut() = engine.clone();
            save_setting(state, SEARCH_ENGINE_SETTING, engine.to_setting(), cx);
        }
        SettingsEvent::SmoothScrollingChanged(enabled) => {
            state.smooth_scrolling.set(*enabled);
            state
                .launcher
                .update(cx, |launcher, _, cx| {
                    launcher.set_smooth_scrolling(*enabled, cx)
                })
                .ok();
            save_setting(state, SMOOTH_SCROLLING_SETTING, enabled.to_string(), cx);
        }
        SettingsEvent::SourceToggled { id, enabled } => {
            if *id == CLIPBOARD_PROVIDER {
                state.clipboard.set_recording(*enabled);
            }
            let disabled = state
                .launcher
                .update(cx, |launcher, window, cx| {
                    launcher.set_provider_enabled(id, *enabled, window, cx);
                    launcher.disabled_providers().join(",")
                })
                .ok();
            if let Some(disabled) = disabled {
                save_setting(state, DISABLED_PROVIDERS_SETTING, disabled, cx);
            }
        }
        SettingsEvent::ClipboardRetentionChanged(retention) => {
            let (history, retention) = (state.clipboard.clone(), *retention);
            // Trimming deletes rows and image files.
            cx.background_executor()
                .spawn(async move { history.set_retention(retention) })
                .detach();
            save_setting(
                state,
                CLIPBOARD_RETENTION_SETTING,
                retention.to_setting(),
                cx,
            );
        }
        SettingsEvent::IgnoredAppsChanged(apps) => {
            save_setting(
                state,
                IGNORED_APPS_SETTING,
                IgnoredApp::format_list(apps),
                cx,
            );
        }
        SettingsEvent::WinVChanged(enabled) => {
            set_win_v(&state.hotkeys, *enabled);
            state.win_v.set(*enabled);
            save_setting(state, WIN_V_SETTING, enabled.to_string(), cx);
        }
        SettingsEvent::SelectionCopyChanged(enabled) => {
            state.selection_copy.set(*enabled);
            state
                .launcher
                .update(cx, |launcher, _, _| launcher.set_selection_copy(*enabled))
                .ok();
            save_setting(state, SELECTION_COPY_SETTING, enabled.to_string(), cx);
        }
        SettingsEvent::SnippetExpansionChanged(enabled) => {
            state.snippet_expansion.set(*enabled);
            state
                .launcher
                .update(cx, |launcher, _, _| {
                    launcher.set_snippet_expansion(*enabled)
                })
                .ok();
            save_setting(state, snippet::EXPANSION_SETTING, enabled.to_string(), cx);
        }
        SettingsEvent::ExcludedAppsChanged(apps) => {
            state
                .launcher
                .update(cx, |launcher, _, _| {
                    launcher.set_excluded_snippet_apps(apps)
                })
                .ok();
            save_setting(
                state,
                snippet::EXCLUDED_APPS_SETTING,
                IgnoredApp::format_list(apps),
                cx,
            );
        }
        SettingsEvent::AutostartChanged => {
            if let Some(tray) = &state.tray {
                tray.sync_autostart();
            }
        }
        // The launcher owns aliases and item hotkeys and saves them.
        SettingsEvent::AliasChanged { item_id, alias } => {
            state
                .launcher
                .update(cx, |launcher, _, cx| {
                    launcher.assign_alias(item_id, alias.as_deref().unwrap_or_default(), cx);
                })
                .ok();
        }
        SettingsEvent::ItemHotkeyChanged { item_id, hotkey } => {
            state
                .launcher
                .update(cx, |launcher, _, cx| {
                    launcher.hotkey_assigned(item_id, *hotkey, cx)
                })
                .ok();
        }
    }
}

/// Makes Win+V open the clipboard history, or gives it back to Windows.
fn set_win_v(hotkeys: &hotkey::Service, enabled: bool) {
    let item = enabled.then_some(CLIPBOARD_HISTORY_ITEM);
    if let Err(err) = hotkeys.intercept_win_v(item) {
        eprintln!("[spotlight] {err}");
    }
}

/// Starts the clipboard listener and keeps what it reports in `history`, on
/// a thread of its own (images are written to disk). Expired entries go
/// first, and images saved before encryption are encrypted.
fn record_clipboard(history: Arc<ClipboardHistory>) {
    let (copied_tx, copied_rx) = async_channel::unbounded();
    if let Err(err) = clipboard::listen(copied_tx) {
        eprintln!("[spotlight] clipboard history is off: {err}");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("clipboard-history".into())
        .spawn(move || {
            history.trim();
            history.seal_old_images();
            while let Ok(copied) = copied_rx.recv_blocking() {
                history.record(copied);
            }
        });
    if let Err(err) = spawned {
        eprintln!("[spotlight] clipboard history is off: {err}");
    }
}

/// How the settings window reads the launcher's aliases, item hotkeys and
/// search sources.
fn launcher_reader(launcher: WindowHandle<Launcher>) -> LauncherReader {
    Rc::new(move |cx: &App| {
        let launcher = launcher.read(cx).ok()?;
        Some(LauncherState {
            shortcuts: launcher.shortcuts(),
            items: launcher.extension_items(),
            sources: launcher.sources(),
            ignored_apps: launcher.ignored_clip_apps(),
            excluded_apps: launcher.excluded_snippet_apps(),
        })
    })
}

/// Writes a setting on a background thread; without a database it is kept
/// for this run only.
fn save_setting(state: &AppState, key: &'static str, value: String, cx: &mut App) {
    let Some(store) = state.store.clone() else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let result = store
                .lock()
                .map_err(|_| "store lock poisoned".to_owned())
                .and_then(|store| {
                    store
                        .set_setting(key, &value)
                        .map_err(|err| err.to_string())
                });
            if let Err(err) = result {
                eprintln!("[spotlight] could not save setting {key:?}: {err}");
            }
        })
        .detach();
}

fn hotkey_label(hotkey: Option<Hotkey>) -> SharedString {
    hotkey.map_or("no hotkey".into(), |hotkey| hotkey.to_string().into())
}

/// `%APPDATA%\spotlight`, or `SPOTLIGHT_DATA_DIR`: the database and caches.
fn data_dir() -> Option<PathBuf> {
    env::var_os("SPOTLIGHT_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("spotlight")))
}

/// `spotlight.db` in the data folder ([`data_dir`]), encrypted with the key
/// in Credential Manager; a plain one from before is encrypted on the way.
/// Without a database the launcher still works; it just doesn't remember
/// usage or settings. Also returns the path for display.
fn open_store(dir: Option<&Path>) -> (Option<Store>, SharedString) {
    let Some(dir) = dir else {
        return (None, "unavailable (APPDATA is not set)".into());
    };
    let path = dir.join("spotlight.db");
    let shown: SharedString = path.display().to_string().into();
    let key = database_key(&path);
    let opened = match Store::open(&path, key.as_ref()) {
        // Encrypted with some other key: unreadable too, so the same as
        // a key that is gone.
        Err(spotlight_store::Error::SqliteFailure(err, _))
            if key.is_some() && err.code == spotlight_store::ErrorCode::NotADatabase =>
        {
            set_aside(&path);
            Store::open(&path, key.as_ref())
        }
        opened => opened,
    };
    match opened {
        Ok(store) => (Some(store), shown),
        Err(err) => {
            eprintln!("[spotlight] could not open {}: {err}", path.display());
            (None, format!("{shown} (could not open)").into())
        }
    }
}

/// The database key, which also unlocks the clipboard images. `None` when
/// Credential Manager can't keep one: the database then stays unencrypted.
fn database_key(path: &Path) -> Option<Key> {
    let (key, new) = vault::database_key()
        .inspect_err(|err| {
            eprintln!("[spotlight] no database key, so the database is not encrypted: {err}")
        })
        .ok()?;
    // Encrypted with a key that is gone (another Windows account, a reset
    // profile): it can't be read, so it is kept aside and a new one starts.
    let unreadable =
        path.metadata().is_ok_and(|meta| meta.len() > 0) && !spotlight_store::is_plain(path);
    if new && unreadable {
        set_aside(path);
    }
    vault::unlock_files(&key);
    Some(key)
}

/// Renames the database at `path` (with its log files) out of the way.
fn set_aside(path: &Path) {
    let stamp = spotlight_core::frecency::now();
    for suffix in ["", "-wal", "-shm"] {
        let mut from = path.as_os_str().to_owned();
        from.push(suffix);
        let mut to = path
            .with_extension(format!("unreadable-{stamp}.db"))
            .into_os_string();
        to.push(suffix);
        if Path::new(&from).exists()
            && let Err(err) = std::fs::rename(&from, &to)
        {
            eprintln!("[spotlight] could not move the unreadable database aside: {err}");
        }
    }
    eprintln!(
        "[spotlight] {} can't be read with the saved key; kept as unreadable-{stamp}.db",
        path.display()
    );
}
