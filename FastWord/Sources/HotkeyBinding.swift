import AppKit
import Carbon.HIToolbox

/// A side-aware modifier key. Unlike `CGEventFlags`, which collapses left and
/// right into a single bit, every modifier here maps onto the device-dependent
/// mask that macOS also sets on the event, so "left ⌥ is down" and "right ⌥ is
/// down" are distinguishable states.
enum ModifierKey: Int64, CaseIterable {
    case leftControl = 59
    case rightControl = 62
    case leftOption = 58
    case rightOption = 61
    case leftShift = 56
    case rightShift = 60
    case leftCommand = 55
    case rightCommand = 54
    case fn = 63

    /// Device-dependent bit from `IOLLEvent.h` (NX_DEVICE*KEYMASK). These are
    /// carried in the raw value of both `CGEventFlags` and
    /// `NSEvent.ModifierFlags`.
    var deviceMask: UInt64 {
        switch self {
        case .leftControl: return 0x0000_0001
        case .rightControl: return 0x0000_2000
        case .leftShift: return 0x0000_0002
        case .rightShift: return 0x0000_0004
        case .leftCommand: return 0x0000_0008
        case .rightCommand: return 0x0000_0010
        case .leftOption: return 0x0000_0020
        case .rightOption: return 0x0000_0040
        case .fn: return 0x0080_0000  // NX_SECONDARYFNMASK
        }
    }

    /// The side-agnostic mask (`.maskControl`, `.maskAlternate`, …).
    var genericMask: UInt64 {
        switch self {
        case .leftControl, .rightControl: return CGEventFlags.maskControl.rawValue
        case .leftOption, .rightOption: return CGEventFlags.maskAlternate.rawValue
        case .leftShift, .rightShift: return CGEventFlags.maskShift.rawValue
        case .leftCommand, .rightCommand: return CGEventFlags.maskCommand.rawValue
        case .fn: return CGEventFlags.maskSecondaryFn.rawValue
        }
    }

    /// The same physical modifier on the other side of the keyboard, if any.
    var sibling: ModifierKey? {
        switch self {
        case .leftControl: return .rightControl
        case .rightControl: return .leftControl
        case .leftOption: return .rightOption
        case .rightOption: return .leftOption
        case .leftShift: return .rightShift
        case .rightShift: return .leftShift
        case .leftCommand: return .rightCommand
        case .rightCommand: return .leftCommand
        case .fn: return nil
        }
    }

    /// Sort order used when rendering a chord, matching the macOS convention
    /// (fn, ⌃, ⌥, ⇧, ⌘).
    var displayOrder: Int {
        switch self {
        case .fn: return 0
        case .leftControl, .rightControl: return 1
        case .leftOption, .rightOption: return 2
        case .leftShift, .rightShift: return 3
        case .leftCommand, .rightCommand: return 4
        }
    }

    var symbol: String {
        switch self {
        case .leftControl, .rightControl: return "⌃"
        case .leftOption, .rightOption: return "⌥"
        case .leftShift, .rightShift: return "⇧"
        case .leftCommand, .rightCommand: return "⌘"
        case .fn: return "fn"
        }
    }

    /// "L" / "R" suffix, empty for keys that exist only once.
    var sideSuffix: String {
        switch self {
        case .leftControl, .leftOption, .leftShift, .leftCommand: return "L"
        case .rightControl, .rightOption, .rightShift, .rightCommand: return "R"
        case .fn: return ""
        }
    }

    var displayName: String { symbol + sideSuffix }

    /// Whether this specific key is held, given a flags mask.
    ///
    /// Prefers the device bit. Some external keyboards (and some remapping
    /// utilities) only set the generic bit; in that case we report both sides
    /// as held rather than silently never matching.
    func isHeld(in rawFlags: UInt64) -> Bool {
        if rawFlags & deviceMask != 0 { return true }
        guard let sibling else { return rawFlags & genericMask != 0 }
        let anySideBit = deviceMask | sibling.deviceMask
        return (rawFlags & genericMask != 0) && (rawFlags & anySideBit == 0)
    }
}

/// A user-defined push-to-talk trigger: any number of side-aware modifiers,
/// optionally plus one regular key.
///
/// Modifier-only bindings (`⌥R`, `⌃L⌥L`) are the classic FastWord behaviour.
/// Bindings with a regular key (`⌃⌥D`, `F13`) are suppressed while held so the
/// character is not typed into the focused field.
struct HotkeyBinding: Codable, Equatable, Hashable {
    /// Keycode of the non-modifier key; `nil` for modifier-only bindings.
    var keyCode: Int64?
    /// Side-aware keycodes of the modifiers that must be held.
    var modifierKeyCodes: [Int64]
    /// Character captured at record time, used to label layout-dependent keys.
    var keyLabel: String?

    init(keyCode: Int64? = nil, modifierKeyCodes: [Int64] = [], keyLabel: String? = nil) {
        self.keyCode = keyCode
        self.modifierKeyCodes = modifierKeyCodes
        self.keyLabel = keyLabel
    }

    var isEmpty: Bool { keyCode == nil && modifierKeyCodes.isEmpty }

    var modifiers: [ModifierKey] {
        modifierKeyCodes
            .compactMap { ModifierKey(rawValue: $0) }
            .sorted { ($0.displayOrder, $0.rawValue) < ($1.displayOrder, $1.rawValue) }
    }

    /// True when the binding can be triggered by holding modifiers alone.
    var isModifierOnly: Bool { keyCode == nil }

    /// Side-agnostic mask of the bound modifiers, for comparing against
    /// system shortcut definitions (which don't record left/right).
    var genericModifierMask: UInt64 {
        modifiers.reduce(0) { $0 | $1.genericMask }
    }

    /// Whether the key types a character, as opposed to function keys,
    /// navigation keys and the numeric keypad.
    static func isTextProducing(keyCode: Int64) -> Bool {
        if specialKeyNames[keyCode] != nil { return false }
        guard let character = translateToCharacter(keyCode: keyCode) else { return false }
        return character.unicodeScalars.contains { !CharacterSet.controlCharacters.contains($0) }
    }

    var displayName: String {
        if isEmpty { return NSLocalizedString("hotkey.none", comment: "") }
        let mods = modifiers.map(\.displayName).joined()
        guard let keyCode else { return mods }
        let key = HotkeyBinding.keyName(for: keyCode, fallback: keyLabel)
        return mods.isEmpty ? key : "\(mods) \(key)"
    }

    // MARK: - Presets

    static let rightOption = HotkeyBinding(modifierKeyCodes: [ModifierKey.rightOption.rawValue])
    static let rightControl = HotkeyBinding(modifierKeyCodes: [ModifierKey.rightControl.rawValue])

    /// The single-modifier bindings offered as one-click presets in Settings —
    /// the same set the pre-0.7 picker exposed.
    static let presets: [HotkeyBinding] = [
        .rightOption,
        HotkeyBinding(modifierKeyCodes: [ModifierKey.leftOption.rawValue]),
        HotkeyBinding(modifierKeyCodes: [ModifierKey.rightCommand.rawValue]),
        HotkeyBinding(modifierKeyCodes: [ModifierKey.leftCommand.rawValue]),
        .rightControl,
        HotkeyBinding(modifierKeyCodes: [ModifierKey.leftControl.rawValue]),
        HotkeyBinding(modifierKeyCodes: [ModifierKey.rightShift.rawValue]),
        HotkeyBinding(modifierKeyCodes: [ModifierKey.leftShift.rawValue]),
        HotkeyBinding(modifierKeyCodes: [ModifierKey.fn.rawValue]),
    ]

    // MARK: - Serialisation

    func encoded() -> String {
        guard let data = try? JSONEncoder().encode(self),
              let json = String(data: data, encoding: .utf8) else { return "" }
        return json
    }

    static func decode(_ json: String) -> HotkeyBinding? {
        guard let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(HotkeyBinding.self, from: data)
    }

    // MARK: - Key naming

    /// Keys whose `charactersIgnoringModifiers` is a control character or is
    /// otherwise unhelpful as a label.
    private static let specialKeyNames: [Int64: String] = [
        36: "↩", 48: "⇥", 49: "Space", 51: "⌫", 53: "⎋",
        76: "⌤", 71: "⌧", 117: "⌦", 114: "?⃝",
        115: "↖", 119: "↘", 116: "⇞", 121: "⇟",
        123: "←", 124: "→", 125: "↓", 126: "↑",
        122: "F1", 120: "F2", 99: "F3", 118: "F4", 96: "F5", 97: "F6",
        98: "F7", 100: "F8", 101: "F9", 109: "F10", 103: "F11", 111: "F12",
        105: "F13", 107: "F14", 113: "F15", 106: "F16", 64: "F17",
        79: "F18", 80: "F19", 90: "F20",
        82: "num 0", 83: "num 1", 84: "num 2", 85: "num 3", 86: "num 4",
        87: "num 5", 88: "num 6", 89: "num 7", 91: "num 8", 92: "num 9",
        65: "num .", 67: "num *", 69: "num +", 75: "num /", 78: "num −",
        81: "num =",
    ]

    /// Live layout lookup takes priority over the recorded label, so a binding
    /// captured on one layout is displayed with the character the key produces
    /// now — the physical key is what the user actually presses.
    static func keyName(for keyCode: Int64, fallback: String?) -> String {
        if let name = specialKeyNames[keyCode] { return name }
        if let translated = translateToCharacter(keyCode: keyCode)?
            .trimmingCharacters(in: .whitespacesAndNewlines),
           !translated.isEmpty {
            return translated.uppercased()
        }
        if let label = fallback?.trimmingCharacters(in: .whitespacesAndNewlines),
           !label.isEmpty,
           label.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) {
            return label.uppercased()
        }
        return "#\(keyCode)"
    }

    /// Asks the current keyboard layout what character a keycode produces, so
    /// a binding recorded on one layout still reads correctly elsewhere.
    private static func translateToCharacter(keyCode: Int64) -> String? {
        guard let source = TISCopyCurrentKeyboardLayoutInputSource()?.takeRetainedValue(),
              let layoutPtr = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData)
        else { return nil }
        let layoutData = Unmanaged<CFData>.fromOpaque(layoutPtr).takeUnretainedValue() as Data

        var deadKeyState: UInt32 = 0
        var length = 0
        var chars = [UniChar](repeating: 0, count: 4)

        let status = layoutData.withUnsafeBytes { raw -> OSStatus in
            guard let base = raw.bindMemory(to: UCKeyboardLayout.self).baseAddress else {
                return OSStatus(paramErr)
            }
            return UCKeyTranslate(
                base,
                UInt16(keyCode),
                UInt16(kUCKeyActionDisplay),
                0,
                UInt32(LMGetKbdType()),
                OptionBits(kUCKeyTranslateNoDeadKeysBit),
                &deadKeyState,
                chars.count,
                &length,
                &chars
            )
        }
        guard status == noErr, length > 0 else { return nil }
        return String(utf16CodeUnits: chars, count: length)
    }
}
