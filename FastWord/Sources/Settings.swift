import AppKit
import Foundation

enum SettingsKey {
    static let livePreviewEnabled = "livePreviewEnabled"
    static let hotkey = "hotkey"
    static let language = "language"
    static let transcriptionLanguage = "transcriptionLanguage"
    static let idleEviction = "idleEviction"
    static let audioHandling = "audioHandling"
}

/// How FastWord should treat background audio when the user starts dictating.
enum AudioHandlingChoice: String, CaseIterable, Identifiable {
    case off
    case pauseResume

    var id: String { rawValue }

    var localizationKey: String {
        switch self {
        case .off: return "audio.off"
        case .pauseResume: return "audio.pause_resume"
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

    static var transcriptionLanguageCode: String {
        get { UserDefaults.standard.string(forKey: SettingsKey.transcriptionLanguage) ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: SettingsKey.transcriptionLanguage) }
    }

    static var audioHandling: AudioHandlingChoice {
        get {
            let raw = UserDefaults.standard.string(forKey: SettingsKey.audioHandling) ?? ""
            return AudioHandlingChoice(rawValue: raw) ?? .off
        }
        set { UserDefaults.standard.set(newValue.rawValue, forKey: SettingsKey.audioHandling) }
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
