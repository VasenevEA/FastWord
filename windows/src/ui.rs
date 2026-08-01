// The FastWord window: one top-level window with three tabs — History,
// Settings, Models — mirroring the macOS HistoryView / SettingsView /
// ModelManagementView. Everything is plain Win32 (comctl32) and runs on
// the main thread's message loop; the window is created lazily and hidden
// (not destroyed) on close.

use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, HFONT};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX,
    LVCOLUMNW, LVCF_TEXT, LVCF_WIDTH, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMTEXTW, LVNI_SELECTED, LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS,
    LVS_SINGLESEL, NMHDR, TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_INSERTITEMW,
    TCN_SELCHANGE, WC_LISTVIEWW, WC_TABCONTROLW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetDlgItem, KillTimer, MessageBoxW, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetTimer, ShowWindow, BM_GETCHECK,
    BM_SETCHECK, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CBS_DROPDOWNLIST, CB_ADDSTRING,
    CB_GETCURSEL, CB_SETCURSEL, EN_CHANGE, ES_AUTOHSCROLL, IDYES, MB_ICONQUESTION,
    MB_YESNO, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_NOTIFY, WM_SETFONT, WM_TIMER, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

use crate::{config, history, languages, logging, models, wide};

const WIN_W: i32 = 736;
const WIN_H: i32 = 560;
const PAGE_TOP: i32 = 36;

// Control ids.
const ID_TAB: i32 = 100;
const ID_H_SEARCH: i32 = 101;
const ID_H_LIST: i32 = 102;
const ID_H_COPY: i32 = 103;
const ID_H_DELETE: i32 = 104;
const ID_H_CLEAR: i32 = 105;
const ID_S_HOTKEY: i32 = 201;
const ID_S_SECONDARY: i32 = 202;
const ID_S_COMBO: i32 = 203;
const ID_S_LANGUAGE: i32 = 204;
const ID_S_IDLE: i32 = 205;
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

const TIMER_MODELS: usize = 2;

pub const TAB_HISTORY: usize = 0;
pub const TAB_SETTINGS: usize = 1;
pub const TAB_MODELS: usize = 2;

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
        let tab_ctl = GetDlgItem(hwnd, ID_TAB).unwrap_or_default();
        SendMessageW(
            tab_ctl,
            windows::Win32::UI::Controls::TCM_SETCURSEL,
            WPARAM(tab),
            LPARAM(0),
        );
        refresh_history(hwnd);
        load_settings_into_controls(hwnd);
        refresh_models(hwnd);
        show_page(hwnd, tab);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(hwnd, TIMER_MODELS, 500, None);
    }
}

fn create_window() -> anyhow::Result<()> {
    unsafe {
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
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
                (windows::Win32::Graphics::Gdi::COLOR_BTNFACE.0 + 1) as isize as *mut _,
            ),
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

        let font = CreateFontW(
            -15, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0,
            PCWSTR(wide("Segoe UI").as_ptr()),
        );

        build_controls(hwnd, font);

        *STATE.lock().unwrap() = Some(UiState { hwnd, font, history_rows: Vec::new() });
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

    // Tab control spans the client area.
    let tab = make(hwnd, font, WC_TABCONTROLW, "", WS_VISIBLE.0 | WS_TABSTOP.0, 0, 0, WIN_W, WIN_H, ID_TAB);
    for (i, label) in [ui.tab_history, ui.tab_settings, ui.tab_models].iter().enumerate() {
        let mut text = wide(label);
        let item = TCITEMW {
            mask: TCIF_TEXT,
            pszText: windows::core::PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        SendMessageW(tab, TCM_INSERTITEMW, WPARAM(i), LPARAM(&item as *const _ as isize));
    }

    // --- History page ------------------------------------------------------
    make(hwnd, font, stc, ui.search_label, 0, 16, PAGE_TOP + 14, 70, 20, ID_S_LABELS + 90);
    make(hwnd, font, edt, "", WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32, 90, PAGE_TOP + 10, 340, 24, ID_H_SEARCH);
    let lv = make(
        hwnd, font, WC_LISTVIEWW, "",
        WS_BORDER.0 | WS_TABSTOP.0 | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS) as u32,
        16, PAGE_TOP + 44, WIN_W - 48, 380, ID_H_LIST,
    );
    SendMessageW(lv, LVM_SETEXTENDEDLISTVIEWSTYLE, WPARAM(0), LPARAM(LVS_EX_FULLROWSELECT as isize));
    add_column(lv, 0, ui.col_time, 150);
    add_column(lv, 1, ui.col_text, WIN_W - 48 - 150 - 24);
    make(hwnd, font, btn, ui.btn_copy, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 16, PAGE_TOP + 434, 130, 28, ID_H_COPY);
    make(hwnd, font, btn, ui.btn_delete, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 154, PAGE_TOP + 434, 130, 28, ID_H_DELETE);
    make(hwnd, font, btn, ui.btn_clear_all, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 292, PAGE_TOP + 434, 130, 28, ID_H_CLEAR);

    // --- Settings page -----------------------------------------------------
    let labels = [ui.lbl_hotkey, ui.lbl_secondary, ui.lbl_combo, ui.lbl_language, ui.lbl_idle];
    let combo_ids = [ID_S_HOTKEY, ID_S_SECONDARY, ID_S_COMBO, ID_S_LANGUAGE, ID_S_IDLE];
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
        let y = PAGE_TOP + 210 + i as i32 * 30;
        make(hwnd, font, btn, label, WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32, 16, y, 400, 24, *id);
    }
    make(hwnd, font, btn, ui.btn_save, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 16, PAGE_TOP + 372, 140, 30, ID_S_SAVE);

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

    // --- Models page -------------------------------------------------------
    let mlv = make(
        hwnd, font, WC_LISTVIEWW, "",
        WS_BORDER.0 | WS_TABSTOP.0 | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS) as u32,
        16, PAGE_TOP + 10, WIN_W - 48, 414, ID_M_LIST,
    );
    SendMessageW(mlv, LVM_SETEXTENDEDLISTVIEWSTYLE, WPARAM(0), LPARAM(LVS_EX_FULLROWSELECT as isize));
    add_column(mlv, 0, ui.col_model, 300);
    add_column(mlv, 1, ui.col_size, 90);
    add_column(mlv, 2, ui.col_status, 240);
    make(hwnd, font, btn, ui.btn_download, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 16, PAGE_TOP + 434, 130, 28, ID_M_DOWNLOAD);
    make(hwnd, font, btn, ui.btn_cancel, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 154, PAGE_TOP + 434, 130, 28, ID_M_CANCEL);
    make(hwnd, font, btn, ui.btn_use, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 292, PAGE_TOP + 434, 130, 28, ID_M_USE);
    make(hwnd, font, btn, ui.btn_delete_file, WS_TABSTOP.0 | BS_PUSHBUTTON as u32, 430, PAGE_TOP + 434, 130, 28, ID_M_DELFILE);
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
        (TAB_HISTORY, &[ID_H_SEARCH, ID_H_LIST, ID_H_COPY, ID_H_DELETE, ID_H_CLEAR, ID_S_LABELS + 90]),
        (
            TAB_SETTINGS,
            &[
                ID_S_HOTKEY, ID_S_SECONDARY, ID_S_COMBO, ID_S_LANGUAGE, ID_S_IDLE, ID_S_SKIP,
                ID_S_CLEANUP, ID_S_HUD, ID_S_BEEPS, ID_S_AUTOSTART, ID_S_SAVE, ID_S_LABELS,
                ID_S_LABELS + 1, ID_S_LABELS + 2, ID_S_LABELS + 3, ID_S_LABELS + 4,
            ],
        ),
        (TAB_MODELS, &[ID_M_LIST, ID_M_DOWNLOAD, ID_M_CANCEL, ID_M_USE, ID_M_DELFILE]),
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
        } else if models::local_path(m) == active_path {
            ui.status_active.to_string()
        } else if models::is_downloaded(m) {
            ui.status_downloaded.to_string()
        } else {
            ui.status_not_downloaded.to_string()
        };
        lv_set_row(lv, i, &[m.display, &models::size_string(m.size_bytes), &status]);
    }
    if let Some(idx) = selected {
        use windows::Win32::UI::Controls::{LVIS_SELECTED, LVITEMW, LVM_SETITEMSTATE};
        let item = LVITEMW { state: LVIS_SELECTED, stateMask: LVIS_SELECTED, ..Default::default() };
        SendMessageW(lv, LVM_SETITEMSTATE, WPARAM(idx), LPARAM(&item as *const _ as isize));
    }
}

unsafe fn model_action(hwnd: HWND, cmd: i32) {
    let lv = GetDlgItem(hwnd, ID_M_LIST).unwrap_or_default();
    match cmd {
        ID_M_CANCEL => models::cancel_download(),
        _ => {
            let Some(idx) = lv_selected(lv) else { return };
            let m = &models::CATALOG[idx];
            match cmd {
                ID_M_DOWNLOAD => {
                    if !models::is_downloaded(m) {
                        models::start_download(idx);
                    }
                }
                ID_M_USE => {
                    if models::is_downloaded(m) {
                        let mut cfg = config::load_or_create().unwrap_or_default();
                        cfg.model_path = models::local_path(m).to_string_lossy().into_owned();
                        match config::save(&cfg) {
                            Ok(()) => {
                                logging::log(&format!("active model -> {}", m.filename));
                                prompt_restart(hwnd);
                            }
                            Err(e) => logging::log(&format!("model select failed: {e:#}")),
                        }
                    }
                }
                ID_M_DELFILE => {
                    let cfg = config::load_or_create().unwrap_or_default();
                    if models::is_downloaded(m) && models::local_path(m) != cfg.resolved_model_path() {
                        let _ = std::fs::remove_file(models::local_path(m));
                    }
                }
                _ => {}
            }
        }
    }
    refresh_models(hwnd);
}

// ---------------------------------------------------------------------------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            let _ = KillTimer(hwnd, TIMER_MODELS);
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_TIMER => {
            // Live download progress while the Models tab is in front.
            let tab = GetDlgItem(hwnd, ID_TAB).unwrap_or_default();
            let cur = SendMessageW(tab, TCM_GETCURSEL, WPARAM(0), LPARAM(0)).0;
            if models::take_result().is_some() || (cur == TAB_MODELS as isize && models::downloading_index().is_some()) {
                refresh_models(hwnd);
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            if hdr.idFrom == ID_TAB as usize && hdr.code == TCN_SELCHANGE {
                let cur = SendMessageW(hdr.hwndFrom, TCM_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                if cur >= 0 {
                    show_page(hwnd, cur as usize);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
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
