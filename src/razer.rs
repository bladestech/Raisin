//! Razer HID protocol (90-byte feature reports). See docs/protocol.md.

use std::thread::sleep;
use std::time::Duration;

use crate::devices::{Device, DpiCmd, PollCmd};
use crate::hid;

pub const VID_RAZER: u16 = 0x1532;
pub const DPI_MIN: u16 = 100;

const REPORT_LEN: usize = 90;
const VARSTORE: u8 = 0x01;
const NOSTORE: u8 = 0x00;
/// Older mice scale one DPI byte over 0-6750 DPI.
const BYTE_DPI_SCALE: u32 = 6750;

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
            Error::NotFound => "no supported Razer mouse found",
            Error::Io => "HID transfer failed",
            Error::Busy => "device busy",
            Error::Failure => "command failed",
            Error::Timeout => "no response (mouse asleep or off?)",
            Error::NotSupported => "not supported by this mouse",
            Error::Mismatch => "unexpected response",
        };
        f.write_str(s)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq)]
pub struct DpiStages {
    /// 1-based active stage.
    pub active: u8,
    pub stages: Vec<(u16, u16)>,
}

/// Low-battery blink choices (percent). The firmware ignores values
/// outside 5-30% (tested on the Viper V3 HyperSpeed: 0 and 50% have no effect).
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

fn build(tid: u8, class: u8, id: u8, size: u8, args: &[u8]) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[1] = tid;
    r[5] = size;
    r[6] = class;
    r[7] = id;
    r[8..8 + args.len()].copy_from_slice(args);
    r[88] = r[2..88].iter().fold(0, |a, b| a ^ b);
    r
}

fn hyper_code(hz: u16) -> Option<u8> {
    Some(match hz {
        8000 => 0x01,
        4000 => 0x02,
        2000 => 0x04,
        1000 => 0x08,
        500 => 0x10,
        250 => 0x20,
        125 => 0x40,
        _ => return None,
    })
}

fn standard_code(hz: u16) -> Option<u8> {
    Some(match hz {
        1000 => 0x01,
        500 => 0x02,
        125 => 0x08,
        _ => return None,
    })
}

/// A connected mouse and the table entry describing it.
pub struct Mouse {
    hid: hid::Device,
    pub info: &'static Device,
}

impl Mouse {
    /// Find the first connected mouse from the device table and the HID
    /// collection that answers the Razer protocol.
    pub fn open() -> Result<Mouse> {
        // Collections that expose a 90-byte feature report (+1 for report ID).
        for hi in hid::enumerate(VID_RAZER, None) {
            if hi.feature_len as usize != REPORT_LEN + 1 {
                continue;
            }
            let Some(info) = Device::find(hi.pid) else { continue };
            let Some(dev) = hid::Device::open(&hi) else { continue };
            let m = Mouse { hid: dev, info };
            // Probe with a harmless query. A sleeping mouse may answer
            // Timeout, which still proves this collection speaks the protocol.
            match m.firmware() {
                Ok(_) | Err(Error::Timeout) => return Ok(m),
                Err(_) => continue,
            }
        }
        Err(Error::NotFound)
    }

    /// "Wireless" when the model needs a receiver delay, else "Wired".
    pub fn connection(&self) -> &'static str {
        if self.info.wait_us > 600 {
            "Wireless"
        } else {
            "Wired"
        }
    }

    fn wait(&self) -> Duration {
        Duration::from_micros(self.info.wait_us as u64)
    }

    /// Send one request and return the raw response regardless of status
    /// (for read-only exploration).
    pub fn raw(&self, class: u8, id: u8, size: u8, args: &[u8]) -> Option<[u8; REPORT_LEN]> {
        let req = build(self.info.tid.dpi, class, id, size, args);
        let mut out = [0u8; REPORT_LEN + 1];
        out[1..].copy_from_slice(&req);
        if !self.hid.set_feature(&out) {
            return None;
        }
        sleep(self.wait());
        let mut inp = [0u8; REPORT_LEN + 1];
        if !self.hid.get_feature(&mut inp) {
            return None;
        }
        let mut resp = [0u8; REPORT_LEN];
        resp.copy_from_slice(&inp[1..]);
        Some(resp)
    }

    fn transact(&self, tid: u8, class: u8, id: u8, size: u8, args: &[u8]) -> Result<[u8; REPORT_LEN]> {
        let req = build(tid, class, id, size, args);
        let mut last = Error::Io;
        for _ in 0..5 {
            let mut out = [0u8; REPORT_LEN + 1];
            out[1..].copy_from_slice(&req);
            if !self.hid.set_feature(&out) {
                last = Error::Io;
                sleep(Duration::from_millis(10));
                continue;
            }
            sleep(self.wait());
            let mut inp = [0u8; REPORT_LEN + 1];
            if !self.hid.get_feature(&mut inp) {
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
                0x01 => last = Error::Busy,
                0x03 => return Err(Error::Failure),
                0x04 => return Err(Error::Timeout),
                0x05 => return Err(Error::NotSupported),
                _ => last = Error::Mismatch,
            }
            sleep(Duration::from_millis(10));
        }
        Err(last)
    }

    fn require(&self, supported: bool) -> Result<()> {
        if supported {
            Ok(())
        } else {
            Err(Error::NotSupported)
        }
    }

    // ---- device info ----

    /// Firmware version (major, minor).
    pub fn firmware(&self) -> Result<(u8, u8)> {
        let r = self.transact(self.info.tid.dpi, 0x00, 0x81, 0x02, &[])?;
        Ok((r[8], r[9]))
    }

    pub fn serial(&self) -> Result<String> {
        let r = self.transact(self.info.tid.dpi, 0x00, 0x82, 0x16, &[])?;
        let s: String = r[8..8 + 0x16]
            .iter()
            .take_while(|&&b| b != 0)
            .filter(|b| b.is_ascii_graphic())
            .map(|&b| b as char)
            .collect();
        Ok(s)
    }

    // ---- DPI ----

    pub fn dpi(&self) -> Result<(u16, u16)> {
        let t = self.info.tid.dpi;
        match self.info.dpi {
            DpiCmd::Xy => {
                let r = self.transact(t, 0x04, 0x85, 0x07, &[NOSTORE])?;
                Ok((u16::from_be_bytes([r[9], r[10]]), u16::from_be_bytes([r[11], r[12]])))
            }
            DpiCmd::Byte => {
                let r = self.transact(t, 0x04, 0x81, 0x03, &[])?;
                let un = |b: u8| ((b as u32 * BYTE_DPI_SCALE + 127) / 255) as u16;
                Ok((un(r[8]), un(r[9])))
            }
            DpiCmd::None => Err(Error::NotSupported),
        }
    }

    pub fn clamp_dpi(&self, v: u16) -> u16 {
        v.clamp(DPI_MIN, self.info.dpi_max.max(DPI_MIN))
    }

    pub fn set_dpi(&self, x: u16, y: u16) -> Result<()> {
        let t = self.info.tid.dpi;
        let (x, y) = (self.clamp_dpi(x), self.clamp_dpi(y));
        match self.info.dpi {
            DpiCmd::Xy => {
                let (x, y) = (x.to_be_bytes(), y.to_be_bytes());
                self.transact(t, 0x04, 0x05, 0x07, &[NOSTORE, x[0], x[1], y[0], y[1], 0, 0])?;
            }
            DpiCmd::Byte => {
                let sc =
                    |v: u16| ((v.min(BYTE_DPI_SCALE as u16) as u32 * 255 + BYTE_DPI_SCALE / 2) / BYTE_DPI_SCALE) as u8;
                self.transact(t, 0x04, 0x01, 0x03, &[sc(x), sc(y), 0])?;
            }
            DpiCmd::None => return Err(Error::NotSupported),
        }
        Ok(())
    }

    pub fn dpi_stages(&self) -> Result<DpiStages> {
        self.require(self.info.stages)?;
        let r = self.transact(self.info.tid.stages, 0x04, 0x86, 0x26, &[VARSTORE])?;
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
        self.require(self.info.stages)?;
        let count = s.stages.len().clamp(1, 5);
        let mut args = vec![0u8; 3 + count * 7];
        args[0] = VARSTORE;
        args[1] = s.active.clamp(1, count as u8);
        args[2] = count as u8;
        for (i, &(x, y)) in s.stages.iter().take(count).enumerate() {
            let x = self.clamp_dpi(x).to_be_bytes();
            let y = self.clamp_dpi(y).to_be_bytes();
            let o = 3 + i * 7;
            args[o] = (i + 1) as u8;
            args[o + 1..o + 5].copy_from_slice(&[x[0], x[1], y[0], y[1]]);
        }
        self.transact(self.info.tid.stages, 0x04, 0x06, 0x26, &args)?;
        Ok(())
    }

    // ---- polling rate ----

    pub fn poll_rate(&self) -> Result<u16> {
        let t = self.info.tid.poll;
        match self.info.poll {
            PollCmd::Standard => {
                let r = self.transact(t, 0x00, 0x85, 0x01, &[])?;
                Ok([1000, 500, 125]
                    .into_iter()
                    .find(|&hz| standard_code(hz) == Some(r[8]))
                    .unwrap_or(0))
            }
            PollCmd::Hyper => {
                let r = self.transact(t, 0x00, 0xC0, 0x01, &[])?;
                let rates = [8000, 4000, 2000, 1000, 500, 250, 125];
                Ok(rates.into_iter().find(|&hz| hyper_code(hz) == Some(r[9])).unwrap_or(0))
            }
            PollCmd::None => Err(Error::NotSupported),
        }
    }

    pub fn set_poll_rate(&self, hz: u16) -> Result<()> {
        if !self.info.poll_rates.contains(&hz) {
            return Err(Error::NotSupported);
        }
        let t = self.info.tid.poll;
        match self.info.poll {
            PollCmd::Standard => {
                let code = standard_code(hz).ok_or(Error::NotSupported)?;
                self.transact(t, 0x00, 0x05, 0x01, &[code])?;
            }
            PollCmd::Hyper => {
                // Razer sends each HyperPolling request with 0x00, then 0x01.
                let code = hyper_code(hz).ok_or(Error::NotSupported)?;
                self.transact(t, 0x00, 0x40, 0x02, &[0x00, code])?;
                let t2 = self.info.poll_tid2.unwrap_or(t);
                self.transact(t2, 0x00, 0x40, 0x02, &[0x01, code])?;
            }
            PollCmd::None => return Err(Error::NotSupported),
        }
        Ok(())
    }

    // ---- battery and power ----

    /// Battery percentage (0-100).
    pub fn battery(&self) -> Result<u8> {
        self.battery_raw().map(|b| b.0)
    }

    /// (percent, raw 0-255)
    pub fn battery_raw(&self) -> Result<(u8, u8)> {
        self.require(self.info.battery)?;
        let r = self.transact(self.info.tid.power, 0x07, 0x80, 0x02, &[])?;
        Ok((raw_to_percent(r[9]), r[9]))
    }

    pub fn charging(&self) -> Result<bool> {
        self.require(self.info.charging)?;
        let r = self.transact(self.info.tid.power, 0x07, 0x84, 0x02, &[])?;
        Ok(r[9] != 0)
    }

    /// Battery level (raw 0-255) below which the mouse blinks its LED red.
    pub fn low_battery_threshold(&self) -> Result<u8> {
        self.require(self.info.low_battery)?;
        let r = self.transact(self.info.tid.power, 0x07, 0x81, 0x01, &[])?;
        Ok(r[8])
    }

    pub fn set_low_battery_threshold(&self, raw: u8) -> Result<()> {
        self.require(self.info.low_battery)?;
        self.transact(self.info.tid.power, 0x07, 0x01, 0x01, &[raw])?;
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
    /// low because the mouse measures under load. Other models may answer
    /// "not supported".
    pub fn voltage_mv(&self) -> Result<u16> {
        self.require(self.info.battery)?;
        let r = self.transact(self.info.tid.power, 0x07, 0x85, 0x03, &[])?;
        let raw = u16::from_be_bytes([r[9], r[10]]) as u32;
        Ok((raw * 1800 / 4095) as u16)
    }

    /// Battery chemistry setting (AA mice). Other models may answer
    /// "not supported".
    pub fn battery_type(&self) -> Result<Option<BatteryType>> {
        self.require(self.info.battery)?;
        let r = self.transact(self.info.tid.power, 0x07, 0x94, 0x01, &[])?;
        Ok(BatteryType::ALL.into_iter().find(|t| *t as u8 == r[8]))
    }

    pub fn set_battery_type(&self, t: BatteryType) -> Result<()> {
        self.require(self.info.battery)?;
        self.transact(self.info.tid.power, 0x07, 0x14, 0x01, &[t as u8])?;
        Ok(())
    }

    /// Seconds of inactivity before the mouse sleeps.
    pub fn idle_time(&self) -> Result<u16> {
        self.require(self.info.idle)?;
        let r = self.transact(self.info.tid.power, 0x07, 0x83, 0x02, &[])?;
        Ok(u16::from_be_bytes([r[8], r[9]]))
    }

    pub fn set_idle_time(&self, secs: u16) -> Result<()> {
        self.require(self.info.idle)?;
        let b = secs.clamp(60, 900).to_be_bytes();
        self.transact(self.info.tid.power, 0x07, 0x03, 0x02, &b)?;
        Ok(())
    }

    // ---- scroll wheel ----

    fn scroll_get(&self, supported: bool, id: u8) -> Result<bool> {
        self.require(supported)?;
        let r = self.transact(self.info.tid.scroll, 0x02, id, 0x02, &[VARSTORE])?;
        Ok(r[9] != 0)
    }

    fn scroll_set(&self, supported: bool, id: u8, on: bool) -> Result<()> {
        self.require(supported)?;
        self.transact(self.info.tid.scroll, 0x02, id, 0x02, &[VARSTORE, on as u8])?;
        Ok(())
    }

    /// true = free spin, false = tactile.
    pub fn free_spin(&self) -> Result<bool> {
        self.scroll_get(self.info.scroll_mode, 0x94)
    }

    pub fn set_free_spin(&self, on: bool) -> Result<()> {
        self.scroll_set(self.info.scroll_mode, 0x14, on)
    }

    pub fn scroll_acceleration(&self) -> Result<bool> {
        self.scroll_get(self.info.scroll_accel, 0x96)
    }

    pub fn set_scroll_acceleration(&self, on: bool) -> Result<()> {
        self.scroll_set(self.info.scroll_accel, 0x16, on)
    }

    pub fn smart_reel(&self) -> Result<bool> {
        self.scroll_get(self.info.smart_reel, 0x97)
    }

    pub fn set_smart_reel(&self, on: bool) -> Result<()> {
        self.scroll_set(self.info.smart_reel, 0x17, on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_layout_and_crc() {
        let r = build(0x1F, 0x04, 0x05, 0x07, &[1, 2, 3]);
        assert_eq!(r[1], 0x1F);
        assert_eq!((r[5], r[6], r[7]), (0x07, 0x04, 0x05));
        assert_eq!(&r[8..11], &[1, 2, 3]);
        // CRC is the XOR of bytes 2..=87.
        assert_eq!(r[88], r[2..88].iter().fold(0, |a, b| a ^ b));
    }

    #[test]
    fn low_battery_threshold_matches_openrazer() {
        // OpenRazer documents 0x0C = 5%, 0x26 = 15%, 0x3F = 25%.
        assert_eq!(percent_to_raw(5), 0x0C);
        assert_eq!(percent_to_raw(15), 0x26);
        assert_eq!(percent_to_raw(25), 0x3F);
    }

    #[test]
    fn low_battery_choices_round_trip() {
        for p in LOW_BATTERY_CHOICES {
            assert_eq!(raw_to_percent(percent_to_raw(p)), p);
        }
    }

    #[test]
    fn battery_type_names_parse() {
        for t in BatteryType::ALL {
            let key = format!("{t:?}").to_ascii_lowercase();
            assert_eq!(BatteryType::parse(&key), Some(t));
        }
        assert_eq!(BatteryType::parse("liion"), None); // a Raisin profile, not a mouse type
    }

    #[test]
    fn polling_codes_match_openrazer() {
        assert_eq!(standard_code(1000), Some(0x01));
        assert_eq!(standard_code(125), Some(0x08));
        assert_eq!(standard_code(250), None);
        assert_eq!(hyper_code(8000), Some(0x01));
        assert_eq!(hyper_code(250), Some(0x20));
        assert_eq!(hyper_code(125), Some(0x40));
    }
}
