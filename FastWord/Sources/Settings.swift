import AppKit
import Foundation

enum SettingsKey {
    static let livePreviewEnabled = "livePreviewEnabled"
    static let hotkey = "hotkey"
    static let hotkeySecondary = "hotkeySecondary"
    static let hotkeyUseCombo = "hotkeyUseCombo"
    static let language = "language"
    static let transcriptionLanguage = "transcriptionLanguage"
    static let idleEviction = "idleEviction"
    static let audioHandling = "audioHandling"
    static let activeModel = "activeModel"
    static let skipEmpty = "skipEmpty"
    static let cleanupEnabled = "cleanupEnabled"
}

/// How FastWord should treat background audio when the user starts dictating.
enum AudioHandlingChoice: String, CaseIterable, Identifiable {
    case off
    case pauseResume
    case muteSystem

    var id: String { rawValue }

    var localizationKey: String {
        switch self {
        case .off: return "audio.off"
        case .pauseResume: return "audio.pause_resume"
        case .muteSystem: return "audio.mute_system"
        }
    }

    var displayName: String { NSLocalizedString(localizationKey, comment: "") }
}

/// How long the model stays in RAM after the last transcription before being
/// unloaded. Trade-off: shorter = less idle RAM, longer = no first-use delay.
enum IdleEvictionChoice: String, CaseIterable, Identifiable {
    case oneMinute
    case tenMinutes
    case thirtyMinutes
    case oneHour
    case never

    var id: String { rawValue }

    /// Seconds value sent to the sidecar via FASTWORD_IDLE_EVICT.
    /// `never` is just a very large number — the sidecar checks "idle > N".
    var seconds: Int {
        switch self {
        case .oneMinute: return 60
        case .tenMinutes: return 600
        case .thirtyMinutes: return 1800
        case .oneHour: return 3600
        case .never: return 999_999_999
        }
    }

    var localizationKey: String {
        switch self {
        case .oneMinute: return "idle.one_minute"
        case .tenMinutes: return "idle.ten_minutes"
        case .thirtyMinutes: return "idle.thirty_minutes"
        case .oneHour: return "idle.one_hour"
        case .never: return "idle.never"
        }
    }

    var displayName: String {
        NSLocalizedString(localizationKey, comment: "")
    }
}

/// Languages the Whisper model recognises. Special "auto" lets the model
/// auto-detect; the rest are sent as ISO-639-1 codes to the sidecar.
struct TranscriptionLanguage: Identifiable, Hashable {
    let code: String  // empty = auto-detect
    let name: String
    var id: String { code }

    static let auto = TranscriptionLanguage(code: "", name: "Auto-detect")

    /// Curated short list — the most-used languages. Whisper supports ~99,
    /// but a giant picker is worse UX than a sensible default plus a few common.
    static let all: [TranscriptionLanguage] = [
        .auto,
        TranscriptionLanguage(code: "en", name: "English"),
        TranscriptionLanguage(code: "ru", name: "Русский"),
        TranscriptionLanguage(code: "zh", name: "中文"),
        TranscriptionLanguage(code: "es", name: "Español"),
        TranscriptionLanguage(code: "fr", name: "Français"),
        TranscriptionLanguage(code: "de", name: "Deutsch"),
        TranscriptionLanguage(code: "it", name: "Italiano"),
        TranscriptionLanguage(code: "pt", name: "Português"),
        TranscriptionLanguage(code: "ja", name: "日本語"),
        TranscriptionLanguage(code: "ko", name: "한국어"),
        TranscriptionLanguage(code: "ar", name: "العربية"),
        TranscriptionLanguage(code: "hi", name: "हिन्दी"),
        TranscriptionLanguage(code: "uk", name: "Українська"),
        TranscriptionLanguage(code: "pl", name: "Polski"),
        TranscriptionLanguage(code: "tr", name: "Türkçe"),
        TranscriptionLanguage(code: "nl", name: "Nederlands"),
    ]

    static func find(by code: String) -> TranscriptionLanguage {
        all.first { $0.code == code } ?? .auto
    }

    /// Returns the system's primary language code mapped onto one of our
    /// supported entries (e.g. "en-US" → "en"), or .auto if the system
    /// language is something we don't list explicitly.
    static func systemDefault() -> TranscriptionLanguage {
        guard let raw = Locale.preferredLanguages.first else { return .auto }
        let code = String(raw.prefix(2)).lowercased()
        return find(by: code)
    }

    /// Short hint phrase in the given language, used as Whisper's
    /// `initial_prompt` when the user keeps the picker on "Auto-detect".
    /// Source of the trick: Superwhisper's documented vocabulary-hint
    /// approach — Whisper biases toward the language of words it sees in
    /// the prompt, which dramatically reduces the rate of detecting short
    /// non-English clips as English.
    static func promptHint(forCode code: String) -> String? {
        switch code {
        case "ru": return "Это голосовая заметка на русском."
        case "en": return "This is a voice note in English."
        case "zh": return "这是一条中文语音记录。"
        case "es": return "Esta es una nota de voz en español."
        case "fr": return "Ceci est une note vocale en français."
        case "de": return "Dies ist eine Sprachnotiz auf Deutsch."
        case "it": return "Questa è una nota vocale in italiano."
        case "pt": return "Esta é uma nota de voz em português."
        case "ja": return "これは日本語の音声メモです。"
        case "ko": return "이것은 한국어 음성 메모입니다."
        case "ar": return "هذه ملاحظة صوتية باللغة العربية."
        case "hi": return "यह हिंदी में एक वॉइस नोट है।"
        case "uk": return "Це голосова нотатка українською."
        case "pl": return "To jest notatka głosowa po polsku."
        case "tr": return "Bu Türkçe bir sesli nottur."
        case "nl": return "Dit is een spraaknotitie in het Nederlands."
        default: return nil
        }
    }
}

enum LanguageChoice: String, CaseIterable, Identifiable {
    case system = ""
    case english = "en"
    case russian = "ru"
    case chineseSimplified = "zh-Hans"

    var id: String { rawValue }

    var displayName: String {
        let key: String
        switch self {
        case .system: key = "language.system"
        case .english: key = "language.english"
        case .russian: key = "language.russian"
        case .chineseSimplified: key = "language.chinese_simplified"
        }
        return NSLocalizedString(key, comment: "")
    }
}

/// How a configured second key relates to the primary one.
enum HotkeyComboMode: String, CaseIterable, Identifiable {
    /// Recording fires when *either* the primary or the secondary key is
    /// held. Use this when you want two keys that behave interchangeably
    /// (e.g. one Mac keyboard and one external keyboard with different
    /// available modifiers).
    case either
    /// Recording fires only when *both* primary and secondary are held
    /// together. Classic chord shortcut.
    case both

    var id: String { rawValue }
    var displayName: String { NSLocalizedString("combo.\(rawValue)", comment: "") }
}

enum HotkeyChoice: String, CaseIterable, Identifiable {
    case rightOption
    case leftOption
    case rightCommand
    case leftCommand
    case rightControl
    case leftControl
    case rightShift
    case leftShift
    case fn

    var id: String { rawValue }

    var displayName: String {
        let key: String
        switch self {
        case .rightOption: key = "hotkey.right_option"
        case .leftOption: key = "hotkey.left_option"
        case .rightCommand: key = "hotkey.right_command"
        case .leftCommand: key = "hotkey.left_command"
        case .rightControl: key = "hotkey.right_control"
        case .leftControl: key = "hotkey.left_control"
        case .rightShift: key = "hotkey.right_shift"
        case .leftShift: key = "hotkey.left_shift"
        case .fn: key = "hotkey.fn"
        }
        return NSLocalizedString(key, comment: "")
    }

    var keyCode: Int64 {
        switch self {
        case .rightOption: return 61
        case .leftOption: return 58
        case .rightCommand: return 54
        case .leftCommand: return 55
        case .rightControl: return 62
        case .leftControl: return 59
        case .rightShift: return 60
        case .leftShift: return 56
        case .fn: return 63
        }
    }

    var modifierFlag: CGEventFlags {
        switch self {
        case .rightOption, .leftOption: return .maskAlternate
        case .rightCommand, .leftCommand: return .maskCommand
        case .rightControl, .leftControl: return .maskControl
        case .rightShift, .leftShift: return .maskShift
        case .fn: return .maskSecondaryFn
        }
    }
}

enum AppSettings {
    static let hotkeyChangedNotification = Notification.Name("FastWord.hotkeyChanged")
    static let idleEvictionChangedNotification = Notification.Name("FastWord.idleEvictionChanged")
    static let activeModelChangedNotification = Notification.Name("FastWord.activeModelChanged")

    static var livePreviewEnabled: Bool {
        get { UserDefaults.standard.bool(forKey: SettingsKey.livePreviewEnabled) }
        set { UserDefaults.standard.set(newValue, forKey: SettingsKey.livePreviewEnabled) }
    }

    static var hotkey: HotkeyChoice {
        get {
            let raw = UserDefaults.standard.string(forKey: SettingsKey.hotkey) ?? ""
            return HotkeyChoice(rawValue: raw) ?? .rightOption
        }
        set {
            UserDefaults.standard.set(newValue.rawValue, forKey: SettingsKey.hotkey)
            NotificationCenter.default.post(name: hotkeyChangedNotification, object: nil)
        }
    }

    /// Optional second modifier — when `useComboHotkey` is true, both must
    /// be held down simultaneously to trigger recording.
    static var hotkeySecondary: HotkeyChoice {
        get {
            let raw = UserDefaults.standard.string(forKey: SettingsKey.hotkeySecondary) ?? ""
            return HotkeyChoice(rawValue: raw) ?? .rightControl
        }
        set {
            UserDefaults.standard.set(newValue.rawValue, forKey: SettingsKey.hotkeySecondary)
            NotificationCenter.default.post(name: hotkeyChangedNotification, object: nil)
        }
    }

    /// When true, the hotkey takes a second modifier into account
    /// (the meaning of which is controlled by `hotkeyComboMode`).
    static var useComboHotkey: Bool {
        get { UserDefaults.standard.bool(forKey: SettingsKey.hotkeyUseCombo) }
        set {
            UserDefaults.standard.set(newValue, forKey: SettingsKey.hotkeyUseCombo)
            NotificationCenter.default.post(name: hotkeyChangedNotification, object: nil)
        }
    }

    static var hotkeyComboMode: HotkeyComboMode {
        get {
            let raw = UserDefaults.standard.string(forKey: "hotkeyComboMode") ?? ""
            return HotkeyComboMode(rawValue: raw) ?? .either
        }
        set {
            UserDefaults.standard.set(newValue.rawValue, forKey: "hotkeyComboMode")
            NotificationCenter.default.post(name: hotkeyChangedNotification, object: nil)
        }
    }

    static var transcriptionLanguageCode: String {
        get { UserDefaults.standard.string(forKey: SettingsKey.transcriptionLanguage) ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: SettingsKey.transcriptionLanguage) }
    }

    /// On first launch, the user has no preference yet — default to the
    /// system language rather than leaving "Auto-detect" on. Whisper's
    /// auto-detect is unreliable for short clips, and a wrong guess produces
    /// a confusingly translated transcript (Russian audio → English
    /// gibberish). System-language is a much better starting point; the user
    /// can still pick "Auto" or override later.
    static func initializeTranscriptionLanguageIfNeeded() {
        guard UserDefaults.standard.object(forKey: SettingsKey.transcriptionLanguage) == nil else {
            return
        }
        let code = TranscriptionLanguage.systemDefault().code
        UserDefaults.standard.set(code, forKey: SettingsKey.transcriptionLanguage)
    }

    /// Filename of the user's chosen model. Empty string means "use the
    /// bundled default". Storing the filename (not a full path) keeps the
    /// reference stable across app/model relocations.
    static var activeModelFilename: String {
        get { UserDefaults.standard.string(forKey: SettingsKey.activeModel) ?? "" }
        set {
            UserDefaults.standard.set(newValue, forKey: SettingsKey.activeModel)
            NotificationCenter.default.post(name: activeModelChangedNotification, object: nil)
        }
    }

    static var audioHandling: AudioHandlingChoice {
        get {
            let raw = UserDefaults.standard.string(forKey: SettingsKey.audioHandling) ?? ""
            return AudioHandlingChoice(rawValue: raw) ?? .off
        }
        set { UserDefaults.standard.set(newValue.rawValue, forKey: SettingsKey.audioHandling) }
    }

    /// When true, the sidecar uses a stricter no-speech threshold and the
    /// post-processor's guardrails kick in; clearly-silent recordings come
    /// back as an empty string and are not pasted.
    static var skipEmpty: Bool {
        get {
            if UserDefaults.standard.object(forKey: SettingsKey.skipEmpty) == nil { return true }
            return UserDefaults.standard.bool(forKey: SettingsKey.skipEmpty)
        }
        set { UserDefaults.standard.set(newValue, forKey: SettingsKey.skipEmpty) }
    }

    /// When true, run the programmatic post-processor on every transcript
    /// (strip known YouTube-credit-style hallucinations, collapse repeated
    /// words, trim leading punctuation noise).
    static var cleanupEnabled: Bool {
        get {
            if UserDefaults.standard.object(forKey: SettingsKey.cleanupEnabled) == nil { return true }
            return UserDefaults.standard.bool(forKey: SettingsKey.cleanupEnabled)
        }
        set { UserDefaults.standard.set(newValue, forKey: SettingsKey.cleanupEnabled) }
    }

    static var idleEviction: IdleEvictionChoice {
        get {
            let raw = UserDefaults.standard.string(forKey: SettingsKey.idleEviction) ?? ""
            return IdleEvictionChoice(rawValue: raw) ?? .tenMinutes
        }
        set {
            UserDefaults.standard.set(newValue.rawValue, forKey: SettingsKey.idleEviction)
            NotificationCenter.default.post(name: idleEvictionChangedNotification, object: nil)
        }
    }
}
