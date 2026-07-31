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
