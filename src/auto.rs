//! Automatic switching: per-app rules and following Windows power.
//!
//! While an app with a rule is in front, its power mode and/or DPI stage
//! apply. With "Power Saver on battery or energy saver" on, Power Saver
//! applies while the PC runs on battery or energy saver is on (an app
//! rule's own power mode wins). When nothing applies any more, the
//! settings from before the switch come back.
//!
//! This module is pure logic; the tray app watches the foreground window
//! and power state and writes the result to the mouse.

use crate::power::Mode;

/// Most app rules the settings window offers.
pub const MAX_RULES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRule {
    /// Lowercase file name, e.g. "cs2.exe".
    pub exe: String,
    /// None = leave the power mode alone.
    pub mode: Option<Mode>,
    /// 1-based DPI stage; None = leave it alone.
    pub stage: Option<u8>,
}

impl AppRule {
    pub fn new(exe: &str) -> AppRule {
        AppRule {
            exe: exe_name(exe),
            mode: None,
            stage: None,
        }
    }

    /// From a settings line value: `cs2.exe,max,3` (empty fields allowed).
    pub fn parse(v: &str) -> Option<AppRule> {
        let mut parts = v.split(',').map(str::trim);
        let exe = exe_name(parts.next()?);
        if exe.is_empty() {
            return None;
        }
        let mode = parts.next().and_then(Mode::parse);
        let stage = parts
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|s| (1..=5).contains(s));
        Some(AppRule { exe, mode, stage })
    }

    pub fn to_value(&self) -> String {
        format!(
            "{},{},{}",
            self.exe,
            self.mode.map_or("", Mode::key),
            self.stage.map_or(String::new(), |s| s.to_string())
        )
    }
}

/// File name of a path, lowercased: `C:\Games\CS2.exe` -> `cs2.exe`.
pub fn exe_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or("").trim().to_lowercase()
}

/// What triggered an automatic switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    App(String),
    Power,
}

/// Settings to apply automatically right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub trigger: Trigger,
    pub mode: Option<Mode>,
    pub stage: Option<u8>,
}

impl Target {
    /// Short description for the tray tooltip, e.g. "cs2.exe" or "on battery".
    pub fn label(&self) -> String {
        match &self.trigger {
            Trigger::App(exe) => exe.clone(),
            Trigger::Power => "power saving".into(),
        }
    }
}

/// What should apply, given the app in front and whether Windows is saving
/// power. An app rule wins; its unset power mode falls back to the power
/// rule.
pub fn target(rules: &[AppRule], foreground: Option<&str>, follow_power: bool, saving: bool) -> Option<Target> {
    let power_mode = (follow_power && saving).then_some(Mode::Saver);
    let rule = foreground.and_then(|fg| rules.iter().find(|r| r.exe == fg));
    match rule {
        Some(r) if r.mode.is_some() || r.stage.is_some() || power_mode.is_some() => Some(Target {
            trigger: Trigger::App(r.exe.clone()),
            mode: r.mode.or(power_mode),
            stage: r.stage,
        }),
        _ => power_mode.map(|m| Target {
            trigger: Trigger::Power,
            mode: Some(m),
            stage: None,
        }),
    }
}

/// The settings automatic switching touches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub poll: Option<u16>,
    pub idle: Option<u16>,
    /// 1-based active DPI stage.
    pub stage: Option<u8>,
}

/// `base` with the target's settings applied, for a mouse with `rates`
/// and `stages` DPI stages.
pub fn desired(base: Snapshot, t: &Target, rates: &[u16], stages: usize) -> Snapshot {
    Snapshot {
        poll: t
            .mode
            .and_then(|m| m.poll(rates))
            .or(base.poll)
            .filter(|_| base.poll.is_some()),
        idle: base.idle.map(|i| t.mode.map_or(i, Mode::idle)),
        stage: t
            .stage
            .filter(|&s| base.stage.is_some() && (s as usize) <= stages)
            .or(base.stage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATES: &[u16] = &[125, 500, 1000];

    fn rule(exe: &str, mode: Option<Mode>, stage: Option<u8>) -> AppRule {
        AppRule {
            exe: exe.into(),
            mode,
            stage,
        }
    }

    #[test]
    fn rules_round_trip_through_settings_lines() {
        let r = rule("cs2.exe", Some(Mode::Max), Some(3));
        assert_eq!(r.to_value(), "cs2.exe,max,3");
        assert_eq!(AppRule::parse(&r.to_value()), Some(r));
        assert_eq!(AppRule::parse("Game.EXE,,"), Some(rule("game.exe", None, None)));
        assert_eq!(AppRule::parse("a.exe,turbo,9"), Some(rule("a.exe", None, None)));
        assert_eq!(AppRule::parse(",max,1"), None);
    }

    #[test]
    fn exe_names_are_file_names() {
        assert_eq!(exe_name(r"C:\Games\Counter-Strike\CS2.exe"), "cs2.exe");
        assert_eq!(exe_name("notepad.exe"), "notepad.exe");
    }

    #[test]
    fn app_rules_apply_while_their_app_is_in_front() {
        let rules = [rule("cs2.exe", Some(Mode::Max), Some(2))];
        let t = target(&rules, Some("cs2.exe"), false, false).unwrap();
        assert_eq!(t.trigger, Trigger::App("cs2.exe".into()));
        assert_eq!((t.mode, t.stage), (Some(Mode::Max), Some(2)));
        assert_eq!(target(&rules, Some("explorer.exe"), false, false), None);
        assert_eq!(target(&rules, None, false, false), None);
    }

    #[test]
    fn power_saving_applies_power_saver() {
        let t = target(&[], Some("explorer.exe"), true, true).unwrap();
        assert_eq!((t.trigger, t.mode), (Trigger::Power, Some(Mode::Saver)));
        // Only when turned on, and only while Windows is saving power.
        assert_eq!(target(&[], None, false, true), None);
        assert_eq!(target(&[], None, true, false), None);
    }

    #[test]
    fn app_rules_beat_power_saving() {
        let rules = [
            rule("cs2.exe", Some(Mode::Max), None),
            rule("photoshop.exe", None, Some(1)),
        ];
        let t = target(&rules, Some("cs2.exe"), true, true).unwrap();
        assert_eq!(t.mode, Some(Mode::Max));
        // A rule without a power mode keeps Power Saver on battery.
        let t = target(&rules, Some("photoshop.exe"), true, true).unwrap();
        assert_eq!((t.mode, t.stage), (Some(Mode::Saver), Some(1)));
    }

    #[test]
    fn empty_rules_do_nothing() {
        let rules = [rule("game.exe", None, None)];
        assert_eq!(target(&rules, Some("game.exe"), false, false), None);
    }

    #[test]
    fn desired_overrides_only_what_the_target_sets() {
        let base = Snapshot {
            poll: Some(1000),
            idle: Some(300),
            stage: Some(3),
        };
        let saver = Target {
            trigger: Trigger::Power,
            mode: Some(Mode::Saver),
            stage: None,
        };
        let d = desired(base, &saver, RATES, 5);
        assert_eq!(
            d,
            Snapshot {
                poll: Some(125),
                idle: Some(60),
                stage: Some(3)
            }
        );
        let stage_only = Target {
            trigger: Trigger::App("x.exe".into()),
            mode: None,
            stage: Some(1),
        };
        assert_eq!(
            desired(base, &stage_only, RATES, 5),
            Snapshot { stage: Some(1), ..base }
        );
        // A stage the mouse doesn't have is ignored.
        let stage_9 = Target {
            stage: Some(5),
            ..stage_only
        };
        assert_eq!(desired(base, &stage_9, RATES, 3), base);
    }

    #[test]
    fn desired_skips_settings_the_mouse_lacks() {
        let base = Snapshot::default();
        let max = Target {
            trigger: Trigger::Power,
            mode: Some(Mode::Max),
            stage: Some(2),
        };
        assert_eq!(desired(base, &max, RATES, 5), Snapshot::default());
    }
}
