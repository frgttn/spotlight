//! Animated scrolling: for any element that tracks a `ScrollHandle`
//! ([`Scroller`]), and for a `uniform_list` of fixed-height rows
//! ([`SmoothScroll`]).
//!
//! GPUI jumps straight to the new offset, both for `scroll_to_item` and for
//! mouse-wheel notches. This keeps a target offset instead and moves the list
//! towards it a little on every frame, easing out. Precise (touchpad) scrolling
//! already arrives in small pixel steps, so it is left to GPUI.
//!
//! Offsets follow GPUI's convention: 0 at the top, more negative further down.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    Context, DispatchPhase, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, ScrollDelta, ScrollHandle, ScrollStrategy, ScrollWheelEvent, Styled,
    UniformListScrollHandle, WeakEntity, canvas, point, px,
};

/// Time constant of the ease-out: after this long, about 63% of the distance
/// is covered, so a move settles in roughly 150 ms.
const TAU: f32 = 0.045;
/// Closer than this to the target, snap to it and stop.
const SETTLE_PX: f32 = 0.5;

/// Eases a scroll handle towards a target offset. Track [`Scroller::handle`]
/// on the scrolling element, feed it wheel notches with [`capture_wheel`],
/// and call [`Scroller::tick`] from `render`.
pub struct Scroller {
    handle: ScrollHandle,
    enabled: bool,
    target: Option<f32>,
    last_tick: Option<Instant>,
}

impl Scroller {
    pub fn new(handle: ScrollHandle, enabled: bool) -> Self {
        Self {
            handle,
            enabled,
            target: None,
            last_tick: None,
        }
    }

    pub fn handle(&self) -> ScrollHandle {
        self.handle.clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Turning it off finishes a running animation at once.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled && let Some(target) = self.target.take() {
            self.set_offset(target);
        }
    }

    /// Jumps to the top, for new content.
    pub fn reset(&mut self) {
        self.target = None;
        self.set_offset(0.);
    }

    /// Scrolls by `delta` (positive moves the content down, like GPUI's wheel
    /// delta), within the content GPUI last laid out.
    pub fn scroll_by(&mut self, delta: Pixels) {
        let max = f32::from(self.handle.max_offset().height);
        self.move_to((self.heading() + f32::from(delta)).clamp(-max, 0.));
    }

    /// How far down the content is scrolled, in pixels (0 at the top).
    pub fn scrolled(&self) -> f32 {
        -self.offset()
    }

    /// Advances the animation by one frame. Call it from `render`; returns
    /// whether another frame is needed.
    pub fn tick(&mut self) -> bool {
        let Some(target) = self.target else {
            return false;
        };
        let now = Instant::now();
        // The first frame of a move, and frames after a stall, count as one 60 Hz frame
        // at most 50 ms long, so a hitch doesn't turn into a jump.
        let dt = self
            .last_tick
            .map_or(Duration::from_millis(16), |last| now - last)
            .min(Duration::from_millis(50))
            .as_secs_f32();
        self.last_tick = Some(now);

        let current = self.offset();
        let next = current + (target - current) * (1. - (-dt / TAU).exp());
        if (target - next).abs() < SETTLE_PX {
            self.set_offset(target);
            self.target = None;
            self.last_tick = None;
            false
        } else {
            self.set_offset(next);
            true
        }
    }

    /// Where the content is heading: the target while moving, else where it is.
    fn heading(&self) -> f32 {
        self.target.unwrap_or_else(|| self.offset())
    }

    /// Animates to `to` (already clamped), or jumps there when turned off.
    fn move_to(&mut self, to: f32) {
        if self.enabled {
            if self.target.is_none() {
                self.last_tick = None;
            }
            self.target = Some(to);
        } else {
            self.set_offset(to);
        }
    }

    fn height(&self) -> f32 {
        f32::from(self.handle.bounds().size.height)
    }

    fn offset(&self) -> f32 {
        f32::from(self.handle.offset().y)
    }

    fn set_offset(&self, y: f32) {
        let x = self.handle.offset().x;
        self.handle.set_offset(point(x, px(y)));
    }
}

/// A [`Scroller`] for a `uniform_list`, which also scrolls rows into view.
pub struct SmoothScroll {
    handle: UniformListScrollHandle,
    row_height: f32,
    /// The list's vertical padding, on each side.
    padding: f32,
    /// How much of the list's top is hidden above its parent's edge: rows
    /// scrolled up stop below it.
    top_inset: f32,
    /// While the scroll thumb is dragged: where the pointer went down, and
    /// how far down the list was then (0 to 1).
    drag: Option<(f32, f32)>,
    scroller: Scroller,
}

impl SmoothScroll {
    pub fn new(handle: UniformListScrollHandle, row_height: f32, padding: f32) -> Self {
        let base = handle.0.borrow().base_handle.clone();
        Self {
            handle,
            row_height,
            padding,
            top_inset: 0.,
            drag: None,
            scroller: Scroller::new(base, true),
        }
    }

    pub fn handle(&self) -> UniformListScrollHandle {
        self.handle.clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.scroller.is_enabled()
    }

    /// Turning it off finishes a running animation at once.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.scroller.set_enabled(enabled);
    }

    pub fn set_top_inset(&mut self, inset: f32) {
        self.top_inset = inset;
    }

    /// Jumps to the top, for a new set of results.
    pub fn reset(&mut self) {
        self.scroller.target = None;
        self.handle.scroll_to_item(0, ScrollStrategy::Top);
    }

    /// Scrolls just enough to show row `ix` of `rows`.
    pub fn reveal(&mut self, ix: usize, rows: usize) {
        let viewport = self.viewport();
        if viewport <= 0. {
            // Not laid out yet: let GPUI place it on the next layout.
            self.handle.scroll_to_item(ix, ScrollStrategy::Top);
            return;
        }
        // Relative to where the list is heading, so holding ↓ doesn't fall behind.
        let from = self.scroller.heading();
        let top = ix as f32 * self.row_height;
        let bottom = top + self.row_height;
        let to = if top < -from + self.top_inset {
            -(top - self.top_inset)
        } else if bottom > -from + viewport {
            -(bottom - viewport)
        } else {
            return;
        };
        self.scroll_to(to, rows);
    }

    /// Whether GPUI has laid the list out yet, so its height is known.
    pub fn is_laid_out(&self) -> bool {
        self.scroller.height() > 0.
    }

    /// Where a scroll indicator's thumb goes for a list of `rows`, as
    /// fractions of the visible height: its top and its length. None while
    /// everything fits, or before the list is laid out.
    pub fn thumb(&self, rows: usize) -> Option<(f32, f32)> {
        let height = self.scroller.height();
        let visible = height - self.top_inset;
        let content = rows as f32 * self.row_height + 2. * self.padding;
        let max = content - height;
        if visible <= 0. || max <= 0.5 {
            return None;
        }
        let length = (visible / (content - self.top_inset)).clamp(0.08, 1.);
        let progress = (self.scroller.scrolled() / max).clamp(0., 1.);
        Some((progress * (1. - length), length))
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The pointer went down on the scroll track `at` this far down it (0 to
    /// 1), at window height `y`: off the thumb, the thumb jumps there first;
    /// then it follows the pointer.
    pub fn grab(&mut self, at: f32, y: f32, rows: usize) {
        let Some((top, length)) = self.thumb(rows) else {
            return;
        };
        if !(top..=top + length).contains(&at) {
            self.set_progress((at - length / 2.) / (1. - length).max(1e-3), rows);
        }
        self.drag = Some((y, self.progress(rows)));
    }

    /// The pointer moved to window height `y` while dragging the thumb along
    /// a track `track` tall.
    pub fn drag_to(&mut self, y: f32, track: f32, rows: usize) {
        let (Some((from, progress)), Some((_, length))) = (self.drag, self.thumb(rows)) else {
            return;
        };
        let travel = (track * (1. - length)).max(1.);
        self.set_progress(progress + (y - from) / travel, rows);
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    /// How far down the list is, 0 at the top and 1 at the bottom.
    fn progress(&self, rows: usize) -> f32 {
        let max = self.max_offset(rows);
        if max <= 0. {
            0.
        } else {
            (self.scroller.scrolled() / max).clamp(0., 1.)
        }
    }

    /// Jumps (no animation: the pointer leads) to `progress` down the list.
    fn set_progress(&mut self, progress: f32, rows: usize) {
        self.scroller.target = None;
        self.scroller
            .set_offset(-(progress.clamp(0., 1.) * self.max_offset(rows)));
    }

    fn max_offset(&self, rows: usize) -> f32 {
        (rows as f32 * self.row_height + 2. * self.padding - self.scroller.height()).max(0.)
    }

    /// Scrolls by `delta` (positive moves the content down, like GPUI's wheel delta).
    pub fn scroll_by(&mut self, delta: Pixels, rows: usize) {
        let from = self.scroller.heading();
        self.scroll_to(from + f32::from(delta), rows);
    }

    /// Advances the animation by one frame. Call it from `render`; returns
    /// whether another frame is needed.
    pub fn tick(&mut self) -> bool {
        self.scroller.tick()
    }

    fn scroll_to(&mut self, to: f32, rows: usize) {
        let max = (rows as f32 * self.row_height - self.viewport()).max(0.);
        self.scroller.move_to(to.clamp(-max, 0.));
    }

    /// Height available to rows, inside the list's padding.
    fn viewport(&self) -> f32 {
        self.scroller.height() - 2. * self.padding
    }
}

/// An overlay for a scrolling element that takes mouse-wheel notches before
/// the element sees them (capture phase) and hands them to `scroll` instead.
/// When `scroll` returns `false` (smooth scrolling is off) the notch falls
/// through to GPUI, as does precise scrolling. Place it absolutely over the
/// element, in a parent that doesn't scroll.
/// Lets the pointer drag a [`SmoothScroll`] list's thumb, or click its track
/// to jump: place it absolutely over the track. `list` gives the view's
/// list and its number of rows. Moves are followed anywhere in the window
/// while the button is held.
pub fn drag_track<T: 'static>(
    view: WeakEntity<T>,
    list: impl Fn(&mut T) -> (&mut SmoothScroll, usize) + 'static,
) -> impl IntoElement {
    let list = Rc::new(list);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let height = f32::from(bounds.size.height);
            let (down_view, down_list) = (view.clone(), list.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !bounds.contains(&event.position)
                    || height <= 0.
                {
                    return;
                }
                let at = f32::from(event.position.y - bounds.origin.y) / height;
                let y = f32::from(event.position.y);
                down_view
                    .update(cx, |this, cx| {
                        let (scroll, rows) = down_list(this);
                        scroll.grab(at, y, rows);
                        cx.notify();
                    })
                    .ok();
                cx.stop_propagation();
            });
            let (move_view, move_list) = (view.clone(), list.clone());
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let y = f32::from(event.position.y);
                let held = event.pressed_button == Some(MouseButton::Left);
                move_view
                    .update(cx, |this, cx| {
                        let (scroll, rows) = move_list(this);
                        if !scroll.is_dragging() {
                            return;
                        }
                        if held {
                            scroll.drag_to(y, height, rows);
                        } else {
                            scroll.release();
                        }
                        cx.notify();
                    })
                    .ok();
            });
            let (up_view, up_list) = (view.clone(), list.clone());
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                    return;
                }
                up_view
                    .update(cx, |this, cx| {
                        let (scroll, _) = up_list(this);
                        if scroll.is_dragging() {
                            scroll.release();
                            cx.notify();
                        }
                    })
                    .ok();
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

pub fn capture_wheel<T: 'static>(
    view: WeakEntity<T>,
    scroll: impl Fn(&mut T, Pixels, &mut Context<T>) -> bool + 'static,
) -> impl IntoElement {
    let scroll = Rc::new(scroll);
    canvas(
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
                let handled = view
                    .update(cx, |this, cx| scroll(this, delta, cx))
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
    .size_full()
}
