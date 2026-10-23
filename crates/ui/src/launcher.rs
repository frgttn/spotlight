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
    Hsla, KeyBinding, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, Subscription,
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
    commands, icons,
    metrics::{self, PaintProbe, Percentiles, Probe, Summary, ms},
    win,
};
use spotlight_search::{RootSearch, Row};
use spotlight_store::Store;

use crate::{
    smooth_scroll::SmoothScroll,
    text_input::{self, TextInput},
    theme,
};

actions!(
    launcher,
    [SelectNext, SelectPrev, Confirm, Dismiss, Quit, OpenSettings]
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

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("ctrl-p", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(CONTEXT)),
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
    /// Set by a first Enter on a command that asks for confirmation (restart,
    /// shut down): that item's id. Moving the selection or typing clears it.
    confirming: Option<String>,
    scroll: SmoothScroll,
    /// Extracted shell icons by item id. Items without one show a badge.
    icons: HashMap<String, Arc<RenderImage>>,
    /// In-memory copy of the store's frecency table, updated on every launch.
    frecency: HashMap<String, Frecency>,
    /// `None` when the database couldn't be opened; usage is then not remembered.
    store: Option<Arc<Mutex<Store>>>,
    visible: bool,
    stats: Stats,
    _subscriptions: Vec<Subscription>,
}

impl Launcher {
    pub fn new(
        hotkey: SharedString,
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

        let input = cx.new(|cx| TextInput::new("Search apps and commands, or calculate…", cx));
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

        let mut this = Self {
            hwnd,
            input,
            loading: providers.len(),
            search: RootSearch::new(providers),
            results: Vec::new(),
            selected: 0,
            confirming: None,
            scroll: {
                let mut scroll =
                    SmoothScroll::new(UniformListScrollHandle::new(), ROW_HEIGHT, LIST_PADDING);
                scroll.set_enabled(smooth_scrolling);
                scroll
            },
            icons: HashMap::new(),
            frecency,
            store,
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

    /// Re-runs root search and resets the selection. Returns the search time in ms.
    fn refilter(&mut self, query: &str) -> f64 {
        let started = Instant::now();
        let now = frecency::now();
        let frecency = &self.frecency;
        self.results = self.search.search(query, |id| {
            frecency.get(id).map_or(0., |used| used.score_at(now))
        });
        self.selected = 0;
        self.confirming = None;
        self.scroll.reset();
        ms(started.elapsed())
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
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.replace_text_in_range(None, text, window, cx)
        });
    }

    /// Every catalog, apps included, has loaded.
    pub fn apps_loaded(&self) -> bool {
        self.loading == 0
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
        win::hide(self.hwnd);
        self.visible = false;
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
            self.confirming = None;
            self.scroll.reveal(self.selected, self.results.len());
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected > 0 {
            self.selected -= 1;
            self.confirming = None;
            self.scroll.reveal(self.selected, self.results.len());
            cx.notify();
        }
    }

    /// Runs the selected item's primary action.
    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.results.get(self.selected) else {
            self.hide(cx);
            return;
        };
        let item = self.item(row);
        let Some(action) = item.primary_action() else {
            self.hide(cx);
            return;
        };
        if action.command.needs_confirmation() && self.confirming.as_ref() != Some(&item.id) {
            self.confirming = Some(item.id.clone());
            cx.notify();
            return;
        }

        let command = action.command.clone();
        // Only catalog items are ranked, so only their use is worth remembering.
        let visited = matches!(row, Row::Catalog(_)).then(|| item.id.clone());
        match &command {
            Command::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text.clone())),
            command => commands::execute(command),
        }
        if let Some(id) = visited {
            self.record_visit(id, cx);
        }
        self.hide(cx);
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

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        self.hide(cx);
    }

    fn render_row(&self, row: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let item = self.item(&self.results[row]);
        let selected = row == self.selected;
        let confirming = self.confirming.as_ref() == Some(&item.id);
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
                    this.confirm(&Confirm, window, cx);
                }))
                .child(self.render_icon(item))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .child(SharedString::from(item.title.clone())),
                )
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
                .child("No results")
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
                    .text_size(px(20.))
                    .line_height(px(28.))
                    .child(self.input.clone()),
            )
            .child(div().h(px(1.)).bg(rgba(0xffffff14)))
            .child(results)
            .child(
                div()
                    .px_4()
                    .py_2()
                    .bg(rgba(0x00000033))
                    .text_size(px(11.))
                    .text_color(rgba(0xffffff80))
                    .truncate()
                    .child(self.footer()),
            )
            // Last child, so it paints after everything else in the frame.
            .child(
                canvas(|_, _, _| {}, move |_, _, _, _| paint.borrow_mut().painted())
                    .absolute()
                    .size_full(),
            )
    }
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
