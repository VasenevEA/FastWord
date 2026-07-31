// Minimal file logger: %LOCALAPPDATA%\fastword\fastword-win.log.
// The app runs without a console (windows subsystem), so this is the only
// place diagnostics go.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

fn log_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fastword")
        .join("fastword-win.log")
}

static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn file() -> &'static Mutex<Option<std::fs::File>> {
    LOG.get_or_init(|| {
        let path = log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Start fresh when the log grows past ~1 MB.
        let truncate = std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false);
        let f = OpenOptions::new()
            .create(true)
            .append(!truncate)
            .write(true)
            .truncate(truncate)
            .open(&path)
            .ok();
        Mutex::new(f)
    })
}

pub fn log(msg: &str) {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{stamp}] {msg}\n");
    eprint!("{line}");
    if let Ok(mut guard) = file().lock() {
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }
}
