# Architecture plan

Spotlight is a keyboard launcher for Windows, in the spirit of Raycast and Spotlight, written in pure Rust.

**Decisions:**
- **Single native process** in Rust, with a **GPUI** UI. Why GPUI: see [ui-framework-decision.md](ui-framework-decision.md).
- **No JavaScript runtime:** no Node/Bun process and no embedded JS engine. Compatibility with Raycast's extension API is out of scope.
- **Extensibility in three layers:**
  1. Built-in commands in Rust.
  2. **Script commands** (phase 1).
  3. **WASM plugins** (phase 2).

---

## 1. Process model

```
spotlight.exe (single process, Rust)
├─ UI thread: GPUI app. Launcher window (prewarmed, hidden), settings window
├─ tokio runtime: search providers, indexing, IO, script runs
├─ win thread: message loop for low-level keyboard hook + WinEvent hooks + clipboard listener
└─ child processes: script commands only (short-lived, bounded by timeout and output limits)
```

Inside the process, components talk over typed Rust channels (`tokio::sync::mpsc` / `async-channel`), so there's no JSON-RPC internally. JSON appears only at external boundaries: script commands, deeplinks and the WASM plugin interface.

Later, and optional: a tiny elevated helper for interacting with admin windows. It isn't needed for the MVP.

---

## 2. Cargo workspace layout (target)

```
/crates
  app            # binary: wiring, GPUI app, window lifecycle, tray, single-instance
  ui             # GPUI views: launcher, list, action panel, detail (markdown), form, settings
  core           # domain types: SearchItem, Action, Provider trait, ranking, frecency
  search         # aggregator: fan-out to providers, cancel on new keystroke, merge + rank (nucleo)
  platform-win   # windows-rs: hotkey/hook, focus, window mgmt, clipboard, UIA, app discovery, icons
  store          # rusqlite (+ SQLCipher later): settings, frecency, clipboard history, snippets, quicklinks
  providers/
    apps         # shell:AppsFolder (Win32 + MSIX), icon extraction, launch
    system       # lock, sleep, restart, empty bin, toggle dark mode, ...
    calc         # fend-core (units, currency later), inline result
    web          # URL detection + web search fallback
    quicklinks   # templated URLs with {query}
    snippets     # store + text expansion (keyboard hook buffer)
    clipboard    # history provider
    windows      # window management commands
    files        # Windows Search / Everything SDK first, own indexer later
    scripts      # script commands
  plugin-host    # (phase 2) wasmtime + component model host
  plugin-api     # (phase 2) WIT definitions + Rust guest SDK crate for plugin authors
```

**Rule: providers never touch the UI.** They return data (`SearchItem`, `View`), and `ui` renders it. A script or plugin view is therefore rendered by exactly the same code as a built-in one.

GPUI-specific code stays in `ui`, behind our own `View` model. GPUI is pinned (`=0.2.2`) and its API changes often, so this keeps upgrades contained.

---

## 3. Core contracts

```rust
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn search(&self, query: &Query, cx: SearchCx) -> Vec<SearchItem>; // cx carries a cancel token
    async fn run(&self, item: &ItemId, action: &ActionId, cx: RunCx) -> RunOutcome;
}

pub struct SearchItem {
    pub id: ItemId,              // "{provider}:{instance}", stable for frecency
    pub title: String, pub subtitle: Option<String>, pub keywords: Vec<String>,
    pub icon: Icon,              // Path | Builtin(name) | Bytes
    pub accessories: Vec<Accessory>,
    pub actions: Vec<Action>,    // [0] = primary (Enter), the rest are in the Ctrl+K panel
}

pub enum RunOutcome { Close, KeepOpen, PushView(View), Hud(String), Toast(Toast), Error(String) }

pub enum View {                  // declarative, serializable (serde): reused for scripts and WASM
    List   { items: Vec<SearchItem>, searchable: bool, placeholder: Option<String> },
    Detail { markdown: String, metadata: Vec<(String, String)>, actions: Vec<Action> },
    Form   { fields: Vec<Field>, submit: Action },
}
```

**Ranking v1** adds three parts:
- the `nucleo` fuzzy score over title, keywords and aliases;
- a frecency score (visit count with exponential time decay, stored in SQLite);
- a bonus for exact prefix matches.

---

## 4. Script commands (phase 1 extensibility)

Script commands are a simple, language-agnostic way to extend the launcher.

- **Location:** `%APPDATA%\spotlight\scripts\`, plus folders the user adds, watched with `notify`.
- **Metadata** goes in a header comment, so a `.ps1`, `.py`, `.cmd` or `.exe` file needs no other config:
  ```
  # @spotlight.title      Toggle VPN
  # @spotlight.mode       silent | hud | list | detail
  # @spotlight.argument1  { "placeholder": "query", "optional": true }
  # @spotlight.icon       🔒
  ```
- **Running:** the script is spawned with its arguments. A timeout and a cap on stdout size protect the app.
- **Output by mode:**
  - `silent` / `hud`: the last line of stdout becomes the HUD text.
  - `list` / `detail`: stdout is **JSON matching `View`**, using the same serde types as the core.
- **Actions** from scripts are limited to safe built-ins: open URL, copy, paste, run the script again with arguments, open a path.

---

## 5. WASM plugins (phase 2)

- **Runtime:** `wasmtime` + the **component model** + WIT, the same approach Zed uses for its extensions.
- **Languages:** Rust, or any language that compiles to WASI components. If an author wants JS via componentize, that's their choice; we don't ship a JS engine.
- **WIT sketch:**
  ```wit
  package spotlight:plugin@0.1.0;
  interface types { /* search-item, action, view: mirror core::View */ }
  interface host {                      // capabilities, each gated by a manifest permission
    http-fetch: func(req: request) -> result<response, string>;
    clipboard-read: func() -> option<string>;
    clipboard-write: func(text: string);
    storage-get: func(key: string) -> option<string>;
    storage-set: func(key: string, value: string);
    open-url: func(url: string);
    show-hud: func(text: string);
  }
  world plugin {
    import host;
    export search: func(query: string) -> list<search-item>;
    export run: func(item-id: string, action-id: string) -> run-outcome;
  }
  ```
- **Packaging:** each plugin is a folder with `plugin.toml` (id, name, commands, **permissions**) and `plugin.wasm`.
- **Sandbox:** no filesystem or network access unless the plugin declares the permission. Fuel and epoch limits stop runaway plugins.
- **Stateful UI** (forms, live lists) uses the same `View` enum: `run` returns a new `View`, so no reconciler is needed.

---

## 6. Windows platform layer (`platform-win`)

| Need | API |
|---|---|
| Global hotkey | `RegisterHotKey`, changeable at run time. The settings recorder installs a `WH_KEYBOARD_LL` hook while recording, so it sees combinations other apps own and can say they're taken. Later the same hook for Win-key combos and snippet expansion |
| Reliable focus when showing | `SetForegroundWindow` right after `WM_HOTKEY`; `AttachThreadInput` fallback |
| Hide on blur | GPUI window activation events |
| Rounded corners | `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE)` |
| Background | GPUI `Blurred` + a dense tint. GPUI's accent-policy blur barely blurs on Windows 11; the DWM acrylic system backdrop was tried and rejected (light grey, unstable focus) |
| Apps | `shell:AppsFolder` enumeration (Win32 + MSIX in one list); launch via `ShellExecuteW("shell:AppsFolder\<id>")` |
| Icons | `IShellItemImageFactory::GetImage`, cached as PNG on disk |
| Clipboard history | `AddClipboardFormatListener` |
| Paste into the previous app | Restore focus, then `SendInput` Ctrl+V |
| Selected text | UIA `TextPattern`, falling back to a simulated Ctrl+C |
| Window management | `SetWindowPos`, `MonitorFromWindow`, `GetMonitorInfoW`, `SetWinEventHook` |
| Single instance + deeplinks | Named mutex + named pipe; `spotlight://` registered under `HKCU\Software\Classes` |
| Login item | `HKCU\...\Run` |

GPUI gotcha: with `WindowOptions { show: false }`, GPUI applies the window size only in its own `activate()`. If the window is shown through Win32, set the size yourself, DPI-aware; otherwise it stays `CW_USEDEFAULT` (nearly full screen).

---

## 7. Key crates

`gpui` (pinned, no `gpui-component`) · `windows` · `tokio` · `nucleo` · `rusqlite` · `fend-core` · `serde`/`serde_json` · `notify` · `tray-icon` · `pulldown-cmark` (markdown in Detail) · `image` · `tracing` · `directories` · later: `wasmtime`, `tantivy`.

---

## 8. Roadmap

0. ✅ **Spike**: hotkey, prewarmed window, fuzzy app search, launch, measurements. See [ui-framework-decision.md](ui-framework-decision.md).
1. ✅ Workspace (`app`, `ui`, `core`, `search`, `store`, `platform-win`), SQLite store, frecency ranking, app icons, tray, single instance, start at login.
   Settings window with a hotkey recorder (conflicts reported) and start at login.
   Still to do: the `Provider` trait and `providers/` (apps, system, calc, web); searchable settings; icon disk cache.
2. Action panel (Ctrl+K), aliases, per-item hotkeys, frecency tuning.
3. Clipboard history, snippets (with expansion), quicklinks.
4. Window management, file search.
5. Script commands.
6. WASM plugin host + SDK + 2–3 sample plugins.
7. Packaging (MSIX or Velopack), auto-update, code signing.
