import AppKit
import Carbon.HIToolbox
import SwiftUI

/// Click-to-record shortcut field.
///
/// Captures whatever the user presses: modifiers alone (`⌥R`, `⌃L⌥L`) or
/// modifiers plus one regular key (`⌃⌥D`, `F13`). A modifier-only chord is
/// committed when the last modifier comes back up, so the user can hold the
/// combination and inspect it before releasing.
struct ShortcutRecorderField: View {
    @Binding var binding: HotkeyBinding

    @State private var isRecording = false
    @State private var held: [Int64] = []
    @State private var lastHeld: [Int64] = []
    @State private var monitor: Any?
    @State private var disarm: DispatchWorkItem?

    /// A field left armed silently swallows the next thing typed and saves it
    /// as the hotkey, which is how a stray ⇧3 ends up as your trigger. Give up
    /// on its own if nothing is pressed.
    private let armedTimeout: TimeInterval = 6

    var body: some View {
        Button(action: toggleRecording) {
            HStack(spacing: 6) {
                Text(fieldText)
                    .font(.system(size: 13, weight: isRecording ? .regular : .medium))
                    .foregroundStyle(isRecording ? .secondary : .primary)
                Spacer(minLength: 0)
                if isRecording {
                    Text(LocalizedStringKey("hotkey.record_hint"))
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .frame(minWidth: 160, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color(nsColor: .textBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .strokeBorder(
                        isRecording ? Color.accentColor : Color(nsColor: .separatorColor),
                        lineWidth: isRecording ? 2 : 1
                    )
            )
            .contentShape(RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .onDisappear(perform: endRecording)
        // The local monitor swallows every key event, so never leave it armed
        // once the Settings window stops being the one the user is looking at.
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didResignKeyNotification)) { _ in
            endRecording()
        }
    }

    private var fieldText: String {
        if isRecording {
            let pending = held.isEmpty ? lastHeld : held
            if pending.isEmpty { return NSLocalizedString("hotkey.press_keys", comment: "") }
            return HotkeyBinding(modifierKeyCodes: pending).displayName
        }
        return binding.displayName
    }

    // MARK: - Capture

    private func toggleRecording() {
        isRecording ? endRecording() : beginRecording()
    }

    private func beginRecording() {
        guard !isRecording else { return }
        held = []
        lastHeld = []
        isRecording = true
        // Silence the global hotkey while capturing, otherwise pressing the
        // current trigger would start a dictation behind the Settings window.
        NotificationCenter.default.post(name: AppSettings.hotkeyCaptureBeganNotification, object: nil)
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { event in
            handle(event)
            return nil  // swallow everything so nothing types or beeps
        }
        scheduleDisarm()
    }

    private func scheduleDisarm() {
        disarm?.cancel()
        let item = DispatchWorkItem {
            if isRecording, held.isEmpty { endRecording() }
        }
        disarm = item
        DispatchQueue.main.asyncAfter(deadline: .now() + armedTimeout, execute: item)
    }

    private func endRecording() {
        disarm?.cancel()
        disarm = nil
        if let monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
        if isRecording {
            NotificationCenter.default.post(name: AppSettings.hotkeyCaptureEndedNotification, object: nil)
        }
        isRecording = false
        held = []
        lastHeld = []
    }

    private func handle(_ event: NSEvent) {
        scheduleDisarm()

        switch event.type {
        case .flagsChanged:
            updateHeld(from: event)
            // Releasing the last modifier without ever pressing a regular key
            // commits the chord the user was holding.
            if held.isEmpty, !lastHeld.isEmpty {
                commit(HotkeyBinding(modifierKeyCodes: lastHeld))
            }

        case .keyDown:
            let keyCode = Int64(event.keyCode)
            if keyCode == Int64(kVK_Escape), held.isEmpty {
                endRecording()
                return
            }
            commit(
                HotkeyBinding(
                    keyCode: keyCode,
                    modifierKeyCodes: held,
                    keyLabel: event.charactersIgnoringModifiers
                )
            )

        default:
            break
        }
    }

    /// Recomputes which side-aware modifiers are down, preserving press order.
    private func updateHeld(from event: NSEvent) {
        let raw = UInt64(event.modifierFlags.rawValue)
        let changed = Int64(event.keyCode)

        for modifier in ModifierKey.allCases {
            // `fn`'s mask is also set by arrow and function keys, so trust it
            // only on its own flagsChanged event.
            let isDown = modifier == .fn
                ? (changed == ModifierKey.fn.rawValue && modifier.isHeld(in: raw))
                : modifier.isHeld(in: raw)

            if isDown {
                if !held.contains(modifier.rawValue) { held.append(modifier.rawValue) }
            } else if modifier != .fn || changed == ModifierKey.fn.rawValue {
                held.removeAll { $0 == modifier.rawValue }
            }
        }
        if !held.isEmpty { lastHeld = held }
    }

    private func commit(_ newBinding: HotkeyBinding) {
        guard !newBinding.isEmpty else { return }
        binding = newBinding
        endRecording()
    }
}
