// The FastWord window: one top-level window with three tabs — History,
// Settings, Models — mirroring the macOS HistoryView / SettingsView /
// ModelManagementView. Everything is plain Win32 (comctl32) and runs on
// the main thread's message loop; the window is created lazily and hidden
// (not destroyed) on close.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DrawTextW, EndPaint, FillRect,
    SelectObject, SetBkMode, SetTextColor, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    HBRUSH, HFONT, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, ICC_LISTVIEW_CLASSES, ICC_TAB_CLASSES,
    INITCOMMONCONTROLSEX, LVCOLUMNW, LVCF_TEXT, LVCF_WIDTH, LVIF_TEXT, LVITEMW,
    LVM_DELETEALLITEMS, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETBKCOLOR, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVM_SETTEXTBKCOLOR,
    LVM_SETTEXTCOLOR, LVNI_SELECTED, LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS,
    LVS_SINGLESEL, LVS_NOCOLUMNHEADER, WC_LISTVIEWW, DRAWITEMSTRUCT, ODS_SELECTED, ODS_DISABLED,
    LVN_ITEMCHANGED, NMHDR,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DrawIconEx, GetDlgItem, KillTimer, MessageBoxW,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetTimer, ShowWindow, BM_GETCHECK,
    BM_SETCHECK, BS_AUTOCHECKBOX, BS_OWNERDRAW, CBS_DROPDOWNLIST, CB_ADDSTRING,
    CB_GETCURSEL, CB_SETCURSEL, DI_NORMAL, EN_CHANGE, ES_AUTOHSCROLL,
    IDYES, MB_ICONQUESTION, MB_YESNO, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX,
    WM_CTLCOLORSTATIC, WM_DRAWITEM, WM_NOTIFY, WM_PAINT, WM_SETFONT, WM_TIMER, WNDCLASSW, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};

use crate::{config, history, languages, logging, models, wide};

const WIN_W: i32 = 736;
const WIN_H: i32 = 600;
const HEADER_H: i32 = 64;
const PAGE_TOP: i32 = HEADER_H + 12;

// Brand-dark theme (matches the HUD and the app icon).
const CLR_BG: u32 = rgb(22, 22, 31);
const CLR_HEADER: u32 = rgb(13, 22, 48);
const CLR_INPUT: u32 = rgb(35, 35, 49);
const CLR_LIST_BG: u32 = rgb(27, 27, 38);
const CLR_BTN: u32 = rgb(43, 43, 58);
const CLR_ACCENT: u32 = rgb(78, 110, 232);
const CLR_TEXT: u32 = rgb(233, 234, 242);
const CLR_TEXT_DIM: u32 = rgb(154, 161, 184);

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

/// Buttons drawn as accent-colored (primary actions); the rest are dark grey.
const ACCENT_BUTTON_IDS: &[i32] = &[220 /* save */, 302 /* download */, 304 /* use */];

// Header navigation "pills" replacing the old tab control.
const ID_NAV_BASE: i32 = 400; // 400..=402
static CURRENT_TAB: AtomicUsize = AtomicUsize::new(0);

// Control ids.
const ID_H_SEARCH: i32 = 101;
const ID_H_LIST: i32 = 102;
const ID_H_COPY: i32 = 103;
const ID_H_DELETE: i32 = 104;
const ID_H_CLEAR: i32 = 105;
const ID_H_COL0: i32 = 120;
const ID_H_COL1: i32 = 121;
const ID_H_SEARCHLBL: i32 = 122;
const ID_S_HOTKEY: i32 = 201;
const ID_S_SECONDARY: i32 = 202;
const ID_S_COMBO: i32 = 203;
const ID_S_LANGUAGE: i32 = 204;
const ID_S_IDLE: i32 = 205;
const ID_S_UILANG: i32 = 206;
const ID_S_SKIP: i32 = 210;
const ID_S_CLEANUP: i32 = 211;
const ID_S_HUD: i32 = 212;
const ID_S_BEEPS: i32 = 213;
const ID_S_AUTOSTART: i32 = 214;
const ID_S_SAVE: i32 = 220;
const ID_S_LABELS: i32 = 230; // 230..=234 static labels
const ID_M_LIST: i32 = 301;
const ID_M_DOWNLOAD: i32 = 302;
const ID_M_CANCEL: i32 = 303;
const ID_M_USE: i32 = 304;
const ID_M_DELFILE: i32 = 305;
const ID_M_COL0: i32 = 320;
const ID_M_COL1: i32 = 321;
const ID_M_COL2: i32 = 322;

const TIMER_MODELS: usize = 2;

pub const TAB_HISTORY: usize = 0;
pub const TAB_MODELS: usize = 1;
pub const TAB_SETTINGS: usize = 2;

/// Options offered in the hotkey combos, in display order.
const HOTKEYS: &[&str] = &[
    "right_ctrl", "left_ctrl", "right_alt", "left_alt", "right_shift", "left_shift",
    "caps_lock", "scroll_lock", "pause", "f13", "f14", "f15", "f16",
];
const IDLE_SECS: &[u64] = &[60, 600, 1800, 3600, 999_999_999];

struct UiState {
    hwnd: HWND,
    /// Kept alive for the lifetime of the controls that use it.
    #[allow(dead_code)]
    font: HFONT,
    #[allow(dead_code)]
    title_font: HFONT,
    // Theme brushes, created once and reused in WM_CTLCOLOR* handlers.
    br_bg: HBRUSH,
    br_input: HBRUSH,
    /// (id, text, formatted local time) of currently displayed history rows.
    history_rows: Vec<(String, String, String)>,
}
unsafe impl Send for UiState {}

static STATE: Mutex<Option<UiState>> = Mutex::new(None);

fn hwnd() -> Option<HWND> {
    STATE.lock().unwrap().as_ref().map(|s| s.hwnd)
}

/// Open the window on the given tab (creating it on first use).
pub fn open(tab: usize) {
    if hwnd().is_none() {
        if let Err(e) = create_window() {
            logging::log(&format!("main window failed: {e:#}"));
            return;
        }
    }
    let Some(hwnd) = hwnd() else { return };
    unsafe {
        CURRENT_TAB.store(tab, Ordering::Relaxed);
        refresh_history(hwnd);
        load_settings_into_controls(hwnd);
        refresh_models(hwnd);
        update_history_buttons(hwnd);
        update_model_buttons(hwnd);
        show_page(hwnd, tab);
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(hwnd, TIMER_MODELS, 500, None);
    }
}

/// GDI+ startup token (GDI has no anti-aliasing; the rounded buttons are
/// drawn through GDI+ instead).
static GDIPLUS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe fn ensure_gdiplus() {
    GDIPLUS.get_or_init(|| {
        use windows::Win32::Graphics::GdiPlus::{GdiplusStartup, GdiplusStartupInput};
        let mut token = 0usize;
        let input = GdiplusStartupInput { GdiplusVersion: 1, ..Default::default() };
        let _ = GdiplusStartup(&mut token, &input, std::ptr::null_mut());
        token
    });
}

/// Anti-aliased rounded-rectangle fill via GDI+ (color is a GDI COLORREF).
unsafe fn fill_rounded_aa(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    r: &RECT,
    colorref: u32,
    radius: f32,
) {
    use windows::Win32::Graphics::GdiPlus::*;
    ensure_gdiplus();

    let mut graphics: *mut GpGraphics = std::ptr::null_mut();
    if GdipCreateFromHDC(hdc, &mut graphics) != Status(0) {
        return;
    }
    let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);

    // COLORREF is 0x00BBGGRR; GDI+ wants 0xAARRGGBB.
    let (rr, gg, bb) = (colorref & 0xFF, (colorref >> 8) & 0xFF, (colorref >> 16) & 0xFF);
    let argb = 0xFF00_0000u32 | (rr << 16) | (gg << 8) | bb;

    let mut path: *mut GpPath = std::ptr::null_mut();
    let _ = GdipCreatePath(FillModeAlternate, &mut path);
    let (x, y) = (r.left as f32, r.top as f32);
    let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
    let d = (radius * 2.0).min(w).min(h);
    // Inset by half a pixel so the AA edge isn't clipped by the control rect.
    let (x, y, w, h) = (x + 0.5, y + 0.5, w - 1.0, h - 1.0);
    let _ = GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
    let _ = GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
    let _ = GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
    let _ = GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
    let _ = GdipClosePathFigure(path);

    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    let _ = GdipCreateSolidFill(argb, &mut brush);
    let _ = GdipFillPath(graphics, brush as *mut GpBrush, path);
    let _ = GdipDeleteBrush(brush as *mut GpBrush);
    let _ = GdipDeletePath(path);
    let _ = GdipDeleteGraphics(graphics);
}

/// Opt the process into the OS dark mode for common controls (undocumented
/// uxtheme ordinal 135, `SetPreferredAppMode(ForceDark)` — the same call
/// every dark-mode Win32 app makes; present since Windows 10 1809). Without
/// it, `DarkMode_*` control themes render their text in light-mode colors.
unsafe fn enable_app_dark_mode() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    let Ok(lib) = LoadLibraryW(PCWSTR(wide("uxtheme.dll").as_ptr())) else { return };
    let ordinal_135 = windows::core::PCSTR(135usize as *const u8); // by-ordinal lookup
    if let Some(f) = GetProcAddress(lib, ordinal_135) {
        // fn SetPreferredAppMode(mode: i32) -> i32; 2 = ForceDark
        let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
        set_mode(2);
    }
}

fn create_window() -> anyhow::Result<()> {
    unsafe {
        enable_app_dark_mode();
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_TAB_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);

        let instance = GetModuleHandleW(None)?;
        let class_name = wide("FastWordMainWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hIcon: crate::app_icon(),
            hbrBackground: CreateSolidBrush(COLORREF(CLR_BG)),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let title = wide("FastWord");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            120,
            120,
            WIN_W,
            WIN_H,
            None,
            None,
            instance,
            None,
        )?;

        // Dark titlebar (Windows 10 1809+; harmless elsewhere).
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );

        let font = CreateFontW(
            -15, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0,
            PCWSTR(wide("Segoe UI").as_ptr()),
        );
        let title_font = CreateFontW(
            -24, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0,
            PCWSTR(wide("Segoe UI").as_ptr()),
        );

        build_controls(hwnd, font);

        *STATE.lock().unwrap() = Some(UiState {
            hwnd,
            font,
            title_font,
            br_bg: CreateSolidBrush(COLORREF(CLR_BG)),
            br_input: CreateSolidBrush(COLORREF(CLR_INPUT)),
            history_rows: Vec::new(),
        });
        Ok(())
    }
}

unsafe fn make(
    hwnd: HWND,
    font: HFONT,
    class: PCWSTR,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: i32,
) -> HWND {
    let text_w = wide(text);
    let ctl = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(text_w.as_ptr()),
        WINDOW_STYLE(style) | WS_CHILD,
        x,
        y,
        w,
        h,
        hwnd,
        windows::Win32::UI::WindowsAndMessaging::HMENU(id as isize as *mut _),
        GetModuleHandleW(None).unwrap_or_default(),
        None,
    )
    .unwrap_or_default();
    SendMessageW(ctl, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    ctl
}

unsafe fn build_controls(hwnd: HWND, font: HFONT) {
    let ui = languages::ui();
    // Keep the class-name buffers alive for the whole function — a bare
    // `PCWSTR(wide(...).as_ptr())` would dangle after its statement.
    let stc_s = wide("STATIC");
    let btn_s = wide("BUTTON");
    let edt_s = wide("EDIT");
    let cmb_s = wide("COMBOBOX");
    let stc = PCWSTR(stc_s.as_ptr());
    let btn = PCWSTR(btn_s.as_ptr());
    let edt = PCWSTR(edt_s.as_ptr());
    let cmb = PCWSTR(cmb_s.as_ptr());

    // Header navigation pills (owner-drawn) instead of a classic tab strip.
    let nav_labels = [ui.tab_history, ui.tab_models, ui.tab_settings];
    let mut nx = 190;
    for (i, label) in nav_labels.iter().enumerate() {
        let w = 40 + 11 * label.chars().count() as i32;
        make(
            hwnd, font, PCWSTR(btn_s.as_ptr()), label,
            WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32,
            nx, 15, w, 34, ID_NAV_BASE + i as i32,
        );
        nx += w + 8;
    }

    // --- History page ------------------------------------------------------
    make(hwnd, font, stc, ui.search_label, 0, 16, PAGE_TOP + 14, 70, 20, ID_H_SEARCHLBL);
    make(hwnd, font, edt, "", WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32, 90, PAGE_TOP + 10, 340, 24, ID_H_SEARCH);
    make(hwnd, font, stc, ui.col_time, 0, 24, PAGE_TOP + 44, 140, 18, ID_H_COL0);
    make(hwnd, font, stc, ui.col_text, 0, 24 + 150, PAGE_TOP + 44, 300, 18, ID_H_COL1);
    let lv = make(
        hwnd, font, WC_LISTVIEWW, "",
        WS_BORDER.0 | WS_TABSTOP.0 | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER) as u32,
        16, PAGE_TOP + 66, WIN_W - 48, 358, ID_H_LIST,
    );
    SendMessageW(lv, LVM_SETEXTENDEDLISTVIEWSTYLE, WPARAM(0), LPARAM(LVS_EX_FULLROWSELECT as isize));
    add_column(lv, 0, ui.col_time, 150);
    add_column(lv, 1, ui.col_text, WIN_W - 48 - 150 - 24);
    make(hwnd, font, btn, ui.btn_copy, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 16, PAGE_TOP + 434, 130, 28, ID_H_COPY);
    make(hwnd, font, btn, ui.btn_delete, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 154, PAGE_TOP + 434, 130, 28, ID_H_DELETE);
    make(hwnd, font, btn, ui.btn_clear_all, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 292, PAGE_TOP + 434, 130, 28, ID_H_CLEAR);

    // --- Settings page -----------------------------------------------------
    let labels = [
        ui.lbl_hotkey, ui.lbl_secondary, ui.lbl_combo, ui.lbl_language, ui.lbl_idle,
        ui.lbl_ui_language,
    ];
    let combo_ids = [ID_S_HOTKEY, ID_S_SECONDARY, ID_S_COMBO, ID_S_LANGUAGE, ID_S_IDLE, ID_S_UILANG];
    for (i, (label, id)) in labels.iter().zip(combo_ids).enumerate() {
        let y = PAGE_TOP + 20 + i as i32 * 36;
        make(hwnd, font, stc, label, 0, 16, y + 4, 200, 20, ID_S_LABELS + i as i32);
        make(hwnd, font, cmb, "", WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32, 230, y, 240, 300, id);
    }
    let checks = [
        (ui.chk_skip_empty, ID_S_SKIP),
        (ui.chk_cleanup, ID_S_CLEANUP),
        (ui.chk_hud, ID_S_HUD),
        (ui.chk_beeps, ID_S_BEEPS),
        (ui.chk_autostart, ID_S_AUTOSTART),
    ];
    for (i, (label, id)) in checks.iter().enumerate() {
        let y = PAGE_TOP + 246 + i as i32 * 30;
        make(hwnd, font, btn, label, WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32, 16, y, 400, 24, *id);
    }
    make(hwnd, font, btn, ui.btn_save, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 16, PAGE_TOP + 404, 140, 30, ID_S_SAVE);

    // Populate fixed combo contents.
    let add = |id: i32, s: &str| {
        let w = wide(s);
        SendMessageW(GetDlgItem(hwnd, id).unwrap_or_default(), CB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
    };
    for k in HOTKEYS {
        add(ID_S_HOTKEY, k);
    }
    add(ID_S_SECONDARY, ui.item_none);
    for k in HOTKEYS {
        add(ID_S_SECONDARY, k);
    }
    add(ID_S_COMBO, ui.combo_either);
    add(ID_S_COMBO, ui.combo_both);
    add(ID_S_LANGUAGE, ui.lang_system);
    add(ID_S_LANGUAGE, ui.lang_auto);
    for code in languages::SUPPORTED {
        add(ID_S_LANGUAGE, code);
    }
    for label in [ui.idle_1m, ui.idle_10m, ui.idle_30m, ui.idle_1h, ui.idle_never] {
        add(ID_S_IDLE, label);
    }
    for label in [ui.ui_lang_system, "English", "Русский", "中文"] {
        add(ID_S_UILANG, label);
    }

    // --- Models page -------------------------------------------------------
    make(hwnd, font, stc, ui.col_model, 0, 24, PAGE_TOP + 10, 280, 18, ID_M_COL0);
    make(hwnd, font, stc, ui.col_size, 0, 24 + 300, PAGE_TOP + 10, 84, 18, ID_M_COL1);
    make(hwnd, font, stc, ui.col_status, 0, 24 + 300 + 90, PAGE_TOP + 10, 200, 18, ID_M_COL2);
    let mlv = make(
        hwnd, font, WC_LISTVIEWW, "",
        WS_BORDER.0 | WS_TABSTOP.0 | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER) as u32,
        16, PAGE_TOP + 32, WIN_W - 48, 392, ID_M_LIST,
    );
    SendMessageW(mlv, LVM_SETEXTENDEDLISTVIEWSTYLE, WPARAM(0), LPARAM(LVS_EX_FULLROWSELECT as isize));
    add_column(mlv, 0, ui.col_model, 300);
    add_column(mlv, 1, ui.col_size, 90);
    add_column(mlv, 2, ui.col_status, 240);
    make(hwnd, font, btn, ui.btn_download, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 16, PAGE_TOP + 434, 130, 28, ID_M_DOWNLOAD);
    make(hwnd, font, btn, ui.btn_cancel, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 154, PAGE_TOP + 434, 130, 28, ID_M_CANCEL);
    make(hwnd, font, btn, ui.btn_use, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 292, PAGE_TOP + 434, 130, 28, ID_M_USE);
    make(hwnd, font, btn, ui.btn_delete_file, WS_TABSTOP.0 | BS_OWNERDRAW as u32, 430, PAGE_TOP + 434, 130, 28, ID_M_DELFILE);

    // --- Dark theming for the system controls -----------------------------
    // Explorer's dark theme skins scrollbars/headers; explicit colors cover
    // the client areas. Combos/edits get the dark "Common File Dialog" skin.
    let dark_explorer = wide("DarkMode_Explorer");
    let dark_cfd = wide("DarkMode_CFD");
    let dark_items = wide("DarkMode_ItemsView");
    for lv in [
        GetDlgItem(hwnd, ID_H_LIST).unwrap_or_default(),
        GetDlgItem(hwnd, ID_M_LIST).unwrap_or_default(),
    ] {
        let _ = SetWindowTheme(lv, PCWSTR(dark_explorer.as_ptr()), PCWSTR::null());
        SendMessageW(lv, LVM_SETBKCOLOR, WPARAM(0), LPARAM(CLR_LIST_BG as isize));
        SendMessageW(lv, LVM_SETTEXTBKCOLOR, WPARAM(0), LPARAM(CLR_LIST_BG as isize));
        SendMessageW(lv, LVM_SETTEXTCOLOR, WPARAM(0), LPARAM(CLR_TEXT as isize));
        // The column header is a child SysHeader32 with its own theme.
        let header = HWND(
            SendMessageW(lv, windows::Win32::UI::Controls::LVM_GETHEADER, WPARAM(0), LPARAM(0)).0
                as *mut _,
        );
        let _ = SetWindowTheme(header, PCWSTR(dark_items.as_ptr()), PCWSTR::null());
    }
    for id in [ID_H_SEARCH, ID_S_HOTKEY, ID_S_SECONDARY, ID_S_COMBO, ID_S_LANGUAGE, ID_S_IDLE, ID_S_UILANG] {
        let ctl = GetDlgItem(hwnd, id).unwrap_or_default();
        let _ = SetWindowTheme(ctl, PCWSTR(dark_cfd.as_ptr()), PCWSTR::null());
    }
    // Checkboxes: visual styles paint their own (light-theme) text and
    // ignore WM_CTLCOLORSTATIC, so drop the theme — the classic glyph plus
    // our colors reads fine on the dark background.
    let empty = wide("");
    for id in [ID_S_SKIP, ID_S_CLEANUP, ID_S_HUD, ID_S_BEEPS, ID_S_AUTOSTART] {
        let ctl = GetDlgItem(hwnd, id).unwrap_or_default();
        let _ = SetWindowTheme(ctl, PCWSTR(empty.as_ptr()), PCWSTR(empty.as_ptr()));
    }
}

unsafe fn add_column(lv: HWND, index: usize, title: &str, width: i32) {
    let mut text = wide(title);
    let col = LVCOLUMNW {
        mask: LVCF_TEXT | LVCF_WIDTH,
        cx: width,
        pszText: windows::core::PWSTR(text.as_mut_ptr()),
        ..Default::default()
    };
    SendMessageW(lv, LVM_INSERTCOLUMNW, WPARAM(index), LPARAM(&col as *const _ as isize));
}

unsafe fn lv_set_row(lv: HWND, row: usize, cols: &[&str]) {
    let mut first = wide(cols[0]);
    let item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row as i32,
        pszText: windows::core::PWSTR(first.as_mut_ptr()),
        ..Default::default()
    };
    SendMessageW(lv, LVM_INSERTITEMW, WPARAM(0), LPARAM(&item as *const _ as isize));
    for (i, text) in cols.iter().enumerate().skip(1) {
        let mut t = wide(text);
        let sub = LVITEMW {
            mask: LVIF_TEXT,
            iItem: row as i32,
            iSubItem: i as i32,
            pszText: windows::core::PWSTR(t.as_mut_ptr()),
            ..Default::default()
        };
        SendMessageW(lv, LVM_SETITEMTEXTW, WPARAM(row), LPARAM(&sub as *const _ as isize));
    }
}

unsafe fn lv_selected(lv: HWND) -> Option<usize> {
    let idx = SendMessageW(lv, LVM_GETNEXTITEM, WPARAM(usize::MAX), LPARAM(LVNI_SELECTED as isize)).0;
    (idx >= 0).then_some(idx as usize)
}

unsafe fn show_page(hwnd: HWND, tab: usize) {
    let pages: [(usize, &[i32]); 3] = [
        (TAB_HISTORY, &[ID_H_SEARCH, ID_H_LIST, ID_H_COPY, ID_H_DELETE, ID_H_CLEAR, ID_H_SEARCHLBL, ID_H_COL0, ID_H_COL1]),
        (
            TAB_SETTINGS,
            &[
                ID_S_HOTKEY, ID_S_SECONDARY, ID_S_COMBO, ID_S_LANGUAGE, ID_S_IDLE, ID_S_UILANG,
                ID_S_SKIP, ID_S_CLEANUP, ID_S_HUD, ID_S_BEEPS, ID_S_AUTOSTART, ID_S_SAVE,
                ID_S_LABELS, ID_S_LABELS + 1, ID_S_LABELS + 2, ID_S_LABELS + 3, ID_S_LABELS + 4,
                ID_S_LABELS + 5,
            ],
        ),
        (TAB_MODELS, &[ID_M_LIST, ID_M_DOWNLOAD, ID_M_CANCEL, ID_M_USE, ID_M_DELFILE, ID_M_COL0, ID_M_COL1, ID_M_COL2]),
    ];
    for (page, ids) in pages {
        for id in ids {
            let ctl = GetDlgItem(hwnd, *id).unwrap_or_default();
            let _ = ShowWindow(ctl, if page == tab { SW_SHOW } else { SW_HIDE });
        }
    }
}

// ---------------------------------------------------------------------------
// History page

unsafe fn refresh_history(hwnd: HWND) {
    use windows::Win32::UI::Controls::{LVIS_SELECTED, LVM_ENSUREVISIBLE, LVM_GETTOPINDEX, LVM_SETITEMSTATE};

    let filter = get_text(GetDlgItem(hwnd, ID_H_SEARCH).unwrap_or_default()).to_lowercase();
    let rows = match history::HistoryStore::open_default().and_then(|s| s.load_all()) {
        Ok(rows) => rows,
        Err(e) => {
            logging::log(&format!("history load failed: {e:#}"));
            Vec::new()
        }
    };
    let lv = GetDlgItem(hwnd, ID_H_LIST).unwrap_or_default();
    // Rebuilding the list resets the scroll and selection; remember both so
    // deleting entries one by one doesn't throw the user back to the top.
    let old_top = SendMessageW(lv, LVM_GETTOPINDEX, WPARAM(0), LPARAM(0)).0.max(0) as usize;
    let old_sel = lv_selected(lv);

    SendMessageW(lv, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    let mut kept = Vec::new();
    for (id, text, ts) in rows {
        if !filter.is_empty() && !text.to_lowercase().contains(&filter) {
            continue;
        }
        let when = chrono::DateTime::from_timestamp(ts as i64, 0)
            .map(|utc| {
                utc.with_timezone(&chrono::Local)
                    .format("%d.%m.%Y %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        lv_set_row(lv, kept.len(), &[&when, &text.replace('\n', " ")]);
        kept.push((id, text, when));
    }

    if !kept.is_empty() {
        let last = kept.len() - 1;
        if let Some(sel) = old_sel {
            // After a delete the same visual position now holds the next row.
            let item = LVITEMW { state: LVIS_SELECTED, stateMask: LVIS_SELECTED, ..Default::default() };
            SendMessageW(lv, LVM_SETITEMSTATE, WPARAM(sel.min(last)), LPARAM(&item as *const _ as isize));
        }
        // Scroll to the bottom first, then minimally back up — that puts the
        // remembered row at the top of the view again.
        SendMessageW(lv, LVM_ENSUREVISIBLE, WPARAM(last), LPARAM(0));
        SendMessageW(lv, LVM_ENSUREVISIBLE, WPARAM(old_top.min(last)), LPARAM(0));
    }

    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.history_rows = kept;
    }
    update_history_buttons(hwnd);
}

unsafe fn get_text(ctl: HWND) -> String {
    let mut buf = [0u16; 1024];
    let n = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(ctl, &mut buf);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn history_selected(hwnd: HWND) -> Option<(String, String)> {
    let lv = GetDlgItem(hwnd, ID_H_LIST).unwrap_or_default();
    let idx = lv_selected(lv)?;
    STATE
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s.history_rows.get(idx).map(|(id, text, _)| (id.clone(), text.clone())))
}

// ---------------------------------------------------------------------------
// Settings page

unsafe fn load_settings_into_controls(hwnd: HWND) {
    let cfg = config::load_or_create().unwrap_or_default();
    let sel = |id: i32, index: usize| {
        SendMessageW(GetDlgItem(hwnd, id).unwrap_or_default(), CB_SETCURSEL, WPARAM(index), LPARAM(0));
    };
    sel(ID_S_HOTKEY, HOTKEYS.iter().position(|k| **k == cfg.hotkey).unwrap_or(0));
    sel(
        ID_S_SECONDARY,
        if cfg.hotkey_secondary.trim().is_empty() {
            0
        } else {
            HOTKEYS.iter().position(|k| **k == cfg.hotkey_secondary).map(|i| i + 1).unwrap_or(0)
        },
    );
    sel(ID_S_COMBO, if cfg.combo_mode == "both" { 1 } else { 0 });
    sel(
        ID_S_LANGUAGE,
        match cfg.language.trim() {
            "system" | "" => 0,
            "auto" => 1,
            code => languages::SUPPORTED.iter().position(|c| *c == code).map(|i| i + 2).unwrap_or(0),
        },
    );
    sel(ID_S_IDLE, IDLE_SECS.iter().position(|s| *s == cfg.idle_evict_secs).unwrap_or(1));
    sel(
        ID_S_UILANG,
        match cfg.ui_language.trim() {
            "en" => 1,
            "ru" => 2,
            "zh" => 3,
            _ => 0,
        },
    );
    let check = |id: i32, on: bool| {
        SendMessageW(GetDlgItem(hwnd, id).unwrap_or_default(), BM_SETCHECK, WPARAM(on as usize), LPARAM(0));
    };
    check(ID_S_SKIP, cfg.skip_empty);
    check(ID_S_CLEANUP, cfg.cleanup_enabled);
    check(ID_S_HUD, cfg.hud);
    check(ID_S_BEEPS, cfg.beeps);
    check(ID_S_AUTOSTART, crate::autostart::is_enabled());
}

unsafe fn save_settings(hwnd: HWND) {
    let cur = |id: i32| SendMessageW(GetDlgItem(hwnd, id).unwrap_or_default(), CB_GETCURSEL, WPARAM(0), LPARAM(0)).0.max(0) as usize;
    let checked = |id: i32| SendMessageW(GetDlgItem(hwnd, id).unwrap_or_default(), BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1;

    let mut cfg = config::load_or_create().unwrap_or_default();
    cfg.hotkey = HOTKEYS[cur(ID_S_HOTKEY).min(HOTKEYS.len() - 1)].to_string();
    let sec = cur(ID_S_SECONDARY);
    cfg.hotkey_secondary = if sec == 0 { String::new() } else { HOTKEYS[(sec - 1).min(HOTKEYS.len() - 1)].to_string() };
    cfg.combo_mode = if cur(ID_S_COMBO) == 1 { "both" } else { "either" }.to_string();
    cfg.language = match cur(ID_S_LANGUAGE) {
        0 => "system".to_string(),
        1 => "auto".to_string(),
        i => languages::SUPPORTED.get(i - 2).unwrap_or(&"system").to_string(),
    };
    cfg.idle_evict_secs = IDLE_SECS[cur(ID_S_IDLE).min(IDLE_SECS.len() - 1)];
    cfg.ui_language = match cur(ID_S_UILANG) {
        1 => "en",
        2 => "ru",
        3 => "zh",
        _ => "system",
    }
    .to_string();
    cfg.skip_empty = checked(ID_S_SKIP);
    cfg.cleanup_enabled = checked(ID_S_CLEANUP);
    cfg.hud = checked(ID_S_HUD);
    cfg.beeps = checked(ID_S_BEEPS);

    // Autostart lives in the registry, not the config file; applies
    // immediately, no restart needed.
    if let Err(e) = crate::autostart::set_enabled(checked(ID_S_AUTOSTART)) {
        logging::log(&format!("autostart update failed: {e:#}"));
    }

    match config::save(&cfg) {
        Ok(()) => {
            logging::log("settings saved from UI");
            prompt_restart(hwnd);
        }
        Err(e) => logging::log(&format!("settings save failed: {e:#}")),
    }
}

unsafe fn prompt_restart(hwnd: HWND) {
    let ui = languages::ui();
    let text = wide(ui.restart_prompt);
    let title = wide("FastWord");
    let clicked = MessageBoxW(hwnd, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_YESNO | MB_ICONQUESTION);
    if clicked == IDYES {
        crate::restart_app();
    }
}

// ---------------------------------------------------------------------------
// Models page

unsafe fn refresh_models(hwnd: HWND) {
    let ui = languages::ui();
    let cfg = config::load_or_create().unwrap_or_default();
    let active_path = cfg.resolved_model_path();
    let downloading = models::downloading_index();

    let lv = GetDlgItem(hwnd, ID_M_LIST).unwrap_or_default();
    let selected = lv_selected(lv);
    SendMessageW(lv, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    for (i, m) in models::CATALOG.iter().enumerate() {
        let status = if downloading == Some(i) {
            let pct = (models::progress_bytes() * 100 / m.size_bytes.max(1)).min(100);
            ui.status_downloading.replace("{pct}", &pct.to_string())
        } else if !cfg.uses_gigaam() && models::local_path(m) == active_path {
            ui.status_active.to_string()
        } else if models::is_downloaded(m) {
            ui.status_downloaded.to_string()
        } else {
            ui.status_not_downloaded.to_string()
        };
        lv_set_row(lv, i, &[m.display, &models::size_string(m.size_bytes), &status]);
    }
    // GigaAM — the extra directory-based engine row (Russian only).
    let g_status = if downloading == Some(models::GIGAAM_INDEX) {
        let pct = (models::progress_bytes() * 100 / models::GIGAAM_SIZE_BYTES).min(100);
        ui.status_downloading.replace("{pct}", &pct.to_string())
    } else if cfg.uses_gigaam() && models::gigaam_installed() {
        ui.status_active.to_string()
    } else if models::gigaam_installed() {
        ui.status_downloaded.to_string()
    } else {
        ui.status_not_downloaded.to_string()
    };
    lv_set_row(
        lv,
        models::GIGAAM_INDEX,
        &[models::GIGAAM_DISPLAY, &models::size_string(models::GIGAAM_SIZE_BYTES), &g_status],
    );
    if let Some(idx) = selected {
        use windows::Win32::UI::Controls::{LVIS_SELECTED, LVITEMW, LVM_SETITEMSTATE};
        let item = LVITEMW { state: LVIS_SELECTED, stateMask: LVIS_SELECTED, ..Default::default() };
        SendMessageW(lv, LVM_SETITEMSTATE, WPARAM(idx), LPARAM(&item as *const _ as isize));
    }
}

/// Enable/disable the model buttons to match the selected row's state:
/// download only what's absent, use/delete only what's present, cancel
/// only while a download runs. Selecting nothing disables the row actions.
unsafe fn update_model_buttons(hwnd: HWND) {
    let lv = GetDlgItem(hwnd, ID_M_LIST).unwrap_or_default();
    let cfg = config::load_or_create().unwrap_or_default();
    let downloading = models::downloading_index().is_some();

    let (downloaded, active) = match lv_selected(lv) {
        Some(idx) if idx == models::GIGAAM_INDEX => {
            let inst = models::gigaam_installed();
            (inst, inst && cfg.uses_gigaam())
        }
        Some(idx) => {
            let m = &models::CATALOG[idx];
            let inst = models::is_downloaded(m);
            (inst, inst && !cfg.uses_gigaam() && models::local_path(m) == cfg.resolved_model_path())
        }
        None => (false, false),
    };
    let has_sel = lv_selected(lv).is_some();

    let enable = |id: i32, on: bool| {
        let _ = EnableWindow(GetDlgItem(hwnd, id).unwrap_or_default(), on);
    };
    enable(ID_M_DOWNLOAD, has_sel && !downloaded && !downloading);
    enable(ID_M_CANCEL, downloading);
    enable(ID_M_USE, downloaded && !active);
    enable(ID_M_DELFILE, downloaded && !active);
}

/// Copy/Delete need a selected row; Clear-all needs a non-empty list.
unsafe fn update_history_buttons(hwnd: HWND) {
    let lv = GetDlgItem(hwnd, ID_H_LIST).unwrap_or_default();
    let has_sel = lv_selected(lv).is_some();
    let has_rows = STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| !s.history_rows.is_empty())
        .unwrap_or(false);
    let enable = |id: i32, on: bool| {
        let _ = EnableWindow(GetDlgItem(hwnd, id).unwrap_or_default(), on);
    };
    enable(ID_H_COPY, has_sel);
    enable(ID_H_DELETE, has_sel);
    enable(ID_H_CLEAR, has_rows);
}

unsafe fn model_action(hwnd: HWND, cmd: i32) {
    let lv = GetDlgItem(hwnd, ID_M_LIST).unwrap_or_default();
    match cmd {
        ID_M_CANCEL => models::cancel_download(),
        _ => {
            let Some(idx) = lv_selected(lv) else { return };
            if idx == models::GIGAAM_INDEX {
                gigaam_action(hwnd, cmd);
            } else {
                let m = &models::CATALOG[idx];
                match cmd {
                    ID_M_DOWNLOAD => {
                        if !models::is_downloaded(m) {
                            models::start_download(idx);
                        }
                    }
                    ID_M_USE => {
                        if models::is_downloaded(m) {
                            set_active_model(hwnd, &models::local_path(m).to_string_lossy(), m.filename);
                        }
                    }
                    ID_M_DELFILE => {
                        let cfg = config::load_or_create().unwrap_or_default();
                        if models::is_downloaded(m)
                            && (cfg.uses_gigaam() || models::local_path(m) != cfg.resolved_model_path())
                        {
                            let _ = std::fs::remove_file(models::local_path(m));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    refresh_models(hwnd);
}

unsafe fn gigaam_action(hwnd: HWND, cmd: i32) {
    match cmd {
        ID_M_DOWNLOAD => {
            if !models::gigaam_installed() {
                models::start_download(models::GIGAAM_INDEX);
            }
        }
        ID_M_USE => {
            if models::gigaam_installed() {
                set_active_model(hwnd, config::GIGAAM_SENTINEL, "gigaam");
            }
        }
        ID_M_DELFILE => {
            let cfg = config::load_or_create().unwrap_or_default();
            if !cfg.uses_gigaam() {
                models::delete_gigaam();
            }
        }
        _ => {}
    }
}

unsafe fn set_active_model(hwnd: HWND, model_path: &str, log_name: &str) {
    let mut cfg = config::load_or_create().unwrap_or_default();
    cfg.model_path = model_path.to_string();
    match config::save(&cfg) {
        Ok(()) => {
            logging::log(&format!("active model -> {log_name}"));
            prompt_restart(hwnd);
        }
        Err(e) => logging::log(&format!("model select failed: {e:#}")),
    }
}

// ---------------------------------------------------------------------------

/// Navy header band with the app icon, title and nav-pill background.
unsafe fn paint_header(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    let header = RECT { left: 0, top: 0, right: WIN_W, bottom: HEADER_H };
    let br = CreateSolidBrush(COLORREF(CLR_HEADER));
    FillRect(hdc, &header, br);
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(br);

    let _ = DrawIconEx(hdc, 18, (HEADER_H - 32) / 2, crate::app_icon(), 32, 32, 0, None, DI_NORMAL);

    let title_font = STATE.lock().unwrap().as_ref().map(|s| s.title_font).unwrap_or_default();
    let old = SelectObject(hdc, title_font);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, COLORREF(CLR_TEXT));
    let text = wide("FastWord");
    let mut rect = RECT { left: 62, top: 0, right: 190, bottom: HEADER_H };
    DrawTextW(hdc, &mut wide_slice(&text), &mut rect, DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);

    let _ = EndPaint(hwnd, &ps);
}

/// Text without the trailing NUL for DrawTextW.
fn wide_slice(v: &[u16]) -> Vec<u16> {
    v.iter().copied().take_while(|&c| c != 0).collect()
}

/// Owner-drawn buttons: rounded flat rects. Accent for primary actions and
/// the active nav pill; dark grey otherwise; nav inactive is borderless on
/// the header band.
unsafe fn draw_button(dis: &DRAWITEMSTRUCT) {
    let id = dis.CtlID as i32;
    let pressed = dis.itemState.0 & ODS_SELECTED.0 != 0;
    let is_nav = (ID_NAV_BASE..ID_NAV_BASE + 3).contains(&id);
    let nav_active = is_nav && CURRENT_TAB.load(Ordering::Relaxed) == (id - ID_NAV_BASE) as usize;

    let disabled = dis.itemState.0 & ODS_DISABLED.0 != 0;
    let (bg, text_color) = if is_nav {
        if nav_active {
            (CLR_ACCENT, CLR_TEXT)
        } else {
            (CLR_HEADER, CLR_TEXT_DIM)
        }
    } else if disabled {
        // Greyed-out: barely-raised surface, dimmed label.
        (rgb(31, 31, 42), rgb(100, 105, 126))
    } else if ACCENT_BUTTON_IDS.contains(&id) {
        (if pressed { rgb(60, 88, 200) } else { CLR_ACCENT }, CLR_TEXT)
    } else {
        (if pressed { rgb(58, 58, 76) } else { CLR_BTN }, CLR_TEXT)
    };

    // Parent background behind the rounded corners.
    let back = CreateSolidBrush(COLORREF(if is_nav { CLR_HEADER } else { CLR_BG }));
    FillRect(dis.hDC, &dis.rcItem, back);
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(back);

    let r = &dis.rcItem;
    let radius = if is_nav { (r.bottom - r.top) as f32 / 2.0 } else { 6.0 };
    fill_rounded_aa(dis.hDC, r, bg, radius);

    // Label.
    let mut buf = [0u16; 128];
    let n = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(dis.hwndItem, &mut buf);
    let mut label: Vec<u16> = buf[..n.max(0) as usize].to_vec();
    SetBkMode(dis.hDC, TRANSPARENT);
    SetTextColor(dis.hDC, COLORREF(text_color));
    let mut rect = dis.rcItem;
    DrawTextW(dis.hDC, &mut label, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            let _ = KillTimer(hwnd, TIMER_MODELS);
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_TIMER => {
            // Live download progress while the Models tab is in front.
            let cur = CURRENT_TAB.load(Ordering::Relaxed);
            if models::take_result().is_some() || (cur == TAB_MODELS && models::downloading_index().is_some()) {
                refresh_models(hwnd);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            paint_header(hwnd);
            LRESULT(0)
        }
        // Dark backgrounds for labels/checkbox text, edits and dropdown lists.
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _);
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(CLR_TEXT));
            let guard = STATE.lock().unwrap();
            let Some(state) = guard.as_ref() else {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            };
            let brush = if msg == WM_CTLCOLORSTATIC { state.br_bg } else { state.br_input };
            if msg != WM_CTLCOLORSTATIC {
                windows::Win32::Graphics::Gdi::SetBkColor(hdc, COLORREF(CLR_INPUT));
            }
            LRESULT(brush.0 as isize)
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            draw_button(dis);
            LRESULT(1)
        }
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            if hdr.code == LVN_ITEMCHANGED {
                match hdr.idFrom as i32 {
                    ID_H_LIST => update_history_buttons(hwnd),
                    ID_M_LIST => update_model_buttons(hwnd),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if (ID_NAV_BASE..ID_NAV_BASE + 3).contains(&id) {
                let tab = (id - ID_NAV_BASE) as usize;
                CURRENT_TAB.store(tab, Ordering::Relaxed);
                show_page(hwnd, tab);
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
                return LRESULT(0);
            }
            match id {
                ID_H_SEARCH if code == EN_CHANGE => refresh_history(hwnd),
                ID_H_COPY => {
                    if let Some((_, text)) = history_selected(hwnd) {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            let _ = cb.set_text(text);
                        }
                    }
                }
                ID_H_DELETE => {
                    if let Some((row_id, _)) = history_selected(hwnd) {
                        if let Ok(store) = history::HistoryStore::open_default() {
                            let _ = store.delete(&row_id);
                        }
                        refresh_history(hwnd);
                    }
                }
                ID_H_CLEAR => {
                    if let Ok(store) = history::HistoryStore::open_default() {
                        let _ = store.delete_all();
                    }
                    refresh_history(hwnd);
                }
                ID_S_SAVE => save_settings(hwnd),
                ID_M_DOWNLOAD | ID_M_CANCEL | ID_M_USE | ID_M_DELFILE => model_action(hwnd, id),
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
