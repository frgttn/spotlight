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
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    App, Application, Bounds, SharedString, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, prelude::*, px, size,
};
use spotlight_platform_win::{
    hotkey::{self, Hotkey},
    instance::{self, Instance},
    metrics, win,
};
use spotlight_store::Store;
use spotlight_ui::{
    launcher::{self, Launcher, OpenSettings, Quit},
    settings::{self, Settings, SettingsEvent},
    text_input,
};

use crate::tray::{Command, Tray};

/// `settings` key holding the launcher hotkey, in `Hotkey`'s display form.
const HOTKEY_SETTING: &str = "hotkey";
/// `settings` key: `"true"` or `"false"`; missing means on.
const SMOOTH_SCROLLING_SETTING: &str = "smooth_scrolling";

/// What the tray, the settings window and app-level actions share.
struct AppState {
    launcher: WindowHandle<Launcher>,
    hotkeys: hotkey::Service,
    hotkey: Cell<Option<Hotkey>>,
    smooth_scrolling: Cell<bool>,
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
    let (store, data_path) = open_store();
    let store = store.map(|store| Arc::new(Mutex::new(store)));

    let setting = |key: &str| {
        store
            .as_ref()
            .and_then(|store| store.lock().ok()?.setting(key).ok().flatten())
    };
    let smooth_scrolling = setting(SMOOTH_SCROLLING_SETTING).is_none_or(|value| value != "false");
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
                            spotlight_providers::all(),
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
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        // Frameless: the view draws its own header with a drag area and close button.
        titlebar: None,
        is_resizable: false,
        is_minimizable: false,
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
                view_state.data_path.clone(),
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
        SettingsEvent::AutostartChanged => {
            if let Some(tray) = &state.tray {
                tray.sync_autostart();
            }
        }
    }
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

/// `%APPDATA%\spotlight\spotlight.db`, or `SPOTLIGHT_DATA_DIR`. Without a
/// database the launcher still works; it just doesn't remember usage or settings.
/// Also returns the path for display.
fn open_store() -> (Option<Store>, SharedString) {
    let Some(dir) = env::var_os("SPOTLIGHT_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("spotlight")))
    else {
        return (None, "unavailable (APPDATA is not set)".into());
    };
    let path = dir.join("spotlight.db");
    let shown: SharedString = path.display().to_string().into();
    match Store::open(&path) {
        Ok(store) => (Some(store), shown),
        Err(err) => {
            eprintln!("[spotlight] could not open {}: {err}", path.display());
            (None, format!("{shown} (could not open)").into())
        }
    }
}
