import AppKit
import Carbon.HIToolbox

/// Something that stands between a recorded hotkey and FastWord actually
/// receiving it. Surfaced in Settings the moment a binding is chosen, so a
/// clashing shortcut fails loudly instead of just doing nothing.
struct HotkeyConflict: Equatable {
    enum Kind {
        /// The same binding is already used by FastWord's other slot.
        case duplicate
        /// macOS handles this before any event tap; it will never arrive.
        case reserved
        /// Matches an enabled shortcut in System Settings → Keyboard.
        case systemShortcut
        /// The `fn` key is configured to do something else system-wide.
        case fnRepurposed
        /// A character key with no modifiers — bindable, but it stops typing.
        case typingKey
    }

    let kind: Kind
    /// Human-readable name of what it clashes with, e.g. "Spotlight (⌘Space)".
    let detail: String

    var isBlocking: Bool {
        switch kind {
        case .duplicate, .reserved, .systemShortcut, .fnRepurposed: return true
        case .typingKey: return false
        }
    }

    var message: String {
        switch kind {
        case .duplicate:
            return NSLocalizedString("conflict.duplicate", comment: "")
        case .reserved:
            return String(format: NSLocalizedString("conflict.reserved", comment: ""), detail)
        case .systemShortcut:
            return String(format: NSLocalizedString("conflict.system", comment: ""), detail)
        case .fnRepurposed:
            return NSLocalizedString("conflict.fn", comment: "")
        case .typingKey:
            return String(format: NSLocalizedString("conflict.typing_key", comment: ""), detail)
        }
    }
}

enum HotkeyConflictChecker {

    /// Shift + Control + Option + Command. The `fn` bit is deliberately left
    /// out of comparisons: arrow and function keys set it on their own, so
    /// system shortcut entries carry it spuriously.
    private static let comparableModifiers: UInt64 =
        CGEventFlags.maskShift.rawValue
        | CGEventFlags.maskControl.rawValue
        | CGEventFlags.maskAlternate.rawValue
        | CGEventFlags.maskCommand.rawValue

    static func conflict(for binding: HotkeyBinding, otherBinding: HotkeyBinding?) -> HotkeyConflict? {
        guard !binding.isEmpty else { return nil }

        if let otherBinding, !otherBinding.isEmpty, otherBinding == binding {
            return HotkeyConflict(kind: .duplicate, detail: binding.displayName)
        }
        if let reserved = reservedConflict(for: binding) { return reserved }
        if let system = systemShortcutConflict(for: binding) { return system }
        if let fnIssue = fnConflict(for: binding) { return fnIssue }
        if let typing = typingKeyWarning(for: binding) { return typing }
        return nil
    }

    // MARK: - Reserved by macOS

    /// Combinations macOS consumes above the session event tap, so no amount
    /// of permissions makes them reachable.
    private static func reservedConflict(for binding: HotkeyBinding) -> HotkeyConflict? {
        guard let keyCode = binding.keyCode else { return nil }
        let mods = binding.genericModifierMask & comparableModifiers
        let cmd = CGEventFlags.maskCommand.rawValue
        let opt = CGEventFlags.maskAlternate.rawValue
        let shift = CGEventFlags.maskShift.rawValue

        switch (keyCode, mods) {
        case (Int64(kVK_Tab), cmd), (Int64(kVK_Tab), cmd | shift):
            return HotkeyConflict(kind: .reserved, detail: "⌘⇥ — " + NSLocalizedString("conflict.app_switcher", comment: ""))
        case (Int64(kVK_ANSI_Grave), cmd), (Int64(kVK_ANSI_Grave), cmd | shift):
            return HotkeyConflict(kind: .reserved, detail: "⌘` — " + NSLocalizedString("conflict.window_switcher", comment: ""))
        case (Int64(kVK_Escape), cmd | opt), (Int64(kVK_Escape), cmd | opt | shift):
            return HotkeyConflict(kind: .reserved, detail: "⌥⌘⎋ — " + NSLocalizedString("conflict.force_quit", comment: ""))
        default:
            return nil
        }
    }

    // MARK: - System Settings → Keyboard → Shortcuts

    /// Names for the symbolic-hotkey IDs that are stable and worth naming.
    /// Anything else falls back to a generic label rather than risking a
    /// wrong one.
    private static let symbolicHotkeyNames: [Int: String] = [
        28: "conflict.name.screenshot_file",
        29: "conflict.name.screenshot_clipboard",
        30: "conflict.name.screenshot_area_file",
        31: "conflict.name.screenshot_area_clipboard",
        32: "conflict.name.mission_control",
        33: "conflict.name.app_windows",
        60: "conflict.name.previous_input_source",
        61: "conflict.name.next_input_source",
        64: "conflict.name.spotlight",
        65: "conflict.name.spotlight_finder",
        79: "conflict.name.space_left",
        80: "conflict.name.space_right",
        98: "conflict.name.help_menu",
        118: "conflict.name.desktop_1",
        119: "conflict.name.desktop_2",
        120: "conflict.name.desktop_3",
        121: "conflict.name.desktop_4",
        160: "conflict.name.launchpad",
        163: "conflict.name.notification_center",
        175: "conflict.name.notification_center",
        184: "conflict.name.screenshot_options",
    ]

    private static func systemShortcutConflict(for binding: HotkeyBinding) -> HotkeyConflict? {
        guard let keyCode = binding.keyCode else { return nil }
        let mods = binding.genericModifierMask & comparableModifiers

        guard let raw = CFPreferencesCopyAppValue(
            "AppleSymbolicHotKeys" as CFString,
            "com.apple.symbolichotkeys" as CFString
        ) as? [String: Any] else { return nil }

        for (idString, entry) in raw {
            guard let entry = entry as? [String: Any],
                  (entry["enabled"] as? Bool) == true,
                  let value = entry["value"] as? [String: Any],
                  (value["type"] as? String) == "standard",
                  let parameters = value["parameters"] as? [Any],
                  parameters.count >= 3,
                  let otherKey = (parameters[1] as? NSNumber)?.int64Value,
                  let otherMask = (parameters[2] as? NSNumber)?.uint64Value,
                  otherKey != 65535,
                  otherKey == keyCode,
                  otherMask & comparableModifiers == mods
            else { continue }

            let name: String
            if let id = Int(idString), let key = symbolicHotkeyNames[id] {
                name = NSLocalizedString(key, comment: "")
            } else {
                name = NSLocalizedString("conflict.name.generic", comment: "")
            }
            return HotkeyConflict(kind: .systemShortcut, detail: "\(name) (\(binding.displayName))")
        }
        return nil
    }

    // MARK: - fn

    /// `fn` only reaches FastWord when System Settings → Keyboard leaves it on
    /// "Do Nothing"; otherwise the system claims the press.
    private static func fnConflict(for binding: HotkeyBinding) -> HotkeyConflict? {
        guard binding.modifierKeyCodes.contains(ModifierKey.fn.rawValue) else { return nil }
        let usage = CFPreferencesCopyAppValue(
            "AppleFnUsageType" as CFString,
            "com.apple.HIToolbox" as CFString
        ) as? NSNumber
        // Absent means the macOS default, which is not "Do Nothing" on
        // machines with a globe key, so treat a missing value as configured.
        guard let usage else { return nil }
        return usage.intValue == 0 ? nil : HotkeyConflict(kind: .fnRepurposed, detail: "fn")
    }

    // MARK: - Bare typing keys

    /// A character key with no modifiers is legal but swallows that character
    /// everywhere while FastWord runs, which is rarely what someone wants.
    private static func typingKeyWarning(for binding: HotkeyBinding) -> HotkeyConflict? {
        guard let keyCode = binding.keyCode, binding.modifierKeyCodes.isEmpty else { return nil }
        let name = HotkeyBinding.keyName(for: keyCode, fallback: binding.keyLabel)
        // Function keys, the numeric keypad and navigation keys don't produce
        // text, so losing them costs nothing.
        guard HotkeyBinding.isTextProducing(keyCode: keyCode) else { return nil }
        return HotkeyConflict(kind: .typingKey, detail: name)
    }
}
