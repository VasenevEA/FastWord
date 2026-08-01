// Language handling: the same curated list and prompt-hint trick as the
// macOS app. When the language is "auto", a short hint phrase in the system
// language is passed as Whisper's initial_prompt — it biases language
// detection and dramatically reduces wrong-language results on short clips.

/// ISO-639-1 codes the UI/config knows about (Whisper itself supports ~99).
pub const SUPPORTED: &[&str] = &[
    "en", "ru", "zh", "es", "fr", "de", "it", "pt", "ja", "ko", "ar", "hi", "uk", "pl", "tr",
    "nl",
];

pub fn prompt_hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "ru" => "Это голосовая заметка на русском.",
        "en" => "This is a voice note in English.",
        "zh" => "这是一条中文语音记录。",
        "es" => "Esta es una nota de voz en español.",
        "fr" => "Ceci est une note vocale en français.",
        "de" => "Dies ist eine Sprachnotiz auf Deutsch.",
        "it" => "Questa è una nota vocale in italiano.",
        "pt" => "Esta é uma nota de voz em português.",
        "ja" => "これは日本語の音声メモです。",
        "ko" => "이것은 한국어 음성 메모입니다.",
        "ar" => "هذه ملاحظة صوتية باللغة العربية.",
        "hi" => "यह हिंदी में एक वॉइस नोट है।",
        "uk" => "Це голосова нотатка українською.",
        "pl" => "To jest notatka głosowa po polsku.",
        "tr" => "Bu Türkçe bir sesli nottur.",
        "nl" => "Dit is een spraaknotitie in het Nederlands.",
        _ => return None,
    })
}

/// Map a BCP-47 locale name ("ru-RU", "en_US") onto a supported code,
/// or None if we don't list it.
pub fn normalize_locale(locale: &str) -> Option<&'static str> {
    let code = locale
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    SUPPORTED.iter().find(|&&c| c == code).copied()
}

/// The system display language mapped to a supported code, if any.
pub fn system_language() -> Option<&'static str> {
    sys_locale::get_locale().and_then(|l| normalize_locale(&l))
}

/// UI strings, localized like the macOS app (English, Русский, 简体中文 —
/// same set as FastWord/Resources/*.lproj).
pub struct UiStrings {
    pub menu_open_history: &'static str,
    pub menu_open_log: &'static str,
    pub menu_quit: &'static str,
    pub hud_listening: &'static str,
    pub hud_transcribing: &'static str,
    /// Tray tooltip; `{key}` is replaced with the configured hotkey name.
    pub tip_hold_to_dictate: &'static str,
    pub gpu_title: &'static str,
    pub gpu_text: &'static str,
    // Main window
    pub menu_settings: &'static str,
    pub menu_models: &'static str,
    pub tab_history: &'static str,
    pub tab_settings: &'static str,
    pub tab_models: &'static str,
    pub search_label: &'static str,
    pub btn_copy: &'static str,
    pub btn_delete: &'static str,
    pub btn_clear_all: &'static str,
    pub col_time: &'static str,
    pub col_text: &'static str,
    pub lbl_hotkey: &'static str,
    pub lbl_secondary: &'static str,
    pub lbl_combo: &'static str,
    pub lbl_language: &'static str,
    pub lbl_idle: &'static str,
    pub item_none: &'static str,
    pub combo_either: &'static str,
    pub combo_both: &'static str,
    pub lang_system: &'static str,
    pub lang_auto: &'static str,
    pub idle_1m: &'static str,
    pub idle_10m: &'static str,
    pub idle_30m: &'static str,
    pub idle_1h: &'static str,
    pub idle_never: &'static str,
    pub chk_skip_empty: &'static str,
    pub chk_cleanup: &'static str,
    pub chk_hud: &'static str,
    pub chk_beeps: &'static str,
    pub chk_autostart: &'static str,
    pub btn_save: &'static str,
    pub restart_prompt: &'static str,
    pub col_model: &'static str,
    pub col_size: &'static str,
    pub col_status: &'static str,
    pub status_active: &'static str,
    pub status_downloaded: &'static str,
    pub status_not_downloaded: &'static str,
    /// `{pct}` replaced with the percentage.
    pub status_downloading: &'static str,
    pub btn_download: &'static str,
    pub btn_cancel: &'static str,
    pub btn_use: &'static str,
    pub btn_delete_file: &'static str,
    pub model_missing_prompt: &'static str,
}

static EN: UiStrings = UiStrings {
    menu_open_history: "Open history folder",
    menu_open_log: "Open log",
    menu_quit: "Quit FastWord",
    hud_listening: "Listening…",
    hud_transcribing: "Transcribing…",
    tip_hold_to_dictate: "FastWord — hold {key} to dictate",
    gpu_title: "FastWord — GPU available",
    gpu_text: "An NVIDIA GPU was detected, but FastWord is using CPU \
               transcription.\n\nThe GPU is roughly 10x faster. To enable it, run\n\
               windows\\scripts\\setup-gpu.ps1\n(installs the CUDA toolkit — needs \
               one admin confirmation — and rebuilds the transcription engine).\n\n\
               Open the scripts folder now?",
    menu_settings: "Settings",
    menu_models: "Models",
    tab_history: "History",
    tab_settings: "Settings",
    tab_models: "Models",
    search_label: "Search:",
    btn_copy: "Copy",
    btn_delete: "Delete",
    btn_clear_all: "Clear all",
    col_time: "Time",
    col_text: "Text",
    lbl_hotkey: "Hotkey",
    lbl_secondary: "Second key",
    lbl_combo: "Combination",
    lbl_language: "Language",
    lbl_idle: "Unload model after",
    item_none: "(none)",
    combo_either: "either key",
    combo_both: "both together",
    lang_system: "System language",
    lang_auto: "Auto-detect",
    idle_1m: "1 minute",
    idle_10m: "10 minutes",
    idle_30m: "30 minutes",
    idle_1h: "1 hour",
    idle_never: "never",
    chk_skip_empty: "Skip silent recordings",
    chk_cleanup: "Clean transcription artefacts",
    chk_hud: "On-screen indicator (HUD)",
    chk_beeps: "Sound cues",
    chk_autostart: "Start with Windows",
    btn_save: "Save",
    restart_prompt: "Settings saved. Restart FastWord now to apply them?",
    col_model: "Model",
    col_size: "Size",
    col_status: "Status",
    status_active: "active",
    status_downloaded: "downloaded",
    status_not_downloaded: "—",
    status_downloading: "downloading {pct}%",
    btn_download: "Download",
    btn_cancel: "Cancel",
    btn_use: "Use",
    btn_delete_file: "Delete file",
    model_missing_prompt: "No speech model is installed yet. Open the Models tab to download one?",
};

static RU: UiStrings = UiStrings {
    menu_open_history: "Папка истории",
    menu_open_log: "Открыть лог",
    menu_quit: "Выйти из FastWord",
    hud_listening: "Слушаю…",
    hud_transcribing: "Распознаю…",
    tip_hold_to_dictate: "FastWord — зажмите {key} для диктовки",
    gpu_title: "FastWord — доступна GPU",
    gpu_text: "Обнаружена NVIDIA GPU, но FastWord распознаёт речь на CPU.\n\n\
               GPU примерно в 10 раз быстрее. Чтобы включить её, запустите\n\
               windows\\scripts\\setup-gpu.ps1\n(установит CUDA — потребуется одно \
               подтверждение администратора — и пересоберёт движок).\n\n\
               Открыть папку со скриптами?",
    menu_settings: "Настройки",
    menu_models: "Модели",
    tab_history: "История",
    tab_settings: "Настройки",
    tab_models: "Модели",
    search_label: "Поиск:",
    btn_copy: "Копировать",
    btn_delete: "Удалить",
    btn_clear_all: "Очистить всё",
    col_time: "Время",
    col_text: "Текст",
    lbl_hotkey: "Горячая клавиша",
    lbl_secondary: "Вторая клавиша",
    lbl_combo: "Комбинация",
    lbl_language: "Язык распознавания",
    lbl_idle: "Выгружать модель через",
    item_none: "(нет)",
    combo_either: "любая из двух",
    combo_both: "обе вместе",
    lang_system: "Язык системы",
    lang_auto: "Автоопределение",
    idle_1m: "1 минуту",
    idle_10m: "10 минут",
    idle_30m: "30 минут",
    idle_1h: "1 час",
    idle_never: "никогда",
    chk_skip_empty: "Пропускать тихие записи",
    chk_cleanup: "Чистить артефакты распознавания",
    chk_hud: "Экранный индикатор (HUD)",
    chk_beeps: "Звуковые сигналы",
    chk_autostart: "Запускать при входе в Windows",
    btn_save: "Сохранить",
    restart_prompt: "Настройки сохранены. Перезапустить FastWord сейчас, чтобы применить их?",
    col_model: "Модель",
    col_size: "Размер",
    col_status: "Статус",
    status_active: "активна",
    status_downloaded: "скачана",
    status_not_downloaded: "—",
    status_downloading: "загрузка {pct}%",
    btn_download: "Скачать",
    btn_cancel: "Отмена",
    btn_use: "Использовать",
    btn_delete_file: "Удалить файл",
    model_missing_prompt: "Модель распознавания ещё не установлена. Открыть раздел «Модели», чтобы скачать её?",
};

static ZH: UiStrings = UiStrings {
    menu_open_history: "打开历史文件夹",
    menu_open_log: "打开日志",
    menu_quit: "退出 FastWord",
    hud_listening: "正在聆听…",
    hud_transcribing: "正在识别…",
    tip_hold_to_dictate: "FastWord — 按住 {key} 开始听写",
    gpu_title: "FastWord — 检测到 GPU",
    gpu_text: "检测到 NVIDIA GPU，但 FastWord 正在使用 CPU 识别。\n\n\
               GPU 大约快 10 倍。要启用它，请运行\n\
               windows\\scripts\\setup-gpu.ps1\n（安装 CUDA 工具包 — 需要一次\
               管理员确认 — 并重新构建识别引擎）。\n\n\
               现在打开脚本文件夹吗？",
    menu_settings: "设置",
    menu_models: "模型",
    tab_history: "历史",
    tab_settings: "设置",
    tab_models: "模型",
    search_label: "搜索：",
    btn_copy: "复制",
    btn_delete: "删除",
    btn_clear_all: "全部清除",
    col_time: "时间",
    col_text: "文本",
    lbl_hotkey: "快捷键",
    lbl_secondary: "第二按键",
    lbl_combo: "组合方式",
    lbl_language: "识别语言",
    lbl_idle: "空闲后卸载模型",
    item_none: "（无）",
    combo_either: "任意一个",
    combo_both: "同时按住",
    lang_system: "系统语言",
    lang_auto: "自动检测",
    idle_1m: "1 分钟",
    idle_10m: "10 分钟",
    idle_30m: "30 分钟",
    idle_1h: "1 小时",
    idle_never: "从不",
    chk_skip_empty: "跳过无声录音",
    chk_cleanup: "清理识别伪影",
    chk_hud: "屏幕指示器 (HUD)",
    chk_beeps: "提示音",
    chk_autostart: "开机时启动",
    btn_save: "保存",
    restart_prompt: "设置已保存。现在重启 FastWord 以应用吗？",
    col_model: "模型",
    col_size: "大小",
    col_status: "状态",
    status_active: "使用中",
    status_downloaded: "已下载",
    status_not_downloaded: "—",
    status_downloading: "下载中 {pct}%",
    btn_download: "下载",
    btn_cancel: "取消",
    btn_use: "使用",
    btn_delete_file: "删除文件",
    model_missing_prompt: "尚未安装语音模型。打开“模型”选项卡进行下载吗？",
};

pub fn ui() -> &'static UiStrings {
    match system_language() {
        Some("ru") => &RU,
        Some("zh") => &ZH,
        _ => &EN,
    }
}

/// Resolve the config `language` value into what gets sent to the sidecar:
/// `(language_code, initial_prompt)`. Empty language code = auto-detect.
pub fn resolve(config_language: &str) -> (String, Option<String>) {
    match config_language.trim().to_lowercase().as_str() {
        "auto" | "" => {
            // Auto-detect, but bias toward the system language via the
            // prompt-hint trick (same as macOS "Auto-detect").
            let hint = system_language()
                .and_then(prompt_hint)
                .map(str::to_string);
            (String::new(), hint)
        }
        "system" => match system_language() {
            Some(code) => (code.to_string(), None),
            None => (String::new(), None),
        },
        code => {
            // Be forgiving about locale-style values: "ru-RU" → "ru".
            // Unlisted codes pass through as-is — Whisper knows ~99
            // languages, far more than our curated list.
            let normalized = code.split(['-', '_']).next().unwrap_or(code).to_string();
            (normalized, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_language_has_a_hint() {
        for code in SUPPORTED {
            assert!(prompt_hint(code).is_some(), "missing hint for {code}");
        }
    }

    #[test]
    fn unknown_language_has_no_hint() {
        assert!(prompt_hint("xx").is_none());
    }

    #[test]
    fn normalizes_locale_names() {
        assert_eq!(normalize_locale("ru-RU"), Some("ru"));
        assert_eq!(normalize_locale("en_US"), Some("en"));
        assert_eq!(normalize_locale("zh-Hans-CN"), Some("zh"));
        assert_eq!(normalize_locale("tlh"), None);
        assert_eq!(normalize_locale(""), None);
    }

    #[test]
    fn explicit_code_resolves_as_is() {
        assert_eq!(resolve("ru"), ("ru".to_string(), None));
        assert_eq!(resolve("EN"), ("en".to_string(), None));
    }

    #[test]
    fn auto_resolves_to_empty_language() {
        let (lang, _hint) = resolve("auto");
        assert!(lang.is_empty());
    }
}
