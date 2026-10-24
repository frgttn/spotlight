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
use spotlight_core::{
    item::Command,
    view::{Field, FieldKind, FieldValue, FormView, View},
};
use spotlight_platform_win::metrics::{self, selftest::*};
use spotlight_ui::launcher::{Launcher, WindowMode};

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
        ("text area", text_area_round_trip(window, cx).await),
        ("quicklink", quicklink_round_trip(window, cx).await),
        ("deeplink", deeplink_round_trip(window, cx).await),
        ("switcher", switcher_round_trip(window, cx).await),
        ("files", files_round_trip(window, cx).await),
        ("file prefix", prefix_round_trip(window, cx).await),
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
        let snippets = snippet_round_trip(window, cx).await;
        eprintln!(
            "[selftest] snippets: {snippets} {}",
            if snippets == SNIPPETS_DONE {
                "OK"
            } else {
                "FAIL"
            }
        );
        // Pastes for real, so only when asked, and only into the window
        // named (a test harness's text box, as a hex HWND).
        if let Some(target) = env::var("SPOTLIGHT_SELFTEST_PASTE")
            .ok()
            .and_then(|hwnd| isize::from_str_radix(hwnd.trim_start_matches("0x"), 16).ok())
        {
            let pasted = snippet_paste(window, target, cx).await;
            eprintln!("[selftest] snippet paste: {pasted}");
        }
    }

    window.update(cx, |launcher, _, cx| launcher.hide(cx)).ok();

    let compact = compact_round_trip(window, cx).await;
    eprintln!("[selftest] compact mode: {compact}");

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

/// Opens the launcher in compact mode and checks the window's height as it
/// collapses and grows: search field only, ↓ shows the results, Esc hides
/// them, typing shows them, clearing the query hides them. Returns how far
/// it got, with the heights seen.
async fn compact_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let height = |cx: &mut AsyncApp| {
        window
            .update(cx, |_, window, _| f32::from(window.viewport_size().height))
            .unwrap_or_default()
    };
    // Set apart from showing, as in Settings: the hidden window takes the
    // collapsed size meanwhile.
    window
        .update(cx, |launcher, _, cx| {
            launcher.set_window_mode(WindowMode::Compact, cx);
            launcher.set_pinned(true);
        })
        .ok();
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    window
        .update(cx, |launcher, window, cx| {
            launcher.show(Instant::now(), window, cx)
        })
        .ok();
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let mut seen = Vec::new();
    // (step, whether the results should show)
    let steps: [(&str, bool); 5] = [
        ("open", false),
        ("down", true),
        ("escape", false),
        ("type", true),
        ("ctrl-backspace", false),
    ];
    let mut result = "OK".to_owned();
    for (step, expanded) in steps {
        match step {
            "open" => {}
            "type" => {
                window
                    .update(cx, |launcher, window, cx| {
                        launcher.type_text("code", window, cx)
                    })
                    .ok();
            }
            keys => {
                press(window, keys, cx);
            }
        }
        executor.timer(Duration::from_millis(SETTLE_MS)).await;
        let height = height(cx);
        seen.push(format!("{step} {height:.0}"));
        // Collapsed is the search row alone (52); expanded is hundreds.
        if (height > 100.) != expanded && result == "OK" {
            result = format!("FAIL at {step}");
        }
    }
    window
        .update(cx, |launcher, _, cx| {
            launcher.set_pinned(false);
            launcher.hide(cx);
            launcher.set_window_mode(WindowMode::Expanded, cx);
        })
        .ok();
    format!("{result} ({})", seen.join(", "))
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

/// A `spotlight://` link to Clipboard History, by its title's slug: the
/// launcher shows just that command and asks, Enter opens it with the
/// link's fallback text in the search field.
/// Switch Windows lists the open windows, Search Processes this session's
/// processes; Enter isn't pressed in either (it would switch or end one).
async fn switcher_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let mut counts = Vec::new();
    for title in ["Switch Windows", "Search Processes"] {
        if let Err(err) = open_from_search(window, title, title, cx).await {
            return err;
        }
        let opened = window
            .update(cx, |launcher, _, _| {
                (launcher.screen_title(), launcher.result_titles().len())
            })
            .ok();
        match opened {
            Some((Some(screen), rows)) if screen == title && rows > 0 => counts.push(rows),
            other => return format!("{title} gave {other:?}"),
        }
        if let Err(err) = back_to_search(window, cx).await {
            return err;
        }
    }
    format!(
        "{} windows, {} processes, back to search",
        counts[0], counts[1]
    )
}

/// Search Files: once the index is built, typing finds the Desktop folder
/// of the user profile, Tab's Folders filter keeps it, and Esc goes back.
/// Enter isn't pressed (it would open Explorer).
/// `*desktop` in root search lists the Desktop folder (files only), and Tab
/// goes on in Search Files with `desktop` typed.
async fn prefix_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let titles = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, _| launcher.result_titles())
            .unwrap_or_default()
    };
    let mut found = false;
    for _ in 0..20 {
        press(window, "ctrl-a", cx);
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text("*desktop", window, cx)
            })
            .ok();
        executor.timer(Duration::from_millis(300)).await;
        let rows = titles(cx);
        if rows.iter().any(|title| title == "Desktop") {
            if rows.iter().any(|title| title == "Calculator") {
                return format!("root results mixed in: {rows:?}");
            }
            found = true;
            break;
        }
    }
    if !found {
        return format!("*desktop gave {:?}", titles(cx));
    }
    press(window, "tab", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let opened = window
        .update(cx, |launcher, _, cx| {
            (launcher.screen_title(), launcher.query(cx))
        })
        .ok();
    if opened != Some((Some("Search Files".into()), "desktop".into())) {
        return format!("Tab gave {opened:?}");
    }
    if let Err(err) = back_to_search(window, cx).await {
        return err;
    }
    "found Desktop, Tab opened Search Files with it, back to search".into()
}

async fn files_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    /// Indexing a large profile takes a while; this is how long to wait.
    const TRIES: usize = 120;
    let executor = cx.background_executor().clone();
    if let Err(err) = open_from_search(window, "search files", "Search Files", cx).await {
        return err;
    }
    let titles = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, _| launcher.result_titles())
            .unwrap_or_default()
    };
    let started = Instant::now();
    let mut found = false;
    for _ in 0..TRIES {
        // Typed again each time: the list asks for rows as the text changes.
        press(window, "ctrl-a", cx);
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text("desktop", window, cx)
            })
            .ok();
        executor.timer(Duration::from_millis(500)).await;
        if titles(cx).iter().any(|title| title == "Desktop") {
            found = true;
            break;
        }
    }
    if !found {
        return format!(
            "Desktop not found after {:.0} s",
            started.elapsed().as_secs_f64()
        );
    }
    let waited = started.elapsed().as_secs_f64();
    press(window, "tab", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let folders = titles(cx);
    if !folders.iter().any(|title| title == "Desktop") {
        return format!("the Folders filter dropped Desktop: {folders:?}");
    }
    if let Err(err) = back_to_search(window, cx).await {
        return err;
    }
    format!("found Desktop after {waited:.1} s, kept by Folders, back to search")
}

async fn deeplink_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    const LINK: &str =
        "spotlight://extensions/spotlight/clipboard/clipboard-history?fallbackText=zzq";
    let executor = cx.background_executor().clone();
    window
        .update(cx, |launcher, window, cx| {
            launcher.open_link(LINK, window, cx)
        })
        .ok();
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let rows = window
        .update(cx, |launcher, _, _| launcher.result_titles())
        .unwrap_or_default();
    if rows != ["Clipboard History"] {
        return format!("link showed {rows:?}");
    }
    press(window, "ctrl-k", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let actions = window
        .update(cx, |launcher, _, _| launcher.panel_actions())
        .ok()
        .flatten();
    if actions.as_deref() != Some(&["Open Command".to_owned(), "Always Open Command".to_owned()]) {
        return format!("asked with {actions:?}");
    }
    press(window, "ctrl-k", cx);
    press(window, "enter", cx);
    executor.timer(Duration::from_millis(SETTLE_MS)).await;
    let opened = window
        .update(cx, |launcher, _, cx| {
            (launcher.screen_title(), launcher.query(cx))
        })
        .ok();
    if opened != Some((Some("Clipboard History".into()), "zzq".into())) {
        return format!("Enter gave {opened:?}");
    }
    match back_to_search(window, cx).await {
        Ok(()) => "asked, opened with its text, back to search".into(),
        Err(err) => err,
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

/// A form with a text area: Enter starts a new line there instead of
/// submitting, ↑ goes up a row, Ctrl+End to the end, and Ctrl+Enter submits
/// (refused while it's empty, so nothing runs). `SPOTLIGHT_SELFTEST_HOLD=<ms>`
/// keeps it open that long with a long wrapped line, for a look from outside.
async fn text_area_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let settle = || executor.timer(Duration::from_millis(SETTLE_MS));
    let form = FormView {
        title: "Selftest Text Area".into(),
        fields: vec![
            Field {
                id: "name".into(),
                label: "Name".into(),
                kind: FieldKind::Text {
                    placeholder: "Name".into(),
                    default: String::new(),
                    required: false,
                },
            },
            Field {
                id: "text".into(),
                label: "Snippet".into(),
                kind: FieldKind::TextArea {
                    placeholder: "Text".into(),
                    default: String::new(),
                    required: true,
                },
            },
        ],
        submit_title: "Save".into(),
        submit: Command::Copy("{text}".into()),
    };
    window
        .update(cx, |launcher, window, cx| {
            launcher.open_view(View::Form(form), window, cx)
        })
        .ok();
    settle().await;
    let type_text = |text: &str, cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text(text, window, cx)
            })
            .ok();
    };
    let text = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, cx| launcher.form_state(cx))
            .ok()
            .flatten()
            .and_then(|(values, _)| match values.get(1) {
                Some(FieldValue::Text(text)) => Some(text.clone()),
                _ => None,
            })
    };

    press(window, "tab", cx);
    type_text("one", cx);
    press(window, "enter", cx);
    type_text("two", cx);
    settle().await;
    if text(cx).as_deref() != Some("one\ntwo") {
        return format!("Enter didn't start a line ({:?})", text(cx));
    }
    press(window, "up", cx);
    type_text("X", cx);
    press(window, "ctrl-end", cx);
    type_text("!", cx);
    settle().await;
    if text(cx).as_deref() != Some("oneX\ntwo!") {
        return format!("↑ or Ctrl+End went wrong ({:?})", text(cx));
    }

    if let Some(hold) = env::var("SPOTLIGHT_SELFTEST_HOLD")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
    {
        press(window, "enter", cx);
        type_text(
            "A long line that has to wrap: the quick brown fox jumps over the lazy dog, \
             then over the fence, the river and the hill behind the old mill.",
            cx,
        );
        press(window, "shift-up", cx);
        eprintln!("[selftest] text area held open for {hold} ms");
        executor.timer(Duration::from_millis(hold)).await;
    }

    press(window, "ctrl-a", cx);
    press(window, "backspace", cx);
    press(window, "ctrl-enter", cx);
    settle().await;
    let refused = window
        .update(cx, |launcher, _, cx| launcher.form_state(cx))
        .ok()
        .flatten()
        .and_then(|(_, error)| error);
    if refused.as_deref() != Some("Snippet is required") {
        return format!("lines fine, but Ctrl+Enter gave {refused:?}");
    }
    match back_to_search(window, cx).await {
        Ok(()) => {
            "Enter adds a line, ↑ and Ctrl+End move, Ctrl+Enter refused empty, back to search"
                .into()
        }
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
    if first.as_deref() != Some("GitHub “rust gpui”") {
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

const SNIPPETS_DONE: &str = "saved, taken keyword refused, listed, exported, deleted, imported, imported again: nothing new";

/// Creates a snippet through its form (Ctrl+Enter saves: Enter is a new
/// line in its text), refuses a second one with the same keyword, finds it
/// in Search Snippets, exports it, deletes it from the action panel, imports
/// the export and deletes it again. Nothing is pasted.
async fn snippet_round_trip(window: WindowHandle<Launcher>, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let settle = || executor.timer(Duration::from_millis(SETTLE_MS));
    let type_text = |text: &str, cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text(text, window, cx)
            })
            .ok();
    };
    let state = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, cx| {
                (launcher.screen_title(), launcher.form_state(cx))
            })
            .ok()
    };
    let titles = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, _| launcher.result_titles())
            .unwrap_or_default()
    };

    // Creates "Selftest Snippet" (or another name) with keyword ;zzs.
    let create = async |name: &str, cx: &mut AsyncApp| -> Result<(), String> {
        open_from_search(window, "create snippet", "Create Snippet", cx).await?;
        type_text(name, cx);
        press(window, "tab", cx);
        type_text("Dear {argument name=\"Who\"},", cx);
        press(window, "enter", cx);
        type_text("{cursor}", cx);
        press(window, "tab", cx);
        type_text(";zzs", cx);
        press(window, "ctrl-enter", cx);
        Ok(())
    };
    // Search Snippets, filtered to "selftest".
    let open_list = async |cx: &mut AsyncApp| -> Result<(), String> {
        open_from_search(window, "search snippets", "Search Snippets", cx).await?;
        type_text("selftest", cx);
        Ok(())
    };
    // Deletes the selected snippet from the action panel (it asks to confirm).
    let delete = |cx: &mut AsyncApp| -> Result<(), String> {
        press(window, "ctrl-k", cx);
        let actions = window
            .update(cx, |launcher, _, _| launcher.panel_actions())
            .ok()
            .flatten()
            .unwrap_or_default();
        let Some(ix) = actions.iter().position(|title| title == "Delete Snippet") else {
            return Err(format!("no Delete in {actions:?}"));
        };
        for _ in 0..ix {
            press(window, "down", cx);
        }
        press(window, "enter", cx);
        press(window, "enter", cx);
        Ok(())
    };

    if let Err(why) = create("Selftest Snippet", cx).await {
        return why;
    }
    settle().await;
    if !matches!(state(cx), Some((None, None))) {
        return format!("not saved ({:?})", state(cx));
    }
    if let Err(why) = create("Selftest Again", cx).await {
        return why;
    }
    settle().await;
    let refused = state(cx)
        .and_then(|(_, form)| form)
        .and_then(|(_, error)| error);
    if refused.as_deref() != Some(";zzs already pastes “Selftest Snippet”") {
        return format!("saved, but the same keyword gave {refused:?}");
    }
    if let Err(why) = back_to_search(window, cx).await {
        return why;
    }

    if let Err(why) = open_list(cx).await {
        return why;
    }
    settle().await;
    if titles(cx) != ["Selftest Snippet"] {
        return format!("saved, but the list has {:?}", titles(cx));
    }

    let file = env::temp_dir().join(format!("spotlight-selftest-{}.json", std::process::id()));
    let exported = window
        .update(cx, |launcher, _, _| launcher.export_snippets_to(&file))
        .unwrap_or_else(|_| Err("window gone".into()));
    let json = std::fs::read_to_string(&file).unwrap_or_default();
    if exported != Ok(1) || !json.contains(r#""keyword": ";zzs""#) {
        let _ = std::fs::remove_file(&file);
        return format!("listed, but the export gave {exported:?}: {json}");
    }

    if let Err(why) = delete(cx) {
        let _ = std::fs::remove_file(&file);
        return why;
    }
    settle().await;
    if !titles(cx).is_empty() {
        let _ = std::fs::remove_file(&file);
        return format!("exported, but still listed: {:?}", titles(cx));
    }

    let import = |cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, _, _| launcher.import_snippets_from(&file))
            .unwrap_or_default()
    };
    let first = import(cx);
    let second = import(cx);
    let _ = std::fs::remove_file(&file);
    if first != "Imported 1 snippet" || second != "Nothing new to import" {
        return format!("deleted, but importing gave {first:?}, then {second:?}");
    }
    if let Err(why) = back_to_search(window, cx).await {
        return why;
    }
    if let Err(why) = open_list(cx).await {
        return why;
    }
    settle().await;
    if titles(cx) != ["Selftest Snippet"] {
        return format!("imported, but the list has {:?}", titles(cx));
    }
    if let Err(why) = delete(cx) {
        return why;
    }
    settle().await;
    if let Err(why) = back_to_search(window, cx).await {
        return why;
    }
    SNIPPETS_DONE.into()
}

/// Saves `<b>{cursor}</b>` as a snippet, pastes it from Search Snippets into
/// `target`, then deletes it. What arrived there (`<b></b>`, the caret
/// before `</b>`) is for the harness to check. When another window was in
/// front before the launcher, nothing is pasted: keys would go to that app.
async fn snippet_paste(window: WindowHandle<Launcher>, target: isize, cx: &mut AsyncApp) -> String {
    let executor = cx.background_executor().clone();
    let settle = || executor.timer(Duration::from_millis(SETTLE_MS));
    let type_text = |text: &str, cx: &mut AsyncApp| {
        window
            .update(cx, |launcher, window, cx| {
                launcher.type_text(text, window, cx)
            })
            .ok();
    };
    let open_list = async |cx: &mut AsyncApp| -> Result<(), String> {
        open_from_search(window, "search snippets", "Search Snippets", cx).await?;
        type_text("selftest paste", cx);
        settle().await;
        Ok(())
    };

    // Hidden and shown again, so the window in front is the one to paste into.
    window.update(cx, |launcher, _, cx| launcher.hide(cx)).ok();
    executor.timer(Duration::from_millis(HIDDEN_MS)).await;
    window
        .update(cx, |launcher, window, cx| {
            launcher.show(Instant::now(), window, cx)
        })
        .ok();
    settle().await;

    if let Err(why) = open_from_search(window, "create snippet", "Create Snippet", cx).await {
        return why;
    }
    type_text("Selftest Paste", cx);
    press(window, "tab", cx);
    type_text("<b>{cursor}</b>", cx);
    press(window, "ctrl-enter", cx);
    settle().await;
    if let Err(why) = open_list(cx).await {
        return why;
    }
    let listed = window
        .update(cx, |launcher, _, _| launcher.result_titles())
        .unwrap_or_default();
    let previous = window
        .update(cx, |launcher, _, _| launcher.previous_window())
        .unwrap_or_default();
    // Otherwise Ctrl+Enter, Copy Snippet: it only puts it on the clipboard.
    let paste = if previous == target {
        "enter"
    } else {
        "ctrl-enter"
    };
    if listed != ["Selftest Paste"] {
        return format!("not listed ({listed:?})");
    }
    press(window, paste, cx);
    // The paste happens on a thread of its own once the window is in front.
    executor.timer(Duration::from_millis(1000)).await;

    window
        .update(cx, |launcher, window, cx| {
            launcher.show(Instant::now(), window, cx)
        })
        .ok();
    settle().await;
    if let Err(why) = open_list(cx).await {
        return why;
    }
    press(window, "ctrl-k", cx);
    let actions = window
        .update(cx, |launcher, _, _| launcher.panel_actions())
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Some(ix) = actions.iter().position(|title| title == "Delete Snippet") {
        for _ in 0..ix {
            press(window, "down", cx);
        }
        press(window, "enter", cx);
        press(window, "enter", cx);
    }
    settle().await;
    press(window, "escape", cx);
    window.update(cx, |launcher, _, cx| launcher.hide(cx)).ok();
    if previous == target {
        format!("pasted into {target:#x}")
    } else {
        format!("copied only: {previous:#x} was in front, not {target:#x}")
    }
}
