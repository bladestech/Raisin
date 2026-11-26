//! Events the mouse sends by itself, such as a DPI button press.
//!
//! Razer receivers report these on their vendor collections as small input
//! reports. A DPI change is `05 02 XX XX YY YY` (big-endian X and Y DPI),
//! checked on a Viper V3 HyperSpeed. Each readable collection gets a
//! thread that blocks in ReadFile, so nothing polls: the threads sleep
//! until the mouse sends something, and end when it's unplugged.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::hid::{self, Device};
use crate::razer::VID_RAZER;

/// Listener threads still running.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// A DPI change from the mouse's report, if `report` is one.
pub fn parse_dpi(report: &[u8]) -> Option<(u16, u16)> {
    match report {
        [0x05, 0x02, x0, x1, y0, y1, ..] => {
            let x = u16::from_be_bytes([*x0, *x1]);
            let y = u16::from_be_bytes([*y0, *y1]);
            (x > 0).then_some((x, y))
        }
        _ => None,
    }
}

pub fn listening() -> bool {
    RUNNING.load(Ordering::Relaxed) > 0
}

/// Collections that can carry events: not the mouse or keyboard ones
/// (Windows owns those), generic-desktop "undefined" or vendor pages.
fn is_event_collection(d: &hid::DeviceInfo) -> bool {
    d.input_len > 0 && ((d.usage_page == 0x0001 && d.usage == 0x0000) || d.usage_page >= 0xFF00)
}

/// Start listening to mouse `pid`; `on_dpi` runs on the listener thread
/// for every DPI change. Returns how many collections are being read.
pub fn start(pid: u16, on_dpi: fn(u16, u16)) -> usize {
    let mut started = 0;
    for info in hid::enumerate(VID_RAZER, Some(pid))
        .into_iter()
        .filter(is_event_collection)
    {
        let Some(dev) = Device::open_input(&info) else { continue };
        let len = (info.input_len as usize).max(16);
        RUNNING.fetch_add(1, Ordering::Relaxed);
        let spawned = std::thread::Builder::new()
            .name("raisin-events".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut buf = vec![0u8; len];
                while let Some(n) = dev.read(&mut buf) {
                    if let Some((x, y)) = parse_dpi(&buf[..n]) {
                        on_dpi(x, y);
                    }
                }
                RUNNING.fetch_sub(1, Ordering::Relaxed);
            });
        match spawned {
            Ok(_) => started += 1,
            Err(_) => {
                RUNNING.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
    started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_reports_parse() {
        // Captured from a Viper V3 HyperSpeed, stage 4 (3200 DPI).
        let r = [0x05, 0x02, 0x0C, 0x80, 0x0C, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_dpi(&r), Some((3200, 3200)));
        let r = [0x05, 0x02, 0x01, 0x90, 0x01, 0x90];
        assert_eq!(parse_dpi(&r), Some((400, 400)));
    }

    #[test]
    fn other_reports_are_ignored() {
        assert_eq!(parse_dpi(&[0x05, 0x03, 0x0C, 0x80, 0x0C, 0x80]), None);
        assert_eq!(parse_dpi(&[0x04, 0x02, 0x0C, 0x80, 0x0C, 0x80]), None);
        assert_eq!(parse_dpi(&[0x05, 0x02, 0, 0, 0, 0]), None);
        assert_eq!(parse_dpi(&[0x05, 0x02]), None);
    }
}
