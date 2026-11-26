//! On-screen popup for DPI and power mode changes: a small rounded panel
//! near the bottom of the screen that fades out after a moment.
//!
//! Drawn with plain GDI (no Direct2D) so the tray stays small; the window
//! is created on first use and reused. It never takes focus and clicks go
//! through it.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::w;

const CLASS: &str = "RaisinOsd";
const TIMER_HOLD: usize = 1;
const TIMER_FADE: usize = 2;
const HOLD_MS: u32 = 1100;
const FADE_STEP_MS: u32 = 15;
const OPAQUE: u8 = 242;

// Layout, in DIPs.
const W: i32 = 200;
const H: i32 = 76;
const BOTTOM_GAP: i32 = 64;

/// What the popup shows.
#[derive(Clone, PartialEq)]
pub struct Content {
    /// Big line, e.g. "1600 DPI".
    pub title: String,
    /// Small line under it, e.g. "125 Hz  ·  sleeps after 1 min".
    pub detail: String,
    /// Stage dots: (active 1-based, total).
    pub dots: Option<(usize, usize)>,
}

struct Osd {
    hwnd: HWND,
    content: Content,
    alpha: u8,
    dpi: u32,
    big: HFONT,
    small: HFONT,
}

thread_local! {
    static OSD: RefCell<Option<Osd>> = const { RefCell::new(None) };
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

unsafe fn font(size: i32, weight: i32, face: &str) -> HFONT {
    CreateFontW(
        -size,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        w(face).as_ptr(),
    )
}

/// Show `content` (or refresh it if already showing) and restart the timer.
pub fn show(content: Content) {
    unsafe {
        let hwnd = window();
        if hwnd.is_null() {
            return;
        }
        // The monitor under the pointer.
        let mut pt: POINT = zeroed();
        GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        windows_sys::Win32::UI::HiDpi::GetDpiForMonitor(
            mon,
            windows_sys::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
            &mut dx,
            &mut dy,
        );
        let dpi = dx.max(96);
        let s = |v: i32| v * dpi as i32 / 96;
        let work = mi.rcWork;
        let (ww, wh) = (s(W), s(H));
        let x = (work.left + work.right - ww) / 2;
        let y = work.bottom - wh - s(BOTTOM_GAP);

        OSD.with(|o| {
            let mut o = o.borrow_mut();
            let Some(o) = o.as_mut() else { return };
            if o.dpi != dpi {
                DeleteObject(o.big);
                DeleteObject(o.small);
                o.big = font(s(26), FW_SEMIBOLD as i32, "Segoe UI Variable Display");
                o.small = font(s(12), FW_NORMAL as i32, "Segoe UI Variable Text");
                o.dpi = dpi;
            }
            o.content = content;
            o.alpha = OPAQUE;
        });
        KillTimer(hwnd, TIMER_FADE);
        SetLayeredWindowAttributes(hwnd, 0, OPAQUE, LWA_ALPHA);
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, ww, wh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        InvalidateRect(hwnd, null(), 0);
        SetTimer(hwnd, TIMER_HOLD, HOLD_MS, None);
    }
}

/// The popup window, created on first use.
unsafe fn window() -> HWND {
    if let Some(h) = OSD.with(|o| o.borrow().as_ref().map(|o| o.hwnd)) {
        return h;
    }
    let hinst = GetModuleHandleW(null());
    let class = w(CLASS);
    let wc = WNDCLASSW {
        style: CS_DROPSHADOW,
        lpfnWndProc: Some(proc_),
        hInstance: hinst,
        lpszClassName: class.as_ptr(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        ..zeroed()
    };
    RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        class.as_ptr(),
        w("Raisin").as_ptr(),
        WS_POPUP,
        0,
        0,
        W,
        H,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if hwnd.is_null() {
        return hwnd;
    }
    let round = DWMWCP_ROUND;
    DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
        &round as *const _ as *const _,
        size_of::<i32>() as u32,
    );
    OSD.with(|o| {
        *o.borrow_mut() = Some(Osd {
            hwnd,
            content: Content {
                title: String::new(),
                detail: String::new(),
                dots: None,
            },
            alpha: 0,
            dpi: 0,
            big: null_mut(),
            small: null_mut(),
        })
    });
    hwnd
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    OSD.with(|o| {
        let o = o.borrow();
        let Some(o) = o.as_ref() else { return };
        let mut rc: RECT = zeroed();
        GetClientRect(hwnd, &mut rc);
        let s = |v: i32| v * o.dpi as i32 / 96;
        let dark = super::settings::system_dark();
        let (bg, fg, fg2, dim) = if dark {
            (rgb(32, 32, 32), rgb(255, 255, 255), rgb(170, 170, 170), rgb(80, 80, 80))
        } else {
            (rgb(249, 249, 249), rgb(26, 26, 26), rgb(96, 96, 96), rgb(200, 200, 200))
        };
        let accent = super::settings::accent_rgb(dark);

        // Draw off-screen, then copy: no flicker.
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
        let old_bmp = SelectObject(mem, bmp);
        let brush = CreateSolidBrush(bg);
        FillRect(mem, &rc, brush);
        DeleteObject(brush);
        SetBkMode(mem, TRANSPARENT as i32);

        let old_font = SelectObject(mem, o.big);
        SetTextColor(mem, fg);
        let mut tr = RECT {
            left: 0,
            top: s(10),
            right: rc.right,
            bottom: s(46),
        };
        let title: Vec<u16> = o.content.title.encode_utf16().collect();
        DrawTextW(
            mem,
            title.as_ptr(),
            title.len() as i32,
            &mut tr,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );

        if let Some((active, total)) = o.content.dots {
            // Stage dots: the active one in the accent color.
            let d = s(8);
            let gap = s(7);
            let row = total as i32 * d + (total as i32 - 1) * gap;
            let mut x = (rc.right - row) / 2;
            let y = s(54);
            SelectObject(mem, GetStockObject(NULL_PEN));
            for i in 1..=total {
                let b = CreateSolidBrush(if i == active { accent } else { dim });
                let ob = SelectObject(mem, b);
                Ellipse(mem, x, y, x + d + 1, y + d + 1);
                SelectObject(mem, ob);
                DeleteObject(b);
                x += d + gap;
            }
        } else if !o.content.detail.is_empty() {
            SelectObject(mem, o.small);
            SetTextColor(mem, fg2);
            let mut dr = RECT {
                left: 0,
                top: s(46),
                right: rc.right,
                bottom: s(66),
            };
            let detail: Vec<u16> = o.content.detail.encode_utf16().collect();
            DrawTextW(
                mem,
                detail.as_ptr(),
                detail.len() as i32,
                &mut dr,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER,
            );
        }

        BitBlt(hdc, 0, 0, rc.right, rc.bottom, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old_font);
        SelectObject(mem, old_bmp);
        DeleteObject(bmp);
        DeleteDC(mem);
    });
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_TIMER if wp == TIMER_HOLD => {
            KillTimer(hwnd, TIMER_HOLD);
            SetTimer(hwnd, TIMER_FADE, FADE_STEP_MS, None);
            0
        }
        WM_TIMER if wp == TIMER_FADE => {
            let a = OSD.with(|o| {
                let mut o = o.borrow_mut();
                let o = o.as_mut()?;
                o.alpha = o.alpha.saturating_sub(24);
                Some(o.alpha)
            });
            match a {
                Some(a) if a > 0 => {
                    SetLayeredWindowAttributes(hwnd, 0, a, LWA_ALPHA);
                }
                _ => {
                    KillTimer(hwnd, TIMER_FADE);
                    ShowWindow(hwnd, SW_HIDE);
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
