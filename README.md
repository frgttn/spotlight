# Spotlight

A fast keyboard launcher for Windows, in the spirit of Raycast and macOS Spotlight. Written in pure Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

> **Status: early.** The stack is validated and the code is split into the workspace planned in [docs/architecture.md](docs/architecture.md). So far: global hotkey, prewarmed window, root search over apps and system commands with frecency, an inline calculator, URLs and web search, built-in latency and memory measurements.

## What works today

- **Global hotkey:** Alt+Space by default, falling back to Ctrl+Shift+Space if another app already owns it. Change it in Settings.
- **Window:** frameless, topmost, prewarmed, DPI-aware, opens on the monitor under the mouse. It hides on Esc or when it loses focus.
- **Root search** merges built-in providers into one list:
  - **Calculator** ([fend](https://github.com/printfn/fend)) at the top when the query reads as a calculation: `12*12`, `1 km to mi`, `0xff to decimal`. Enter copies the answer.
  - **URLs:** `github.com`, `localhost:3000` or `http://…` get an *Open* row at the top.
  - **Apps:** fuzzy search ([nucleo](https://github.com/helix-editor/nucleo)) over all installed apps (Win32 and Microsoft Store/MSIX, via `shell:AppsFolder`).
  - **System commands:** Lock Screen, Sleep, Restart, Shut Down, Sign Out, Empty Recycle Bin, Toggle Dark Mode, Toggle Mute, also found by keywords ("reboot", "trash"). Restart, Shut Down and Sign Out ask for a second Enter.
  - **Web search** for the query (Google) as the last row.
- **Frecency:** apps and commands you use often and recently rank higher, and with an empty query they are listed first. A launch counts half as much after a week. Usage is stored in SQLite at `%APPDATA%\spotlight\spotlight.db`.
- **Keys:** ↑/↓ or Ctrl+N/P to move, Enter or a click to launch, Ctrl+, for settings, Ctrl+Q to quit.
- **Search field** edits like any Windows text box: Ctrl+←/→ moves by word, Shift selects (also with Home/End), Ctrl+Backspace / Ctrl+Delete delete a word, Ctrl+A/C/X/V, double click selects a word and triple click everything.
- **Text input** with IME support.
- **Icons:** real app icons from the shell, extracted in the background after startup (letter badges show until they arrive).
- **Single instance:** launching Spotlight again (Start menu, a shortcut) opens the running instance instead of starting a second one.
- **Tray icon:** left click opens the launcher; the menu has Open, Settings, Start at login and Quit.
- **Settings window** (Ctrl+, in the launcher, the tray menu, or `spotlight.exe --settings`), frameless like the launcher: drag it by the header, close it with ✕ or Esc.
  - *Launcher hotkey:* click the field and press a combination. While recording, a low-level keyboard hook catches the keys, so a combination another app already owns is caught too and reported as taken; the old hotkey then stays. The choice is saved in the database.
  - *Smooth scrolling:* the result list animates to its new position when you move the selection or turn the mouse wheel (touchpad scrolling is already smooth and is left alone). On by default.
  - *Start at login:* stored in the per-user `Run` registry key. Disabling it in Task Manager's Startup tab is respected. Also in the tray menu.

The binary uses the Windows GUI subsystem, so no console window opens. Logs still go to the terminal it was started from.

## Requirements

- Windows 10 1809+ (developed on Windows 11)
- Rust 1.88+ (`rustup`, `x86_64-pc-windows-msvc`)
- Visual Studio 2022 Build Tools with **Desktop development with C++** (MSVC and the Windows SDK; GPUI needs the SDK's DirectX shader compiler)

## Build and run

```bash
cargo run --release
```

Then press **Alt+Space** (or the hotkey you set).

| Environment variable | Effect |
|---|---|
| `SPOTLIGHT_SHOW_ON_START=1` | Show the window immediately |
| `SPOTLIGHT_SYNTHETIC=10000` | Add 10,000 filler rows (list performance test) |
| `SPOTLIGHT_SELFTEST=1` | Run the scripted measurement, print a summary, quit (skips the single-instance check, so it can run next to a normal instance) |
| `SPOTLIGHT_DATA_DIR=<dir>` | Keep the database in `<dir>` instead of `%APPDATA%\spotlight` |

The self-test flashes the window on screen for about 10 seconds, so don't type while it runs:

```bash
SPOTLIGHT_SELFTEST=1 SPOTLIGHT_SYNTHETIC=10000 cargo run --release
```

GPUI's own memory floor (one window, one label):

```bash
cargo run -p spotlight --example baseline --release
```

Results and the reasoning behind choosing GPUI: [docs/ui-framework-decision.md](docs/ui-framework-decision.md).

## Layout

| Path | What |
|---|---|
| `crates/app` | The `spotlight` binary: wiring, window lifecycle, tray icon, self-test, GPUI baseline example |
| `crates/ui` | GPUI views: launcher (search field, result list, metrics footer), settings and text input |
| `crates/core` | Domain types: `SearchItem`, actions and commands, the `Provider` trait, frecency |
| `crates/search` | Root search: merges providers; ranking by fuzzy score (title and keywords), prefix bonus, frecency |
| `crates/providers` | Built-in providers: apps, system commands, calculator, web |
| `crates/store` | SQLite (`rusqlite`, bundled): settings and frecency, with schema migrations |
| `crates/platform-win` | Windows code: hotkey thread, single instance, app discovery and icons, running commands (launch, open URL, system commands), autostart, window placement, measurements |
| `docs/` | Architecture plan and the UI framework decision |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

`crates/ui/src/text_input.rs` is adapted from GPUI's `examples/input.rs` (Copyright Zed Industries, Inc., Apache-2.0).

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

"Spotlight" is a trademark of Apple Inc. This project is not affiliated with Apple or Raycast.
