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
    // It deletes one word: the query typed above ends in " code".
    eprintln!(
        "[selftest] ctrl+backspace: {before:?} -> {after:?} (handled: {handled}) {}",
        if !before.is_empty() && before.strip_suffix("code") == Some(after.as_str()) {
            "OK"
        } else {
            "FAIL"
        }
    );

    // Ctrl+Left moves to the start of the previous word: "visual |studio ".
    let word_left = Keystroke::parse("ctrl-left").expect("valid keystroke");
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(word_left, cx)
    })
    .ok();
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text("X", window, cx)
        })
        .ok();
    sleep(SETTLE_MS).await;
    let edited = window
        .update(cx, |launcher, _, cx| launcher.query(cx))
        .unwrap_or_default();
    eprintln!(
        "[selftest] ctrl+left: {after:?} -> {edited:?} {}",
        if edited == "visual Xstudio " {
            "OK"
        } else {
            "FAIL"
        }
    );

    // Root search end to end: the calculator's answer is the first row.
    // Ctrl+A first, so the typed text replaces the query.
    let select_all = Keystroke::parse("ctrl-a").expect("valid keystroke");
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(select_all, cx)
    })
    .ok();
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

    // Ctrl+K opens the action panel on that row; Esc closes the panel, not the window.
    press(window, "ctrl-k", cx);
    sleep(SETTLE_MS).await;
    let panel = window
        .update(cx, |launcher, _, _| launcher.panel_actions())
        .ok()
        .flatten();
    press(window, "escape", cx);
    sleep(SETTLE_MS).await;
    let (closed, visible) = window
        .update(cx, |launcher, _, _| {
            (launcher.panel_actions().is_none(), launcher.is_visible())
        })
        .unwrap_or_default();
    let expected = ["Copy Answer", "Copy Calculation"].map(String::from);
    eprintln!(
        "[selftest] action panel: {panel:?}, esc closes it: {closed}, window stays: {visible} {}",
        if panel.as_deref() == Some(&expected[..]) && closed && visible {
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

/// Sends `keystroke` through GPUI's key dispatch, as a real key press arrives.
/// `update_window` doesn't lease the launcher view, so its handlers can run.
fn press(window: WindowHandle<Launcher>, keystroke: &str, cx: &mut AsyncApp) -> bool {
    let keystroke = Keystroke::parse(keystroke).expect("valid keystroke");
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx)
    })
    .unwrap_or(false)
}
