//! Views opened on top of root search ([`View`]): their state on the
//! launcher's screen stack, and how grids, details and forms are drawn. Lists
//! reuse root search's rows, and every view keeps the action panel and Esc.

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, ScrollHandle, ScrollStrategy,
    SharedString, Window, div, prelude::*, px, rgba, uniform_list,
};
use spotlight_core::view::{DetailView, FieldKind, FieldValue, FormView, ListView, View};

use super::{Launcher, Row, keycaps};
use crate::{markdown, text_input::TextInput, theme};

/// Tiles per grid row.
pub(super) const GRID_COLUMNS: usize = 5;
const TILE_HEIGHT: f32 = 112.;
const TILE_ICON: f32 = 44.;
/// How far ↑/↓ scroll a detail view.
const DETAIL_STEP: f32 = 48.;

/// One level of the screen stack above root search.
pub(super) enum Screen {
    List {
        view: ListView,
        grid: bool,
    },
    Detail {
        view: DetailView,
        scroll: ScrollHandle,
    },
    Form(FormState),
}

pub(super) struct FormState {
    pub view: FormView,
    /// One per field of `view`, in order.
    pub inputs: Vec<FieldInput>,
    /// Why the last submit was refused.
    pub error: Option<SharedString>,
}

pub(super) enum FieldInput {
    Text(Entity<TextInput>),
    Checkbox { on: bool, focus: FocusHandle },
    Dropdown { selected: usize, focus: FocusHandle },
}

impl FieldInput {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            FieldInput::Text(input) => input.focus_handle(cx),
            FieldInput::Checkbox { focus, .. } | FieldInput::Dropdown { focus, .. } => {
                focus.clone()
            }
        }
    }

    fn value(&self, cx: &App) -> FieldValue {
        match self {
            FieldInput::Text(input) => FieldValue::Text(input.read(cx).text().to_owned()),
            FieldInput::Checkbox { on, .. } => FieldValue::Checkbox(*on),
            FieldInput::Dropdown { selected, .. } => FieldValue::Dropdown(*selected),
        }
    }
}

impl Screen {
    fn new(view: View, cx: &mut Context<Launcher>) -> Self {
        match view {
            View::List(view) => Screen::List { view, grid: false },
            View::Grid(view) => Screen::List { view, grid: true },
            View::Detail(view) => Screen::Detail {
                view,
                scroll: ScrollHandle::new(),
            },
            View::Form(view) => {
                let inputs = view
                    .fields
                    .iter()
                    .map(|field| match &field.kind {
                        FieldKind::Text {
                            placeholder,
                            default,
                            ..
                        } => {
                            let (placeholder, default) = (placeholder.clone(), default.clone());
                            FieldInput::Text(cx.new(|cx| {
                                let mut input = TextInput::new(placeholder, cx);
                                input.set_text(default, cx);
                                input
                            }))
                        }
                        FieldKind::Checkbox { default } => FieldInput::Checkbox {
                            on: *default,
                            focus: cx.focus_handle(),
                        },
                        FieldKind::Dropdown { default, options } => FieldInput::Dropdown {
                            selected: (*default).min(options.len().saturating_sub(1)),
                            focus: cx.focus_handle(),
                        },
                    })
                    .collect();
                Screen::Form(FormState {
                    view,
                    inputs,
                    error: None,
                })
            }
        }
    }

    pub(super) fn title(&self) -> &str {
        match self {
            Screen::List { view, .. } => &view.title,
            Screen::Detail { view, .. } => &view.title,
            Screen::Form(form) => &form.view.title,
        }
    }

    /// Lists and grids filter with the search field; details and forms don't
    /// use it.
    pub(super) fn has_search(&self) -> bool {
        matches!(self, Screen::List { .. })
    }
}

impl Launcher {
    /// Opens `view` on top of the current screen.
    pub(super) fn push_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        let screen = Screen::new(view, cx);
        self.screens.push(screen);
        self.enter_screen(window, cx);
    }

    /// Back to the screen below (root search at the bottom).
    pub(super) fn pop_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.screens.pop();
        self.enter_screen(window, cx);
    }

    /// Sets up the search field, the rows and the keyboard for the top screen.
    pub(super) fn enter_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder: SharedString = match self.screens.last() {
            None => super::SEARCH_PLACEHOLDER.into(),
            Some(Screen::List { view, .. }) => view
                .placeholder
                .clone()
                .unwrap_or_else(|| format!("Search {}…", view.title.to_lowercase()))
                .into(),
            Some(_) => SharedString::default(),
        };
        let grid = self.in_grid();
        self.input.update(cx, |input, cx| {
            input.clear(cx);
            input.set_placeholder(placeholder, cx);
            // ←/→ move between tiles; the text cursor has little to do there.
            input.set_pass_horizontal_arrows(grid);
        });
        self.refilter("");
        match self.screens.last() {
            None | Some(Screen::List { .. }) => window.focus(&self.input.focus_handle(cx)),
            Some(Screen::Detail { .. }) => window.focus(&self.focus),
            Some(Screen::Form(form)) => match form.inputs.first() {
                Some(input) => window.focus(&input.focus_handle(cx)),
                None => window.focus(&self.focus),
            },
        }
        cx.notify();
    }

    pub(super) fn in_grid(&self) -> bool {
        matches!(self.screens.last(), Some(Screen::List { grid: true, .. }))
    }

    /// The items of the open list or grid, to filter.
    pub(super) fn list_items(&self) -> Option<&[spotlight_core::item::SearchItem]> {
        match self.screens.last() {
            Some(Screen::List { view, .. }) => Some(&view.items),
            _ => None,
        }
    }

    /// Moves the grid selection by `delta` tiles, staying inside the grid.
    pub(super) fn grid_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.results.len();
        if count == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        if next != self.selected {
            self.selected = next;
            self.confirming = None;
            self.grid_scroll
                .scroll_to_item(next / GRID_COLUMNS, ScrollStrategy::Center);
            cx.notify();
        }
    }

    /// ↑/↓ in a detail view scroll it.
    pub(super) fn detail_scroll(&mut self, down: bool, cx: &mut Context<Self>) {
        if let Some(Screen::Detail { scroll, .. }) = self.screens.last() {
            let mut offset = scroll.offset();
            // Offsets are negative: scrolling down makes `y` smaller.
            offset.y += px(if down { -DETAIL_STEP } else { DETAIL_STEP });
            scroll.set_offset(offset);
            cx.notify();
        }
    }

    /// Submits the open form: checks it, fills its command and runs that.
    pub(super) fn submit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Screen::Form(form)) = self.screens.last() else {
            return;
        };
        let values: Vec<FieldValue> = form.inputs.iter().map(|input| input.value(cx)).collect();
        match form.view.check(&values) {
            Ok(()) => {
                let command = form.view.fill(&values);
                self.perform(&command, window, cx);
            }
            Err(why) => {
                if let Some(Screen::Form(form)) = self.screens.last_mut() {
                    form.error = Some(why.into());
                }
                cx.notify();
            }
        }
    }

    /// Tab / Shift+Tab: the next or previous field, wrapping around.
    pub(super) fn focus_field(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Screen::Form(form)) = self.screens.last() else {
            return;
        };
        let count = form.inputs.len();
        if count == 0 {
            return;
        }
        let current = form
            .inputs
            .iter()
            .position(|input| input.focus_handle(cx).is_focused(window))
            .unwrap_or(count - 1);
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        window.focus(&form.inputs[next].focus_handle(cx));
        cx.notify();
    }

    /// Space / ←/→ on the focused checkbox or dropdown.
    pub(super) fn change_choice(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Screen::Form(form)) = self.screens.last_mut() else {
            return;
        };
        for (field, input) in form.view.fields.iter().zip(&mut form.inputs) {
            match input {
                FieldInput::Checkbox { on, focus } if focus.is_focused(window) => *on = !*on,
                FieldInput::Dropdown { selected, focus } if focus.is_focused(window) => {
                    if let FieldKind::Dropdown { options, .. } = &field.kind
                        && !options.is_empty()
                    {
                        let count = options.len() as isize;
                        *selected = (*selected as isize + delta).rem_euclid(count) as usize;
                    }
                }
                _ => continue,
            }
            form.error = None;
            cx.notify();
            return;
        }
    }

    /// The open form's values and its error. For the self-test.
    pub fn form_state(&self, cx: &App) -> Option<(Vec<FieldValue>, Option<String>)> {
        let Some(Screen::Form(form)) = self.screens.last() else {
            return None;
        };
        let values = form.inputs.iter().map(|input| input.value(cx)).collect();
        Some((values, form.error.as_ref().map(|error| error.to_string())))
    }

    /// The open form's text field that has the keyboard.
    pub(super) fn focused_form_input(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<Entity<TextInput>> {
        let Some(Screen::Form(form)) = self.screens.last() else {
            return None;
        };
        form.inputs.iter().find_map(|input| match input {
            FieldInput::Text(text) if text.focus_handle(cx).is_focused(window) => {
                Some(text.clone())
            }
            _ => None,
        })
    }

    /// The selected row or tile. For the self-test.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Lists and grids: the rows; details and forms: their own body.
    pub(super) fn render_screen_body(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match self.screens.last()? {
            Screen::List { grid: true, .. } => Some(self.render_grid(cx)),
            Screen::List { grid: false, .. } => None,
            Screen::Detail { view, scroll } => Some(render_detail(view, scroll)),
            Screen::Form(form) => Some(self.render_form(form, window, cx)),
        }
    }

    fn render_grid(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.results.len().div_ceil(GRID_COLUMNS);
        uniform_list(
            "grid",
            rows,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|row| {
                        let start = row * GRID_COLUMNS;
                        let end = (start + GRID_COLUMNS).min(this.results.len());
                        div()
                            .w_full()
                            .h(px(TILE_HEIGHT))
                            .px_2()
                            .flex()
                            .children((start..end).map(|ix| this.render_tile(ix, cx)))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.grid_scroll.clone())
        .size_full()
        .py_1()
        .into_any_element()
    }

    fn render_tile(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(Row::Computed(item)) = self.results.get(ix) else {
            return div().into_any_element();
        };
        let selected = ix == self.selected;
        div()
            .id(("tile", ix))
            .flex_1()
            .max_w(px((super::WINDOW_SIZE.0 - 16.) / GRID_COLUMNS as f32))
            .h_full()
            .p_1()
            .child(
                div()
                    .id("tile-inner")
                    .size_full()
                    .px_2()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded(px(8.))
                    .when(selected, |tile| tile.bg(rgba(0xffffff1f)))
                    .when(!selected, |tile| {
                        tile.hover(|style| style.bg(rgba(0xffffff0f)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.selected = ix;
                        this.confirm(&super::Confirm, window, cx);
                    }))
                    .child(self.render_icon_sized(item, TILE_ICON))
                    // Centred when it fits; otherwise cut at the end with "…".
                    .child(
                        div().w_full().flex().justify_center().child(
                            div()
                                .max_w_full()
                                .text_size(px(12.))
                                .truncate()
                                .child(SharedString::from(item.title.clone())),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_form(
        &self,
        form: &FormState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fields =
            form.view
                .fields
                .iter()
                .zip(&form.inputs)
                .enumerate()
                .map(|(ix, (field, input))| {
                    let focused = input.focus_handle(cx).is_focused(window);
                    let control: AnyElement = match (input, &field.kind) {
                        (FieldInput::Text(input), _) => div()
                            .flex_1()
                            .h(px(34.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(rgba(if focused {
                                theme::ACCENT
                            } else {
                                theme::SURFACE_BORDER
                            }))
                            .bg(rgba(theme::SURFACE))
                            .child(div().flex_1().child(input.clone()))
                            .into_any_element(),
                        (FieldInput::Checkbox { on, focus }, _) => {
                            let on = *on;
                            div()
                                .id(("checkbox", ix))
                                .key_context(CHOICE_CONTEXT)
                                .track_focus(focus)
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.toggle_checkbox(ix, window, cx)
                                }))
                                .child(
                                    div()
                                        .size(px(18.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(rgba(if focused {
                                            theme::ACCENT
                                        } else {
                                            theme::SURFACE_BORDER
                                        }))
                                        .when(on, |check| check.bg(rgba(theme::ACCENT)))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(12.))
                                        .when(on, |check| check.child("✓")),
                                )
                                .into_any_element()
                        }
                        (
                            FieldInput::Dropdown { selected, focus },
                            FieldKind::Dropdown { options, .. },
                        ) => {
                            let selected = *selected;
                            div()
                                .id(("dropdown", ix))
                                .key_context(CHOICE_CONTEXT)
                                .track_focus(focus)
                                .flex_1()
                                .p_1()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgba(if focused {
                                    theme::ACCENT
                                } else {
                                    0x00000000
                                }))
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .children(options.iter().enumerate().map(|(option, (label, _))| {
                                    let chosen = option == selected;
                                    div()
                                        .id(("option", ix * 100 + option))
                                        .px_2()
                                        .h(px(26.))
                                        .flex()
                                        .items_center()
                                        .rounded_full()
                                        .border_1()
                                        .text_size(px(12.))
                                        .cursor_pointer()
                                        .when(chosen, |chip| {
                                            chip.border_color(rgba(theme::ACCENT))
                                                .bg(rgba(theme::ACCENT_SOFT))
                                        })
                                        .when(!chosen, |chip| {
                                            chip.border_color(rgba(theme::SURFACE_BORDER))
                                                .text_color(rgba(theme::TEXT_MUTED))
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.choose_option(ix, option, window, cx)
                                        }))
                                        .child(SharedString::from(label.clone()))
                                }))
                                .into_any_element()
                        }
                        (FieldInput::Dropdown { .. }, _) => div().into_any_element(),
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_none()
                                .w(px(110.))
                                .text_size(px(13.))
                                .text_color(rgba(theme::TEXT_MUTED))
                                .child(SharedString::from(field.label.clone())),
                        )
                        .child(control)
                });
        div()
            .flex_1()
            .px_6()
            .py_5()
            .flex()
            .flex_col()
            .gap_3()
            .children(fields)
            .when_some(form.error.clone(), |body, error| {
                body.child(
                    div()
                        .pl(px(122.))
                        .text_size(px(12.))
                        .text_color(rgba(theme::ERROR))
                        .child(error),
                )
            })
            .child(
                div()
                    .pl(px(122.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(11.))
                    .text_color(rgba(theme::TEXT_FAINT))
                    .child("Tab moves between fields;")
                    .child(keycaps("Enter"))
                    .child(SharedString::from(form.view.submit_title.to_lowercase()))
                    .child("·")
                    .child(keycaps("Esc"))
                    .child("back"),
            )
            .into_any_element()
    }

    fn toggle_checkbox(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Screen::Form(form)) = self.screens.last_mut()
            && let Some(FieldInput::Checkbox { on, focus }) = form.inputs.get_mut(ix)
        {
            *on = !*on;
            window.focus(focus);
            form.error = None;
            cx.notify();
        }
    }

    fn choose_option(
        &mut self,
        ix: usize,
        option: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Screen::Form(form)) = self.screens.last_mut()
            && let Some(FieldInput::Dropdown { selected, focus }) = form.inputs.get_mut(ix)
        {
            *selected = option;
            window.focus(focus);
            form.error = None;
            cx.notify();
        }
    }
}

/// Key context of focused checkboxes and dropdowns, where Space and ←/→
/// change the choice (elsewhere Space types a space).
pub(super) const CHOICE_CONTEXT: &str = "FormChoice";

fn render_detail(view: &DetailView, scroll: &ScrollHandle) -> AnyElement {
    let text = div()
        .id("detail")
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .px_5()
        .py_4()
        .text_size(px(14.))
        .child(markdown::render(&view.markdown));
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(text)
        .when(!view.metadata.is_empty(), |body| {
            body.child(
                div()
                    .flex_none()
                    .w(px(220.))
                    .h_full()
                    .px_4()
                    .py_4()
                    .border_l_1()
                    .border_color(rgba(theme::HAIRLINE))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(view.metadata.iter().map(|(label, value)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgba(theme::TEXT_FAINT))
                                    .child(SharedString::from(label.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgba(theme::TEXT))
                                    .child(SharedString::from(value.clone())),
                            )
                    })),
            )
        })
        .into_any_element()
}
