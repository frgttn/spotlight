//! Text input with IME support: one line, or several that wrap
//! ([`TextInput::multiline`]).
//!
//! Adapted from gpui 0.2.2 `examples/input.rs` (Apache-2.0, Zed Industries).
//! Changes: the usual Windows editing keys (word-wise movement, selection and
//! deletion with Ctrl), double and triple click, launcher styling, a text inset
//! for the caret, a change event, a fixed `character_index_for_point`, the
//! multiline mode, no demo app.

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, ScrollWheelEvent, ShapedLine, SharedString, Style,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill,
    hsla, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

actions!(
    text_input,
    [
        Backspace,
        DeleteWordLeft,
        Delete,
        DeleteWordRight,
        Left,
        Right,
        WordLeft,
        WordRight,
        SelectLeft,
        SelectRight,
        SelectWordLeft,
        SelectWordRight,
        SelectAll,
        Home,
        End,
        SelectToHome,
        SelectToEnd,
        DocumentStart,
        DocumentEnd,
        SelectToDocumentStart,
        SelectToDocumentEnd,
        Up,
        Down,
        SelectUp,
        SelectDown,
        Newline,
        Paste,
        Cut,
        Copy,
    ]
);

const CONTEXT: &str = "TextInput";
/// Added to [`CONTEXT`] in a multiline input, whose Enter and ↑/↓ edit
/// instead of reaching the views around it.
const MULTILINE: &str = "multiline";
const MULTILINE_CONTEXT: &str = "TextInput && multiline";
/// A multiline input grows with its text between these many rows, then scrolls.
const MIN_ROWS: usize = 3;
const MAX_ROWS: usize = 6;
/// How wide a selected line break is drawn.
const NEWLINE_WIDTH: f32 = 5.;
/// Gap between the field's left edge and the text, so the caret at offset 0
/// sits in front of the first glyph instead of on top of it.
const TEXT_INSET: f32 = 3.;
const CURSOR_WIDTH: f32 = 2.;

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        // Binding it also stops Windows' Ctrl+Backspace DEL (0x7F) char from being typed.
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("ctrl-delete", DeleteWordRight, Some(CONTEXT)),
        KeyBinding::new("left", Left, Some(CONTEXT)),
        KeyBinding::new("right", Right, Some(CONTEXT)),
        KeyBinding::new("ctrl-left", WordLeft, Some(CONTEXT)),
        KeyBinding::new("ctrl-right", WordRight, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        KeyBinding::new("ctrl-v", Paste, Some(CONTEXT)),
        KeyBinding::new("shift-insert", Paste, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-insert", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-x", Cut, Some(CONTEXT)),
        KeyBinding::new("shift-delete", Cut, Some(CONTEXT)),
        // Home/End: the start or end of the row; with Ctrl, of all the text.
        KeyBinding::new("home", Home, Some(CONTEXT)),
        KeyBinding::new("ctrl-home", DocumentStart, Some(CONTEXT)),
        KeyBinding::new("end", End, Some(CONTEXT)),
        KeyBinding::new("ctrl-end", DocumentEnd, Some(CONTEXT)),
        KeyBinding::new("shift-home", SelectToHome, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-home", SelectToDocumentStart, Some(CONTEXT)),
        KeyBinding::new("shift-end", SelectToEnd, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-end", SelectToDocumentEnd, Some(CONTEXT)),
        // Only in a multiline input; elsewhere they reach the views around it.
        KeyBinding::new("enter", Newline, Some(MULTILINE_CONTEXT)),
        KeyBinding::new("shift-enter", Newline, Some(MULTILINE_CONTEXT)),
        KeyBinding::new("up", Up, Some(MULTILINE_CONTEXT)),
        KeyBinding::new("down", Down, Some(MULTILINE_CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(MULTILINE_CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(MULTILINE_CONTEXT)),
    ]);
}

/// Emitted whenever the text (including in-progress IME composition) changes.
pub struct Changed;

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// ←/→ go to the parent view instead of moving the cursor (a grid uses them).
    pass_horizontal_arrows: bool,
    /// Several lines that wrap; Enter starts a new one.
    multiline: bool,
    /// Multiline: the lines as last laid out, and their row height.
    lines: Vec<Line>,
    line_height: Pixels,
    /// Multiline: how far the text is scrolled up.
    scroll_y: Pixels,
    /// Multiline: scroll the caret into view on the next frame.
    reveal: bool,
    /// Multiline: the x ↑/↓ keep to, from where they started.
    goal_x: Option<Pixels>,
}

/// One `\n`-separated line of a multiline input, wrapped to its width.
struct Line {
    /// Byte offset of its first character in the text.
    start: usize,
    /// Its first row's distance from the top of the text.
    top: Pixels,
    layout: WrappedLine,
}

impl Line {
    fn rows(&self) -> usize {
        self.layout.wrap_boundaries.len() + 1
    }

    /// Where each row starts, as offsets into the line.
    fn row_starts(&self) -> impl Iterator<Item = usize> + '_ {
        let runs = &self.layout.unwrapped_layout.runs;
        std::iter::once(0).chain(
            self.layout
                .wrap_boundaries
                .iter()
                .map(|boundary| runs[boundary.run_ix].glyphs[boundary.glyph_ix].index),
        )
    }
}

impl EventEmitter<Changed> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            pass_horizontal_arrows: false,
            multiline: false,
            lines: Vec::new(),
            line_height: px(20.),
            scroll_y: px(0.),
            reveal: false,
            goal_x: None,
        }
    }

    /// An input for text over several lines: long ones wrap, Enter starts a
    /// new one, ↑/↓ move between rows. It grows with its text, up to a point.
    pub fn multiline(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            multiline: true,
            ..Self::new(placeholder, cx)
        }
    }

    /// `text` as this input keeps it: line breaks as `\n` in a multiline
    /// input, spaces in a single-line one.
    fn normalize(&self, text: &str) -> String {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.multiline {
            text
        } else {
            text.replace('\n', " ")
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replaces the text, all of it selected (so typing replaces it), without
    /// emitting [`Changed`].
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text: SharedString = text.into();
        self.content = self.normalize(&text).into();
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_y = px(0.);
        cx.notify();
    }

    /// Replaces the text with the cursor at its end and emits [`Changed`],
    /// as if it had been typed.
    pub fn type_over(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.reveal = true;
        cx.emit(Changed);
        cx.notify();
    }

    /// Lets ←/→ (without modifiers) through to the views around the field.
    pub fn set_pass_horizontal_arrows(&mut self, pass: bool) {
        self.pass_horizontal_arrows = pass;
    }

    /// The grey text shown while the field is empty.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// Resets the text without emitting [`Changed`]; the caller refreshes its own state.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.is_selecting = false;
        self.scroll_y = px(0.);
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.pass_horizontal_arrows {
            return cx.propagate();
        }
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.pass_horizontal_arrows {
            return cx.propagate();
        }
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    /// As in a Windows edit box, a selection collapses to the side it's moved from.
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let from = if self.selected_range.is_empty() {
            self.cursor_offset()
        } else {
            self.selected_range.start
        };
        self.move_to(previous_word_start(&self.content, from), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let from = if self.selected_range.is_empty() {
            self.cursor_offset()
        } else {
            self.selected_range.end
        };
        self.move_to(next_word_start(&self.content, from), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_word_start(&self.content, self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_word_start(&self.content, self.cursor_offset()), cx);
    }

    fn select_to_home(&mut self, _: &SelectToHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_edge(self.cursor_offset(), false), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_edge(self.cursor_offset(), true), cx);
    }

    fn document_start(&mut self, _: &DocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_to_document_start(
        &mut self,
        _: &SelectToDocumentStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }

    fn select_to_document_end(
        &mut self,
        _: &SelectToDocumentEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.content.len(), cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(false, false, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(true, false, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(false, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(true, true, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    /// Moves (or selects) one row up or down from the caret, keeping to the
    /// x it started from. Past the first or last row: the start or end of
    /// the text.
    fn move_vertically(&mut self, down: bool, select: bool, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let Some(at) = position_in(&self.lines, cursor, self.line_height) else {
            return;
        };
        let goal = self.goal_x.unwrap_or(at.x);
        let y = if down {
            at.y + self.line_height * 1.5
        } else {
            at.y - self.line_height * 0.5
        };
        let target = if y < px(0.) {
            0
        } else if y >= text_height(&self.lines, self.line_height) {
            self.content.len()
        } else {
            offset_in(&self.lines, point(goal, y), self.line_height).min(self.content.len())
        };
        if select {
            self.select_to(target, cx);
        } else {
            self.move_to(target, cx);
        }
        self.goal_x = Some(goal);
    }

    /// The start (or end) of the row `offset` is on: the whole text in a
    /// single-line input, the wrapped row in a multiline one.
    fn row_edge(&self, offset: usize, end: bool) -> usize {
        if !self.multiline {
            return if end { self.content.len() } else { 0 };
        }
        let Some(at) = position_in(&self.lines, offset, self.line_height) else {
            return offset;
        };
        let x = if end { px(1e6) } else { px(0.) };
        let row_middle = at.y + self.line_height / 2.;
        offset_in(&self.lines, point(x, row_middle), self.line_height).min(self.content.len())
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    /// Deletes the selection, or the word before the cursor.
    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            self.selected_range = previous_word_start(&self.content, cursor)..cursor;
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    /// Deletes the selection, or up to the start of the next word.
    fn delete_word_right(
        &mut self,
        _: &DeleteWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            self.selected_range = cursor..next_word_start(&self.content, cursor);
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.click_count {
            2 => {
                self.selected_range =
                    word_at(&self.content, self.index_for_mouse_position(event.position));
                self.selection_reversed = false;
                cx.notify();
                return;
            }
            3.. => {
                self.selected_range = if self.multiline {
                    paragraph_at(&self.content, self.index_for_mouse_position(event.position))
                } else {
                    0..self.content.len()
                };
                self.selection_reversed = false;
                cx.notify();
                return;
            }
            _ => {}
        }
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    /// Multiline: the wheel scrolls the text (clamped when it's next laid out).
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(self.line_height).y;
        self.scroll_y = (self.scroll_y - delta).max(px(0.));
        cx.stop_propagation();
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = self.normalize(&text);
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.goal_x = None;
        self.reveal = true;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.multiline {
            let Some(bounds) = self.last_bounds else {
                return 0;
            };
            let local = point(
                position.x - bounds.left() - px(TEXT_INSET),
                position.y - bounds.top() + self.scroll_y,
            );
            return offset_in(&self.lines, local, self.line_height).min(self.content.len());
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left() - px(TEXT_INSET))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.goal_x = None;
        self.reveal = true;
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

/// Word segments of `text` by Unicode rules (UAX #29): (start, end, is whitespace).
/// Punctuation forms segments of its own; `foo.bar` stays one word.
fn segments(text: &str) -> impl DoubleEndedIterator<Item = (usize, usize, bool)> + '_ {
    text.split_word_bound_indices()
        .map(|(start, word)| (start, start + word.len(), word.trim().is_empty()))
}

/// Ctrl+Left: back over spaces, then to the start of the word before them.
fn previous_word_start(text: &str, offset: usize) -> usize {
    segments(text)
        .rev()
        .filter(|&(start, _, _)| start < offset)
        .find(|&(_, _, space)| !space)
        .map_or(0, |(start, _, _)| start)
}

/// Ctrl+Right, the Windows way: past the rest of the current word and the
/// spaces after it, to the start of the next word.
fn next_word_start(text: &str, offset: usize) -> usize {
    let mut ahead = segments(text)
        .filter(|&(_, end, _)| end > offset)
        .peekable();
    // Skip the word the cursor is in (or at the start of), if any.
    ahead.next_if(|&(_, _, space)| !space);
    ahead
        .find(|&(_, _, space)| !space)
        .map_or(text.len(), |(start, _, _)| start)
}

/// The word (or run of spaces) under `offset`, for double-click. Past the end
/// of the text, the last one.
fn word_at(text: &str, offset: usize) -> Range<usize> {
    segments(text)
        .find(|&(start, end, _)| start <= offset && offset < end)
        .or_else(|| segments(text).next_back())
        .map_or(0..0, |(start, end, _)| start..end)
}

/// The `\n`-separated line under `offset`, without its line break, for
/// triple-click in a multiline input.
fn paragraph_at(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |ix| offset + ix);
    start..end
}

/// The height of all the rows of `lines`.
fn text_height(lines: &[Line], line_height: Pixels) -> Pixels {
    lines
        .last()
        .map_or(px(0.), |line| line.top + line_height * line.rows())
}

/// Where the caret goes for `offset`, from the top left of the text. At a
/// wrap, the end of the upper row. `None` before the first layout.
fn position_in(lines: &[Line], offset: usize, line_height: Pixels) -> Option<Point<Pixels>> {
    let line = lines.iter().rev().find(|line| line.start <= offset)?;
    let local = (offset - line.start).min(line.layout.len());
    let at = line
        .layout
        .position_for_index(local, line_height)
        .unwrap_or_default();
    Some(point(at.x, line.top + at.y))
}

/// The offset closest to `position` (from the top left of the text). Above
/// the text it's on the first row, below it on the last.
fn offset_in(lines: &[Line], position: Point<Pixels>, line_height: Pixels) -> usize {
    let Some(first) = lines.first() else {
        return 0;
    };
    let line = lines
        .iter()
        .rev()
        .find(|line| line.top <= position.y)
        .unwrap_or(first);
    // Inside the line's rows: below the last one `closest_index_for_position`
    // gives up and says 0.
    let bottom = line_height * line.rows() - px(1.);
    let local = point(
        position.x.max(px(0.)),
        (position.y - line.top).clamp(px(0.), bottom),
    );
    match line.layout.closest_index_for_position(local, line_height) {
        Ok(ix) | Err(ix) => line.start + ix,
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.goal_x = None;
        self.reveal = true;
        cx.emit(Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.goal_x = None;
        self.reveal = true;

        cx.emit(Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if self.multiline {
            let origin = point(bounds.left() + px(TEXT_INSET), bounds.top() - self.scroll_y);
            let start = position_in(&self.lines, range.start, self.line_height)?;
            let end = position_in(&self.lines, range.end, self.line_height)?;
            // On the start's row; the IME only needs to know where to show.
            let end_x = if end.y == start.y { end.x } else { start.x };
            return Some(Bounds::from_corners(
                origin + start,
                origin + point(end_x, start.y + self.line_height),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        let left = bounds.left() + px(TEXT_INSET);
        Some(Bounds::from_corners(
            point(left + last_layout.x_for_index(range.start), bounds.top()),
            point(left + last_layout.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        // `localize` already makes the point relative to the field. (The gpui
        // example subtracted it from the absolute x, which always yields the left edge.)
        let local = self.last_bounds?.localize(&point)?;
        if self.multiline {
            let local = gpui::point(local.x - px(TEXT_INSET), local.y + self.scroll_y);
            let offset = offset_in(&self.lines, local, self.line_height);
            return Some(self.offset_to_utf16(offset.min(self.content.len())));
        }
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(local.x - px(TEXT_INSET))?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

impl TextInput {
    /// The text to draw (the placeholder while empty) and its runs, with the
    /// IME composition underlined.
    fn display(&self, window: &Window) -> (SharedString, Vec<TextRun>) {
        let style = window.text_style();
        let (display_text, text_color) = if self.content.is_empty() {
            (self.placeholder.clone(), hsla(0., 0., 1., 0.5))
        } else {
            (self.content.clone(), style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = self.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };
        (display_text, runs)
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let (display_text, runs) = input.display(window);

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let text_left = bounds.left() + px(TEXT_INSET);
        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        // Centered on the boundary between glyphs.
                        point(text_left + cursor_pos - px(CURSOR_WIDTH / 2.), bounds.top()),
                        size(px(CURSOR_WIDTH), bounds.bottom() - bounds.top()),
                    ),
                    rgb(0x6aa8ff),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            text_left + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            text_left + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba(0x6aa8ff55),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        let origin = point(bounds.left() + px(TEXT_INSET), bounds.top());
        line.paint(origin, window.line_height(), window, cx)
            .unwrap();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

/// The text of a multiline input: wrapped rows, as many as it needs between
/// [`MIN_ROWS`] and [`MAX_ROWS`], scrolled to keep the caret in view.
struct AreaElement {
    input: Entity<TextInput>,
}

struct AreaPrepaint {
    lines: Vec<Line>,
    scroll_y: Pixels,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for AreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The width text wraps at inside an area `width` wide.
fn wrap_width(width: Pixels) -> Pixels {
    (width - px(TEXT_INSET * 2.)).max(px(1.))
}

impl Element for AreaElement {
    type RequestLayoutState = ();
    type PrepaintState = AreaPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (text, runs) = self.input.read(cx).display(window);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        // Its height depends on how many rows the text wraps to at its width.
        let layout = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let rows: usize = window
                .text_system()
                .shape_text(text.clone(), font_size, &runs, width.map(wrap_width), None)
                .map(|lines| {
                    lines
                        .iter()
                        .map(|line| line.wrap_boundaries.len() + 1)
                        .sum()
                })
                .unwrap_or(1);
            size(
                width.unwrap_or_default(),
                line_height * rows.clamp(MIN_ROWS, MAX_ROWS),
            )
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let (text, runs) = input.display(window);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        let shaped = window
            .text_system()
            .shape_text(
                text,
                font_size,
                &runs,
                Some(wrap_width(bounds.size.width)),
                None,
            )
            .unwrap_or_default();
        let mut lines = Vec::with_capacity(shaped.len());
        let (mut start, mut top) = (0, px(0.));
        for layout in shaped {
            let line = Line { start, top, layout };
            start += line.layout.len() + 1;
            top += line_height * line.rows();
            lines.push(line);
        }

        // Keep the caret in view after it moved; leave a wheel scroll alone.
        let caret = position_in(&lines, cursor, line_height).unwrap_or_default();
        let mut scroll_y = input.scroll_y;
        if input.reveal {
            if caret.y < scroll_y {
                scroll_y = caret.y;
            } else if caret.y + line_height > scroll_y + bounds.size.height {
                scroll_y = caret.y + line_height - bounds.size.height;
            }
        }
        let max_scroll = (text_height(&lines, line_height) - bounds.size.height).max(px(0.));
        scroll_y = scroll_y.clamp(px(0.), max_scroll);

        let origin = point(bounds.left() + px(TEXT_INSET), bounds.top() - scroll_y);
        let (cursor, selection) = if selected_range.is_empty() {
            let quad = fill(
                Bounds::new(
                    origin + point(caret.x - px(CURSOR_WIDTH / 2.), caret.y),
                    size(px(CURSOR_WIDTH), line_height),
                ),
                rgb(0x6aa8ff),
            );
            (Some(quad), Vec::new())
        } else {
            let quads = selection_rows(&lines, &selected_range, line_height)
                .map(|row| fill(row + origin, rgba(0x6aa8ff55)))
                .collect();
            (None, quads)
        };
        AreaPrepaint {
            lines,
            scroll_y,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let line_height = window.line_height();
        let scroll_y = prepaint.scroll_y;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            for line in &prepaint.lines {
                let origin = point(
                    bounds.left() + px(TEXT_INSET),
                    bounds.top() + line.top - scroll_y,
                );
                // Rows above or below the field aren't drawn.
                let bottom = origin.y + line_height * line.rows();
                if bottom < bounds.top() || origin.y > bounds.bottom() {
                    continue;
                }
                line.layout
                    .paint(origin, line_height, TextAlign::Left, None, window, cx)
                    .unwrap();
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });

        let lines = std::mem::take(&mut prepaint.lines);
        self.input.update(cx, |input, _cx| {
            input.lines = lines;
            input.line_height = line_height;
            input.scroll_y = scroll_y;
            input.reveal = false;
            input.last_bounds = Some(bounds);
        });
    }
}

/// The selected part of each row, from the top left of the text. A selected
/// line break shows as a short block at the end of its row.
fn selection_rows<'a>(
    lines: &'a [Line],
    selected: &'a Range<usize>,
    line_height: Pixels,
) -> impl Iterator<Item = Bounds<Pixels>> + 'a {
    lines.iter().flat_map(move |line| {
        let layout = &line.layout.unwrapped_layout;
        let starts: Vec<usize> = line.row_starts().collect();
        let line_len = line.layout.len();
        (0..starts.len()).filter_map(move |row| {
            let row_start = starts[row];
            let row_end = starts.get(row + 1).copied().unwrap_or(line_len);
            let last_row = row + 1 == starts.len();
            let from = selected.start.saturating_sub(line.start).max(row_start);
            let to = selected.end.saturating_sub(line.start).min(row_end);
            let breaks_line = last_row && selected.end > line.start + line_len;
            if selected.end <= line.start + row_start
                || selected.start > line.start + row_end
                || (from >= to && !breaks_line)
            {
                return None;
            }
            let row_x = layout.x_for_index(row_start);
            let left = layout.x_for_index(from.min(to)) - row_x;
            let mut right = layout.x_for_index(to) - row_x;
            if breaks_line {
                right += px(NEWLINE_WIDTH);
            }
            let top = line.top + line_height * row;
            Some(Bounds::from_corners(
                point(left, top),
                point(right, top + line_height),
            ))
        })
    })
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut context = gpui::KeyContext::new_with_defaults();
        context.add(CONTEXT);
        if self.multiline {
            context.add(MULTILINE);
        }
        div()
            .flex()
            .w_full()
            .key_context(context)
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_to_home))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::document_start))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::select_to_document_start))
            .on_action(cx.listener(Self::select_to_document_end))
            .when(self.multiline, |input| {
                input
                    .on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::select_up))
                    .on_action(cx.listener(Self::select_down))
                    .on_action(cx.listener(Self::newline))
                    .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            })
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .map(|input| {
                if self.multiline {
                    input.child(AreaElement { input: cx.entity() })
                } else {
                    input.child(TextElement { input: cx.entity() })
                }
            })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_left_goes_to_word_starts() {
        let text = "visual studio  code";
        assert_eq!(previous_word_start(text, text.len()), 15);
        assert_eq!(previous_word_start(text, 15), 7);
        assert_eq!(previous_word_start(text, 9), 7);
        assert_eq!(previous_word_start(text, 7), 0);
        assert_eq!(previous_word_start(text, 0), 0);
        assert_eq!(previous_word_start("   ", 3), 0);
    }

    #[test]
    fn ctrl_right_goes_past_word_and_spaces() {
        let text = "visual studio  code";
        assert_eq!(next_word_start(text, 0), 7);
        assert_eq!(next_word_start(text, 3), 7);
        assert_eq!(next_word_start(text, 6), 7);
        assert_eq!(next_word_start(text, 7), 15);
        assert_eq!(next_word_start(text, 15), text.len());
        assert_eq!(next_word_start(text, text.len()), text.len());
    }

    #[test]
    fn punctuation_and_non_latin_words() {
        let text = "привет, мир 1+2";
        assert_eq!(next_word_start(text, 0), "привет".len());
        assert_eq!(
            previous_word_start(text, text.len()),
            "привет, мир 1+".len()
        );
        assert_eq!(word_at(text, 2), 0.."привет".len());
        assert_eq!(previous_word_start("github.com", 10), 0);
    }

    #[test]
    fn triple_click_in_a_multiline_input_picks_the_line() {
        let text = "first\nsecond line\n\nlast";
        assert_eq!(paragraph_at(text, 0), 0..5);
        assert_eq!(paragraph_at(text, 5), 0..5);
        assert_eq!(paragraph_at(text, 9), 6..17);
        assert_eq!(paragraph_at(text, 18), 18..18);
        assert_eq!(paragraph_at(text, 99), 19..23);
    }

    #[test]
    fn double_click_picks_the_word_under_the_pointer() {
        let text = "visual studio";
        assert_eq!(word_at(text, 9), 7..13);
        assert_eq!(word_at(text, 6), 6..7);
        assert_eq!(word_at(text, 99), 7..13);
        assert_eq!(word_at("", 0), 0..0);
    }
}
