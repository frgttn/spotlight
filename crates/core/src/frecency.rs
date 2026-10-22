//! Frecency: how often and how recently an item was used, as one number.
//!
//! Each use adds 1 to a score that halves every [`HALF_LIFE_SECS`]. Storing
//! the score together with the time it was last updated is enough to compute
//! the current value at any later moment, so nothing has to be recomputed in
//! the background.

use std::time::{SystemTime, UNIX_EPOCH};

/// A use a week ago counts half as much as a use now.
pub const HALF_LIFE_SECS: f64 = 7. * 24. * 3600.;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frecency {
    /// Decayed score as of `last_used`.
    pub score: f64,
    /// Unix seconds of the last use; 0 for an item never used.
    pub last_used: i64,
    /// Total number of uses, undecayed. Not used for ranking.
    pub count: u32,
}

impl Frecency {
    /// The score decayed to `now` (unix seconds).
    pub fn score_at(&self, now: i64) -> f64 {
        let elapsed = (now - self.last_used).max(0) as f64;
        self.score * 0.5f64.powf(elapsed / HALF_LIFE_SECS)
    }

    /// The state after one more use at `now`.
    pub fn visit(self, now: i64) -> Self {
        Self {
            score: self.score_at(now) + 1.,
            last_used: now,
            count: self.count + 1,
        }
    }
}

/// Current time in unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEK: i64 = HALF_LIFE_SECS as i64;

    #[test]
    fn halves_every_half_life() {
        let used = Frecency::default().visit(1_000);
        assert_eq!(used.score_at(1_000), 1.);
        assert!((used.score_at(1_000 + WEEK) - 0.5).abs() < 1e-9);
        assert!((used.score_at(1_000 + 2 * WEEK) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn visits_accumulate_with_decay() {
        let twice = Frecency::default().visit(0).visit(WEEK);
        assert!((twice.score - 1.5).abs() < 1e-9);
        assert_eq!(twice.count, 2);
        assert_eq!(twice.last_used, WEEK);
    }

    #[test]
    fn clock_going_backwards_does_not_inflate() {
        let used = Frecency::default().visit(1_000);
        assert_eq!(used.score_at(0), 1.);
    }
}
