//! Settings window: custom-drawn with Direct2D/DirectWrite on a Windows 11
//! acrylic backdrop. Follows the system light/dark theme and accent color.
//!
//! Everything here (window, D2D factory, render target, fonts) is created
//! when the window opens and released when it closes, so the tray app stays
//! at its idle footprint.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows::core::{w as wstr, PCWSTR};
use windows::Win32::Foundation::HWND as WHWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::UI::Controls::MARGINS;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
const WM_MOUSELEAVE: u32 = 0x02A3;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{autostart_enabled, set_autostart, w, with_app, App, IDLE_CHOICES};

type SysPcwstr = windows_sys::core::PCWSTR;

/// Logo pixels for the header: premultiplied BGRA, generated from
/// Raisin.png by tools/make-icons.ps1.
const LOGO_PX: u32 = 128;
static LOGO_BGRA: &[u8] = include_bytes!("../assets/logo.bgra");
use crate::config::TrayIcon;
use crate::razer;

const CLASS: &str = "RaisinSettings";
const TIMER_LIVE: usize = 1;
const LIVE_MS: u32 = 5000;

// Layout, in DIPs.
const WIDTH: f32 = 420.0;
const HEIGHT: f32 = 708.0;
const PAD: f32 = 16.0;
const CARD_W: f32 = WIDTH - 2.0 * PAD;
const ROW_H: f32 = 30.0;
const CTRL_W: f32 = 252.0;

const BATTERY_LABELS: [&str; 4] = ["Alkaline", "NiMH", "Lithium", "Li-ion"];
const LOWBAT_LABELS: [&str; 6] = ["5%", "10%", "15%", "20%", "25%", "30%"];
const POLL_LABELS: [&str; 3] = ["125 Hz", "500 Hz", "1000 Hz"];
const IDLE_LABELS: [&str; 5] = ["1 min", "2 min", "5 min", "10 min", "15 min"];

// ---------- data shown in the window ----------

/// Snapshot of app state, so painting never needs the app borrowed.
#[derive(Clone, Default, PartialEq)]
struct View {
    /// Mouse not read yet (window just opened).
    loading: bool,
    connected: bool,
    asleep: bool,
    battery: Option<u8>,
    voltage_mv: Option<u16>,
    battery_choice: Option<usize>,
    liion_step: bool,
    lowbat: Option<usize>,
    stages: Vec<u16>,
    /// 1-based; 0 = unknown.
    active: usize,
    poll: Option<usize>,
    idle: Option<usize>,
    autostart: bool,
    tray_icon: usize,
}

impl View {
    fn from_app(app: &App) -> View {
        let s = &app.state;
        View {
            loading: app.last_refresh.is_none(),
            connected: s.connected,
            asleep: s.asleep,
            battery: s.battery,
            voltage_mv: s.voltage_mv,
            battery_choice: app.battery_choice(),
            liion_step: app.config.liion_step,
            lowbat: app.lowbat_index(),
            stages: s.stages.as_ref().map(|st| st.stages.iter().map(|v| v.0).collect()).unwrap_or_default(),
            active: s.stages.as_ref().map_or(0, |st| st.active as usize),
            poll: s.poll.and_then(|p| razer::POLL_RATES.iter().position(|&r| r == p)),
            idle: s.idle.and_then(|v| IDLE_CHOICES.iter().position(|&c| c == v)),
            autostart: autostart_enabled(),
            tray_icon: TrayIcon::ALL.iter().position(|&t| t == app.config.tray_icon).unwrap_or(0),
        }
    }

    fn usable(&self) -> bool {
        self.connected && !self.asleep
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
    Poll(usize),
    Idle(usize),
    TrayIcon(usize),
    Autostart,
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
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

// ---------- theme ----------

#[derive(Clone, Copy)]
struct C(f32, f32, f32, f32);

impl C {
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
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

fn system_dark() -> bool {
    reg_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "AppsUseLightTheme") == Some(0)
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

// ---------- graphics ----------

struct Fonts {
    big: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
    center: IDWriteTextFormat,
    value: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
}

struct Gfx {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    fonts: Fonts,
    logo: Option<ID2D1Bitmap>,
}

fn text_format(dw: &IDWriteFactory, family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, center: bool) -> windows::core::Result<IDWriteTextFormat> {
    unsafe {
        let f = dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, wstr!("en-us"))?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        f.SetTextAlignment(if center { DWRITE_TEXT_ALIGNMENT_CENTER } else { DWRITE_TEXT_ALIGNMENT_LEADING })?;
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
                pixelSize: D2D_SIZE_U { width: rc.right as u32, height: rc.bottom as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = factory.CreateHwndRenderTarget(&props, &hprops)?;
            // ClearType needs an opaque background; grayscale works on glass.
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&C(1.0, 1.0, 1.0, 1.0).d2d(), None)?;

            let text = wstr!("Segoe UI Variable Text");
            let display = wstr!("Segoe UI Variable Display");
            let fonts = Fonts {
                big: text_format(&dw, display, 30.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
                body: text_format(&dw, text, 13.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
                body_bold: text_format(&dw, text, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
                small: text_format(&dw, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
                center: text_format(&dw, text, 12.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
                value: text_format(&dw, text, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?,
                glyph: text_format(&dw, wstr!("Segoe Fluent Icons"), 10.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
            };
            let logo = rt
                .CreateBitmap(
                    D2D_SIZE_U { width: LOGO_PX, height: LOGO_PX },
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
            Ok(Gfx { rt, brush, fonts, logo })
        }
    }

    fn color(&self, c: C) {
        unsafe { self.brush.SetColor(&c.d2d()) };
    }

    fn fill(&self, r: Rect, radius: f32, c: C) {
        self.color(c);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
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

    fn text(&self, s: &str, r: Rect, f: &IDWriteTextFormat, c: C) {
        self.color(c);
        let u: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.rt.DrawText(&u, f, &r.d2d(), &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
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
            })
        });
        fit_to_dpi(hwnd, dpi);
        SetTimer(hwnd, TIMER_LIVE, LIVE_MS, None);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        UpdateWindow(hwnd); // paint now, before the caller's blocking mouse read
    }
}

unsafe fn create() -> Option<HWND> {
    let hinst = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null());
    let dpi = GetDpiForSystem();
    let (ww, wh) = outer_size(dpi);
    let x = (GetSystemMetrics(SM_CXSCREEN) - ww) / 2;
    let y = (GetSystemMetrics(SM_CYSCREEN) - wh) / 2;
    let hwnd = CreateWindowExW(
        0,
        w(CLASS).as_ptr(),
        w("Raisin").as_ptr(),
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        x, y, ww, wh,
        null_mut(), null_mut(), hinst, null(),
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
    Some(hwnd)
}

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

fn outer_size(dpi: u32) -> (i32, i32) {
    let s = |v: f32| (v * dpi as f32 / 96.0).round() as i32;
    let mut rc = RECT { left: 0, top: 0, right: s(WIDTH), bottom: s(HEIGHT) };
    unsafe { AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi) };
    (rc.right - rc.left, rc.bottom - rc.top)
}

unsafe fn fit_to_dpi(hwnd: HWND, dpi: u32) {
    let (ww, wh) = outer_size(dpi);
    SetWindowPos(hwnd, null_mut(), 0, 0, ww, wh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
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
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
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
        if u.view != view {
            u.view = view;
            unsafe { InvalidateRect(u.hwnd, null(), 0) };
        } else if reset_edits {
            unsafe { InvalidateRect(u.hwnd, null(), 0) };
        }
    });
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

fn bump(v: u16, up: bool) -> u16 {
    let n = if up { v.saturating_add(step_size(v)) } else { v.saturating_sub(step_size(v.saturating_sub(1))) };
    n.clamp(razer::DPI_MIN, razer::DPI_MAX)
}

impl Ui {
    fn stages(&self) -> &[u16] {
        self.pending.as_deref().unwrap_or(&self.view.stages)
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
            let bg = if self.acrylic { C(0.0, 0.0, 0.0, 0.0) } else { t.fallback_bg };
            g.rt.Clear(Some(&bg.d2d()));
        }
        self.draw(&g, &t, &mut hits);
        let ok = unsafe { g.rt.EndDraw(None, None) }.is_ok();
        self.hits = hits;
        // On device loss, rebuild everything on the next paint.
        if ok {
            self.gfx = Some(g);
        }
    }

    fn draw(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>) {
        let v = &self.view;
        let usable = v.usable();
        let hover = self.hover;

        // --- Header: battery glyph, level, voltage ---
        let gx = PAD + 2.0;
        let gy = 30.0;
        let body = Rect::new(gx, gy, 40.0, 22.0);
        g.stroke(body, 5.0, 1.5, if usable { t.text } else { t.text3 });
        g.fill(Rect::new(gx + 40.5, gy + 7.0, 3.0, 8.0), 1.5, if usable { t.text } else { t.text3 });
        if let (true, Some(b)) = (usable, v.battery) {
            let inner = body.inset(3.5);
            let w = (inner.w * b.min(100) as f32 / 100.0).max(3.0);
            g.fill(Rect::new(inner.x, inner.y, w, inner.h), 2.5, level_color(b));
        }
        let tx = PAD + 62.0;
        let (big, sub) = if v.loading {
            ("Raisin".to_string(), "Reading mouse…".to_string())
        } else if !v.connected {
            ("Not connected".to_string(), "Plug in the HyperSpeed receiver".to_string())
        } else if v.asleep {
            ("Asleep".to_string(), "Move the mouse to wake it".to_string())
        } else {
            let big = v.battery.map_or("—".to_string(), |b| format!("{b}%"));
            let mut sub = String::from("Viper V3 HyperSpeed");
            if let Some(mv) = v.voltage_mv {
                sub += &format!("  ·  ≈{}.{:02} V", mv / 1000, mv % 1000 / 10);
            }
            (big, sub)
        };
        g.text(&big, Rect::new(tx, 14.0, 270.0, 40.0), &g.fonts.big, t.text);
        g.text(&sub, Rect::new(tx + 1.0, 52.0, 270.0, 20.0), &g.fonts.small, t.text2);
        if let Some(logo) = &g.logo {
            let r = Rect::new(WIDTH - PAD - 44.0, 20.0, 44.0, 44.0);
            unsafe {
                g.rt.DrawBitmap(logo, Some(&r.d2d()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }

        // --- Battery card ---
        let mut y = 92.0;
        let card = Rect::new(PAD, y, CARD_W, 142.0);
        self.card(g, t, card);
        y += 14.0;
        self.label(g, t, "Battery type", y, usable);
        self.segmented(g, t, hits, y, &BATTERY_LABELS, v.battery_choice, usable, Hit::BatType, hover);
        y += ROW_H + 12.0;
        self.label(g, t, "Warn at", y, usable);
        self.segmented(g, t, hits, y, &LOWBAT_LABELS, v.lowbat, usable, Hit::Lowbat, hover);
        y += ROW_H + 12.0;
        let liion = v.battery_choice == Some(3);
        self.toggle_row(g, t, hits, y, "Li-ion cells drop voltage when low", v.liion_step, usable && liion, Hit::Step, hover);

        // --- DPI card ---
        y = 246.0;
        let card = Rect::new(PAD, y, CARD_W, 250.0);
        self.card(g, t, card);
        let cx = PAD + 16.0;
        let inner_w = CARD_W - 32.0;
        y += 14.0;
        g.text("DPI stages", Rect::new(cx, y, 200.0, ROW_H), &g.fonts.body_bold, if usable { t.text } else { t.text3 });
        let dirty = self.pending.as_ref().is_some_and(|p| *p != v.stages);
        if dirty && usable {
            let apply = Rect::new(cx + inner_w - 76.0, y + 1.0, 76.0, 28.0);
            let a = if hover == Some(Hit::Apply) { t.accent.mix(t.text, 0.12) } else { t.accent };
            g.fill(apply, 6.0, a);
            g.text("Apply", apply, &g.fonts.center, t.on_accent);
            hits.push((apply, Hit::Apply));
            let revert = Rect::new(apply.x - 72.0, y + 1.0, 66.0, 28.0);
            if hover == Some(Hit::Revert) {
                g.fill(revert, 6.0, t.ctrl_hover);
            }
            g.text("Revert", revert, &g.fonts.center, t.text2);
            hits.push((revert, Hit::Revert));
        }
        y += ROW_H + 6.0;
        let stages = self.stages().to_vec();
        for i in 0..5 {
            let row = Rect::new(cx - 6.0, y + i as f32 * 38.0, inner_w + 12.0, 34.0);
            if i < stages.len() {
                self.stage_row(g, t, hits, row, i, stages[i], stages.len(), usable, hover);
            } else if i == stages.len() && usable {
                if hover == Some(Hit::AddStage) {
                    g.fill(row, 6.0, t.ctrl_hover);
                }
                g.text("+  Add stage", row.inset(0.0).shift(14.0), &g.fonts.body, t.text2);
                hits.push((row, Hit::AddStage));
            }
        }

        // --- Performance & power card ---
        y = 508.0;
        let card = Rect::new(PAD, y, CARD_W, 184.0);
        self.card(g, t, card);
        y += 14.0;
        self.label(g, t, "Polling rate", y, usable);
        self.segmented(g, t, hits, y, &POLL_LABELS, v.poll, usable, Hit::Poll, hover);
        y += ROW_H + 12.0;
        self.label(g, t, "Sleep after", y, usable);
        self.segmented(g, t, hits, y, &IDLE_LABELS, v.idle, usable, Hit::Idle, hover);
        y += ROW_H + 12.0;
        // Not a mouse setting: always enabled.
        self.label(g, t, "Tray icon", y, true);
        let tray_labels = TrayIcon::ALL.map(|t| t.label());
        self.segmented(g, t, hits, y, &tray_labels, Some(v.tray_icon), true, Hit::TrayIcon, hover);
        y += ROW_H + 12.0;
        self.toggle_row(g, t, hits, y, "Start Raisin with Windows", v.autostart, true, Hit::Autostart, hover);
    }

    fn card(&self, g: &Gfx, t: &Theme, r: Rect) {
        g.fill(r, 8.0, t.card);
        g.stroke(r, 8.0, 1.0, t.card_stroke);
    }

    fn label(&self, g: &Gfx, t: &Theme, s: &str, y: f32, enabled: bool) {
        g.text(s, Rect::new(PAD + 16.0, y, 110.0, ROW_H), &g.fonts.body, if enabled { t.text } else { t.text3 });
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
        let seg_w = r.w / labels.len() as f32;
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
                if enabled { t.text } else { t.text3 }
            };
            g.text(l, s, &g.fonts.center, color);
            if enabled && !sel {
                hits.push((s, hit(i)));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn toggle_row(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, y: f32, text: &str, on: bool, enabled: bool, hit: Hit, hover: Option<Hit>) {
        let row = Rect::new(PAD + 6.0, y, CARD_W - 12.0, ROW_H);
        if enabled && hover == Some(hit) {
            g.fill(row, 6.0, t.ctrl_hover);
        }
        g.text(text, Rect::new(PAD + 16.0, y, 300.0, ROW_H), &g.fonts.body, if enabled { t.text } else { t.text3 });
        let tr = Rect::new(PAD + CARD_W - 16.0 - 40.0, y + 5.0, 40.0, 20.0);
        if on {
            g.fill(tr, 10.0, if enabled { t.accent } else { t.text3 });
            g.fill(Rect::new(tr.x + 23.0, tr.y + 3.0, 14.0, 14.0), 7.0, t.on_accent);
        } else {
            g.stroke(tr, 10.0, 1.0, if enabled { t.text2 } else { t.text3 });
            g.fill(Rect::new(tr.x + 5.0, tr.y + 5.0, 10.0, 10.0), 5.0, if enabled { t.text2 } else { t.text3 });
        }
        if enabled {
            hits.push((row, hit));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_row(&self, g: &Gfx, t: &Theme, hits: &mut Vec<(Rect, Hit)>, row: Rect, i: usize, value: u16, count: usize, usable: bool, hover: Option<Hit>) {
        let v = &self.view;
        let active = v.active == i + 1;
        let row_hover = matches!(hover, Some(Hit::Stage(j) | Hit::Minus(j) | Hit::Plus(j) | Hit::Remove(j)) if j == i);
        if active && usable {
            g.fill(row, 6.0, t.accent.alpha(0.16));
            g.fill(Rect::new(row.x, row.y + 9.0, 3.0, row.h - 18.0), 1.5, t.accent);
        } else if usable && row_hover {
            g.fill(row, 6.0, t.ctrl_hover);
        }
        // Radio
        let rc = Rect::new(row.x + 14.0, row.y + 9.0, 16.0, 16.0);
        if active {
            g.fill(rc, 8.0, if usable { t.accent } else { t.text3 });
            g.fill(rc.inset(5.0), 3.0, t.on_accent);
        } else {
            g.stroke(rc, 8.0, 1.0, if usable { t.text2 } else { t.text3 });
        }
        let fg = if usable { t.text } else { t.text3 };
        g.text(&format!("Stage {}", i + 1), Rect::new(row.x + 40.0, row.y, 100.0, row.h), &g.fonts.body, fg);

        // Stepper: [-] value [+]  (x)
        let right = row.x + row.w - 10.0;
        let rm = Rect::new(right - 22.0, row.y + 6.0, 22.0, 22.0);
        let plus = Rect::new(rm.x - 8.0 - 28.0, row.y + 3.0, 28.0, 28.0);
        let val = Rect::new(plus.x - 76.0, row.y, 76.0, row.h);
        let minus = Rect::new(val.x - 28.0, row.y + 3.0, 28.0, 28.0);
        for (r, label, h) in [(minus, "\u{E738}", Hit::Minus(i)), (plus, "\u{E710}", Hit::Plus(i))] {
            let c = if usable && hover == Some(h) { t.ctrl_hover.mix(t.ctrl, 0.3) } else { t.ctrl };
            g.fill(r, 6.0, c);
            g.stroke(r, 6.0, 1.0, t.ctrl_stroke);
            g.text(label, r, &g.fonts.glyph, fg);
            if usable {
                hits.push((r, h));
            }
        }
        g.text(&format!("{value}"), val, &g.fonts.value, fg);
        if usable && count > 1 && row_hover {
            if hover == Some(Hit::Remove(i)) {
                g.fill(rm, 11.0, t.ctrl_hover);
            }
            g.text("\u{E711}", rm, &g.fonts.glyph, t.text2);
        }
        if usable {
            if count > 1 {
                hits.push((rm, Hit::Remove(i)));
            }
            // Row last, so the buttons above win the hit test.
            hits.push((row, Hit::Stage(i)));
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp & 0xFFFF) as i16 as f32;
        let y = ((lp >> 16) & 0xFFFF) as i16 as f32;
        let s = 96.0 / self.dpi as f32;
        (x * s, y * s)
    }

    fn edit_stages(&mut self, f: impl FnOnce(&mut Vec<u16>)) {
        let mut p = self.pending.clone().unwrap_or_else(|| self.view.stages.clone());
        f(&mut p);
        self.pending = if p == self.view.stages { None } else { Some(p) };
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }
}

trait Shift {
    fn shift(self, dx: f32) -> Rect;
}

impl Shift for Rect {
    fn shift(self, dx: f32) -> Rect {
        Rect::new(self.x + dx, self.y, self.w - dx, self.h)
    }
}

// ---------- input ----------

/// Run a click. Local edits stay in the window; mouse settings go through
/// the app (which then calls `sync`).
fn activate(hit: Hit) {
    let v = with_ui(|u| u.view.clone()).unwrap_or_default();
    match hit {
        Hit::BatType(i) => drop(with_app(|a| a.act(|a| a.set_battery_choice(i)))),
        Hit::Step => drop(with_app(|a| a.act(|a| a.set_liion_step(!v.liion_step)))),
        Hit::Lowbat(i) => drop(with_app(|a| a.act(|a| a.set_lowbat(razer::LOW_BATTERY_CHOICES[i])))),
        Hit::Poll(i) => drop(with_app(|a| a.act(|a| a.set_poll(razer::POLL_RATES[i])))),
        Hit::Idle(i) => drop(with_app(|a| a.act(|a| a.set_idle(IDLE_CHOICES[i])))),
        Hit::TrayIcon(i) => drop(with_app(|a| a.set_tray_icon(TrayIcon::ALL[i]))),
        Hit::Stage(i) => {
            // Unapplied edits would be lost by a stage switch; apply them first.
            let pending = with_ui(|u| u.pending.clone()).flatten();
            match pending {
                Some(p) => drop(with_app(|a| a.apply_stages(p))),
                None => {}
            }
            with_app(|a| a.act(|a| a.select_stage(i)));
        }
        Hit::Minus(i) => drop(with_ui(|u| u.edit_stages(|s| s[i] = bump(s[i], false)))),
        Hit::Plus(i) => drop(with_ui(|u| u.edit_stages(|s| s[i] = bump(s[i], true)))),
        Hit::Remove(i) => drop(with_ui(|u| {
            u.edit_stages(|s| {
                if s.len() > 1 {
                    s.remove(i);
                }
            })
        })),
        Hit::AddStage => drop(with_ui(|u| {
            u.edit_stages(|s| {
                if s.len() < 5 {
                    let next = s.last().map_or(800, |&l| l.saturating_mul(2)).clamp(razer::DPI_MIN, razer::DPI_MAX);
                    s.push(next);
                }
            })
        })),
        Hit::Apply => {
            if let Some(p) = with_ui(|u| u.pending.clone()).flatten() {
                with_app(|a| a.apply_stages(p));
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
    }
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
        WM_SIZE => {
            let (cw, ch) = ((lp & 0xFFFF) as u32, ((lp >> 16) & 0xFFFF) as u32);
            with_ui(|u| {
                if let Some(g) = &u.gfx {
                    let _ = g.rt.Resize(&D2D_SIZE_U { width: cw, height: ch });
                }
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
            SetWindowPos(hwnd, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
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
            let over = with_ui(|u| u.hover.is_some()).unwrap_or(false);
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
            // Scroll over a stage row to nudge its DPI.
            let delta = ((wp >> 16) & 0xFFFF) as i16;
            let mut pt = POINT { x: (lp & 0xFFFF) as i16 as i32, y: ((lp >> 16) & 0xFFFF) as i16 as i32 };
            ScreenToClient(hwnd, &mut pt);
            with_ui(|u| {
                let s = 96.0 / u.dpi as f32;
                if let Some(Hit::Stage(i) | Hit::Minus(i) | Hit::Plus(i) | Hit::Remove(i)) = u.hit_at(pt.x as f32 * s, pt.y as f32 * s) {
                    u.edit_stages(|st| st[i] = bump(st[i], delta > 0));
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
