//! The launcher view: search field, result list, metrics footer.

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
    AnyElement, App, ClipboardItem, Context, DispatchPhase, Entity, EntityInputHandler, Focusable,
    Hsla, KeyBinding, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, actions, canvas, div, hsla, img, prelude::*, px, rgb, rgba,
    uniform_list,
};
use image::{Frame, RgbaImage};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_core::{
    frecency::{self, Frecency},
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};
use spotlight_platform_win::{
    commands,
    hotkey::{self, Hotkey, Recorded, SetError},
    icons,
    metrics::{self, PaintProbe, Percentiles, Probe, Summary, ms},
    win,
};
use spotlight_search::{Candidate, RootSearch, Row, Searcher};
use spotlight_store::{Calculation, Store};

use crate::{
    smooth_scroll::SmoothScroll,
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
/// Launcher window size in logical (96-DPI) pixels.
pub const WINDOW_SIZE: (f32, f32) = (680., 420.);
/// The apps provider's id; its catalog load time is the "apps" figure in the
/// footer and the self-test summary.
const APPS_PROVIDER: &str = "apps";
/// How long a footer toast stays.
const TOAST_DURATION: Duration = Duration::from_millis(2500);
/// HUD size in logical pixels, and how long it stays.
const HUD_SIZE: (f32, f32) = (300., 44.);
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

/// What the result list shows: root search, or a list opened from it.
/// Esc goes back to root search.
enum Screen {
    Root,
    /// Most recent first.
    CalculatorHistory(Vec<Calculation>),
}

/// The calculator's answer row; using it adds the calculation to the history.
const CALC_ANSWER_ID: &str = "calc:answer";
/// Item ids of history rows: this, then the calculation's id in the store.
const HISTORY_ITEM: &str = "calc-history:";
const SEARCH_PLACEHOLDER: &str = "Search apps and commands, or calculate…";

/// An item with an alias or a hotkey, as the settings window lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shortcut {
    pub item_id: String,
    pub title: String,
    pub alias: Option<String>,
    pub hotkey: Option<Hotkey>,
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
    /// Catalogs still loading.
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
    screen: Screen,
    /// Ranks the rows of a list opened from root search.
    list_searcher: Searcher,
    visible: bool,
    stats: Stats,
    _subscriptions: Vec<Subscription>,
}

impl Launcher {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hotkey: SharedString,
        hotkeys: hotkey::Service,
        mut providers: Vec<Arc<dyn Provider>>,
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

        let mut this = Self {
            hwnd,
            input,
            loading: providers.len(),
            search: RootSearch::new(providers),
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
            screen: Screen::Root,
            list_searcher: Searcher::default(),
            visible: false,
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

    /// Loads every provider's catalog on its own thread (app discovery takes
    /// ~200 ms of COM calls) and merges each one into the search as it arrives.
    fn load_catalogs(&mut self, window_scale: f32, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::unbounded();
        for (ix, provider) in self.search.providers().iter().enumerate() {
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
                    this.catalog_loaded(ix, items, elapsed, window_scale, cx)
                });
                if updated.is_err() {
                    return;
                }
            }
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
        eprintln!(
            "[spotlight] {id}: {} items in {:.1} ms",
            items.len(),
            ms(elapsed)
        );
        if id == APPS_PROVIDER {
            self.stats.apps = items.len();
            self.stats.discover_ms = Some(ms(elapsed));
        }
        let app_icons: Vec<(String, String)> = items
            .iter()
            .filter_map(|item| match &item.icon {
                Icon::App(launch_id) => Some((item.id.clone(), launch_id.clone())),
                _ => None,
            })
            .collect();

        self.search.set_catalog(provider, items);
        self.loading = self.loading.saturating_sub(1);
        // Catalog indices moved, so the rows are rebuilt (and the selection resets).
        let query = self.query(cx);
        self.refilter(&query);
        self.load_icons(app_icons, window_scale, cx);
        cx.notify();
    }

    /// Extracts shell icons in the background; rows switch from badges to icons
    /// as they arrive. `items` holds (item id, `AppsFolder` parsing name) pairs.
    fn load_icons(
        &mut self,
        items: Vec<(String, String)>,
        window_scale: f32,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let (ids, launch_ids): (Vec<String>, Vec<String>) = items.into_iter().unzip();
        // Sized for the monitor the window was created on; GPUI scales it if that changes.
        let size = (ICON_SIZE * window_scale).ceil() as u32;
        let (tx, rx) = async_channel::unbounded();
        icons::spawn(launch_ids, size, tx);

        cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                // Apply whatever else has arrived too, so the list re-renders once per batch.
                let batch: Vec<_> = std::iter::once(first)
                    .chain(std::iter::from_fn(|| rx.try_recv().ok()))
                    .collect();
                let updated = this.update(cx, |this, cx| {
                    for (ix, icon) in batch {
                        if let Some(image) = render_image(icon) {
                            this.icons.insert(ids[ix].clone(), image);
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
        self.results = match &self.screen {
            Screen::Root => {
                let frecency = &self.frecency;
                self.search.search(
                    query,
                    |id| frecency.get(id).map_or(0., |used| used.score_at(now)),
                    &self.aliases,
                )
            }
            Screen::CalculatorHistory(calculations) => {
                let items: Vec<SearchItem> = calculations
                    .iter()
                    .map(|calculation| history_item(calculation, now))
                    .collect();
                let candidates = items.iter().map(|item| Candidate {
                    title: &item.title,
                    keywords: &item.keywords,
                    alias: None,
                    frecency: 0.,
                });
                let order = self.list_searcher.filter(query, candidates);
                order
                    .into_iter()
                    .map(|ix| Row::Computed(items[ix].clone()))
                    .collect()
            }
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
    /// Types into the field that has the keyboard: the alias editor's while it's open.
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = match &self.editor {
            Some(Editor::Alias { input, .. }) => input.clone(),
            _ => self.input.clone(),
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

    pub fn show(&mut self, pressed_at: Instant, window: &mut Window, cx: &mut Context<Self>) {
        self.hud = None;
        if !matches!(self.screen, Screen::Root) {
            self.screen = Screen::Root;
            self.input.update(cx, |input, cx| {
                input.set_placeholder(SEARCH_PLACEHOLDER, cx)
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
        if self.selected > 0 {
            self.selected -= 1;
            self.confirming = None;
            self.scroll.reveal(self.selected, self.results.len());
            cx.notify();
        }
    }

    fn selected_actions(&self) -> Vec<Entry> {
        self.results
            .get(self.selected)
            .map(|row| self.actions_for(row))
            .unwrap_or_default()
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

    /// Ctrl+Enter: the second action (Run as Administrator, Copy Calculation…).
    fn secondary_action(
        &mut self,
        _: &SecondaryAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.panel.is_none() && self.editor.is_none() {
            self.run_action(1, window, cx);
        }
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
        if command == Command::OpenCalculatorHistory {
            self.record_visit(id, cx);
            return self.open_calculator_history(window, cx);
        }
        // Using an answer keeps it; using a history entry moves it to the top.
        if id == CALC_ANSWER_ID {
            let expression = self.query(cx).trim().to_owned();
            self.remember_calculation(expression, title, cx);
        } else if let Some(calculation) = self.history_entry(&id) {
            self.remember_calculation(calculation.expression, calculation.answer, cx);
        }
        self.run_command(&command, cx);
        // Only catalog items are ranked, so only their use is worth remembering.
        if matches!(row, Row::Catalog(_)) {
            self.record_visit(id, cx);
        }
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
            eprintln!("[spotlight] hotkey for {id:?}, which isn't loaded (yet)");
            return;
        };
        let Some(action) = self.search.catalog()[ix].primary_action().cloned() else {
            return;
        };
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
        } else if !matches!(self.screen, Screen::Root) {
            self.go_to(Screen::Root, cx);
        } else {
            self.hide(cx);
        }
    }

    /// Switches the result list to `screen`, with an empty query.
    fn go_to(&mut self, screen: Screen, cx: &mut Context<Self>) {
        let placeholder = match &screen {
            Screen::Root => SEARCH_PLACEHOLDER,
            Screen::CalculatorHistory(_) => "Search calculator history…",
        };
        self.screen = screen;
        self.input.update(cx, |input, cx| {
            input.clear(cx);
            input.set_placeholder(placeholder, cx);
        });
        self.refilter("");
        cx.notify();
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
        self.go_to(Screen::CalculatorHistory(calculations), cx);
        window.focus(&self.input.focus_handle(cx));
    }

    /// The history entry behind a row of the history list.
    fn history_entry(&self, item_id: &str) -> Option<Calculation> {
        let id: i64 = item_id.strip_prefix(HISTORY_ITEM)?.parse().ok()?;
        match &self.screen {
            Screen::CalculatorHistory(calculations) => calculations
                .iter()
                .find(|calculation| calculation.id == id)
                .cloned(),
            Screen::Root => None,
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
        if let Screen::CalculatorHistory(calculations) = &mut self.screen {
            calculations.retain(|calculation| calculation.id != id);
        }
        self.persist(cx, move |store| store.delete_calculation(id));
        let query = self.query(cx);
        self.refilter(&query);
        self.show_toast("Entry deleted".into(), cx);
    }

    fn clear_calculations(&mut self, cx: &mut Context<Self>) {
        if let Screen::CalculatorHistory(calculations) = &mut self.screen {
            calculations.clear();
        }
        self.persist(cx, |store| store.clear_calculations());
        let query = self.query(cx);
        self.refilter(&query);
        self.show_toast("Calculator history cleared".into(), cx);
    }

    /// The opened list's name, for the tag next to the search field; `None`
    /// in root search.
    pub fn screen_title(&self) -> Option<&'static str> {
        match self.screen {
            Screen::Root => None,
            Screen::CalculatorHistory(_) => Some("Calculator History"),
        }
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
        if let Some(icon) = self.icons.get(&item.id) {
            return img(icon.clone())
                .size(px(ICON_SIZE))
                .flex_none()
                .into_any_element();
        }
        let badge = div()
            .size(px(ICON_SIZE))
            .flex_none()
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center();
        match item.icon {
            Icon::Glyph(glyph) => badge
                .bg(rgba(0xffffff1a))
                .font_family(icon_font())
                .text_size(px(15.))
                .child(SharedString::from(glyph.to_string()))
                .into_any_element(),
            // Plain items, and apps whose icon hasn't arrived or doesn't exist.
            Icon::App(_) | Icon::None => {
                let initial: SharedString = item
                    .title
                    .chars()
                    .next()
                    .map(|ch| ch.to_uppercase().collect::<String>())
                    .unwrap_or_default()
                    .into();
                badge
                    .bg(badge_color(&item.title))
                    .text_size(px(13.))
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
        let primary = self
            .selected_actions()
            .into_iter()
            .next()
            .map(|action| SharedString::from(action.title));
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
                            .child(keycaps("Enter")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child("Actions")
                            .child(keycaps("Ctrl K")),
                    )
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
                .bg(rgba(theme::BACKGROUND_TINT))
                .text_color(rgba(theme::TEXT))
                .text_size(px(14.))
                .child(div().truncate().child(text.clone()))
                .into_any_element();
        }
        if self.scroll.tick() {
            window.request_animation_frame();
        }

        let results: AnyElement = if self.results.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgba(0xffffff60))
                .child(match &self.screen {
                    Screen::CalculatorHistory(calculations) if calculations.is_empty() => {
                        "No calculations yet. Copy a calculator answer to keep it here."
                    }
                    _ => "No results",
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

            // Wheel notches are taken before the list sees them (capture phase)
            // and animated instead; everything else falls through to GPUI.
            let this = cx.entity().downgrade();
            let wheel = canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture
                            || !bounds.contains(&event.position)
                            || !matches!(event.delta, ScrollDelta::Lines(_))
                        {
                            return;
                        }
                        let delta = event.delta.pixel_delta(window.line_height()).y;
                        let handled = this
                            .update(cx, |this, cx| {
                                if !this.scroll.is_enabled() {
                                    return false;
                                }
                                this.scroll.scroll_by(delta, this.results.len());
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if handled {
                            cx.stop_propagation();
                        }
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full();

            div()
                .flex_1()
                .relative()
                .child(list)
                .child(wheel)
                .into_any_element()
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
            .on_action(cx.listener(Self::dismiss))
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
                    .when_some(self.screen_title(), |row, title| {
                        row.child(
                            div()
                                .flex_none()
                                .px_2()
                                .rounded(px(5.))
                                .bg(rgba(theme::SURFACE_HOVER))
                                .text_size(px(12.))
                                .line_height(px(22.))
                                .text_color(rgba(theme::TEXT_MUTED))
                                .child(title),
                        )
                    })
                    .child(div().flex_1().min_w_0().child(self.input.clone())),
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

/// Windows 11's icon font, or its Windows 10 predecessor (same code points).
fn icon_font() -> &'static str {
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
