//! Animated scrolling for a `uniform_list` of fixed-height rows.
//!
//! GPUI jumps straight to the new offset, both for `scroll_to_item` and for
//! mouse-wheel notches. This keeps a target offset instead and moves the list
//! towards it a little on every frame, easing out. Precise (touchpad) scrolling
//! already arrives in small pixel steps, so it is left to GPUI.
//!
//! Offsets follow GPUI's convention: 0 at the top, more negative further down.

use std::time::{Duration, Instant};

use gpui::{Pixels, ScrollStrategy, UniformListScrollHandle, point, px};

/// Time constant of the ease-out: after this long, about 63% of the distance
/// is covered, so a move settles in roughly 150 ms.
const TAU: f32 = 0.045;
/// Closer than this to the target, snap to it and stop.
const SETTLE_PX: f32 = 0.5;

pub struct SmoothScroll {
    handle: UniformListScrollHandle,
    row_height: f32,
    /// The list's vertical padding, on each side.
    padding: f32,
    enabled: bool,
    target: Option<f32>,
    last_tick: Option<Instant>,
}

impl SmoothScroll {
    pub fn new(handle: UniformListScrollHandle, row_height: f32, padding: f32) -> Self {
        Self {
            handle,
            row_height,
            padding,
            enabled: true,
            target: None,
            last_tick: None,
        }
    }

    pub fn handle(&self) -> UniformListScrollHandle {
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

    /// Jumps to the top, for a new set of results.
    pub fn reset(&mut self) {
        self.target = None;
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
        let from = self.target.unwrap_or_else(|| self.offset());
        let top = ix as f32 * self.row_height;
        let bottom = top + self.row_height;
        let to = if top < -from {
            -top
        } else if bottom > -from + viewport {
            -(bottom - viewport)
        } else {
            return;
        };
        self.scroll_to(to, rows);
    }

    /// Scrolls by `delta` (positive moves the content down, like GPUI's wheel delta).
    pub fn scroll_by(&mut self, delta: Pixels, rows: usize) {
        let from = self.target.unwrap_or_else(|| self.offset());
        self.scroll_to(from + f32::from(delta), rows);
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

    fn scroll_to(&mut self, to: f32, rows: usize) {
        let max = (rows as f32 * self.row_height - self.viewport()).max(0.);
        let to = to.clamp(-max, 0.);
        if self.enabled {
            if self.target.is_none() {
                self.last_tick = None;
            }
            self.target = Some(to);
        } else {
            self.set_offset(to);
        }
    }

    /// Height available to rows, inside the list's padding.
    fn viewport(&self) -> f32 {
        let height = self.handle.0.borrow().base_handle.bounds().size.height;
        f32::from(height) - 2. * self.padding
    }

    fn offset(&self) -> f32 {
        f32::from(self.handle.0.borrow().base_handle.offset().y)
    }

    fn set_offset(&self, y: f32) {
        let state = self.handle.0.borrow();
        let x = state.base_handle.offset().x;
        state.base_handle.set_offset(point(x, px(y)));
    }
}
