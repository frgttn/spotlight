//! The settings window: launcher hotkey, web search engine, smooth scrolling,
//! which search sources are on, how much clipboard history to keep, start at
//! login, and every alias and item hotkey.
//!
//! The window has no system frame. The header is the drag area and carries
//! its own close button, so it matches the launcher instead of a stock
//! Windows dialog.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, SharedString, Stateful, Subscription, Task, Window, WindowControlArea, actions,
    div, prelude::*, px, rgba,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_core::{
    clip::{self, IgnoredApp, Retention},
    web::{self, SearchEngine},
};
use spotlight_platform_win::{
    autostart,
    hotkey::{self, Hotkey, Recorded, SetError},
    win,
};

use crate::{
    launcher::{Shortcut, Source},
    text_input::TextInput,
    theme,
};

actions!(settings, [Close, Save]);

const CONTEXT: &str = "Settings";
/// Logical size of the settings window.
pub const WINDOW_SIZE: (f32, f32) = (600., 560.);

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
    /// Search providers and whether each is on.
    pub sources: Vec<Source>,
    /// Apps whose copies the clipboard history doesn't keep.
    pub ignored_apps: Vec<IgnoredApp>,
}

/// Reads the launcher's current state; `None` while the launcher can't be
/// read (it is mid-update, as when its Ctrl+, opens this window).
pub type LauncherReader = Rc<dyn Fn(&App) -> Option<LauncherState>>;

pub enum SettingsEvent {
    /// The launcher hotkey is now this one; persist it and update labels.
    HotkeyChanged(Hotkey),
    SmoothScrollingChanged(bool),
    /// The web search engine is now this one; switch to it and save it.
    SearchEngineChanged(SearchEngine),
    /// The clipboard history now keeps this much; trim it and save it.
    ClipboardRetentionChanged(Retention),
    /// The apps whose copies aren't kept are now these; save them.
    IgnoredAppsChanged(Vec<IgnoredApp>),
    /// Win+V opens the clipboard history (`true`) or Windows' own.
    WinVChanged(bool),
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
    /// The alias being edited in place: item id and the field.
    editing_alias: Option<(String, Entity<TextInput>)>,
    /// Outcome of the last change in the list: item id, text, and whether it is an error.
    shortcut_message: Option<(String, SharedString, bool)>,
    smooth_scrolling: bool,
    search_engine: SearchEngine,
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
    autostart: bool,
    data_path: SharedString,
    focus: FocusHandle,
    recorder: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<SettingsEvent> for Settings {}

impl Settings {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hotkeys: hotkey::Service,
        hotkey: Option<Hotkey>,
        smooth_scrolling: bool,
        search_engine: SearchEngine,
        clipboard_retention: Retention,
        win_v: bool,
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
        // Recording takes every key press system-wide, so it ends as soon as
        // the user clicks elsewhere in the window or switches to another one.
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
        ];
        let focus = cx.focus_handle();
        window.focus(&focus);
        let LauncherState {
            shortcuts,
            sources,
            ignored_apps,
        } = launcher(cx).unwrap_or_default();
        Self {
            hotkeys,
            hotkey,
            recording: None,
            message: None,
            recording_for: None,
            shortcuts,
            launcher,
            editing_alias: None,
            shortcut_message: None,
            smooth_scrolling,
            custom_engine: match &search_engine {
                SearchEngine::Custom(template) => Some(custom_engine_input(template, cx)),
                SearchEngine::Preset(_) => None,
            },
            search_engine,
            engine_message: None,
            sources,
            clipboard_retention,
            ignored_apps,
            win_v,
            autostart: autostart::is_enabled(),
            data_path,
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
        self.sources = state.sources;
        self.ignored_apps = state.ignored_apps;
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

    /// Enter: saves the alias or the custom search URL being edited.
    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.custom_engine
            && input.focus_handle(cx).is_focused(window)
        {
            return self.save_custom_engine(window, cx);
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
            for shortcut in &mut self.shortcuts {
                if shortcut
                    .alias
                    .as_ref()
                    .is_some_and(|other| other.eq_ignore_ascii_case(alias))
                {
                    shortcut.alias = None;
                }
            }
        }
        self.set_local(&item_id, |shortcut| shortcut.alias = alias);
        cx.notify();
    }

    /// Changes the local copy right away. A row left with neither an alias nor
    /// a hotkey stays until the next refresh, so a slip can be undone in place.
    fn set_local(&mut self, item_id: &str, change: impl FnOnce(&mut Shortcut)) {
        if let Some(shortcut) = self
            .shortcuts
            .iter_mut()
            .find(|shortcut| shortcut.item_id == item_id)
        {
            change(shortcut);
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
                    .child(custom),
            )
            .children(field)
            .children(message)
    }

    fn toggle_smooth_scrolling(&mut self, cx: &mut Context<Self>) {
        self.smooth_scrolling = !self.smooth_scrolling;
        cx.emit(SettingsEvent::SmoothScrollingChanged(self.smooth_scrolling));
        cx.notify();
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

    /// One row per search provider, each with its switch.
    fn source_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.sources.iter().enumerate().map(|(ix, source)| {
            div()
                .flex()
                .flex_col()
                .when(ix > 0, |row| row.child(divider()))
                .child(row(source.name, source.description).child(switch(
                    ("source", ix),
                    source.enabled,
                    cx.listener(move |this, _, _, cx| this.toggle_source(ix, cx)),
                )))
        });
        card().children(rows)
    }

    fn set_clipboard_retention(&mut self, retention: Retention, cx: &mut Context<Self>) {
        if retention != self.clipboard_retention {
            self.clipboard_retention = retention;
            cx.emit(SettingsEvent::ClipboardRetentionChanged(retention));
            cx.notify();
        }
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
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
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
                    .child(remove_button(
                        ("unignore", ix),
                        cx.listener(move |this, _, _, cx| this.unignore_app(ix, cx)),
                    ))
            }))
            .into_any_element()
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

    /// Esc: cancels an alias or custom URL edit, otherwise closes the window.
    fn close(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        let editing_engine = self
            .custom_engine
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        if editing_engine {
            // Back to what is in use: a custom engine keeps its field.
            self.custom_engine = match &self.search_engine {
                SearchEngine::Custom(template) => Some(custom_engine_input(template, cx)),
                SearchEngine::Preset(_) => None,
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

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .h(px(48.))
            .pl_5()
            .pr_2()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgba(theme::HAIRLINE))
            .child(
                // Dragging the window works anywhere in the header except the close button.
                div()
                    .id("drag")
                    .window_control_area(WindowControlArea::Drag)
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded(px(5.))
                            .bg(rgba(theme::ACCENT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .child("S"),
                    )
                    .child(div().text_size(px(14.)).child("Spotlight Settings")),
            )
            .child(
                div()
                    .id("close")
                    .size(px(32.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgba(theme::TEXT_MUTED))
                    .hover(|style| {
                        style
                            .bg(rgba(theme::SURFACE_HOVER))
                            .text_color(rgba(theme::TEXT))
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| this.close(&Close, window, cx)))
                    .child("✕"),
            )
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
        let rows = self.shortcuts.iter().enumerate().map(|(ix, shortcut)| {
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
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(shortcut.title.clone())),
                        )
                        .child(self.alias_cell(ix, shortcut, cx))
                        .child(self.hotkey_cell(ix, shortcut, cx)),
                )
                .children(message)
        });
        card().children(rows).into_any_element()
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

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hotkey_hint = if self.recording.is_some() {
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

        let launcher = card()
            .child(row("Launcher hotkey", hotkey_hint).child(self.hotkey_control(cx)))
            .children(message)
            .child(divider())
            .child(row(
                "Web search",
                "When nothing else fits, the last result searches the web with this.",
            ))
            .child(self.engine_control(cx))
            .child(divider())
            .child(
                row(
                    "Smooth scrolling",
                    "Animate the result list when you scroll or move the selection.",
                )
                .child(switch(
                    "smooth-scrolling",
                    self.smooth_scrolling,
                    cx.listener(|this, _, _, cx| this.toggle_smooth_scrolling(cx)),
                )),
            );

        let shortcuts_hint = if self.recording.is_some() && self.recording_for.is_some() {
            "Press the keys now, or Esc to cancel."
        } else if self.editing_alias.is_some() {
            "Enter saves (empty removes the alias), Esc cancels."
        } else {
            "Click an alias or hotkey to change it."
        };

        let system = card().child(
            row(
                "Start at login",
                "Run Spotlight in the background when you sign in.",
            )
            .child(switch(
                "autostart",
                self.autostart,
                cx.listener(|this, _, _, cx| this.toggle_autostart(cx)),
            )),
        );

        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::save))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgba(theme::BACKGROUND_TINT))
            .text_color(rgba(theme::TEXT))
            .text_size(px(14.))
            .child(self.header(cx))
            .child(
                // Focus lives below the header: an element that tracks focus
                // claims mouse-downs on it, and a claimed mouse-down in the
                // header would stop Windows from starting a window drag.
                div()
                    .id("content")
                    .track_focus(&self.focus)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pt_4()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_label("Launcher"))
                    .child(launcher)
                    .child(div().h(px(6.)))
                    .child(section_label("Search sources"))
                    .child(self.source_list(cx))
                    .child(div().h(px(6.)))
                    .child(section_label("Clipboard history"))
                    .child(self.clipboard_card(cx))
                    .child(div().h(px(6.)))
                    .child(section_label("System"))
                    .child(system)
                    .child(div().h(px(6.)))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .gap_4()
                            .child(section_label("Aliases & hotkeys"))
                            .child(
                                div()
                                    .px_1()
                                    .text_size(px(11.))
                                    .text_color(rgba(theme::TEXT_FAINT))
                                    .child(shortcuts_hint),
                            ),
                    )
                    .child(self.shortcut_list(cx)),
            )
            .child(
                div()
                    .flex_none()
                    .px_5()
                    .py_3()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .text_size(px(11.))
                    .text_color(rgba(theme::TEXT_FAINT))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(format!("Data: {}", self.data_path)),
                    )
                    .child(div().flex_none().child("Esc to close")),
            )
    }
}

fn section_label(text: &'static str) -> impl IntoElement {
    div()
        .px_1()
        .text_size(px(11.))
        .text_color(rgba(theme::TEXT_FAINT))
        .child(text.to_uppercase())
}

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded(px(10.))
        .border_1()
        .border_color(rgba(theme::SURFACE_BORDER))
        .bg(rgba(theme::SURFACE))
}

fn divider() -> impl IntoElement {
    div().mx_4().h(px(1.)).bg(rgba(theme::HAIRLINE))
}

/// A settings row: title and description on the left, the control on the right.
fn row(title: &'static str, description: &'static str) -> gpui::Div {
    div()
        .min_h(px(60.))
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(title)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgba(theme::TEXT_MUTED))
                        .child(description),
                ),
        )
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

/// A choice in a row of options; the selected one is filled with the accent.
fn chip(id: impl Into<ElementId>, label: &'static str, selected: bool) -> Stateful<gpui::Div> {
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
        .child(label)
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
