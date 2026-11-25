#![windows_subsystem = "windows"]

mod config;
mod hid;
mod razer;
mod tray;

use config::Config;
use razer::{BatteryType, DpiStages, Mouse};

const USAGE: &str = "\
Raisin: Razer Viper V3 HyperSpeed configurator

  raisin                   run in the system tray
  raisin info              show battery, DPI, stages, polling rate, sleep timer
  raisin dpi <n>           set current DPI
  raisin stages <a,b,..>   set DPI stages (1-5), keeps active stage
  raisin stage <n>         switch to stage n (1-based)
  raisin poll <125|500|1000>
  raisin sleep <seconds>   sleep timer, 60-900
  raisin lowbat <5-30>     red low-battery blink threshold, percent
  raisin battery <alkaline|nimh|lithium|liion|liion-step>
                           battery type. liion = alkaline curve + 5% blink;
                           liion-step = for cells that drop voltage when low
                           (30% blink + Raisin warns on the sudden drop)
  raisin probe             list Razer HID interfaces
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
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    let code = cli(&args);
    std::process::exit(code);
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
                i.vid, i.pid, i.usage_page, i.usage, i.feature_len, i.path_string()
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
                    println!("07/{id:02X} status={:02X} size={:02X}\n  {}", r[0], r[5], args.join(" "));
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
    if !matches!(cmd, "info" | "dpi" | "stages" | "stage" | "poll" | "sleep" | "lowbat" | "battery") {
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

    let r: razer::Result<()> = (|| {
        match cmd {
            "info" => {
                let (pct, raw) = m.battery_raw()?;
                let mv = m.voltage_mv()?;
                println!("battery:  {pct}% (raw {raw}/255) · ≈{}.{:02} V", mv / 1000, mv % 1000 / 10);
                println!("type:     {}", battery_type_name(&m, &Config::load())?);
                let (x, y) = m.dpi()?;
                println!("dpi:      {x} x {y}");
                let s = m.dpi_stages()?;
                let list: Vec<String> = s.stages.iter().map(|v| v.0.to_string()).collect();
                println!("stages:   {} (active {})", list.join(", "), s.active);
                println!("poll:     {} Hz", m.poll_rate()?);
                println!("sleep:    {} s", m.idle_time()?);
                println!("lowbat:   {}", fmt_lowbat(m.low_battery_threshold()?));
            }
            "dpi" => {
                let Some(v) = num(arg) else { return Ok(println!("{USAGE}")) };
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
                    println!("{USAGE}");
                    return Ok(());
                }
                let cur = m.dpi_stages()?;
                let active = (cur.active as usize).clamp(1, vals.len()) as u8;
                let st = DpiStages { active, stages: vals.iter().map(|&v| (v, v)).collect() };
                m.set_dpi_stages(&st)?;
                let (x, y) = st.stages[active as usize - 1];
                m.set_dpi(x, y)?;
                let s = m.dpi_stages()?;
                println!("stages: {:?} (active {})", s.stages.iter().map(|v| v.0).collect::<Vec<_>>(), s.active);
            }
            "stage" => {
                let Some(n) = num(arg) else { return Ok(println!("{USAGE}")) };
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
                let Some(v) = num(arg) else { return Ok(println!("{USAGE}")) };
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
                        let Some(t) = BatteryType::parse(a) else { return Ok(println!("{USAGE}")) };
                        cfg.liion = false;
                        m.set_battery_type(t)?;
                    }
                    None => return Ok(println!("{USAGE}")),
                }
                cfg.save();
                println!("battery type: {}", battery_type_name(&m, &cfg)?);
                println!("battery:      {}%", m.battery()?);
                println!("lowbat:       {}", fmt_lowbat(m.low_battery_threshold()?));
            }
            "sleep" => {
                let Some(v) = num(arg) else { return Ok(println!("{USAGE}")) };
                m.set_idle_time(v)?;
                println!("sleep: {} s", m.idle_time()?);
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
