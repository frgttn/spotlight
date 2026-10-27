# Spotlight

A fast keyboard launcher for Windows, in the spirit of Raycast and macOS Spotlight. Written in pure Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

> **Status: early.** The stack is validated and the code is split into the workspace planned in [docs/architecture.md](docs/architecture.md). So far: global hotkey, prewarmed window, root search over apps and system commands with frecency, an inline calculator, URLs and web search, built-in latency and memory measurements.

## What works today

- **Global hotkey:** Alt+Space by default, falling back to Ctrl+Shift+Space if another app already owns it. Change it in Settings.
- **Window:** frameless, topmost, prewarmed, DPI-aware, opens on the monitor under the mouse. It hides on Esc or when it loses focus.
- **Root search** merges built-in providers into one list:
  - **Calculator** ([fend](https://github.com/printfn/fend)) at the top when the query reads as a calculation: `12*12`, `1 km to mi`, `0xff to decimal`, `100 usd to eur`. Enter copies the answer. Currency rates come from [ExchangeRate-API](https://www.exchangerate-api.com)'s open feed (166 currencies, updated daily), are cached in `rates.json` next to the database and refreshed in the background after 12 hours, so conversions also work offline. Answers you use (Enter, Copy) go to **Calculator History**, a command that opens a list of its own: search it, Enter copies an answer, Ctrl+K also has Copy Calculation, Delete Entry and Clear History; Esc goes back. It keeps the last 200.
- **Views** open on top of search, and Esc goes back: lists (Calculator History), grids (**Browse Apps**, every app as a large tile; arrows move), details with markdown (**Keyboard Shortcuts**, and *Show Details* in an app's Ctrl+K) and forms (**Search the Web…**: a query and an engine; Tab moves between fields, Enter submits).
  - **URLs:** `github.com`, `localhost:3000` or `http://…` get an *Open* row at the top.
  - **Apps:** fuzzy search ([nucleo](https://github.com/helix-editor/nucleo)) over all installed apps (Win32 and Microsoft Store/MSIX, via `shell:AppsFolder`).
  - **Settings pages:** about 90 pages of the Windows Settings app (Display, Wi-Fi, Default Apps, Startup Apps…), found by title or keyword ("wallpaper", "uninstall").
  - **Control Panel:** every item of "All Control Panel Items" on this PC (Device Manager, Mouse, Credential Manager…), with their own icons.
  - **Games** from Steam, Epic, GOG and Ubisoft Connect, read from the launchers' own records; games that already have a Start menu shortcut aren't listed twice. Enter starts the game through its launcher.
  - **System commands:** Lock Screen, Sleep, Restart, Shut Down, Sign Out, Empty Recycle Bin, Toggle Dark Mode, Toggle Mute, Volume Up / Down, Toggle Bluetooth, Toggle HDR (on the display under the mouse pointer), Eject All Drives and Night Light Settings, also found by keywords ("reboot", "trash"). Restart, Shut Down and Sign Out ask for a second Enter. `volume 40` sets the volume; `eject` lists each removable drive (USB sticks, external USB disks, optical drives). The result appears in the HUD ("Bluetooth Off", "Volume 40%", or why it failed). Night light only has a Settings page: Windows has no public way to switch it.
  - **Quicklinks:** links you save under a name: a URL, a folder or file path (`%USERPROFILE%` works), or an app's deeplink (`vscode://…`). Each is a command of its own, ranked by use, with aliases and hotkeys like any other. Placeholders as in Raycast: `{query}`, `{argument name="Repo" default="zed" options="a,b"}` (a link with arguments opens a form for them), `{date}`, `{time}`, `{uuid}` filled when it opens, `{selection}` and `{clipboard}`. A web link shows its site's icon, fetched from the site itself (its `<link rel="icon">`, else `/favicon.ico`; no icon service) and kept in the database for a week. Ctrl+K → *Use as Web Search* on a link with `{query}` makes it the engine of the "Search … for" row. Give one a keyword to use it like a search bang: `!gh tokio` (or `tokio !gh`) opens GitHub's search for "tokio" straight away. A link that takes no typed text opens with Enter on its full keyword.
  - **Keywords:** `!` plus part of a keyword (`!g`) lists the quicklinks whose keyword starts with it, and Enter completes one to `!gh `. About 125 come preinstalled (search engines, dev docs, package registries, media, security and more; see `quicklink::DEFAULTS`) and can be edited or deleted; they stay out of root search, so reach them with `!keyword` or *Search Quicklinks*, while your own and edited ones are listed there as usual. *Create Quicklink* and Ctrl+K → *Edit Quicklink…* take a name, link, keyword and the browser to open it with (Default App or any browser registered on this PC).
  - **QR codes:** *Generate QR Code* (a form), `qr <text>`, *Generate QR Code from Selection* or *from Clipboard* shows a code for any text, with Copy Image, Save Image… (PNG) and Copy Text. *Scan QR Code from Screen* hides the launcher, captures every display and opens what the codes say (one code opens as it is, several are listed); *Scan QR Code from Clipboard* reads the copied image, or a copied picture file. What a code says is read for what it is: Wi-Fi networks (Connect to Network, which saves the network to Windows and joins it; Copy Password), contacts (Add to Contacts opens it as a vCard; Copy Phone Number, Send Email), emails, phone numbers (Call), text messages, places (Open in Maps) and events (Add to Calendar opens it as an .ics), with their details beside the code; links get Open Link. Images copied into clipboard history are read for QR codes when copied, so search finds a screenshot by its code's text, and the entry gets Show QR Code, Copy QR Code Text and Open QR Code Link. Both made and read with pure Rust ([qrcode](https://github.com/kennytm/qrcode-rust), [rqrr](https://github.com/WanzenBug/rqrr)); codes drawn light on dark are read too.
  - **Web search** for the query as the last row, with the engine chosen in Settings: Google (default), Bing, DuckDuckGo, Brave Search, Ecosia, Perplexity, or a custom results URL with `{query}` (Kagi, Startpage…).
- **Frecency:** apps and commands you use often and recently rank higher, and with an empty query they are listed first. A launch counts half as much after a week. Settings → General → Rank by Usage turns this off (usage is still recorded, so turning it back on restores the order). Usage is stored in SQLite at `%APPDATA%\spotlight\spotlight.db`: SQLCipher, with the key in Windows Credential Manager (`Spotlight/database`); clipboard images next to it are encrypted too.
- **Keys:** ↑/↓ or Ctrl+N/P to move, Enter or a click to run the primary action, Ctrl+Enter for the second one, Ctrl+, for settings, Ctrl+Q to quit.
- **Action panel (Ctrl+K):** every action of the selected item. Apps: Open, Run as Administrator, Show in Explorer, Copy Path (Win32 apps with a file on disk), Uninstall… (opens Installed apps in Settings). Calculator: Copy Answer, Copy Calculation. URLs: Open, Copy URL. Anything you've used also gets Reset Ranking. Esc closes the panel; the footer shows what Enter does.
- **Aliases:** Set Alias… in the panel gives an app or command a short name. Typing exactly that name puts it first (`vs` → Visual Studio Code); it also matches like a keyword. An alias belongs to one item: giving it to another moves it. Shown as a tag in the row.
- **Item hotkeys:** Set Hotkey… records a global shortcut that runs the item's primary action without opening the launcher (Restart, Shut Down and Sign Out open it on that item and wait for Enter). Combinations another app owns are reported as taken. Shown as key caps in the row. Aliases and hotkeys are stored in the database, and all of them are listed in Settings, where you can edit an alias in place, re-record a hotkey or remove either.
- **HUD:** after copying or switching the theme, a short message appears at the bottom of the screen without taking focus. It is the launcher window itself, shrunk, so it costs no extra memory. System commands report their result there ("Bluetooth Off", "Volume 40%"), and any command that fails says why ("Couldn't open it: the file is gone"). Actions that keep the window open (Reset Ranking, aliases, history) confirm in the footer instead.
- **Search field** edits like any Windows text box: Ctrl+←/→ moves by word, Shift selects (also with Home/End), Ctrl+Backspace / Ctrl+Delete delete a word, Ctrl+A/C/X/V, double click selects a word and triple click everything.
- **Text input** with IME support.
- **Icons:** real app icons from the shell, extracted in the background after startup (letter badges show until they arrive).
- **Single instance:** launching Spotlight again (Start menu, a shortcut) opens the running instance instead of starting a second one.
- **Tray icon:** left click opens the launcher; the menu has Open, Settings, Start at login, Check for Updates (Restart to Update once one is downloaded) and Quit.
- **Settings window** (Ctrl+, in the launcher, the tray menu, or `spotlight.exe --settings`), frameless like the launcher: drag it by the header, close it with ✕ or Esc.
  - *Launcher hotkey:* click the field and press a combination. While recording, a low-level keyboard hook catches the keys, so a combination another app already owns is caught too and reported as taken; the old hotkey then stays. The choice is saved in the database.
  - *Smooth scrolling:* the result list animates to its new position when you move the selection or turn the mouse wheel (touchpad scrolling is already smooth and is left alone). On by default.
  - *Search sources:* a switch for each provider (Applications, Settings Pages, Control Panel, Games, System Commands, Calculator, Quicklinks, Web Search and URLs, Spotlight Commands). A source that's off loads nothing (the calculator then fetches no exchange rates) and adds no results; turning it back on takes effect at once. A hotkey given to one of its items says in the HUD that the source is off. Saved as `disabled_providers`.
  - *Start at login:* stored in the per-user `Run` registry key. Disabling it in Task Manager's Startup tab is respected. Also in the tray menu.

The binary uses the Windows GUI subsystem, so no console window opens. Logs still go to the terminal it was started from.

## Install

Download `Spotlight-win-Setup.exe` from the [latest release](https://github.com/frgttn/spotlight/releases/latest) and run it. It installs for your user into `%LocalAppData%\Spotlight` without an administrator prompt, adds Spotlight to the Start menu and turns on start at login. Updates download in the background; *Restart to Update* in the tray menu or Settings › About applies them, or the next start does. Uninstall from Windows Settings › Apps: that removes start at login, the `spotlight://` links and the file index service (with a UAC prompt) too, and keeps your data in `%APPDATA%\spotlight`.

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

## Releasing

1. Set the new `version` in the workspace `Cargo.toml`.
2. Move the notes under `[Unreleased]` in [CHANGELOG.md](CHANGELOG.md) to a section `## [<version>] - <date>`.
3. Commit, then tag and push: `git tag v<version>` and `git push origin v<version>`.

[`release.yml`](.github/workflows/release.yml) then builds, packs with [Velopack](https://velopack.io) and publishes the GitHub release, which installed copies update from. A copy run from `target/` (not installed) doesn't update itself.

## Layout

| Path | What |
|---|---|
| `crates/app` | The `spotlight` binary: wiring, window lifecycle, tray icon, self-test, GPUI baseline example |
| `crates/ui` | GPUI views: launcher (search field, result list, metrics footer), settings and text input |
| `crates/core` | Domain types: `SearchItem`, actions and commands, the `Provider` trait, frecency |
| `crates/search` | Root search: merges providers; ranking by fuzzy score (title and keywords), prefix bonus, frecency |
| `crates/files` | File index for Search Files: entry table changed in place, unique names in an arena and an `fst`, ranked search with `neo_frizbee`, a parallel walk with exclusions (examples: `bench`, `walk`) |
| `crates/providers` | Built-in providers: apps, system commands, calculator, web, quicklinks, QR codes |
| `crates/qr` | QR codes: made from text (`qrcode`), read from images (`rqrr`), and the views that show them |
| `crates/store` | SQLCipher (`rusqlite`, bundled; encrypted SQLite): settings, frecency, clipboard history, snippets, quicklinks, with schema migrations |
| `crates/platform-win` | Windows code: hotkey thread, single instance, app discovery and icons, running commands (launch, open URL, system commands), autostart, window placement, folder listing and change watching for the file index, NTFS MFT and USN journal reading, the index service's protocol, measurements |
| `crates/service` | `spotlight-service.exe`, the optional file index service: reads NTFS file tables and change journals as LocalSystem, keeps sealed snapshots, serves the launcher over a named pipe with caller and access checks |
| `vendor/libsqlite3-sys` | `libsqlite3-sys` patched so SQLCipher uses Windows CNG instead of OpenSSL (`sqlcipher/crypto_cng.c`); no OpenSSL or Perl needed to build |
| `docs/` | Architecture plan and the UI framework decision |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

`crates/ui/src/text_input.rs` is adapted from GPUI's `examples/input.rs` (Copyright Zed Industries, Inc., Apache-2.0).

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

"Spotlight" is a trademark of Apple Inc. This project is not affiliated with Apple or Raycast.
