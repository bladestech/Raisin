//! How long the battery should last, from the history in history.rs.
//! Shown as a rough "about 3 weeks left"; never for the Li-ion profile.

use crate::history::{Cell, Sample};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// Seconds until 0%, counted from `now`.
    pub remaining_secs: u64,
    /// Average drain over the current discharge, percent per day.
    pub percent_per_day: f32,
}

/// Need at least this much time in the current discharge...
pub const MIN_SPAN: u64 = 12 * 60 * 60;
/// ...and at least this many percentage points of drop.
pub const MIN_DROP: u8 = 3;
/// A rise this large between two readings means fresh batteries or a
/// charge we didn't see: the discharge starts there.
pub const RESET_RISE: u8 = 10;
/// Estimates longer than this aren't shown.
pub const MAX_REMAINING: u64 = 365 * 24 * 60 * 60;

pub fn estimate(samples: &[Sample], now: u64) -> Option<Estimate> {
    unimplemented!()
}

/// "about 5 hours", "about 3 days", "about 2 weeks", "about 4 months".
pub fn describe(secs: u64) -> String {
    unimplemented!()
}
