//! Power modes: one-click presets for polling rate and sleep timer.
//!
//! - Power Saver: the lowest polling rate and the shortest sleep timer.
//! - Balanced: 1000 Hz (or the fastest rate below it), default sleep timer.
//! - Max: the fastest rate the mouse supports, default sleep timer.
//!
//! The mode isn't stored on the mouse; it's worked out from the current
//! settings, so changing the polling rate or sleep timer by hand shows no
//! mode selected.

/// Sleep timer for Power Saver, in seconds (the shortest choice offered).
pub const SAVER_IDLE: u16 = 60;
/// Sleep timer for Balanced and Max: Razer's default, 5 minutes.
pub const DEFAULT_IDLE: u16 = 300;
/// Balanced polling rate.
pub const BALANCED_HZ: u16 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Saver,
    Balanced,
    Max,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Saver, Mode::Balanced, Mode::Max];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Saver => "Power Saver",
            Mode::Balanced => "Balanced",
            Mode::Max => "Max",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Mode::Saver => "saver",
            Mode::Balanced => "balanced",
            Mode::Max => "max",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s.to_ascii_lowercase().as_str() {
            "saver" | "powersaver" | "power-saver" | "eco" | "low" => Some(Mode::Saver),
            "balanced" | "balance" | "normal" => Some(Mode::Balanced),
            "max" | "maximum" | "performance" | "high" => Some(Mode::Max),
            _ => None,
        }
    }

    /// Polling rate for this mode out of the mouse's `rates`.
    pub fn poll(self, rates: &[u16]) -> Option<u16> {
        match self {
            Mode::Saver => rates.iter().copied().min(),
            Mode::Max => rates.iter().copied().max(),
            Mode::Balanced => rates
                .iter()
                .copied()
                .filter(|&r| r <= BALANCED_HZ)
                .max()
                .or_else(|| rates.iter().copied().min()),
        }
    }

    pub fn idle(self) -> u16 {
        match self {
            Mode::Saver => SAVER_IDLE,
            Mode::Balanced | Mode::Max => DEFAULT_IDLE,
        }
    }

    /// Whether the mouse is set up the way this mode would set it.
    /// `idle` is None on mice without a sleep timer.
    pub fn matches(self, rates: &[u16], poll: u16, idle: Option<u16>) -> bool {
        self.poll(rates) == Some(poll) && idle.is_none_or(|i| i == self.idle())
    }
}

/// The mode the mouse is currently in, if any. On mice where two modes
/// come out the same (e.g. Balanced and Max on a 1000 Hz mouse),
/// `preferred` (the one picked last) wins.
pub fn current(rates: &[u16], poll: u16, idle: Option<u16>, preferred: Option<Mode>) -> Option<Mode> {
    if let Some(p) = preferred.filter(|p| p.matches(rates, poll, idle)) {
        return Some(p);
    }
    Mode::ALL.into_iter().find(|m| m.matches(rates, poll, idle))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HYPER: &[u16] = &[125, 500, 1000, 2000, 4000, 8000];
    const STANDARD: &[u16] = &[125, 500, 1000];
    const SLOW: &[u16] = &[125, 500];

    #[test]
    fn modes_pick_rates_from_the_mouse() {
        assert_eq!(Mode::Saver.poll(HYPER), Some(125));
        assert_eq!(Mode::Balanced.poll(HYPER), Some(1000));
        assert_eq!(Mode::Max.poll(HYPER), Some(8000));
        // Balanced never goes above 1000 Hz, and Max can't exceed the mouse.
        assert_eq!(Mode::Balanced.poll(STANDARD), Some(1000));
        assert_eq!(Mode::Max.poll(STANDARD), Some(1000));
        assert_eq!(Mode::Balanced.poll(SLOW), Some(500));
        assert_eq!(Mode::Max.poll(&[]), None);
    }

    #[test]
    fn saver_uses_the_shortest_sleep() {
        assert_eq!(Mode::Saver.idle(), SAVER_IDLE);
        assert!(Mode::Saver.idle() < Mode::Balanced.idle());
    }

    #[test]
    fn current_mode_is_worked_out_from_settings() {
        assert_eq!(current(HYPER, 125, Some(60), None), Some(Mode::Saver));
        assert_eq!(current(HYPER, 1000, Some(300), None), Some(Mode::Balanced));
        assert_eq!(current(HYPER, 8000, Some(300), None), Some(Mode::Max));
        // Hand-picked settings match no mode.
        assert_eq!(current(HYPER, 1000, Some(900), None), None);
        assert_eq!(current(HYPER, 4000, Some(300), None), None);
        // No sleep timer: the rate decides.
        assert_eq!(current(HYPER, 8000, None, None), Some(Mode::Max));
    }

    #[test]
    fn ties_go_to_the_last_picked_mode() {
        assert_eq!(current(STANDARD, 1000, Some(300), None), Some(Mode::Balanced));
        assert_eq!(current(STANDARD, 1000, Some(300), Some(Mode::Max)), Some(Mode::Max));
        // A preference that doesn't match is ignored.
        assert_eq!(current(HYPER, 1000, Some(300), Some(Mode::Max)), Some(Mode::Balanced));
    }

    #[test]
    fn parse_accepts_common_names() {
        assert_eq!(Mode::parse("saver"), Some(Mode::Saver));
        assert_eq!(Mode::parse("Balanced"), Some(Mode::Balanced));
        assert_eq!(Mode::parse("max"), Some(Mode::Max));
        assert_eq!(Mode::parse("turbo"), None);
        for m in Mode::ALL {
            assert_eq!(Mode::parse(m.key()), Some(m));
        }
    }
}
