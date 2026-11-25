//! Supported mice and how each one speaks the protocol.
//!
//! The table itself (`devices_table.rs`) is generated from OpenRazer by
//! `tools/openrazer_devices.py`.

#[path = "devices_table.rs"]
mod table;

pub use table::DEVICES;

/// How a mouse sets its DPI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DpiCmd {
    None,
    /// `04/05`: 16-bit X/Y values.
    Xy,
    /// `04/01`: one byte per axis, scaled to 0-255 over 0-6750 DPI (older mice).
    Byte,
}

/// How a mouse sets its polling rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PollCmd {
    None,
    /// `00/05`: 125, 500 or 1000 Hz.
    Standard,
    /// `00/40`: HyperPolling, up to 8000 Hz.
    Hyper,
}

/// Transaction IDs differ between models, and on a few models between features.
#[derive(Clone, Copy, Debug)]
pub struct Tids {
    pub dpi: u8,
    pub poll: u8,
    /// Battery, sleep timer and low-battery threshold (class 0x07).
    pub power: u8,
    pub stages: u8,
    /// Scroll wheel mode, acceleration and Smart Reel (class 0x02).
    pub scroll: u8,
}

#[derive(Debug)]
pub struct Device {
    pub pid: u16,
    pub name: &'static str,
    pub dpi: DpiCmd,
    pub dpi_max: u16,
    pub poll: PollCmd,
    pub poll_rates: &'static [u16],
    pub stages: bool,
    pub battery: bool,
    /// Rechargeable: reports whether it's charging.
    pub charging: bool,
    pub idle: bool,
    pub low_battery: bool,
    /// Scroll wheel can switch between tactile and free spin.
    pub scroll_mode: bool,
    pub scroll_accel: bool,
    pub smart_reel: bool,
    pub tid: Tids,
    /// Hyper-polling: transaction ID for the second (`0x01`) request, if it differs.
    pub poll_tid2: Option<u8>,
    /// Delay between sending a request and reading the reply.
    pub wait_us: u32,
}

/// USB product IDs verified on real hardware. Everything else in the table
/// comes from OpenRazer and is untested with Raisin.
pub const TESTED: &[u16] = &[0x00B8];

impl Device {
    pub fn find(pid: u16) -> Option<&'static Device> {
        DEVICES.iter().find(|d| d.pid == pid)
    }

    pub fn tested(&self) -> bool {
        TESTED.contains(&self.pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_unique_pids() {
        let mut pids: Vec<u16> = DEVICES.iter().map(|d| d.pid).collect();
        let n = pids.len();
        pids.sort();
        pids.dedup();
        assert_eq!(pids.len(), n);
    }

    #[test]
    fn viper_v3_hyperspeed_matches_hardware() {
        // Values confirmed on the real mouse.
        let d = Device::find(0x00B8).expect("Viper V3 HyperSpeed in table");
        assert_eq!(d.dpi, DpiCmd::Xy);
        assert_eq!(d.dpi_max, 30000);
        assert_eq!(d.poll, PollCmd::Standard);
        assert_eq!(d.poll_rates, &[125, 500, 1000]);
        assert!(d.stages && d.battery && d.idle && d.low_battery);
        assert_eq!((d.tid.dpi, d.tid.power), (0x1F, 0x1F));
        assert_eq!(d.wait_us, 59_900);
    }

    #[test]
    fn standard_polling_only_lists_encodable_rates() {
        for d in DEVICES.iter().filter(|d| d.poll == PollCmd::Standard) {
            assert!(d.poll_rates.iter().all(|r| [125, 500, 1000].contains(r)), "{}", d.name);
        }
    }

    #[test]
    fn dpi_devices_have_a_max() {
        for d in DEVICES.iter().filter(|d| d.dpi != DpiCmd::None) {
            assert!(d.dpi_max >= 100, "{}", d.name);
        }
    }
}
