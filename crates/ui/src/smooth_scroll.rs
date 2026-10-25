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
    Context, DispatchPhase, IntoElement, Pixels, ScrollDelta, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, Styled, UniformListScrollHandle, WeakEntity, canvas, point, px,
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
