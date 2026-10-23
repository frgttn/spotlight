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
  store          # rusqlite + SQLCipher: settings, frecency, clipboard history, snippets, quicklinks
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

**As built** (`core::view`): `View::{List, Grid, Detail, Form}`. Actions open them with
`Command::PushView(view)`; the launcher keeps a stack of them over root search, and Esc pops one.
List and Grid filter their items with the search field; Detail renders markdown
(`pulldown-cmark`) with a metadata column; a Form's submit command has `{field}` placeholders
that `FormView::fill` replaces (dropdown values first and raw, so one can pick a URL template;
text values percent-encoded inside URLs). serde derives come with script commands, the first
outside source of views.

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
| Clipboard history | `AddClipboardFormatListener` on a message-only window of its own thread; reads 100 ms after a change (apps write in steps); skips copies marked `ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer Ignore` or `CanIncludeInClipboardHistory = 0` |
| Paste into the previous app | Foreground window remembered when the launcher opens; `SetForegroundWindow` while still foreground, then `SendInput` Ctrl+V once it has focus (blocked by UIPI in elevated apps) |
| Win+V | Can't be registered (Windows owns it): a `WH_KEYBOARD_LL` hook on the hotkey thread swallows the V, then taps an unused key so releasing Win doesn't open Start |
| Selected text | Read only when a command with `{selection}` runs: the launcher hands the foreground back to the app it came from, then UIA (`GetFocusedElement` → `TextPattern.GetSelection`, same process only). Otherwise, unless turned off or the window is a terminal, simulated Ctrl+C: the clipboard is copied aside format by format, the selection read, the clipboard put back, and the history skips those changes (by clipboard sequence number) |
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
   Still to do: searchable settings; icon disk cache.
1.5 ✅ Root search core, part 1: `Provider` trait, `SearchItem`/`Action`/`Command` in `core`, `RootSearch` in `search` (answers, then ranked catalog, then fallbacks), providers for apps, system commands, calculator (`fend-core`) and web/URLs.
   The providers live in one `providers` crate (modules, not one crate each) until one needs heavy dependencies of its own.
   `Provider` is synchronous for now: catalogs load on background threads and per-keystroke answers are bounded (the calculator gives up after 30 ms). Async with cancellation comes with providers that do IO (files, plugins).
   Actions are data (`Command`: launch app, open URL, copy, system command) that the app runs, rather than a `Provider::run` callback, so scripts and plugins can only ask for this safe set.
   Each provider can be turned off in Settings (`RootSearch::set_enabled`): it then has no catalog loaded and is skipped for answers and fallbacks. `Provider::name` / `description` label its switch.
2. Root search core, part 2: ✅ action panel (Ctrl+K), ✅ view stack (List / Grid / Detail / Form, Esc goes back), ✅ HUD and toasts, ✅ aliases and per-item hotkeys, ✅ calculator currencies and history; ✅ more sources (Settings pages, Control Panel, Steam / Epic / GOG / Ubisoft games); still to do: frecency tuning.
3. ✅ Quicklinks (with search bangs); ✅ clipboard history: text, links, colours, files and images (PNG files plus a 640 px copy for the preview), pinning, filters (Tab), retention by age and count, ignored apps, paste into the previous app, Win+V as an option. Lists can show a preview of the selected row (`SearchItem::preview`) and a filter (`ListView::filters`). Still to do: snippets (with expansion).
   ✅ Selected text: `{selection}` in `OpenUrl` URLs and quicklinks (`Command::needs_selection` / `with_selection`), the Selected Text source (Search Selection on the Web, Translate Selection); turning the source off means the selection is never read, and Settings can forbid the Ctrl+C fallback.
3.5 ✅ Database encryption: `rusqlite` with `bundled-sqlcipher`. SQLCipher's crypto is Windows CNG (`vendor/libsqlite3-sys/sqlcipher/crypto_cng.c`, a patched `libsqlite3-sys`), so building needs no OpenSSL or Perl; the format is standard SQLCipher 4. The key is 32 random bytes in Credential Manager (generic credential `Spotlight/database`, as Raycast keeps its `main.db` key), used raw (`x'…'`), so nothing to type. A plain database is converted once with `sqlcipher_export`. Clipboard images are sealed with AES-256-GCM (CNG) under a key derived from the database key (`platform-win::vault`); older plain images are sealed at startup. A database the saved key can't read is renamed to `spotlight.unreadable-<time>.db` and a new one starts.
4. Window management, file search.
5. Script commands.
6. WASM plugin host + SDK + 2–3 sample plugins.
7. Packaging (MSIX or Velopack), auto-update, code signing.
