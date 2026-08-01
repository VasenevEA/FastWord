// One-time GPU suggestion: when an NVIDIA GPU is present but the sidecar
// was built CPU-only, offer the user the setup-gpu.ps1 path once. The offer
// is a Yes/No message box; "Yes" opens the folder with the script.

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONINFORMATION, MB_YESNO,
};

/// Pure decision logic, kept separate for tests.
pub fn should_offer(backend: &str, nvidia_present: bool, already_shown: bool) -> bool {
    backend == "cpu" && nvidia_present && !already_shown
}

/// The NVIDIA driver ships nvml.dll into System32 — its presence is a
/// reliable "NVIDIA GPU with driver installed" signal without linking
/// against anything.
pub fn nvidia_driver_present() -> bool {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(windir).join("System32").join("nvml.dll").exists()
}

fn marker_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fastword")
        .join("gpu-hint-shown")
}

/// Locate windows\scripts (dev layout: exe lives in windows\target\release;
/// packaged layout: scripts next to the exe).
fn scripts_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidates = [
        dir.join("scripts"),
        dir.parent()?.parent()?.join("scripts"),
    ];
    candidates
        .into_iter()
        .find(|c| c.join("setup-gpu.ps1").exists())
}

/// Call once per launch with the backend reported by the sidecar. Shows the
/// offer at most once ever (marker file). Runs the blocking message box on
/// its own thread so dictation is not held up.
pub fn maybe_offer(backend: &str) {
    let already_shown = marker_path().exists();
    if !should_offer(backend, nvidia_driver_present(), already_shown) {
        return;
    }
    if let Some(parent) = marker_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(marker_path(), b"shown");
    crate::logging::log("NVIDIA GPU detected with a CPU-only sidecar — offering GPU setup");

    std::thread::spawn(|| {
        let text = wide(
            "An NVIDIA GPU was detected, but FastWord is using CPU \
             transcription.\n\nThe GPU is roughly 10x faster. To enable it, run\n\
             windows\\scripts\\setup-gpu.ps1\n(installs the CUDA toolkit — needs \
             one admin confirmation — and rebuilds the transcription engine).\n\n\
             Open the scripts folder now?",
        );
        let caption = wide("FastWord — GPU available");
        let clicked = unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(caption.as_ptr()),
                MB_YESNO | MB_ICONINFORMATION,
            )
        };
        if clicked == IDYES {
            if let Some(dir) = scripts_dir() {
                crate::shell_open(&dir.to_string_lossy());
            } else {
                crate::logging::log("scripts folder not found next to the app or repo");
            }
        }
    });
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_only_for_cpu_backend_with_nvidia_and_not_shown() {
        assert!(should_offer("cpu", true, false));
    }

    #[test]
    fn no_offer_when_already_on_gpu() {
        assert!(!should_offer("cuda", true, false));
        assert!(!should_offer("vulkan", true, false));
    }

    #[test]
    fn no_offer_without_nvidia() {
        assert!(!should_offer("cpu", false, false));
    }

    #[test]
    fn no_offer_twice() {
        assert!(!should_offer("cpu", true, true));
    }
}
