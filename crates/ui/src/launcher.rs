//! The launcher view: search field, result list, metrics footer, and the
//! views opened on top of it (see `views`).

mod clipboard;
mod quicklinks;
mod snippets;
mod views;

use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, EntityInputHandler, FocusHandle, Focusable,
    Hsla, KeyBinding, RenderImage, SharedString, Subscription, Task, UniformListScrollHandle,
    Window, actions, canvas, div, hsla, img, prelude::*, px, rgb, rgba, uniform_list,
};
use image::{Frame, RgbaImage};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_core::{
    frecency::{self, Frecency},
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    view::{ListView, View},
};
use spotlight_platform_win::{
    clipboard as system_clipboard, commands, expander,
    hotkey::{self, Hotkey, Recorded, SetError},
    icons,
    metrics::{self, PaintProbe, Percentiles, Probe, Summary, ms},
    selection, win,
};
use spotlight_search::{Candidate, RootSearch, Row, Searcher};
use spotlight_store::{Calculation, Store};

use self::{
    clipboard::{PREVIEW_LIST_WIDTH, PreviewImage},
    views::{CHOICE_CONTEXT, GRID_COLUMNS, Screen},
};
use crate::{
    smooth_scroll::{SmoothScroll, capture_wheel},
    text_input::{self, TextInput},
    theme,
};

actions!(
    launcher,
    [
        SelectNext,
        SelectPrev,
        Confirm,
        SecondaryAction,
        ToggleActions,
        FocusNextField,
        FocusPrevField,
        NextChoice,
        PrevChoice,
        ToggleChoice,
        Dismiss,
        Quit,
        OpenSettings
    ]
);

const CONTEXT: &str = "Launcher";
const ROW_HEIGHT: f32 = 44.;
/// Space above the first and below the last row.
const LIST_PADDING: f32 = 4.;
/// Row icon size in logical pixels.
const ICON_SIZE: f32 = 26.;
/// Icons are extracted at the grid's tile size, so tiles are sharp too.
const ICON_EXTRACT_SIZE: f32 = 44.;
/// Launcher window size in logical (96-DPI) pixels.
pub const WINDOW_SIZE: (f32, f32) = (680., 420.);
/// The apps provider's id; its catalog load time is the "apps" figure in the
/// footer and the self-test summary.
const APPS_PROVIDER: &str = "apps";
/// How long a footer toast stays.
const TOAST_DURATION: Duration = Duration::from_millis(2500);
/// HUD size in logical pixels, and how long it stays.
const HUD_SIZE: (f32, f32) = (240., 48.);
const HUD_DURATION: Duration = Duration::from_millis(1500);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("ctrl-p", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(CONTEXT)),
        KeyBinding::new("ctrl-enter", SecondaryAction, Some(CONTEXT)),
        KeyBinding::new("ctrl-k", ToggleActions, Some(CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        KeyBinding::new("tab", FocusNextField, Some(CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevField, Some(CONTEXT)),
        // Only on a focused checkbox or dropdown, so Space still types elsewhere.
        KeyBinding::new("right", NextChoice, Some(CHOICE_CONTEXT)),
        KeyBinding::new("left", PrevChoice, Some(CHOICE_CONTEXT)),
        KeyBinding::new("space", ToggleChoice, Some(CHOICE_CONTEXT)),
        KeyBinding::new("ctrl-q", Quit, None),
        // Not handled here: it bubbles up to the app, which owns the settings window.
        KeyBinding::new("ctrl-,", OpenSettings, Some(CONTEXT)),
    ]);
}

/// Filler rows for the 10k-item list test (`SPOTLIGHT_SYNTHETIC`).
struct Synthetic(usize);

impl Provider for Synthetic {
    fn id(&self) -> &'static str {
        "synthetic"
    }

    fn name(&self) -> &'static str {
        "Synthetic Rows"
    }

    fn catalog(&self) -> Vec<SearchItem> {
        (0..self.0)
            .map(|ix| {
                SearchItem::new(
                    item_id("synthetic", &ix.to_string()),
                    format!("Synthetic item {ix:05} {}", FILLER[ix % FILLER.len()]),
                )
                .subtitle("Synthetic")
            })
            .collect()
    }
}

/// One line in the action panel: an action of the item, or one the launcher
/// adds to every catalog item (ranking, alias, hotkey).
#[derive(Clone)]
struct Entry {
    title: String,
    kind: EntryKind,
}

#[derive(Clone)]
enum EntryKind {
    Command(Command),
    ResetRanking,
    EditAlias,
    RemoveAlias,
    RecordHotkey,
    RemoveHotkey,
    /// A calculator history entry, by its id in the store.
    DeleteCalculation(i64),
    ClearCalculations,
}

impl Entry {
    fn needs_confirmation(&self) -> bool {
        match &self.kind {
            EntryKind::Command(command) => command.needs_confirmation(),
            EntryKind::ClearCalculations => true,
            _ => false,
        }
    }
}

/// The calculator's answer row; using it adds the calculation to the history.
const CALC_ANSWER_ID: &str = "calc:answer";
/// Item ids of history rows: this, then the calculation's id in the store.
const HISTORY_ITEM: &str = "calc-history:";
/// The Selected Text provider: turned off, the selection is never read.
const SELECTION_PROVIDER: &str = "selection";
const SEARCH_PLACEHOLDER: &str = "Search apps and commands, or calculate…";

/// An item with an alias or a hotkey, as the settings window lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shortcut {
    pub item_id: String,
    pub title: String,
    pub alias: Option<String>,
    pub hotkey: Option<Hotkey>,
}

/// An item that can take an alias or a hotkey, as its extension's page in
/// the settings window lists it.
#[derive(Clone)]
pub struct ExtensionItem {
    /// The id of the search provider it comes from.
    pub provider: String,
    pub shortcut: Shortcut,
    pub subtitle: Option<String>,
    /// Its icon once extracted, or the glyph it is drawn with.
    pub image: Option<Arc<RenderImage>>,
    pub glyph: Option<char>,
}

/// A search provider, as the settings window lists it to turn it on or off.
#[derive(Clone)]
pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub enabled: bool,
}

/// A small card over the window for setting an item's alias or hotkey.
enum Editor {
    Alias {
        item_id: String,
        title: String,
        input: Entity<TextInput>,
    },
    Hotkey {
        item_id: String,
        title: String,
        /// Why the last combination wasn't taken.
        message: Option<SharedString>,
        /// Receives what the recorder hook sees.
        _recording: Task<()>,
    },
}

/// Measurements, shown in the footer and logged to stderr.
#[derive(Default)]
struct Stats {
    hotkey: SharedString,
    apps: usize,
    discover_ms: Option<f64>,
    shows: u32,
    filter_ms: Option<f64>,
    filter_samples: Vec<f64>,
    /// Painted by a `canvas` placed last in the tree.
    paint: Rc<RefCell<PaintProbe>>,
}

pub struct Launcher {
    hwnd: isize,
    input: Entity<TextInput>,
    search: RootSearch,
    /// Catalogs still loading at startup.
    loading: usize,
    results: Vec<Row>,
    selected: usize,
    /// The action panel (Ctrl+K) is open, with this action selected.
    panel: Option<usize>,
    /// Set by a first Enter on an action that asks for confirmation (restart,
    /// shut down): that item's id and the action's index. Moving the selection
    /// or typing clears it.
    confirming: Option<(String, usize)>,
    /// A short message in the footer, for actions that keep the window open.
    toast: Option<SharedString>,
    /// Bumped on every toast, so an older toast's timer doesn't clear a newer one.
    toast_generation: usize,
    /// While set, the (otherwise hidden) window is the HUD: a small message at
    /// the bottom of the screen, shown without focus. A second window would
    /// cost ~30 MB of GPU state for something on screen 1.5 s at a time.
    hud: Option<SharedString>,
    hud_generation: usize,
    scroll: SmoothScroll,
    /// Extracted shell icons by item id. Items without one show a badge.
    icons: HashMap<String, Arc<RenderImage>>,
    /// In-memory copy of the store's frecency table, updated on every launch.
    frecency: HashMap<String, Frecency>,
    /// `None` when the database couldn't be opened; usage is then not remembered.
    store: Option<Arc<Mutex<Store>>>,
    /// Item id → the user's alias for it.
    aliases: HashMap<String, String>,
    /// Item id → its registered hotkey.
    item_hotkeys: HashMap<String, Hotkey>,
    hotkeys: hotkey::Service,
    editor: Option<Editor>,
    /// Views opened from root search, the top one showing; empty in root search.
    screens: Vec<Screen>,
    /// Holds the keyboard in views without a search field (details, forms).
    focus: FocusHandle,
    grid_scroll: UniformListScrollHandle,
    /// Ranks the rows of a list opened from root search.
    list_searcher: Searcher,
    visible: bool,
    /// The window that was active when the launcher opened, to paste into
    /// and read the selected text from.
    previous_window: isize,
    /// Whether reading the selected text may fall back to Ctrl+C.
    selection_copy: bool,
    /// Expands snippet keywords typed in other apps; `None` until started.
    expander: Option<expander::Service>,
    /// Whether keywords expand, as set in Settings (Snippets must be on too).
    snippet_expansion: bool,
    /// The clipboard preview's image.
    preview_image: Option<PreviewImage>,
    /// Images no longer shown, freed on the next frame (it takes the window).
    stale_images: Vec<Arc<RenderImage>>,
    stats: Stats,
    _subscriptions: Vec<Subscription>,
}

impl Launcher {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hotkey: SharedString,
        hotkeys: hotkey::Service,
        mut providers: Vec<Arc<dyn Provider>>,
        disabled_providers: &[String],
        synthetic: usize,
        store: Option<Arc<Mutex<Store>>>,
        smooth_scrolling: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hwnd = match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
            other => panic!("expected a Win32 window handle, got {other:?}"),
        };
        win::round_corners(hwnd);

        let input = cx.new(|cx| TextInput::new(SEARCH_PLACEHOLDER, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, _, _: &text_input::Changed, window, cx| this.query_changed(window, cx),
            ),
            cx.observe_window_activation(window, |this, window, cx| {
                if this.visible && !window.is_window_active() {
                    this.hide(cx);
                }
            }),
        ];

        if synthetic > 0 {
            providers.push(Arc::new(Synthetic(synthetic)));
        }

        let frecency = store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|store| {
                store.frecency().unwrap_or_else(|err| {
                    eprintln!("[spotlight] could not read frecency: {err}");
                    HashMap::new()
                })
            })
            .unwrap_or_default();
        let (aliases, saved_hotkeys) = store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|store| {
                let aliases = store.aliases().unwrap_or_else(|err| {
                    eprintln!("[spotlight] could not read aliases: {err}");
                    HashMap::new()
                });
                let hotkeys = store.item_hotkeys().unwrap_or_else(|err| {
                    eprintln!("[spotlight] could not read item hotkeys: {err}");
                    HashMap::new()
                });
                (aliases, hotkeys)
            })
            .unwrap_or_default();
        // Registered now, so they work before the window is ever shown. One
        // another app has taken since stays saved but inactive for this run.
        let item_hotkeys = saved_hotkeys
            .into_iter()
            .filter_map(|(id, text)| {
                let hotkey = Hotkey::parse(&text)?;
                match hotkeys.set_item(&id, Some(hotkey)) {
                    Ok(()) => Some((id, hotkey)),
                    Err(err) => {
                        eprintln!("[spotlight] hotkey {hotkey} for {id:?}: {err}");
                        None
                    }
                }
            })
            .collect();

        let mut search = RootSearch::new(providers);
        for ix in 0..search.providers().len() {
            let id = search.providers()[ix].id();
            if disabled_providers.iter().any(|disabled| disabled == id) {
                search.set_enabled(ix, false);
            }
        }

        let mut this = Self {
            hwnd,
            input,
            loading: 0,
            search,
            results: Vec::new(),
            selected: 0,
            panel: None,
            confirming: None,
            toast: None,
            toast_generation: 0,
            hud: None,
            hud_generation: 0,
            scroll: {
                let mut scroll =
                    SmoothScroll::new(UniformListScrollHandle::new(), ROW_HEIGHT, LIST_PADDING);
                scroll.set_enabled(smooth_scrolling);
                scroll
            },
            icons: HashMap::new(),
            frecency,
            store,
            aliases,
            item_hotkeys,
            hotkeys,
            editor: None,
            screens: Vec::new(),
            focus: cx.focus_handle(),
            grid_scroll: UniformListScrollHandle::new(),
            list_searcher: Searcher::default(),
            visible: false,
            previous_window: 0,
            selection_copy: true,
            expander: None,
            snippet_expansion: true,
            preview_image: None,
            stale_images: Vec::new(),
            stats: Stats {
                hotkey,
                ..Default::default()
            },
            _subscriptions: subscriptions,
        };
        this.refilter("");
        this.load_catalogs(window.scale_factor(), cx);
        this
    }

    /// Loads every turned-on provider's catalog on its own thread (app
    /// discovery takes ~200 ms of COM calls) and merges each one into the
    /// search as it arrives.
    fn load_catalogs(&mut self, window_scale: f32, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::unbounded();
        for (ix, provider) in self.search.providers().iter().enumerate() {
            if !self.search.is_enabled(ix) {
                continue;
            }
            self.loading += 1;
            let provider = provider.clone();
            let tx = tx.clone();
            let spawned = thread::Builder::new()
                .name(format!("catalog-{}", provider.id()))
                .spawn(move || {
                    let started = Instant::now();
                    let items = provider.catalog();
                    let _ = tx.send_blocking((ix, items, started.elapsed()));
                });
            if let Err(err) = spawned {
                eprintln!("[spotlight] could not start a catalog thread: {err}");
                self.loading -= 1;
            }
        }

        cx.spawn(async move |this, cx| {
            while let Ok((ix, items, elapsed)) = rx.recv().await {
                let updated = this.update(cx, |this, cx| {
                    this.loading = this.loading.saturating_sub(1);
                    this.catalog_loaded(ix, items, elapsed, window_scale, cx)
                });
                if updated.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Loads one provider's catalog again, after the user changed what it
    /// lists (a quicklink saved or deleted) or turned it on.
    fn reload_catalog(&mut self, id: &str, window_scale: f32, cx: &mut Context<Self>) {
        let Some(ix) = self
            .search
            .providers()
            .iter()
            .position(|provider| provider.id() == id)
        else {
            return;
        };
        let provider = self.search.providers()[ix].clone();
        let load = cx.background_executor().spawn(async move {
            let started = Instant::now();
            let items = provider.catalog();
            (items, started.elapsed())
        });
        cx.spawn(async move |this, cx| {
            let (items, elapsed) = load.await;
            this.update(cx, |this, cx| {
                this.catalog_loaded(ix, items, elapsed, window_scale, cx)
            })
            .ok();
        })
        .detach();
    }

    fn catalog_loaded(
        &mut self,
        provider: usize,
        items: Vec<SearchItem>,
        elapsed: Duration,
        window_scale: f32,
        cx: &mut Context<Self>,
    ) {
        let id = self.search.providers()[provider].id();
        if !self.search.is_enabled(provider) {
            eprintln!("[spotlight] {id}: turned off while loading; catalog dropped");
            return;
        }
        eprintln!(
            "[spotlight] {id}: {} items in {:.1} ms",
            items.len(),
            ms(elapsed)
        );
        if id == APPS_PROVIDER {
            self.stats.apps = items.len();
            self.stats.discover_ms = Some(ms(elapsed));
        }
        let shell_icons: Vec<(String, String)> = items
            .iter()
            .filter_map(|item| Some((item.id.clone(), shell_name(&item.icon)?)))
            .collect();

        self.search.set_catalog(provider, items);
        // Catalog indices moved, so the rows are rebuilt (and the selection resets).
        let query = self.query(cx);
        self.refilter(&query);
        self.load_icons(shell_icons, window_scale, cx);
        cx.notify();
    }

    /// Extracts shell icons in the background; rows switch from badges to icons
    /// as they arrive. `items` holds (item id, shell parsing name) pairs; each
    /// distinct icon is extracted once (every Settings page has the same one).
    fn load_icons(
        &mut self,
        items: Vec<(String, String)>,
        window_scale: f32,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let mut names: Vec<String> = Vec::new();
        let mut ids: Vec<Vec<String>> = Vec::new();
        for (id, name) in items {
            match names.iter().position(|known| *known == name) {
                Some(ix) => ids[ix].push(id),
                None => {
                    names.push(name);
                    ids.push(vec![id]);
                }
            }
        }
        // Sized for the monitor the window was created on; GPUI scales it if that changes.
        let size = (ICON_EXTRACT_SIZE * window_scale).ceil() as u32;
        let (tx, rx) = async_channel::unbounded();
        icons::spawn(names, size, tx);

        cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                // Apply whatever else has arrived too, so the list re-renders once per batch.
                let batch: Vec<_> = std::iter::once(first)
                    .chain(std::iter::from_fn(|| rx.try_recv().ok()))
                    .collect();
                let updated = this.update(cx, |this, cx| {
                    for (ix, icon) in batch {
                        if let Some(image) = render_image(icon) {
                            for id in &ids[ix] {
                                this.icons.insert(id.clone(), image.clone());
                            }
                        }
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Every search provider, in search order, for the settings window.
    pub fn sources(&self) -> Vec<Source> {
        self.search
            .providers()
            .iter()
            .enumerate()
            .map(|(ix, provider)| Source {
                id: provider.id(),
                name: provider.name(),
                description: provider.description(),
                enabled: self.search.is_enabled(ix),
            })
            .collect()
    }

    /// The ids of the providers that are turned off, for saving.
    pub fn disabled_providers(&self) -> Vec<&'static str> {
        self.sources()
            .into_iter()
            .filter(|source| !source.enabled)
            .map(|source| source.id)
            .collect()
    }

    /// Turns provider `id` on (loading its catalog again) or off (dropping it).
    pub fn set_provider_enabled(
        &mut self,
        id: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .search
            .providers()
            .iter()
            .position(|provider| provider.id() == id)
        else {
            return;
        };
        if self.search.is_enabled(ix) == enabled {
            return;
        }
        self.search.set_enabled(ix, enabled);
        if enabled {
            self.reload_catalog(id, window.scale_factor(), cx);
        }
        if id == snippets::PROVIDER {
            self.sync_expander();
        }
        // Catalog indices moved (or answers changed), so the rows are rebuilt.
        let query = self.query(cx);
        self.refilter(&query);
        cx.notify();
    }

    pub fn set_smooth_scrolling(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.scroll.set_enabled(enabled);
        cx.notify();
    }

    /// Shown in the footer.
    pub fn set_hotkey_label(&mut self, label: SharedString, cx: &mut Context<Self>) {
        self.stats.hotkey = label;
        cx.notify();
    }

    pub fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().to_owned()
    }

    /// Re-runs the search (root search, or the open list's) and resets the
    /// selection. Returns the search time in ms.
    fn refilter(&mut self, query: &str) -> f64 {
        let started = Instant::now();
        let now = frecency::now();
        let Self {
            screens,
            list_searcher,
            search,
            frecency,
            aliases,
            ..
        } = self;
        self.results = match screens.last() {
            None => search.search(
                query,
                |id| frecency.get(id).map_or(0., |used| used.score_at(now)),
                aliases,
            ),
            Some(Screen::List { view, filter, .. }) => {
                let shown: Vec<&SearchItem> = match view.filters.get(*filter) {
                    Some(filter) => view
                        .items
                        .iter()
                        .filter(|item| filter.keeps(item))
                        .collect(),
                    None => view.items.iter().collect(),
                };
                let candidates = shown.iter().map(|item| Candidate {
                    title: &item.title,
                    keywords: &item.keywords,
                    alias: None,
                    frecency: 0.,
                });
                list_searcher
                    .filter(query, candidates)
                    .into_iter()
                    .map(|ix| Row::Computed(shown[ix].clone()))
                    .collect()
            }
            Some(_) => Vec::new(),
        };
        self.selected = 0;
        self.panel = None;
        self.confirming = None;
        self.scroll.reset();
        ms(started.elapsed())
    }

    /// The item's actions, then the ones the launcher adds to catalog items.
    /// Computed rows (calculator, web) change with the query, so they can't
    /// keep an alias, a hotkey or a ranking.
    fn actions_for(&self, row: &Row) -> Vec<Entry> {
        let item = self.item(row);
        let mut entries: Vec<Entry> = item
            .actions
            .iter()
            .map(|action| Entry {
                title: action.title.clone(),
                kind: EntryKind::Command(action.command.clone()),
            })
            .collect();
        if let Some(id) = item
            .id
            .strip_prefix(HISTORY_ITEM)
            .and_then(|id| id.parse().ok())
        {
            entries.push(Entry {
                title: "Delete Entry".into(),
                kind: EntryKind::DeleteCalculation(id),
            });
            entries.push(Entry {
                title: "Clear History".into(),
                kind: EntryKind::ClearCalculations,
            });
            return entries;
        }
        if !matches!(row, Row::Catalog(_)) || item.actions.is_empty() {
            return entries;
        }
        let mut add = |title: &str, kind| {
            entries.push(Entry {
                title: title.to_owned(),
                kind,
            })
        };
        if self.aliases.contains_key(&item.id) {
            add("Change Alias…", EntryKind::EditAlias);
            add("Remove Alias", EntryKind::RemoveAlias);
        } else {
            add("Set Alias…", EntryKind::EditAlias);
        }
        if self.item_hotkeys.contains_key(&item.id) {
            add("Change Hotkey…", EntryKind::RecordHotkey);
            add("Remove Hotkey", EntryKind::RemoveHotkey);
        } else {
            add("Set Hotkey…", EntryKind::RecordHotkey);
        }
        if self.frecency.contains_key(&item.id) {
            add("Reset Ranking", EntryKind::ResetRanking);
        }
        entries
    }

    fn item<'a>(&'a self, row: &'a Row) -> &'a SearchItem {
        match row {
            Row::Catalog(ix) => &self.search.catalog()[*ix],
            Row::Computed(item) => item,
        }
    }

    fn query_changed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let changed_at = Instant::now();
        let query = self.query(cx);
        let filter_ms = self.refilter(&query);
        self.stats.filter_ms = Some(filter_ms);
        self.stats.filter_samples.push(filter_ms);
        self.stats.paint.borrow_mut().arm(changed_at, Probe::Key);
        cx.notify();
    }

    /// Self-test: types text through the same IME entry point the platform uses.
    /// Types into the field that has the keyboard: the alias editor's while
    /// it's open, or a form's focused text field.
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = match &self.editor {
            Some(Editor::Alias { input, .. }) => input.clone(),
            _ => self
                .focused_form_input(window, cx)
                .unwrap_or_else(|| self.input.clone()),
        };
        input.update(cx, |input, cx| {
            input.replace_text_in_range(None, text, window, cx)
        });
    }

    /// The action selected in the open panel. For the self-test.
    pub fn panel_selected(&self) -> Option<String> {
        let selected = self.panel?;
        self.selected_actions()
            .into_iter()
            .nth(selected)
            .map(|entry| entry.title)
    }

    /// Whether the alias editor is open. For the self-test.
    pub fn editing_alias(&self) -> bool {
        matches!(self.editor, Some(Editor::Alias { .. }))
    }

    /// Item id → alias. For the self-test.
    pub fn aliases(&self) -> &HashMap<String, String> {
        &self.aliases
    }

    /// Every catalog, apps included, has loaded.
    pub fn apps_loaded(&self) -> bool {
        self.loading == 0
    }

    /// Action titles in the open action panel, or `None` when it's closed.
    /// For the self-test.
    pub fn panel_actions(&self) -> Option<Vec<String>> {
        self.panel?;
        Some(
            self.selected_actions()
                .into_iter()
                .map(|action| action.title)
                .collect(),
        )
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Titles of the current results, top first. For the self-test.
    pub fn result_titles(&self) -> Vec<String> {
        self.results
            .iter()
            .map(|row| self.item(row).title.clone())
            .collect()
    }

    pub fn summary(&self) -> Summary {
        let paint = self.stats.paint.borrow();
        Summary {
            apps: self.stats.apps,
            items: self.search.catalog().len(),
            discover_ms: self.stats.discover_ms,
            first_show_ms: paint.first_show_ms,
            show: Percentiles::of(&paint.show_samples),
            filter: Percentiles::of(&self.stats.filter_samples),
            key_to_paint: Percentiles::of(&paint.key_samples),
        }
    }

    /// Hotkey handler. `pressed_at` is when the hotkey thread dequeued `WM_HOTKEY`.
    pub fn toggle(&mut self, pressed_at: Instant, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible && win::is_visible(self.hwnd) {
            self.hide(cx);
        } else {
            self.show(pressed_at, window, cx);
        }
    }

    /// The window that was in front before the launcher, which pastes go
    /// to. For the self-test.
    pub fn previous_window(&self) -> isize {
        self.previous_window
    }

    pub fn show(&mut self, pressed_at: Instant, window: &mut Window, cx: &mut Context<Self>) {
        let foreground = system_clipboard::foreground_window();
        if foreground != self.hwnd {
            self.previous_window = foreground;
        }
        self.hud = None;
        if !self.screens.is_empty() {
            self.screens.clear();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(SEARCH_PLACEHOLDER, cx);
                input.set_pass_horizontal_arrows(false);
            });
        }
        self.input.update(cx, |input, cx| input.clear(cx));
        self.stats.filter_ms = Some(self.refilter(""));
        // Explicit size: with `show: false` GPUI defers its own placement to
        // `activate()`, which we bypass, so the window would keep CW_USEDEFAULT.
        win::show_on_active_monitor(self.hwnd, Some(WINDOW_SIZE));
        self.visible = true;
        window.focus(&self.input.focus_handle(cx));

        let probe = if self.stats.shows == 0 {
            Probe::FirstShow
        } else {
            Probe::Show
        };
        self.stats.shows += 1;
        self.stats.paint.borrow_mut().arm(pressed_at, probe);
        cx.notify();
    }

    /// Shows the window unless it is already up (tray click, second launch).
    /// Unlike [`Self::toggle`], it never hides.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible && win::is_visible(self.hwnd) {
            win::focus(self.hwnd);
        } else {
            self.show(Instant::now(), window, cx);
        }
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.close_editor(None, cx);
        self.forget_preview_image();
        win::hide(self.hwnd);
        self.visible = false;
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(action) = self.panel {
            let count = self.selected_actions().len();
            self.panel = Some((action + 1).min(count.saturating_sub(1)));
            cx.notify();
            return;
        }
        if self.in_grid() {
            return self.grid_move(GRID_COLUMNS as isize, cx);
        }
        if matches!(self.screens.last(), Some(Screen::Detail { .. })) {
            return self.detail_scroll(true, cx);
        }
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
            self.confirming = None;
            self.scroll.reveal(self.selected, self.results.len());
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(action) = self.panel {
            self.panel = Some(action.saturating_sub(1));
            cx.notify();
            return;
        }
        if self.in_grid() {
            return self.grid_move(-(GRID_COLUMNS as isize), cx);
        }
        if matches!(self.screens.last(), Some(Screen::Detail { .. })) {
            return self.detail_scroll(false, cx);
        }
        if self.selected > 0 {
            self.selected -= 1;
            self.confirming = None;
            self.scroll.reveal(self.selected, self.results.len());
            cx.notify();
        }
    }

    /// The actions Enter, Ctrl+Enter and the panel offer: the selected row's,
    /// or a detail view's own.
    fn selected_actions(&self) -> Vec<Entry> {
        if let Some(Screen::Detail { view, .. }) = self.screens.last() {
            return view
                .actions
                .iter()
                .map(|action| Entry {
                    title: action.title.clone(),
                    kind: EntryKind::Command(action.command.clone()),
                })
                .collect();
        }
        self.results
            .get(self.selected)
            .map(|row| self.actions_for(row))
            .unwrap_or_default()
    }

    // ←/→ that the search field passes on in a grid.
    fn grid_left(&mut self, _: &text_input::Left, _: &mut Window, cx: &mut Context<Self>) {
        self.grid_move(-1, cx);
    }

    fn grid_right(&mut self, _: &text_input::Right, _: &mut Window, cx: &mut Context<Self>) {
        self.grid_move(1, cx);
    }

    /// Tab: the next form field, or the next choice of a list's filter.
    fn focus_next_field(
        &mut self,
        _: &FocusNextField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.step_filter(true, cx) {
            self.focus_field(true, window, cx);
        }
    }

    fn focus_prev_field(
        &mut self,
        _: &FocusPrevField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.step_filter(false, cx) {
            self.focus_field(false, window, cx);
        }
    }

    fn next_choice(&mut self, _: &NextChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.change_choice(1, window, cx);
    }

    fn prev_choice(&mut self, _: &PrevChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.change_choice(-1, window, cx);
    }

    fn toggle_choice(&mut self, _: &ToggleChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.change_choice(1, window, cx);
    }

    /// Enter: saves the alias being edited, or runs the action picked in the
    /// panel, the one waiting for a second Enter, or the primary action.
    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        match &self.editor {
            Some(Editor::Alias { .. }) => return self.save_alias(window, cx),
            // The recorder hook takes every key; Enter can't arrive, but just in case.
            Some(Editor::Hotkey { .. }) => return,
            None => {}
        }
        match self.screens.last() {
            Some(Screen::Form(_)) if self.panel.is_none() => return self.submit_form(window, cx),
            Some(Screen::Detail { .. }) => {
                let action = self.panel.unwrap_or(0);
                return self.run_action(action, window, cx);
            }
            _ => {}
        }
        let waiting = self
            .confirming
            .as_ref()
            .filter(|(id, _)| {
                self.results
                    .get(self.selected)
                    .is_some_and(|row| &self.item(row).id == id)
            })
            .map(|&(_, action)| action);
        let action = self.panel.or(waiting).unwrap_or(0);
        if self.selected_actions().is_empty() {
            self.hide(cx);
        } else {
            self.run_action(action, window, cx);
        }
    }

    /// Ctrl+Enter: the second action (Run as Administrator, Copy Calculation…),
    /// or a form's submit.
    fn secondary_action(
        &mut self,
        _: &SecondaryAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.panel.is_some() || self.editor.is_some() {
            return;
        }
        // A form's submit: Enter can't be it in a text area, where it starts
        // a new line.
        if matches!(self.screens.last(), Some(Screen::Form(_))) {
            return self.submit_form(window, cx);
        }
        self.run_action(1, window, cx);
    }

    fn toggle_actions(&mut self, _: &ToggleActions, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        self.panel = match self.panel {
            Some(_) => None,
            None if !self.selected_actions().is_empty() => Some(0),
            None => None,
        };
        self.confirming = None;
        cx.notify();
    }

    /// Runs action `ix` of the selected item. Commands then hide the window;
    /// the launcher's own entries (alias, hotkey, ranking) keep it open.
    fn run_action(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // A detail view's actions belong to the view, not to a row.
        if let Some(Screen::Detail { view, .. }) = self.screens.last() {
            let Some(action) = view.actions.get(ix).cloned() else {
                return;
            };
            let key = (view.title.clone(), ix);
            self.panel = None;
            if action.command.needs_confirmation() && self.confirming.as_ref() != Some(&key) {
                self.confirming = Some(key);
                cx.notify();
                return;
            }
            self.confirming = None;
            return self.perform(&action.command, window, cx);
        }
        let Some(row) = self.results.get(self.selected).cloned() else {
            return;
        };
        let Some(entry) = self.actions_for(&row).into_iter().nth(ix) else {
            return;
        };
        let item = self.item(&row);
        let (id, title) = (item.id.clone(), item.title.clone());
        self.panel = None;
        if entry.needs_confirmation() && self.confirming.as_ref() != Some(&(id.clone(), ix)) {
            self.confirming = Some((id, ix));
            cx.notify();
            return;
        }
        self.confirming = None;

        let command = match entry.kind {
            EntryKind::Command(command) => command,
            EntryKind::ResetRanking => {
                // The point is to see the item drop back.
                self.reset_ranking(&id, cx);
                self.show_toast(format!("Ranking reset for {title}").into(), cx);
                return;
            }
            EntryKind::EditAlias => return self.open_alias_editor(id, title, window, cx),
            EntryKind::RemoveAlias => {
                self.set_alias(&id, None, cx);
                self.show_toast(format!("Alias removed from {title}").into(), cx);
                return;
            }
            EntryKind::RecordHotkey => return self.open_hotkey_editor(id, title, cx),
            EntryKind::RemoveHotkey => {
                if let Err(err) = self.hotkeys.set_item(&id, None) {
                    eprintln!("[spotlight] could not remove the hotkey of {id:?}: {err}");
                }
                self.hotkey_assigned(&id, None, cx);
                self.show_toast(format!("Hotkey removed from {title}").into(), cx);
                return;
            }
            EntryKind::DeleteCalculation(calculation) => {
                return self.delete_calculation(calculation, cx);
            }
            EntryKind::ClearCalculations => return self.clear_calculations(cx),
        };
        // Using an answer keeps it; using a history entry moves it to the top.
        if id == CALC_ANSWER_ID {
            let expression = self.query(cx).trim().to_owned();
            self.remember_calculation(expression, title, cx);
        } else if let Some((expression, answer)) = self.history_entry(&id) {
            self.remember_calculation(expression, answer, cx);
        }
        // Only catalog items are ranked, so only their use is worth
        // remembering, also when they're used from a list (Browse Apps).
        let ranked = matches!(row, Row::Catalog(_))
            || self.search.catalog().iter().any(|item| item.id == id);
        let edits = matches!(
            command,
            Command::SaveQuicklink(_)
                | Command::DeleteQuicklink(_)
                | Command::SaveSnippet(_)
                | Command::DeleteSnippet(_)
                | Command::Complete(_)
        );
        if ranked && !edits {
            self.record_visit(id, cx);
        }
        self.perform(&command, window, cx);
    }

    /// Opens a view, or carries out the command and hides the window.
    fn perform(&mut self, command: &Command, window: &mut Window, cx: &mut Context<Self>) {
        if command.needs_selection() {
            return self.run_with_selection(command.clone(), cx);
        }
        match command {
            Command::PushView(view) => self.push_view((**view).clone(), window, cx),
            Command::OpenCalculatorHistory => self.open_calculator_history(window, cx),
            Command::SaveQuicklink(link) => self.save_quicklink(link.clone(), window, cx),
            Command::DeleteQuicklink(id) => self.delete_quicklink(*id, window, cx),
            Command::SaveSnippet(snippet) => self.save_snippet(snippet.clone(), window, cx),
            Command::DeleteSnippet(id) => self.delete_snippet(*id, window, cx),
            Command::PasteSnippet { text, paste } => self.paste_snippet(text, *paste, cx),
            Command::ImportSnippets => self.import_snippets(cx),
            Command::ExportSnippets => self.export_snippets(cx),
            Command::OpenProviderView { provider, view } => {
                self.open_provider_view(provider, view, window, cx)
            }
            Command::Complete(text) => self.complete(text.clone(), window, cx),
            Command::RestoreClip { id, paste } => self.restore_clip(*id, *paste, cx),
            Command::PinClip { id, pinned } => self.pin_clip(*id, *pinned, window, cx),
            Command::DeleteClip(id) => self.delete_clip(*id, window, cx),
            Command::ClearClipboardHistory => self.clear_clips(window, cx),
            Command::IgnoreClipApp(app) => self.ignore_clip_app(app.clone(), cx),
            command => self.run_command(command, cx),
        }
    }

    /// Asks `provider` for its view `id` and opens it.
    fn open_provider_view(
        &mut self,
        provider: &str,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self
            .search
            .providers()
            .iter()
            .find(|candidate| candidate.id() == provider)
            .and_then(|provider| provider.view(id));
        match view {
            Some(view) => {
                self.push_view(view, window, cx);
                if let Some(Screen::List { origin, .. }) = self.screens.last_mut() {
                    *origin = Some((provider.to_owned(), id.to_owned()));
                }
            }
            None => {
                eprintln!("[spotlight] {provider} has no view {id:?}");
                self.show_toast("Nothing to show".into(), cx);
            }
        }
    }

    /// Back to root search with `text` in the field, as if typed.
    fn complete(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.screens.is_empty() {
            self.screens.clear();
            self.enter_screen(window, cx);
        }
        self.input.update(cx, |input, cx| input.type_over(text, cx));
        window.focus(&self.input.focus_handle(cx));
    }

    /// Carries out `command`, hides the window, and shows a HUD where the
    /// result would otherwise be invisible.
    fn run_command(&mut self, command: &Command, cx: &mut Context<Self>) {
        let mut hud: Option<SharedString> = None;
        match command {
            Command::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                hud = Some("Copied to Clipboard".into());
            }
            command => self.run_in_background(command.clone(), cx),
        }
        self.hide(cx);
        if let Some(text) = hud {
            self.show_hud(text, cx);
        }
    }

    /// Reads the text selected in the app the launcher came from, then runs
    /// `command` with it. The launcher hides first: the app has to be in
    /// front for its selection to be read.
    fn run_with_selection(&mut self, command: Command, cx: &mut Context<Self>) {
        let provider = self
            .search
            .providers()
            .iter()
            .position(|provider| provider.id() == SELECTION_PROVIDER);
        if !provider.is_some_and(|ix| self.search.is_enabled(ix)) {
            self.hide(cx);
            self.show_hud("Selected Text is turned off in Settings".into(), cx);
            return;
        }
        let outcome = selection::spawn(self.previous_window, self.hwnd, self.selection_copy);
        self.hide(cx);
        cx.spawn(async move |this, cx| {
            let outcome = outcome
                .recv()
                .await
                .unwrap_or_else(|_| Err("Couldn't read the selection".into()));
            this.update(cx, |this, cx| match outcome {
                Ok(Some(text)) => this.run_command(&command.with_selection(&text), cx),
                Ok(None) => this.show_hud("No text selected".into(), cx),
                Err(err) => this.show_hud(err.into(), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Lets reading the selected text use Ctrl+C when an app doesn't share it.
    pub fn set_selection_copy(&mut self, enabled: bool) {
        self.selection_copy = enabled;
    }

    /// Loads provider `id`'s catalog again (its items depend on a setting).
    pub fn reload_provider(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_catalog(id, window.scale_factor(), cx);
    }

    /// Runs a command in the background and shows what it reports
    /// ("Bluetooth Off", "Volume 40%", or why it failed) in the HUD, unless
    /// the launcher has been opened again meanwhile.
    fn run_in_background(&mut self, command: Command, cx: &mut Context<Self>) {
        let outcome = commands::spawn(command);
        cx.spawn(async move |this, cx| {
            let Ok(outcome) = outcome.recv().await else {
                return;
            };
            let text = match outcome {
                Ok(Some(text)) | Err(text) => text,
                Ok(None) => return,
            };
            this.update(cx, |this, cx| {
                if !this.visible {
                    this.show_hud(text.into(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// An item hotkey was pressed: runs that item's primary action without
    /// showing the launcher. One that asks for confirmation (Shut Down) opens
    /// the launcher on just that item instead, waiting for Enter.
    pub fn run_by_id(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.search.catalog().iter().position(|item| item.id == id) else {
            // Item ids start with their provider's id.
            let provider = id.split(':').next().unwrap_or_default();
            let off = self
                .sources()
                .into_iter()
                .find(|source| source.id == provider && !source.enabled);
            match off {
                Some(source) => self.show_hud(
                    format!("{} is turned off in Settings", source.name).into(),
                    cx,
                ),
                None => eprintln!("[spotlight] hotkey for {id:?}, which isn't loaded (yet)"),
            }
            return;
        };
        let Some(action) = self.search.catalog()[ix].primary_action().cloned() else {
            return;
        };
        // Commands that open a view (Clipboard History) open the launcher on
        // it; pressed again while that view is up, the launcher closes.
        if matches!(
            action.command,
            Command::PushView(_)
                | Command::OpenProviderView { .. }
                | Command::OpenCalculatorHistory
        ) {
            let title = &self.search.catalog()[ix].title;
            if self.visible && self.screen_title().as_ref() == Some(title) {
                self.hide(cx);
                return;
            }
            self.show(Instant::now(), window, cx);
            self.record_visit(id.to_owned(), cx);
            self.perform(&action.command, window, cx);
            return;
        }
        if action.command.needs_selection() {
            // The hotkey was pressed in the app whose selection it's for.
            self.previous_window = system_clipboard::foreground_window();
            self.record_visit(id.to_owned(), cx);
            return self.run_with_selection(action.command, cx);
        }
        if action.command.needs_confirmation() {
            self.show(Instant::now(), window, cx);
            self.results = vec![Row::Catalog(ix)];
            self.confirming = Some((id.to_owned(), 0));
            cx.notify();
            return;
        }
        self.run_command(&action.command, cx);
        self.record_visit(id.to_owned(), cx);
    }

    fn open_alias_editor(
        &mut self,
        item_id: String,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.aliases.get(&item_id).cloned().unwrap_or_default();
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Short name, e.g. vs", cx);
            input.set_text(current, cx);
            input
        });
        window.focus(&input.focus_handle(cx));
        self.editor = Some(Editor::Alias {
            item_id,
            title,
            input,
        });
        cx.notify();
    }

    /// Saves the alias being edited (an empty one removes it) and closes the editor.
    fn save_alias(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Editor::Alias {
            item_id,
            title,
            input,
        }) = &self.editor
        else {
            return;
        };
        let (item_id, title) = (item_id.clone(), title.clone());
        let alias = input.read(cx).text().to_owned();
        let message = match self.assign_alias(&item_id, &alias, cx) {
            Some(alias) => format!("Type “{alias}” to find {title}"),
            None => format!("Alias removed from {title}"),
        };
        self.close_editor(Some(window), cx);
        self.show_toast(message.into(), cx);
    }

    /// Gives `item_id` the alias `alias` (trimmed; empty removes it) and saves
    /// it. One alias, one item: whoever had it loses it. Returns the alias set.
    pub fn assign_alias(
        &mut self,
        item_id: &str,
        alias: &str,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let alias = alias.trim();
        if alias.is_empty() {
            self.set_alias(item_id, None, cx);
            cx.notify();
            return None;
        }
        let taken: Vec<String> = self
            .aliases
            .iter()
            .filter(|(other, existing)| *other != item_id && existing.eq_ignore_ascii_case(alias))
            .map(|(other, _)| other.clone())
            .collect();
        for other in taken {
            self.set_alias(&other, None, cx);
        }
        self.set_alias(item_id, Some(alias.to_owned()), cx);
        cx.notify();
        Some(alias.to_owned())
    }

    /// Records that `item_id`'s hotkey is now `hotkey` (already registered, or
    /// unregistered for `None`, with the hotkey service) and saves it.
    pub fn hotkey_assigned(
        &mut self,
        item_id: &str,
        hotkey: Option<Hotkey>,
        cx: &mut Context<Self>,
    ) {
        match hotkey {
            Some(hotkey) => self.item_hotkeys.insert(item_id.to_owned(), hotkey),
            None => self.item_hotkeys.remove(item_id),
        };
        self.persist_hotkey(item_id, hotkey, cx);
        cx.notify();
    }

    /// Every item that has an alias or a hotkey, by title. For the settings window.
    pub fn shortcuts(&self) -> Vec<Shortcut> {
        let mut ids: Vec<&String> = self
            .aliases
            .keys()
            .chain(self.item_hotkeys.keys())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let mut shortcuts: Vec<Shortcut> = ids
            .into_iter()
            .map(|id| Shortcut {
                item_id: id.clone(),
                // Items that haven't loaded (or no longer exist) show their id.
                title: self
                    .search
                    .catalog()
                    .iter()
                    .find(|item| &item.id == id)
                    .map_or_else(|| id.clone(), |item| item.title.clone()),
                alias: self.aliases.get(id).cloned(),
                hotkey: self.item_hotkeys.get(id).copied(),
            })
            .collect();
        shortcuts.sort_by_cached_key(|shortcut| shortcut.title.to_lowercase());
        shortcuts
    }

    /// Every item that can take an alias or a hotkey (the ones that do
    /// something), by title. A provider that is off has none loaded.
    pub fn extension_items(&self) -> Vec<ExtensionItem> {
        let mut items: Vec<ExtensionItem> = self
            .search
            .catalog()
            .iter()
            .filter(|item| !item.actions.is_empty())
            .map(|item| ExtensionItem {
                provider: item.id.split(':').next().unwrap_or_default().to_owned(),
                shortcut: Shortcut {
                    item_id: item.id.clone(),
                    title: item.title.clone(),
                    alias: self.aliases.get(&item.id).cloned(),
                    hotkey: self.item_hotkeys.get(&item.id).copied(),
                },
                subtitle: item.subtitle.clone(),
                image: self.icons.get(&item.id).cloned(),
                glyph: match item.icon {
                    Icon::Glyph(glyph) => Some(glyph),
                    _ => None,
                },
            })
            .collect();
        items.sort_by_cached_key(|item| item.shortcut.title.to_lowercase());
        items
    }

    fn set_alias(&mut self, item_id: &str, alias: Option<String>, cx: &mut Context<Self>) {
        match &alias {
            Some(alias) => self.aliases.insert(item_id.to_owned(), alias.clone()),
            None => self.aliases.remove(item_id),
        };
        let item_id = item_id.to_owned();
        self.persist(cx, move |store| store.set_alias(&item_id, alias.as_deref()));
    }

    fn open_hotkey_editor(&mut self, item_id: String, title: String, cx: &mut Context<Self>) {
        let recording = self.record_hotkey(cx);
        self.editor = Some(Editor::Hotkey {
            item_id,
            title,
            message: None,
            _recording: recording,
        });
        cx.notify();
    }

    /// Starts the recorder hook and feeds what it sees to [`Self::recorded`].
    /// While it runs no other app gets key presses, so it ends on Esc, on a
    /// combination that's taken, and when the window hides.
    fn record_hotkey(&self, cx: &mut Context<Self>) -> Task<()> {
        let (tx, rx) = async_channel::unbounded();
        self.hotkeys.record(tx);
        cx.spawn(async move |this, cx| {
            while let Ok(recorded) = rx.recv().await {
                let keep = this.update(cx, |this, cx| this.recorded(recorded, cx));
                if !keep.unwrap_or(false) {
                    break;
                }
            }
        })
    }

    /// One key press from the recorder. Returns whether to keep recording.
    fn recorded(&mut self, recorded: Recorded, cx: &mut Context<Self>) -> bool {
        let Some(Editor::Hotkey {
            item_id,
            title,
            message,
            ..
        }) = &mut self.editor
        else {
            return false;
        };
        let hotkey = match recorded {
            Recorded::Cancel => {
                self.close_editor(None, cx);
                return false;
            }
            Recorded::NeedsModifier => {
                *message = Some("Add Ctrl, Alt, Shift or Win to the key.".into());
                cx.notify();
                return true;
            }
            Recorded::Hotkey(hotkey) => hotkey,
        };
        let (item_id, title) = (item_id.clone(), title.clone());
        // `set_item` ends recording, whatever the outcome.
        match self.hotkeys.set_item(&item_id, Some(hotkey)) {
            Ok(()) => {
                self.hotkey_assigned(&item_id, Some(hotkey), cx);
                self.editor = None;
                self.show_toast(format!("{hotkey} now runs {title}").into(), cx);
                false
            }
            Err(err) => {
                let text = match err {
                    SetError::InUse => format!("{hotkey} is already taken. Try another."),
                    SetError::Other(err) => format!("Couldn't use {hotkey}: {err}"),
                };
                // Listen again for the next try; this task's loop ends.
                let recording = self.record_hotkey(cx);
                if let Some(Editor::Hotkey {
                    message,
                    _recording,
                    ..
                }) = &mut self.editor
                {
                    *message = Some(text.into());
                    *_recording = recording;
                }
                cx.notify();
                false
            }
        }
    }

    /// Closes the alias or hotkey editor, ending recording, and gives the
    /// search field the keyboard again when there's a `window` to focus.
    fn close_editor(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        match self.editor.take() {
            Some(Editor::Hotkey { .. }) => self.hotkeys.resume(),
            Some(Editor::Alias { .. }) => {}
            None => return,
        }
        if let Some(window) = window {
            window.focus(&self.input.focus_handle(cx));
        }
        cx.notify();
    }

    fn persist_hotkey(&self, item_id: &str, hotkey: Option<Hotkey>, cx: &mut Context<Self>) {
        let item_id = item_id.to_owned();
        let text = hotkey.map(|hotkey| hotkey.to_string());
        self.persist(cx, move |store| {
            store.set_item_hotkey(&item_id, text.as_deref())
        });
    }

    /// Runs a database write on a background thread; without a database the
    /// change lasts for this run only.
    fn persist(
        &self,
        cx: &mut Context<Self>,
        write: impl FnOnce(&Store) -> spotlight_store::Result<()> + Send + 'static,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let result = store
                    .lock()
                    .map_err(|_| "store lock poisoned".to_owned())
                    .and_then(|store| write(&store).map_err(|err| err.to_string()));
                if let Err(err) = result {
                    eprintln!("[spotlight] could not save: {err}");
                }
            })
            .detach();
    }

    /// Turns the hidden window into the HUD for a moment. Showing the launcher
    /// (hotkey, tray) ends it early.
    pub fn show_hud(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.hud = Some(text);
        self.hud_generation += 1;
        let generation = self.hud_generation;
        win::show_without_focus_near_bottom(self.hwnd, HUD_SIZE);
        cx.notify();

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HUD_DURATION).await;
            this.update(cx, |this, cx| {
                if this.hud_generation == generation && this.hud.take().is_some() {
                    win::hide(this.hwnd);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn reset_ranking(&mut self, id: &str, cx: &mut Context<Self>) {
        self.frecency.remove(id);
        let query = self.query(cx);
        self.refilter(&query);
        // Keep the same item selected where it now ranks.
        if let Some(row) = self.results.iter().position(|row| self.item(row).id == id) {
            self.selected = row;
            self.scroll.reveal(row, self.results.len());
        }
        cx.notify();

        let Some(store) = self.store.clone() else {
            return;
        };
        let id = id.to_owned();
        cx.background_executor()
            .spawn(async move {
                let result = store
                    .lock()
                    .map_err(|_| "store lock poisoned".to_owned())
                    .and_then(|store| store.forget(&id).map_err(|err| err.to_string()));
                if let Err(err) = result {
                    eprintln!("[spotlight] could not reset the ranking of {id:?}: {err}");
                }
            })
            .detach();
    }

    fn show_toast(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.toast = Some(message);
        self.toast_generation += 1;
        let generation = self.toast_generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_DURATION).await;
            this.update(cx, |this, cx| {
                if this.toast_generation == generation {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Updates frecency in memory right away and in the database on a background thread.
    fn record_visit(&mut self, id: String, cx: &mut Context<Self>) {
        let now = frecency::now();
        let used = self.frecency.entry(id.clone()).or_default();
        *used = used.visit(now);

        let Some(store) = self.store.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let result = store
                    .lock()
                    .map_err(|_| "store lock poisoned".to_owned())
                    .and_then(|mut store| {
                        store.record_visit(&id, now).map_err(|err| err.to_string())
                    });
                if let Err(err) = result {
                    eprintln!("[spotlight] could not save frecency for {id:?}: {err}");
                }
            })
            .detach();
    }

    /// Esc closes the alias editor or the action panel first, then goes back
    /// from an opened list to root search, then hides the window.
    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            self.close_editor(Some(window), cx);
        } else if self.panel.take().is_some() {
            cx.notify();
        } else if !self.screens.is_empty() {
            self.pop_screen(window, cx);
        } else {
            self.hide(cx);
        }
    }

    fn open_calculator_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let calculations = self
            .store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|store| {
                store.calculations().unwrap_or_else(|err| {
                    eprintln!("[spotlight] could not read the calculator history: {err}");
                    Vec::new()
                })
            })
            .unwrap_or_default();
        let now = frecency::now();
        let list = ListView {
            title: "Calculator History".into(),
            placeholder: Some("Search calculator history…".into()),
            items: calculations
                .iter()
                .map(|calculation| history_item(calculation, now))
                .collect(),
            empty: Some("No calculations yet. Copy a calculator answer to keep it here.".into()),
            filters: Vec::new(),
        };
        self.push_view(View::List(list), window, cx);
    }

    /// The expression and answer behind a row of the history list.
    fn history_entry(&self, item_id: &str) -> Option<(String, String)> {
        if !item_id.starts_with(HISTORY_ITEM) {
            return None;
        }
        let item = self.list_items()?.iter().find(|item| item.id == item_id)?;
        match item.keywords.as_slice() {
            [expression, answer] => Some((expression.clone(), answer.clone())),
            _ => None,
        }
    }

    /// Saves a calculation the user used to the history (on a background thread).
    fn remember_calculation(&self, expression: String, answer: String, cx: &mut Context<Self>) {
        if expression.is_empty() {
            return;
        }
        let now = frecency::now();
        let Some(store) = self.store.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let result = store
                    .lock()
                    .map_err(|_| "store lock poisoned".to_owned())
                    .and_then(|mut store| {
                        store
                            .add_calculation(&expression, &answer, now)
                            .map_err(|err| err.to_string())
                    });
                if let Err(err) = result {
                    eprintln!("[spotlight] could not save the calculation: {err}");
                }
            })
            .detach();
    }

    fn delete_calculation(&mut self, id: i64, cx: &mut Context<Self>) {
        let item_id = format!("{HISTORY_ITEM}{id}");
        if let Some(Screen::List { view, .. }) = self.screens.last_mut() {
            view.items.retain(|item| item.id != item_id);
        }
        self.persist(cx, move |store| store.delete_calculation(id));
        let query = self.query(cx);
        self.refilter(&query);
        self.show_toast("Entry deleted".into(), cx);
    }

    fn clear_calculations(&mut self, cx: &mut Context<Self>) {
        if let Some(Screen::List { view, .. }) = self.screens.last_mut() {
            view.items.clear();
        }
        self.persist(cx, |store| store.clear_calculations());
        let query = self.query(cx);
        self.refilter(&query);
        self.show_toast("Calculator history cleared".into(), cx);
    }

    /// The open view's title; `None` in root search.
    pub fn screen_title(&self) -> Option<String> {
        self.screens.last().map(|screen| screen.title().to_owned())
    }

    fn render_row(&self, row: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let item = self.item(&self.results[row]);
        let selected = row == self.selected;
        let confirming = selected
            && self
                .confirming
                .as_ref()
                .is_some_and(|(id, _)| id == &item.id);
        let subtitle: SharedString = if confirming {
            "Press Enter again to confirm".into()
        } else {
            item.subtitle.clone().unwrap_or_default().into()
        };

        // Outer row spans the list width (uniform_list items otherwise shrink to
        // their content); the inner block carries the inset, rounded highlight.
        div().id(row).w_full().h(px(ROW_HEIGHT)).px_2().child(
            div()
                .id("highlight")
                .size_full()
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded(px(6.))
                .when(selected, |row| row.bg(rgba(0xffffff1f)))
                .when(!selected, |row| {
                    row.hover(|style| style.bg(rgba(0xffffff0f)))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.selected != row {
                        this.selected = row;
                        this.confirming = None;
                    }
                    this.panel = None;
                    this.confirm(&Confirm, window, cx);
                }))
                .child(self.render_icon(item))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .child(SharedString::from(item.title.clone())),
                )
                .when_some(self.aliases.get(&item.id), |row, alias| {
                    row.child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(rgba(theme::SURFACE_BORDER))
                            .text_size(px(11.))
                            .text_color(rgba(theme::TEXT_MUTED))
                            .child(SharedString::from(alias.clone())),
                    )
                })
                .when_some(self.item_hotkeys.get(&item.id), |row, hotkey| {
                    row.child(keycaps(&hotkey_keys(*hotkey)))
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(if confirming {
                            rgba(theme::ERROR)
                        } else {
                            rgba(0xffffff70)
                        })
                        .child(subtitle),
                ),
        )
    }

    fn render_icon(&self, item: &SearchItem) -> AnyElement {
        self.render_icon_sized(item, ICON_SIZE)
    }

    /// The item's icon, glyph or letter badge, `size` logical pixels square.
    fn render_icon_sized(&self, item: &SearchItem, size: f32) -> AnyElement {
        if let Some(icon) = self.icons.get(&item.id) {
            return img(icon.clone())
                .size(px(size))
                .flex_none()
                .into_any_element();
        }
        let scale = size / ICON_SIZE;
        let badge = div()
            .size(px(size))
            .flex_none()
            .rounded(px(6. * scale))
            .flex()
            .items_center()
            .justify_center();
        match item.icon {
            Icon::Glyph(glyph) => badge
                .bg(rgba(0xffffff1a))
                .font_family(icon_font())
                .text_size(px(15. * scale))
                .child(SharedString::from(glyph.to_string()))
                .into_any_element(),
            // Plain items, and apps whose icon hasn't arrived or doesn't exist.
            Icon::App(_) | Icon::Shell(_) | Icon::None => {
                let initial: SharedString = item
                    .title
                    .chars()
                    .next()
                    .map(|ch| ch.to_uppercase().collect::<String>())
                    .unwrap_or_default()
                    .into();
                badge
                    .bg(badge_color(&item.title))
                    .text_size(px(13. * scale))
                    .child(initial)
                    .into_any_element()
            }
        }
    }

    /// The Ctrl+K panel: the selected item's actions, above the footer's right end.
    fn render_panel(&self, selected: usize, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .results
            .get(self.selected)
            .map(|row| SharedString::from(self.item(row).title.clone()))
            .unwrap_or_default();
        let rows = self
            .selected_actions()
            .into_iter()
            .enumerate()
            .map(|(ix, action)| {
                let shortcut = match ix {
                    0 => Some("Enter"),
                    1 => Some("Ctrl Enter"),
                    _ => None,
                };
                div()
                    .id(ix)
                    .h(px(32.))
                    .px_2()
                    .mx_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(5.))
                    .when(ix == selected, |row| row.bg(rgba(0xffffff1f)))
                    .when(ix != selected, |row| {
                        row.hover(|style| style.bg(rgba(0xffffff0f)))
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.run_action(ix, window, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .child(SharedString::from(action.title)),
                    )
                    .children(shortcut.map(keycaps))
            });
        div()
            .absolute()
            .right(px(10.))
            .bottom(px(38.))
            .w(px(300.))
            .py_1()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(rgba(theme::SURFACE_BORDER))
            .bg(rgba(theme::PANEL))
            .shadow_lg()
            .text_size(px(13.))
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .pb_1()
                    .text_size(px(11.))
                    .text_color(rgba(theme::TEXT_MUTED))
                    .truncate()
                    .child(title),
            )
            .children(rows)
            .into_any_element()
    }

    /// The alias or hotkey editor: a card above the footer.
    fn render_editor(&self, editor: &Editor) -> AnyElement {
        let (heading, body, note, hint): (String, AnyElement, Option<SharedString>, &str) =
            match editor {
                Editor::Alias { title, input, .. } => (
                    format!("Alias for {title}"),
                    div()
                        .px_2()
                        .py_1()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(rgba(theme::ACCENT))
                        .bg(rgba(theme::SURFACE))
                        .child(input.clone())
                        .into_any_element(),
                    None,
                    "Enter saves (empty removes it) · Esc cancels",
                ),
                Editor::Hotkey { title, message, .. } => (
                    format!("Hotkey for {title}"),
                    div()
                        .py_1()
                        .text_size(px(15.))
                        .child("Press a key combination…")
                        .into_any_element(),
                    message.clone(),
                    "It works everywhere, even with the launcher closed · Esc cancels",
                ),
            };
        div()
            .absolute()
            .left(px(10.))
            .right(px(10.))
            .bottom(px(38.))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(rgba(theme::SURFACE_BORDER))
            .bg(rgba(theme::PANEL))
            .shadow_lg()
            .text_size(px(13.))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgba(theme::TEXT_MUTED))
                    .truncate()
                    .child(SharedString::from(heading)),
            )
            .child(body)
            .when_some(note, |card, note| {
                card.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgba(theme::ERROR))
                        .child(note),
                )
            })
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgba(theme::TEXT_FAINT))
                    .child(hint),
            )
            .into_any_element()
    }

    /// Right side of the footer: what Enter does, and how to see the rest.
    fn footer_hints(&self) -> impl IntoElement {
        let (primary, primary_keys) = match self.screens.last() {
            Some(Screen::Form(form)) => (
                Some(SharedString::from(form.view.submit_title.clone())),
                form.submit_keys(),
            ),
            _ => (
                self.selected_actions()
                    .into_iter()
                    .next()
                    .map(|action| SharedString::from(action.title)),
                "Enter",
            ),
        };
        let has_actions = !self.selected_actions().is_empty();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .when_some(primary, |hints, primary| {
                hints
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().text_color(rgba(theme::TEXT)).child(primary))
                            .child(keycaps(primary_keys)),
                    )
                    .when(has_actions, |hints| {
                        hints.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child("Actions")
                                .child(keycaps("Ctrl K")),
                        )
                    })
            })
    }

    /// Paint-probe values here lag one frame (they are recorded while painting).
    fn footer(&self) -> String {
        let memory = metrics::memory();
        let paint = self.stats.paint.borrow();
        let fmt = |value: Option<f64>| value.map_or("–".to_owned(), |ms| format!("{ms:.1}"));
        format!(
            "{hotkey} · {results}/{items} items · apps {discover} ms · hotkey→paint first {first} / last {last} ms · filter {filter} ms · key→paint {key} ms · RAM ws {ws:.0} MB / private {private:.0} MB",
            hotkey = self.stats.hotkey,
            results = self.results.len(),
            items = self.search.catalog().len(),
            discover = fmt(self.stats.discover_ms),
            first = fmt(paint.first_show_ms),
            last = fmt(paint.last_show_ms),
            filter = fmt(self.stats.filter_ms),
            key = fmt(paint.key_ms),
            ws = memory.working_set_mb,
            private = memory.private_mb,
        )
    }
}

impl Render for Launcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(text) = &self.hud {
            // An explicit size from the top left, not `size_full`: GPUI can
            // still think the window has the launcher's size when the HUD's
            // first frame is laid out, which would centre the text off screen.
            let (width, height) = HUD_SIZE;
            return div()
                .w(px(width))
                .h(px(height))
                .flex()
                .items_center()
                .justify_center()
                .px_4()
                .bg(rgba(theme::HUD_TINT))
                .border_1()
                .border_color(rgba(theme::SURFACE_BORDER))
                .rounded(px(8.))
                .text_color(rgba(theme::TEXT))
                .text_size(px(14.))
                .line_height(px(20.))
                .text_center()
                .child(div().truncate().child(text.clone()))
                .into_any_element();
        }
        self.drop_stale_images(window);
        if self.scroll.tick() {
            window.request_animation_frame();
        }

        let results: AnyElement = if let Some(body) = self.render_screen_body(window, cx) {
            body
        } else if self.results.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgba(0xffffff60))
                .child(match self.screens.last() {
                    Some(Screen::List { view, .. }) if view.items.is_empty() => {
                        SharedString::from(view.empty.clone().unwrap_or("Nothing here".into()))
                    }
                    _ => "No results".into(),
                })
                .into_any_element()
        } else {
            let list = uniform_list(
                "results",
                self.results.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    range
                        .map(|row| this.render_row(row, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.handle())
            .size_full()
            .py(px(LIST_PADDING));

            // Wheel notches are animated instead; everything else falls through to GPUI.
            let wheel = capture_wheel(cx.entity().downgrade(), |this: &mut Self, delta, cx| {
                if !this.scroll.is_enabled() {
                    return false;
                }
                this.scroll.scroll_by(delta, this.results.len());
                cx.notify();
                true
            });

            div()
                .flex_1()
                .relative()
                .child(list)
                .child(wheel)
                .into_any_element()
        };
        // Lists with previews (clipboard history): rows on the left, the
        // selected row's preview on the right.
        let results = if self.shows_previews() {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .flex_none()
                        .w(px(PREVIEW_LIST_WIDTH))
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(results),
                )
                .child(self.render_preview(cx))
                .into_any_element()
        } else {
            results
        };

        let paint = self.stats.paint.clone();
        div()
            .key_context(CONTEXT)
            .relative()
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::secondary_action))
            .on_action(cx.listener(Self::toggle_actions))
            .on_action(cx.listener(Self::grid_left))
            .on_action(cx.listener(Self::grid_right))
            .on_action(cx.listener(Self::focus_next_field))
            .on_action(cx.listener(Self::focus_prev_field))
            .on_action(cx.listener(Self::next_choice))
            .on_action(cx.listener(Self::prev_choice))
            .on_action(cx.listener(Self::toggle_choice))
            .on_action(cx.listener(Self::dismiss))
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgba(theme::BACKGROUND_TINT))
            .text_color(rgb(0xececec))
            .text_size(px(14.))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(20.))
                    .line_height(px(28.))
                    .map(|row| match self.screens.last() {
                        // Views without a search field show their title there.
                        Some(screen) if !screen.has_search() => row
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(screen.title().to_owned())),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(11.))
                                    .text_color(rgba(theme::TEXT_FAINT))
                                    .child(keycaps("Esc"))
                                    .child("back"),
                            ),
                        screen => row
                            .when_some(screen, |row, screen| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .px_2()
                                        .rounded(px(5.))
                                        .bg(rgba(theme::SURFACE_HOVER))
                                        .text_size(px(12.))
                                        .line_height(px(22.))
                                        .text_color(rgba(theme::TEXT_MUTED))
                                        .child(SharedString::from(screen.title().to_owned())),
                                )
                            })
                            .child(div().flex_1().min_w_0().child(self.input.clone()))
                            .when_some(self.filter_label(), |row, label| {
                                row.child(
                                    div()
                                        .id("filter")
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .px_2()
                                        .rounded(px(5.))
                                        .border_1()
                                        .border_color(rgba(theme::SURFACE_BORDER))
                                        .text_size(px(12.))
                                        .line_height(px(22.))
                                        .text_color(rgba(theme::TEXT_MUTED))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgba(theme::SURFACE_HOVER)))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.step_filter(true, cx);
                                        }))
                                        .child(label)
                                        .child(keycaps("Tab")),
                                )
                            }),
                    }),
            )
            .child(div().h(px(1.)).bg(rgba(0xffffff14)))
            .child(results)
            .child(
                div()
                    .h(px(30.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_4()
                    .bg(rgba(0x00000033))
                    .text_size(px(11.))
                    .text_color(rgba(0xffffff80))
                    .child(match &self.toast {
                        Some(toast) => div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(rgba(theme::SUCCESS))
                            .child(toast.clone()),
                        None => div().flex_1().min_w_0().truncate().child(self.footer()),
                    })
                    .child(self.footer_hints()),
            )
            .when_some(self.panel, |root, selected| {
                root.child(self.render_panel(selected, cx))
            })
            .when_some(self.editor.as_ref(), |root, editor| {
                root.child(self.render_editor(editor))
            })
            // Last child, so it paints after everything else in the frame.
            .child(
                canvas(|_, _, _| {}, move |_, _, _, _| paint.borrow_mut().painted())
                    .absolute()
                    .size_full(),
            )
            .into_any_element()
    }
}

/// The keys of `hotkey` for [`keycaps`]: `"Ctrl Alt K"`. Key names never
/// contain `+` (the plus key displays as `=`, the same key).
fn hotkey_keys(hotkey: Hotkey) -> String {
    hotkey.to_string().replace('+', " ")
}

/// A row of the calculator history: `expression = answer`, found by either.
fn history_item(calculation: &Calculation, now: i64) -> SearchItem {
    let Calculation {
        id,
        expression,
        answer,
        used_at,
    } = calculation;
    let mut item = SearchItem::new(
        format!("{HISTORY_ITEM}{id}"),
        format!("{expression} = {answer}"),
    )
    .subtitle(days_ago(*used_at, now))
    // The calculator glyph, as on the calculator's own rows.
    .icon(Icon::Glyph('\u{E8EF}'))
    .action("Copy Answer", Command::Copy(answer.clone()))
    .action(
        "Copy Calculation",
        Command::Copy(format!("{expression} = {answer}")),
    );
    item.keywords = vec![expression.clone(), answer.clone()];
    item
}

/// "Today", "Yesterday", "5 days ago", by calendar days in UTC.
fn days_ago(then: i64, now: i64) -> String {
    const DAY: i64 = 24 * 3600;
    match now.div_euclid(DAY) - then.div_euclid(DAY) {
        ..=0 => "Today".into(),
        1 => "Yesterday".into(),
        days => format!("{days} days ago"),
    }
}

/// Key caps for a shortcut such as `"Ctrl K"`, one per key.
fn keycaps(shortcut: &str) -> impl IntoElement + use<> {
    let keys: Vec<SharedString> = shortcut
        .split(' ')
        .map(|key| SharedString::from(key.to_owned()))
        .collect();
    div()
        .flex()
        .gap(px(3.))
        .children(keys.into_iter().map(|key| {
            div()
                .px(px(5.))
                .rounded(px(4.))
                .bg(rgba(theme::SURFACE_HOVER))
                .text_size(px(10.))
                .text_color(rgba(theme::TEXT_MUTED))
                .child(key)
        }))
}

/// What to extract the icon from, for icons that come from the shell.
fn shell_name(icon: &Icon) -> Option<String> {
    match icon {
        Icon::App(launch_id) => Some(format!("shell:AppsFolder\\{launch_id}")),
        Icon::Shell(name) => Some(name.clone()),
        Icon::Glyph(_) | Icon::None => None,
    }
}

/// Windows 11's icon font, or its Windows 10 predecessor (same code points).
pub(crate) fn icon_font() -> &'static str {
    static FONT: OnceLock<&'static str> = OnceLock::new();
    FONT.get_or_init(|| {
        let windows =
            env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        if windows.join(r"Fonts\SegoeIcons.ttf").exists() {
            "Segoe Fluent Icons"
        } else {
            "Segoe MDL2 Assets"
        }
    })
}

/// GPUI wants BGRA with straight alpha, which is what `icons` produces; the
/// `RgbaImage` type is only the buffer container here.
fn render_image(icon: icons::Icon) -> Option<Arc<RenderImage>> {
    let buffer = RgbaImage::from_raw(icon.width, icon.height, icon.bgra)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
}

/// A stable pastel colour per title, drawn while an icon loads or when there is none.
fn badge_color(title: &str) -> Hsla {
    let mut hasher = DefaultHasher::new();
    title.hash(&mut hasher);
    let hue = (hasher.finish() % 360) as f32 / 360.;
    hsla(hue, 0.45, 0.42, 1.)
}

const FILLER: &[&str] = &[
    "alpha report",
    "budget sheet",
    "camera roll",
    "design notes",
    "export task",
    "font preview",
    "git client",
    "hex editor",
    "image viewer",
    "journal",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_long_ago() {
        const DAY: i64 = 24 * 3600;
        let now = 100 * DAY + 3600;
        assert_eq!(days_ago(now - 60, now), "Today");
        assert_eq!(days_ago(now - DAY, now), "Yesterday");
        assert_eq!(days_ago(now - 5 * DAY, now), "5 days ago");
        assert_eq!(days_ago(now + 60, now), "Today");
    }

    #[test]
    fn history_rows_are_found_by_expression_or_answer() {
        let item = history_item(
            &Calculation {
                id: 7,
                expression: "100 usd to eur".into(),
                answer: "92.10 EUR".into(),
                used_at: 0,
            },
            0,
        );
        assert_eq!(item.id, "calc-history:7");
        assert_eq!(item.title, "100 usd to eur = 92.10 EUR");
        assert_eq!(item.keywords, ["100 usd to eur", "92.10 EUR"]);
        assert_eq!(item.actions[0].command, Command::Copy("92.10 EUR".into()));
    }

    #[test]
    fn hotkeys_split_into_key_caps() {
        let caps = |text| hotkey_keys(Hotkey::parse(text).unwrap());
        assert_eq!(caps("Ctrl+Alt+K"), "Ctrl Alt K");
        assert_eq!(caps("Win+F12"), "Win F12");
        assert_eq!(caps("Ctrl++"), "Ctrl =");
    }
}
