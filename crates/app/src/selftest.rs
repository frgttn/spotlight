//! `SPOTLIGHT_SELFTEST=1`: drives the real show/hide and text-input paths, prints
//! a summary and quits. It injects presses into the same channel the hotkey
//! thread feeds, so it skips only the OS's hotkey dispatch and key-event delivery.
//! The script itself lives in `spotlight_platform_win::metrics::selftest`, unchanged
//! since the GPUI/Slint comparison so the numbers stay comparable.

use std::time::{Duration, Instant};

use gpui::{App, AppContext, AsyncApp, Keystroke, WindowHandle};
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

    let alias = alias_round_trip(window, cx).await;
    eprintln!(
        "[selftest] alias: {alias} {}",
        if alias == "set, found first, removed" {
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

/// Gives Lock Screen an alias through the action panel and editor, checks that
/// the alias finds it first, then removes it again. Returns how far it got.
///
/// Lock Screen's primary action locks the screen, so Enter is only pressed
/// after checking that the panel has the intended entry selected or the
/// editor is open.
async fn alias_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    const ITEM: &str = "system:lock";
    const ALIAS: &str = "zq9";
    let executor = cx.background_executor().clone();
    let settle = || executor.timer(Duration::from_millis(SETTLE_MS));
    let read = |cx: &mut AsyncApp, f: &dyn Fn(&Launcher, &App) -> String| {
        window
            .update(cx, |launcher, _, cx| f(launcher, cx))
            .unwrap_or_default()
    };

    // Selects the panel entry `title` (opening the panel) and presses Enter on it.
    async fn pick(window: WindowHandle<Launcher>, title: &str, cx: &mut AsyncApp) -> bool {
        press(window, "ctrl-k", cx);
        let titles = window
            .update(cx, |launcher, _, _| launcher.panel_actions())
            .ok()
            .flatten()
            .unwrap_or_default();
        let Some(ix) = titles.iter().position(|entry| entry.starts_with(title)) else {
            press(window, "escape", cx);
            return false;
        };
        for _ in 0..ix {
            press(window, "down", cx);
        }
        let selected = window
            .update(cx, |launcher, _, _| launcher.panel_selected())
            .ok()
            .flatten();
        if !selected.is_some_and(|selected| selected.starts_with(title)) {
            press(window, "escape", cx);
            return false;
        }
        press(window, "enter", cx)
    }

    press(window, "ctrl-a", cx);
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text("lock screen", window, cx)
        })
        .ok();
    settle().await;
    let first = read(cx, &|launcher, _| {
        launcher
            .result_titles()
            .into_iter()
            .next()
            .unwrap_or_default()
    });
    if first != "Lock Screen" {
        return format!("Lock Screen not first ({first:?})");
    }

    if !pick(window, "Set Alias", cx).await && !pick(window, "Change Alias", cx).await {
        return "no Set Alias entry".into();
    }
    settle().await;
    if read(cx, &|launcher, _| launcher.editing_alias().to_string()) != "true" {
        return "editor didn't open".into();
    }
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text(ALIAS, window, cx)
        })
        .ok();
    press(window, "enter", cx);
    settle().await;
    if read(cx, &|launcher, _| {
        launcher.aliases().get(ITEM).cloned().unwrap_or_default()
    }) != ALIAS
    {
        return "alias not saved".into();
    }

    press(window, "ctrl-a", cx);
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text(ALIAS, window, cx)
        })
        .ok();
    settle().await;
    let first = read(cx, &|launcher, _| {
        launcher
            .result_titles()
            .into_iter()
            .next()
            .unwrap_or_default()
    });
    if first != "Lock Screen" {
        return format!("alias didn't find it first ({first:?})");
    }

    if !pick(window, "Remove Alias", cx).await {
        return "set, found first, but no Remove Alias entry".into();
    }
    settle().await;
    if read(cx, &|launcher, _| {
        launcher.aliases().contains_key(ITEM).to_string()
    }) != "false"
    {
        return "set, found first, not removed".into();
    }
    "set, found first, removed".into()
}
