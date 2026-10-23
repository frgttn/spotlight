//! `SPOTLIGHT_SELFTEST=1`: drives the real show/hide and text-input paths, prints
//! a summary and quits. It injects presses into the same channel the hotkey
//! thread feeds, so it skips only the OS's hotkey dispatch and key-event delivery.
//! The script itself lives in `spotlight_platform_win::metrics::selftest`, unchanged
//! since the GPUI/Slint comparison so the numbers stay comparable.

use std::time::{Duration, Instant};

use gpui::{AppContext, AsyncApp, Keystroke, WindowHandle};
use spotlight_platform_win::metrics::{self, selftest::*};
use spotlight_ui::launcher::Launcher;

pub async fn run(window: WindowHandle<Launcher>, cx: &mut AsyncApp) {
    let executor = cx.background_executor().clone();
    let sleep = |ms: u64| executor.timer(Duration::from_millis(ms));

    // Wait for app discovery so the list has its real size.
    while !window
        .update(cx, |launcher, _, _| launcher.apps_loaded())
        .unwrap_or(true)
    {
        sleep(50).await;
    }
    let startup = metrics::memory();

    for _ in 0..SHOW_CYCLES {
        window
            .update(cx, |launcher, window, cx| {
                launcher.show(Instant::now(), window, cx)
            })
            .ok();
        sleep(SHOWN_MS).await;
        window.update(cx, |launcher, _, cx| launcher.hide(cx)).ok();
        sleep(HIDDEN_MS).await;
    }

    window
        .update(cx, |launcher, window, cx| {
            launcher.show(Instant::now(), window, cx)
        })
        .ok();
    sleep(SETTLE_MS).await;
    for ch in QUERY.chars() {
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text(ch.encode_utf8(&mut [0; 4]), window, cx)
            })
            .ok();
        sleep(KEY_INTERVAL_MS).await;
    }
    sleep(SETTLE_MS).await;

    // Ctrl+Backspace through GPUI's key dispatch (binding → action), not the action directly.
    // `update_window` doesn't lease the launcher view, so its subscription can run.
    let before = window
        .update(cx, |launcher, _, cx| launcher.query(cx))
        .unwrap_or_default();
    let keystroke = Keystroke::parse("ctrl-backspace").expect("valid keystroke");
    let handled = cx
        .update_window(window.into(), |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx)
        })
        .unwrap_or(false);
    sleep(SETTLE_MS).await;
    let after = window
        .update(cx, |launcher, _, cx| launcher.query(cx))
        .unwrap_or_default();
    eprintln!(
        "[selftest] ctrl+backspace: {before:?} -> {after:?} (handled: {handled}) {}",
        if after.is_empty() && !before.is_empty() {
            "OK"
        } else {
            "FAIL"
        }
    );

    // Root search end to end: the calculator's answer is the first row.
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text("12*12", window, cx)
        })
        .ok();
    sleep(SETTLE_MS).await;
    let first = window
        .update(cx, |launcher, _, _| {
            launcher.result_titles().into_iter().next()
        })
        .ok()
        .flatten();
    eprintln!(
        "[selftest] calculator: 12*12 -> {first:?} {}",
        if first.as_deref() == Some("144") {
            "OK"
        } else {
            "FAIL"
        }
    );

    window.update(cx, |launcher, _, cx| launcher.hide(cx)).ok();

    sleep(IDLE_MS).await;
    let idle = metrics::memory();

    if let Ok(summary) = window.update(cx, |launcher, _, _| launcher.summary()) {
        summary.print(startup, idle);
    }
    cx.update(|cx| cx.quit()).ok();
}
