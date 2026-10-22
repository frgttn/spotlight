# Spotlight

A fast keyboard launcher for Windows, in the spirit of Raycast and macOS Spotlight. Written in pure Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

> **Status: early spike.** The repository currently contains the prototype used to validate the stack:
> - global hotkey and a prewarmed window;
> - fuzzy search over installed apps;
> - launching apps;
> - built-in latency and memory measurements.
>
> The full app is planned in [docs/architecture.md](docs/architecture.md).

## What works today

- **Global hotkey:** Alt+Space, falling back to Ctrl+Shift+Space if another app already owns it.
- **Window:** frameless, topmost, prewarmed, DPI-aware, opens on the monitor under the mouse. It hides on Esc or when it loses focus.
- **Search:** fuzzy search ([nucleo](https://github.com/helix-editor/nucleo)) over all installed apps (Win32 and Microsoft Store/MSIX, via `shell:AppsFolder`).
- **Keys:** ↑/↓ or Ctrl+N/P to move, Enter or a click to launch, Ctrl+Backspace to clear the query, Ctrl+Q to quit.
- **Text input** with IME support.

App icons are placeholders (coloured letter badges) for now.

## Requirements

- Windows 10 1809+ (developed on Windows 11)
- Rust 1.88+ (`rustup`, `x86_64-pc-windows-msvc`)
- Visual Studio 2022 Build Tools with **Desktop development with C++** (MSVC and the Windows SDK; GPUI needs the SDK's DirectX shader compiler)

## Build and run

```bash
cargo run -p spike-gpui --release
```

Then press **Alt+Space**.

| Environment variable | Effect |
|---|---|
| `SPIKE_SHOW_ON_START=1` | Show the window immediately |
| `SPIKE_SYNTHETIC=10000` | Add 10,000 filler rows (list performance test) |
| `SPIKE_SELFTEST=1` | Run the scripted measurement, print a summary, quit |

The self-test flashes the window on screen for about 10 seconds, so don't type while it runs:

```bash
SPIKE_SELFTEST=1 SPIKE_SYNTHETIC=10000 cargo run -p spike-gpui --release
```

GPUI's own memory floor (one window, one label):

```bash
cargo run -p spike-gpui --example baseline --release
```

Results and the reasoning behind choosing GPUI: [docs/ui-framework-decision.md](docs/ui-framework-decision.md).

## Layout

| Path | What |
|---|---|
| `crates/spike-common` | UI-independent Windows code: hotkey thread, app discovery and launch, window placement, fuzzy search, measurements |
| `spikes/gpui` | The GPUI launcher: text input, result list, metrics footer, self-test |
| `docs/` | Architecture plan and the UI framework decision |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

`spikes/gpui/src/text_input.rs` is adapted from GPUI's `examples/input.rs` (Copyright Zed Industries, Inc., Apache-2.0).

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

"Spotlight" is a trademark of Apple Inc. This project is not affiliated with Apple or Raycast.
