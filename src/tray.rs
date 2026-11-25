//! Tray app: battery icon, right-click menu, and an on-demand stage editor.
//! Everything is plain Win32; the editor window only exists while open.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Config, TrayIcon};
use crate::devices::Device;
use crate::power::{self, Mode};
use crate::razer::{self, BatteryType, DpiStages, Error, Mouse};

const WM_TRAY: u32 = WM_APP + 1;
/// Sent by a second launch of raisin.exe to the running instance.
const WM_SHOW_SETTINGS: u32 = WM_APP + 2;
/// Sent by the settings process after it changes something.
const WM_RELOAD: u32 = WM_APP + 3;
const TIMER_BATTERY: usize = 1;
const BATTERY_INTERVAL_MS: u32 = 5 * 60 * 1000;
/// Alternate mode: swap between logo and battery gauge this often.
const TIMER_ALTERNATE: usize = 2;
const ALTERNATE_MS: u32 = 4000;
const HOVER_REFRESH: Duration = Duration::from_secs(5);

// Menu command ids.
const ID_STAGE: usize = 100; // 100..105
const ID_POLL: usize = 200; // 200..203
const ID_IDLE: usize = 300; // 300..305
const ID_LOWBAT: usize = 500; // 500..506
const ID_BATTYPE: usize = 600; // 600..603
const ID_LIION: usize = 603; // must follow the BatteryType ids (radio group)
const ID_LIION_STEP: usize = 610;
const ID_TRAYICON: usize = 700; // 700..703
const ID_FREESPIN: usize = 800;
const ID_SCROLL_ACCEL: usize = 801;
const ID_SMART_REEL: usize = 802;
const ID_POWER: usize = 900; // 900..903
/// A drop this large between two readings = Li-ion low-voltage signal.
const STEP_DROP: u8 = 30;
const ID_SETTINGS: usize = 400;
const ID_AUTOSTART: usize = 401;
const ID_REFRESH: usize = 402;
const ID_EXIT: usize = 403;

const IDLE_CHOICES: [u16; 5] = [60, 120, 300, 600, 900];
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fmt_volts(mv: Option<u16>) -> String {
    mv.map_or(String::new(), |mv| format!(" · ≈{}.{:02} V", mv / 1000, mv % 1000 / 10))
}

fn copy_w(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for c in s.encode_utf16().take(dst.len() - 1) {
        dst[n] = c;
        n += 1;
    }
    dst[n] = 0;
}

/// What we last read from the mouse. `None` fields are unknown or not
/// supported by this model.
#[derive(Default)]
struct State {
    device: Option<&'static Device>,
    connection: &'static str,
    firmware: Option<(u8, u8)>,
    serial: Option<String>,
    battery: Option<u8>,
    charging: Option<bool>,
    dpi: Option<(u16, u16)>,
    stages: Option<DpiStages>,
    poll: Option<u16>,
    idle: Option<u16>,
    /// Raw low-battery blink threshold.
    lowbat: Option<u8>,
    battery_type: Option<BatteryType>,
    voltage_mv: Option<u16>,
    free_spin: Option<bool>,
    scroll_accel: Option<bool>,
    smart_reel: Option<bool>,
    asleep: bool,
    connected: bool,
}

struct App {
    hwnd: HWND,
    mouse: Option<Mouse>,
    state: State,
    /// Battery gauge icon (rebuilt on every refresh).
    icon: HICON,
    /// The app logo, loaded once from the exe's resources.
    logo_icon: HICON,
    /// Alternate mode: currently showing the logo.
    logo_phase: bool,
    last_refresh: Option<Instant>,
    /// Low-battery notification already shown for the current batteries.
    warned: bool,
    /// Previous battery reading, for spotting a Li-ion low-voltage step.
    prev_battery: Option<u8>,
    config: Config,
    taskbar_created: u32,
    /// True in the settings process: no tray icon or notifications.
    headless: bool,
}

/// Tell the tray process to re-read settings and the mouse.
fn notify_tray() {
    unsafe {
        let tray = FindWindowExW(HWND_MESSAGE, null_mut(), w("RaisinTray").as_ptr(), null());
        if !tray.is_null() {
            PostMessageW(tray, WM_RELOAD, 0, 0);
        }
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the app state. Returns None if the state is already in use:
/// TrackPopupMenu and MessageBox run nested message loops, so tray clicks
/// can arrive while a handler is still running. Those are dropped.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

impl App {
    fn mouse(&mut self) -> Option<&Mouse> {
        if self.mouse.is_none() {
            self.mouse = Mouse::open().ok();
        }
        self.mouse.as_ref()
    }

    /// Classify an error: drop the handle if the receiver went away.
    fn note_err(&mut self, e: &Error) {
        match e {
            Error::Timeout => self.state.asleep = true,
            Error::Io | Error::NotFound => {
                self.mouse = None;
                self.state.connected = false;
            }
            _ => {}
        }
    }

    fn refresh(&mut self, full: bool) {
        self.last_refresh = Some(Instant::now());
        let prev_pid = self.state.device.map(|d| d.pid);
        let Some(m) = self.mouse() else {
            self.state = State::default();
            self.update_icon();
            return;
        };
        let device = m.info;
        let connection = m.connection();
        let battery = m.battery();
        let voltage = m.voltage_mv();
        let charging = m.charging();
        let dpi = m.dpi();
        let poll = m.poll_rate();
        let full_reads = full.then(|| {
            (
                m.dpi_stages(),
                m.idle_time(),
                m.low_battery_threshold(),
                m.battery_type(),
                m.free_spin(),
                m.scroll_acceleration(),
                m.smart_reel(),
            )
        });
        // Firmware and serial don't change: read them once per mouse.
        let ident = (prev_pid != Some(device.pid)).then(|| (m.firmware(), m.serial()));

        if prev_pid != Some(device.pid) {
            self.state = State::default(); // a different mouse was plugged in
        }
        self.state.device = Some(device);
        self.state.connection = connection;
        self.state.connected = true;
        self.state.asleep = false;

        // Store a read: "not supported" clears the field, other errors are
        // classified (asleep, unplugged) and keep the last known value.
        macro_rules! apply {
            ($field:ident, $r:expr) => {
                match $r {
                    Ok(v) => self.state.$field = Some(v),
                    Err(Error::NotSupported) => self.state.$field = None,
                    Err(e) => self.note_err(&e),
                }
            };
        }
        apply!(battery, battery);
        apply!(voltage_mv, voltage);
        apply!(charging, charging);
        apply!(dpi, dpi);
        apply!(poll, poll);
        if let Some((fw, serial)) = ident {
            apply!(firmware, fw);
            apply!(serial, serial);
        }
        match full_reads {
            Some((stages, idle, lowbat, btype, spin, accel, reel)) => {
                apply!(stages, stages);
                apply!(idle, idle);
                apply!(lowbat, lowbat);
                match btype {
                    Ok(t) => self.state.battery_type = t,
                    Err(Error::NotSupported) => self.state.battery_type = None,
                    Err(e) => self.note_err(&e),
                }
                apply!(free_spin, spin);
                apply!(scroll_accel, accel);
                apply!(smart_reel, reel);
            }
            None => {
                // Light refresh: follow DPI-button presses by matching the
                // current DPI to a stage.
                if let (Some(st), Some((x, _))) = (self.state.stages.as_mut(), self.state.dpi) {
                    let cur = st.stages.get((st.active as usize).max(1) - 1).map(|s| s.0);
                    if cur != Some(x) {
                        if let Some(i) = st.stages.iter().position(|s| s.0 == x) {
                            st.active = (i + 1) as u8;
                        }
                    }
                }
            }
        }
        self.update_icon();
        self.check_low_battery();
    }

    /// Li-ion profile active (alkaline curve + Raisin's Li-ion setting).
    fn liion(&self) -> bool {
        self.config.liion && self.state.battery_type == Some(BatteryType::Alkaline)
    }

    /// Li-ion: High/Low from voltage; the percentage isn't meaningful.
    fn liion_high(&self) -> bool {
        config::liion_high(self.state.voltage_mv, self.state.battery)
    }

    /// "87%" or, for Li-ion, "High"/"Low"; plus voltage and charging.
    fn battery_text(&self) -> Option<String> {
        let b = self.state.battery?;
        let level = if self.liion() {
            if self.liion_high() { "High" } else { "Low" }.to_string()
        } else {
            format!("{b}%")
        };
        let charging = if self.state.charging == Some(true) {
            " · charging"
        } else {
            ""
        };
        Some(format!("{level}{}{charging}", fmt_volts(self.state.voltage_mv)))
    }

    /// Level for the battery icon (Li-ion: full or nearly empty).
    fn icon_level(&self) -> Option<u8> {
        if self.liion() {
            Some(if self.liion_high() { 100 } else { 5 })
        } else {
            self.state.battery
        }
    }

    /// Polling rates the connected mouse supports.
    fn poll_rates(&self) -> &'static [u16] {
        self.state.device.map_or(&[], |d| d.poll_rates)
    }

    fn device_name(&self) -> &'static str {
        self.state.device.map_or("Razer mouse", |d| d.name)
    }

    fn tooltip(&self) -> String {
        let s = &self.state;
        if !s.connected {
            return "Raisin: no supported mouse found".into();
        }
        if s.asleep {
            return format!("Raisin: {} asleep", self.device_name());
        }
        let mut t = self.device_name().to_string();
        if let Some(b) = self.battery_text() {
            t += &format!("\nBattery {b}");
        }
        if let Some((x, _)) = s.dpi {
            t += &format!("\n{x} DPI");
        }
        if let Some(p) = s.poll {
            t += &format!(" · {p} Hz");
        }
        t
    }
    fn nid(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    fn add_icon(&mut self) {
        if self.headless {
            return;
        }
        if self.icon.is_null() {
            self.icon = make_battery_icon(self.icon_level(), self.state.connected && !self.state.asleep);
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.shown_icon();
        copy_w(&mut nid.szTip, &self.tooltip());
        unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
    }

    /// Rebuild the battery gauge from the latest reading and show it.
    fn update_icon(&mut self) {
        if self.headless {
            return;
        }
        let old = self.icon;
        self.icon = make_battery_icon(self.icon_level(), self.state.connected && !self.state.asleep);
        self.push_icon();
        if !old.is_null() {
            unsafe { DestroyIcon(old) };
        }
    }

    /// Battery at or below the warning level: the gauge always wins then.
    fn battery_low(&self) -> bool {
        if !self.state.connected || self.state.asleep || self.state.battery.is_none() {
            return false;
        }
        if self.liion() {
            return !self.liion_high();
        }
        let t = self.state.lowbat.map_or(15, razer::raw_to_percent);
        self.state.battery.is_some_and(|b| b <= t)
    }

    fn shown_icon(&self) -> HICON {
        // Mice without a battery have no gauge to show.
        let no_battery = self.state.device.is_some_and(|d| !d.battery);
        let logo = !self.logo_icon.is_null()
            && !self.battery_low()
            && (no_battery
                || match self.config.tray_icon {
                    TrayIcon::Battery => false,
                    TrayIcon::Logo => true,
                    TrayIcon::Alternate => self.logo_phase,
                });
        if logo {
            self.logo_icon
        } else {
            self.icon
        }
    }

    fn push_icon(&self) {
        if self.headless {
            return;
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = self.shown_icon();
        copy_w(&mut nid.szTip, &self.tooltip());
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    /// Start or stop the swap timer to match the configured tray icon mode.
    fn apply_tray_mode(&mut self) {
        if self.headless {
            return;
        }
        self.logo_phase = false;
        unsafe {
            if self.config.tray_icon == TrayIcon::Alternate {
                SetTimer(self.hwnd, TIMER_ALTERNATE, ALTERNATE_MS, None);
            } else {
                KillTimer(self.hwnd, TIMER_ALTERNATE);
            }
        }
        self.push_icon();
    }

    fn set_tray_icon(&mut self, mode: TrayIcon) {
        self.config.tray_icon = mode;
        self.config.save();
        if self.headless {
            settings::sync(self, false);
            notify_tray();
        } else {
            self.apply_tray_mode();
        }
    }

    fn reset_battery_tracking(&mut self) {
        self.warned = false;
        self.prev_battery = None;
    }

    fn notify(&self, title: &str, text: &str) {
        if self.headless {
            return;
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_WARNING;
        copy_w(&mut nid.szInfoTitle, title);
        copy_w(&mut nid.szInfo, text);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    fn check_low_battery(&mut self) {
        if self.headless {
            return; // the tray process owns notifications
        }
        let Some(b) = self.state.battery else { return };
        let prev = self.prev_battery.replace(b);
        let name = self.device_name();
        if self.state.charging == Some(true) {
            self.warned = false; // being charged: nothing to warn about
            return;
        }

        if self.liion() {
            // Li-ion: High/Low from voltage. Smart cells drop their voltage
            // on purpose when nearly empty; the reading falls off a cliff.
            let stepped = self.config.liion_step && prev.is_some_and(|p| p >= 50 && b + STEP_DROP <= p);
            if self.liion_high() && !stepped {
                self.warned = false;
            } else if !self.warned {
                self.warned = true;
                self.notify(
                    "Mouse battery low",
                    &format!("The Li-ion battery in your {name} is nearly empty. Swap or charge it soon."),
                );
            }
            return;
        }

        if b > 80 {
            self.warned = false; // fresh batteries
        }
        // Same threshold as the mouse's own red blink (default 15% until read).
        let t = self.state.lowbat.map_or(15, razer::raw_to_percent);
        if b <= t && !self.warned {
            self.warned = true;
            let action = if self.state.device.is_some_and(|d| d.charging) && self.state.battery_type.is_none() {
                "Time to charge it."
            } else {
                "Time for fresh batteries."
            };
            self.notify("Mouse battery low", &format!("Your {name} is at {b}%. {action}"));
        }
    }

    fn show_menu(&mut self) {
        self.refresh(true);
        let s = &self.state;
        unsafe {
            let menu = CreatePopupMenu();
            AppendMenuW(menu, MF_STRING, ID_SETTINGS, w("Settings…").as_ptr());
            SetMenuDefaultItem(menu, ID_SETTINGS as u32, 0);
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            let info = |m: HMENU, text: &str| {
                AppendMenuW(m, MF_STRING | MF_GRAYED, 0, w(text).as_ptr());
            };
            if !s.connected {
                info(menu, "No supported mouse found");
            } else if s.asleep {
                info(menu, &format!("{} asleep (move it to wake)", self.device_name()));
            } else {
                info(menu, self.device_name());
                if let Some(b) = self.battery_text() {
                    info(menu, &format!("Battery: {b}"));
                }
                match (s.dpi, s.poll) {
                    (Some((x, _)), Some(p)) => info(menu, &format!("{x} DPI · {p} Hz")),
                    (Some((x, _)), None) => info(menu, &format!("{x} DPI")),
                    (None, Some(p)) => info(menu, &format!("{p} Hz")),
                    (None, None) => {}
                }
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());

            let usable = s.connected && !s.asleep;
            let dis = if usable { 0 } else { MF_GRAYED };

            // DPI stages
            if let Some(st) = &s.stages {
                let sub = CreatePopupMenu();
                for (i, (x, _)) in st.stages.iter().enumerate() {
                    AppendMenuW(
                        sub,
                        MF_STRING | dis,
                        ID_STAGE + i,
                        w(&format!("Stage {}: {x} DPI", i + 1)).as_ptr(),
                    );
                }
                if st.active >= 1 && (st.active as usize) <= st.stages.len() {
                    let last = ID_STAGE + st.stages.len() - 1;
                    CheckMenuRadioItem(
                        sub,
                        ID_STAGE as u32,
                        last as u32,
                        (ID_STAGE + st.active as usize - 1) as u32,
                        MF_BYCOMMAND,
                    );
                }
                AppendMenuW(menu, MF_POPUP | dis, sub as usize, w("DPI stage").as_ptr());
            }

            // Power mode (presets for polling rate + sleep timer)
            let rates = self.poll_rates();
            if s.poll.is_some() && !rates.is_empty() {
                let sub = CreatePopupMenu();
                for (i, m) in Mode::ALL.iter().enumerate() {
                    AppendMenuW(sub, MF_STRING | dis, ID_POWER + i, w(m.label()).as_ptr());
                }
                if let Some(i) = self.power_mode().and_then(|p| Mode::ALL.iter().position(|&m| m == p)) {
                    let last = ID_POWER + Mode::ALL.len() - 1;
                    CheckMenuRadioItem(sub, ID_POWER as u32, last as u32, (ID_POWER + i) as u32, MF_BYCOMMAND);
                }
                AppendMenuW(menu, MF_POPUP | dis, sub as usize, w("Power mode").as_ptr());
            }

            // Polling rate (this mouse's rates)
            if s.poll.is_some() && !rates.is_empty() {
                let sub = CreatePopupMenu();
                for (i, hz) in rates.iter().enumerate() {
                    AppendMenuW(sub, MF_STRING | dis, ID_POLL + i, w(&format!("{hz} Hz")).as_ptr());
                }
                if let Some(i) = s.poll.and_then(|p| rates.iter().position(|&r| r == p)) {
                    CheckMenuRadioItem(
                        sub,
                        ID_POLL as u32,
                        (ID_POLL + rates.len() - 1) as u32,
                        (ID_POLL + i) as u32,
                        MF_BYCOMMAND,
                    );
                }
                AppendMenuW(menu, MF_POPUP | dis, sub as usize, w("Polling rate").as_ptr());
            }

            // Scroll wheel
            if s.free_spin.is_some() || s.scroll_accel.is_some() || s.smart_reel.is_some() {
                let sub = CreatePopupMenu();
                let check = |v: Option<bool>| if v == Some(true) { MF_CHECKED } else { 0 };
                if s.free_spin.is_some() {
                    AppendMenuW(
                        sub,
                        MF_STRING | dis | check(s.free_spin),
                        ID_FREESPIN,
                        w("Free spin").as_ptr(),
                    );
                }
                if s.scroll_accel.is_some() {
                    let f = MF_STRING | dis | check(s.scroll_accel);
                    AppendMenuW(sub, f, ID_SCROLL_ACCEL, w("Scroll acceleration").as_ptr());
                }
                if s.smart_reel.is_some() {
                    AppendMenuW(
                        sub,
                        MF_STRING | dis | check(s.smart_reel),
                        ID_SMART_REEL,
                        w("Smart Reel").as_ptr(),
                    );
                }
                AppendMenuW(menu, MF_POPUP | dis, sub as usize, w("Scroll wheel").as_ptr());
            }

            // Sleep timer
            if s.idle.is_some() {
                let sub = CreatePopupMenu();
                for (i, secs) in IDLE_CHOICES.iter().enumerate() {
                    AppendMenuW(
                        sub,
                        MF_STRING | dis,
                        ID_IDLE + i,
                        w(&format!("{} min", secs / 60)).as_ptr(),
                    );
                }
                if let Some(i) = s.idle.and_then(|v| IDLE_CHOICES.iter().position(|&c| c == v)) {
                    CheckMenuRadioItem(
                        sub,
                        ID_IDLE as u32,
                        (ID_IDLE + IDLE_CHOICES.len() - 1) as u32,
                        (ID_IDLE + i) as u32,
                        MF_BYCOMMAND,
                    );
                }
                AppendMenuW(menu, MF_POPUP | dis, sub as usize, w("Sleep after").as_ptr());
            }

            // Battery type (discharge curve, AA mice). Li-ion is Raisin's own
            // profile: alkaline curve + low threshold, in the settings file.
            if s.battery_type.is_some() {
                let sub = CreatePopupMenu();
                for (i, t) in BatteryType::ALL.iter().enumerate() {
                    AppendMenuW(sub, MF_STRING | dis, ID_BATTYPE + i, w(t.name()).as_ptr());
                }
                AppendMenuW(sub, MF_STRING | dis, ID_LIION, w("Li-ion 1.5 V rechargeable").as_ptr());
                let liion_active = self.battery_choice() == Some(3);
                if let Some(i) = self.battery_choice() {
                    CheckMenuRadioItem(
                        sub,
                        ID_BATTYPE as u32,
                        ID_LIION as u32,
                        (ID_BATTYPE + i) as u32,
                        MF_BYCOMMAND,
                    );
                }
                AppendMenuW(sub, MF_SEPARATOR, 0, null());
                let step_flags = if !liion_active || !usable { MF_GRAYED } else { 0 }
                    | if self.config.liion_step { MF_CHECKED } else { 0 };
                AppendMenuW(
                    sub,
                    MF_STRING | step_flags,
                    ID_LIION_STEP,
                    w("Li-ion cells drop voltage when low").as_ptr(),
                );
                AppendMenuW(menu, MF_POPUP | dis, sub as usize, w("Battery type").as_ptr());
            }

            // Low-battery blink threshold
            if s.lowbat.is_some() {
                let sub = CreatePopupMenu();
                for (i, p) in razer::LOW_BATTERY_CHOICES.iter().enumerate() {
                    AppendMenuW(sub, MF_STRING | dis, ID_LOWBAT + i, w(&format!("{p}%")).as_ptr());
                }
                if let Some(i) = self.lowbat_index() {
                    let last = ID_LOWBAT + razer::LOW_BATTERY_CHOICES.len() - 1;
                    CheckMenuRadioItem(sub, ID_LOWBAT as u32, last as u32, (ID_LOWBAT + i) as u32, MF_BYCOMMAND);
                }
                AppendMenuW(menu, MF_POPUP | dis, sub as usize, w("Low battery warning").as_ptr());
            }

            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            // Tray icon (not a mouse setting, so always available)
            let sub = CreatePopupMenu();
            for (i, t) in TrayIcon::ALL.iter().enumerate() {
                AppendMenuW(sub, MF_STRING, ID_TRAYICON + i, w(t.label()).as_ptr());
            }
            if let Some(i) = TrayIcon::ALL.iter().position(|&t| t == self.config.tray_icon) {
                CheckMenuRadioItem(
                    sub,
                    ID_TRAYICON as u32,
                    (ID_TRAYICON + 2) as u32,
                    (ID_TRAYICON + i) as u32,
                    MF_BYCOMMAND,
                );
            }
            AppendMenuW(menu, MF_POPUP, sub as usize, w("Tray icon").as_ptr());
            let auto = if autostart_enabled() { MF_CHECKED } else { 0 };
            AppendMenuW(menu, MF_STRING | auto, ID_AUTOSTART, w("Start with Windows").as_ptr());
            AppendMenuW(menu, MF_STRING, ID_REFRESH, w("Refresh").as_ptr());
            AppendMenuW(menu, MF_STRING, ID_EXIT, w("Exit").as_ptr());

            let mut pt: POINT = zeroed();
            GetCursorPos(&mut pt);
            SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                null(),
            ) as usize;
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu); // destroys submenus too
            if cmd != 0 {
                self.command(cmd);
            }
        }
    }

    fn command(&mut self, cmd: usize) {
        match cmd {
            c if (ID_STAGE..ID_STAGE + 5).contains(&c) => self.act(|a| a.select_stage(c - ID_STAGE)),
            c if (ID_POLL..ID_POLL + self.poll_rates().len()).contains(&c) => {
                let hz = self.poll_rates()[c - ID_POLL];
                self.act(|a| a.set_poll(hz))
            }
            c if (ID_POWER..ID_POWER + Mode::ALL.len()).contains(&c) => {
                self.act(|a| a.set_power(Mode::ALL[c - ID_POWER]))
            }
            ID_FREESPIN => {
                let on = self.state.free_spin != Some(true);
                self.act(|a| a.need_mouse()?.set_free_spin(on))
            }
            ID_SCROLL_ACCEL => {
                let on = self.state.scroll_accel != Some(true);
                self.act(|a| a.need_mouse()?.set_scroll_acceleration(on))
            }
            ID_SMART_REEL => {
                let on = self.state.smart_reel != Some(true);
                self.act(|a| a.need_mouse()?.set_smart_reel(on))
            }
            c if (ID_IDLE..ID_IDLE + IDLE_CHOICES.len()).contains(&c) => {
                self.act(|a| a.set_idle(IDLE_CHOICES[c - ID_IDLE]))
            }
            c if (ID_LOWBAT..ID_LOWBAT + razer::LOW_BATTERY_CHOICES.len()).contains(&c) => {
                self.act(|a| a.set_lowbat(razer::LOW_BATTERY_CHOICES[c - ID_LOWBAT]))
            }
            c if (ID_BATTYPE..=ID_LIION).contains(&c) => self.act(|a| a.set_battery_choice(c - ID_BATTYPE)),
            ID_LIION_STEP => {
                let on = !self.config.liion_step;
                self.act(|a| a.set_liion_step(on))
            }
            ID_SETTINGS => self.open_settings(),
            ID_AUTOSTART => {
                set_autostart(!autostart_enabled());
                settings::sync(self, false);
            }
            c if (ID_TRAYICON..ID_TRAYICON + 3).contains(&c) => self.set_tray_icon(TrayIcon::ALL[c - ID_TRAYICON]),
            ID_REFRESH => self.act(|_| Ok(())),
            ID_EXIT => {
                // Close after this handler releases the app state, so
                // WM_DESTROY can remove the tray icon.
                unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
            }
            _ => {}
        }
    }

    /// Apply a setting: report errors, re-read the mouse, update the window.
    fn act(&mut self, f: impl FnOnce(&mut App) -> razer::Result<()>) {
        if let Err(e) = f(self) {
            self.note_err(&e);
            self.error_balloon(&e);
        }
        self.refresh(true);
        settings::sync(self, false);
        if self.headless {
            notify_tray();
        }
    }

    fn need_mouse(&mut self) -> razer::Result<&Mouse> {
        self.mouse().ok_or(Error::NotFound)
    }

    fn select_stage(&mut self, i: usize) -> razer::Result<()> {
        let Some(mut st) = self.state.stages.clone() else {
            return Ok(());
        };
        if i >= st.stages.len() {
            return Ok(());
        }
        st.active = (i + 1) as u8;
        let (x, y) = st.stages[i];
        let m = self.need_mouse()?;
        m.set_dpi_stages(&st)?;
        m.set_dpi(x, y)
    }

    fn set_poll(&mut self, hz: u16) -> razer::Result<()> {
        self.need_mouse()?.set_poll_rate(hz)
    }

    fn set_idle(&mut self, secs: u16) -> razer::Result<()> {
        self.need_mouse()?.set_idle_time(secs)
    }

    /// Apply a power mode: polling rate, then the sleep timer if the mouse
    /// has one.
    fn set_power(&mut self, mode: Mode) -> razer::Result<()> {
        self.config.power = Some(mode);
        self.config.save();
        let rates = self.poll_rates();
        let has_idle = self.state.idle.is_some();
        let m = self.need_mouse()?;
        if let Some(hz) = mode.poll(rates) {
            m.set_poll_rate(hz)?;
        }
        if has_idle {
            m.set_idle_time(mode.idle())?;
        }
        Ok(())
    }

    /// The power mode the current settings match, if any.
    fn power_mode(&self) -> Option<Mode> {
        let poll = self.state.poll?;
        power::current(self.poll_rates(), poll, self.state.idle, self.config.power)
    }

    fn set_lowbat(&mut self, pct: u8) -> razer::Result<()> {
        self.need_mouse()?.set_low_battery_percent(pct)
    }

    /// 0-2 = BatteryType::ALL, 3 = Raisin's Li-ion profile.
    fn set_battery_choice(&mut self, choice: usize) -> razer::Result<()> {
        self.reset_battery_tracking(); // level jumps with the new curve
        self.config.liion = choice == 3;
        self.config.save();
        let pct = self.config.liion_threshold();
        let m = self.need_mouse()?;
        match BatteryType::ALL.get(choice) {
            Some(&t) => m.set_battery_type(t),
            None => {
                m.set_battery_type(BatteryType::Alkaline)?;
                m.set_low_battery_percent(pct)
            }
        }
    }

    fn set_liion_step(&mut self, on: bool) -> razer::Result<()> {
        self.config.liion_step = on;
        self.config.save();
        let pct = self.config.liion_threshold();
        self.need_mouse()?.set_low_battery_percent(pct)
    }

    /// Current battery choice index (see set_battery_choice).
    fn battery_choice(&self) -> Option<usize> {
        let t = self.state.battery_type?;
        if self.config.liion && t == BatteryType::Alkaline {
            Some(3)
        } else {
            BatteryType::ALL.iter().position(|&x| x == t)
        }
    }

    /// Index into LOW_BATTERY_CHOICES nearest to what the mouse holds.
    fn lowbat_index(&self) -> Option<usize> {
        let pct = razer::raw_to_percent(self.state.lowbat?) as i32;
        razer::LOW_BATTERY_CHOICES
            .iter()
            .enumerate()
            .min_by_key(|(_, &c)| (c as i32 - pct).abs())
            .map(|(i, _)| i)
    }

    fn error_balloon(&self, e: &Error) {
        if self.headless {
            return; // the settings window shows the mouse's state instead
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_ERROR;
        copy_w(&mut nid.szInfoTitle, "Raisin");
        copy_w(&mut nid.szInfo, &format!("Couldn't apply setting: {e}"));
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    /// The settings window runs as its own `raisin.exe --settings` process,
    /// so its Direct2D memory is returned to the OS when it closes.
    fn open_settings(&mut self) {
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--settings").spawn();
        }
    }

    fn apply_stages(&mut self, values: Vec<u16>) {
        if values.is_empty() {
            return;
        }
        let active_dpi = self.state.dpi.map(|d| d.0);
        let old_active = self.state.stages.as_ref().map_or(1, |s| s.active);
        // Keep the same active stage number if it still exists.
        let active = (old_active as usize).clamp(1, values.len()) as u8;
        let st = DpiStages {
            active,
            stages: values.iter().map(|&v| (v, v)).collect(),
        };
        let target = st.stages[active as usize - 1];
        self.act(|a| {
            let m = a.need_mouse()?;
            m.set_dpi_stages(&st)?;
            if active_dpi != Some(target.0) {
                m.set_dpi(target.0, target.1)?;
            }
            Ok(())
        });
        settings::sync(self, true);
    }
}

// ---------- autostart ----------

fn exe_path() -> Vec<u16> {
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    buf.truncate(n);
    buf
}

fn autostart_enabled() -> bool {
    unsafe {
        let mut key: HKEY = null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return false;
        }
        let r = RegQueryValueExW(key, w("Raisin").as_ptr(), null(), null_mut(), null_mut(), null_mut());
        RegCloseKey(key);
        r == 0
    }
}

fn set_autostart(on: bool) {
    unsafe {
        let mut key: HKEY = null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), 0, KEY_SET_VALUE, &mut key) != 0 {
            return;
        }
        if on {
            let mut v: Vec<u16> = "\"".encode_utf16().collect();
            v.extend(exe_path());
            v.extend("\"".encode_utf16());
            v.push(0);
            RegSetValueExW(
                key,
                w("Raisin").as_ptr(),
                0,
                REG_SZ,
                v.as_ptr() as *const u8,
                (v.len() * 2) as u32,
            );
        } else {
            RegDeleteValueW(key, w("Raisin").as_ptr());
        }
        RegCloseKey(key);
    }
}

fn light_taskbar() -> bool {
    unsafe {
        let mut v: u32 = 0;
        let mut sz = 4u32;
        RegGetValueW(
            HKEY_CURRENT_USER,
            w(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            w("SystemUsesLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut v as *mut u32 as *mut _,
            &mut sz,
        ) == 0
            && v == 1
    }
}

// ---------- icon ----------

/// Draw a horizontal battery glyph, filled to `level`, as a 32-bit alpha icon.
fn make_battery_icon(level: Option<u8>, active: bool) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as i32;
    let px = (size as f32 / 16.0).max(1.0);
    let t = px.round() as i32; // stroke thickness
    let outline: u32 = if !active {
        0xFF80_8080
    } else if light_taskbar() {
        0xFF20_2020
    } else {
        0xFFF0_F0F0
    };
    let fill: u32 = match level {
        _ if !active => 0,
        Some(l) if l <= 5 => 0xFFE5_3935,
        Some(l) if l <= 15 => 0xFFFB_8C00,
        Some(_) => 0xFF43_A047,
        None => 0,
    };

    // Geometry (in pixels) for the body and the terminal nub.
    let left = (1.0 * px) as i32;
    let right = size - (3.0 * px) as i32; // exclusive
    let top = (4.0 * px) as i32;
    let bottom = size - (4.0 * px) as i32; // exclusive
    let nub_w = ((2.0 * px) as i32).max(1);
    let nub_top = top + (bottom - top) / 3;
    let nub_bot = bottom - (bottom - top) / 3;

    let inner_l = left + t + t.max(1);
    let inner_r = right - t - t.max(1);
    let inner_t = top + t + t.max(1);
    let inner_b = bottom - t - t.max(1);
    let pct = level.unwrap_or(0).min(100) as i32;
    let fill_r = inner_l + ((inner_r - inner_l) * pct + 99) / 100;

    let mut pixels = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let in_body = x >= left && x < right && y >= top && y < bottom;
            let in_hole = x >= left + t && x < right - t && y >= top + t && y < bottom - t;
            let in_nub = x >= right && x < right + nub_w && y >= nub_top && y < nub_bot;
            let in_fill = fill != 0 && x >= inner_l && x < fill_r.max(inner_l + 1) && y >= inner_t && y < inner_b;
            let c = if (in_body && !in_hole) || in_nub {
                outline
            } else if in_fill {
                fill
            } else {
                0
            };
            pixels[(y * size + x) as usize] = c;
        }
    }

    unsafe {
        let mut bmi: BITMAPINFO = zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size;
        bmi.bmiHeader.biHeight = -size; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut core::ffi::c_void = null_mut();
        let hdc = GetDC(null_mut());
        let color = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        ReleaseDC(null_mut(), hdc);
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        let mask = CreateBitmap(size, size, 1, 1, null());
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

// ---------- window proc / entry ----------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lp as u32 {
                WM_RBUTTONUP => {
                    with_app(|a| a.show_menu());
                }
                WM_LBUTTONUP => {
                    with_app(|a| a.open_settings());
                }
                WM_MOUSEMOVE => {
                    with_app(|a| {
                        if a.last_refresh.is_none_or(|t| t.elapsed() > HOVER_REFRESH) {
                            a.refresh(false);
                        }
                    });
                }
                _ => {}
            }
            0
        }
        WM_SHOW_SETTINGS => {
            with_app(|a| a.open_settings());
            0
        }
        WM_RELOAD => {
            with_app(|a| {
                a.config = Config::load();
                a.refresh(true);
                a.apply_tray_mode();
            });
            0
        }
        WM_TIMER if wp == TIMER_BATTERY => {
            with_app(|a| a.refresh(false));
            0
        }
        WM_TIMER if wp == TIMER_ALTERNATE => {
            with_app(|a| {
                a.logo_phase = !a.logo_phase;
                a.push_icon();
            });
            0
        }
        WM_DESTROY => {
            with_app(|a| {
                let nid = a.nid();
                Shell_NotifyIconW(NIM_DELETE, &nid);
            });
            PostQuitMessage(0);
            0
        }
        _ => {
            // Explorer restarted: re-add the icon.
            if with_app(|a| msg == a.taskbar_created).unwrap_or(false) {
                with_app(|a| a.add_icon());
                return 0;
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}

pub fn run() {
    unsafe {
        // One instance only.
        CreateMutexW(null(), 0, w("Local\\RaisinTray").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Tray already running: become the settings window instead.
            run_settings();
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = GetDpiForSystem();

        let hinst = GetModuleHandleW(null());
        let class = w("RaisinTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            hinst,
            null(),
        );
        let taskbar_created = RegisterWindowMessageW(w("TaskbarCreated").as_ptr());
        let px = GetSystemMetrics(SM_CXSMICON);
        let logo_icon = LoadImageW(
            hinst,
            1 as windows_sys::core::PCWSTR,
            IMAGE_ICON,
            px,
            px,
            LR_DEFAULTCOLOR,
        ) as HICON;

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                hwnd,
                mouse: None,
                state: State::default(),
                icon: null_mut(),
                logo_icon,
                logo_phase: false,
                last_refresh: None,
                warned: false,
                prev_battery: None,
                config: Config::load(),
                taskbar_created,
                headless: false,
            })
        });
        with_app(|a| {
            a.add_icon();
            a.refresh(true);
            a.apply_tray_mode();
        });
        SetTimer(hwnd, TIMER_BATTERY, BATTERY_INTERVAL_MS, None);
        message_loop();
    }
}

unsafe fn message_loop() {
    let mut msg: MSG = zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// `raisin.exe --settings`: just the settings window, in its own process.
/// Exits when the window closes.
pub fn run_settings() {
    unsafe {
        CreateMutexW(null(), 0, w("Local\\RaisinSettings").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let h = FindWindowW(w("RaisinSettings").as_ptr(), null());
            if !h.is_null() {
                ShowWindow(h, SW_RESTORE);
                SetForegroundWindow(h);
            }
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        settings::register(GetModuleHandleW(null()));
        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                hwnd: null_mut(),
                mouse: None,
                state: State::default(),
                icon: null_mut(),
                logo_icon: null_mut(),
                logo_phase: false,
                last_refresh: None,
                warned: false,
                prev_battery: None,
                config: Config::load(),
                taskbar_created: 0,
                headless: true,
            })
        });
        // Show the window right away ("Reading mouse…"), then fill it in.
        with_app(|a| {
            settings::open(a);
            a.refresh(true);
            settings::sync(a, true);
        });
        message_loop();
    }
}

#[path = "settings.rs"]
mod settings;
