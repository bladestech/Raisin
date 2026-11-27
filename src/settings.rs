//! Settings window: custom-drawn with Direct2D/DirectWrite on a Windows 11
//! acrylic backdrop. Follows the system light/dark theme and accent color.
//!
//! The layout adapts to the connected mouse: tabs, cards and rows only
//! appear for features it has. The window is as tall as its tallest tab and
//! scrolls when the screen is too small. It runs as its own
//! `raisin.exe --settings` process, so its memory goes back to Windows when
//! it closes.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows::core::{w as wstr, PCWSTR};
use windows::Win32::Foundation::HWND as WHWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::UI::Controls::MARGINS;
use windows_numerics::{Matrix3x2, Vector2};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{autostart_enabled, set_autostart, w, with_app, App, IDLE_CHOICES};
use crate::auto::{AppRule, MAX_RULES};
use crate::config::TrayIcon;
use crate::estimate;
use crate::history::{self, Sample};
use crate::power::Mode;
use crate::razer;

type SysPcwstr = windows_sys::core::PCWSTR;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Posted after painting when the content height changed.
const WM_FIT: u32 = WM_APP + 1;
const CF_UNICODETEXT: u32 = 13;

/// Logo pixels for the header: premultiplied BGRA, generated from
/// Raisin.png by tools/make-icons.ps1.
const LOGO_PX: u32 = 128;
static LOGO_BGRA: &[u8] = include_bytes!("../assets/logo.bgra");

const CLASS: &str = "RaisinSettings";
const TIMER_LIVE: usize = 1;
const LIVE_MS: u32 = 5000;
const TIMER_COPIED: usize = 2;

// Layout, in DIPs.
const WIDTH: f32 = 420.0;
/// Height before the mouse has been read; the window then fits its content.
const INITIAL_HEIGHT: f32 = 640.0;
const PAD: f32 = 16.0;
const CARD_W: f32 = WIDTH - 2.0 * PAD;
const ROW_H: f32 = 30.0;
const ROW_GAP: f32 = 12.0;
const CARD_PAD: f32 = 14.0;
const CARD_GAP: f32 = 12.0;
const CTRL_W: f32 = 252.0;

const BATTERY_LABELS: [&str; 4] = ["Alkaline", "NiMH", "Lithium", "Li-ion"];
const LOWBAT_LABELS: [&str; 6] = ["5%", "10%", "15%", "20%", "25%", "30%"];
const IDLE_LABELS: [&str; 5] = ["1 min", "2 min", "5 min", "10 min", "15 min"];
/// Header (battery, name, logo) height, then the tab strip.
const HEADER_H: f32 = 88.0;
const TAB_H: f32 = 36.0;
/// Info bubble width.
const TIP_W: f32 = 264.0;
const TIP_PAD: f32 = 10.0;

// ---------- data shown in the window ----------

/// Snapshot of app state, so painting never needs the app borrowed.
#[derive(Clone, Default, PartialEq)]
struct View {
    /// Mouse not read yet (window just opened).
    loading: bool,
    connected: bool,
    asleep: bool,
    name: String,
    usb_id: Option<u16>,
    tested: bool,
    connection: &'static str,
    firmware: Option<(u8, u8)>,
    serial: Option<String>,
    battery: Option<u8>,
    voltage_mv: Option<u16>,
    charging: bool,
    liion: bool,
    liion_high: bool,
    /// Battery type selector index (3 = Li-ion profile); None if the mouse
    /// has no battery type setting.
    battery_choice: Option<usize>,
    liion_step: bool,
    lowbat: Option<usize>,
    has_lowbat: bool,
    has_dpi: bool,
    dpi: u16,
    dpi_max: u16,
    has_stages: bool,
    stages: Vec<u16>,
    /// 1-based; 0 = unknown.
    active: usize,
    poll_rates: Vec<u16>,
    poll: Option<usize>,
    /// Index into power::Mode::ALL of the mode the settings match.
    power: Option<usize>,
    has_idle: bool,
    idle: Option<usize>,
    free_spin: Option<bool>,
    scroll_accel: Option<bool>,
    smart_reel: Option<bool>,
    autostart: bool,
    tray_icon: usize,
    auto_power: bool,
    rules: Vec<AppRule>,
    dpi_popup: bool,
    hotkeys: bool,
    /// Battery history graph: (x, y, charging), x and y from 0 to 1 across
    /// the last GRAPH_DAYS. y is the percentage, or the voltage for Li-ion.
    graph: Vec<(f32, f32, bool)>,
    graph_volts: bool,
    /// "about 3 weeks left"
    estimate: Option<String>,
}

/// The battery graph covers this many days, one point per hour at most.
const GRAPH_DAYS: u64 = 7;
const GRAPH_POINTS: f32 = (GRAPH_DAYS * 24) as f32;
/// Li-ion graph range, millivolts: where these cells sit.
const LIION_GRAPH_MV: (f32, f32) = (1300.0, 1550.0);
const GRAPH_H: f32 = 64.0;

thread_local! {
    /// The history file, read again at most once a minute.
    static HISTORY: RefCell<Option<(Instant, Vec<Sample>)>> = const { RefCell::new(None) };
}

fn with_history<R>(f: impl FnOnce(&[Sample]) -> R) -> R {
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        if h.as_ref().is_none_or(|(t, _)| t.elapsed() >= Duration::from_secs(60)) {
            *h = Some((Instant::now(), history::load()));
        }
        f(h.as_ref().map_or(&[], |(_, s)| s))
    })
}

/// Graph points for the last GRAPH_DAYS before `now`: the last reading in
/// each hour. Li-ion (`volts`) plots the voltage, since its percentage
/// means nothing.
fn graph_points(samples: &[Sample], now: u64, volts: bool) -> Vec<(f32, f32, bool)> {
    let span = GRAPH_DAYS * 86400;
    let start = now.saturating_sub(span);
    let (lo, hi) = LIION_GRAPH_MV;
    let mut out: Vec<(f32, f32, bool)> = Vec::new();
    let mut bucket = None;
    for s in samples.iter().filter(|s| s.time >= start && s.time <= now) {
        let y = if volts {
            let Some(mv) = s.voltage_mv else { continue };
            ((mv as f32 - lo) / (hi - lo)).clamp(0.0, 1.0)
        } else {
            s.percent as f32 / 100.0
        };
        let x = (s.time - start) as f32 / span as f32;
        let b = (x * GRAPH_POINTS) as u32;
        let p = (x, y, s.charging);
        match out.last_mut() {
            Some(last) if bucket == Some(b) => *last = p,
            _ => out.push(p),
        }
        bucket = Some(b);
    }
    out
}

impl View {
    fn from_app(app: &App) -> View {
        let s = &app.state;
        let d = s.device;
        let (graph, estimate) = if s.battery.is_some() {
            with_history(|h| {
                let now = history::now();
                let e = estimate::estimate(h, now).map(|e| {
                    let d = estimate::describe(e.remaining_secs);
                    let mut c = d.chars();
                    let first = c.next().map(|f| f.to_uppercase().to_string()).unwrap_or_default();
                    format!("{first}{} left", c.as_str())
                });
                (graph_points(h, now, app.liion()), e)
            })
        } else {
            (Vec::new(), None)
        };
        View {
            loading: app.last_refresh.is_none(),
            connected: s.connected,
            asleep: s.asleep,
            name: d.map_or("Razer mouse", |d| d.name).to_string(),
            usb_id: d.map(|d| d.pid),
            tested: d.is_some_and(|d| d.tested()),
            connection: s.connection,
            firmware: s.firmware,
            serial: s.serial.clone().filter(|s| !s.is_empty()),
            battery: s.battery,
            voltage_mv: s.voltage_mv,
            charging: s.charging == Some(true),
            liion: app.liion(),
            liion_high: app.liion_high(),
            battery_choice: app.battery_choice(),
            liion_step: app.config.liion_step,
            lowbat: app.lowbat_index(),
            has_lowbat: s.lowbat.is_some(),
            has_dpi: s.dpi.is_some(),
            dpi: s.dpi.map_or(0, |d| d.0),
            dpi_max: d.map_or(razer::DPI_MIN, |d| d.dpi_max),
            has_stages: s.stages.is_some(),
            stages: s
                .stages
                .as_ref()
                .map(|st| st.stages.iter().map(|v| v.0).collect())
                .unwrap_or_default(),
            active: s.stages.as_ref().map_or(0, |st| st.active as usize),
            poll_rates: if s.poll.is_some() {
                app.poll_rates().to_vec()
            } else {
                Vec::new()
            },
            poll: s.poll.and_then(|p| app.poll_rates().iter().position(|&r| r == p)),
            power: app.power_mode().and_then(|p| Mode::ALL.iter().position(|&m| m == p)),
            has_idle: s.idle.is_some(),
            idle: s.idle.and_then(|v| IDLE_CHOICES.iter().position(|&c| c == v)),
            free_spin: s.free_spin,
            scroll_accel: s.scroll_accel,
            smart_reel: s.smart_reel,
            autostart: autostart_enabled(),
            tray_icon: TrayIcon::ALL
                .iter()
                .position(|&t| t == app.config.tray_icon)
                .unwrap_or(0),
            auto_power: app.config.auto_power,
            rules: app.config.rules.clone(),
            dpi_popup: app.config.dpi_popup,
            hotkeys: app.config.hotkeys,
            graph,
            graph_volts: app.liion(),
            estimate,
        }
    }

    /// Tabs with something to show for this mouse.
    fn tabs(&self) -> Vec<Tab> {
        if !self.connected {
            return vec![Tab::App];
        }
        let has_poll = !self.poll_rates.is_empty();
        let mut tabs = vec![Tab::Mouse];
        if self.battery.is_some() || self.battery_choice.is_some() || self.has_lowbat {
            tabs.push(Tab::Battery);
        }
        if has_poll || self.has_idle || !self.scroll_rows().is_empty() {
            tabs.push(Tab::Performance);
        }
        if has_poll {
            tabs.push(Tab::Automatic);
        }
        tabs.push(Tab::App);
        tabs
    }

    /// Scroll wheel toggles this mouse has: (label, on, click, tip).
    fn scroll_rows(&self) -> Vec<(&'static str, bool, Hit, Tip)> {
        [
            ("Free-spin scrolling", self.free_spin, Hit::FreeSpin, Tip::FreeSpin),
            (
                "Scroll acceleration",
                self.scroll_accel,
                Hit::ScrollAccel,
                Tip::ScrollAccel,
            ),
            ("Smart Reel", self.smart_reel, Hit::SmartReel, Tip::SmartReel),
        ]
        .into_iter()
        .filter_map(|(l, s, h, t)| s.map(|on| (l, on, h, t)))
        .collect()
    }

    /// DPI stages a rule can pick (5 until the mouse has been read).
    fn stage_choices(&self) -> usize {
        if self.has_stages {
            self.stages.len()
        } else {
            5
        }
    }

    fn usable(&self) -> bool {
        self.connected && !self.asleep
    }

    /// Values the DPI card edits: the stages, or just the current DPI on mice
    /// without stages.
    fn dpi_values(&self) -> Vec<u16> {
        if self.has_stages {
            self.stages.clone()
        } else if self.has_dpi {
            vec![self.dpi]
        } else {
            Vec::new()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    BatType(usize),
    Step,
    Lowbat(usize),
    Stage(usize),
    Minus(usize),
    Plus(usize),
    Remove(usize),
    AddStage,
    Apply,
    Revert,
    Power(usize),
    Poll(usize),
    Idle(usize),
    FreeSpin,
    ScrollAccel,
    SmartReel,
    TrayIcon(usize),
    Autostart,
    CopySerial,
    /// Hovering an (i) shows its explanation.
    Info(Tip),
    AutoPower,
    RuleMode(usize),
    RuleStage(usize),
    RuleRemove(usize),
    AddRule,
    Tab(Tab),
    /// Index into LINKS.
    Link(usize),
    DpiPopup,
    Hotkeys,
}

/// About section links: (label, URL).
const LINKS: [(&str, &str); 4] = [
    ("GitHub", "https://github.com/bladestech/Raisin"),
    ("Releases", "https://github.com/bladestech/Raisin/releases"),
    (
        "Report an issue",
        "https://github.com/bladestech/Raisin/issues/new/choose",
    ),
    ("OpenRazer", "https://github.com/openrazer/openrazer"),
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    /// DPI, mouse details.
    Mouse,
    /// History graph, estimate, battery type, warning level.
    Battery,
    /// Power mode, polling rate, sleep timer, scroll wheel.
    Performance,
    /// Per-app rules, Power Saver on battery.
    Automatic,
    /// Tray icon, start with Windows.
    App,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Mouse => "Mouse",
            Tab::Battery => "Battery",
            Tab::Performance => "Performance",
            Tab::Automatic => "Automatic",
            Tab::App => "App",
        }
    }
}

/// Explanations behind the (i) icons.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tip {
    BatteryType,
    WarnAt,
    LiionStep,
    Stages,
    SingleDpi,
    FreeSpin,
    ScrollAccel,
    SmartReel,
    Power,
    Poll,
    Sleep,
    TrayIcon,
    Auto,
    AutoPower,
    DpiPopup,
    Hotkeys,
}

impl Tip {
    fn text(self) -> &'static str {
        match self {
            Tip::BatteryType => {
                "Tells the mouse which discharge curve to read its battery level with. \
                 Li-ion is for regulated 1.5 V rechargeable AAs: they hold their voltage until \
                 nearly empty, so the percentage can't be trusted and Raisin shows High or Low instead."
            }
            Tip::WarnAt => {
                "The battery level where the mouse starts blinking red, with a Windows \
                 notification from Raisin. The mouse only accepts 5 to 30%."
            }
            Tip::LiionStep => {
                "Turn this on if your cells drop their voltage on purpose when nearly empty \
                 (many smart Li-ion AAs do). The warning moves to 30%, so the drop sets it off."
            }
            Tip::Stages => {
                "The sensitivity steps the DPI button under the wheel cycles through. \
                 Click a stage to switch to it. Use + and - or scroll over a value to change it, \
                 then press Apply."
            }
            Tip::SingleDpi => {
                "Mouse sensitivity in dots per inch: higher moves the pointer further. \
                 Use + and - or scroll over the value, then press Apply."
            }
            Tip::FreeSpin => "Unlocks the wheel so it spins freely, for flying through long pages.",
            Tip::ScrollAccel => "Scrolls further the faster you spin the wheel.",
            Tip::SmartReel => "Switches between tactile and free-spin by itself, depending on how fast you scroll.",
            Tip::Power => {
                "Power Saver: the lowest polling rate and a 1-minute sleep timer, for the \
                 longest battery life.\nBalanced: 1000 Hz and a 5-minute sleep timer.\n\
                 Max: the fastest polling rate this mouse supports and a 5-minute sleep timer."
            }
            Tip::Poll => {
                "How many times a second the mouse reports to the PC. Higher feels smoother \
                 but uses more battery and CPU. Above 1000 Hz mostly helps on high refresh \
                 rate displays."
            }
            Tip::Sleep => {
                "How long the mouse waits without being used before it sleeps to save power. \
                 Move it or click to wake it."
            }
            Tip::TrayIcon => {
                "What the tray icon shows: the battery gauge, the Raisin logo, or the two \
                 taking turns. The gauge always shows when the battery is low."
            }
            Tip::Auto => {
                "Changes the power mode or DPI stage while an app is in front, like Max and \
                 a higher DPI stage in a game. Your settings come back when you switch to \
                 something else or quit Raisin. The tray app does the switching, so keep it running."
            }
            Tip::AutoPower => {
                "Switches to Power Saver while your PC runs on battery or energy saver is on, \
                 and back afterwards. An app with its own power mode still gets it."
            }
            Tip::DpiPopup => {
                "Shows the new DPI and stage near the bottom of the screen when you press the \
                 DPI button. The mouse reports the press itself, so this costs no battery."
            }
            Tip::Hotkeys => {
                "Ctrl+Alt+= and Ctrl+Alt+- step through the DPI stages.\n\
                 Ctrl+Alt+P cycles Power Saver, Balanced and Max.\n\
                 They work in any app while the tray app runs."
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.x,
            top: self.y,
            right: self.x + self.w,
            bottom: self.y + self.h,
        }
    }
}

// ---------- theme ----------

#[derive(Clone, Copy)]
struct C(f32, f32, f32, f32);

impl C {
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: self.0,
            g: self.1,
            b: self.2,
            a: self.3,
        }
    }
    fn alpha(self, a: f32) -> C {
        C(self.0, self.1, self.2, self.3 * a)
    }
    fn mix(self, o: C, t: f32) -> C {
        C(
            self.0 + (o.0 - self.0) * t,
            self.1 + (o.1 - self.1) * t,
            self.2 + (o.2 - self.2) * t,
            self.3 + (o.3 - self.3) * t,
        )
    }
}

struct Theme {
    text: C,
    text2: C,
    text3: C,
    card: C,
    card_stroke: C,
    ctrl: C,
    ctrl_hover: C,
    ctrl_stroke: C,
    accent: C,
    on_accent: C,
    /// Solid background when acrylic isn't available (Windows 10).
    fallback_bg: C,
    /// Info bubble.
    tip_bg: C,
}

fn reg_dword(path: &str, name: &str) -> Option<u32> {
    unsafe {
        let mut v: u32 = 0;
        let mut sz = 4u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            w(path).as_ptr(),
            w(name).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut v as *mut u32 as *mut _,
            &mut sz,
        );
        (r == 0).then_some(v)
    }
}

pub(super) fn system_dark() -> bool {
    reg_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    ) == Some(0)
}

fn system_accent() -> C {
    // AccentColor is 0xAABBGGRR.
    match reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor") {
        Some(v) => C(
            (v & 0xFF) as f32 / 255.0,
            ((v >> 8) & 0xFF) as f32 / 255.0,
            ((v >> 16) & 0xFF) as f32 / 255.0,
            1.0,
        ),
        None => C(0.0, 0.47, 0.83, 1.0),
    }
}

/// The theme's accent as a GDI color (for the tray's DPI popup).
pub(super) fn accent_rgb(dark: bool) -> u32 {
    let a = theme(dark).accent;
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    c(a.0) | c(a.1) << 8 | c(a.2) << 16
}

fn theme(dark: bool) -> Theme {
    let accent = system_accent();
    if dark {
        Theme {
            text: C(1.0, 1.0, 1.0, 1.0),
            text2: C(1.0, 1.0, 1.0, 0.64),
            text3: C(1.0, 1.0, 1.0, 0.32),
            card: C(1.0, 1.0, 1.0, 0.05),
            card_stroke: C(1.0, 1.0, 1.0, 0.07),
            ctrl: C(1.0, 1.0, 1.0, 0.06),
            ctrl_hover: C(1.0, 1.0, 1.0, 0.11),
            ctrl_stroke: C(1.0, 1.0, 1.0, 0.09),
            // Fluent uses a lighter accent on dark surfaces.
            accent: accent.mix(C(1.0, 1.0, 1.0, 1.0), 0.35),
            on_accent: C(0.0, 0.0, 0.0, 0.9),
            fallback_bg: C(0.13, 0.13, 0.13, 1.0),
            tip_bg: C(0.17, 0.17, 0.17, 0.98),
        }
    } else {
        Theme {
            text: C(0.1, 0.1, 0.1, 1.0),
            text2: C(0.0, 0.0, 0.0, 0.6),
            text3: C(0.0, 0.0, 0.0, 0.32),
            card: C(1.0, 1.0, 1.0, 0.55),
            card_stroke: C(0.0, 0.0, 0.0, 0.06),
            ctrl: C(1.0, 1.0, 1.0, 0.7),
            ctrl_hover: C(0.0, 0.0, 0.0, 0.05),
            ctrl_stroke: C(0.0, 0.0, 0.0, 0.08),
            accent: accent.mix(C(0.0, 0.0, 0.0, 1.0), 0.1),
            on_accent: C(1.0, 1.0, 1.0, 1.0),
            fallback_bg: C(0.95, 0.95, 0.95, 1.0),
            tip_bg: C(0.99, 0.99, 0.99, 0.98),
        }
    }
}

fn level_color(pct: u8) -> C {
    if pct <= 10 {
        C(0.90, 0.25, 0.22, 1.0)
    } else if pct <= 25 {
        C(0.98, 0.62, 0.10, 1.0)
    } else {
        C(0.30, 0.72, 0.35, 1.0)
    }
}

fn fmt_volts(mv: u16) -> String {
    format!("≈{}.{:02} V", mv / 1000, mv % 1000 / 10)
}

// ---------- graphics ----------

struct Fonts {
    big: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
    center: IDWriteTextFormat,
    value: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    info: IDWriteTextFormat,
    /// Wrapping, top-aligned text for info bubbles.
    tip: IDWriteTextFormat,
}

struct Gfx {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    dw: IDWriteFactory,
    fonts: Fonts,
    logo: Option<ID2D1Bitmap>,
}

fn text_format(
    dw: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
) -> windows::core::Result<IDWriteTextFormat> {
    unsafe {
        let f = dw.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            wstr!("en-us"),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        f.SetTextAlignment(align)?;
        Ok(f)
    }
}

thread_local! {
    /// Created once per settings process.
    static FACTORIES: RefCell<Option<(ID2D1Factory, IDWriteFactory)>> = const { RefCell::new(None) };
}

fn factories() -> windows::core::Result<(ID2D1Factory, IDWriteFactory)> {
    FACTORIES.with(|f| {
        let mut f = f.borrow_mut();
        if f.is_none() {
            unsafe {
                let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                *f = Some((d2d, dw));
            }
        }
        Ok(f.clone().unwrap())
    })
}

impl Gfx {
    fn new(hwnd: HWND, dpi: u32) -> windows::core::Result<Gfx> {
        unsafe {
            let (factory, dw) = factories()?;
            let mut rc: RECT = zeroed();
            GetClientRect(hwnd, &mut rc);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                // Software rendering: a small window that only redraws on
                // change doesn't need the GPU, and the GPU path loads ~25 MB of
                // driver that stays resident after the window closes.
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: WHWND(hwnd),
                pixelSize: D2D_SIZE_U {
                    width: rc.right as u32,
                    height: rc.bottom as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = factory.CreateHwndRenderTarget(&props, &hprops)?;
            // ClearType needs an opaque background; grayscale works on glass.
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&C(1.0, 1.0, 1.0, 1.0).d2d(), None)?;

            let text = wstr!("Segoe UI Variable Text");
            let display = wstr!("Segoe UI Variable Display");
            let (lead, mid) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER);
            let fonts = Fonts {
                big: text_format(&dw, display, 30.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, lead)?,
                body: text_format(&dw, text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?,
                body_bold: text_format(&dw, text, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, lead)?,
                small: text_format(&dw, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?,
                center: text_format(&dw, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, mid)?,
                value: text_format(&dw, text, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, mid)?,
                glyph: text_format(&dw, wstr!("Segoe Fluent Icons"), 10.0, DWRITE_FONT_WEIGHT_NORMAL, mid)?,
                info: text_format(&dw, wstr!("Segoe Fluent Icons"), 12.0, DWRITE_FONT_WEIGHT_NORMAL, mid)?,
                tip: {
                    let f = text_format(&dw, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, lead)?;
                    f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                    f
                },
            };
            let logo = rt
                .CreateBitmap(
                    D2D_SIZE_U {
                        width: LOGO_PX,
                        height: LOGO_PX,
                    },
                    Some(LOGO_BGRA.as_ptr() as *const _),
                    LOGO_PX * 4,
                    &D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    },
                )
                .ok();
            Ok(Gfx {
                rt,
                brush,
                dw,
                fonts,
                logo,
            })
        }
    }

    fn color(&self, c: C) {
        unsafe { self.brush.SetColor(&c.d2d()) };
    }

    fn fill(&self, r: Rect, radius: f32, c: C) {
        self.color(c);
        let rr = D2D1_ROUNDED_RECT {
            rect: r.d2d(),
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.rt.FillRoundedRectangle(&rr, &self.brush) };
    }

    fn stroke(&self, r: Rect, radius: f32, width: f32, c: C) {
        self.color(c);
        // Align 1-px strokes to the pixel grid.
        let h = width / 2.0;
        let rr = D2D1_ROUNDED_RECT {
            rect: Rect::new(r.x + h, r.y + h, r.w - width, r.h - width).d2d(),
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.rt.DrawRoundedRectangle(&rr, &self.brush, width, None) };
    }

    fn line(&self, a: (f32, f32), b: (f32, f32), width: f32, c: C) {
        self.color(c);
        let p = |(x, y): (f32, f32)| Vector2 { X: x, Y: y };
        unsafe { self.rt.DrawLine(p(a), p(b), &self.brush, width, None) };
    }

    fn text(&self, s: &str, r: Rect, f: &IDWriteTextFormat, c: C) {
        self.color(c);
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.rt.DrawText(
                &u,
                f,
                &r.d2d(),
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// Width of `s` in DIPs.
    fn measure(&self, s: &str, f: &IDWriteTextFormat) -> f32 {
        self.metrics(s, f, 1000.0)
            .map_or(0.0, |m| m.widthIncludingTrailingWhitespace)
    }

    /// Height of `s` wrapped to `width` DIPs.
    fn wrapped_height(&self, s: &str, f: &IDWriteTextFormat, width: f32) -> f32 {
        self.metrics(s, f, width).map_or(0.0, |m| m.height)
    }

    fn metrics(&self, s: &str, f: &IDWriteTextFormat, width: f32) -> Option<DWRITE_TEXT_METRICS> {
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let layout = self.dw.CreateTextLayout(&u, f, width, 1000.0).ok()?;
            let mut m: DWRITE_TEXT_METRICS = zeroed();
            layout.GetMetrics(&mut m).ok()?;
            Some(m)
        }
    }
}

// ---------- window state ----------

struct Ui {
    hwnd: HWND,
    view: View,
    /// DPI values being edited but not yet written to the mouse.
    pending: Option<Vec<u16>>,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tracking: bool,
    gfx: Option<Gfx>,
    dpi: u32,
    dark: bool,
    acrylic: bool,
    /// Content height from the last paint, in DIPs.
    content_h: f32,
    /// Visible client height, in DIPs. Smaller than `content_h` when the
    /// window is capped to the screen; the content then scrolls.
    view_h: f32,
    /// Scroll offset, in DIPs.
    scroll: f32,
    tab: Tab,
    /// "Copied" feedback for the serial number.
    copied_until: Option<Instant>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.try_borrow_mut().ok()?.as_mut().map(f))
}

fn window() -> HWND {
    with_ui(|u| u.hwnd).unwrap_or(null_mut())
}

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let class = w(CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            // Black = fully transparent once the frame is extended.
            hbrBackground: GetStockObject(BLACK_BRUSH) as HBRUSH,
            ..zeroed()
        };
        RegisterClassW(&wc);
    }
}

pub fn open(app: &App) {
    let existing = window();
    if !existing.is_null() {
        sync(app, true);
        unsafe {
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
        }
        return;
    }
    unsafe {
        let Some(hwnd) = create() else { return };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let dark = system_dark();
        let acrylic = apply_backdrop(hwnd, dark);
        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                hwnd,
                view: View::from_app(app),
                pending: None,
                hits: Vec::new(),
                hover: None,
                pressed: None,
                tracking: false,
                gfx: None,
                dpi,
                dark,
                acrylic,
                content_h: INITIAL_HEIGHT,
                view_h: INITIAL_HEIGHT,
                scroll: 0.0,
                tab: Tab::Mouse,
                copied_until: None,
            })
        });
        fit_window(hwnd, dpi, INITIAL_HEIGHT, true);
        SetTimer(hwnd, TIMER_LIVE, LIVE_MS, None);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        UpdateWindow(hwnd); // paint now, before the caller's blocking mouse read
    }
}

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

unsafe fn create() -> Option<HWND> {
    let hinst = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null());
    let hwnd = CreateWindowExW(
        0,
        w(CLASS).as_ptr(),
        w("Raisin").as_ptr(),
        STYLE,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        100,
        100,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if hwnd.is_null() {
        return None;
    }
    // App icon from the exe's resources (assets/raisin.ico).
    for (which, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
        let px = GetSystemMetrics(metric);
        let icon = LoadImageW(hinst, 1 as SysPcwstr, IMAGE_ICON, px, px, LR_DEFAULTCOLOR);
        SendMessageW(hwnd, WM_SETICON, which as usize, icon as isize);
    }
    let _ = GetDpiForSystem();
    Some(hwnd)
}

fn outer_size(dpi: u32, height: f32) -> (i32, i32) {
    let s = |v: f32| (v * dpi as f32 / 96.0).round() as i32;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: s(WIDTH),
        bottom: s(height),
    };
    unsafe { AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi) };
    (rc.right - rc.left, rc.bottom - rc.top)
}

/// Size the window for `height` DIPs of content, capped to the screen it's
/// on (the content scrolls then). `center` places it in the middle of the
/// work area; otherwise it keeps its vertical center.
unsafe fn fit_window(hwnd: HWND, dpi: u32, height: f32, center: bool) {
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
    let work = mi.rcWork;
    let (ww, mut wh) = outer_size(dpi, height);
    let max_h = work.bottom - work.top;
    if wh > max_h {
        // Shrink the client area by the overflow, keeping the frame.
        let over = (wh - max_h) as f32 * 96.0 / dpi as f32;
        wh = outer_size(dpi, (height - over).max(200.0)).1;
    }
    let mut cur: RECT = zeroed();
    GetWindowRect(hwnd, &mut cur);
    let (cx, cy) = if center {
        ((work.left + work.right) / 2, (work.top + work.bottom) / 2)
    } else {
        ((cur.left + cur.right) / 2, (cur.top + cur.bottom) / 2)
    };
    let x = (cx - ww / 2).max(work.left);
    let y = (cy - wh / 2).clamp(work.top, (work.bottom - wh).max(work.top));
    SetWindowPos(hwnd, null_mut(), x, y, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
}

/// Acrylic backdrop + dark title bar. Returns false if unsupported.
unsafe fn apply_backdrop(hwnd: HWND, dark: bool) -> bool {
    let h = WHWND(hwnd);
    let dark_flag: i32 = dark as i32;
    let _ = DwmSetWindowAttribute(
        h,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark_flag as *const i32 as *const _,
        size_of::<i32>() as u32,
    );
    let backdrop = DWMSBT_TRANSIENTWINDOW; // acrylic
    let ok = DwmSetWindowAttribute(
        h,
        DWMWA_SYSTEMBACKDROP_TYPE,
        &backdrop as *const DWM_SYSTEMBACKDROP_TYPE as *const _,
        size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    )
    .is_ok();
    if ok {
        let m = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(h, &m);
    }
    ok
}

/// Push app state into the window. `reset_edits` drops unapplied DPI edits.
pub fn sync(app: &App, reset_edits: bool) {
    let view = View::from_app(app);
    with_ui(|u| {
        if reset_edits {
            u.pending = None;
        }
        if u.view != view || reset_edits {
            u.view = view;
            unsafe { InvalidateRect(u.hwnd, null(), 0) };
        }
    });
}

fn copy_to_clipboard(hwnd: HWND, text: &str) -> bool {
    let u: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return false;
        }
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, u.len() * 2);
        let mut ok = false;
        if !mem.is_null() {
            let p = GlobalLock(mem) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(u.as_ptr(), p, u.len());
                GlobalUnlock(mem);
                ok = !SetClipboardData(CF_UNICODETEXT, mem as HANDLE).is_null();
            }
        }
        CloseClipboard();
        ok
    }
}

// ---------- painting ----------

fn step_size(v: u16) -> u16 {
    if v < 1000 {
        50
    } else if v < 5000 {
        100
    } else {
        500
    }
}

fn bump(v: u16, up: bool, max: u16) -> u16 {
    let n = if up {
        v.saturating_add(step_size(v))
    } else {
        v.saturating_sub(step_size(v.saturating_sub(1)))
    };
    n.clamp(razer::DPI_MIN, max.max(razer::DPI_MIN))
}

/// Top-left of a `w`×`h` info bubble for an (i) at `anchor`: centered under
/// it, kept inside the visible part of the content (`top`..`bottom`), and
/// flipped above when it won't fit below.
fn bubble_pos(anchor: Rect, w: f32, h: f32, top: f32, bottom: f32) -> (f32, f32) {
    let x = (anchor.x + anchor.w / 2.0 - w / 2.0).clamp(PAD / 2.0, WIDTH - PAD / 2.0 - w);
    let below = anchor.y + anchor.h + 4.0;
    let y = if below + h <= bottom - 4.0 {
        below
    } else {
        (anchor.y - 4.0 - h).max(top + 4.0)
    };
    (x, y)
}

/// Scroll offset kept within the content.
fn clamp_scroll(scroll: f32, content_h: f32, view_h: f32) -> f32 {
    scroll.clamp(0.0, (content_h - view_h).max(0.0))
}

/// Short labels when many rates must fit in the segmented control.
fn poll_label(hz: u16, many: bool) -> String {
    match (many, hz) {
        (true, h) if h >= 1000 => format!("{}K", h / 1000),
        (true, h) => h.to_string(),
        (false, h) => format!("{h} Hz"),
    }
}

impl Ui {
    fn dpi_values(&self) -> Vec<u16> {
        self.pending.clone().unwrap_or_else(|| self.view.dpi_values())
    }

    fn paint(&mut self) {
        if self.gfx.is_none() {
            self.gfx = Gfx::new(self.hwnd, self.dpi).ok();
        }
        let Some(g) = self.gfx.take() else { return };
        let t = theme(self.dark);
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        unsafe {
            g.rt.BeginDraw();
            let bg = if self.acrylic {
                C(0.0, 0.0, 0.0, 0.0)
            } else {
                t.fallback_bg
            };
            g.rt.Clear(Some(&bg.d2d()));
            g.rt.SetTransform(&Matrix3x2::translation(0.0, -self.scroll));
        }
        let h = self.draw(&g, &t, &mut hits);
        unsafe { g.rt.SetTransform(&Matrix3x2::identity()) };
        self.scrollbar(&g, &t, h);
        let ok = unsafe { g.rt.EndDraw(None, None) }.is_ok();
        self.hits = hits;
        // On device loss, rebuild everything on the next paint.
        if ok {
            self.gfx = Some(g);
        }
        if (h - self.content_h).abs() > 0.5 {
            self.content_h = h;
            self.scroll = clamp_scroll(self.scroll, h, self.view_h);
            // Resize outside this paint (WM_SIZE needs the UI state).
            unsafe { PostMessageW(self.hwnd, WM_FIT, 0, 0) };
        }
    }

    fn scrollable(&self) -> bool {
        self.content_h > self.view_h + 0.5
    }

    /// Thin overlay bar on the right while the content is taller than the
    /// window.
    fn scrollbar(&self, g: &Gfx, t: &Theme, content_h: f32) {
        if content_h <= self.view_h + 0.5 {
            return;
        }
        let track = self.view_h - 8.0;
        let len = (track * self.view_h / content_h).max(24.0);
        let pos = 4.0 + (track - len) * self.scroll / (content_h - self.view_h);
        g.fill(Rect::new(WIDTH - 6.0, pos, 3.0, len), 1.5, t.text3);
    }

    /// Tab shown now: the picked one, or the first one this mouse has.
    fn current_tab(&self, tabs: &[Tab]) -> Tab {
        if tabs.contains(&self.tab) {
            self.tab
        } else {
            tabs.first().copied().unwrap_or(Tab::App)
        }
    }

    /// Draw everything; returns the content height.
    fn draw(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>) -> f32 {
        self.header(g, t);
        let v = &self.view;
        if v.loading {
            return HEADER_H;
        }
        let tabs = v.tabs();
        let cur = self.current_tab(&tabs);
        let top = if tabs.len() > 1 {
            self.tab_bar(g, t, hits, &tabs, cur);
            HEADER_H + TAB_H + CARD_GAP
        } else {
            HEADER_H
        };

        // The window is as tall as the tallest tab, so switching tabs doesn't
        // resize it. Measure the others with their drawing clipped away.
        let mut bottom = top;
        for &tab in tabs.iter().filter(|&&tab| tab != cur) {
            let mut scratch = Vec::new();
            unsafe {
                g.rt.PushAxisAlignedClip(&Rect::new(0.0, 0.0, 0.0, 0.0).d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            }
            bottom = bottom.max(self.draw_tab(g, t, &mut scratch, tab, top));
            unsafe { g.rt.PopAxisAlignedClip() };
        }
        bottom = bottom.max(self.draw_tab(g, t, hits, cur, top));
        let height = bottom + PAD - CARD_GAP;

        // Info bubble on top of everything.
        if let Some(Hit::Info(tip)) = self.hover {
            if let Some(&(anchor, _)) = hits.iter().find(|(_, h)| *h == Hit::Info(tip)) {
                let bottom = self.scroll + self.view_h.min(height);
                self.bubble(g, t, anchor, tip.text(), self.scroll, bottom);
            }
        }
        height
    }

    /// Tabs under the header, as a segmented strip.
    fn tab_bar(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, tabs: &[Tab], cur: Tab) {
        let r = Rect::new(PAD, HEADER_H, CARD_W, TAB_H);
        g.fill(r, 8.0, t.ctrl);
        g.stroke(r, 8.0, 1.0, t.ctrl_stroke);
        let w = r.w / tabs.len() as f32;
        for (i, &tab) in tabs.iter().enumerate() {
            let s = Rect::new(r.x + i as f32 * w, r.y, w, r.h);
            let hit = Hit::Tab(tab);
            if tab == cur {
                g.fill(s.inset(3.0), 6.0, t.card.mix(t.text, 0.06));
                g.fill(
                    Rect::new(s.x + s.w / 2.0 - 10.0, s.y + s.h - 6.0, 20.0, 3.0),
                    1.5,
                    t.accent,
                );
            } else if self.hover == Some(hit) {
                g.fill(s.inset(3.0), 6.0, t.ctrl_hover);
            }
            let fg = if tab == cur { t.text } else { t.text2 };
            g.text(tab.label(), Rect::new(s.x, s.y - 1.0, s.w, s.h), &g.fonts.center, fg);
            if tab != cur {
                hits.push((s, hit));
            }
        }
    }

    /// Draw one tab's cards from `y`; returns the y below the last card.
    fn draw_tab(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, tab: Tab, mut y: f32) -> f32 {
        let v = &self.view;
        let usable = v.usable();
        let hover = self.hover;
        match tab {
            Tab::Mouse => {
                if v.has_stages || v.has_dpi {
                    y = self.dpi_card(g, t, hits, y) + CARD_GAP;
                }
                y = self.about(g, t, hits, y);
            }
            Tab::Battery => {
                if v.battery.is_some() {
                    y = self.history_card(g, t, y) + CARD_GAP;
                }
                // Battery settings (AA mice and those with a warning threshold)
                if v.battery_choice.is_some() || v.has_lowbat {
                    let rows = v.battery_choice.is_some() as usize * 2 + v.has_lowbat as usize;
                    let card = self.card(g, t, y, rows);
                    let mut ry = card.y + CARD_PAD;
                    if v.battery_choice.is_some() {
                        self.label_info(g, t, hits, "Battery type", ry, usable, Tip::BatteryType);
                        self.segmented(
                            g,
                            t,
                            hits,
                            ry,
                            &BATTERY_LABELS,
                            v.battery_choice,
                            usable,
                            Hit::BatType,
                            hover,
                        );
                        ry += ROW_H + ROW_GAP;
                    }
                    if v.has_lowbat {
                        self.label_info(g, t, hits, "Warn at", ry, usable, Tip::WarnAt);
                        self.segmented(g, t, hits, ry, &LOWBAT_LABELS, v.lowbat, usable, Hit::Lowbat, hover);
                        ry += ROW_H + ROW_GAP;
                    }
                    if v.battery_choice.is_some() {
                        let liion = v.battery_choice == Some(3);
                        let text = "Li-ion cells drop voltage when low";
                        self.info_after(g, t, hits, text, &g.fonts.body, ry, Tip::LiionStep);
                        self.toggle_row(g, t, hits, ry, text, v.liion_step, usable && liion, Hit::Step, hover);
                    }
                    y = card.y + card.h + CARD_GAP;
                }
            }
            Tab::Performance => {
                let has_poll = !v.poll_rates.is_empty();
                let rows = has_poll as usize * 2 + v.has_idle as usize;
                if rows > 0 {
                    let card = self.card(g, t, y, rows);
                    let mut ry = card.y + CARD_PAD;
                    if has_poll {
                        self.label_info(g, t, hits, "Power mode", ry, usable, Tip::Power);
                        let labels = Mode::ALL.map(Mode::label);
                        self.segmented(g, t, hits, ry, &labels, v.power, usable, Hit::Power, hover);
                        ry += ROW_H + ROW_GAP;
                        self.label_info(g, t, hits, "Polling rate", ry, usable, Tip::Poll);
                        let many = v.poll_rates.len() > 4;
                        let labels: Vec<String> = v.poll_rates.iter().map(|&hz| poll_label(hz, many)).collect();
                        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                        self.segmented(g, t, hits, ry, &refs, v.poll, usable, Hit::Poll, hover);
                        ry += ROW_H + ROW_GAP;
                    }
                    if v.has_idle {
                        self.label_info(g, t, hits, "Sleep after", ry, usable, Tip::Sleep);
                        self.segmented(g, t, hits, ry, &IDLE_LABELS, v.idle, usable, Hit::Idle, hover);
                    }
                    y = card.y + card.h + CARD_GAP;
                }
                let scroll = v.scroll_rows();
                if !scroll.is_empty() {
                    let card = self.card(g, t, y, scroll.len());
                    let mut ry = card.y + CARD_PAD;
                    for (text, on, hit, tip) in scroll {
                        self.info_after(g, t, hits, text, &g.fonts.body, ry, tip);
                        self.toggle_row(g, t, hits, ry, text, on, usable, hit, hover);
                        ry += ROW_H + ROW_GAP;
                    }
                    y = card.y + card.h + CARD_GAP;
                }
            }
            Tab::Automatic => {
                y = self.auto_card(g, t, hits, y) + CARD_GAP;
            }
            Tab::App => {
                // Not mouse settings: always enabled.
                let card = self.card(g, t, y, 4);
                let mut ry = card.y + CARD_PAD;
                self.label_info(g, t, hits, "Tray icon", ry, true, Tip::TrayIcon);
                let tray_labels = TrayIcon::ALL.map(|t| t.label());
                self.segmented(
                    g,
                    t,
                    hits,
                    ry,
                    &tray_labels,
                    Some(v.tray_icon),
                    true,
                    Hit::TrayIcon,
                    hover,
                );
                ry += ROW_H + ROW_GAP;
                self.toggle_row(
                    g,
                    t,
                    hits,
                    ry,
                    "Start Raisin with Windows",
                    v.autostart,
                    true,
                    Hit::Autostart,
                    hover,
                );
                ry += ROW_H + ROW_GAP;
                let text = "Show DPI changes on screen";
                self.info_after(g, t, hits, text, &g.fonts.body, ry, Tip::DpiPopup);
                self.toggle_row(g, t, hits, ry, text, v.dpi_popup, true, Hit::DpiPopup, hover);
                ry += ROW_H + ROW_GAP;
                let text = "Keyboard shortcuts";
                self.info_after(g, t, hits, text, &g.fonts.body, ry, Tip::Hotkeys);
                self.toggle_row(g, t, hits, ry, text, v.hotkeys, true, Hit::Hotkeys, hover);
                y = card.y + card.h + CARD_GAP;
                y = self.about_app(g, t, hits, y);
            }
        }
        y
    }

    /// Row label followed by an (i).
    #[allow(clippy::too_many_arguments)]
    fn label_info(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, s: &str, y: f32, enabled: bool, tip: Tip) {
        self.label(g, t, s, y, enabled);
        self.info_after(g, t, hits, s, &g.fonts.body, y, tip);
    }

    /// An (i) just after `s` drawn at the row's text position. Its hit goes
    /// in first, so it wins over a toggle row underneath.
    #[allow(clippy::too_many_arguments)]
    fn info_after(
        &self,
        g: &Gfx,
        t: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
        s: &str,
        f: &IDWriteTextFormat,
        y: f32,
        tip: Tip,
    ) {
        let x = PAD + 16.0 + g.measure(s, f) + 3.0;
        self.info(g, t, hits, x, y + (ROW_H - 20.0) / 2.0, tip);
    }

    fn info(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, x: f32, y: f32, tip: Tip) {
        let r = Rect::new(x, y, 20.0, 20.0);
        let hovered = self.hover == Some(Hit::Info(tip));
        g.text("\u{E946}", r, &g.fonts.info, if hovered { t.accent } else { t.text3 });
        hits.push((r, Hit::Info(tip)));
    }

    /// Info bubble under (or, near the bottom, over) `anchor`.
    #[allow(clippy::too_many_arguments)]
    fn bubble(&self, g: &Gfx, t: &Theme, anchor: Rect, text: &str, top: f32, bottom: f32) {
        // Non-breaking hyphens: keep "Li-ion" and "1-minute" on one line.
        let text = &text.replace('-', "\u{2011}");
        let text_h = g.wrapped_height(text, &g.fonts.tip, TIP_W - 2.0 * TIP_PAD);
        let h = text_h + 2.0 * TIP_PAD;
        let (x, y) = bubble_pos(anchor, TIP_W, h, top, bottom);
        let r = Rect::new(x, y, TIP_W, h);
        // Soft shadow, then the bubble.
        g.fill(
            Rect::new(r.x - 1.0, r.y + 1.0, r.w + 2.0, r.h + 3.0),
            9.0,
            C(0.0, 0.0, 0.0, 0.18),
        );
        g.fill(r, 8.0, t.tip_bg);
        g.stroke(r, 8.0, 1.0, t.ctrl_stroke);
        g.text(text, r.inset(TIP_PAD), &g.fonts.tip, t.text);
    }

    fn header(&self, g: &Gfx, t: &Theme) {
        let v = &self.view;
        let usable = v.usable();
        let tx = PAD + 62.0;
        let has_battery = v.battery.is_some();

        // Battery glyph (or a mouse-free spot for wired mice).
        if has_battery || v.loading || !v.connected {
            let gy = 30.0;
            let body = Rect::new(PAD + 2.0, gy, 40.0, 22.0);
            let fg = if usable { t.text } else { t.text3 };
            g.stroke(body, 5.0, 1.5, fg);
            g.fill(Rect::new(PAD + 42.5, gy + 7.0, 3.0, 8.0), 1.5, fg);
            if let (true, Some(b)) = (usable, v.battery) {
                let level = if v.liion {
                    if v.liion_high {
                        100
                    } else {
                        5
                    }
                } else {
                    b
                };
                let inner = body.inset(3.5);
                let w = (inner.w * level.min(100) as f32 / 100.0).max(3.0);
                g.fill(Rect::new(inner.x, inner.y, w, inner.h), 2.5, level_color(level));
            }
        }
        let tx = if has_battery || v.loading || !v.connected {
            tx
        } else {
            PAD + 2.0
        };

        let mut sub = v.name.clone();
        let big = if v.loading {
            sub = "Reading mouse…".into();
            "Raisin".to_string()
        } else if !v.connected {
            sub = "Plug in a supported Razer mouse".into();
            "Not connected".to_string()
        } else if v.asleep {
            sub = format!("{} · move it to wake", v.name);
            "Asleep".to_string()
        } else if let Some(b) = v.battery {
            if v.liion {
                if v.liion_high { "High" } else { "Low" }.to_string()
            } else {
                let _ = b;
                format!("{b}%")
            }
        } else if v.has_dpi {
            format!("{} DPI", v.dpi)
        } else {
            v.name.clone()
        };
        if usable {
            if let Some(mv) = v.voltage_mv.filter(|_| has_battery) {
                sub += &format!("  ·  {}", fmt_volts(mv));
            }
            if v.charging {
                sub += "  ·  Charging";
            }
        }
        let big_color = if usable && v.liion && !v.liion_high {
            level_color(5)
        } else {
            t.text
        };
        g.text(&big, Rect::new(tx, 14.0, 280.0, 40.0), &g.fonts.big, big_color);

        // Li-ion: the percentage stays visible but crossed out, since the
        // mouse can't measure a regulated cell's charge.
        if usable && v.liion {
            if let Some(b) = v.battery {
                let x = tx + g.measure(&big, &g.fonts.big) + 10.0;
                let pct = format!("{b}%");
                let w = g.measure(&pct, &g.fonts.body);
                g.text(&pct, Rect::new(x, 17.0, w + 4.0, 40.0), &g.fonts.body, t.text3);
                g.fill(Rect::new(x - 1.0, 36.5, w + 2.0, 1.2), 0.6, t.text3);
            }
        }
        g.text(&sub, Rect::new(tx + 1.0, 52.0, 300.0, 20.0), &g.fonts.small, t.text2);

        if let Some(logo) = &g.logo {
            let r = Rect::new(WIDTH - PAD - 44.0, 20.0, 44.0, 44.0);
            unsafe {
                g.rt.DrawBitmap(logo, Some(&r.d2d()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
    }

    /// Draw a card sized for `rows` standard rows; returns its rect.
    fn card(&self, g: &Gfx, t: &Theme, y: f32, rows: usize) -> Rect {
        let h = CARD_PAD * 2.0 + rows as f32 * ROW_H + rows.saturating_sub(1) as f32 * ROW_GAP;
        let r = Rect::new(PAD, y, CARD_W, h);
        self.card_rect(g, t, r);
        r
    }

    fn card_rect(&self, g: &Gfx, t: &Theme, r: Rect) {
        g.fill(r, 8.0, t.card);
        g.stroke(r, 8.0, 1.0, t.card_stroke);
    }

    fn label(&self, g: &Gfx, t: &Theme, s: &str, y: f32, enabled: bool) {
        g.text(
            s,
            Rect::new(PAD + 16.0, y, 110.0, ROW_H),
            &g.fonts.body,
            if enabled { t.text } else { t.text3 },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn segmented(
        &self,
        g: &Gfx,
        t: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
        y: f32,
        labels: &[&str],
        selected: Option<usize>,
        enabled: bool,
        hit: fn(usize) -> Hit,
        hover: Option<Hit>,
    ) {
        let r = Rect::new(PAD + CARD_W - 16.0 - CTRL_W, y, CTRL_W, ROW_H);
        g.fill(r, 6.0, t.ctrl);
        g.stroke(r, 6.0, 1.0, t.ctrl_stroke);
        let seg_w = r.w / labels.len().max(1) as f32;
        for (i, l) in labels.iter().enumerate() {
            let s = Rect::new(r.x + i as f32 * seg_w, r.y, seg_w, r.h);
            let sel = selected == Some(i);
            let color = if sel {
                let a = if enabled { t.accent } else { t.text3 };
                g.fill(s.inset(3.0), 4.0, a);
                t.on_accent
            } else {
                if enabled && hover == Some(hit(i)) {
                    g.fill(s.inset(3.0), 4.0, t.ctrl_hover);
                }
                if enabled {
                    t.text
                } else {
                    t.text3
                }
            };
            g.text(l, s, &g.fonts.center, color);
            if enabled && !sel {
                hits.push((s, hit(i)));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn toggle_row(
        &self,
        g: &Gfx,
        t: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
        y: f32,
        text: &str,
        on: bool,
        enabled: bool,
        hit: Hit,
        hover: Option<Hit>,
    ) {
        let row = Rect::new(PAD + 6.0, y, CARD_W - 12.0, ROW_H);
        if enabled && hover == Some(hit) {
            g.fill(row, 6.0, t.ctrl_hover);
        }
        g.text(
            text,
            Rect::new(PAD + 16.0, y, 300.0, ROW_H),
            &g.fonts.body,
            if enabled { t.text } else { t.text3 },
        );
        let tr = Rect::new(PAD + CARD_W - 16.0 - 40.0, y + 5.0, 40.0, 20.0);
        if on {
            g.fill(tr, 10.0, if enabled { t.accent } else { t.text3 });
            g.fill(Rect::new(tr.x + 23.0, tr.y + 3.0, 14.0, 14.0), 7.0, t.on_accent);
        } else {
            g.stroke(tr, 10.0, 1.0, if enabled { t.text2 } else { t.text3 });
            g.fill(
                Rect::new(tr.x + 5.0, tr.y + 5.0, 10.0, 10.0),
                5.0,
                if enabled { t.text2 } else { t.text3 },
            );
        }
        if enabled {
            hits.push((row, hit));
        }
    }

    /// DPI stages, or a single DPI value on mice without stages.
    fn dpi_card(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, y: f32) -> f32 {
        let v = &self.view;
        let usable = v.usable();
        let hover = self.hover;
        let values = self.dpi_values();
        let slots = if v.has_stages { 5 } else { 1 };
        let h = CARD_PAD * 2.0 + ROW_H + 6.0 + slots as f32 * 38.0 - 4.0;
        let card = Rect::new(PAD, y, CARD_W, h);
        self.card_rect(g, t, card);
        let cx = PAD + 16.0;
        let inner_w = CARD_W - 32.0;
        let mut ry = y + CARD_PAD;
        let title = if v.has_stages {
            "DPI stages".to_string()
        } else {
            format!("DPI (max {})", v.dpi_max)
        };
        g.text(
            &title,
            Rect::new(cx, ry, 200.0, ROW_H),
            &g.fonts.body_bold,
            if usable { t.text } else { t.text3 },
        );
        let tip = if v.has_stages { Tip::Stages } else { Tip::SingleDpi };
        self.info_after(g, t, hits, &title, &g.fonts.body_bold, ry, tip);
        let dirty = self.pending.as_ref().is_some_and(|p| *p != v.dpi_values());
        if dirty && usable {
            let apply = Rect::new(cx + inner_w - 76.0, ry + 1.0, 76.0, 28.0);
            let a = if hover == Some(Hit::Apply) {
                t.accent.mix(t.text, 0.12)
            } else {
                t.accent
            };
            g.fill(apply, 6.0, a);
            g.text("Apply", apply, &g.fonts.center, t.on_accent);
            hits.push((apply, Hit::Apply));
            let revert = Rect::new(apply.x - 72.0, ry + 1.0, 66.0, 28.0);
            if hover == Some(Hit::Revert) {
                g.fill(revert, 6.0, t.ctrl_hover);
            }
            g.text("Revert", revert, &g.fonts.center, t.text2);
            hits.push((revert, Hit::Revert));
        }
        ry += ROW_H + 6.0;
        for i in 0..slots {
            let row = Rect::new(cx - 6.0, ry + i as f32 * 38.0, inner_w + 12.0, 34.0);
            if i < values.len() {
                self.stage_row(g, t, hits, row, i, values[i], values.len(), usable, hover);
            } else if i == values.len() && usable && v.has_stages {
                if hover == Some(Hit::AddStage) {
                    g.fill(row, 6.0, t.ctrl_hover);
                }
                let label = Rect::new(row.x + 14.0, row.y, row.w - 14.0, row.h);
                g.text("+  Add stage", label, &g.fonts.body, t.text2);
                hits.push((row, Hit::AddStage));
            }
        }
        y + h
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_row(
        &self,
        g: &Gfx,
        t: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
        row: Rect,
        i: usize,
        value: u16,
        count: usize,
        usable: bool,
        hover: Option<Hit>,
    ) {
        let v = &self.view;
        let stages = v.has_stages;
        let active = stages && v.active == i + 1;
        let row_hover = matches!(hover, Some(Hit::Stage(j) | Hit::Minus(j) | Hit::Plus(j) | Hit::Remove(j)) if j == i);
        if active && usable {
            g.fill(row, 6.0, t.accent.alpha(0.16));
            g.fill(Rect::new(row.x, row.y + 9.0, 3.0, row.h - 18.0), 1.5, t.accent);
        } else if usable && row_hover && stages {
            g.fill(row, 6.0, t.ctrl_hover);
        }
        let fg = if usable { t.text } else { t.text3 };
        if stages {
            // Radio
            let rc = Rect::new(row.x + 14.0, row.y + 9.0, 16.0, 16.0);
            if active {
                g.fill(rc, 8.0, if usable { t.accent } else { t.text3 });
                g.fill(rc.inset(5.0), 3.0, t.on_accent);
            } else {
                g.stroke(rc, 8.0, 1.0, if usable { t.text2 } else { t.text3 });
            }
            g.text(
                &format!("Stage {}", i + 1),
                Rect::new(row.x + 40.0, row.y, 100.0, row.h),
                &g.fonts.body,
                fg,
            );
        } else {
            g.text(
                "Current",
                Rect::new(row.x + 14.0, row.y, 100.0, row.h),
                &g.fonts.body,
                fg,
            );
        }

        // Stepper: [-] value [+]  (x)
        let right = row.x + row.w - 10.0;
        let rm = Rect::new(right - 22.0, row.y + 6.0, 22.0, 22.0);
        let plus = Rect::new(rm.x - 8.0 - 28.0, row.y + 3.0, 28.0, 28.0);
        let val = Rect::new(plus.x - 76.0, row.y, 76.0, row.h);
        let minus = Rect::new(val.x - 28.0, row.y + 3.0, 28.0, 28.0);
        for (r, label, h) in [(minus, "\u{E738}", Hit::Minus(i)), (plus, "\u{E710}", Hit::Plus(i))] {
            let c = if usable && hover == Some(h) {
                t.ctrl_hover.mix(t.ctrl, 0.3)
            } else {
                t.ctrl
            };
            g.fill(r, 6.0, c);
            g.stroke(r, 6.0, 1.0, t.ctrl_stroke);
            g.text(label, r, &g.fonts.glyph, fg);
            if usable {
                hits.push((r, h));
            }
        }
        g.text(&format!("{value}"), val, &g.fonts.value, fg);
        if usable && stages && count > 1 && row_hover {
            if hover == Some(Hit::Remove(i)) {
                g.fill(rm, 11.0, t.ctrl_hover);
            }
            g.text("\u{E711}", rm, &g.fonts.glyph, t.text2);
        }
        if usable && stages {
            if count > 1 {
                hits.push((rm, Hit::Remove(i)));
            }
            // Row last, so the buttons above win the hit test.
            hits.push((row, Hit::Stage(i)));
        }
    }

    /// Per-app rules and the Power Saver on battery toggle.
    fn auto_card(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, y: f32) -> f32 {
        let v = &self.view;
        let hover = self.hover;
        let can_add = v.rules.len() < MAX_RULES;
        let rows = v.rules.len() + can_add as usize;
        let h = CARD_PAD * 2.0 + ROW_H + 6.0 + ROW_H + 8.0 + rows as f32 * 38.0 - 4.0;
        let card = Rect::new(PAD, y, CARD_W, h);
        self.card_rect(g, t, card);
        let cx = PAD + 16.0;
        let inner_w = CARD_W - 32.0;
        let mut ry = y + CARD_PAD;
        let title = "Automatic switching";
        g.text(title, Rect::new(cx, ry, 220.0, ROW_H), &g.fonts.body_bold, t.text);
        self.info_after(g, t, hits, title, &g.fonts.body_bold, ry, Tip::Auto);
        ry += ROW_H + 6.0;

        // Settings, not mouse commands: usable even while the mouse sleeps.
        let text = "Power Saver on battery or energy saver";
        self.info_after(g, t, hits, text, &g.fonts.body, ry, Tip::AutoPower);
        self.toggle_row(g, t, hits, ry, text, v.auto_power, true, Hit::AutoPower, hover);
        ry += ROW_H + 8.0;

        for (i, r) in v.rules.iter().enumerate() {
            let row = Rect::new(cx - 6.0, ry + i as f32 * 38.0, inner_w + 12.0, 34.0);
            let row_hover = matches!(hover, Some(Hit::RuleMode(j) | Hit::RuleStage(j) | Hit::RuleRemove(j)) if j == i);
            if row_hover {
                g.fill(row, 6.0, t.ctrl_hover.alpha(0.6));
            }
            let name = r.exe.strip_suffix(".exe").unwrap_or(&r.exe);
            g.text(
                name,
                Rect::new(row.x + 14.0, row.y, 112.0, row.h),
                &g.fonts.body,
                t.text,
            );

            let right = row.x + row.w - 10.0;
            let rm = Rect::new(right - 22.0, row.y + 6.0, 22.0, 22.0);
            let stage = Rect::new(rm.x - 8.0 - 84.0, row.y + 3.0, 84.0, 28.0);
            let mode = Rect::new(stage.x - 8.0 - 104.0, row.y + 3.0, 104.0, 28.0);
            let mode_label = r.mode.map_or("Same mode", Mode::label);
            let stage_label = r.stage.map_or("Same DPI".to_string(), |s| format!("Stage {s}"));
            self.dropdown(g, t, hits, mode, mode_label, r.mode.is_some(), Hit::RuleMode(i));
            self.dropdown(g, t, hits, stage, &stage_label, r.stage.is_some(), Hit::RuleStage(i));
            if hover == Some(Hit::RuleRemove(i)) {
                g.fill(rm, 11.0, t.ctrl_hover);
            }
            g.text(
                "\u{E711}",
                rm,
                &g.fonts.glyph,
                if row_hover { t.text2 } else { t.text3 },
            );
            hits.push((rm, Hit::RuleRemove(i)));
        }
        if can_add {
            let row = Rect::new(cx - 6.0, ry + v.rules.len() as f32 * 38.0, inner_w + 12.0, 34.0);
            if hover == Some(Hit::AddRule) {
                g.fill(row, 6.0, t.ctrl_hover);
            }
            let label = Rect::new(row.x + 14.0, row.y, row.w - 14.0, row.h);
            g.text("+  Add app", label, &g.fonts.body, t.text2);
            hits.push((row, Hit::AddRule));
        }
        y + h
    }

    /// A button that opens a menu: "Label  ⌄".
    #[allow(clippy::too_many_arguments)]
    fn dropdown(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, r: Rect, label: &str, set: bool, hit: Hit) {
        let fill = if self.hover == Some(hit) {
            t.ctrl_hover.mix(t.ctrl, 0.3)
        } else {
            t.ctrl
        };
        g.fill(r, 6.0, fill);
        g.stroke(r, 6.0, 1.0, t.ctrl_stroke);
        let fg = if set { t.text } else { t.text2 };
        g.text(label, Rect::new(r.x + 10.0, r.y, r.w - 30.0, r.h), &g.fonts.small, fg);
        g.text(
            "\u{E70D}",
            Rect::new(r.x + r.w - 24.0, r.y, 16.0, r.h),
            &g.fonts.glyph,
            t.text2,
        );
        hits.push((r, hit));
    }

    /// About Raisin: version, what it is, links, credits.
    fn about_app(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, y: f32) -> f32 {
        let h = CARD_PAD * 2.0 + 28.0 + 20.0 + 30.0 + 20.0;
        let card = Rect::new(PAD, y, CARD_W, h);
        self.card_rect(g, t, card);
        let x = PAD + 16.0;
        let w = CARD_W - 32.0;
        let mut ly = y + CARD_PAD;

        // "Raisin 0.1.5" with the logo.
        if let Some(logo) = &g.logo {
            let r = Rect::new(x, ly + 2.0, 24.0, 24.0);
            unsafe {
                g.rt.DrawBitmap(logo, Some(&r.d2d()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
        let name_x = x + 32.0;
        g.text("Raisin", Rect::new(name_x, ly, 100.0, 28.0), &g.fonts.body_bold, t.text);
        let vx = name_x + g.measure("Raisin", &g.fonts.body_bold) + 6.0;
        let version = concat!("Version ", env!("CARGO_PKG_VERSION"));
        g.text(version, Rect::new(vx, ly + 1.0, 160.0, 28.0), &g.fonts.small, t.text2);
        ly += 28.0;
        g.text(
            "Lightweight Razer mouse settings, without Synapse.",
            Rect::new(x, ly, w, 20.0),
            &g.fonts.small,
            t.text2,
        );
        ly += 24.0;

        // Links, side by side.
        let mut lx = x - 6.0;
        for (i, (label, _)) in LINKS.iter().enumerate() {
            let lw = g.measure(label, &g.fonts.small) + 12.0;
            let r = Rect::new(lx, ly, lw, 24.0);
            if self.hover == Some(Hit::Link(i)) {
                g.fill(r, 4.0, t.ctrl_hover);
            }
            g.text(label, r, &g.fonts.center, t.accent);
            hits.push((r, Hit::Link(i)));
            lx += lw + 4.0;
        }
        ly += 30.0;
        g.text(
            "Mouse details come from the OpenRazer project. MIT licensed.",
            Rect::new(x, ly, w, 20.0),
            &g.fonts.small,
            t.text3,
        );
        y + h + CARD_GAP
    }

    /// Device details: firmware, USB ID, connection, support status, serial.
    /// The last week of battery readings, and how long it should last.
    fn history_card(&self, g: &Gfx, t: &Theme, y: f32) -> f32 {
        let v = &self.view;
        let h = CARD_PAD * 2.0 + 20.0 + 10.0 + GRAPH_H + 18.0;
        let card = Rect::new(PAD, y, CARD_W, h);
        self.card_rect(g, t, card);
        let x = PAD + 16.0;
        let w = CARD_W - 32.0;
        let ty = y + CARD_PAD;
        g.text("Last 7 days", Rect::new(x, ty, w, 20.0), &g.fonts.body, t.text);
        let note = if v.graph_volts {
            Some("Voltage".to_string())
        } else {
            v.estimate.clone()
        };
        if let Some(n) = note {
            let nw = g.measure(&n, &g.fonts.small);
            g.text(
                &n,
                Rect::new(x + w - nw, ty + 2.0, nw + 2.0, 20.0),
                &g.fonts.small,
                t.text2,
            );
        }

        // Guides at 0, 50 and 100% (or the Li-ion range), then the line.
        let gy = ty + 30.0;
        for f in [0.0, 0.5, 1.0] {
            let ly = (gy + GRAPH_H * (1.0 - f)).round() + 0.5;
            g.line((x, ly), (x + w, ly), 1.0, t.card_stroke);
        }
        if v.graph.len() < 2 {
            let msg = "No readings yet. Raisin adds one every 5 minutes while it runs in the tray.";
            let r = Rect::new(x + 24.0, gy + GRAPH_H / 2.0 - 18.0, w - 48.0, 36.0);
            g.fill(r.inset(-6.0), 6.0, t.card);
            g.text(msg, r, &g.fonts.center, t.text3);
        }
        let pt = |(px, py, _): (f32, f32, bool)| (x + px * w, gy + GRAPH_H * (1.0 - py));
        // No line across gaps longer than a few hours (mouse off or asleep).
        let gap = 4.0 / GRAPH_POINTS;
        for pair in v.graph.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b.0 - a.0 > gap {
                continue;
            }
            let c = if b.2 {
                t.accent
            } else if v.graph_volts {
                level_color(if b.1 > 0.4 { 100 } else { 5 })
            } else {
                level_color((b.1 * 100.0).round() as u8)
            };
            g.line(pt(a), pt(b), 2.0, c);
        }

        let ly = gy + GRAPH_H + 4.0;
        g.text("7 days ago", Rect::new(x, ly, w, 16.0), &g.fonts.small, t.text3);
        let now = "Now";
        let nw = g.measure(now, &g.fonts.small);
        g.text(now, Rect::new(x + w - nw, ly, nw + 2.0, 16.0), &g.fonts.small, t.text3);
        y + h
    }

    fn about(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, y: f32) -> f32 {
        let v = &self.view;
        let h = CARD_PAD * 2.0 + 20.0 * 2.0 + 4.0;
        let card = Rect::new(PAD, y, CARD_W, h);
        self.card_rect(g, t, card);
        let x = PAD + 16.0;
        let w = CARD_W - 32.0;
        let mut line1 = Vec::new();
        if let Some((maj, min)) = v.firmware {
            line1.push(format!("Firmware {maj}.{min:02}"));
        }
        if let Some(pid) = v.usb_id {
            line1.push(format!("USB 1532:{pid:04X}"));
        }
        if !v.connection.is_empty() {
            line1.push(v.connection.to_string());
        }
        let ly = y + CARD_PAD;
        g.text(&line1.join("  ·  "), Rect::new(x, ly, w, 20.0), &g.fonts.small, t.text2);
        let support = if v.tested {
            "Tested with Raisin"
        } else {
            "Untested: settings come from OpenRazer's data"
        };
        g.text(support, Rect::new(x, ly + 24.0, w, 20.0), &g.fonts.small, t.text2);

        // Serial: copy instead of showing it, so it stays out of screenshots.
        if v.serial.is_some() {
            let copied = self.copied_until.is_some_and(|t| t > Instant::now());
            let label = if copied { "Serial copied" } else { "Copy serial" };
            let lw = g.measure(label, &g.fonts.small) + 16.0;
            let r = Rect::new(x + w - lw + 8.0, ly, lw, 20.0);
            if self.hover == Some(Hit::CopySerial) {
                g.fill(r, 4.0, t.ctrl_hover);
            }
            g.text(label, r, &g.fonts.center, if copied { t.text2 } else { t.accent });
            hits.push((r, Hit::CopySerial));
        }
        y + h + CARD_GAP
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp & 0xFFFF) as i16 as f32;
        let y = ((lp >> 16) & 0xFFFF) as i16 as f32;
        let s = 96.0 / self.dpi as f32;
        (x * s, y * s + self.scroll)
    }

    fn edit_dpi(&mut self, f: impl FnOnce(&mut Vec<u16>, u16)) {
        let base = self.view.dpi_values();
        let mut p = self.pending.clone().unwrap_or_else(|| base.clone());
        f(&mut p, self.view.dpi_max);
        self.pending = if p == base { None } else { Some(p) };
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }
}

// ---------- input ----------

/// Run a click. Local edits stay in the window; mouse settings go through
/// the app (which then calls `sync`).
fn activate(hit: Hit) {
    let v = with_ui(|u| u.view.clone()).unwrap_or_default();
    let toggle = |cur: Option<bool>| cur != Some(true);
    match hit {
        Hit::BatType(i) => drop(with_app(|a| a.act(|a| a.set_battery_choice(i)))),
        Hit::Step => drop(with_app(|a| a.act(|a| a.set_liion_step(!v.liion_step)))),
        Hit::Lowbat(i) => drop(with_app(|a| a.act(|a| a.set_lowbat(razer::LOW_BATTERY_CHOICES[i])))),
        Hit::Poll(i) => {
            if let Some(&hz) = v.poll_rates.get(i) {
                with_app(|a| a.act(|a| a.set_poll(hz)));
            }
        }
        Hit::Idle(i) => drop(with_app(|a| a.act(|a| a.set_idle(IDLE_CHOICES[i])))),
        Hit::Power(i) => drop(with_app(|a| a.act(|a| a.set_power(Mode::ALL[i])))),
        Hit::Info(_) => {}
        Hit::DpiPopup => drop(with_app(|a| a.set_dpi_popup(!v.dpi_popup))),
        Hit::Hotkeys => drop(with_app(|a| a.set_hotkeys(!v.hotkeys))),
        Hit::Link(i) => {
            if let Some((_, url)) = LINKS.get(i) {
                unsafe {
                    windows_sys::Win32::UI::Shell::ShellExecuteW(
                        window(),
                        w("open").as_ptr(),
                        w(url).as_ptr(),
                        null(),
                        null(),
                        SW_SHOWNORMAL,
                    );
                }
            }
        }
        Hit::Tab(tab) => drop(with_ui(|u| {
            u.tab = tab;
            u.scroll = 0.0;
            u.hover = None;
            unsafe { InvalidateRect(u.hwnd, null(), 0) };
        })),
        Hit::AutoPower => drop(with_app(|a| a.set_auto_power(!v.auto_power))),
        Hit::RuleMode(i) => {
            let Some(rule) = v.rules.get(i) else { return };
            let mut items = vec![(1, "Same mode".to_string(), rule.mode.is_none())];
            items.push((0, String::new(), false));
            for (k, m) in Mode::ALL.iter().enumerate() {
                items.push((2 + k, m.label().to_string(), rule.mode == Some(*m)));
            }
            let pick = popup(hit, &items, &[]);
            if pick != 0 {
                let mode = pick.checked_sub(2).map(|k| Mode::ALL[k]);
                with_app(|a| a.edit_rules(|r| r[i].mode = mode));
            }
        }
        Hit::RuleStage(i) => {
            let Some(rule) = v.rules.get(i) else { return };
            let mut items = vec![(1, "Same DPI".to_string(), rule.stage.is_none())];
            items.push((0, String::new(), false));
            for s in 1..=v.stage_choices() {
                let label = match v.stages.get(s - 1) {
                    Some(dpi) => format!("Stage {s}  ·  {dpi} DPI"),
                    None => format!("Stage {s}"),
                };
                items.push((1 + s, label, rule.stage == Some(s as u8)));
            }
            let pick = popup(hit, &items, &[]);
            if pick != 0 {
                let stage = pick.checked_sub(1).filter(|&s| s > 0).map(|s| s as u8);
                with_app(|a| a.edit_rules(|r| r[i].stage = stage));
            }
        }
        Hit::RuleRemove(i) => drop(with_app(|a| {
            a.edit_rules(|r| {
                if i < r.len() {
                    r.remove(i);
                }
            })
        })),
        Hit::AddRule => {
            let apps = running_apps();
            let taken: Vec<usize> = (0..apps.len())
                .filter(|&k| v.rules.iter().any(|r| r.exe == apps[k]))
                .collect();
            let mut items: Vec<(usize, String, bool)> = apps
                .iter()
                .enumerate()
                .map(|(k, a)| (100 + k, a.strip_suffix(".exe").unwrap_or(a).to_string(), false))
                .collect();
            let grayed: Vec<usize> = taken.iter().map(|k| 100 + k).collect();
            if !items.is_empty() {
                items.push((0, String::new(), false));
            }
            items.push((1, "Browse for a program…".to_string(), false));
            let pick = popup(hit, &items, &grayed);
            let exe = match pick {
                0 => None,
                1 => browse_exe(window()),
                k => apps.get(k - 100).cloned(),
            };
            if let Some(exe) = exe.filter(|e| !e.is_empty() && !v.rules.iter().any(|r| r.exe == *e)) {
                // A new rule starts on Max: the usual reason to add a game.
                let mut rule = AppRule::new(&exe);
                rule.mode = Some(Mode::Max);
                with_app(|a| a.edit_rules(|r| r.push(rule)));
            }
        }
        Hit::FreeSpin => {
            let on = toggle(v.free_spin);
            with_app(|a| a.act(|a| a.need_mouse()?.set_free_spin(on)));
        }
        Hit::ScrollAccel => {
            let on = toggle(v.scroll_accel);
            with_app(|a| a.act(|a| a.need_mouse()?.set_scroll_acceleration(on)));
        }
        Hit::SmartReel => {
            let on = toggle(v.smart_reel);
            with_app(|a| a.act(|a| a.need_mouse()?.set_smart_reel(on)));
        }
        Hit::TrayIcon(i) => drop(with_app(|a| a.set_tray_icon(TrayIcon::ALL[i]))),
        Hit::Stage(i) => {
            // Unapplied edits would be lost by a stage switch; apply them first.
            let pending = with_ui(|u| u.pending.clone()).flatten();
            if let Some(p) = pending {
                with_app(|a| a.apply_stages(p));
            }
            with_app(|a| a.act(|a| a.select_stage(i)));
        }
        Hit::Minus(i) => drop(with_ui(|u| u.edit_dpi(|s, max| s[i] = bump(s[i], false, max)))),
        Hit::Plus(i) => drop(with_ui(|u| u.edit_dpi(|s, max| s[i] = bump(s[i], true, max)))),
        Hit::Remove(i) => drop(with_ui(|u| {
            u.edit_dpi(|s, _| {
                if s.len() > 1 {
                    s.remove(i);
                }
            })
        })),
        Hit::AddStage => drop(with_ui(|u| {
            u.edit_dpi(|s, max| {
                if s.len() < 5 {
                    let next = s.last().map_or(800, |&l| l.saturating_mul(2));
                    s.push(next.clamp(razer::DPI_MIN, max.max(razer::DPI_MIN)));
                }
            })
        })),
        Hit::Apply => {
            if let Some(p) = with_ui(|u| u.pending.clone()).flatten() {
                if v.has_stages {
                    with_app(|a| a.apply_stages(p));
                } else if let Some(&dpi) = p.first() {
                    with_app(|a| a.act(|a| a.need_mouse()?.set_dpi(dpi, dpi)));
                    with_ui(|u| u.pending = None);
                }
            }
        }
        Hit::Revert => drop(with_ui(|u| {
            u.pending = None;
            unsafe { InvalidateRect(u.hwnd, null(), 0) };
        })),
        Hit::Autostart => {
            set_autostart(!v.autostart);
            with_ui(|u| {
                u.view.autostart = autostart_enabled();
                unsafe { InvalidateRect(u.hwnd, null(), 0) };
            });
        }
        Hit::CopySerial => {
            if let Some(serial) = &v.serial {
                with_ui(|u| {
                    if copy_to_clipboard(u.hwnd, serial) {
                        u.copied_until = Some(Instant::now() + Duration::from_secs(2));
                        unsafe {
                            SetTimer(u.hwnd, TIMER_COPIED, 2000, None);
                            InvalidateRect(u.hwnd, null(), 0);
                        }
                    }
                });
            }
        }
    }
}

/// Show a menu under the control for `hit`. `items` are (id, label,
/// checked); id 0 is a separator. Returns the picked id, or 0.
fn popup(hit: Hit, items: &[(usize, String, bool)], grayed: &[usize]) -> usize {
    let Some((hwnd, pt)) = with_ui(|u| {
        let r = u.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r)?;
        let s = u.dpi as f32 / 96.0;
        let mut pt = POINT {
            x: (r.x * s).round() as i32,
            y: ((r.y + r.h - u.scroll) * s).round() as i32 + 2,
        };
        unsafe { ClientToScreen(u.hwnd, &mut pt) };
        Some((u.hwnd, pt))
    })
    .flatten() else {
        return 0;
    };
    unsafe {
        let menu = CreatePopupMenu();
        for (id, label, checked) in items {
            if *id == 0 {
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
                continue;
            }
            let mut flags = MF_STRING;
            if *checked {
                flags |= MF_CHECKED;
            }
            if grayed.contains(id) {
                flags |= MF_GRAYED;
            }
            AppendMenuW(menu, flags, *id, w(label).as_ptr());
        }
        let pick = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        );
        DestroyMenu(menu);
        // The menu ate the mouse-up; drop the stale hover.
        with_ui(|u| {
            u.hover = None;
            u.tracking = false;
            InvalidateRect(u.hwnd, null(), 0);
        });
        pick.max(0) as usize
    }
}

/// Programs with a normal window open, for "Add app". Skips Raisin and
/// shell hosts a rule would make no sense for.
fn running_apps() -> Vec<String> {
    const SKIP: [&str; 6] = [
        "applicationframehost.exe",
        "textinputhost.exe",
        "shellexperiencehost.exe",
        "startmenuexperiencehost.exe",
        "searchhost.exe",
        "lockapp.exe",
    ];
    unsafe extern "system" fn add(h: HWND, lp: LPARAM) -> windows_sys::core::BOOL {
        let list = &mut *(lp as *mut Vec<String>);
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if IsWindowVisible(h) == 0
            || GetWindowTextLengthW(h) == 0
            || ex & WS_EX_TOOLWINDOW != 0
            || !GetWindow(h, GW_OWNER).is_null()
        {
            return 1;
        }
        // Hidden UWP windows and other virtual desktops' windows.
        let mut cloaked = 0u32;
        let r = DwmGetWindowAttribute(
            WHWND(h),
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        );
        if r.is_ok() && cloaked != 0 {
            return 1;
        }
        if let Some(name) = super::window_exe(h) {
            if !list.contains(&name) {
                list.push(name);
            }
        }
        1
    }
    let mut list: Vec<String> = Vec::new();
    unsafe { EnumWindows(Some(add), &mut list as *mut Vec<String> as LPARAM) };
    let own = super::own_exe();
    list.retain(|n| *n != own && !SKIP.contains(&n.as_str()));
    list.sort();
    list
}

/// Pick a program file; returns its lowercase file name.
fn browse_exe(owner: HWND) -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::*;
    let mut file = [0u16; 1024];
    let filter: Vec<u16> = "Programs (*.exe)\0*.exe\0\0".encode_utf16().collect();
    let title = w("Choose a program");
    unsafe {
        let mut ofn: OPENFILENAMEW = zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = owner;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = file.as_mut_ptr();
        ofn.nMaxFile = file.len() as u32;
        ofn.lpstrTitle = title.as_ptr();
        ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_HIDEREADONLY | OFN_EXPLORER;
        if GetOpenFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    let n = file.iter().position(|&c| c == 0).unwrap_or(0);
    Some(crate::auto::exe_name(&String::from_utf16_lossy(&file[..n])))
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            with_ui(|u| u.paint());
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_FIT => {
            if let Some((dpi, h)) = with_ui(|u| (u.dpi, u.content_h)) {
                fit_window(hwnd, dpi, h, false);
            }
            0
        }
        WM_SIZE => {
            let (cw, ch) = ((lp & 0xFFFF) as u32, ((lp >> 16) & 0xFFFF) as u32);
            with_ui(|u| {
                if let Some(g) = &u.gfx {
                    let _ = g.rt.Resize(&D2D_SIZE_U { width: cw, height: ch });
                }
                u.view_h = ch as f32 * 96.0 / u.dpi as f32;
                u.scroll = clamp_scroll(u.scroll, u.content_h, u.view_h);
                InvalidateRect(hwnd, null(), 0);
            });
            0
        }
        WM_DPICHANGED => {
            let dpi = ((wp >> 16) & 0xFFFF) as u32;
            let r = &*(lp as *const RECT);
            with_ui(|u| {
                u.dpi = dpi;
                if let Some(g) = &u.gfx {
                    g.rt.SetDpi(dpi as f32, dpi as f32);
                }
            });
            SetWindowPos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            0
        }
        WM_SETTINGCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED => {
            // Theme or accent changed.
            let dark = system_dark();
            with_ui(|u| {
                u.dark = dark;
                u.acrylic = apply_backdrop(hwnd, dark);
                InvalidateRect(hwnd, null(), 0);
            });
            0
        }
        WM_MOUSEMOVE => {
            with_ui(|u| {
                if !u.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                    u.tracking = true;
                }
                let (x, y) = u.dip(lp);
                let h = u.hit_at(x, y);
                if h != u.hover {
                    u.hover = h;
                    InvalidateRect(hwnd, null(), 0);
                }
            });
            0
        }
        WM_MOUSELEAVE => {
            with_ui(|u| {
                u.tracking = false;
                if u.hover.take().is_some() {
                    InvalidateRect(hwnd, null(), 0);
                }
            });
            0
        }
        WM_SETCURSOR if (lp & 0xFFFF) as u32 == HTCLIENT => {
            // Hand over anything clickable; (i) icons only show a bubble.
            let over = with_ui(|u| matches!(u.hover, Some(h) if !matches!(h, Hit::Info(_)))).unwrap_or(false);
            SetCursor(LoadCursorW(null_mut(), if over { IDC_HAND } else { IDC_ARROW }));
            1
        }
        WM_LBUTTONDOWN => {
            with_ui(|u| {
                let (x, y) = u.dip(lp);
                u.pressed = u.hit_at(x, y);
            });
            0
        }
        WM_LBUTTONUP => {
            let hit = with_ui(|u| {
                let (x, y) = u.dip(lp);
                let h = u.hit_at(x, y);
                let p = u.pressed.take();
                (h == p).then_some(h).flatten()
            })
            .flatten();
            if let Some(h) = hit {
                activate(h);
            }
            0
        }
        WM_MOUSEWHEEL => {
            // Over a DPI value: nudge it. Elsewhere, scroll the window if
            // it doesn't fit. When it scrolls, only the steppers nudge, so
            // scrolling past the DPI card doesn't change DPI.
            let delta = ((wp >> 16) & 0xFFFF) as i16;
            let mut pt = POINT {
                x: (lp & 0xFFFF) as i16 as i32,
                y: ((lp >> 16) & 0xFFFF) as i16 as i32,
            };
            ScreenToClient(hwnd, &mut pt);
            with_ui(|u| {
                let s = 96.0 / u.dpi as f32;
                let (x, y) = (pt.x as f32 * s, pt.y as f32 * s + u.scroll);
                let hit = u.hit_at(x, y);
                let nudge = match hit {
                    Some(Hit::Minus(i) | Hit::Plus(i)) => Some(i),
                    Some(Hit::Stage(i) | Hit::Remove(i)) if !u.scrollable() => Some(i),
                    _ => None,
                };
                if let Some(i) = nudge {
                    u.edit_dpi(|st, max| st[i] = bump(st[i], delta > 0, max));
                } else if u.scrollable() {
                    let scroll = clamp_scroll(u.scroll - delta as f32 / 120.0 * 60.0, u.content_h, u.view_h);
                    if scroll != u.scroll {
                        u.scroll = scroll;
                        // What's under the pointer moved.
                        u.hover = u.hit_at(x, pt.y as f32 * s + scroll);
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
            });
            0
        }
        WM_KEYDOWN if wp == VK_ESCAPE as usize => {
            DestroyWindow(hwnd);
            0
        }
        WM_TIMER if wp == TIMER_LIVE => {
            with_app(|a| {
                a.refresh(false);
                sync(a, false);
            });
            0
        }
        WM_TIMER if wp == TIMER_COPIED => {
            KillTimer(hwnd, TIMER_COPIED);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER_LIVE);
            for which in [ICON_SMALL, ICON_BIG] {
                let icon = SendMessageW(hwnd, WM_GETICON, which as usize, 0) as HICON;
                if !icon.is_null() {
                    DestroyIcon(icon);
                }
            }
            // Drop the window state and all D2D/DWrite resources.
            UI.with(|u| {
                if let Ok(mut u) = u.try_borrow_mut() {
                    *u = None;
                }
            });
            // This window is the whole settings process; exiting returns
            // all its memory to the OS.
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_stepper_is_symmetric_at_boundaries() {
        assert_eq!(bump(950, true, 30000), 1000);
        assert_eq!(bump(1000, false, 30000), 950);
        assert_eq!(bump(1000, true, 30000), 1100);
        assert_eq!(bump(5000, false, 30000), 4900);
        assert_eq!(bump(5000, true, 30000), 5500);
    }

    #[test]
    fn dpi_stepper_clamps_to_the_mouse() {
        assert_eq!(bump(razer::DPI_MIN, false, 30000), razer::DPI_MIN);
        assert_eq!(bump(30000, true, 30000), 30000);
        assert_eq!(bump(1800, true, 1800), 1800);
    }

    #[test]
    fn bubbles_stay_inside_the_window() {
        let w = TIP_W;
        // Under the icon when there's room.
        let (x, y) = bubble_pos(Rect::new(100.0, 100.0, 20.0, 20.0), w, 80.0, 0.0, 600.0);
        assert_eq!(y, 124.0);
        assert!(x >= 0.0 && x + w <= WIDTH);
        // Flipped above near the bottom.
        let (_, y) = bubble_pos(Rect::new(100.0, 560.0, 20.0, 20.0), w, 80.0, 0.0, 600.0);
        assert_eq!(y, 560.0 - 4.0 - 80.0);
        // Pulled in from the edges.
        let (x, _) = bubble_pos(Rect::new(10.0, 100.0, 20.0, 20.0), w, 80.0, 0.0, 600.0);
        assert_eq!(x, PAD / 2.0);
        let (x, _) = bubble_pos(Rect::new(WIDTH - 20.0, 100.0, 20.0, 20.0), w, 80.0, 0.0, 600.0);
        assert_eq!(x, WIDTH - PAD / 2.0 - w);
        // Scrolled: stays below the visible top.
        let (_, y) = bubble_pos(Rect::new(100.0, 420.0, 20.0, 20.0), w, 80.0, 400.0, 500.0);
        assert_eq!(y, 404.0);
    }

    #[test]
    fn scrolling_stays_within_the_content() {
        assert_eq!(clamp_scroll(-10.0, 1000.0, 800.0), 0.0);
        assert_eq!(clamp_scroll(500.0, 1000.0, 800.0), 200.0);
        assert_eq!(clamp_scroll(50.0, 600.0, 800.0), 0.0);
    }

    #[test]
    fn every_tip_has_text() {
        use Tip::*;
        for t in [
            BatteryType,
            WarnAt,
            LiionStep,
            Stages,
            SingleDpi,
            FreeSpin,
            ScrollAccel,
            SmartReel,
            Power,
            Poll,
            Sleep,
            TrayIcon,
            Auto,
            AutoPower,
            DpiPopup,
            Hotkeys,
        ] {
            assert!(t.text().len() > 20, "{t:?}");
        }
    }

    #[test]
    fn poll_labels_shorten_when_crowded() {
        assert_eq!(poll_label(1000, false), "1000 Hz");
        assert_eq!(poll_label(8000, true), "8K");
        assert_eq!(poll_label(125, true), "125");
    }
}
