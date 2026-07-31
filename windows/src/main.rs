// FastWord for Windows — local, private, push-to-talk dictation.
//
// Hold the hotkey (default: Right Ctrl), speak, release — the transcript is
// pasted into the focused text field. Whisper runs entirely on-device via
// the same fastword-sidecar used by the macOS app.
//
// Architecture: the main thread owns the Win32 surface (hidden window, tray
// icon, WH_KEYBOARD_LL hook, message loop); a controller thread owns the
// microphone, the sidecar process and the history db, and runs the
// record → transcribe → clean → paste pipeline.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod history;
mod hotkey;
mod insert;
mod languages;
mod logging;
mod postprocess;
mod sidecar;

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Context, Result};
use hotkey::{ComboMode, PttEvent, PttTracker};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallNextHookEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW,
    MessageBoxW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowsHookExW,
    TrackPopupMenu, TranslateMessage, UnhookWindowsHookEx, HHOOK, IDI_APPLICATION,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, MB_ICONERROR, MB_ICONHAND, MB_OK, MF_SEPARATOR, MF_STRING,
    MSG, SW_SHOWNORMAL, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WH_KEYBOARD_LL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONUP,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW,
};

const WM_TRAYICON: u32 = WM_APP + 1;
const TRAY_ICON_ID: u32 = 1;

const CMD_OPEN_CONFIG: usize = 1;
const CMD_OPEN_HISTORY: usize = 2;
const CMD_OPEN_LOG: usize = 3;
const CMD_QUIT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppEvent {
    PttDown,
    PttUp,
    CancelKey,
    Quit,
}

static EVENT_TX: OnceLock<Sender<AppEvent>> = OnceLock::new();
static TRACKER: Mutex<Option<PttTracker>> = Mutex::new(None);

fn send_event(ev: AppEvent) {
    if let Some(tx) = EVENT_TX.get() {
        let _ = tx.send(ev);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn message_box(text: &str, caption: &str) {
    let text_w = wide(text);
    let caption_w = wide(caption);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text_w.as_ptr()),
            PCWSTR(caption_w.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn main() {
    logging::log("FastWord for Windows starting");
    if let Err(err) = run_app() {
        let msg = format!("{err:#}");
        logging::log(&format!("fatal: {msg}"));
        message_box(&msg, "FastWord");
        std::process::exit(1);
    }
    logging::log("FastWord exited cleanly");
}

fn run_app() -> Result<()> {
    ensure_single_instance()?;

    let cfg = config::load_or_create()
        .with_context(|| format!("loading {}", config::config_path().display()))?;

    let primary = hotkey::parse_hotkey(&cfg.hotkey)
        .ok_or_else(|| anyhow!("unknown hotkey \"{}\" in config.toml", cfg.hotkey))?;
    let secondary = if cfg.hotkey_secondary.trim().is_empty() {
        None
    } else {
        Some(hotkey::parse_hotkey(&cfg.hotkey_secondary).ok_or_else(|| {
            anyhow!("unknown hotkey_secondary \"{}\" in config.toml", cfg.hotkey_secondary)
        })?)
    };
    let combo_mode = ComboMode::parse(&cfg.combo_mode);

    let model_path = cfg.resolved_model_path();
    if !model_path.exists() {
        return Err(anyhow!(
            "Whisper model not found at:\n{}\n\nRun windows\\scripts\\get-model.ps1 to download it,\nor set model_path in {}",
            model_path.display(),
            config::config_path().display()
        ));
    }

    let sidecar_exe = sidecar::find_sidecar_exe().ok_or_else(|| {
        anyhow!("fastword-sidecar.exe not found (expected next to fastword-win.exe)")
    })?;
    logging::log(&format!(
        "sidecar: {} | model: {}",
        sidecar_exe.display(),
        model_path.display()
    ));

    let client = sidecar::SidecarClient::spawn(&sidecar_exe, &model_path, cfg.idle_evict_secs)?;

    let (language, initial_prompt) = languages::resolve(&cfg.language);
    logging::log(&format!(
        "hotkey: {}{} | language: {}",
        cfg.hotkey,
        secondary.map(|_| format!(" + {} ({})", cfg.hotkey_secondary, cfg.combo_mode)).unwrap_or_default(),
        if language.is_empty() { "auto" } else { &language }
    ));

    let (tx, rx) = channel();
    EVENT_TX
        .set(tx)
        .map_err(|_| anyhow!("event channel already initialized"))?;
    *TRACKER.lock().unwrap() = Some(PttTracker::new(primary, secondary, combo_mode));

    let controller_cfg = ControllerConfig {
        language,
        initial_prompt,
        skip_empty: cfg.skip_empty,
        cleanup_enabled: cfg.cleanup_enabled,
        beeps: cfg.beeps,
    };
    let controller = std::thread::spawn(move || controller_loop(rx, client, controller_cfg));

    // Win32 surface: keyboard hook, hidden window, tray icon, message loop.
    let hook = unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None).context("GetModuleHandleW")?.into();
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), hinstance, 0)
            .context("installing keyboard hook")?
    };

    let hwnd = create_hidden_window()?;
    let tip = format!("FastWord — hold {} to dictate", cfg.hotkey);
    add_tray_icon(hwnd, &tip)?;
    logging::log("ready — tray icon installed");

    run_message_loop();

    remove_tray_icon(hwnd);
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
    send_event(AppEvent::Quit);
    let _ = controller.join();
    Ok(())
}

fn ensure_single_instance() -> Result<()> {
    unsafe {
        let name = wide("Local\\FastWordWinSingleInstance");
        let _mutex = CreateMutexW(None, true, PCWSTR(name.as_ptr()));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Err(anyhow!("FastWord is already running (check the tray)."));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Keyboard hook

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        // Ignore events we synthesized ourselves (the Ctrl+V paste chord),
        // otherwise a Ctrl hotkey would re-trigger recording.
        let injected = kb.flags.0 & LLKHF_INJECTED.0 != 0;
        if !injected {
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if down || up {
                if kb.vkCode == VK_ESCAPE.0 as u32 {
                    if down {
                        send_event(AppEvent::CancelKey);
                    }
                } else if let Some(ev) = TRACKER
                    .lock()
                    .ok()
                    .and_then(|mut t| t.as_mut().and_then(|t| t.on_key(kb.vkCode, down)))
                {
                    send_event(match ev {
                        PttEvent::Start => AppEvent::PttDown,
                        PttEvent::Stop => AppEvent::PttUp,
                    });
                }
            }
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

// ---------------------------------------------------------------------------
// Hidden window + tray icon

fn create_hidden_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class_name = wide("FastWordTrayWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(anyhow!("RegisterClassW failed"));
        }
        let title = wide("FastWord");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )
        .context("CreateWindowExW")?;
        Ok(hwnd)
    }
}

fn tray_icon_data(hwnd: HWND, tip: &str) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        ..Default::default()
    };
    nid.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() };
    let tip_w: Vec<u16> = tip.encode_utf16().take(127).collect();
    nid.szTip[..tip_w.len()].copy_from_slice(&tip_w);
    nid
}

fn add_tray_icon(hwnd: HWND, tip: &str) -> Result<()> {
    let nid = tray_icon_data(hwnd, tip);
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
    if !ok.as_bool() {
        return Err(anyhow!("Shell_NotifyIconW(NIM_ADD) failed"));
    }
    Ok(())
}

fn remove_tray_icon(hwnd: HWND) {
    let nid = tray_icon_data(hwnd, "");
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let mouse = (lparam.0 as u32) & 0xFFFF;
            if mouse == WM_RBUTTONUP || mouse == WM_LBUTTONUP {
                show_tray_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let items: [(usize, &str); 3] = [
        (CMD_OPEN_CONFIG, "Open config"),
        (CMD_OPEN_HISTORY, "Open history folder"),
        (CMD_OPEN_LOG, "Open log"),
    ];
    for (id, label) in items {
        let label_w = wide(label);
        let _ = AppendMenuW(menu, MF_STRING, id, PCWSTR(label_w.as_ptr()));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let quit_w = wide("Quit FastWord");
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, PCWSTR(quit_w.as_ptr()));

    let mut pt = Default::default();
    let _ = GetCursorPos(&mut pt);
    // Required for the menu to close when clicking elsewhere.
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
        pt.x,
        pt.y,
        0,
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);

    match cmd.0 as usize {
        CMD_OPEN_CONFIG => shell_open(&config::config_path().to_string_lossy()),
        CMD_OPEN_HISTORY => shell_open(&history::default_dir().to_string_lossy()),
        CMD_OPEN_LOG => shell_open(
            &dirs::data_local_dir()
                .unwrap_or_default()
                .join("fastword")
                .join("fastword-win.log")
                .to_string_lossy(),
        ),
        CMD_QUIT => {
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}

fn shell_open(path: &str) {
    let verb = wide("open");
    let path_w = wide(path);
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn run_message_loop() {
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Controller: record → transcribe → clean → paste

struct ControllerConfig {
    language: String,
    initial_prompt: Option<String>,
    skip_empty: bool,
    cleanup_enabled: bool,
    beeps: bool,
}

fn beep(enabled: bool, style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE) {
    if enabled {
        unsafe {
            let _ = MessageBeep(style);
        }
    }
}

fn controller_loop(
    rx: Receiver<AppEvent>,
    mut client: sidecar::SidecarClient,
    cfg: ControllerConfig,
) {
    // cpal streams are not Send, so the recorder lives here, on the thread
    // that starts and stops it.
    let mut recorder = audio::Recorder::new();
    let history = match history::HistoryStore::open_default() {
        Ok(h) => Some(h),
        Err(e) => {
            logging::log(&format!("history db unavailable: {e:#}"));
            None
        }
    };

    // Load the model up front so the first dictation isn't slow.
    let t0 = std::time::Instant::now();
    match client.warmup() {
        Ok(_) => logging::log(&format!("model warmed up in {:.1}s", t0.elapsed().as_secs_f32())),
        Err(e) => logging::log(&format!("warmup failed: {e:#}")),
    }

    let mut recording = false;
    while let Ok(event) = rx.recv() {
        match event {
            AppEvent::Quit => break,
            AppEvent::PttDown if !recording => match recorder.start() {
                Ok(()) => {
                    recording = true;
                    logging::log("recording started");
                    beep(cfg.beeps, MB_OK);
                }
                Err(e) => {
                    logging::log(&format!("could not start recording: {e:#}"));
                    beep(cfg.beeps, MB_ICONHAND);
                }
            },
            AppEvent::CancelKey if recording => {
                recorder.cancel();
                recording = false;
                logging::log("recording cancelled (Escape)");
            }
            AppEvent::PttUp if recording => {
                recording = false;
                let pcm = recorder.stop();
                logging::log(&format!(
                    "recording stopped: {:.2}s of audio",
                    pcm.len() as f32 / audio::TARGET_SAMPLE_RATE as f32
                ));
                if pcm.len() < 1600 {
                    // Under 0.1 s — an accidental tap.
                    continue;
                }
                let t = std::time::Instant::now();
                match client.transcribe(
                    &pcm,
                    &cfg.language,
                    cfg.initial_prompt.as_deref(),
                    cfg.skip_empty,
                ) {
                    Ok(raw) => {
                        let text = if cfg.cleanup_enabled {
                            postprocess::clean(&raw)
                        } else {
                            raw.trim().to_string()
                        };
                        logging::log(&format!(
                            "transcribed in {:.1}s: {} chars",
                            t.elapsed().as_secs_f32(),
                            text.chars().count()
                        ));
                        // Escape pressed while we were transcribing → discard.
                        let mut cancelled = false;
                        while let Ok(ev) = rx.try_recv() {
                            match ev {
                                AppEvent::CancelKey => cancelled = true,
                                AppEvent::Quit => return,
                                _ => {}
                            }
                        }
                        if cancelled {
                            logging::log("result discarded (Escape during transcription)");
                            continue;
                        }
                        if text.is_empty() {
                            logging::log("empty transcript — nothing to paste");
                            continue;
                        }
                        if let Err(e) = insert::insert_text(&text) {
                            logging::log(&format!("paste failed: {e:#}"));
                            beep(cfg.beeps, MB_ICONHAND);
                            continue;
                        }
                        if let Some(h) = &history {
                            if let Err(e) = h.insert(&text) {
                                logging::log(&format!("history insert failed: {e:#}"));
                            }
                        }
                    }
                    Err(e) => {
                        logging::log(&format!("transcription failed: {e:#}"));
                        beep(cfg.beeps, MB_ICONHAND);
                    }
                }
            }
            // PttDown while recording / PttUp while idle / Cancel while idle.
            _ => {}
        }
    }
    logging::log("controller stopped");
}
