//! Spotlight: wiring for the launcher window. See `README.md`.
//!
//! Environment:
//! - `SPOTLIGHT_SYNTHETIC=10000` adds that many filler rows for the list test.
//! - `SPOTLIGHT_SHOW_ON_START=1` shows the window immediately (handy while developing).
//! - `SPOTLIGHT_SELFTEST=1` runs the scripted measurement in `selftest.rs` and quits.
//! - `SPOTLIGHT_DATA_DIR=<dir>` keeps the database there instead of `%APPDATA%\spotlight`.

mod selftest;

use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui::{
    App, Application, Bounds, SharedString, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, prelude::*, px, size,
};
use spotlight_platform_win::{hotkey, metrics};
use spotlight_store::Store;
use spotlight_ui::{
    launcher::{self, Launcher, Quit},
    text_input,
};

fn main() {
    let synthetic = env::var("SPOTLIGHT_SYNTHETIC")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0usize);
    let show_on_start = env::var_os("SPOTLIGHT_SHOW_ON_START").is_some();
    let self_test = env::var_os("SPOTLIGHT_SELFTEST").is_some();
    let store = open_store();

    let (presses_tx, presses_rx) = async_channel::unbounded::<Instant>();
    let hotkey_label: SharedString = match hotkey::spawn(presses_tx.clone()) {
        Ok(label) => {
            eprintln!("[spotlight] hotkey registered: {label}");
            label.into()
        }
        Err(err) => {
            eprintln!("[spotlight] {err}; use SPOTLIGHT_SHOW_ON_START=1 to test without a hotkey");
            "no hotkey".into()
        }
    };

    Application::new().run(move |cx: &mut App| {
        text_input::bind_keys(cx);
        launcher::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let (width, height) = launcher::WINDOW_SIZE;
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
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
                |window, cx| cx.new(|cx| Launcher::new(hotkey_label, synthetic, store, window, cx)),
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

/// `%APPDATA%\spotlight\spotlight.db`, or `SPOTLIGHT_DATA_DIR`. Without a
/// database the launcher still works; it just doesn't remember usage.
fn open_store() -> Option<Store> {
    let dir = env::var_os("SPOTLIGHT_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("spotlight"))
        })?;
    let path = dir.join("spotlight.db");
    match Store::open(&path) {
        Ok(store) => Some(store),
        Err(err) => {
            eprintln!("[spotlight] could not open {}: {err}", path.display());
            None
        }
    }
}
