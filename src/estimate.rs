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
    let newest = *samples.last()?;
    if newest.charging || newest.cell == Cell::Liion {
        return None;
    }

    // Walk back from the newest reading to the start of this discharge.
    let mut start = samples.len() - 1;
    while start > 0 {
        let prev = samples[start - 1];
        let cur = samples[start];
        let big_rise = cur.percent as u16 >= prev.percent as u16 + RESET_RISE as u16;
        if prev.charging || prev.cell != newest.cell || big_rise {
            break;
        }
        start -= 1;
    }

    let first = samples[start];
    if newest.time.saturating_sub(first.time) < MIN_SPAN {
        return None;
    }
    if (first.percent as u16) < newest.percent as u16 + MIN_DROP as u16 {
        return None;
    }

    // Least-squares line percent = a + b * t, with t in seconds from `first`.
    let run = &samples[start..];
    let n = run.len() as f64;
    let ts: Vec<f64> = run.iter().map(|s| s.time.saturating_sub(first.time) as f64).collect();
    let ys: Vec<f64> = run.iter().map(|s| s.percent as f64).collect();
    let mean_t = ts.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let sxy: f64 = ts.iter().zip(&ys).map(|(t, y)| (t - mean_t) * (y - mean_y)).sum();
    let sxx: f64 = ts.iter().map(|t| (t - mean_t) * (t - mean_t)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let b = sxy / sxx;
    if b.is_nan() || b >= 0.0 {
        return None;
    }

    // Seconds from the newest reading down to 0%, less the time already passed.
    let to_zero = newest.percent as f64 / -b;
    if to_zero > MAX_REMAINING as f64 {
        return None;
    }
    let elapsed = now.saturating_sub(newest.time) as f64;
    let remaining = (to_zero - elapsed).max(0.0) as u64;

    Some(Estimate {
        remaining_secs: remaining,
        percent_per_day: (-b * 86_400.0) as f32,
    })
}

/// "about 5 hours", "about 3 days", "about 2 weeks", "about 4 months".
pub fn describe(secs: u64) -> String {
    const HOUR: u64 = 60 * 60;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;

    // Each unit is picked from the value rounded to whole units, so a value
    // just under a boundary never reads as the next unit's number.
    if secs < HOUR {
        return "less than an hour".to_string();
    }
    let hours = (secs + HOUR / 2) / HOUR;
    if hours < 48 {
        return if hours == 1 {
            "about 1 hour".to_string()
        } else {
            format!("about {hours} hours")
        };
    }
    let days = (secs + DAY / 2) / DAY;
    if days < 14 {
        return format!("about {days} days");
    }
    if days < 60 {
        let weeks = (secs + WEEK / 2) / WEEK;
        return format!("about {weeks} weeks");
    }
    let months = (secs + MONTH / 2) / MONTH;
    format!("about {months} months")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn sample(time: u64, percent: u8, charging: bool, cell: Cell) -> Sample {
        Sample {
            time,
            percent,
            voltage_mv: None,
            charging,
            cell,
        }
    }

    /// Samples every `step` seconds from 0 to `days`, starting at 100%
    /// and draining `per_day` percentage points per day.
    fn steady(days: u64, per_day: f64, step: u64) -> Vec<Sample> {
        let end = days * DAY;
        (0..=end / step)
            .map(|k| {
                let t = k * step;
                let pct = (100.0 - per_day * t as f64 / DAY as f64).round() as u8;
                sample(t, pct, false, Cell::Alkaline)
            })
            .collect()
    }

    #[test]
    fn steady_drain_estimate() {
        let samples = steady(3, 2.0, 300);
        let newest = *samples.last().unwrap();
        assert_eq!(newest.percent, 94);
        let est = estimate(&samples, newest.time).expect("should estimate");
        assert!((est.percent_per_day - 2.0).abs() < 0.05, "{est:?}");
        let days = est.remaining_secs as f64 / DAY as f64;
        assert!((days - 47.0).abs() < 1.0, "{days} days");
    }

    #[test]
    fn none_when_no_samples() {
        assert_eq!(estimate(&[], 1_000), None);
    }

    #[test]
    fn none_when_charging() {
        let mut samples = steady(3, 2.0, 300);
        let last = samples.len() - 1;
        samples[last].charging = true;
        assert_eq!(estimate(&samples, samples[last].time), None);
    }

    #[test]
    fn none_for_liion() {
        let samples: Vec<Sample> = steady(3, 2.0, 300)
            .into_iter()
            .map(|s| Sample { cell: Cell::Liion, ..s })
            .collect();
        assert_eq!(estimate(&samples, samples.last().unwrap().time), None);
    }

    #[test]
    fn none_when_span_under_twelve_hours() {
        // 6 hours, 3 points of drop: the span is too short.
        let samples: Vec<Sample> = (0..=72)
            .map(|k| {
                let t = k * 300;
                let pct = 100 - (3 * k / 72) as u8;
                sample(t, pct, false, Cell::Alkaline)
            })
            .collect();
        assert_eq!(estimate(&samples, samples.last().unwrap().time), None);
    }

    #[test]
    fn none_when_drop_under_three() {
        // 3 days, 2 points of drop.
        let samples: Vec<Sample> = (0..=864)
            .map(|k| {
                let t = k * 300;
                let pct = 100 - (2 * k / 864) as u8;
                sample(t, pct, false, Cell::Alkaline)
            })
            .collect();
        assert_eq!(estimate(&samples, samples.last().unwrap().time), None);
    }

    #[test]
    fn none_when_flat() {
        let samples: Vec<Sample> = (0..=864).map(|k| sample(k * 300, 50, false, Cell::Alkaline)).collect();
        assert_eq!(estimate(&samples, samples.last().unwrap().time), None);
    }

    #[test]
    fn none_when_trend_rising() {
        // Drops 18 points, then climbs back up through small rises. The
        // drop check passes but the fitted trend is positive.
        let points: [(f64, u8); 7] = [
            (0.0, 60),
            (0.5, 42),
            (1.0, 42),
            (1.5, 48),
            (2.0, 48),
            (2.5, 52),
            (3.0, 55),
        ];
        let samples: Vec<Sample> = points
            .iter()
            .map(|&(d, p)| sample((d * DAY as f64) as u64, p, false, Cell::Alkaline))
            .collect();
        assert_eq!(estimate(&samples, samples.last().unwrap().time), None);
    }

    #[test]
    fn battery_swap_uses_only_samples_after_jump() {
        // Old pack drains fast (about 10 points a day), then a fresh pack
        // goes in and the reading jumps back to 100%.
        let mut samples: Vec<Sample> = (0..=288)
            .map(|k| {
                let t = k * 300;
                let pct = (80.0 - 10.0 * t as f64 / DAY as f64).round() as u8;
                sample(t, pct, false, Cell::Alkaline)
            })
            .collect();
        let jump = samples.last().unwrap().time + 300;
        let post: Vec<Sample> = steady(3, 2.0, 300)
            .into_iter()
            .map(|s| sample(s.time + jump, s.percent, false, Cell::Alkaline))
            .collect();
        samples.extend(post.iter().copied());

        let now = samples.last().unwrap().time;
        let full = estimate(&samples, now).expect("should estimate");
        let only_new = estimate(&post, now).expect("should estimate");
        assert_eq!(full, only_new);
        assert!((full.percent_per_day - 2.0).abs() < 0.05, "{full:?}");
    }

    #[test]
    fn small_noise_rises_do_not_reset_run() {
        // Steady 2 points a day, with +1 bumps on every tenth reading.
        let samples: Vec<Sample> = steady(3, 2.0, 300)
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let bump = if i % 10 == 5 { 1 } else { 0 };
                sample(s.time, s.percent + bump, false, Cell::Alkaline)
            })
            .collect();
        let newest = *samples.last().unwrap();
        // The newest reading is not bumped (index 864 % 10 == 4).
        let est = estimate(&samples, newest.time).expect("run must survive noise");
        assert!((est.percent_per_day - 2.0).abs() < 0.3, "{est:?}");
        let days = est.remaining_secs as f64 / DAY as f64;
        assert!((days - 47.0).abs() < 2.0, "{days} days");
    }

    #[test]
    fn cell_change_resets_run() {
        // Alkaline for the first day, then NiMH for three days.
        let mut samples: Vec<Sample> = steady(1, 2.0, 300);
        let start = samples.last().unwrap().time + 300;
        let nimh: Vec<Sample> = steady(3, 2.0, 300)
            .into_iter()
            .map(|s| sample(s.time + start, s.percent.saturating_sub(2), false, Cell::NiMH))
            .collect();
        samples.extend(nimh.iter().copied());

        let now = samples.last().unwrap().time;
        let full = estimate(&samples, now).expect("should estimate");
        let only_nimh = estimate(&nimh, now).expect("should estimate");
        assert_eq!(full, only_nimh);
    }

    #[test]
    fn elapsed_time_reduces_remaining() {
        let samples = steady(3, 2.0, 300);
        let newest = *samples.last().unwrap();
        let now_zero = estimate(&samples, newest.time).unwrap();
        let now_later = estimate(&samples, newest.time + DAY).unwrap();
        let gap = now_zero.remaining_secs - now_later.remaining_secs;
        assert!(gap.abs_diff(DAY) <= 1, "gap {gap}");
        assert_eq!(now_zero.percent_per_day, now_later.percent_per_day);
    }

    #[test]
    fn remaining_floors_at_zero() {
        let samples = steady(3, 2.0, 300);
        let newest = *samples.last().unwrap();
        let est = estimate(&samples, newest.time + 100 * DAY).unwrap();
        assert_eq!(est.remaining_secs, 0);
    }

    #[test]
    fn describe_units() {
        assert_eq!(describe(0), "less than an hour");
        assert_eq!(describe(30 * 60), "less than an hour");
        assert_eq!(describe(60 * 60), "about 1 hour");
        assert_eq!(describe(5 * 60 * 60), "about 5 hours");
        assert_eq!(describe(47 * 60 * 60), "about 47 hours");
        assert_eq!(describe(2 * DAY), "about 2 days");
        assert_eq!(describe((13.9 * DAY as f64) as u64), "about 2 weeks");
        assert_eq!(describe(21 * DAY), "about 3 weeks");
        assert_eq!(describe(59 * DAY), "about 8 weeks");
        assert_eq!(describe(90 * DAY), "about 3 months");
    }
}
