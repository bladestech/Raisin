//! Battery history: one line per reading in %APPDATA%\Raisin\battery.csv,
//! written by the tray process. Read back for the graph and the estimate
//! (see estimate.rs).
//!
//! Line format (no header): `time,percent,voltage_mv,charging,cell`
//!   time       Unix seconds (UTC)
//!   percent    0-100, as the mouse reports it
//!   voltage_mv cell voltage in millivolts, empty when the mouse doesn't report it
//!   charging   0 or 1
//!   cell       Cell::key(): builtin, alkaline, nimh, lithium, liion
//! Example: `1764150000,87,1452,0,alkaline`

use std::path::PathBuf;

/// What powers the mouse, as far as the reading goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    /// Built-in rechargeable battery (no battery type setting).
    Builtin,
    Alkaline,
    NiMH,
    Lithium,
    /// Raisin's Li-ion 1.5 V profile: the percentage means nothing.
    Liion,
}

impl Cell {
    pub fn key(self) -> &'static str {
        match self {
            Cell::Builtin => "builtin",
            Cell::Alkaline => "alkaline",
            Cell::NiMH => "nimh",
            Cell::Lithium => "lithium",
            Cell::Liion => "liion",
        }
    }

    pub fn parse(s: &str) -> Option<Cell> {
        [Cell::Builtin, Cell::Alkaline, Cell::NiMH, Cell::Lithium, Cell::Liion]
            .into_iter()
            .find(|c| c.key() == s)
    }
}

/// One battery reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Unix seconds (UTC).
    pub time: u64,
    pub percent: u8,
    pub voltage_mv: Option<u16>,
    pub charging: bool,
    pub cell: Cell,
}

/// Don't record more often than this, unless something changed (see
/// `should_record`). The tray reads the battery every 5 minutes.
pub const MIN_INTERVAL: u64 = 4 * 60;
/// Keep this much history...
pub const MAX_AGE: u64 = 60 * 24 * 60 * 60;
/// ...and at most this many samples.
pub const MAX_SAMPLES: usize = 20_000;
/// Rewrite the file trimmed once it grows past this.
pub const COMPACT_BYTES: u64 = 700 * 1024;

/// %APPDATA%\Raisin\battery.csv
pub fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Raisin").join("battery.csv"))
}

/// The current time in Unix seconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `sample` as one line, without the newline.
pub fn format_line(sample: &Sample) -> String {
    unimplemented!()
}

/// Parse one line; None if it is malformed in any way (wrong field count,
/// bad number, percent over 100, charging not 0/1, unknown cell).
/// Surrounding whitespace (including '\r') is ignored.
pub fn parse_line(line: &str) -> Option<Sample> {
    unimplemented!()
}

/// Every well-formed line of `text`, in file order. Bad lines are skipped.
pub fn parse(text: &str) -> Vec<Sample> {
    unimplemented!()
}

/// Whether `new` is worth a line: always if there is no `last`; otherwise if
/// at least MIN_INTERVAL has passed since `last`, or the clock went
/// backwards, or `charging` or `cell` changed.
pub fn should_record(last: Option<&Sample>, new: &Sample) -> bool {
    unimplemented!()
}

/// Drop samples older than `now - MAX_AGE`, then the oldest ones beyond
/// MAX_SAMPLES. Keeps order.
pub fn trim(samples: &mut Vec<Sample>, now: u64) {
    unimplemented!()
}

/// All samples in the file (empty if it doesn't exist).
pub fn load() -> Vec<Sample> {
    unimplemented!()
}

/// The newest sample in the file, reading only its last few hundred bytes
/// (the tray calls this at startup and must stay light).
pub fn last() -> Option<Sample> {
    unimplemented!()
}

/// Append `sample` to the file, creating the folder and file if needed.
/// If the file is then larger than COMPACT_BYTES, rewrite it with `trim`
/// applied. Errors are ignored: the history is best effort.
pub fn record(sample: &Sample) {
    unimplemented!()
}
