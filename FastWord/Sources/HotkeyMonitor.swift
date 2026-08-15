import AppKit
import Carbon.HIToolbox
import IOKit.hid

final class HotkeyMonitor {
    var onPressStart: (() -> Void)?
    var onPressEnd: (() -> Void)?
    var onCancel: (() -> Void)?
    var onPermissionMissing: (() -> Void)?

    private(set) var isActive = false
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    /// False when we had to fall back to a listen-only tap, in which case a
    /// binding that includes a regular key still types its character.
    private(set) var canSuppressKeys = false

    /// Side-aware modifier keycodes currently held.
    private var modifiersDown: Set<Int64> = []
    /// Non-modifier keycodes currently held.
    private var keysDown: Set<Int64> = []
    /// Keycodes whose key-down we swallowed, so we swallow their key-up too
    /// even if a modifier was released first.
    private var swallowedKeys: Set<Int64> = []
    private var triggerActive = false
    /// True while Settings is recording a new shortcut.
    private var isSuspended = false

    private var primary: HotkeyBinding = AppSettings.hotkeyBinding
    private var secondary: HotkeyBinding?
    private var comboMode: HotkeyComboMode = .either

    func reloadHotkey() {
        primary = AppSettings.hotkeyBinding
        comboMode = AppSettings.hotkeyComboMode
        if AppSettings.useComboHotkey {
            let candidate = AppSettings.hotkeySecondaryBinding
            // Avoid the silly case of "same binding twice" — fall back to single.
            secondary = (candidate.isEmpty || candidate == primary) ? nil : candidate
        } else {
            secondary = nil
        }
        HotkeyMonitor.dlog("binding now: primary=\(primary.displayName), secondary=\(secondary?.displayName ?? "none"), mode=\(comboMode.rawValue)")
        loggedEvents = 0
        // Force release whatever was held under the old config.
        modifiersDown.removeAll()
        keysDown.removeAll()
        swallowedKeys.removeAll()
        if triggerActive {
            triggerActive = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

    func start() {
        reloadHotkey()
        HotkeyMonitor.dlog("--- FastWord start ---")
        logPermissionState()
        installEventTap()
        // After the system wakes from sleep, the event tap is sometimes left
        // disabled and macOS does not redeliver tap-disabled events. Re-arm or
        // recreate it explicitly when we get NSWorkspace.didWakeNotification.
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(systemDidWake(_:)),
            name: NSWorkspace.didWakeNotification,
            object: nil
        )
        NotificationCenter.default.addObserver(
            self, selector: #selector(captureBegan),
            name: AppSettings.hotkeyCaptureBeganNotification, object: nil
        )
        NotificationCenter.default.addObserver(
            self, selector: #selector(captureEnded),
            name: AppSettings.hotkeyCaptureEndedNotification, object: nil
        )
    }

    @objc private func captureBegan() {
        isSuspended = true
        forceRelease()
    }

    @objc private func captureEnded() {
        isSuspended = false
        forceRelease()
    }

    @objc private func systemDidWake(_ note: Notification) {
        forceRelease()
        if let tap = eventTap, CGEvent.tapIsEnabled(tap: tap) == false {
            CGEvent.tapEnable(tap: tap, enable: true)
            return
        }
        // If the tap is gone or fails to re-enable, rebuild it from scratch.
        if eventTap == nil {
            installEventTap()
        }
    }

    private func installEventTap() {
        let mask = (1 << CGEventType.flagsChanged.rawValue)
            | (1 << CGEventType.keyDown.rawValue)
            | (1 << CGEventType.keyUp.rawValue)
            | (1 << CGEventType.tapDisabledByTimeout.rawValue)
            | (1 << CGEventType.tapDisabledByUserInput.rawValue)

        let callback: CGEventTapCallBack = { _, type, event, refcon in
            guard let refcon else { return Unmanaged.passUnretained(event) }
            let monitor = Unmanaged<HotkeyMonitor>.fromOpaque(refcon).takeUnretainedValue()
            return monitor.handle(type: type, event: event)
        }

        // A default tap lets us swallow the trigger keystroke so a binding
        // like ⌃⌥D doesn't also type "d" into the focused field. It needs
        // Accessibility; if that isn't granted yet, fall back to a listen-only
        // tap so modifier-only bindings keep working.
        var suppressing = true
        var tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: CGEventMask(mask),
            callback: callback,
            userInfo: Unmanaged.passUnretained(self).toOpaque()
        )
        if tap == nil {
            suppressing = false
            tap = CGEvent.tapCreate(
                tap: .cgSessionEventTap,
                place: .headInsertEventTap,
                options: .listenOnly,
                eventsOfInterest: CGEventMask(mask),
                callback: callback,
                userInfo: Unmanaged.passUnretained(self).toOpaque()
            )
            if tap != nil {
                NSLog("FastWord: no Accessibility permission — hotkeys with a regular key will still type their character.")
            }
        }

        guard let tap else {
            NSLog("FastWord: failed to create event tap. Grant Input Monitoring + Accessibility in System Settings.")
            HotkeyMonitor.dlog("event tap FAILED to create in both modes — no keys will ever arrive")
            DispatchQueue.main.async { [weak self] in self?.onPermissionMissing?() }
            return
        }
        HotkeyMonitor.dlog("event tap created, mode=\(suppressing ? "defaultTap (can suppress)" : "listenOnly (cannot suppress)")")
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.eventTap = tap
        self.runLoopSource = source
        self.isActive = true
        self.canSuppressKeys = suppressing
    }

    func stop() {
        if let tap = eventTap { CGEvent.tapEnable(tap: tap, enable: false) }
        if let src = runLoopSource { CFRunLoopRemoveSource(CFRunLoopGetMain(), src, .commonModes) }
        eventTap = nil
        runLoopSource = nil
        isActive = false
    }

    // MARK: - Diagnostics

    /// Append-only log at ~/.fastword/hotkey.log. A hotkey that doesn't fire
    /// has several possible causes that look identical from the outside
    /// (missing permission, the event tap never created, another app eating
    /// the keys, a binding that doesn't match); this records which one it is.
    static let logURL: URL = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".fastword/hotkey.log")
    private static let logQueue = DispatchQueue(label: "fastword.hotkey.log")

    static func dlog(_ message: String) {
        let line = "[\(ISO8601DateFormatter().string(from: Date()))] \(message)\n"
        logQueue.async {
            guard let data = line.data(using: .utf8) else { return }
            let fm = FileManager.default
            let dir = logURL.deletingLastPathComponent()
            if !fm.fileExists(atPath: dir.path) {
                try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
            }
            if let handle = try? FileHandle(forWritingTo: logURL) {
                try? handle.seekToEnd()
                try? handle.write(contentsOf: data)
                try? handle.close()
            } else {
                try? data.write(to: logURL)
            }
        }
    }

    private func logPermissionState() {
        let accessibility = AXIsProcessTrusted()
        let listen: String
        switch IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) {
        case kIOHIDAccessTypeGranted: listen = "granted"
        case kIOHIDAccessTypeDenied: listen = "DENIED"
        default: listen = "unknown"
        }
        HotkeyMonitor.dlog("permissions: accessibility=\(accessibility ? "granted" : "DENIED"), inputMonitoring=\(listen)")
        HotkeyMonitor.dlog("binding: primary=\(primary.displayName) \(primary), secondary=\(secondary.map { "\($0.displayName) \($0)" } ?? "none"), mode=\(comboMode.rawValue)")
    }

    /// Logs the first few modifier events after start-up so a binding that
    /// never matches can be compared against what the keyboard actually sent.
    private var loggedEvents = 0
    private func logEvent(_ raw: UInt64, _ changedKey: Int64, matched: Bool) {
        guard loggedEvents < 40 else { return }
        loggedEvents += 1
        HotkeyMonitor.dlog(String(
            format: "flags=0x%llx key=%lld down=%@ matched=%@",
            raw, changedKey,
            modifiersDown.sorted().map(String.init).joined(separator: ",") as NSString,
            matched ? "YES" : "no"
        ))
    }

    // MARK: - Event handling

    private func handle(type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        let passthrough = Unmanaged.passUnretained(event)

        switch type {
        case .tapDisabledByTimeout, .tapDisabledByUserInput:
            if let tap = eventTap { CGEvent.tapEnable(tap: tap, enable: true) }
            forceRelease()
            return passthrough

        case _ where isSuspended:
            return passthrough

        case .flagsChanged:
            updateModifiers(from: event)
            evaluate()
            logEvent(event.flags.rawValue, event.getIntegerValueField(.keyboardEventKeycode), matched: triggerActive)
            return passthrough

        case .keyDown:
            let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
            updateModifiers(from: event)
            keysDown.insert(keyCode)
            // Escape cancels an in-flight recording, unless the user has
            // actually bound Escape as their trigger.
            if keyCode == Int64(kVK_Escape), !isBound(keyCode: keyCode) {
                DispatchQueue.main.async { [weak self] in self?.onCancel?() }
                evaluate()
                return passthrough
            }
            evaluate()
            if triggerActive, isBound(keyCode: keyCode) {
                swallowedKeys.insert(keyCode)
                return nil
            }
            return passthrough

        case .keyUp:
            let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
            updateModifiers(from: event)
            keysDown.remove(keyCode)
            evaluate()
            if swallowedKeys.remove(keyCode) != nil { return nil }
            return passthrough

        default:
            return passthrough
        }
    }

    /// Rebuilds the held-modifier set from the event's flags.
    ///
    /// A `flagsChanged` event names the exact key that changed, which is the
    /// only signal that always tells left from right: virtual keyboards
    /// (Karabiner, remapping tools, some Bluetooth stacks) re-post events
    /// without the device-dependent NX bits, and then the bits alone can't
    /// distinguish the sides — or, worse, report the wrong one.
    ///
    /// So: the changed key is resolved from its own keycode, using device
    /// bits only when they are actually present. Every other modifier is
    /// re-synced conservatively, and left alone when the flags are ambiguous.
    private func updateModifiers(from event: CGEvent) {
        let raw = event.flags.rawValue
        let changedKey = event.getIntegerValueField(.keyboardEventKeycode)

        if event.type == .flagsChanged, let changed = ModifierKey(rawValue: changedKey) {
            setDown(changed, isDownForChangedKey(changed, raw: raw))
        }

        for modifier in ModifierKey.allCases where modifier.rawValue != changedKey {
            // fn's generic mask is also set by arrow and function keys, so it
            // is only ever updated by its own flagsChanged event above.
            guard modifier != .fn else { continue }
            if raw & modifier.genericMask == 0 {
                setDown(modifier, false)          // definitely released
            } else if raw & modifier.deviceMask != 0 {
                setDown(modifier, true)           // definitely held
            }
            // Otherwise the modifier is held on *some* side but the flags
            // don't say which. Leave the existing state alone rather than
            // guessing.
        }
    }

    /// Down-state of the key whose own flagsChanged event this is.
    private func isDownForChangedKey(_ modifier: ModifierKey, raw: UInt64) -> Bool {
        let ownBit = raw & modifier.deviceMask != 0
        let siblingBit = modifier.sibling.map { raw & $0.deviceMask != 0 } ?? false
        // When the event carries side bits, they settle it — that's what makes
        // "hold both ⌥, release one" work. When it carries none, the generic
        // mask is all we have, and the keycode already told us which key.
        if ownBit || siblingBit { return ownBit }
        return raw & modifier.genericMask != 0
    }

    private func setDown(_ modifier: ModifierKey, _ isDown: Bool) {
        if isDown {
            modifiersDown.insert(modifier.rawValue)
        } else {
            modifiersDown.remove(modifier.rawValue)
        }
    }

    private func matches(_ binding: HotkeyBinding) -> Bool {
        guard !binding.isEmpty else { return false }
        guard binding.modifierKeyCodes.allSatisfy({ modifiersDown.contains($0) }) else { return false }
        guard let keyCode = binding.keyCode else { return true }
        return keysDown.contains(keyCode)
    }

    private func isBound(keyCode: Int64) -> Bool {
        if primary.keyCode == keyCode { return true }
        return secondary?.keyCode == keyCode
    }

    private func evaluate() {
        let nowActive: Bool
        if let secondary {
            switch comboMode {
            case .either: nowActive = matches(primary) || matches(secondary)
            case .both: nowActive = matches(primary) && matches(secondary)
            }
        } else {
            nowActive = matches(primary)
        }

        if nowActive, !triggerActive {
            triggerActive = true
            DispatchQueue.main.async { [weak self] in self?.onPressStart?() }
        } else if !nowActive, triggerActive {
            triggerActive = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

    /// Drops all held state after an event we may have missed (sleep, tap
    /// timeout), ending any recording that was in flight.
    private func forceRelease() {
        modifiersDown.removeAll()
        keysDown.removeAll()
        swallowedKeys.removeAll()
        if triggerActive {
            triggerActive = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }
}
