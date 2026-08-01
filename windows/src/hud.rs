// On-screen recording indicator — the Windows take on the macOS HUD panel
// (HUDController.swift): a small rounded dark panel at the bottom-center of
// the work area with a 5-bar equalizer. Red bars + "Listening…" while
// recording (bar heights driven by the live mic level), yellow bars +
// "Transcribing…" while whisper runs.
//
// The window lives on the main (message-loop) thread; the controller thread
// changes its state with `set_state`, which posts a message — safe from any
// thread.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
    CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect,
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TextOutW,
    PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, KillTimer, RegisterClassW, SetTimer, SetWindowPos,
    ShowWindow, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA, SWP_NOACTIVATE,
    SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_APP,
    WM_ERASEBKGND, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::Win32::UI::WindowsAndMessaging::{SetLayeredWindowAttributes, LWA_ALPHA};

const HUD_W: i32 = 220;
const HUD_H: i32 = 64;
const CORNER: i32 = 18;
const WM_HUD_STATE: u32 = WM_APP + 10;
const TIMER_ID: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HudState {
    Hidden = 0,
    Recording = 1,
    Transcribing = 2,
}

static HUD_HWND: AtomicIsize = AtomicIsize::new(0);
static MODE: AtomicIsize = AtomicIsize::new(0);
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Change the HUD state from any thread.
pub fn set_state(state: HudState) {
    let hwnd = HUD_HWND.load(Ordering::Relaxed);
    if hwnd != 0 {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                HWND(hwnd as _),
                WM_HUD_STATE,
                WPARAM(state as usize),
                LPARAM(0),
            );
        }
    }
}

/// Create the (hidden) HUD window. Call once from the main thread.
pub fn create() -> anyhow::Result<()> {
    let _ = EPOCH.set(Instant::now());
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = crate::wide("FastWordHud");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(hud_proc),
            hInstance: instance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(anyhow::anyhow!("RegisterClassW(hud) failed"));
        }
        let title = crate::wide("FastWord HUD");
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            HUD_W,
            HUD_H,
            None,
            None,
            instance,
            None,
        )?;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 242, LWA_ALPHA);
        // Rounded corners; the system takes ownership of the region.
        let rgn = CreateRoundRectRgn(0, 0, HUD_W + 1, HUD_H + 1, CORNER, CORNER);
        SetWindowRgn(hwnd, rgn, true);
        HUD_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    }
    Ok(())
}

fn phase() -> f64 {
    EPOCH.get().map(|e| e.elapsed().as_secs_f64()).unwrap_or(0.0)
}

fn label(mode: HudState) -> &'static str {
    let ru = crate::languages::system_language() == Some("ru");
    match (mode, ru) {
        (HudState::Recording, true) => "Слушаю…",
        (HudState::Recording, false) => "Listening…",
        (HudState::Transcribing, true) => "Распознаю…",
        (HudState::Transcribing, false) => "Transcribing…",
        _ => "",
    }
}

unsafe extern "system" fn hud_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HUD_STATE => {
            let new_mode = wparam.0 as isize;
            let old_mode = MODE.swap(new_mode, Ordering::Relaxed);
            if new_mode == HudState::Hidden as isize {
                let _ = KillTimer(hwnd, TIMER_ID);
                let _ = ShowWindow(hwnd, SW_HIDE);
            } else {
                if old_mode == HudState::Hidden as isize {
                    position_bottom_center(hwnd);
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    SetTimer(hwnd, TIMER_ID, 33, None);
                }
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // double-buffered paint, no bg erase
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn position_bottom_center(hwnd: HWND) {
    let mut work = RECT::default();
    let _ = SystemParametersInfoW(
        SPI_GETWORKAREA,
        0,
        Some(&mut work as *mut RECT as *mut _),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    );
    // Mirrors the macOS HUD: centered, 60px above the bottom of the work area.
    let x = work.left + (work.right - work.left - HUD_W) / 2;
    let y = work.bottom - HUD_H - 60;
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
}

unsafe fn paint(hwnd: HWND) {
    let mode = match MODE.load(Ordering::Relaxed) {
        1 => HudState::Recording,
        2 => HudState::Transcribing,
        _ => HudState::Hidden,
    };

    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    // Off-screen buffer to avoid flicker.
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, HUD_W, HUD_H);
    let old_bmp = SelectObject(mem, bmp);

    // Background: near-black slate, like the mac panel's dark material.
    let bg = CreateSolidBrush(COLORREF(rgb(32, 32, 40)));
    let full = RECT { left: 0, top: 0, right: HUD_W, bottom: HUD_H };
    FillRect(mem, &full, bg);
    let _ = DeleteObject(bg);

    // Equalizer: 5 bars, 6px wide, 4px apart, heights 4..28 (mac values).
    let bar_color = match mode {
        HudState::Recording => rgb(235, 72, 60),   // red
        HudState::Transcribing => rgb(235, 190, 60), // yellow
        HudState::Hidden => rgb(128, 128, 128),
    };
    let bar_brush = CreateSolidBrush(COLORREF(bar_color));
    let level = crate::audio::current_level() as f64;
    let t = phase();
    const SEEDS: [f64; 5] = [0.13, 0.71, 0.37, 0.93, 0.52];
    let (min_h, max_h) = (4.0f64, 28.0f64);
    let eq_left = 20;
    let eq_mid = HUD_H / 2;
    for i in 0..5 {
        let h = if mode == HudState::Transcribing {
            // Indeterminate wave (same curve as the mac HUD).
            let v = ((t * 4.0 + i as f64 * 0.6).sin() + 1.0) / 2.0;
            min_h + v * (max_h - min_h) * 0.7
        } else {
            let osc = ((t * 6.0 + i as f64 * 0.9 + SEEDS[i] * 6.0).sin() + 1.0) / 2.0;
            min_h + level * (0.6 + 0.4 * osc) * (max_h - min_h)
        } as i32;
        let x = eq_left + i as i32 * 10;
        let bar = RECT {
            left: x,
            top: eq_mid - h / 2,
            right: x + 6,
            bottom: eq_mid + h / 2,
        };
        FillRect(mem, &bar, bar_brush);
    }
    let _ = DeleteObject(bar_brush);

    // Label.
    let face = crate::wide("Segoe UI");
    let font = CreateFontW(
        -15, // ~11pt at 96dpi, negative = char height
        0,
        0,
        0,
        600, // semibold
        0,
        0,
        0,
        1, // DEFAULT_CHARSET
        0,
        0,
        5, // CLEARTYPE_QUALITY
        0,
        PCWSTR(face.as_ptr()),
    );
    let old_font = SelectObject(mem, font);
    SetBkMode(mem, TRANSPARENT);
    SetTextColor(mem, COLORREF(rgb(235, 235, 240)));
    let text: Vec<u16> = label(mode).encode_utf16().collect();
    let _ = TextOutW(mem, 84, eq_mid - 10, &text);
    SelectObject(mem, old_font);
    let _ = DeleteObject(font);

    let _ = BitBlt(hdc, 0, 0, HUD_W, HUD_H, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}
