// Start-with-Windows via the classic HKCU Run key. No admin rights needed;
// the value points at the current exe.

use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "FastWord";

fn open_run_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY> {
    let mut key = HKEY::default();
    let path = crate::wide(RUN_KEY);
    unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), 0, access, &mut key)
            .ok()
            .context("opening HKCU Run key")?;
    }
    Ok(key)
}

pub fn is_enabled() -> bool {
    let Ok(key) = open_run_key(KEY_QUERY_VALUE) else { return false };
    let name = crate::wide(VALUE_NAME);
    let found = unsafe {
        RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, None, None, None).is_ok()
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    found
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let name = crate::wide(VALUE_NAME);
    let result = unsafe {
        if enabled {
            let exe = std::env::current_exe().context("current exe path")?;
            let value = crate::wide(&format!("\"{}\"", exe.display()));
            let bytes: &[u8] = std::slice::from_raw_parts(
                value.as_ptr() as *const u8,
                value.len() * 2,
            );
            RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes)).ok()
        } else {
            // Deleting a value that doesn't exist is fine.
            let status = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
            if status == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                status.ok()
            }
        }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result.context("updating autostart registry value")?;
    crate::logging::log(&format!(
        "autostart {}",
        if enabled { "enabled" } else { "disabled" }
    ));
    Ok(())
}
