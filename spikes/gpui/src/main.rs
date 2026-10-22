//! GPUI launcher spike. See `launcher/README.md` for what it measures.
//!
//! Environment:
//! - `SPIKE_SYNTHETIC=10000` adds that many filler rows for the list test.
//! - `SPIKE_SHOW_ON_START=1` shows the window immediately (handy while developing).
//! - `SPIKE_SELFTEST=1` runs the scripted measurement in `selftest.rs` and quits.

mod launcher;
mod selftest;
mod text_input;

use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, SharedString, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, prelude::*, px, size,
};
use spike_common::{hotkey, metrics};

use crate::launcher::{Launcher, Quit};

fn main() {
    let synthetic = std::env::var("SPIKE_SYNTHETIC")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0usize);
    let show_on_start = std::env::var_os("SPIKE_SHOW_ON_START").is_some();
    let self_test = std::env::var_os("SPIKE_SELFTEST").is_some();

    let (presses_tx, presses_rx) = async_channel::unbounded::<Instant>();
    let hotkey_label: SharedString = match hotkey::spawn(presses_tx.clone()) {
        Ok(label) => {
            eprintln!("[spike] hotkey registered: {label}");
            label.into()
        }
        Err(err) => {
            eprintln!("[spike] {err}; use SPIKE_SHOW_ON_START=1 to test without a hotkey");
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
                    app_id: Some("rea.launcher.spike-gpui".into()),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Launcher::new(hotkey_label, synthetic, window, cx)),
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
                    "[spike] RAM working set {:.1} MB, private {:.1} MB",
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
