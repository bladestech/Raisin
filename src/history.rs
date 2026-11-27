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
    let voltage = sample.voltage_mv.map_or(String::new(), |v| v.to_string());
    format!(
        "{},{},{},{},{}",
        sample.time,
        sample.percent,
        voltage,
        u8::from(sample.charging),
        sample.cell.key()
    )
}

/// Parse one line; None if it is malformed in any way (wrong field count,
/// bad number, percent over 100, charging not 0/1, unknown cell).
/// Surrounding whitespace (including '\r') is ignored.
pub fn parse_line(line: &str) -> Option<Sample> {
    let fields: Vec<&str> = line.trim().split(',').collect();
    let [time, percent, voltage, charging, cell] = fields[..] else {
        return None;
    };
    let percent: u8 = percent.parse().ok().filter(|&p| p <= 100)?;
    let voltage_mv = match voltage {
        "" => None,
        v => Some(v.parse().ok()?),
    };
    let charging = match charging {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    Some(Sample {
        time: time.parse().ok()?,
        percent,
        voltage_mv,
        charging,
        cell: Cell::parse(cell)?,
    })
}

/// Every well-formed line of `text`, in file order. Bad lines are skipped.
pub fn parse(text: &str) -> Vec<Sample> {
    text.lines().filter_map(parse_line).collect()
}

/// Whether `new` is worth a line: always if there is no `last`; otherwise if
/// at least MIN_INTERVAL has passed since `last`, or the clock went
/// backwards, or `charging` or `cell` changed.
pub fn should_record(last: Option<&Sample>, new: &Sample) -> bool {
    let Some(last) = last else {
        return true;
    };
    new.time < last.time
        || new.time - last.time >= MIN_INTERVAL
        || new.charging != last.charging
        || new.cell != last.cell
}

/// Drop samples older than `now - MAX_AGE`, then the oldest ones beyond
/// MAX_SAMPLES. Keeps order.
pub fn trim(samples: &mut Vec<Sample>, now: u64) {
    samples.retain(|s| s.time.saturating_add(MAX_AGE) >= now);
    if samples.len() > MAX_SAMPLES {
        let extra = samples.len() - MAX_SAMPLES;
        samples.drain(..extra);
    }
}

/// All samples in the file (empty if it doesn't exist).
pub fn load() -> Vec<Sample> {
    let Some(path) = path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// The newest sample in the file, reading only its last few hundred bytes
/// (the tray calls this at startup and must stay light).
pub fn last() -> Option<Sample> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path()?).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(512))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    // The first line may be cut by the seek; parse_line rejects it if so.
    String::from_utf8_lossy(&bytes).lines().rev().find_map(parse_line)
}

/// Append `sample` to the file, creating the folder and file if needed.
/// If the file is then larger than COMPACT_BYTES, rewrite it with `trim`
/// applied. Errors are ignored: the history is best effort.
pub fn record(sample: &Sample) {
    let _ = try_record(sample);
}

fn try_record(sample: &Sample) -> std::io::Result<()> {
    use std::io::Write;

    let Some(path) = path() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().append(true).create(true).open(&path)?;
    file.write_all(format!("{}\n", format_line(sample)).as_bytes())?;
    drop(file);

    if std::fs::metadata(&path)?.len() > COMPACT_BYTES {
        let mut samples = load();
        trim(&mut samples, now());
        let text: String = samples.iter().map(|s| format!("{}\n", format_line(s))).collect();
        let tmp = path.with_file_name("battery.csv.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(time: u64) -> Sample {
        Sample {
            time,
            percent: 87,
            voltage_mv: None,
            charging: false,
            cell: Cell::Builtin,
        }
    }

    #[test]
    fn round_trip_with_and_without_voltage() {
        let a = Sample {
            time: 1764150000,
            percent: 87,
            voltage_mv: None,
            charging: true,
            cell: Cell::Builtin,
        };
        assert_eq!(format_line(&a), "1764150000,87,,1,builtin");
        assert_eq!(parse_line(&format_line(&a)), Some(a));

        let b = Sample {
            time: 5,
            percent: 0,
            voltage_mv: Some(1452),
            charging: false,
            cell: Cell::Alkaline,
        };
        assert_eq!(format_line(&b), "5,0,1452,0,alkaline");
        assert_eq!(parse_line(&format_line(&b)), Some(b));
    }

    #[test]
    fn every_cell_round_trips() {
        for cell in [Cell::Builtin, Cell::Alkaline, Cell::NiMH, Cell::Lithium, Cell::Liion] {
            let s = Sample { cell, ..sample(42) };
            assert_eq!(parse_line(&format_line(&s)), Some(s));
        }
    }

    #[test]
    fn parse_line_rejects_malformed() {
        for line in [
            "1,87,,0",
            "1,87,,0,builtin,x",
            "1,101,,0,builtin",
            "1,87,,2,builtin",
            "1,87,,0,foo",
            "abc,87,,0,builtin",
            "1,87,x,0,builtin",
            "",
            "   ",
        ] {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
    }

    #[test]
    fn parse_line_accepts_trailing_cr() {
        let expected = Sample {
            time: 1764150000,
            percent: 87,
            voltage_mv: None,
            charging: true,
            cell: Cell::Builtin,
        };
        assert_eq!(parse_line("1764150000,87,,1,builtin\r"), Some(expected));
    }

    #[test]
    fn parse_skips_bad_lines() {
        let text = "1,87,,0,builtin\r\ngarbage\n\n2,50,1400,1,lithium\n3,1,,0,foo\n";
        let v = parse(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].time, 1);
        assert_eq!(v[1].time, 2);
    }

    #[test]
    fn should_record_cases() {
        let last = sample(1000);
        assert!(should_record(None, &sample(1000)));
        assert!(!should_record(Some(&last), &sample(1000)));
        assert!(!should_record(Some(&last), &sample(1000 + MIN_INTERVAL - 1)));
        assert!(should_record(Some(&last), &sample(1000 + MIN_INTERVAL)));
        let charging = Sample {
            charging: true,
            ..sample(1001)
        };
        assert!(should_record(Some(&last), &charging));
        let cell = Sample {
            cell: Cell::NiMH,
            ..sample(1001)
        };
        assert!(should_record(Some(&last), &cell));
        assert!(should_record(Some(&last), &sample(999)));
    }

    #[test]
    fn trim_by_age() {
        let now = 1_000_000_000;
        let edge = sample(now - MAX_AGE);
        let new = sample(now);
        let mut v = vec![sample(now - MAX_AGE - 1), edge, new];
        trim(&mut v, now);
        assert_eq!(v, vec![edge, new]);
    }

    #[test]
    fn trim_by_count() {
        let base = 1_000_000;
        let total = MAX_SAMPLES + 5;
        let mut v: Vec<Sample> = (0..total).map(|i| sample(base + i as u64)).collect();
        trim(&mut v, base + total as u64);
        assert_eq!(v.len(), MAX_SAMPLES);
        assert_eq!(v[0].time, base + 5);
        assert_eq!(v[MAX_SAMPLES - 1].time, base + total as u64 - 1);
    }

    #[test]
    fn record_load_last_in_temp_appdata() {
        let dir = std::env::temp_dir().join(format!("raisin-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let old = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", &dir);

        assert!(load().is_empty());
        assert_eq!(last(), None);

        let samples = [
            sample(1000),
            sample(1300),
            Sample {
                voltage_mv: Some(1400),
                cell: Cell::Lithium,
                ..sample(1600)
            },
        ];
        for s in &samples {
            record(s);
        }
        assert_eq!(load(), samples.to_vec());
        assert_eq!(last(), Some(samples[2]));

        match old {
            Some(v) => std::env::set_var("APPDATA", v),
            None => std::env::remove_var("APPDATA"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
