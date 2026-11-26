//! Tiny key=value settings file for things the mouse can't store itself.
//! Lives at %APPDATA%\Raisin\settings.txt.

use std::path::PathBuf;

use crate::auto::{AppRule, MAX_RULES};
use crate::power::Mode;

/// What the tray icon shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TrayIcon {
    Battery,
    Logo,
    /// Switch between the logo and the battery gauge.
    #[default]
    Alternate,
}

impl TrayIcon {
    pub const ALL: [TrayIcon; 3] = [TrayIcon::Battery, TrayIcon::Logo, TrayIcon::Alternate];

    pub fn label(self) -> &'static str {
        match self {
            TrayIcon::Battery => "Battery",
            TrayIcon::Logo => "Logo",
            TrayIcon::Alternate => "Alternate",
        }
    }

    fn key(self) -> &'static str {
        match self {
            TrayIcon::Battery => "battery",
            TrayIcon::Logo => "logo",
            TrayIcon::Alternate => "alternate",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Li-ion 1.5 V profile: alkaline curve + low threshold.
    pub liion: bool,
    /// Cells drop their voltage on purpose when nearly empty.
    pub liion_step: bool,
    pub tray_icon: TrayIcon,
    /// Power mode picked last; breaks ties where two modes set the same.
    pub power: Option<Mode>,
    /// Power Saver while on battery or energy saver.
    pub auto_power: bool,
    /// Per-app power mode / DPI stage.
    pub rules: Vec<AppRule>,
}

/// Regulated Li-ion AAs sit near 1.5 V until they're nearly empty, then
/// drop (smart cells drop on purpose). Below this, show "Low".
pub const LIION_LOW_MV: u16 = 1400;

/// Li-ion level as High/Low: from voltage when the mouse reports it,
/// otherwise from the (unreliable) percentage.
pub fn liion_high(voltage_mv: Option<u16>, percent: Option<u8>) -> bool {
    match (voltage_mv, percent) {
        (Some(mv), _) => mv >= LIION_LOW_MV,
        (None, Some(p)) => p >= 50,
        (None, None) => true,
    }
}

/// Mouse threshold for the Li-ion profile: barely ever fires.
pub const LIION_THRESHOLD: u8 = 5;
/// With a low-voltage step: the highest threshold the mouse honors, so the
/// step is most likely to land under it.
pub const LIION_STEP_THRESHOLD: u8 = 30;

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Raisin").join("settings.txt"))
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return c;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let on = v == "1";
            match k.trim() {
                "liion" => c.liion = on,
                "liion_step" => c.liion_step = on,
                "tray_icon" => {
                    if let Some(t) = TrayIcon::ALL.into_iter().find(|t| t.key() == v) {
                        c.tray_icon = t;
                    }
                }
                "power" => c.power = Mode::parse(v),
                "auto_power" => c.auto_power = on,
                "app" => {
                    if let Some(r) = AppRule::parse(v) {
                        if c.rules.len() < MAX_RULES && !c.rules.iter().any(|x| x.exe == r.exe) {
                            c.rules.push(r);
                        }
                    }
                }
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut text = format!(
            "liion={}\nliion_step={}\ntray_icon={}\npower={}\nauto_power={}\n",
            self.liion as u8,
            self.liion_step as u8,
            self.tray_icon.key(),
            self.power.map_or("", Mode::key),
            self.auto_power as u8,
        );
        for r in &self.rules {
            text += &format!("app={}\n", r.to_value());
        }
        let _ = std::fs::write(p, text);
    }

    pub fn liion_threshold(&self) -> u8 {
        if self.liion_step {
            LIION_STEP_THRESHOLD
        } else {
            LIION_THRESHOLD
        }
    }
}
