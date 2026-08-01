// User configuration, stored as TOML at %APPDATA%\FastWord\config.toml.
// A commented default file is written on first launch.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Push-to-talk key. See hotkey::parse_hotkey for accepted names.
    pub hotkey: String,
    /// Optional second key ("" = none).
    pub hotkey_secondary: String,
    /// "either" — either key alone records; "both" — chord.
    pub combo_mode: String,
    /// "system" (default) — use the Windows display language;
    /// "auto" — let Whisper detect; or an ISO-639-1 code like "ru".
    pub language: String,
    /// Absolute path to a ggml whisper model. Empty → the default location
    /// %LOCALAPPDATA%\fastword\models\ggml-large-v3-turbo-q5_0.bin.
    pub model_path: String,
    /// Seconds of inactivity before the sidecar drops the model from RAM.
    pub idle_evict_secs: u64,
    /// Drop clearly-silent recordings instead of pasting hallucinations.
    pub skip_empty: bool,
    /// Run the transcript post-processor (strip hallucinations, repeats…).
    pub cleanup_enabled: bool,
    /// Audible cues on record start/stop and on errors.
    pub beeps: bool,
    /// Floating on-screen indicator while recording/transcribing (like the
    /// macOS HUD).
    pub hud: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "right_ctrl".into(),
            hotkey_secondary: String::new(),
            combo_mode: "either".into(),
            language: "system".into(),
            model_path: String::new(),
            idle_evict_secs: 600,
            skip_empty: true,
            cleanup_enabled: true,
            beeps: false,
            hud: true,
        }
    }
}

pub const DEFAULT_CONFIG_TOML: &str = r#"# FastWord for Windows configuration.
# Changes take effect after restarting FastWord (tray icon -> Quit).

# Push-to-talk key: hold to record, release to insert the transcript.
# Accepted: left_ctrl, right_ctrl, left_alt, right_alt, left_shift,
# right_shift, left_win, right_win, caps_lock, scroll_lock, pause, f1..f24.
hotkey = "right_ctrl"

# Optional second key ("" = disabled).
hotkey_secondary = ""

# How the second key combines with the first:
# "either" - either key alone triggers recording
# "both"   - both keys must be held together
combo_mode = "either"

# Transcription language:
# "system" - use the Windows display language (recommended)
# "auto"   - let Whisper auto-detect (unreliable on short clips)
# or an ISO code: "ru", "en", "zh", "de", ...
language = "system"

# Path to a ggml whisper.cpp model. Empty means
# %LOCALAPPDATA%\fastword\models\ggml-large-v3-turbo-q5_0.bin
# (run scripts\get-model.ps1 to download it there).
model_path = ""

# Seconds of idle time before the model is dropped from RAM.
idle_evict_secs = 600

# Skip clearly-silent recordings instead of pasting hallucinated text.
skip_empty = true

# Clean common Whisper artefacts (YouTube-credit phrases, word repeats).
cleanup_enabled = true

# Floating on-screen indicator while recording/transcribing (macOS-style HUD).
hud = true

# Beep on record start/stop and on errors (off by default - the HUD already
# gives feedback).
beeps = false
"#;

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FastWord")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Default model location, matching scripts/get-model.ps1.
pub fn default_model_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fastword")
        .join("models")
        .join("ggml-large-v3-turbo-q5_0.bin")
}

pub fn parse(text: &str) -> Result<Config> {
    toml::from_str(text).context("parsing config.toml")
}

/// Load the config, writing the commented default file on first run.
pub fn load_or_create() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        std::fs::create_dir_all(config_dir()).context("creating config dir")?;
        std::fs::write(&path, DEFAULT_CONFIG_TOML).context("writing default config")?;
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path).context("reading config.toml")?;
    parse(&text)
}

impl Config {
    pub fn resolved_model_path(&self) -> PathBuf {
        if self.model_path.trim().is_empty() {
            default_model_path()
        } else {
            PathBuf::from(self.model_path.trim())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_toml_parses_to_defaults() {
        let cfg = parse(DEFAULT_CONFIG_TOML).unwrap();
        let def = Config::default();
        assert_eq!(cfg.hotkey, def.hotkey);
        assert_eq!(cfg.hotkey_secondary, def.hotkey_secondary);
        assert_eq!(cfg.combo_mode, def.combo_mode);
        assert_eq!(cfg.language, def.language);
        assert_eq!(cfg.model_path, def.model_path);
        assert_eq!(cfg.idle_evict_secs, def.idle_evict_secs);
        assert_eq!(cfg.skip_empty, def.skip_empty);
        assert_eq!(cfg.cleanup_enabled, def.cleanup_enabled);
        assert_eq!(cfg.beeps, def.beeps);
        assert_eq!(cfg.hud, def.hud);
    }

    #[test]
    fn hud_replaces_beeps_by_default() {
        let cfg = parse("").unwrap();
        assert!(cfg.hud);
        assert!(!cfg.beeps);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg = parse("hotkey = \"f13\"").unwrap();
        assert_eq!(cfg.hotkey, "f13");
        assert_eq!(cfg.idle_evict_secs, 600);
        assert!(cfg.cleanup_enabled);
    }

    #[test]
    fn empty_config_is_all_defaults() {
        let cfg = parse("").unwrap();
        assert_eq!(cfg.hotkey, "right_ctrl");
    }

    #[test]
    fn bad_toml_is_an_error() {
        assert!(parse("hotkey = [broken").is_err());
    }

    #[test]
    fn explicit_model_path_wins() {
        let cfg = parse("model_path = \"C:/models/tiny.bin\"").unwrap();
        assert_eq!(
            cfg.resolved_model_path(),
            PathBuf::from("C:/models/tiny.bin")
        );
    }

    #[test]
    fn empty_model_path_uses_default_location() {
        let cfg = parse("").unwrap();
        let p = cfg.resolved_model_path();
        assert!(p.ends_with("fastword/models/ggml-large-v3-turbo-q5_0.bin")
            || p.ends_with("fastword\\models\\ggml-large-v3-turbo-q5_0.bin"));
    }
}
