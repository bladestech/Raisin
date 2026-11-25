//! Razer HID protocol (90-byte feature reports), per OpenRazer.

use std::thread::sleep;
use std::time::Duration;

use crate::hid::{self, Device};

pub const VID_RAZER: u16 = 0x1532;
pub const PID_VIPER_V3_HYPERSPEED: u16 = 0x00B8;
pub const DPI_MAX: u16 = 30000;
pub const DPI_MIN: u16 = 100;
pub const POLL_RATES: [u16; 3] = [125, 500, 1000];

const TRANSACTION_ID: u8 = 0x1F;
const REPORT_LEN: usize = 90;
/// Wireless receiver needs ~60 ms between SET and GET (OpenRazer: 59900 us).
const RECEIVER_WAIT: Duration = Duration::from_micros(59_900);

const VARSTORE: u8 = 0x01;
const NOSTORE: u8 = 0x00;

#[derive(Debug)]
pub enum Error {
    NotFound,
    Io,
    Busy,
    Failure,
    Timeout,
    NotSupported,
    Mismatch,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Error::NotFound => "mouse not found",
            Error::Io => "HID transfer failed",
            Error::Busy => "device busy",
            Error::Failure => "command failed",
            Error::Timeout => "no response (mouse asleep or off?)",
            Error::NotSupported => "command not supported",
            Error::Mismatch => "unexpected response",
        };
        f.write_str(s)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Mouse {
    dev: Device,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DpiStages {
    /// 1-based active stage.
    pub active: u8,
    pub stages: Vec<(u16, u16)>,
}

/// Low-battery blink choices (percent). The firmware ignores values
/// outside 5-30% (tested: 0 and 50% have no effect).
pub const LOW_BATTERY_CHOICES: [u8; 6] = [5, 10, 15, 20, 25, 30];

/// Discharge curve the mouse uses to turn cell voltage into a level.
/// Found by probing (not in OpenRazer): get 07/94, set 07/14; 3+ rejected.
/// 0 and 2 verified with a fresh alkaline (100% vs 39%); 1 is the only
/// remaining Synapse option, NiMH.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BatteryType {
    Alkaline = 0,
    NiMH = 1,
    Lithium = 2,
}

impl BatteryType {
    pub const ALL: [BatteryType; 3] = [BatteryType::Alkaline, BatteryType::NiMH, BatteryType::Lithium];

    pub fn name(self) -> &'static str {
        match self {
            BatteryType::Alkaline => "Alkaline",
            BatteryType::NiMH => "NiMH rechargeable",
            BatteryType::Lithium => "Lithium (non-rechargeable)",
        }
    }

    pub fn parse(s: &str) -> Option<BatteryType> {
        match s.to_ascii_lowercase().as_str() {
            "alkaline" | "alk" => Some(BatteryType::Alkaline),
            "nimh" => Some(BatteryType::NiMH),
            "lithium" | "li" => Some(BatteryType::Lithium),
            _ => None,
        }
    }
}

pub fn percent_to_raw(pct: u8) -> u8 {
    (pct.min(100) as u32 * 255 / 100) as u8
}

pub fn raw_to_percent(raw: u8) -> u8 {
    ((raw as u32 * 100 + 127) / 255) as u8
}

fn build(class: u8, id: u8, size: u8, args: &[u8]) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[1] = TRANSACTION_ID;
    r[5] = size;
    r[6] = class;
    r[7] = id;
    r[8..8 + args.len()].copy_from_slice(args);
    r[88] = r[2..88].iter().fold(0, |a, b| a ^ b);
    r
}

impl Mouse {
    /// Find the receiver/mouse and the HID collection that answers the
    /// Razer protocol.
    pub fn open() -> Result<Mouse> {
        let infos = hid::enumerate(VID_RAZER, Some(PID_VIPER_V3_HYPERSPEED));
        // Collections that expose a 90-byte feature report (+1 for report ID).
        for info in infos.iter().filter(|i| i.feature_len as usize == REPORT_LEN + 1) {
            let Some(dev) = Device::open(info) else { continue };
            let m = Mouse { dev };
            // Probe with a harmless query. A sleeping mouse may answer
            // Timeout, which still proves this collection speaks the protocol.
            match m.transact(0x00, 0x85, 0x01, &[]) {
                Ok(_) | Err(Error::Timeout) => return Ok(m),
                Err(_) => continue,
            }
        }
        Err(Error::NotFound)
    }

    /// Send one request and return the raw response regardless of status
    /// (for read-only exploration).
    pub fn raw(&self, class: u8, id: u8, size: u8, args: &[u8]) -> Option<[u8; REPORT_LEN]> {
        let req = build(class, id, size, args);
        let mut out = [0u8; REPORT_LEN + 1];
        out[1..].copy_from_slice(&req);
        if !self.dev.set_feature(&out) {
            return None;
        }
        sleep(RECEIVER_WAIT);
        let mut inp = [0u8; REPORT_LEN + 1];
        if !self.dev.get_feature(&mut inp) {
            return None;
        }
        let mut resp = [0u8; REPORT_LEN];
        resp.copy_from_slice(&inp[1..]);
        Some(resp)
    }

    fn transact(&self, class: u8, id: u8, size: u8, args: &[u8]) -> Result<[u8; REPORT_LEN]> {
        let req = build(class, id, size, args);
        let mut last = Error::Io;
        for _ in 0..5 {
            let mut out = [0u8; REPORT_LEN + 1];
            out[1..].copy_from_slice(&req);
            if !self.dev.set_feature(&out) {
                last = Error::Io;
                sleep(Duration::from_millis(10));
                continue;
            }
            sleep(RECEIVER_WAIT);
            let mut inp = [0u8; REPORT_LEN + 1];
            if !self.dev.get_feature(&mut inp) {
                last = Error::Io;
                sleep(Duration::from_millis(10));
                continue;
            }
            let mut resp = [0u8; REPORT_LEN];
            resp.copy_from_slice(&inp[1..]);
            if resp[6] != class || resp[7] != id || resp[2..4] != req[2..4] {
                last = Error::Mismatch;
                sleep(Duration::from_millis(10));
                continue;
            }
            match resp[0] {
                0x02 => return Ok(resp),
                0x01 => {
                    last = Error::Busy;
                }
                0x03 => return Err(Error::Failure),
                0x04 => return Err(Error::Timeout),
                0x05 => return Err(Error::NotSupported),
                _ => last = Error::Mismatch,
            }
            sleep(Duration::from_millis(10));
        }
        Err(last)
    }

    /// Battery percentage (0-100).
    pub fn battery(&self) -> Result<u8> {
        self.battery_raw().map(|b| b.0)
    }

    /// (percent, raw 0-255)
    pub fn battery_raw(&self) -> Result<(u8, u8)> {
        let r = self.transact(0x07, 0x80, 0x02, &[])?;
        Ok((((r[9] as u32 * 100 + 127) / 255) as u8, r[9]))
    }

    pub fn dpi(&self) -> Result<(u16, u16)> {
        let r = self.transact(0x04, 0x85, 0x07, &[NOSTORE])?;
        Ok((u16::from_be_bytes([r[9], r[10]]), u16::from_be_bytes([r[11], r[12]])))
    }

    pub fn set_dpi(&self, x: u16, y: u16) -> Result<()> {
        let x = x.clamp(DPI_MIN, DPI_MAX).to_be_bytes();
        let y = y.clamp(DPI_MIN, DPI_MAX).to_be_bytes();
        self.transact(0x04, 0x05, 0x07, &[NOSTORE, x[0], x[1], y[0], y[1], 0, 0])?;
        Ok(())
    }

    pub fn poll_rate(&self) -> Result<u16> {
        let r = self.transact(0x00, 0x85, 0x01, &[])?;
        Ok(match r[8] {
            0x01 => 1000,
            0x02 => 500,
            0x08 => 125,
            _ => 0,
        })
    }

    pub fn set_poll_rate(&self, hz: u16) -> Result<()> {
        let v = match hz {
            1000 => 0x01,
            500 => 0x02,
            125 => 0x08,
            _ => return Err(Error::NotSupported),
        };
        self.transact(0x00, 0x05, 0x01, &[v])?;
        Ok(())
    }

    pub fn dpi_stages(&self) -> Result<DpiStages> {
        let r = self.transact(0x04, 0x86, 0x26, &[VARSTORE])?;
        let a = &r[8..];
        let active = a[1];
        let count = (a[2] as usize).min(5);
        let mut stages = Vec::with_capacity(count);
        for i in 0..count {
            let s = &a[3 + i * 7..3 + i * 7 + 7];
            stages.push((u16::from_be_bytes([s[1], s[2]]), u16::from_be_bytes([s[3], s[4]])));
        }
        Ok(DpiStages { active, stages })
    }

    /// Writes stages to onboard memory. `active` is 1-based.
    pub fn set_dpi_stages(&self, s: &DpiStages) -> Result<()> {
        let count = s.stages.len().clamp(1, 5);
        let mut args = vec![0u8; 3 + count * 7];
        args[0] = VARSTORE;
        args[1] = s.active.clamp(1, count as u8);
        args[2] = count as u8;
        for (i, &(x, y)) in s.stages.iter().take(count).enumerate() {
            let x = x.clamp(DPI_MIN, DPI_MAX).to_be_bytes();
            let y = y.clamp(DPI_MIN, DPI_MAX).to_be_bytes();
            let o = 3 + i * 7;
            args[o] = (i + 1) as u8;
            args[o + 1..o + 5].copy_from_slice(&[x[0], x[1], y[0], y[1]]);
        }
        self.transact(0x04, 0x06, 0x26, &args)?;
        Ok(())
    }

    /// Battery level (raw 0-255) below which the mouse blinks its LED red.
    pub fn low_battery_threshold(&self) -> Result<u8> {
        let r = self.transact(0x07, 0x81, 0x01, &[])?;
        Ok(r[8])
    }

    pub fn set_low_battery_threshold(&self, raw: u8) -> Result<()> {
        self.transact(0x07, 0x01, 0x01, &[raw])?;
        Ok(())
    }

    /// Uses OpenRazer's floor mapping (5% = 0x0C, 15% = 0x26, 25% = 0x3F).
    pub fn set_low_battery_percent(&self, pct: u8) -> Result<()> {
        self.set_low_battery_threshold(percent_to_raw(pct.clamp(5, 30)))
    }

    /// Approximate cell voltage in millivolts.
    /// Found by probing (not in OpenRazer): 07/85 arg[1..3] is a live
    /// big-endian value that tracks the cell. Treated as a 12-bit ADC with a
    /// 1.8 V reference. Checked against a multimeter: fresh alkaline 1.59 V
    /// (meter ~1.60 V), used alkaline 1.35 V (meter 1.38 V). Reads slightly
    /// low because the mouse measures under load.
    pub fn voltage_mv(&self) -> Result<u16> {
        let r = self.transact(0x07, 0x85, 0x03, &[])?;
        let raw = u16::from_be_bytes([r[9], r[10]]) as u32;
        Ok((raw * 1800 / 4095) as u16)
    }

    pub fn battery_type(&self) -> Result<Option<BatteryType>> {
        let r = self.transact(0x07, 0x94, 0x01, &[])?;
        Ok(BatteryType::ALL.into_iter().find(|t| *t as u8 == r[8]))
    }

    pub fn set_battery_type(&self, t: BatteryType) -> Result<()> {
        self.transact(0x07, 0x14, 0x01, &[t as u8])?;
        Ok(())
    }

    /// Seconds of inactivity before the mouse sleeps.
    pub fn idle_time(&self) -> Result<u16> {
        let r = self.transact(0x07, 0x83, 0x02, &[])?;
        Ok(u16::from_be_bytes([r[8], r[9]]))
    }

    pub fn set_idle_time(&self, secs: u16) -> Result<()> {
        let b = secs.clamp(60, 900).to_be_bytes();
        self.transact(0x07, 0x03, 0x02, &b)?;
        Ok(())
    }
}
