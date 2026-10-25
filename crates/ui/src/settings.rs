//! The settings window: launcher hotkey, web search engine, smooth scrolling,
//! which search sources are on, how much clipboard history to keep, where
//! snippet keywords expand, start at login, and every alias and item hotkey.
//!
//! Laid out like Raycast's: a sidebar with a search field, the general pages
//! and one page per extension (search provider), and the chosen page on the
//! right with back and forward buttons. The window has no system frame; the
//! top bars are the drag area and carry their own close button.

use std::{
    ops::Range,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::Duration,
};

use gpui::{
    AnyElement, App, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Image, ImageFormat, KeyBinding, PathPromptOptions, Pixels, ScrollHandle,
    SharedString, Stateful, Subscription, Task, Window, WindowControlArea, actions, div, img,
    prelude::*, px, rgba,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_core::{
    clip::{self, IgnoredApp, Retention},
    file_search::{self, Config as FileSearchConfig, Status as FileSearchStatus},
    item::Command,
    web::{self, SearchEngine},
};
use spotlight_platform_win::{
    autostart, commands,
    hotkey::{self, Hotkey, Recorded, SetError},
    index_service, win,
};

use crate::{
    launcher::{ExtensionItem, Shortcut, Source, WindowMode, icon_font},
    smooth_scroll::{Scroller, capture_wheel},
    text_input::{self, TextInput},
    theme,
};

actions!(settings, [Close, Save]);

const CONTEXT: &str = "Settings";
/// Logical size the settings window opens at.
pub const WINDOW_SIZE: (f32, f32) = (1040., 720.);
/// The smallest it can be resized to: the sidebar and a usable page.
pub const WINDOW_MIN_SIZE: (f32, f32) = (760., 480.);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Close, Some(CONTEXT)),
        KeyBinding::new("enter", Save, Some(CONTEXT)),
    ]);
}

/// What the settings window shows from the launcher.
#[derive(Default)]
pub struct LauncherState {
    /// Items with an alias or a hotkey.
    pub shortcuts: Vec<Shortcut>,
    /// Every item that can take one, listed on its extension's page.
    pub items: Vec<ExtensionItem>,
    /// Search providers and whether each is on.
    pub sources: Vec<Source>,
    /// Apps whose copies the clipboard history doesn't keep.
    pub ignored_apps: Vec<IgnoredApp>,
    /// Apps where snippet keywords don't expand.
    pub excluded_apps: Vec<IgnoredApp>,
    /// The web search engine, which a quicklink's "Use as Web Search" in
    /// the launcher changes too, and its name.
    pub search_engine: (SearchEngine, String),
    /// Commands that open from links without asking: item id and title.
    pub trusted_links: Vec<(String, String)>,
}

/// Reads the launcher's current state; `None` while the launcher can't be
/// read (it is mid-update, as when its Ctrl+, opens this window).
pub type LauncherReader = Rc<dyn Fn(&App) -> Option<LauncherState>>;

/// Reads how far file indexing has got.
pub type FileStatusReader = Rc<dyn Fn() -> FileSearchStatus>;

/// File Search's page id, as its provider names itself.
const FILES_PAGE: &str = "files";
/// How often the File Search page updates its count while indexing.
const STATUS_REFRESH: Duration = Duration::from_millis(500);

pub enum SettingsEvent {
    /// The launcher hotkey is now this one; persist it and update labels.
    HotkeyChanged(Hotkey),
    SmoothScrollingChanged(bool),
    /// The launcher opens in this mode from now on.
    WindowModeChanged(WindowMode),
    /// Root search ranks often-used items first (or no longer does).
    RankByUsageChanged(bool),
    /// The web search engine is now this one; switch to it and save it.
    SearchEngineChanged(SearchEngine),
    /// The clipboard history now keeps this much; trim it and save it.
    ClipboardRetentionChanged(Retention),
    /// The apps whose copies aren't kept are now these; save them.
    IgnoredAppsChanged(Vec<IgnoredApp>),
    /// Win+V opens the clipboard history (`true`) or Windows' own.
    WinVChanged(bool),
    /// Reading the selected text may (or may no longer) use Ctrl+C.
    SelectionCopyChanged(bool),
    /// Snippet keywords typed in other apps expand (or no longer do).
    SnippetExpansionChanged(bool),
    /// The apps where snippet keywords don't expand are now these; save them.
    ExcludedAppsChanged(Vec<IgnoredApp>),
    /// What File Search indexes changed; save it and index again.
    FileSearchChanged(FileSearchConfig),
    /// Rebuild the file index from scratch.
    FileSearchRebuild,
    /// Start at login was turned on or off here.
    AutostartChanged,
    /// The search provider with this id was turned on or off.
    SourceToggled {
        id: &'static str,
        enabled: bool,
    },
    /// The alias of this item was edited here (`None`: removed).
    AliasChanged {
        item_id: String,
        alias: Option<String>,
    },
    /// This item's hotkey was registered (or removed) here; record and save it.
    ItemHotkeyChanged {
        item_id: String,
        hotkey: Option<Hotkey>,
    },
    /// This command asks again before it opens from a link.
    LinkDistrusted(String),
}

pub struct Settings {
    hotkeys: hotkey::Service,
    hotkey: Option<Hotkey>,
    /// Receives what the recorder hook sees; `Some` while recording.
    recording: Option<Task<()>>,
    /// Outcome of the last hotkey change, shown under the row.
    message: Option<(SharedString, bool)>,
    /// What the recorder is recording for: the launcher (`None`) or this item.
    recording_for: Option<String>,
    launcher: LauncherReader,
    /// Aliases and item hotkeys as last read from the launcher.
    shortcuts: Vec<Shortcut>,
    /// Every item that can take an alias or a hotkey, by title.
    items: Vec<ExtensionItem>,
    /// The alias being edited in place: item id and the field.
    editing_alias: Option<(String, Entity<TextInput>)>,
    /// Outcome of the last change in the list: item id, text, and whether it is an error.
    shortcut_message: Option<(String, SharedString, bool)>,
    smooth_scrolling: bool,
    window_mode: WindowMode,
    rank_by_usage: bool,
    search_engine: SearchEngine,
    /// The engine's name: a quicklink engine's is only known to the launcher.
    engine_name: String,
    /// The custom search URL field: shown while a custom engine is in use
    /// or being set up.
    custom_engine: Option<Entity<TextInput>>,
    /// Why the custom URL wasn't taken, or that it was.
    engine_message: Option<(SharedString, bool)>,
    /// Search providers and whether each is on, as last read from the launcher.
    sources: Vec<Source>,
    clipboard_retention: Retention,
    ignored_apps: Vec<IgnoredApp>,
    win_v: bool,
    selection_copy: bool,
    snippet_expansion: bool,
    excluded_apps: Vec<IgnoredApp>,
    /// Commands that open from links without asking: item id and title.
    trusted_links: Vec<(String, String)>,
    /// The apps with windows open, to add to `excluded_apps`; `Some` while
    /// the list is shown.
    app_choices: Option<Vec<IgnoredApp>>,
    file_search: FileSearchConfig,
    file_status: FileStatusReader,
    /// Where a new exclusion rule is typed.
    exclusion_input: Entity<TextInput>,
    /// The file prefix of root search, saved as it is typed.
    prefix_input: Entity<TextInput>,
    /// Redraws the File Search page while indexing, for its count.
    status_refresh: Option<Task<()>>,
    /// The index service's installer or remover is running (behind its UAC
    /// prompt), or how the last run went.
    service_busy: bool,
    service_message: Option<(SharedString, bool)>,
    autostart: bool,
    /// The build's version, shown on About.
    version: &'static str,
    data_path: SharedString,
    /// Pages visited, for back and forward; `history_ix` is the one shown.
    history: Vec<Page>,
    history_ix: usize,
    /// Filters the sidebar.
    search: Entity<TextInput>,
    /// The sidebar's list and the page, animated like the launcher's list
    /// while smooth scrolling is on.
    nav_scroll: Scroller,
    page_scroll: Scroller,
    /// The page's layout changed; render once more with the new one.
    relayout: bool,
    focus: FocusHandle,
    recorder: FocusHandle,
    _subscriptions: [Subscription; 4],
}

impl EventEmitter<SettingsEvent> for Settings {}

impl Settings {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hotkeys: hotkey::Service,
        hotkey: Option<Hotkey>,
        smooth_scrolling: bool,
        window_mode: WindowMode,
        rank_by_usage: bool,
        search_engine: SearchEngine,
        clipboard_retention: Retention,
        win_v: bool,
        selection_copy: bool,
        snippet_expansion: bool,
        file_search: FileSearchConfig,
        file_status: FileStatusReader,
        version: &'static str,
        data_path: SharedString,
        launcher: LauncherReader,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Ok(RawWindowHandle::Win32(handle)) =
            HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
        {
            win::round_corners(handle.hwnd.get());
        }
        // No title bar draws it, but the taskbar and Alt+Tab still show it.
        window.set_window_title("Spotlight Settings");
        let recorder = cx.focus_handle();
        let search = cx.new(|cx| TextInput::new("Search settings…", cx));
        // Recording takes every key press system-wide, so it ends as soon as
        // the user clicks elsewhere in the window or switches to another one.
        let prefix = file_search.prefix.clone();
        let prefix_input = cx.new(|cx| {
            let mut input = TextInput::new("Off", cx);
            input.set_text(prefix, cx);
            input
        });
        let subscriptions = [
            cx.on_blur(&recorder, window, |this, _, cx| this.stop_recording(cx)),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    // Aliases and hotkeys may have changed in the launcher
                    // meanwhile, and on the first activation it may not
                    // have been readable yet.
                    this.refresh_from_launcher(cx);
                } else {
                    this.stop_recording(cx);
                }
            }),
            cx.subscribe_in(
                &search,
                window,
                |this, _, _: &text_input::Changed, _, cx| this.search_changed(cx),
            ),
            // Only root search reads it, so nothing is indexed again.
            cx.subscribe_in(
                &prefix_input,
                window,
                |this, input, _: &text_input::Changed, _, cx| {
                    let prefix = input.read(cx).text().trim().to_owned();
                    if prefix != this.file_search.prefix {
                        this.change_file_search(|config| config.prefix = prefix, cx);
                    }
                },
            ),
        ];
        let focus = cx.focus_handle();
        window.focus(&search.focus_handle(cx));
        let LauncherState {
            shortcuts,
            items,
            sources,
            ignored_apps,
            excluded_apps,
            search_engine: (_, engine_name),
            trusted_links,
        } = launcher(cx).unwrap_or_default();
        let engine_name = if engine_name.is_empty() {
            search_engine.name()
        } else {
            engine_name
        };
        Self {
            hotkeys,
            hotkey,
            recording: None,
            message: None,
            recording_for: None,
            shortcuts,
            items,
            launcher,
            editing_alias: None,
            shortcut_message: None,
            smooth_scrolling,
            window_mode,
            rank_by_usage,
            custom_engine: match &search_engine {
                SearchEngine::Custom(template) => Some(custom_engine_input(template, cx)),
                SearchEngine::Preset(_) | SearchEngine::Quicklink(_) => None,
            },
            search_engine,
            engine_name,
            engine_message: None,
            sources,
            clipboard_retention,
            ignored_apps,
            win_v,
            selection_copy,
            snippet_expansion,
            excluded_apps,
            trusted_links,
            app_choices: None,
            file_search,
            file_status,
            exclusion_input: cx.new(|cx| TextInput::new("Folder name or path", cx)),
            prefix_input,
            status_refresh: None,
            service_busy: false,
            service_message: None,
            autostart: autostart::is_enabled(),
            version,
            data_path,
            history: vec![Page::General],
            history_ix: 0,
            search,
            nav_scroll: Scroller::new(ScrollHandle::new(), smooth_scrolling),
            page_scroll: Scroller::new(ScrollHandle::new(), smooth_scrolling),
            relayout: true,
            focus,
            recorder,
            _subscriptions: subscriptions,
        }
    }

    /// Re-reads start at login, which the tray menu can change too.
    pub fn refresh_autostart(&mut self, cx: &mut Context<Self>) {
        self.autostart = autostart::is_enabled();
        cx.notify();
    }

    fn refresh_from_launcher(&mut self, cx: &mut Context<Self>) {
        let Some(state) = (self.launcher)(cx) else {
            return;
        };
        self.shortcuts = state.shortcuts;
        self.items = state.items;
        self.sources = state.sources;
        self.ignored_apps = state.ignored_apps;
        self.excluded_apps = state.excluded_apps;
        self.trusted_links = state.trusted_links;
        let (engine, name) = state.search_engine;
        if engine != self.search_engine && self.custom_engine.is_none() {
            self.search_engine = engine;
        }
        self.engine_name = name;
        cx.notify();
    }

    /// Records a new hotkey for the launcher (`item: None`) or for an item.
    fn start_recording(
        &mut self,
        item: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.recording.is_some() {
            return;
        }
        self.editing_alias = None;
        self.shortcut_message = None;
        self.recording_for = item;
        window.focus(&self.recorder);
        let (tx, rx) = async_channel::unbounded();
        self.hotkeys.record(tx);
        self.recording = Some(cx.spawn(async move |this, cx| {
            while let Ok(recorded) = rx.recv().await {
                let handled = this.update(cx, |this, cx| this.recorded(recorded, cx));
                if !handled.unwrap_or(true) {
                    break;
                }
            }
        }));
        self.message = None;
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            self.hotkeys.resume();
            cx.notify();
        }
    }

    /// Handles one key press from the recorder. Returns whether to keep recording.
    fn recorded(&mut self, recorded: Recorded, cx: &mut Context<Self>) -> bool {
        if self.recording.is_none() {
            return false;
        }
        let hotkey = match recorded {
            Recorded::Cancel => {
                self.stop_recording(cx);
                return false;
            }
            Recorded::NeedsModifier => {
                self.message = Some(("Add Ctrl, Alt, Shift or Win to the key.".into(), true));
                cx.notify();
                return true;
            }
            Recorded::Hotkey(hotkey) => hotkey,
        };

        // `set` and `set_item` also end recording, whatever the outcome.
        self.recording = None;
        if let Some(item_id) = self.recording_for.take() {
            self.item_hotkey_recorded(item_id, hotkey, cx);
            return false;
        }
        self.message = Some(match self.hotkeys.set(hotkey) {
            Ok(()) => {
                self.hotkey = Some(hotkey);
                cx.emit(SettingsEvent::HotkeyChanged(hotkey));
                (format!("Spotlight now opens with {hotkey}.").into(), false)
            }
            Err(SetError::InUse) => (
                format!(
                    "{hotkey} is already used by another app or by Windows. Try a different shortcut."
                )
                .into(),
                true,
            ),
            Err(SetError::Other(err)) => (format!("Couldn't use {hotkey}: {err}").into(), true),
        });
        cx.notify();
        false
    }

    fn item_hotkey_recorded(&mut self, item_id: String, hotkey: Hotkey, cx: &mut Context<Self>) {
        let (text, is_error) = match self.hotkeys.set_item(&item_id, Some(hotkey)) {
            Ok(()) => {
                self.set_local(&item_id, |shortcut| shortcut.hotkey = Some(hotkey));
                cx.emit(SettingsEvent::ItemHotkeyChanged {
                    item_id: item_id.clone(),
                    hotkey: Some(hotkey),
                });
                (format!("{hotkey} saved."), false)
            }
            Err(SetError::InUse) => (format!("{hotkey} is already taken. Try another."), true),
            Err(SetError::Other(err)) => (format!("Couldn't use {hotkey}: {err}"), true),
        };
        self.shortcut_message = Some((item_id, text.into(), is_error));
        cx.notify();
    }

    fn remove_item_hotkey(&mut self, item_id: String, cx: &mut Context<Self>) {
        if let Err(err) = self.hotkeys.set_item(&item_id, None) {
            eprintln!("[settings] could not remove the hotkey of {item_id:?}: {err}");
        }
        self.set_local(&item_id, |shortcut| shortcut.hotkey = None);
        cx.emit(SettingsEvent::ItemHotkeyChanged {
            item_id,
            hotkey: None,
        });
        cx.notify();
    }

    fn edit_alias(&mut self, item_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        let current = self
            .shortcuts
            .iter()
            .chain(self.items.iter().map(|item| &item.shortcut))
            .find(|shortcut| shortcut.item_id == item_id)
            .and_then(|shortcut| shortcut.alias.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Alias", cx);
            input.set_text(current, cx);
            input
        });
        window.focus(&input.focus_handle(cx));
        self.editing_alias = Some((item_id, input));
        self.shortcut_message = None;
        cx.notify();
    }

    /// Enter: saves the alias, the custom search URL or the exclusion rule
    /// being typed, or leaves the prefix field.
    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.custom_engine
            && input.focus_handle(cx).is_focused(window)
        {
            return self.save_custom_engine(window, cx);
        }
        if self.exclusion_input.focus_handle(cx).is_focused(window) {
            return self.add_exclusion(cx);
        }
        // Saved as typed: Enter just leaves the field.
        if self.prefix_input.focus_handle(cx).is_focused(window) {
            window.focus(&self.focus);
            return;
        }
        let Some((item_id, input)) = self.editing_alias.take() else {
            return;
        };
        let alias = input.read(cx).text().trim().to_owned();
        self.set_alias(item_id, (!alias.is_empty()).then_some(alias), cx);
        window.focus(&self.focus);
    }

    fn set_alias(&mut self, item_id: String, alias: Option<String>, cx: &mut Context<Self>) {
        cx.emit(SettingsEvent::AliasChanged {
            item_id: item_id.clone(),
            alias: alias.clone(),
        });
        // The launcher takes the alias away from any other item that had it.
        if let Some(alias) = &alias {
            let items = self.items.iter_mut().map(|item| &mut item.shortcut);
            for shortcut in self.shortcuts.iter_mut().chain(items) {
                if shortcut
                    .alias
                    .as_ref()
                    .is_some_and(|other| other.eq_ignore_ascii_case(alias))
                {
                    shortcut.alias = None;
                }
            }
        }
        self.set_local(&item_id, |shortcut| shortcut.alias = alias.clone());
        cx.notify();
    }

    /// Changes the local copies right away. A row left with neither an alias
    /// nor a hotkey stays until the next refresh, so a slip can be undone in
    /// place; an item that just got its first one joins the Shortcuts page.
    fn set_local(&mut self, item_id: &str, change: impl Fn(&mut Shortcut)) {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.shortcut.item_id == item_id);
        if let Some(item) = item {
            change(&mut item.shortcut);
        }
        match self
            .shortcuts
            .iter_mut()
            .find(|shortcut| shortcut.item_id == item_id)
        {
            Some(shortcut) => change(shortcut),
            None => {
                if let Some(item) = self
                    .items
                    .iter()
                    .find(|item| item.shortcut.item_id == item_id)
                {
                    self.shortcuts.push(item.shortcut.clone());
                    self.shortcuts
                        .sort_by_cached_key(|shortcut| shortcut.title.to_lowercase());
                }
            }
        }
    }

    fn choose_engine(&mut self, engine: SearchEngine, cx: &mut Context<Self>) {
        self.custom_engine = None;
        self.engine_message = None;
        if engine != self.search_engine {
            self.search_engine = engine.clone();
            cx.emit(SettingsEvent::SearchEngineChanged(engine));
        }
        cx.notify();
    }

    /// Shows the custom URL field and puts the cursor in it. The engine only
    /// changes once a valid URL is saved with Enter.
    fn edit_custom_engine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self
            .custom_engine
            .get_or_insert_with(|| custom_engine_input("https://", cx))
            .clone();
        window.focus(&input.focus_handle(cx));
        self.engine_message = None;
        cx.notify();
    }

    fn save_custom_engine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = &self.custom_engine else {
            return;
        };
        let template = input.read(cx).text().trim().to_owned();
        self.engine_message = Some(match web::check_template(&template) {
            Ok(()) => {
                let engine = SearchEngine::Custom(template);
                let name = engine.name();
                self.search_engine = engine.clone();
                cx.emit(SettingsEvent::SearchEngineChanged(engine));
                window.focus(&self.focus);
                (format!("Searching {name} from now on.").into(), false)
            }
            Err(why) => (why.into(), true),
        });
        cx.notify();
    }

    /// The engine row: one chip per engine, and the custom URL field.
    fn engine_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let custom_selected =
            matches!(self.search_engine, SearchEngine::Custom(_)) || self.custom_engine.is_some();
        let presets = web::PRESETS.iter().enumerate().map(|(ix, preset)| {
            let selected = !custom_selected && self.search_engine == SearchEngine::Preset(preset);
            chip(("engine", ix), preset.name, selected).on_click(cx.listener(
                move |this, _, _, cx| this.choose_engine(SearchEngine::Preset(preset), cx),
            ))
        });
        let custom = chip("engine-custom", "Custom…", custom_selected)
            .on_click(cx.listener(|this, _, window, cx| this.edit_custom_engine(window, cx)));
        // Chosen in the launcher; picking anything else here replaces it.
        let quicklink = (matches!(self.search_engine, SearchEngine::Quicklink(_))
            && !custom_selected)
            .then(|| chip("engine-quicklink", self.engine_name.clone(), true));
        let hint = div()
            .text_size(px(11.))
            .text_color(rgba(theme::TEXT_FAINT))
            .child("A quicklink with {query} can search too: Ctrl+K on it in the launcher, then Use as Web Search.");
        let field = self.custom_engine.as_ref().map(|input| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .h(px(30.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(rgba(theme::SURFACE_BORDER))
                        .bg(rgba(theme::SURFACE))
                        .text_size(px(13.))
                        .child(input.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgba(theme::TEXT_FAINT))
                        .child("A results page URL with {query} where the search text goes. Enter saves."),
                )
        });
        let message = self.engine_message.clone().map(|(text, is_error)| {
            div()
                .text_size(px(12.))
                .text_color(rgba(if is_error {
                    theme::ERROR
                } else {
                    theme::SUCCESS
                }))
                .child(text)
        });
        div()
            .px_4()
            .pb_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1p5()
                    .children(presets)
                    .children(quicklink)
                    .child(custom),
            )
            .children(field)
            .children(message)
            .when(self.custom_engine.is_none(), |row| row.child(hint))
    }

    fn toggle_smooth_scrolling(&mut self, cx: &mut Context<Self>) {
        self.smooth_scrolling = !self.smooth_scrolling;
        self.nav_scroll.set_enabled(self.smooth_scrolling);
        self.page_scroll.set_enabled(self.smooth_scrolling);
        cx.emit(SettingsEvent::SmoothScrollingChanged(self.smooth_scrolling));
        cx.notify();
    }

    fn toggle_rank_by_usage(&mut self, cx: &mut Context<Self>) {
        self.rank_by_usage = !self.rank_by_usage;
        cx.emit(SettingsEvent::RankByUsageChanged(self.rank_by_usage));
        cx.notify();
    }

    fn set_window_mode(&mut self, mode: WindowMode, cx: &mut Context<Self>) {
        if mode != self.window_mode {
            self.window_mode = mode;
            cx.emit(SettingsEvent::WindowModeChanged(mode));
            cx.notify();
        }
    }

    /// The two window modes side by side, each a small picture of the window.
    fn window_mode_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let choices = [WindowMode::Compact, WindowMode::Expanded].map(|mode| {
            let selected = mode == self.window_mode;
            let (id, label) = match mode {
                WindowMode::Compact => ("window-compact", "Compact"),
                WindowMode::Expanded => ("window-expanded", "Expanded"),
            };
            // The launcher in miniature: a search bar, or a whole window.
            let window = div()
                .w(px(if mode == WindowMode::Compact {
                    76.
                } else {
                    92.
                }))
                .h(px(if mode == WindowMode::Compact { 9. } else { 50. }))
                .rounded(px(3.))
                .border_1()
                .border_color(rgba(theme::SURFACE_BORDER))
                .bg(rgba(theme::PANEL))
                .flex()
                .flex_col()
                .child(
                    div().h(px(7.)).px(px(4.)).flex().items_center().child(
                        div()
                            .w(px(24.))
                            .h(px(2.))
                            .rounded_full()
                            .bg(rgba(theme::TEXT_FAINT)),
                    ),
                )
                .when(mode == WindowMode::Expanded, |window| {
                    window
                        .child(div().h(px(1.)).bg(rgba(theme::HAIRLINE)))
                        .children([40., 52., 34.].map(|width| {
                            div().h(px(8.)).px(px(4.)).flex().items_center().child(
                                div()
                                    .w(px(width))
                                    .h(px(2.))
                                    .rounded_full()
                                    .bg(rgba(theme::SURFACE_HOVER)),
                            )
                        }))
                });
            div()
                .id(id)
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_window_mode(mode, cx)))
                .child(
                    div()
                        .w(px(132.))
                        .h(px(76.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .border_2()
                        .border_color(rgba(if selected {
                            theme::ACCENT
                        } else {
                            theme::SURFACE_BORDER
                        }))
                        .bg(rgba(if selected {
                            theme::ACCENT_SOFT
                        } else {
                            theme::SURFACE
                        }))
                        .hover(|style| style.bg(rgba(theme::SURFACE_HOVER)))
                        .child(window),
                )
                .child(
                    div()
                        .px_2()
                        .rounded(px(5.))
                        .text_size(px(12.))
                        .line_height(px(20.))
                        .when(selected, |label| label.bg(rgba(theme::SURFACE_HOVER)))
                        .text_color(rgba(if selected {
                            theme::TEXT
                        } else {
                            theme::TEXT_MUTED
                        }))
                        .child(label),
                )
        });
        div().flex_none().flex().gap_3().children(choices)
    }

    fn toggle_source(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(source) = self.sources.get_mut(ix) else {
            return;
        };
        source.enabled = !source.enabled;
        cx.emit(SettingsEvent::SourceToggled {
            id: source.id,
            enabled: source.enabled,
        });
        cx.notify();
    }

    fn set_clipboard_retention(&mut self, retention: Retention, cx: &mut Context<Self>) {
        if retention != self.clipboard_retention {
            self.clipboard_retention = retention;
            cx.emit(SettingsEvent::ClipboardRetentionChanged(retention));
            cx.notify();
        }
    }

    fn toggle_selection_copy(&mut self, cx: &mut Context<Self>) {
        self.selection_copy = !self.selection_copy;
        cx.emit(SettingsEvent::SelectionCopyChanged(self.selection_copy));
        cx.notify();
    }

    /// Whether the Selected Text source is on, which reading it depends on.
    fn selection_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self
            .sources
            .iter()
            .any(|source| source.id == "selection" && source.enabled);
        let description = if on {
            "Apps that don't share their selection get Ctrl+C; the clipboard is put back right after, and terminals are skipped."
        } else {
            "The extension is off, so the selection is never read."
        };
        card().child(
            row("Copy the selection when needed", description).child(switch(
                "selection-copy",
                on && self.selection_copy,
                cx.listener(|this, _, _, cx| this.toggle_selection_copy(cx)),
            )),
        )
    }

    fn toggle_win_v(&mut self, cx: &mut Context<Self>) {
        self.win_v = !self.win_v;
        cx.emit(SettingsEvent::WinVChanged(self.win_v));
        cx.notify();
    }

    fn unignore_app(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.ignored_apps.len() {
            self.ignored_apps.remove(ix);
            cx.emit(SettingsEvent::IgnoredAppsChanged(self.ignored_apps.clone()));
            cx.notify();
        }
    }

    /// One row per ignored app, with a button that keeps its copies again.
    fn ignored_app_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.ignored_apps.is_empty() {
            return div()
                .px_4()
                .pb_3()
                .text_size(px(12.))
                .text_color(rgba(theme::TEXT_FAINT))
                .child("None. To add one, press Ctrl+K on an entry copied from it and choose Ignore Copies from….")
                .into_any_element();
        }
        div()
            .px_4()
            .pb_3()
            .flex()
            .flex_col()
            .gap_1()
            .children(self.ignored_apps.iter().enumerate().map(|(ix, app)| {
                app_row(app).child(remove_button(
                    ("unignore", ix),
                    cx.listener(move |this, _, _, cx| this.unignore_app(ix, cx)),
                ))
            }))
            .into_any_element()
    }

    fn toggle_snippet_expansion(&mut self, cx: &mut Context<Self>) {
        self.snippet_expansion = !self.snippet_expansion;
        cx.emit(SettingsEvent::SnippetExpansionChanged(
            self.snippet_expansion,
        ));
        cx.notify();
    }

    /// Shows the apps with windows open to pick from, or hides them.
    fn toggle_app_choices(&mut self, cx: &mut Context<Self>) {
        self.app_choices = match self.app_choices {
            Some(_) => None,
            None => Some(win::apps_with_windows()),
        };
        cx.notify();
    }

    fn exclude_app(&mut self, app: IgnoredApp, cx: &mut Context<Self>) {
        if !IgnoredApp::contains(&self.excluded_apps, &app.path) {
            self.excluded_apps.push(app);
            cx.emit(SettingsEvent::ExcludedAppsChanged(
                self.excluded_apps.clone(),
            ));
        }
        self.app_choices = None;
        cx.notify();
    }

    fn unexclude_app(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.excluded_apps.len() {
            self.excluded_apps.remove(ix);
            cx.emit(SettingsEvent::ExcludedAppsChanged(
                self.excluded_apps.clone(),
            ));
            cx.notify();
        }
    }

    /// Whether keywords expand, and the apps where they don't.
    fn snippets_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self
            .sources
            .iter()
            .any(|source| source.id == "snippets" && source.enabled);
        let description = if on {
            "Type a snippet's keyword in any app to replace it with the snippet."
        } else {
            "The extension is off, so keywords never expand."
        };
        let excluded: AnyElement = if self.excluded_apps.is_empty() {
            div()
                .px_4()
                .pb_3()
                .text_size(px(12.))
                .text_color(rgba(theme::TEXT_FAINT))
                .child("None.")
                .into_any_element()
        } else {
            div()
                .px_4()
                .pb_3()
                .flex()
                .flex_col()
                .gap_1()
                .children(self.excluded_apps.iter().enumerate().map(|(ix, app)| {
                    app_row(app).child(remove_button(
                        ("unexclude", ix),
                        cx.listener(move |this, _, _, cx| this.unexclude_app(ix, cx)),
                    ))
                }))
                .into_any_element()
        };
        let choices = self.app_choices.as_ref().map(|apps| {
            let apps: Vec<&IgnoredApp> = apps
                .iter()
                .filter(|app| !IgnoredApp::contains(&self.excluded_apps, &app.path))
                .collect();
            let body: AnyElement = if apps.is_empty() {
                div()
                    .text_size(px(12.))
                    .text_color(rgba(theme::TEXT_FAINT))
                    .child("No other apps have a window open.")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1p5()
                    .children(apps.into_iter().enumerate().map(|(ix, app)| {
                        let chosen = app.clone();
                        chip(("exclude-choice", ix), app.name.clone(), false).on_click(
                            cx.listener(move |this, _, _, cx| this.exclude_app(chosen.clone(), cx)),
                        )
                    }))
                    .into_any_element()
            };
            div()
                .px_4()
                .pb_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgba(theme::TEXT_MUTED))
                        .child("Apps with a window open; click one to add it."),
                )
                .child(body)
        });
        card()
            .child(
                row("Expand keywords while typing", description).child(switch(
                    "snippet-expansion",
                    on && self.snippet_expansion,
                    cx.listener(|this, _, _, cx| this.toggle_snippet_expansion(cx)),
                )),
            )
            .child(divider())
            .child(
                row(
                    "Don't expand in",
                    "Typing a keyword in these apps leaves it as it is.",
                )
                .child(
                    chip("add-excluded", "Add app…", self.app_choices.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_app_choices(cx))),
                ),
            )
            .children(choices)
            .child(excluded)
    }

    fn change_file_search(
        &mut self,
        change: impl FnOnce(&mut FileSearchConfig),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.file_search);
        cx.emit(SettingsEvent::FileSearchChanged(self.file_search.clone()));
        cx.notify();
    }

    /// Asks for a folder and indexes it too.
    fn add_file_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Index".into()),
        });
        cx.spawn(async move |this, cx| {
            let folder = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    eprintln!("[settings] the folder dialog failed: {err}");
                    None
                }
                _ => None,
            };
            let Some(folder) = folder else {
                return;
            };
            this.update(cx, |this, cx| {
                if !this.file_search.folders.contains(&folder) {
                    this.change_file_search(|config| config.folders.push(folder), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn remove_file_folder(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.file_search.folders.len() {
            self.change_file_search(|config| drop(config.folders.remove(ix)), cx);
        }
    }

    fn add_exclusion(&mut self, cx: &mut Context<Self>) {
        let rule = self.exclusion_input.read(cx).text().trim().to_owned();
        if rule.is_empty() {
            return;
        }
        self.exclusion_input.update(cx, |input, cx| input.clear(cx));
        if !self
            .file_search
            .exclusions
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&rule))
        {
            self.change_file_search(|config| config.exclusions.push(rule), cx);
        }
    }

    /// Takes back a busy folder: indexed again, and never left out
    /// automatically again.
    fn keep_busy_folder(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.file_search.hot.len() {
            self.change_file_search(
                |config| {
                    let folder = config.hot.remove(ix);
                    config.kept.push(folder);
                },
                cx,
            );
        }
    }

    fn remove_exclusion(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.file_search.exclusions.len() {
            self.change_file_search(|config| drop(config.exclusions.remove(ix)), cx);
        }
    }

    /// Installs or removes the index service: Windows asks for an
    /// administrator first. Indexing starts again after, with or without it.
    fn change_service(&mut self, install: bool, cx: &mut Context<Self>) {
        if self.service_busy {
            return;
        }
        self.service_busy = true;
        self.service_message = None;
        let job = cx.background_executor().spawn(async move {
            if install {
                index_service::install()
            } else {
                index_service::uninstall()
            }
        });
        cx.spawn(async move |this, cx| {
            let outcome = job.await;
            this.update(cx, |this, cx| {
                this.service_busy = false;
                this.service_message = Some(match outcome {
                    Ok(()) if install => (
                        "Installed. Search Files now reads NTFS drives through it.".into(),
                        false,
                    ),
                    Ok(()) => ("Removed. Search Files walks folders again.".into(), false),
                    Err(err) => (err.into(), true),
                });
                cx.emit(SettingsEvent::FileSearchRebuild);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Folders left out for changing all the time, each with a button to
    /// take it back; nothing while there are none.
    fn busy_folders(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.file_search.hot.is_empty() {
            return None;
        }
        let rows = self.file_search.hot.iter().enumerate().map(|(ix, folder)| {
            app_row(&IgnoredApp {
                name: folder
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: folder.display().to_string(),
            })
            .child(
                chip(("keep-busy", ix), "Stop excluding", false)
                    .on_click(cx.listener(move |this, _, _, cx| this.keep_busy_folder(ix, cx))),
            )
        });
        Some(
            div()
                .child(divider())
                .child(row(
                    "Left out automatically",
                    "These folders changed all the time (caches, logs, build output), so they aren't indexed.",
                ))
                .child(div().px_4().pb_3().flex().flex_col().gap_1().children(rows))
                .into_any_element(),
        )
    }

    /// The index service's row: what it does, its state and a button.
    fn service_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = index_service::state();
        let available = index_service::service_exe().is_some();
        let description = match (state, available) {
            (index_service::State::Running, _) => {
                "Running: NTFS drives are read from their file table and change journal, and changes are kept across restarts."
            }
            (index_service::State::Stopped, _) => {
                "Installed but not running; Search Files walks folders meanwhile."
            }
            (index_service::State::NotInstalled, true) => {
                "A Windows service that reads NTFS drives' file tables and change journals, which needs an administrator once. It tells Spotlight only about folders you can open yourself."
            }
            (index_service::State::NotInstalled, false) => {
                "spotlight-service.exe isn't next to Spotlight, so it can't be installed from here."
            }
        };
        let button = match state {
            _ if self.service_busy => Some(chip("service", "Waiting…", false)),
            index_service::State::NotInstalled if available => Some(
                chip("service", "Install…", false)
                    .on_click(cx.listener(|this, _, _, cx| this.change_service(true, cx))),
            ),
            index_service::State::NotInstalled => None,
            _ if available => Some(
                chip("service", "Remove…", false)
                    .on_click(cx.listener(|this, _, _, cx| this.change_service(false, cx))),
            ),
            _ => None,
        };
        let message = self.service_message.clone().map(|(text, is_error)| {
            div()
                .px_4()
                .pb_3()
                .text_size(px(12.))
                .text_color(rgba(if is_error {
                    theme::ERROR
                } else {
                    theme::SUCCESS
                }))
                .child(text)
        });
        div()
            .child(row("Fast indexing service", description).children(button))
            .children(message)
            .into_any_element()
    }

    /// Keeps the File Search page's count moving while indexing, and stops
    /// once it's done.
    fn refresh_status_while_indexing(&mut self, cx: &mut Context<Self>) {
        if self.status_refresh.is_some()
            || !matches!((self.file_status)(), FileSearchStatus::Indexing(_))
        {
            return;
        }
        self.status_refresh = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_REFRESH).await;
                let indexing = this.update(cx, |this, cx| {
                    cx.notify();
                    matches!((this.file_status)(), FileSearchStatus::Indexing(_))
                });
                if !matches!(indexing, Ok(true)) {
                    break;
                }
            }
            this.update(cx, |this, _| this.status_refresh = None).ok();
        }));
    }

    /// What is indexed, what is left out, and how far indexing has got.
    fn files_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self
            .sources
            .iter()
            .any(|source| source.id == FILES_PAGE && source.enabled);
        let status = if on {
            (self.file_status)().describe()
        } else {
            "The extension is off, so nothing is indexed.".to_owned()
        };
        let folders = file_search::profile()
            .map(|profile| (profile, false))
            .into_iter()
            .chain(
                self.file_search
                    .folders
                    .iter()
                    .cloned()
                    .map(|folder| (folder, true)),
            )
            .enumerate()
            .map(|(ix, (folder, removable))| {
                let name = folder
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| folder.display().to_string());
                let row = app_row(&IgnoredApp {
                    name: if removable {
                        name
                    } else {
                        format!("{name} (your user folder)")
                    },
                    path: folder.display().to_string(),
                });
                if removable {
                    row.child(remove_button(
                        ("remove-folder", ix),
                        cx.listener(move |this, _, _, cx| this.remove_file_folder(ix - 1, cx)),
                    ))
                } else {
                    row
                }
            });
        let rules = if self.file_search.exclusions.is_empty() {
            div()
                .text_size(px(12.))
                .text_color(rgba(theme::TEXT_FAINT))
                .child("Nothing is left out.")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap_1p5()
                .children(
                    self.file_search
                        .exclusions
                        .iter()
                        .enumerate()
                        .map(|(ix, rule)| {
                            div()
                                .h(px(28.))
                                .pl_3()
                                .pr_1()
                                .flex()
                                .items_center()
                                .gap_1()
                                .rounded_full()
                                .border_1()
                                .border_color(rgba(theme::SURFACE_BORDER))
                                .text_size(px(12.))
                                .text_color(rgba(theme::TEXT_MUTED))
                                .child(rule.clone())
                                .child(remove_button(
                                    ("remove-exclusion", ix),
                                    cx.listener(move |this, _, _, cx| {
                                        this.remove_exclusion(ix, cx)
                                    }),
                                ))
                        }),
                )
                .into_any_element()
        };
        let defaults = FileSearchConfig::default().exclusions;
        card()
            .child(
                row("Index", status).child(chip("rebuild-index", "Rebuild", false).on_click(
                    cx.listener(|_, _, _, cx| {
                        cx.emit(SettingsEvent::FileSearchRebuild);
                    }),
                )),
            )
            .child(divider())
            .child(
                row(
                    "Folders",
                    "Your user folder is always searched. Add other folders or whole drives.",
                )
                .child(
                    chip("add-folder", "Add Folder…", false)
                        .on_click(cx.listener(|this, _, _, cx| this.add_file_folder(cx))),
                ),
            )
            .child(
                div()
                    .px_4()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(folders),
            )
            .child(divider())
            .child(
                row(
                    "Leave out",
                    "Folders with these names, anywhere, and these paths with everything in them.",
                )
                .when(self.file_search.exclusions != defaults, |row| {
                    row.child(
                        chip("default-exclusions", "Restore Defaults", false).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let defaults = FileSearchConfig::default().exclusions;
                                this.change_file_search(|config| config.exclusions = defaults, cx)
                            }),
                        ),
                    )
                }),
            )
            .child(
                div()
                    .px_4()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(rules)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(30.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(rgba(theme::SURFACE_BORDER))
                                    .bg(rgba(theme::SURFACE))
                                    .text_size(px(13.))
                                    .child(self.exclusion_input.clone()),
                            )
                            .child(
                                chip("add-exclusion", "Add", false)
                                    .on_click(cx.listener(|this, _, _, cx| this.add_exclusion(cx))),
                            ),
                    ),
            )
            .children(self.busy_folders(cx))
            .child(divider())
            .child(
                row(
                    "In root search",
                    "Start a search with this to find files right there; Tab goes on in Search Files. Empty turns it off.",
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(64.))
                        .h(px(30.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(rgba(theme::SURFACE_BORDER))
                        .bg(rgba(theme::SURFACE))
                        .text_size(px(13.))
                        .child(self.prefix_input.clone()),
                ),
            )
            .child(divider())
            .child(self.service_row(cx))
            .child(divider())
            .child(
                row(
                    "Hidden files",
                    "Also index files and folders marked hidden, and names starting with a dot.",
                )
                .child(switch(
                    "hidden-files",
                    self.file_search.include_hidden,
                    cx.listener(|this, _, _, cx| {
                        this.change_file_search(
                            |config| config.include_hidden = !config.include_hidden,
                            cx,
                        )
                    }),
                )),
            )
    }

    /// How long and how many entries to keep: a row of choices for each.
    fn clipboard_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.clipboard_retention;
        let days = clip::KEEP_DAYS
            .iter()
            .enumerate()
            .map(|(ix, &(days, label))| {
                chip(("keep-days", ix), label, current.days == days).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let retention = Retention {
                            days,
                            ..this.clipboard_retention
                        };
                        this.set_clipboard_retention(retention, cx)
                    },
                ))
            });
        let max = clip::KEEP_MAX
            .iter()
            .enumerate()
            .map(|(ix, &(max, label))| {
                chip(("keep-max", ix), label, current.max == max).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let retention = Retention {
                            max,
                            ..this.clipboard_retention
                        };
                        this.set_clipboard_retention(retention, cx)
                    },
                ))
            });
        let choices = |chips: Vec<Stateful<gpui::Div>>| {
            div()
                .px_4()
                .pb_3()
                .flex()
                .flex_wrap()
                .gap_1p5()
                .children(chips)
        };
        card()
            .child(row(
                "Keep for",
                "Older entries are deleted, pinned ones never. Passwords from apps that mark them private are never kept.",
            ))
            .child(choices(days.collect()))
            .child(divider())
            .child(row(
                "Keep at most",
                "The oldest entries go first once the history is full.",
            ))
            .child(choices(max.collect()))
            .child(divider())
            .child(
                row(
                    "Open with Win+V",
                    "Instead of Windows' own clipboard history. Turned off, Win+V is Windows' again.",
                )
                .child(switch(
                    "win-v",
                    self.win_v,
                    cx.listener(|this, _, _, cx| this.toggle_win_v(cx)),
                )),
            )
            .child(divider())
            .child(row(
                "Ignored apps",
                "Copies made in these apps are never kept.",
            ))
            .child(self.ignored_app_list(cx))
    }

    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = autostart::set_enabled(!self.autostart) {
            eprintln!("[settings] could not change start at login: {err}");
        }
        self.autostart = autostart::is_enabled();
        cx.emit(SettingsEvent::AutostartChanged);
        cx.notify();
    }

    /// Esc: clears the search, cancels an alias or custom URL edit, otherwise
    /// closes the window.
    fn close(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.focus_handle(cx).is_focused(window) && !self.query(cx).is_empty() {
            return self.search.update(cx, |search, cx| search.clear(cx));
        }
        let editing_engine = self
            .custom_engine
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        if editing_engine {
            // Back to what is in use: a custom engine keeps its field.
            self.custom_engine = match &self.search_engine {
                SearchEngine::Custom(template) => Some(custom_engine_input(template, cx)),
                SearchEngine::Preset(_) | SearchEngine::Quicklink(_) => None,
            };
            self.engine_message = None;
            window.focus(&self.focus);
            cx.notify();
        } else if self.editing_alias.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        } else {
            window.remove_window();
        }
    }

    fn hotkey_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let recording = self.recording.is_some() && self.recording_for.is_none();
        let content: gpui::AnyElement = if recording {
            div()
                .text_color(rgba(theme::ACCENT))
                .child("Press a shortcut…")
                .into_any_element()
        } else if let Some(hotkey) = self.hotkey {
            div()
                .flex()
                .gap_1()
                .children(
                    hotkey
                        .to_string()
                        .split('+')
                        .map(|part| keycap(part.to_owned())),
                )
                .into_any_element()
        } else {
            div()
                .text_color(rgba(theme::TEXT_MUTED))
                .child("Record shortcut")
                .into_any_element()
        };

        div()
            .id("recorder")
            .track_focus(&self.recorder)
            .on_click(cx.listener(|this, _, window, cx| this.start_recording(None, window, cx)))
            .min_w(px(150.))
            .h(px(34.))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .border_1()
            .cursor_pointer()
            .when(recording, |el| {
                el.border_color(rgba(theme::ACCENT))
                    .bg(rgba(theme::ACCENT_SOFT))
            })
            .when(!recording, |el| {
                el.border_color(rgba(theme::SURFACE_BORDER))
                    .hover(|style| style.bg(rgba(theme::SURFACE_HOVER)))
            })
            .child(content)
    }
}

impl Settings {
    /// The list of aliases and item hotkeys, one row per item.
    fn shortcut_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.shortcuts.is_empty() {
            return card()
                .px_4()
                .py_3()
                .text_size(px(12.))
                .text_color(rgba(theme::TEXT_MUTED))
                .child(
                    "None yet. In the launcher, select an app or command, press Ctrl+K and \
                     choose Set Alias… or Set Hotkey….",
                )
                .into_any_element();
        }
        let rows = self
            .shortcuts
            .iter()
            .enumerate()
            .map(|(ix, shortcut)| self.shortcut_row(ix, shortcut, None, None, cx));
        card().children(rows).into_any_element()
    }

    /// An item's row: its icon if given, title and subtitle, then the alias
    /// and hotkey cells and the outcome of the last change under them.
    fn shortcut_row(
        &self,
        ix: usize,
        shortcut: &Shortcut,
        icon: Option<AnyElement>,
        subtitle: Option<&str>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let message = self
            .shortcut_message
            .as_ref()
            .filter(|(id, _, _)| *id == shortcut.item_id)
            .map(|(_, text, is_error)| {
                div()
                    .px_4()
                    .pb_2()
                    .text_size(px(12.))
                    .text_color(rgba(if *is_error {
                        theme::ERROR
                    } else {
                        theme::SUCCESS
                    }))
                    .child(text.clone())
            });
        let subtitle = subtitle.map(|subtitle| {
            div()
                .min_w_0()
                .truncate()
                .text_size(px(12.))
                .text_color(rgba(theme::TEXT_FAINT))
                .child(SharedString::from(subtitle.to_owned()))
        });
        div()
            .flex()
            .flex_col()
            .when(ix > 0, |row| row.child(divider()))
            .child(
                div()
                    .min_h(px(48.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .children(icon)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(shortcut.title.clone())),
                            )
                            .children(subtitle),
                    )
                    .child(self.alias_cell(ix, shortcut, cx))
                    .child(self.hotkey_cell(ix, shortcut, cx)),
            )
            .children(message)
    }

    fn alias_cell(&self, ix: usize, shortcut: &Shortcut, cx: &mut Context<Self>) -> AnyElement {
        if let Some((_, input)) = self
            .editing_alias
            .as_ref()
            .filter(|(id, _)| *id == shortcut.item_id)
        {
            return div()
                .flex_none()
                .w(px(130.))
                .h(px(30.))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(6.))
                .border_1()
                .border_color(rgba(theme::ACCENT))
                .bg(rgba(theme::ACCENT_SOFT))
                .text_size(px(13.))
                .child(input.clone())
                .into_any_element();
        }
        let item_id = shortcut.item_id.clone();
        let label: AnyElement = match &shortcut.alias {
            Some(alias) => div()
                .flex_1()
                .truncate()
                .child(SharedString::from(alias.clone()))
                .into_any_element(),
            None => div()
                .flex_1()
                .text_color(rgba(theme::TEXT_FAINT))
                .child("Add alias")
                .into_any_element(),
        };
        let remove_id = shortcut.item_id.clone();
        cell(("alias", ix))
            .on_click(
                cx.listener(move |this, _, window, cx| {
                    this.edit_alias(item_id.clone(), window, cx)
                }),
            )
            .child(label)
            .when(shortcut.alias.is_some(), |cell| {
                cell.child(remove_button(
                    ("remove-alias", ix),
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_alias(remove_id.clone(), None, cx)
                    }),
                ))
            })
            .into_any_element()
    }

    fn hotkey_cell(&self, ix: usize, shortcut: &Shortcut, cx: &mut Context<Self>) -> AnyElement {
        let recording =
            self.recording.is_some() && self.recording_for.as_ref() == Some(&shortcut.item_id);
        let label: AnyElement = if recording {
            div()
                .flex_1()
                .text_color(rgba(theme::ACCENT))
                .child("Press keys…")
                .into_any_element()
        } else if let Some(hotkey) = shortcut.hotkey {
            div()
                .flex_1()
                .flex()
                .gap_1()
                .children(
                    hotkey
                        .to_string()
                        .split('+')
                        .map(|part| keycap(part.to_owned())),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .text_color(rgba(theme::TEXT_FAINT))
                .child("Record hotkey")
                .into_any_element()
        };
        let item_id = shortcut.item_id.clone();
        let remove_id = shortcut.item_id.clone();
        cell(("hotkey", ix))
            .w(px(170.))
            .when(recording, |cell| {
                cell.border_color(rgba(theme::ACCENT))
                    .bg(rgba(theme::ACCENT_SOFT))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_recording(Some(item_id.clone()), window, cx)
            }))
            .child(label)
            .when(shortcut.hotkey.is_some() && !recording, |cell| {
                cell.child(remove_button(
                    ("remove-hotkey", ix),
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_item_hotkey(remove_id.clone(), cx)
                    }),
                ))
            })
            .into_any_element()
    }
}

impl Drop for Settings {
    /// Closing the window mid-recording must not leave the hotkey unregistered.
    fn drop(&mut self) {
        if self.recording.is_some() {
            self.hotkeys.resume();
        }
    }
}

impl Focusable for Settings {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A page of the window, picked in the sidebar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Extensions,
    Shortcuts,
    About,
    /// The settings of the search provider with this id.
    Extension(&'static str),
}

/// An item row in an extension's list and the line above it. Rows are all
/// this tall, which is what lets the list build only those on screen.
const ITEM_ROW_STRIDE: f32 = 49.;
/// Rows built past each edge of the page, so a fast scroll shows no gap
/// before the next frame catches up.
const OVERSCAN_ROWS: usize = 8;

/// How many pixels of scrolling the top bar's title takes to fade in.
const TITLE_FADE: f32 = 16.;

/// The pages above the extensions in the sidebar.
const PAGES: [Page; 4] = [
    Page::General,
    Page::Extensions,
    Page::Shortcuts,
    Page::About,
];

// Icon colours, after Raycast's.
const RED: u32 = 0xff5f57ff;
const ORANGE: u32 = 0xff9f0aff;
const YELLOW: u32 = 0xe5b000ff;
const GREEN: u32 = 0x30b158ff;
const TEAL: u32 = 0x30a8c8ff;
const BLUE: u32 = 0x0a84ffff;
const INDIGO: u32 = 0x5e5ce6ff;
const PURPLE: u32 = 0xaf52deff;
const PINK: u32 = 0xff375fff;
const GRAY: u32 = 0x6e6e73ff;

impl Page {
    /// Its sidebar icon: a Segoe Fluent Icons glyph and the badge colour.
    fn icon(self) -> (char, u32) {
        match self {
            Page::General => ('\u{E713}', GRAY),
            Page::Extensions => ('\u{EA86}', INDIGO),
            Page::Shortcuts => ('\u{E765}', GRAY),
            Page::About => ('\u{E946}', GRAY),
            Page::Extension(id) => match id {
                "apps" => ('\u{E71D}', BLUE),
                "calc" => ('\u{E8EF}', ORANGE),
                "clipboard" => ('\u{E77F}', RED),
                "control-panel" => ('\u{E770}', TEAL),
                FILES_PAGE => ('\u{E8B7}', YELLOW),
                "games" => ('\u{E7FC}', GREEN),
                "quicklinks" => ('\u{E71B}', PINK),
                "selection" => ('\u{E8B3}', YELLOW),
                "settings" => ('\u{E713}', GRAY),
                "snippets" => ('\u{E943}', PURPLE),
                "spotlight" => ('\u{E721}', INDIGO),
                "system" => ('\u{E7E8}', RED),
                "web" => ('\u{E774}', BLUE),
                _ => ('\u{EA86}', GRAY),
            },
        }
    }

    /// How far down a page with a large centred header its title ends, so
    /// the top bar shows the title once the page scrolls past it.
    fn header_height(self) -> Option<f32> {
        match self {
            Page::Extension(_) => Some(120.),
            Page::About => Some(145.),
            Page::General | Page::Extensions | Page::Shortcuts => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Page::General => "general",
            Page::Extensions => "extensions",
            Page::Shortcuts => "shortcuts",
            Page::About => "about",
            Page::Extension(id) => id,
        }
    }

    /// Settings on the page, so the search finds the page by them.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            Page::General => &[
                "Start at login",
                "Launcher hotkey",
                "Appearance",
                "Smooth scrolling",
                "Search",
                "Rank by usage",
                "Frecency",
            ],
            Page::Extensions => &["Search sources", "Enabled"],
            Page::Shortcuts => &["Aliases", "Hotkeys"],
            Page::About => &[
                "Version",
                "Updates",
                "Changelog",
                "Website",
                "GitHub",
                "Feedback",
                "Data folder",
            ],
            Page::Extension("clipboard") => &["Keep for", "Keep at most", "Win+V", "Ignored apps"],
            Page::Extension(FILES_PAGE) => &[
                "Index",
                "Folders",
                "Leave out",
                "Fast indexing service",
                "In root search",
                "Left out automatically",
                "Hidden files",
            ],
            Page::Extension("selection") => &["Copy the selection", "Ctrl+C"],
            Page::Extension("snippets") => &["Expand keywords", "Don't expand in"],
            Page::Extension("web") => &["Search engine", "Custom"],
            Page::Extension(_) => &[],
        }
    }
}

impl Settings {
    fn page(&self) -> Page {
        self.history[self.history_ix]
    }

    fn page_title(&self, page: Page) -> &'static str {
        match page {
            Page::General => "General",
            Page::Extensions => "Extensions",
            Page::Shortcuts => "Shortcuts",
            Page::About => "About",
            Page::Extension(id) => self
                .sources
                .iter()
                .find(|source| source.id == id)
                .map_or("Extension", |source| source.name),
        }
    }

    /// The search text, lowercased and trimmed.
    fn query(&self, cx: &App) -> String {
        self.search.read(cx).text().trim().to_lowercase()
    }

    fn page_matches(&self, page: Page, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let hit = |text: &str| text.to_lowercase().contains(query);
        hit(self.page_title(page))
            || page.keywords().iter().any(|keyword| hit(keyword))
            || match page {
                Page::Extensions => self.sources.iter().any(|source| hit(source.name)),
                Page::Shortcuts => self.shortcuts.iter().any(|shortcut| {
                    hit(&shortcut.title) || shortcut.alias.as_deref().is_some_and(hit)
                }),
                Page::Extension(id) => {
                    self.sources
                        .iter()
                        .any(|source| source.id == id && hit(source.description))
                        || self
                            .items
                            .iter()
                            .any(|item| item.provider == id && item_matches(item, query))
                }
                Page::General | Page::About => false,
            }
    }

    /// Every page, in sidebar order.
    fn pages(&self) -> impl Iterator<Item = Page> + '_ {
        PAGES
            .into_iter()
            .chain(self.sources.iter().map(|source| Page::Extension(source.id)))
    }

    /// Shows the first page the search finds when the one shown doesn't match.
    fn search_changed(&mut self, cx: &mut Context<Self>) {
        self.relayout = true;
        let query = self.query(cx);
        let first = self.pages().find(|&page| self.page_matches(page, &query));
        if !self.page_matches(self.page(), &query)
            && let Some(first) = first
        {
            self.go(first, cx);
        }
        cx.notify();
    }

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if page == self.page() {
            return;
        }
        self.history.truncate(self.history_ix + 1);
        self.history.push(page);
        self.history_ix += 1;
        self.left_page(cx);
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.history_ix > 0 {
            self.history_ix -= 1;
            self.left_page(cx);
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if self.history_ix + 1 < self.history.len() {
            self.history_ix += 1;
            self.left_page(cx);
        }
    }

    /// Ends what was going on in the page left behind, and reads the
    /// launcher again: catalogs still loading when the window opened (apps
    /// take a moment) have their items by now.
    fn left_page(&mut self, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        self.editing_alias = None;
        self.app_choices = None;
        self.page_scroll.reset();
        self.relayout = true;
        self.refresh_from_launcher(cx);
        cx.notify();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query(cx);
        let general: Vec<_> = PAGES
            .into_iter()
            .filter(|&page| self.page_matches(page, &query))
            .map(|page| self.nav_item(page, cx))
            .collect();
        let extensions: Vec<_> = self
            .sources
            .iter()
            .map(|source| Page::Extension(source.id))
            .filter(|&page| self.page_matches(page, &query))
            .map(|page| self.nav_item(page, cx))
            .collect();
        let nothing = general.is_empty() && extensions.is_empty();

        div()
            .flex_none()
            .w(px(264.))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgba(theme::HAIRLINE))
            .bg(rgba(0x00000030))
            .child(
                div()
                    .id("sidebar-drag")
                    .window_control_area(WindowControlArea::Drag)
                    .flex_none()
                    .h(px(48.))
                    .px_5()
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .text_color(rgba(theme::TEXT_MUTED))
                    .child("Settings"),
            )
            .child(
                div()
                    .flex_none()
                    .mx_3()
                    .mb_3()
                    .h(px(36.))
                    .px_2p5()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgba(theme::SURFACE_BORDER))
                    .bg(rgba(theme::SURFACE))
                    .text_size(px(13.))
                    .child(
                        div()
                            .flex_none()
                            .font_family(icon_font())
                            .text_size(px(12.))
                            .text_color(rgba(theme::TEXT_FAINT))
                            .child("\u{E721}"),
                    )
                    .child(div().flex_1().min_w_0().child(self.search.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(self.nav_list(general, extensions, nothing))
                    .child(capture_wheel(
                        cx.entity().downgrade(),
                        |this: &mut Self, delta, cx| wheel(&mut this.nav_scroll, delta, cx),
                    )),
            )
    }

    fn nav_list(
        &self,
        general: Vec<Stateful<gpui::Div>>,
        extensions: Vec<Stateful<gpui::Div>>,
        nothing: bool,
    ) -> impl IntoElement {
        div()
            .id("nav")
            .track_scroll(&self.nav_scroll.handle())
            .size_full()
            .overflow_y_scroll()
            .px_3()
            .pb_3()
            .flex()
            .flex_col()
            .gap_1()
            .children(general)
            .when(!extensions.is_empty(), |nav| {
                nav.child(div().flex_none().h(px(14.))).children(extensions)
            })
            .when(nothing, |nav| {
                nav.child(
                    div()
                        .px_2()
                        .py_2()
                        .text_size(px(12.))
                        .text_color(rgba(theme::TEXT_FAINT))
                        .child("No matching settings."),
                )
            })
    }

    fn nav_item(&self, page: Page, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let (glyph, color) = page.icon();
        let selected = page == self.page();
        div()
            .id(ElementId::Name(format!("nav-{}", page.key()).into()))
            .flex_none()
            .h(px(38.))
            .px_2p5()
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(8.))
            .text_size(px(14.))
            .cursor_pointer()
            .when(selected, |item| item.bg(rgba(theme::SURFACE_HOVER)))
            .when(!selected, |item| {
                item.hover(|style| style.bg(rgba(theme::SURFACE)))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.go(page, cx)))
            .child(badge(glyph, color, 24.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.page_title(page)),
            )
    }

    /// Back, forward, the page title (the drag area), the extension's switch
    /// on its page, and the minimize and close buttons.
    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can_back = self.history_ix > 0;
        let can_forward = self.history_ix + 1 < self.history.len();
        let page = self.page();
        // Pages with a large centred header name themselves there, and here
        // only once it has scrolled away, fading in like Raycast's.
        let title_opacity = match page.header_height() {
            Some(header) => ((self.page_scroll.scrolled() - header) / TITLE_FADE).clamp(0., 1.),
            None => 1.,
        };
        let extension_switch = match page {
            Page::Extension(id) => {
                self.sources
                    .iter()
                    .position(|source| source.id == id)
                    .map(|ix| {
                        switch(
                            ("source", ix),
                            self.sources[ix].enabled,
                            cx.listener(move |this, _, _, cx| this.toggle_source(ix, cx)),
                        )
                        .mr_2()
                    })
            }
            _ => None,
        };
        div()
            .flex_none()
            .h(px(48.))
            .pl_3()
            .pr_2()
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                icon_button("back", '\u{E72B}', can_back)
                    .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
            )
            .child(
                icon_button("forward", '\u{E72A}', can_forward)
                    .on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))),
            )
            .child(
                // Dragging the window works anywhere here but on the buttons.
                div()
                    .id("drag")
                    .window_control_area(WindowControlArea::Drag)
                    .flex_1()
                    .h_full()
                    .pl_2()
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .child(div().opacity(title_opacity).child(self.page_title(page))),
            )
            .children(extension_switch)
            .child(
                icon_button("minimize", '\u{E921}', true)
                    .on_click(|_, window, _| window.minimize_window()),
            )
            .child(
                icon_button("close", '\u{E8BB}', true)
                    .on_click(cx.listener(|this, _, window, cx| this.close(&Close, window, cx))),
            )
    }

    fn general_page(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let hotkey_hint = if self.recording.is_some() && self.recording_for.is_none() {
            "Press the keys now, or Esc to cancel."
        } else {
            "Opens and closes the search window. Click to change."
        };
        let message = self.message.clone().map(|(text, is_error)| {
            div()
                .px_4()
                .pb_3()
                .text_size(px(12.))
                .text_color(rgba(if is_error {
                    theme::ERROR
                } else {
                    theme::SUCCESS
                }))
                .child(text)
        });
        vec![
            card()
                .child(
                    row(
                        "Start at Login",
                        "Run Spotlight in the background when you sign in.",
                    )
                    .child(switch(
                        "autostart",
                        self.autostart,
                        cx.listener(|this, _, _, cx| this.toggle_autostart(cx)),
                    )),
                )
                .child(divider())
                .child(row("Launcher Hotkey", hotkey_hint).child(self.hotkey_control(cx)))
                .children(message)
                .into_any_element(),
            section_title("Appearance"),
            card()
                .child(
                    row(
                        "Window Mode",
                        "Compact shows only the search field until you type or press ↓.",
                    )
                    .py_3()
                    .child(self.window_mode_control(cx)),
                )
                .child(divider())
                .child(
                    row(
                        "Smooth Scrolling",
                        "Animate the result list when you scroll or move the selection.",
                    )
                    .child(switch(
                        "smooth-scrolling",
                        self.smooth_scrolling,
                        cx.listener(|this, _, _, cx| this.toggle_smooth_scrolling(cx)),
                    )),
                )
                .into_any_element(),
            section_title("Search"),
            card()
                .child(
                    row(
                        "Rank by Usage",
                        "List apps and commands you open often first. Turned off, usage is still remembered.",
                    )
                    .child(switch(
                        "rank-by-usage",
                        self.rank_by_usage,
                        cx.listener(|this, _, _, cx| this.toggle_rank_by_usage(cx)),
                    )),
                )
                .into_any_element(),
        ]
    }

    /// Every extension with its switch; a click elsewhere on a row opens its page.
    fn extensions_page(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let count = self.sources.len();
        let rows = self.sources.iter().enumerate().map(|(ix, source)| {
            let page = Page::Extension(source.id);
            let (glyph, color) = page.icon();
            div()
                .flex()
                .flex_col()
                .when(ix > 0, |row| row.child(divider()))
                .child(
                    div()
                        .id(("extension", ix))
                        .min_h(px(54.))
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .cursor_pointer()
                        .map(|row| card_row_corners(row, ix, count))
                        .hover(|style| style.bg(rgba(theme::SURFACE)))
                        .on_click(cx.listener(move |this, _, _, cx| this.go(page, cx)))
                        .child(badge(glyph, color, 26.))
                        .child(text_block(source.name, source.description))
                        .child(switch(
                            ("source", ix),
                            source.enabled,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_source(ix, cx)
                            }),
                        )),
                )
        });
        vec![
            hint(
                "Turned off, an extension adds nothing to the search. Click one for its settings.",
            ),
            card().children(rows).into_any_element(),
        ]
    }

    /// What to do in a list of aliases and hotkeys right now, or `idle`.
    fn shortcuts_hint(&self, idle: &'static str) -> &'static str {
        if self.recording.is_some() && self.recording_for.is_some() {
            "Press the keys now, or Esc to cancel."
        } else if self.editing_alias.is_some() {
            "Enter saves (empty removes the alias), Esc cancels."
        } else {
            idle
        }
    }

    fn shortcuts_page(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let hint_text = self
            .shortcuts_hint("Aliases and hotkeys of apps and commands. Click one to change it.");
        vec![
            hint(hint_text),
            self.shortcut_list(cx),
            div()
                .pt_3()
                .px_1()
                .text_size(px(13.))
                .text_color(rgba(theme::TEXT_MUTED))
                .child("Open from Links Without Asking")
                .into_any_element(),
            self.trusted_link_list(cx),
        ]
    }

    /// The commands `spotlight://` links run without waiting for Enter, each
    /// with a button that makes it ask again.
    fn trusted_link_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.trusted_links.is_empty() {
            return card()
                .px_4()
                .py_3()
                .text_size(px(12.))
                .text_color(rgba(theme::TEXT_MUTED))
                .child(
                    "None. When a link opens a command, press Ctrl+Enter (Always Open Command) \
                     to stop it asking.",
                )
                .into_any_element();
        }
        card()
            .px_4()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                self.trusted_links
                    .iter()
                    .enumerate()
                    .map(|(ix, (id, title))| {
                        let id = id.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(13.))
                                    .truncate()
                                    .child(title.clone()),
                            )
                            .child(remove_button(
                                ("distrust", ix),
                                cx.listener(move |this, _, _, cx| this.distrust_link(&id, cx)),
                            ))
                    }),
            )
            .into_any_element()
    }

    fn distrust_link(&mut self, id: &str, cx: &mut Context<Self>) {
        self.trusted_links.retain(|(trusted, _)| trusted != id);
        cx.emit(SettingsEvent::LinkDistrusted(id.to_owned()));
        cx.notify();
    }

    /// The extension's items with their alias and hotkey cells, like
    /// Raycast's. While the search finds some of them, only those.
    /// `card_ix` is where the list's card lands among the page's children.
    fn extension_items(
        &self,
        id: &'static str,
        card_ix: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let items: Vec<&ExtensionItem> = self
            .items
            .iter()
            .filter(|item| item.provider == id)
            .collect();
        if items.is_empty() {
            return Vec::new();
        }
        let query = self.query(cx);
        let found: Vec<&ExtensionItem> = items
            .iter()
            .copied()
            .filter(|item| !query.is_empty() && item_matches(item, &query))
            .collect();
        let shown = if found.is_empty() { items } else { found };
        // Only the rows on screen are built; spacers stand in for the rest.
        let range = self.visible_rows(card_ix, shown.len());
        let above = range.start as f32 * ITEM_ROW_STRIDE - 1.;
        let below = (shown.len() - range.end) as f32 * ITEM_ROW_STRIDE;
        let rows: Vec<_> = shown[range.clone()]
            .iter()
            .zip(range.clone())
            .map(|(item, ix)| {
                self.shortcut_row(
                    ix,
                    &item.shortcut,
                    Some(item_icon(item)),
                    item.subtitle.as_deref(),
                    cx,
                )
            })
            .collect();
        vec![
            div()
                .pt_3()
                .px_1()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgba(theme::TEXT_MUTED))
                        .child(format!("{} · {}", items_heading(id), shown.len())),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgba(theme::TEXT_FAINT))
                        .child(self.shortcuts_hint("Click an alias or hotkey to change it.")),
                )
                .into_any_element(),
            card()
                .when(range.start > 0, |card| {
                    card.child(div().flex_none().h(px(above)))
                })
                .children(rows)
                .when(range.end < shown.len(), |card| {
                    card.child(div().flex_none().h(px(below)))
                })
                .into_any_element(),
        ]
    }

    /// Which of `count` item rows in the card at `card_ix` are on screen,
    /// plus a few either side. Uses where the card was laid out last frame;
    /// until there is one, enough rows to fill the page from the top.
    fn visible_rows(&self, card_ix: usize, count: usize) -> Range<usize> {
        let handle = self.page_scroll.handle();
        let viewport = handle.bounds();
        let height = f32::from(viewport.size.height).max(720.);
        let on_screen = (height / ITEM_ROW_STRIDE).ceil() as usize + 1;
        let first = handle
            .bounds_for_item(card_ix)
            .map_or(0, |card| {
                // Child bounds don't include the scroll offset: this is the
                // rows' top within the page's content (inside the border).
                let rows_top = f32::from(card.top() - viewport.top()) + 1.;
                let top = self.page_scroll.scrolled() - rows_top;
                (top / ITEM_ROW_STRIDE).floor().max(0.) as usize
            })
            .min(count);
        first.saturating_sub(OVERSCAN_ROWS)..(first + on_screen + OVERSCAN_ROWS).min(count)
    }

    /// The logo, name and version, then updates, links and where the data is.
    fn about_page(&self) -> Vec<AnyElement> {
        let version = format!("v{}", self.version);
        let mut links = vec![
            link_row("website", '\u{E774}', "Website", WEBSITE),
            link_row("github", '\u{E943}', "GitHub", REPOSITORY),
            link_row(
                "feedback",
                '\u{E8BD}',
                "Send Feedback",
                concat!(env!("CARGO_PKG_REPOSITORY"), "/issues/new"),
            ),
        ];
        if let Some(profile) = X_PROFILE {
            links.push(link_row("x", '\u{E8F2}', "Twitter / X", profile));
        }
        let count = links.len();
        let links = links.into_iter().enumerate().map(|(ix, link)| {
            div()
                .flex()
                .flex_col()
                .when(ix > 0, |row| row.child(divider()))
                .child(card_row_corners(link, ix, count))
        });
        vec![
            div()
                .pt_4()
                .pb_2()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(img(logo()).size(px(88.)).flex_none())
                .child(
                    div()
                        .pt_2()
                        .text_size(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Spotlight"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgba(theme::TEXT_MUTED))
                        .child(self.version),
                )
                .child(
                    div()
                        .pt_1()
                        .text_size(px(12.))
                        .text_color(rgba(theme::TEXT_FAINT))
                        .child("Open source under MIT or Apache-2.0."),
                )
                .into_any_element(),
            section_title("Updates"),
            card()
                .child(row("Spotlight Updates", version).child(
                    button("check-updates", "Check now").on_click(|_, _, _| {
                        open_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/releases/latest"))
                    }),
                ))
                .child(divider())
                .child(card_row_corners(
                    link_row(
                        "changelog",
                        '\u{E8A5}',
                        "Open Changelog",
                        concat!(env!("CARGO_PKG_REPOSITORY"), "/releases"),
                    ),
                    1,
                    2,
                ))
                .into_any_element(),
            section_title("Links"),
            card().children(links).into_any_element(),
            section_title("Data"),
            card()
                .child(card_row_corners(
                    row("Data Folder", self.data_path.clone())
                        .id("data-folder")
                        .cursor_pointer()
                        .hover(|style| style.bg(rgba(theme::SURFACE)))
                        .on_click({
                            let path = self.data_path.to_string();
                            move |_, _, _| show_in_explorer(&path)
                        })
                        .child(
                            div()
                                .flex_none()
                                .font_family(icon_font())
                                .text_size(px(11.))
                                .text_color(rgba(theme::TEXT_FAINT))
                                .child("\u{E8A7}"),
                        ),
                    0,
                    1,
                ))
                .into_any_element(),
        ]
    }

    /// The extension's icon, name and description, then its own settings if
    /// it has any.
    fn extension_page(&self, id: &'static str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(source) = self.sources.iter().find(|source| source.id == id) else {
            return vec![hint("This extension isn't loaded.")];
        };
        let (glyph, color) = Page::Extension(id).icon();
        // Centred like Raycast's: the icon, the name and what it does. Its
        // switch is in the top bar.
        let mut body = vec![
            div()
                .pt_4()
                .pb_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(badge(glyph, color, 64.))
                .child(
                    div()
                        .pt_2()
                        .text_size(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(source.name),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgba(theme::TEXT_MUTED))
                        .child(if source.enabled {
                            source.description
                        } else {
                            "Turned off: it adds nothing to the search."
                        }),
                )
                .into_any_element(),
        ];
        match id {
            "clipboard" => body.extend([
                section_title("History"),
                self.clipboard_card(cx).into_any_element(),
            ]),
            "selection" => body.push(self.selection_card(cx).into_any_element()),
            FILES_PAGE => body.extend([
                section_title("Files"),
                self.files_card(cx).into_any_element(),
            ]),
            "snippets" => body.extend([
                section_title("Expansion"),
                self.snippets_card(cx).into_any_element(),
            ]),
            "web" => body.extend([
                section_title("Search Engine"),
                card()
                    .child(row(
                        "Web Search",
                        "When nothing else fits, the last result searches the web with this.",
                    ))
                    .child(self.engine_control(cx))
                    .into_any_element(),
            ]),
            _ => {}
        }
        // The list's heading comes first, then its card.
        let items = self.extension_items(id, body.len() + 1, cx);
        if body.len() == 1 && items.is_empty() {
            body.push(
                div()
                    .flex()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(rgba(theme::TEXT_FAINT))
                    .child(if source.enabled {
                        "This extension has no other settings."
                    } else {
                        "Turn it on to see its items."
                    })
                    .into_any_element(),
            );
        }
        body.extend(items);
        body
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Both run, so neither waits on the other's frame.
        let nav_moving = self.nav_scroll.tick();
        // A new page or search lays the item list out anew; one more frame
        // builds the rows that layout puts on screen.
        let relayout = std::mem::take(&mut self.relayout);
        if self.page_scroll.tick() || nav_moving || relayout {
            window.request_animation_frame();
        }
        let page = self.page();
        if page == Page::Extension(FILES_PAGE) {
            self.refresh_status_while_indexing(cx);
        }
        let body = match page {
            Page::General => self.general_page(cx),
            Page::Extensions => self.extensions_page(cx),
            Page::Shortcuts => self.shortcuts_page(cx),
            Page::About => self.about_page(),
            Page::Extension(id) => self.extension_page(id, cx),
        };

        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::save))
            .size_full()
            .flex()
            .bg(rgba(theme::BACKGROUND_TINT))
            .text_color(rgba(theme::TEXT))
            .text_size(px(14.))
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.top_bar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(
                                // Focus lives below the top bar: an element
                                // that tracks focus claims mouse-downs on it,
                                // and a claimed mouse-down in the bar would
                                // stop Windows from starting a window drag.
                                div()
                                    .id("page")
                                    .track_focus(&self.focus)
                                    .track_scroll(&self.page_scroll.handle())
                                    .size_full()
                                    .overflow_y_scroll()
                                    .px_5()
                                    .pt_1()
                                    .pb_5()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .children(body),
                            )
                            .child(capture_wheel(
                                cx.entity().downgrade(),
                                |this: &mut Self, delta, cx| {
                                    wheel(&mut this.page_scroll, delta, cx)
                                },
                            )),
                    ),
            )
    }
}

/// Animates a wheel notch while smooth scrolling is on; returns whether it did.
fn wheel(scroller: &mut Scroller, delta: Pixels, cx: &mut Context<Settings>) -> bool {
    if !scroller.is_enabled() {
        return false;
    }
    scroller.scroll_by(delta);
    cx.notify();
    true
}

/// The project's home, from the workspace manifest.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
/// The website: the repository until the project has a site of its own.
const WEBSITE: &str = REPOSITORY;
/// The X (Twitter) profile; About shows no row for it while there is none.
const X_PROFILE: Option<&str> = None;

/// The app's logo (the exe icon's artwork), decoded once.
fn logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    LOGO.get_or_init(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../../app/assets/icon.svg").to_vec(),
        ))
    })
    .clone()
}

/// Opens `url` in the default browser, off the UI thread.
fn open_url(url: &str) {
    // Failures are logged by `commands`; there is nowhere better to show them.
    let _ = commands::spawn(Command::OpenUrl(url.to_owned()));
}

/// Shows the database file in Explorer, selected; without it (not created,
/// or the shown text is a message), its folder if that exists.
fn show_in_explorer(path: &str) {
    let path = std::path::Path::new(path);
    let command = if path.is_file() {
        Command::ShowInFolder(path.display().to_string())
    } else if let Some(folder) = path.parent().filter(|folder| folder.is_dir()) {
        Command::Open(folder.display().to_string())
    } else {
        return;
    };
    let _ = commands::spawn(command);
}

/// A row that opens `url`: a glyph, the title, and an arrow for leaving the app.
fn link_row(
    id: &'static str,
    glyph: char,
    title: &'static str,
    url: &'static str,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(46.))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .hover(|style| style.bg(rgba(theme::SURFACE)))
        .on_click(move |_, _, _| open_url(url))
        .child(
            div()
                .flex_none()
                .w(px(18.))
                .font_family(icon_font())
                .text_size(px(15.))
                .text_color(rgba(theme::TEXT_MUTED))
                .child(SharedString::from(glyph.to_string())),
        )
        .child(div().flex_1().child(title))
        .child(
            div()
                .flex_none()
                .font_family(icon_font())
                .text_size(px(11.))
                .text_color(rgba(theme::TEXT_FAINT))
                .child("\u{E8A7}"),
        )
}

/// A plain push button.
fn button(id: &'static str, label: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(30.))
        .px_4()
        .flex()
        .items_center()
        .rounded(px(7.))
        .bg(rgba(theme::SURFACE_HOVER))
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xffffff26)))
        .child(label)
}

/// A heading between the cards of a page.
fn section_title(text: &'static str) -> AnyElement {
    div()
        .pt_3()
        .px_1()
        .text_size(px(13.))
        .text_color(rgba(theme::TEXT_MUTED))
        .child(text)
        .into_any_element()
}

/// A line of help above a card.
fn hint(text: &'static str) -> AnyElement {
    div()
        .px_1()
        .text_size(px(12.))
        .text_color(rgba(theme::TEXT_FAINT))
        .child(text)
        .into_any_element()
}

/// What an extension's items are, as the heading of their list.
fn items_heading(id: &str) -> &'static str {
    match id {
        "apps" => "Applications",
        "games" => "Games",
        "quicklinks" => "Quicklinks",
        "snippets" => "Snippets",
        "settings" | "control-panel" => "Pages",
        _ => "Commands",
    }
}

fn item_matches(item: &ExtensionItem, query: &str) -> bool {
    item.shortcut.title.to_lowercase().contains(query)
        || item
            .shortcut
            .alias
            .as_ref()
            .is_some_and(|alias| alias.to_lowercase().contains(query))
}

/// An item's icon in the list: its own, its glyph, or its initial.
fn item_icon(item: &ExtensionItem) -> AnyElement {
    if let Some(image) = &item.image {
        return img(image.clone())
            .size(px(20.))
            .flex_none()
            .into_any_element();
    }
    let tile = div()
        .flex_none()
        .size(px(20.))
        .rounded(px(5.))
        .bg(rgba(theme::SURFACE_HOVER))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.));
    match item.glyph {
        Some(glyph) => tile
            .font_family(icon_font())
            .child(SharedString::from(glyph.to_string())),
        None => tile.child(SharedString::from(
            item.shortcut
                .title
                .chars()
                .next()
                .map(|ch| ch.to_uppercase().collect::<String>())
                .unwrap_or_default(),
        )),
    }
    .into_any_element()
}

/// A Segoe Fluent Icons glyph, white on a rounded square of `color`.
fn badge(glyph: char, color: u32, size: f32) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(size * 0.26))
        .bg(rgba(color))
        .flex()
        .items_center()
        .justify_center()
        .font_family(icon_font())
        .text_size(px(size * 0.55))
        .text_color(rgba(0xffffffff))
        .child(SharedString::from(glyph.to_string()))
}

/// A small square button with a glyph; a disabled one is dimmed and inert.
fn icon_button(id: &'static str, glyph: char, enabled: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(28.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .font_family(icon_font())
        .text_size(px(11.))
        .when(enabled, |button| {
            button
                .text_color(rgba(theme::TEXT_MUTED))
                .cursor_pointer()
                .hover(|style| {
                    style
                        .bg(rgba(theme::SURFACE_HOVER))
                        .text_color(rgba(theme::TEXT))
                })
        })
        .when(!enabled, |button| button.text_color(rgba(theme::HAIRLINE)))
        .child(SharedString::from(glyph.to_string()))
}

/// Corner radius of a card.
const CARD_RADIUS: f32 = 10.;

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgba(theme::SURFACE_BORDER))
        .bg(rgba(theme::SURFACE))
}

/// Rounds the outer corners of row `ix` of `count` in a card, so a hover fill
/// follows the card's corners at its ends and stays square between rows.
fn card_row_corners<E: Styled>(row: E, ix: usize, count: usize) -> E {
    // Inside the card's 1 px border.
    let radius = px(CARD_RADIUS - 1.);
    let row = if ix == 0 { row.rounded_t(radius) } else { row };
    if ix + 1 == count {
        row.rounded_b(radius)
    } else {
        row
    }
}

/// A line between the rows of a card, edge to edge like Raycast's, so a
/// row's hover fill meets it without a notch at either end.
fn divider() -> impl IntoElement {
    div().flex_none().h(px(1.)).bg(rgba(theme::HAIRLINE))
}

/// A settings row: title and description on the left, the control on the right.
fn row(title: impl Into<SharedString>, description: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_h(px(54.))
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .child(text_block(title, description))
}

/// A title with a smaller description under it (none when empty).
fn text_block(title: impl Into<SharedString>, description: impl Into<SharedString>) -> gpui::Div {
    let description = description.into();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(title.into())
        .when(!description.is_empty(), |block| {
            block.child(
                div()
                    .text_size(px(12.))
                    .text_color(rgba(theme::TEXT_MUTED))
                    .child(description),
            )
        })
}

fn keycap(label: String) -> impl IntoElement {
    div()
        .min_w(px(24.))
        .h(px(22.))
        .px_1p5()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .bg(rgba(theme::SURFACE_HOVER))
        .border_b_1()
        .border_color(rgba(0x00000066))
        .text_size(px(12.))
        .child(label)
}

fn switch(
    id: impl Into<ElementId>,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .on_click(on_click)
        .flex_none()
        .cursor_pointer()
        .w(px(40.))
        .h(px(22.))
        .p(px(3.))
        .rounded_full()
        .flex()
        .when(on, |el| el.bg(rgba(theme::ACCENT)).justify_end())
        .when(!on, |el| el.bg(rgba(theme::SURFACE_HOVER)))
        .child(div().size(px(16.)).rounded_full().bg(rgba(0xffffffff)))
}

fn custom_engine_input(template: &str, cx: &mut Context<Settings>) -> Entity<TextInput> {
    let template = template.to_owned();
    cx.new(|cx| {
        let mut input = TextInput::new("https://kagi.com/search?q={query}", cx);
        input.set_text(template, cx);
        input
    })
}

/// An app in a list: its name, and its path under it. Add the button.
fn app_row(app: &IgnoredApp) -> gpui::Div {
    div().flex().items_center().gap_3().child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(div().text_size(px(13.)).truncate().child(app.name.clone()))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgba(theme::TEXT_FAINT))
                    .truncate()
                    .child(app.path.clone()),
            ),
    )
}

/// A choice in a row of options; the selected one is filled with the accent.
fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .text_size(px(12.))
        .cursor_pointer()
        .when(selected, |chip| {
            chip.border_color(rgba(theme::ACCENT))
                .bg(rgba(theme::ACCENT_SOFT))
                .text_color(rgba(theme::TEXT))
        })
        .when(!selected, |chip| {
            chip.border_color(rgba(theme::SURFACE_BORDER))
                .text_color(rgba(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgba(theme::SURFACE_HOVER)))
        })
        .child(label.into())
}

/// A clickable field in the shortcut list.
fn cell(id: impl Into<ElementId>) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(130.))
        .h(px(30.))
        .pl_2()
        .pr_1()
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(6.))
        .border_1()
        .border_color(rgba(theme::SURFACE_BORDER))
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(theme::SURFACE_HOVER)))
}

fn remove_button(
    id: impl Into<ElementId>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .size(px(20.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .text_color(rgba(theme::TEXT_FAINT))
        .hover(|style| {
            style
                .bg(rgba(theme::SURFACE_HOVER))
                .text_color(rgba(theme::TEXT))
        })
        .on_click(on_click)
        .child("✕")
}
