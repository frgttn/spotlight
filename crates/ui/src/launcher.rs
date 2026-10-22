//! The launcher view: search field, result list, metrics footer.

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use gpui::{
    App, Context, Entity, EntityInputHandler, Focusable, Hsla, KeyBinding, RenderImage,
    ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, Window, actions, canvas,
    div, hsla, img, prelude::*, px, rgb, rgba, uniform_list,
};
use image::{Frame, RgbaImage};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_core::{
    frecency::{self, Frecency},
    item_id,
};
use spotlight_platform_win::{
    apps::{self, AppEntry},
    icons::{self, Icon},
    metrics::{self, PaintProbe, Percentiles, Probe, Summary, ms},
    win,
};
use spotlight_search::{Candidate, Searcher};
use spotlight_store::Store;

use crate::text_input::{self, TextInput};

actions!(launcher, [SelectNext, SelectPrev, Confirm, Dismiss, Quit]);

const CONTEXT: &str = "Launcher";
const ROW_HEIGHT: f32 = 44.;
/// Row icon size in logical pixels.
const ICON_SIZE: f32 = 26.;
/// Launcher window size in logical (96-DPI) pixels.
pub const WINDOW_SIZE: (f32, f32) = (680., 420.);
/// Background over GPUI's `Blurred` window. GPUI's accent-policy blur barely
/// blurs on Windows 11, so the tint has to hide what's behind on its own.
/// (The DWM acrylic system backdrop was tried: light grey, and the window lost
/// focus and hid itself after a few seconds.)
const BACKGROUND_TINT: u32 = 0x1c1c1ef7;

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("ctrl-p", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

enum Target {
    App(AppEntry),
    /// Filler rows for the 10k-item list test (`SPOTLIGHT_SYNTHETIC`).
    Synthetic,
}

struct Item {
    /// Frecency key; `None` for items that aren't tracked (synthetic rows).
    id: Option<String>,
    title: SharedString,
    subtitle: SharedString,
    /// `None` until extracted (or when there is none): a letter badge is drawn instead.
    icon: Option<Arc<RenderImage>>,
    target: Target,
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
    items: Vec<Item>,
    /// Indices into `items`, best match first.
    results: Vec<usize>,
    selected: usize,
    scroll: UniformListScrollHandle,
    searcher: Searcher,
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
        synthetic: usize,
        store: Option<Store>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hwnd = match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
            other => panic!("expected a Win32 window handle, got {other:?}"),
        };
        win::round_corners(hwnd);

        let input = cx.new(|cx| TextInput::new("Search apps…", cx));
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

        let items = (0..synthetic)
            .map(|ix| Item {
                id: None,
                title: format!("Synthetic item {ix:05} {}", FILLER[ix % FILLER.len()]).into(),
                subtitle: "Synthetic".into(),
                icon: None,
                target: Target::Synthetic,
            })
            .collect();

        let frecency = store
            .as_ref()
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
            items,
            results: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            searcher: Searcher::default(),
            frecency,
            store: store.map(|store| Arc::new(Mutex::new(store))),
            visible: false,
            stats: Stats {
                hotkey,
                ..Default::default()
            },
            _subscriptions: subscriptions,
        };
        this.refilter("");
        this.load_apps(window.scale_factor(), cx);
        this
    }

    /// Discovers apps on a dedicated COM thread and prepends them to the items.
    fn load_apps(&mut self, window_scale: f32, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::bounded(1);
        thread::spawn(move || {
            let started = Instant::now();
            let result = apps::discover();
            let _ = tx.send_blocking((result, started.elapsed()));
        });

        cx.spawn(async move |this, cx| {
            let Ok((result, elapsed)) = rx.recv().await else {
                return;
            };
            this.update(cx, |this, cx| {
                match result {
                    Ok(apps) => {
                        this.stats.apps = apps.len();
                        let items = apps.into_iter().map(|app| Item {
                            id: Some(item_id("apps", &app.launch_id)),
                            title: app.name.clone().into(),
                            subtitle: "Application".into(),
                            icon: None,
                            target: Target::App(app),
                        });
                        this.items.splice(0..0, items);
                        this.load_icons(window_scale, cx);
                    }
                    Err(err) => eprintln!("[spotlight] app discovery failed: {err}"),
                }
                this.stats.discover_ms = Some(ms(elapsed));
                eprintln!(
                    "[spotlight] discovered {} apps in {:.1} ms",
                    this.stats.apps,
                    ms(elapsed)
                );
                let query = this.query(cx);
                this.refilter(&query);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Extracts app icons in the background; rows switch from badges to icons as they arrive.
    /// Apps are at the front of `items` right after discovery, so an app's index
    /// in the request is also its index in `items`.
    fn load_icons(&mut self, window_scale: f32, cx: &mut Context<Self>) {
        let launch_ids: Vec<String> = self
            .items
            .iter()
            .map_while(|item| match &item.target {
                Target::App(app) => Some(app.launch_id.clone()),
                Target::Synthetic => None,
            })
            .collect();
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
                        if let Some(item) = this.items.get_mut(ix) {
                            item.icon = render_image(icon);
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

    pub fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().to_owned()
    }

    /// Re-runs the fuzzy filter and resets the selection. Returns the filter time in ms.
    fn refilter(&mut self, query: &str) -> f64 {
        let started = Instant::now();
        let now = frecency::now();
        let candidates = self.items.iter().map(|item| Candidate {
            title: item.title.as_ref(),
            frecency: item
                .id
                .as_ref()
                .and_then(|id| self.frecency.get(id))
                .map_or(0., |used| used.score_at(now)),
        });
        self.results = self.searcher.filter(query, candidates);
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        ms(started.elapsed())
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

    pub fn apps_loaded(&self) -> bool {
        self.stats.discover_ms.is_some()
    }

    pub fn summary(&self) -> Summary {
        let paint = self.stats.paint.borrow();
        Summary {
            apps: self.stats.apps,
            items: self.items.len(),
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
            self.scroll
                .scroll_to_item(self.selected, ScrollStrategy::Bottom);
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected > 0 {
            self.selected -= 1;
            self.scroll
                .scroll_to_item(self.selected, ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(&ix) = self.results.get(self.selected) {
            let item = &self.items[ix];
            if let Target::App(app) = &item.target {
                app.launch();
            }
            if let Some(id) = item.id.clone() {
                self.record_visit(id, cx);
            }
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
        let item = &self.items[self.results[row]];
        let initial: SharedString = item
            .title
            .chars()
            .next()
            .map(|ch| ch.to_uppercase().collect::<String>())
            .unwrap_or_default()
            .into();

        let selected = row == self.selected;
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
                    this.selected = row;
                    this.confirm(&Confirm, window, cx);
                }))
                .child(match &item.icon {
                    Some(icon) => img(icon.clone())
                        .size(px(ICON_SIZE))
                        .flex_none()
                        .into_any_element(),
                    None => div()
                        .size(px(ICON_SIZE))
                        .flex_none()
                        .rounded(px(6.))
                        .bg(badge_color(&item.title))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(13.))
                        .child(initial)
                        .into_any_element(),
                })
                .child(div().flex_1().truncate().child(item.title.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(rgba(0xffffff70))
                        .child(item.subtitle.clone()),
                ),
        )
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
            items = self.items.len(),
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let results: gpui::AnyElement = if self.results.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgba(0xffffff60))
                .child("No results")
                .into_any_element()
        } else {
            uniform_list(
                "results",
                self.results.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    range
                        .map(|row| this.render_row(row, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .flex_1()
            .py_1()
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
            .bg(rgba(BACKGROUND_TINT))
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

/// GPUI wants BGRA with straight alpha, which is what `icons` produces; the
/// `RgbaImage` type is only the buffer container here.
fn render_image(icon: Icon) -> Option<Arc<RenderImage>> {
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
