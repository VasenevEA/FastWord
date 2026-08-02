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
mod autostart;
mod config;
mod gpu_hint;
mod history;
mod hud;
mod models;
mod ui;
mod hotkey;
mod insert;
mod languages;
mod logging;
mod postprocess;
mod sidecar;

use std::collections::VecDeque;
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
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallNextHookEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW,
    MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SetForegroundWindow, SetWindowsHookExW, TrackPopupMenu, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, IDI_APPLICATION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MB_ICONERROR,
    MB_ICONHAND, MB_OK, MF_SEPARATOR, MF_STRING, MSG, SW_SHOWNORMAL, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WH_KEYBOARD_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WNDCLASSW,
};

const WM_TRAYICON: u32 = WM_APP + 1;
/// Programmatic "open the main window on tab N" (wparam = tab index).
/// Used by tests/automation; the tray menu goes through the same path.
const WM_OPEN_UI: u32 = WM_APP + 2;
const TRAY_ICON_ID: u32 = 1;

const CMD_OPEN_MAIN: usize = 1;
const CMD_SHOW_HISTORY: usize = 2;
const CMD_SHOW_SETTINGS: usize = 3;
const CMD_SHOW_MODELS: usize = 4;
const CMD_OPEN_LOG: usize = 5;
const CMD_QUIT: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppEvent {
    PttDown,
    PttUp,
    CancelKey,
    Quit,
}

static EVENT_TX: OnceLock<Sender<AppEvent>> = OnceLock::new();
static TRACKER: Mutex<Option<PttTracker>> = Mutex::new(None);
/// PTT keys the hook should swallow (system doesn't see them). Only
/// non-modifier keys go here: a Win/CapsLock/F-key hotkey has disruptive
/// side effects (Start menu, caps state), while Ctrl/Alt/Shift are inert
/// when held alone, so those pass through like on macOS. 0 = unused slot.
static SWALLOW_VKS: [std::sync::atomic::AtomicU32; 2] = [
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
];
/// "TaskbarCreated" broadcast id — Explorer restarts wipe tray icons, and
/// every tray app must re-add its icon when this message arrives.
static TASKBAR_CREATED_MSG: OnceLock<u32> = OnceLock::new();
static TRAY_TIP: OnceLock<String> = OnceLock::new();

/// Modifier keys are pass-through; everything else configured as a hotkey
/// gets swallowed by the hook.
fn is_modifier_vk(vk: u32) -> bool {
    (0xA0..=0xA5).contains(&vk) // L/R Shift, Ctrl, Alt
}

fn send_event(ev: AppEvent) {
    if let Some(tx) = EVENT_TX.get() {
        let _ = tx.send(ev);
    }
}

pub fn wide(s: &str) -> Vec<u16> {
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
    languages::set_ui_language(&cfg.ui_language);

    let primary = hotkey::parse_hotkey(&cfg.hotkey)
        .ok_or_else(|| anyhow!("unknown hotkey \"{}\" in config.toml", cfg.hotkey))?;
    let mut secondary = if cfg.hotkey_secondary.trim().is_empty() {
        None
    } else {
        Some(hotkey::parse_hotkey(&cfg.hotkey_secondary).ok_or_else(|| {
            anyhow!("unknown hotkey_secondary \"{}\" in config.toml", cfg.hotkey_secondary)
        })?)
    };
    // Same key twice + combo_mode="both" could never fire — collapse to a
    // single-key hotkey like the macOS monitor does.
    if secondary == Some(primary) {
        logging::log("hotkey_secondary equals hotkey — using single-key mode");
        secondary = None;
    }
    let combo_mode = ComboMode::parse(&cfg.combo_mode);

    // Non-modifier hotkeys are swallowed by the hook so a Win/CapsLock/F-key
    // PTT doesn't open the Start menu / toggle caps on every dictation.
    {
        use std::sync::atomic::Ordering;
        let mut slot = 0;
        for vk in [Some(primary), secondary].into_iter().flatten() {
            if !is_modifier_vk(vk) {
                SWALLOW_VKS[slot].store(vk, Ordering::Relaxed);
                slot += 1;
            }
        }
    }

    // Engine routing (GigaAM is selected in the Models tab and is
    // Russian-only, so its language hint is pinned like on macOS).
    let gigaam_active = cfg.uses_gigaam() && models::gigaam_installed();

    // A missing whisper model is not fatal — a fresh install has none, and
    // a GigaAM-only install doesn't need one at all. Only when *no* usable
    // engine exists do we start without the sidecar and point the user at
    // the Models tab.
    let model_path = cfg.resolved_model_path();
    let model_missing = !model_path.exists() && !gigaam_active;
    if model_missing {
        logging::log(&format!("model not found at {} — starting without sidecar", model_path.display()));
    }

    let sidecar_exe = sidecar::find_sidecar_exe().ok_or_else(|| {
        anyhow!("fastword-sidecar.exe not found (expected next to fastword-win.exe)")
    })?;
    logging::log(&format!(
        "sidecar: {} | model: {}",
        sidecar_exe.display(),
        model_path.display()
    ));

    let spawner = sidecar::SidecarSpawner {
        exe: sidecar_exe.clone(),
        model: model_path.clone(),
        idle_evict_secs: cfg.idle_evict_secs,
    };
    let client = if model_missing { None } else { Some(spawner.spawn()?) };

    let (mut language, mut initial_prompt) = languages::resolve(&cfg.language);
    let engine = if gigaam_active {
        language = "ru".to_string();
        initial_prompt = None;
        Some("gigaam".to_string())
    } else {
        None
    };
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
        engine,
        skip_empty: cfg.skip_empty,
        cleanup_enabled: cfg.cleanup_enabled,
        beeps: cfg.beeps,
        hud: cfg.hud,
        ptt_vks: std::iter::once(primary).chain(secondary).collect(),
    };
    let controller =
        std::thread::spawn(move || controller_loop(rx, client, spawner, controller_cfg));

    // Win32 surface: keyboard hook, hidden window, tray icon, message loop.
    let hook = unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None).context("GetModuleHandleW")?.into();
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), hinstance, 0)
            .context("installing keyboard hook")?
    };

    let hwnd = create_hidden_window()?;
    if cfg.hud {
        if let Err(e) = hud::create() {
            logging::log(&format!("HUD unavailable: {e:#}"));
        }
    }
    let tip = languages::ui().tip_hold_to_dictate.replace("{key}", &cfg.hotkey);
    let _ = TRAY_TIP.set(tip.clone());
    let taskbar_msg = unsafe {
        let name = wide("TaskbarCreated");
        RegisterWindowMessageW(PCWSTR(name.as_ptr()))
    };
    let _ = TASKBAR_CREATED_MSG.set(taskbar_msg);
    add_tray_icon(hwnd, &tip)?;
    logging::log("ready — tray icon installed");

    if model_missing {
        let ui_str = languages::ui();
        let text = wide(ui_str.model_missing_prompt);
        let caption = wide("FastWord");
        let clicked = unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(caption.as_ptr()),
                windows::Win32::UI::WindowsAndMessaging::MB_YESNO
                    | windows::Win32::UI::WindowsAndMessaging::MB_ICONQUESTION,
            )
        };
        if clicked == windows::Win32::UI::WindowsAndMessaging::IDYES {
            ui::open(ui::TAB_MODELS);
        }
    }

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
                } else {
                    if let Some(ev) = TRACKER
                        .lock()
                        .ok()
                        .and_then(|mut t| t.as_mut().and_then(|t| t.on_key(kb.vkCode, down)))
                    {
                        send_event(match ev {
                            PttEvent::Start => AppEvent::PttDown,
                            PttEvent::Stop => AppEvent::PttUp,
                        });
                    }
                    // Swallow non-modifier hotkeys (Win would open the Start
                    // menu, CapsLock would toggle, F1 would open Help — on
                    // every dictation).
                    use std::sync::atomic::Ordering;
                    if SWALLOW_VKS
                        .iter()
                        .any(|a| a.load(Ordering::Relaxed) == kb.vkCode)
                    {
                        return LRESULT(1);
                    }
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

/// The embedded app icon (resource id 1, set by build.rs / winresource) —
/// the same microphone icon the macOS app uses. Falls back to the generic
/// application icon if the resource is missing.
pub fn app_icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        GetModuleHandleW(None)
            .ok()
            .and_then(|m| LoadIconW(m, PCWSTR(1 as *const u16)).ok())
            .or_else(|| LoadIconW(None, IDI_APPLICATION).ok())
            .unwrap_or_default()
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
    nid.hIcon = app_icon();
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
            if mouse == WM_LBUTTONUP {
                // Plain click opens the window; the menu is on right-click.
                ui::open(ui::TAB_HISTORY);
            } else if mouse == WM_RBUTTONUP {
                show_tray_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_OPEN_UI => {
            ui::open(wparam.0.min(2));
            LRESULT(0)
        }
        WM_DESTROY => {
            // Remove the icon while the window is still alive — after
            // DestroyWindow returns, NIM_DELETE fails and the icon ghosts.
            remove_tray_icon(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            // Explorer restarted → all tray icons were wiped; re-add ours.
            if Some(&msg) == TASKBAR_CREATED_MSG.get() {
                let tip = TRAY_TIP.get().map(String::as_str).unwrap_or("FastWord");
                if let Err(e) = add_tray_icon(hwnd, tip) {
                    logging::log(&format!("tray icon re-add failed: {e:#}"));
                } else {
                    logging::log("tray icon re-added after Explorer restart");
                }
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let ui = languages::ui();
    let main_w = wide("FastWord");
    let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_MAIN, PCWSTR(main_w.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let items: [(usize, &str); 3] = [
        (CMD_SHOW_HISTORY, ui.menu_open_history),
        (CMD_SHOW_SETTINGS, ui.menu_settings),
        (CMD_SHOW_MODELS, ui.menu_models),
    ];
    for (id, label) in items {
        let label_w = wide(label);
        let _ = AppendMenuW(menu, MF_STRING, id, PCWSTR(label_w.as_ptr()));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let log_w = wide(ui.menu_open_log);
    let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_LOG, PCWSTR(log_w.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let quit_w = wide(ui.menu_quit);
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, PCWSTR(quit_w.as_ptr()));

    // Bold "default" styling for the first item (double-click semantics).
    let _ = windows::Win32::UI::WindowsAndMessaging::SetMenuDefaultItem(
        menu,
        CMD_OPEN_MAIN as u32,
        0, // by command id, not position
    );

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
    // KB135788: without this no-op message, the *next* popup menu on this
    // hidden window can fail to appear.
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match cmd.0 as usize {
        CMD_OPEN_MAIN => ui::open(ui::TAB_HISTORY),
        CMD_SHOW_HISTORY => ui::open(ui::TAB_HISTORY),
        CMD_SHOW_SETTINGS => ui::open(ui::TAB_SETTINGS),
        CMD_SHOW_MODELS => ui::open(ui::TAB_MODELS),
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

/// Save-and-restart: relaunch the exe after a short delay (letting this
/// process release the single-instance mutex) and quit.
pub fn restart_app() {
    let Ok(exe) = std::env::current_exe() else { return };
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // PowerShell single-quote escaping: double any embedded quotes so a
    // path like C:\Users\O'Brien can't break out of the string.
    let exe_quoted = exe.display().to_string().replace('\'', "''");
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command"])
        .arg(format!(
            "Start-Sleep -Milliseconds 900; Start-Process '{exe_quoted}'"
        ))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    logging::log("restarting after settings change");
    unsafe { PostQuitMessage(0) };
}

pub fn shell_open(path: &str) {
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
        // GetMessageW returns -1 on error, which BOOL::as_bool() would treat
        // as true and spin on a garbage MSG — compare > 0 instead.
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
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
    /// None = whisper; Some("gigaam") = the Russian sherpa-onnx engine.
    engine: Option<String>,
    skip_empty: bool,
    cleanup_enabled: bool,
    beeps: bool,
    hud: bool,
    /// The configured PTT virtual-key codes, for the lost-key-up failsafe.
    ptt_vks: Vec<u32>,
}

/// True while any of the given keys is physically held down.
fn any_key_down(vks: &[u32]) -> bool {
    vks.iter()
        .any(|&vk| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0)
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
    mut client: Option<sidecar::SidecarClient>,
    spawner: sidecar::SidecarSpawner,
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

    if let Some(client) = client.as_mut() {
        // Ask the sidecar what it was built with; if it's CPU-only next to
        // an NVIDIA GPU, offer the GPU setup path (once).
        match client.info() {
            Ok(line) => {
                logging::log(&line);
                gpu_hint::maybe_offer(sidecar::parse_backend(&line));
            }
            Err(e) => logging::log(&format!("sidecar info failed: {e:#}")),
        }

        // Load the active engine up front so the first dictation isn't slow.
        let t0 = std::time::Instant::now();
        match client.warmup(cfg.engine.as_deref()) {
            Ok(_) => logging::log(&format!("model warmed up in {:.1}s", t0.elapsed().as_secs_f32())),
            Err(e) => logging::log(&format!("warmup failed: {e:#}")),
        }
    }

    // Events that arrived while a transcription was in flight are re-queued
    // here so a dictation started during inference isn't silently dropped.
    let mut pending: VecDeque<AppEvent> = VecDeque::new();
    let mut recording = false;
    loop {
        let event = match pending.pop_front() {
            Some(e) => e,
            // While recording, poll the physical key state as a failsafe:
            // the hook's key-up can get lost (RDP focus changes, the UAC
            // secure desktop), which would leave the microphone open
            // forever and swallow every following press.
            None if recording => match rx.recv_timeout(std::time::Duration::from_millis(150)) {
                Ok(e) => e,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if any_key_down(&cfg.ptt_vks) {
                        continue;
                    }
                    logging::log("failsafe: PTT key no longer held — stopping recording");
                    if let Ok(mut t) = TRACKER.lock() {
                        if let Some(t) = t.as_mut() {
                            t.reset();
                        }
                    }
                    AppEvent::PttUp
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(e) => e,
                Err(_) => break,
            },
        };
        match event {
            AppEvent::Quit => break,
            AppEvent::PttDown if !recording => match recorder.start() {
                Ok(()) => {
                    recording = true;
                    logging::log("recording started");
                    if cfg.hud {
                        hud::set_state(hud::HudState::Recording);
                    }
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
                if cfg.hud {
                    hud::set_state(hud::HudState::Hidden);
                }
                logging::log("recording cancelled (Escape)");
            }
            AppEvent::PttUp if recording => {
                recording = false;
                let pcm = recorder.stop();
                let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                logging::log(&format!(
                    "recording stopped: {:.2}s of audio, peak level {:.3}",
                    pcm.len() as f32 / audio::TARGET_SAMPLE_RATE as f32,
                    peak
                ));
                if peak < 0.005 {
                    logging::log(
                        "audio looks silent — check the microphone (input device, level, privacy settings)",
                    );
                }
                if pcm.len() < 1600 {
                    // Under 0.1 s — an accidental tap.
                    if cfg.hud {
                        hud::set_state(hud::HudState::Hidden);
                    }
                    continue;
                }
                let Some(active_client) = client.as_mut() else {
                    // No model installed yet — nothing to transcribe with.
                    logging::log("no model installed — dictation skipped (see the Models tab)");
                    if cfg.hud {
                        hud::set_state(hud::HudState::Hidden);
                    }
                    beep(cfg.beeps, MB_ICONHAND);
                    continue;
                };
                if cfg.hud {
                    hud::set_state(hud::HudState::Transcribing);
                }
                let t = std::time::Instant::now();
                let result = active_client.transcribe(
                    &pcm,
                    &cfg.language,
                    cfg.initial_prompt.as_deref(),
                    cfg.skip_empty,
                    cfg.engine.as_deref(),
                );
                // Drain events that queued up during inference: Escape
                // discards this result; everything else (a new dictation's
                // PttDown/PttUp) is re-queued and handled next.
                let mut cancelled = false;
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        AppEvent::CancelKey => cancelled = true,
                        other => pending.push_back(other),
                    }
                }
                // Whisper is done — drop the HUD before the paste so it
                // never overlaps the target window's own UI.
                if cfg.hud {
                    hud::set_state(hud::HudState::Hidden);
                }
                match result {
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
                        // If the sidecar process died, respawn it so the next
                        // dictation works without restarting the app.
                        if !active_client.is_alive() {
                            logging::log("sidecar is dead — respawning");
                            match spawner.spawn() {
                                Ok(new_client) => {
                                    client = Some(new_client);
                                    logging::log("sidecar respawned");
                                }
                                Err(e) => {
                                    logging::log(&format!("sidecar respawn failed: {e:#}"))
                                }
                            }
                        }
                    }
                }
            }
            // PttDown while recording / PttUp while idle / Cancel while idle.
            _ => {}
        }
    }
    logging::log("controller stopped");
}
