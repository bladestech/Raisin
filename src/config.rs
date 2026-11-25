//! Tiny key=value settings file for things the mouse can't store itself.
//! Lives at %APPDATA%\Raisin\settings.txt.

use std::path::PathBuf;

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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Config {
    /// Li-ion 1.5 V profile: alkaline curve + low threshold.
    pub liion: bool,
    /// Cells drop their voltage on purpose when nearly empty.
    pub liion_step: bool,
    pub tray_icon: TrayIcon,
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
        let text = format!(
            "liion={}\nliion_step={}\ntray_icon={}\n",
            self.liion as u8,
            self.liion_step as u8,
            self.tray_icon.key()
        );
        let _ = std::fs::write(p, text);
    }

    pub fn liion_threshold(&self) -> u8 {
        if self.liion_step { LIION_STEP_THRESHOLD } else { LIION_THRESHOLD }
    }
}
