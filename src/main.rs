#![windows_subsystem = "windows"]

mod auto;
mod config;
mod devices;
mod estimate;
mod events;
mod hid;
mod history;
mod power;
mod razer;
mod tray;

use config::Config;
use razer::{BatteryType, DpiStages, Mouse};

const USAGE: &str = "\
Raisin: lightweight Razer mouse configurator

  raisin                   run in the system tray
  raisin info              mouse, firmware, battery, DPI, polling rate and more
  raisin dpi <n>           set current DPI
  raisin stages <a,b,..>   set DPI stages (1-5), keeps active stage
  raisin stage <n>         switch to stage n (1-based)
  raisin poll <hz>         polling rate (raisin info lists what your mouse supports)
  raisin sleep <seconds>   sleep timer, 60-900
  raisin power <saver|balanced|max>
                           power mode: saver = lowest rate + 1 min sleep;
                           balanced = 1000 Hz + 5 min; max = fastest rate + 5 min
  raisin lowbat <5-30>     red low-battery blink threshold, percent
  raisin battery <alkaline|nimh|lithium|liion|liion-step>
                           battery type (AA mice). liion = alkaline curve + 5% blink;
                           liion-step = for cells that drop voltage when low
                           (30% blink + Raisin warns on the sudden drop)
  raisin scroll <tactile|freespin>
  raisin accel <on|off>    scroll acceleration
  raisin smartreel <on|off>
  raisin history           battery history summary and how long it should last
  raisin devices           list supported mice
  raisin probe            list Razer HID interfaces
  raisin scan [ids]        read-only dump of class 0x07 get commands";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        tray::run();
        return;
    }
    if args[0] == "--settings" {
        tray::run_settings();
        return;
    }
    // GUI-subsystem exe: borrow the parent console for CLI output.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
    let code = cli(&args);
    std::process::exit(code);
}

fn usage() -> razer::Result<()> {
    println!("{USAGE}");
    Ok(())
}

fn battery_type_name(m: &Mouse, cfg: &Config) -> razer::Result<String> {
    let t = m.battery_type()?;
    Ok(match t {
        Some(BatteryType::Alkaline) if cfg.liion => {
            let step = if cfg.liion_step { ", drops voltage when low" } else { "" };
            format!("Li-ion 1.5 V (alkaline curve{step})")
        }
        Some(t) => t.name().to_string(),
        None => "unknown".to_string(),
    })
}

fn fmt_lowbat(raw: u8) -> String {
    format!("{}% (raw {raw})", razer::raw_to_percent(raw))
}

fn cli(args: &[String]) -> i32 {
    let cmd = args[0].as_str();
    let arg = args.get(1).map(String::as_str);

    if cmd == "probe" {
        for i in hid::enumerate(razer::VID_RAZER, None) {
            println!(
                "{:04X}:{:04X} page={:04X} usage={:04X} feat={:3} {}",
                i.vid,
                i.pid,
                i.usage_page,
                i.usage,
                i.feature_len,
                i.path_string()
            );
        }
        return 0;
    }
    if cmd == "scan" {
        // Read-only: query every "get" id (high bit set) in class 0x07.
        let m = match Mouse::open() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        if let Some(ids) = arg.filter(|a| *a != "class") {
            // Full dump of specific ids: raisin scan 8D,94
            for id in ids.split(',').filter_map(|s| u8::from_str_radix(s.trim(), 16).ok()) {
                if let Some(r) = m.raw(0x07, id, 0x50, &[]) {
                    let args: Vec<String> = r[8..40].iter().map(|b| format!("{b:02X}")).collect();
                    println!(
                        "07/{id:02X} status={:02X} size={:02X}\n  {}",
                        r[0],
                        r[5],
                        args.join(" ")
                    );
                }
            }
            return 0;
        }
        // raisin scan            -> class 07, ids 80-FF
        // raisin scan class 00   -> class 00, ids 80-FF (only supported ones shown)
        let class = if arg == Some("class") {
            args.get(2).and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(0x07)
        } else {
            0x07
        };
        for id in 0x80u8..=0xFF {
            match m.raw(class, id, 0x10, &[]) {
                Some(r) if r[0] == 0x02 => {
                    let args: Vec<String> = r[8..24].iter().map(|b| format!("{b:02X}")).collect();
                    println!("{class:02X}/{id:02X}  ok size={:02X} {}", r[5], args.join(" "));
                }
                _ => {}
            }
        }
        return 0;
    }
    if cmd == "history" {
        // The battery history the tray records; no mouse needed.
        let samples = history::load();
        let path = history::path().map(|p| p.display().to_string()).unwrap_or_default();
        let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
            println!("no battery history yet ({path})");
            return 0;
        };
        let days = last.time.saturating_sub(first.time) as f64 / 86400.0;
        println!("file:     {path}");
        println!("readings: {} over {days:.1} days", samples.len());
        let volts = last
            .voltage_mv
            .map_or(String::new(), |mv| format!(", {:.2} V", mv as f64 / 1000.0));
        let charging = if last.charging { ", charging" } else { "" };
        println!("latest:   {}%{volts}{charging} ({})", last.percent, last.cell.key());
        match estimate::estimate(&samples, history::now()) {
            Some(e) => println!(
                "lasts:    {} more ({:.1}% a day)",
                estimate::describe(e.remaining_secs),
                e.percent_per_day
            ),
            None if last.cell == history::Cell::Liion => println!("lasts:    not estimated for Li-ion cells"),
            None => println!("lasts:    not enough history yet"),
        }
        return 0;
    }
    if cmd == "devices" {
        // Every mouse in the table; tested ones marked.
        for d in devices::DEVICES {
            let mark = if d.tested() { "tested" } else { "" };
            println!("1532:{:04X}  {:<52} {mark}", d.pid, d.name);
        }
        println!("{} mice", devices::DEVICES.len());
        return 0;
    }
    if !matches!(
        cmd,
        "info"
            | "dpi"
            | "stages"
            | "stage"
            | "poll"
            | "sleep"
            | "power"
            | "lowbat"
            | "battery"
            | "scroll"
            | "accel"
            | "smartreel"
    ) {
        println!("{USAGE}");
        return if matches!(cmd, "help" | "-h" | "--help") { 0 } else { 2 };
    }

    let m = match Mouse::open() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let num = |s: Option<&str>| s.and_then(|s| s.parse::<u16>().ok());
    let on_off = |s: Option<&str>| match s {
        Some("on" | "1" | "true") => Some(true),
        Some("off" | "0" | "false") => Some(false),
        _ => None,
    };

    let r: razer::Result<()> = (|| {
        match cmd {
            "info" => info(&m)?,
            "dpi" => {
                let Some(v) = num(arg) else {
                    return usage();
                };
                m.set_dpi(v, v)?;
                let (x, _) = m.dpi()?;
                println!("dpi: {x}");
            }
            "stages" => {
                let vals: Vec<u16> = arg
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .take(5)
                    .collect();
                if vals.is_empty() {
                    return usage();
                }
                let cur = m.dpi_stages()?;
                let active = (cur.active as usize).clamp(1, vals.len()) as u8;
                let st = DpiStages {
                    active,
                    stages: vals.iter().map(|&v| (v, v)).collect(),
                };
                m.set_dpi_stages(&st)?;
                let (x, y) = st.stages[active as usize - 1];
                m.set_dpi(x, y)?;
                let s = m.dpi_stages()?;
                println!(
                    "stages: {:?} (active {})",
                    s.stages.iter().map(|v| v.0).collect::<Vec<_>>(),
                    s.active
                );
            }
            "stage" => {
                let Some(n) = num(arg) else {
                    return usage();
                };
                let mut st = m.dpi_stages()?;
                if n == 0 || n as usize > st.stages.len() {
                    eprintln!("stage must be 1-{}", st.stages.len());
                    return Ok(());
                }
                st.active = n as u8;
                m.set_dpi_stages(&st)?;
                let (x, y) = st.stages[n as usize - 1];
                m.set_dpi(x, y)?;
                println!("stage {n}: {x} DPI");
            }
            "poll" => {
                let Some(v) = num(arg) else {
                    return usage();
                };
                if !m.info.poll_rates.contains(&v) {
                    eprintln!("{} supports: {:?} Hz", m.info.name, m.info.poll_rates);
                    return Ok(());
                }
                m.set_poll_rate(v)?;
                println!("poll: {} Hz", m.poll_rate()?);
            }
            "lowbat" => {
                let Some(pct) = num(arg).filter(|v| (5..=30).contains(v)) else {
                    eprintln!("threshold must be 5-30 (the mouse ignores other values)");
                    return Ok(());
                };
                m.set_low_battery_percent(pct as u8)?;
                println!("low battery blink: {}", fmt_lowbat(m.low_battery_threshold()?));
            }
            "battery" => {
                let mut cfg = Config::load();
                match arg {
                    Some(a @ ("liion" | "liion-step")) => {
                        cfg.liion = true;
                        cfg.liion_step = a == "liion-step";
                        m.set_battery_type(BatteryType::Alkaline)?;
                        m.set_low_battery_percent(cfg.liion_threshold())?;
                    }
                    Some(a) => {
                        let Some(t) = BatteryType::parse(a) else {
                            return usage();
                        };
                        cfg.liion = false;
                        m.set_battery_type(t)?;
                    }
                    None => return usage(),
                }
                cfg.save();
                println!("battery type: {}", battery_type_name(&m, &cfg)?);
                println!("battery:      {}", battery_line(&m, &cfg)?);
                println!("lowbat:       {}", fmt_lowbat(m.low_battery_threshold()?));
            }
            "power" => {
                let Some(mode) = arg.and_then(power::Mode::parse) else {
                    return usage();
                };
                let rates = m.info.poll_rates;
                if rates.is_empty() {
                    eprintln!("{} has no polling rate setting", m.info.name);
                    return Ok(());
                }
                if let Some(hz) = mode.poll(rates) {
                    m.set_poll_rate(hz)?;
                }
                let idle = opt(m.idle_time())?;
                if idle.is_some() {
                    m.set_idle_time(mode.idle())?;
                }
                let mut cfg = Config::load();
                cfg.power = Some(mode);
                cfg.save();
                println!("power: {}", mode.label());
                println!("poll:  {} Hz", m.poll_rate()?);
                if idle.is_some() {
                    println!("sleep: {} s", m.idle_time()?);
                }
            }
            "sleep" => {
                let Some(v) = num(arg) else {
                    return usage();
                };
                m.set_idle_time(v)?;
                println!("sleep: {} s", m.idle_time()?);
            }
            "scroll" => {
                let free = match arg {
                    Some("freespin" | "free") => true,
                    Some("tactile") => false,
                    _ => return usage(),
                };
                m.set_free_spin(free)?;
                println!("scroll wheel: {}", if m.free_spin()? { "free spin" } else { "tactile" });
            }
            "accel" => {
                let Some(on) = on_off(arg) else {
                    return usage();
                };
                m.set_scroll_acceleration(on)?;
                println!(
                    "scroll acceleration: {}",
                    if m.scroll_acceleration()? { "on" } else { "off" }
                );
            }
            "smartreel" => {
                let Some(on) = on_off(arg) else {
                    return usage();
                };
                m.set_smart_reel(on)?;
                println!("smart reel: {}", if m.smart_reel()? { "on" } else { "off" });
            }
            _ => unreachable!(),
        }
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Battery as shown to people: Li-ion as High/Low (the percentage is only
/// meaningful for the other chemistries).
fn battery_line(m: &Mouse, cfg: &Config) -> razer::Result<String> {
    let (pct, raw) = m.battery_raw()?;
    let mv = opt(m.voltage_mv())?;
    let volts = mv.map_or(String::new(), |mv| format!(" · ≈{}.{:02} V", mv / 1000, mv % 1000 / 10));
    let liion = cfg.liion && matches!(opt(m.battery_type())?, Some(Some(BatteryType::Alkaline)));
    let charging = if opt(m.charging())? == Some(true) {
        " · charging"
    } else {
        ""
    };
    Ok(if liion {
        let level = if config::liion_high(mv, Some(pct)) {
            "High"
        } else {
            "Low"
        };
        format!("{level} (Li-ion; {pct}% not meaningful){volts}{charging}")
    } else {
        format!("{pct}% (raw {raw}/255){volts}{charging}")
    })
}

/// Treat "not supported" as absent; pass other errors through.
fn opt<T>(r: razer::Result<T>) -> razer::Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(razer::Error::NotSupported) => Ok(None),
        Err(e) => Err(e),
    }
}

fn info(m: &Mouse) -> razer::Result<()> {
    let d = m.info;
    let tested = if d.tested() { "tested" } else { "untested with Raisin" };
    println!(
        "mouse:    {} (1532:{:04X}, {}, {tested})",
        d.name,
        d.pid,
        m.connection()
    );
    if let Some((maj, min)) = opt(m.firmware())? {
        println!("firmware: v{maj}.{min:02}");
    }
    if let Some(s) = opt(m.serial())?.filter(|s| !s.is_empty()) {
        println!("serial:   {s}");
    }
    let cfg = Config::load();
    if d.battery {
        println!("battery:  {}", battery_line(m, &cfg)?);
        if opt(m.battery_type())?.flatten().is_some() {
            println!("type:     {}", battery_type_name(m, &cfg)?);
        }
        if let Some(e) = estimate::estimate(&history::load(), history::now()) {
            println!(
                "lasts:    {} more ({:.1}% a day)",
                estimate::describe(e.remaining_secs),
                e.percent_per_day
            );
        }
    }
    if let Some((x, y)) = opt(m.dpi())? {
        if x == y {
            println!("dpi:      {x} (max {})", d.dpi_max);
        } else {
            println!("dpi:      {x} x {y} (max {})", d.dpi_max);
        }
    }
    if let Some(s) = opt(m.dpi_stages())? {
        let list: Vec<String> = s.stages.iter().map(|v| v.0.to_string()).collect();
        println!("stages:   {} (active {})", list.join(", "), s.active);
    }
    if let Some(p) = opt(m.poll_rate())? {
        println!("poll:     {p} Hz (supports {:?})", d.poll_rates);
    }
    let idle = opt(m.idle_time())?;
    if let Some(s) = idle {
        println!("sleep:    {s} s");
    }
    if let Some(p) = opt(m.poll_rate())?.filter(|_| !d.poll_rates.is_empty()) {
        let mode = power::current(d.poll_rates, p, idle, cfg.power);
        println!("power:    {}", mode.map_or("custom", power::Mode::label));
    }
    if let Some(l) = opt(m.low_battery_threshold())? {
        println!("lowbat:   {}", fmt_lowbat(l));
    }
    if let Some(f) = opt(m.free_spin())? {
        println!("scroll:   {}", if f { "free spin" } else { "tactile" });
    }
    if let Some(a) = opt(m.scroll_acceleration())? {
        println!("accel:    {}", if a { "on" } else { "off" });
    }
    if let Some(s) = opt(m.smart_reel())? {
        println!("smartreel:{}", if s { " on" } else { " off" });
    }
    Ok(())
}
