// Whisper model catalog — mirrors macOS ModelCatalog.swift (same files,
// sizes and download URLs; multilingual models only). Models live in
// %LOCALAPPDATA%\fastword\models\. Downloads run curl.exe (bundled with
// Windows 10+) in the background; progress is polled from the .partial
// file size.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;

pub struct WhisperModel {
    pub filename: &'static str,
    pub display: &'static str,
    pub size_bytes: u64,
}

pub const CATALOG: [WhisperModel; 9] = [
    WhisperModel { filename: "ggml-tiny.bin", display: "Whisper Tiny", size_bytes: 77_700_000 },
    WhisperModel { filename: "ggml-base.bin", display: "Whisper Base", size_bytes: 147_900_000 },
    WhisperModel { filename: "ggml-small.bin", display: "Whisper Small", size_bytes: 487_600_000 },
    WhisperModel { filename: "ggml-medium.bin", display: "Whisper Medium", size_bytes: 1_530_000_000 },
    WhisperModel {
        filename: "ggml-large-v3-turbo-q5_0.bin",
        display: "Whisper Large v3 Turbo Q5",
        size_bytes: 547_000_000,
    },
    WhisperModel {
        filename: "ggml-large-v3-turbo-q8_0.bin",
        display: "Whisper Large v3 Turbo Q8",
        size_bytes: 874_000_000,
    },
    WhisperModel {
        filename: "ggml-large-v3-turbo.bin",
        display: "Whisper Large v3 Turbo (full)",
        size_bytes: 1_620_000_000,
    },
    WhisperModel {
        filename: "ggml-large-v3-q5_0.bin",
        display: "Whisper Large v3 Q5",
        size_bytes: 1_080_000_000,
    },
    WhisperModel {
        filename: "ggml-large-v3.bin",
        display: "Whisper Large v3 (full)",
        size_bytes: 3_100_000_000,
    },
];

pub fn models_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fastword")
        .join("models")
}

pub fn local_path(m: &WhisperModel) -> PathBuf {
    models_dir().join(m.filename)
}

pub fn download_url(m: &WhisperModel) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", m.filename)
}

pub fn is_downloaded(m: &WhisperModel) -> bool {
    local_path(m).exists()
}

pub fn size_string(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes / 1_000_000)
    }
}

// ---------------------------------------------------------------------------
// Background download (at most one at a time).

/// Index into CATALOG of the model being downloaded, -1 = none.
static DOWNLOADING: AtomicI32 = AtomicI32::new(-1);
/// -1 no result; 0 ok; 1 failed (reset when the UI picks it up).
static LAST_RESULT: AtomicI32 = AtomicI32::new(-1);
static DOWNLOADED_BYTES: AtomicU64 = AtomicU64::new(0);
static CURL: Mutex<Option<Child>> = Mutex::new(None);

pub fn downloading_index() -> Option<usize> {
    let i = DOWNLOADING.load(Ordering::Relaxed);
    (i >= 0).then_some(i as usize)
}

pub fn progress_bytes() -> u64 {
    DOWNLOADED_BYTES.load(Ordering::Relaxed)
}

/// Returns Some(true)=success, Some(false)=failure once after a download
/// finishes; None otherwise.
pub fn take_result() -> Option<bool> {
    match LAST_RESULT.swap(-1, Ordering::Relaxed) {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// Start downloading CATALOG[index]; no-op if a download is running.
pub fn start_download(index: usize) -> bool {
    if DOWNLOADING
        .compare_exchange(-1, index as i32, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }
    DOWNLOADED_BYTES.store(0, Ordering::Relaxed);
    let model = &CATALOG[index];
    let dest = local_path(model);
    let partial = dest.with_extension("bin.partial");
    let url = download_url(model);
    crate::logging::log(&format!("downloading {} -> {}", url, dest.display()));

    std::thread::spawn(move || {
        let _ = std::fs::create_dir_all(models_dir());
        let _ = std::fs::remove_file(&partial);

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        let child = Command::new("curl.exe")
            .args(["-L", "--fail", "-sS", "-o"])
            .arg(&partial)
            .arg(&url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();

        let ok = match child {
            Ok(child) => {
                *CURL.lock().unwrap() = Some(child);
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    if let Ok(md) = std::fs::metadata(&partial) {
                        DOWNLOADED_BYTES.store(md.len(), Ordering::Relaxed);
                    }
                    let mut guard = CURL.lock().unwrap();
                    match guard.as_mut().map(|c| c.try_wait()) {
                        Some(Ok(Some(status))) => {
                            *guard = None;
                            break status.success();
                        }
                        Some(Ok(None)) => continue,
                        // Cancelled (killed + cleared) or wait error.
                        _ => break false,
                    }
                }
            }
            Err(e) => {
                crate::logging::log(&format!("curl spawn failed: {e:#}"));
                false
            }
        };

        let ok = ok && std::fs::rename(&partial, &dest).is_ok();
        if !ok {
            let _ = std::fs::remove_file(&partial);
        }
        crate::logging::log(&format!(
            "download {}: {}",
            if ok { "finished" } else { "failed/cancelled" },
            dest.display()
        ));
        LAST_RESULT.store(if ok { 0 } else { 1 }, Ordering::Relaxed);
        DOWNLOADING.store(-1, Ordering::SeqCst);
    });
    true
}

pub fn cancel_download() {
    if let Some(mut child) = CURL.lock().unwrap().take() {
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_macos_list() {
        assert_eq!(CATALOG.len(), 9);
        assert!(CATALOG.iter().any(|m| m.filename == "ggml-large-v3-turbo-q5_0.bin"));
        // English-only .en variants are deliberately excluded.
        assert!(CATALOG.iter().all(|m| !m.filename.contains(".en")));
    }

    #[test]
    fn download_urls_point_at_hf() {
        for m in &CATALOG {
            let url = download_url(m);
            assert!(url.starts_with("https://huggingface.co/ggerganov/whisper.cpp/"));
            assert!(url.ends_with(m.filename));
        }
    }

    #[test]
    fn size_strings() {
        assert_eq!(size_string(547_000_000), "547 MB");
        assert_eq!(size_string(1_620_000_000), "1.6 GB");
        assert_eq!(size_string(3_100_000_000), "3.1 GB");
    }
}
