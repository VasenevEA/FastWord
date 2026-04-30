import AppKit
import Foundation

enum SettingsKey {
    static let livePreviewEnabled = "livePreviewEnabled"
    static let hotkey = "hotkey"
    static let language = "language"
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
}
