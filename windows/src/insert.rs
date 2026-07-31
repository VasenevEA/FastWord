// Text insertion: clipboard + synthesized Ctrl+V, with clipboard
// snapshot/restore — the same near-universal strategy the macOS app settled
// on (AX-style APIs are unreliable in Chromium/Electron inputs there; on
// Windows the equivalent UIA patterns are just as patchy).
//
// The user's clipboard text is restored ~700 ms after the paste, unless
// they copied something new in the meantime.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use arboard::Clipboard;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_V,
};

/// Insert `text` into the focused field of the foreground app.
/// Blocks for ~700 ms (clipboard-restore window).
pub fn insert_text(text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().context("opening clipboard")?;
    // Snapshot whatever text is on the clipboard. (Non-text content —
    // images, files — is not restored; known v1 limitation.)
    let snapshot = clipboard.get_text().ok();

    clipboard
        .set_text(text.to_string())
        .context("writing text to clipboard")?;

    // Give the clipboard write a moment to settle before the paste chord.
    thread::sleep(Duration::from_millis(50));
    send_ctrl_v();

    // Electron / Chromium paste handlers are asynchronous — restoring the
    // clipboard too early races them (700 ms cleared every app the macOS
    // side tested).
    thread::sleep(Duration::from_millis(700));

    // Only restore if the clipboard still holds our text — if the user
    // copied something else during the window, leave it alone.
    let current = clipboard.get_text().ok();
    if current.as_deref() == Some(text) {
        match snapshot {
            Some(old) => {
                let _ = clipboard.set_text(old);
            }
            None => {
                let _ = clipboard.clear();
            }
        }
    }
    Ok(())
}

fn key_event(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_ctrl_v() {
    // A short gap between down and up lets slower apps register the chord.
    unsafe {
        let down = [key_event(VK_CONTROL, false), key_event(VK_V, false)];
        SendInput(&down, std::mem::size_of::<INPUT>() as i32);
        thread::sleep(Duration::from_millis(15));
        let up = [key_event(VK_V, true), key_event(VK_CONTROL, true)];
        SendInput(&up, std::mem::size_of::<INPUT>() as i32);
    }
}
