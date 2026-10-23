//! `SPOTLIGHT_SELFTEST=1`: drives the real show/hide and text-input paths, prints
//! a summary and quits. It injects presses into the same channel the hotkey
//! thread feeds, so it skips only the OS's hotkey dispatch and key-event delivery.
//! The script itself lives in `spotlight_platform_win::metrics::selftest`, unchanged
//! since the GPUI/Slint comparison so the numbers stay comparable.

use std::{
    env,
    time::{Duration, Instant},
};

use gpui::{App, AppContext, AsyncApp, Keystroke, WindowHandle};
use spotlight_core::view::FieldValue;
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

    // Turning the calculator off in Settings takes its answer away at once;
    // turning it back on brings it back.
    let mut toggled = Vec::new();
    for enabled in [false, true] {
        window
            .update(cx, |launcher, window, cx| {
                launcher.set_provider_enabled("calc", enabled, window, cx)
            })
            .ok();
        sleep(SETTLE_MS).await;
        toggled.push(
            window
                .update(cx, |launcher, _, _| {
                    launcher.result_titles().into_iter().next()
                })
                .ok()
                .flatten(),
        );
    }
    eprintln!(
        "[selftest] calculator off, then on: {toggled:?} {}",
        if toggled[0].as_deref() != Some("144") && toggled[1].as_deref() == Some("144") {
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

    let history = history_round_trip(window, cx).await;
    eprintln!(
        "[selftest] calculator history: {history} {}",
        if history == "opened, back to search" {
            "OK"
        } else {
            "FAIL"
        }
    );

    for (name, outcome) in [
        ("grid", grid_round_trip(window, cx).await),
        ("detail", detail_round_trip(window, cx).await),
        ("form", form_round_trip(window, cx).await),
        ("quicklink", quicklink_round_trip(window, cx).await),
        ("bangs", bangs_round_trip(window, cx).await),
    ] {
        eprintln!(
            "[selftest] {name} view: {outcome} {}",
            if outcome.ends_with("back to search") {
                "OK"
            } else {
                "FAIL"
            }
        );
    }

    // Writes to the database, so only in a data folder of its own.
    if env::var_os("SPOTLIGHT_DATA_DIR").is_some() {
        let saved = quicklink_save_and_delete(window, cx).await;
        eprintln!(
            "[selftest] quicklink save: {saved} {}",
            if saved == "saved, opened with !zzq, deleted" {
                "OK"
            } else {
                "FAIL"
            }
        );
    }

    window.update(cx, |launcher, _, cx| launcher.hide(cx)).ok();

    // The HUD, for a look from outside: the line below says when it's up.
    sleep(SETTLE_MS).await;
    window
        .update(cx, |launcher, _, cx| {
            launcher.show_hud("Self-test HUD".into(), cx)
        })
        .ok();
    sleep(SETTLE_MS).await;
    eprintln!("[selftest] hud shown");
    sleep(1000).await;

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

/// Opens Calculator History from root search and goes back with Esc.
/// Returns how far it got.
async fn history_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let settle = || executor.timer(Duration::from_millis(SETTLE_MS));
    press(window, "ctrl-a", cx);
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text("calculator history", window, cx)
        })
        .ok();
    settle().await;
    let first = window
        .update(cx, |launcher, _, _| {
            launcher.result_titles().into_iter().next()
        })
        .ok()
        .flatten();
    // Enter runs the first row: only press it on the row it should be.
    if first.as_deref() != Some("Calculator History") {
        return format!("not the first row ({first:?})");
    }
    press(window, "enter", cx);
    settle().await;
    let opened = window
        .update(cx, |launcher, _, cx| {
            (launcher.screen_title(), launcher.query(cx))
        })
        .ok();
    if opened != Some((Some("Calculator History".to_owned()), String::new())) {
        return format!("didn't open ({opened:?})");
    }
    press(window, "escape", cx);
    settle().await;
    let back = window
        .update(cx, |launcher, _, _| {
            (launcher.screen_title(), launcher.is_visible())
        })
        .ok();
    if back != Some((None, true)) {
        return format!("opened, but Esc gave {back:?}");
    }
    "opened, back to search".into()
}

/// Searches for `query` and, only if `title` is the first row, runs it with
/// Enter (the rows used here open views). Returns why not otherwise.
async fn open_from_search(
    window: WindowHandle<Launcher>,
    query: &str,
    title: &str,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let executor = cx.background_executor().clone();
    press(window, "ctrl-a", cx);
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text(query, window, cx)
        })
        .ok();
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let first = window
        .update(cx, |launcher, _, _| {
            launcher.result_titles().into_iter().next()
        })
        .ok()
        .flatten();
    if first.as_deref() != Some(title) {
        return Err(format!("{title:?} not the first row ({first:?})"));
    }
    press(window, "enter", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    Ok(())
}

/// Esc, then checks that root search is back and the window still open.
async fn back_to_search(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> Result<(), String> {
    press(window, "escape", cx);
    cx.background_executor()
        .timer(Duration::from_millis(SETTLE_MS))
        .await;
    let back = window
        .update(cx, |launcher, _, _| {
            (launcher.screen_title(), launcher.is_visible())
        })
        .ok();
    if back == Some((None, true)) {
        Ok(())
    } else {
        Err(format!("Esc gave {back:?}"))
    }
}

/// Browse Apps: a grid; → moves one tile, ↓ one row. Enter isn't pressed
/// there, it would launch an app.
async fn grid_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    if let Err(why) = open_from_search(window, "browse apps", "Browse Apps", cx).await {
        return why;
    }
    let read = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, _| {
                (
                    launcher.screen_title(),
                    launcher.result_titles().len(),
                    launcher.selected_index(),
                )
            })
            .ok()
    };
    let opened = read(cx);
    let Some((Some(title), tiles, 0)) = opened.clone() else {
        return format!("didn't open ({opened:?})");
    };
    if title != "Apps" || tiles < 7 {
        return format!("opened {title:?} with {tiles} tiles");
    }
    press(window, "right", cx);
    let after_right = read(cx).map(|(_, _, selected)| selected);
    press(window, "down", cx);
    let after_down = read(cx).map(|(_, _, selected)| selected);
    if (after_right, after_down) != (Some(1), Some(6)) {
        return format!("→ then ↓ selected {after_right:?}, {after_down:?}");
    }
    match back_to_search(window, cx).await {
        Ok(()) => format!("{tiles} tiles, → and ↓ move, back to search"),
        Err(why) => why,
    }
}

async fn detail_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    if let Err(why) = open_from_search(window, "keyboard shortcuts", "Keyboard Shortcuts", cx).await
    {
        return why;
    }
    let title = window
        .update(cx, |launcher, _, _| launcher.screen_title())
        .ok()
        .flatten();
    if title.as_deref() != Some("Keyboard Shortcuts") {
        return format!("didn't open ({title:?})");
    }
    press(window, "down", cx);
    match back_to_search(window, cx).await {
        Ok(()) => "opened, scrolled, back to search".into(),
        Err(why) => why,
    }
}

/// Search the Web: Enter with an empty query is refused, Tab and → change
/// the engine. The query stays empty, so no browser opens.
async fn form_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    if let Err(why) = open_from_search(window, "search the web", "Search the Web…", cx).await {
        return why;
    }
    let state = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, cx| launcher.form_state(cx))
            .ok()
            .flatten()
    };
    let Some((values, None)) = state(cx) else {
        return format!("didn't open ({:?})", state(cx));
    };
    if values.first() != Some(&FieldValue::Text(String::new())) {
        return format!("unexpected fields {values:?}");
    }
    press(window, "enter", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let refused = state(cx).and_then(|(_, error)| error);
    if refused.as_deref() != Some("Search for is required") {
        return format!("empty query not refused ({refused:?})");
    }
    press(window, "tab", cx);
    press(window, "right", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let engine = state(cx).and_then(|(values, _)| values.get(1).cloned());
    if engine != Some(FieldValue::Dropdown(1)) {
        return format!("refused empty, but Tab → gave {engine:?}");
    }
    match back_to_search(window, cx).await {
        Ok(()) => "refused empty, Tab → changed engine, back to search".into(),
        Err(why) => why,
    }
}

/// `!gh` fills GitHub's quicklink in root search (not opened), then Create
/// Quicklink refuses an empty name. Nothing is saved or opened.
async fn quicklink_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    press(window, "ctrl-a", cx);
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text("!gh rust gpui", window, cx)
        })
        .ok();
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let first = window
        .update(cx, |launcher, _, _| {
            launcher.result_titles().into_iter().next()
        })
        .ok()
        .flatten();
    if first.as_deref() != Some("Search GitHub “rust gpui”") {
        return format!("!gh not filled in ({first:?})");
    }
    if let Err(why) = open_from_search(window, "create quicklink", "Create Quicklink", cx).await {
        return why;
    }
    let state = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, cx| launcher.form_state(cx))
            .ok()
            .flatten()
    };
    match state(cx) {
        Some((values, None)) if values.len() == 4 => {}
        other => return format!("form didn't open ({other:?})"),
    }
    press(window, "enter", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let refused = state(cx).and_then(|(_, error)| error);
    if refused.as_deref() != Some("Name is required") {
        return format!("empty name not refused ({refused:?})");
    }
    match back_to_search(window, cx).await {
        Ok(()) => "!gh filled in, empty form refused, back to search".into(),
        Err(why) => why,
    }
}

/// Creates a quicklink through its form, finds it with its keyword, then
/// deletes it from the action panel (Enter twice: it asks to confirm).
async fn quicklink_save_and_delete(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let settle = || executor.timer(Duration::from_millis(SETTLE_MS));
    let type_text = |text: &str, cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text(text, window, cx)
            })
            .ok();
    };
    let first = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, _| {
                launcher.result_titles().into_iter().next()
            })
            .ok()
            .flatten()
    };

    if let Err(why) = open_from_search(window, "create quicklink", "Create Quicklink", cx).await {
        return why;
    }
    type_text("Selftest Link", cx);
    press(window, "tab", cx);
    type_text("https://example.com/?q={query}", cx);
    press(window, "tab", cx);
    type_text("zzq", cx);
    press(window, "enter", cx);
    settle().await;
    let screen = window
        .update(cx, |launcher, _, cx| {
            (launcher.screen_title(), launcher.form_state(cx))
        })
        .ok();
    if !matches!(screen, Some((None, None))) {
        return format!("not saved ({screen:?})");
    }

    press(window, "ctrl-a", cx);
    type_text("!zzq hi", cx);
    settle().await;
    let found = first(cx);
    if found.as_deref() != Some("Selftest Link “hi”") {
        return format!("saved, but !zzq gave {found:?}");
    }

    press(window, "ctrl-a", cx);
    type_text("selftest link", cx);
    settle().await;
    if first(cx).as_deref() != Some("Selftest Link") {
        return format!("saved, but not found by name ({:?})", first(cx));
    }
    press(window, "ctrl-k", cx);
    let titles = window
        .update(cx, |launcher, _, _| launcher.panel_actions())
        .ok()
        .flatten()
        .unwrap_or_default();
    let Some(ix) = titles.iter().position(|title| title == "Delete Quicklink") else {
        return format!("saved, opened with !zzq, no Delete in {titles:?}");
    };
    for _ in 0..ix {
        press(window, "down", cx);
    }
    press(window, "enter", cx);
    press(window, "enter", cx);
    settle().await;
    let left = window
        .update(cx, |launcher, _, _| launcher.result_titles())
        .unwrap_or_default();
    if left.iter().any(|title| title == "Selftest Link") {
        return "saved, opened with !zzq, not deleted".into();
    }
    "saved, opened with !zzq, deleted".into()
}

/// The bangs list (downloaded on first use, so it waits for it): `!reddit
/// rust` fills Reddit's bang, Enter on `!reddi` completes it to `!reddit `,
/// and Search Bangs lists them all. Nothing is opened.
async fn bangs_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let first = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, _| {
                launcher.result_titles().into_iter().next()
            })
            .ok()
            .flatten()
    };
    let mut found = None;
    for _ in 0..50 {
        press(window, "ctrl-a", cx);
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text("!reddit rust", window, cx)
            })
            .ok();
        executor.timer(Duration::from_millis(200)).await;
        found = first(cx);
        if found.as_deref() == Some("Reddit “rust”") {
            break;
        }
    }
    if found.as_deref() != Some("Reddit “rust”") {
        return format!("bangs not loaded, offline? ({found:?})");
    }

    press(window, "ctrl-a", cx);
    window
        .update(cx, |launcher, window, cx| {
            launcher.type_text("!reddi", window, cx)
        })
        .ok();
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    if first(cx).as_deref() != Some("Reddit") {
        return format!("!reddi doesn't suggest Reddit ({:?})", first(cx));
    }
    press(window, "enter", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let query = window
        .update(cx, |launcher, _, cx| launcher.query(cx))
        .unwrap_or_default();
    if query != "!reddit " {
        return format!("Enter completed to {query:?}");
    }

    if let Err(why) = open_from_search(window, "search bangs", "Search Bangs", cx).await {
        return why;
    }
    let (title, rows) = window
        .update(cx, |launcher, _, _| {
            (launcher.screen_title(), launcher.result_titles().len())
        })
        .unwrap_or_default();
    if title.as_deref() != Some("Bangs") || rows < 5000 {
        return format!("list gave {title:?} with {rows} rows");
    }
    match back_to_search(window, cx).await {
        Ok(()) => format!("!reddit filled in, !reddi completed, {rows} listed, back to search"),
        Err(why) => why,
    }
}
