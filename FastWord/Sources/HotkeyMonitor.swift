import AppKit
import Carbon.HIToolbox

final class HotkeyMonitor {
    var onPressStart: (() -> Void)?
    var onPressEnd: (() -> Void)?
    var onCancel: (() -> Void)?
    var onPermissionMissing: (() -> Void)?

    private(set) var isActive = false
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?

    private var primaryDown = false
    private var secondaryDown = false
    private var combinedActive = false   // current "both keys held" state

    private var triggerKeyCode: Int64 = AppSettings.hotkey.keyCode
    private var triggerFlag: CGEventFlags = AppSettings.hotkey.modifierFlag
    private var secondaryKeyCode: Int64?
    private var secondaryFlag: CGEventFlags?
    private var comboMode: HotkeyComboMode = .either

    func reloadHotkey() {
        let primary = AppSettings.hotkey
        triggerKeyCode = primary.keyCode
        triggerFlag = primary.modifierFlag
        comboMode = AppSettings.hotkeyComboMode
        if AppSettings.useComboHotkey {
            let secondary = AppSettings.hotkeySecondary
            // Avoid the silly case of "same key twice" — fall back to single.
            if secondary.keyCode != primary.keyCode {
                secondaryKeyCode = secondary.keyCode
                secondaryFlag = secondary.modifierFlag
            } else {
                secondaryKeyCode = nil
                secondaryFlag = nil
            }
        } else {
            secondaryKeyCode = nil
            secondaryFlag = nil
        }
        // Force release whatever was held under the old config.
        primaryDown = false
        secondaryDown = false
        if combinedActive {
            combinedActive = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

    func start() {
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
    }

    @objc private func systemDidWake(_ note: Notification) {
        primaryDown = false
        secondaryDown = false
        if combinedActive {
            combinedActive = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
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
            | (1 << CGEventType.tapDisabledByTimeout.rawValue)
            | (1 << CGEventType.tapDisabledByUserInput.rawValue)
        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .listenOnly,
            eventsOfInterest: CGEventMask(mask),
            callback: { _, type, event, refcon in
                guard let refcon else { return Unmanaged.passUnretained(event) }
                let monitor = Unmanaged<HotkeyMonitor>.fromOpaque(refcon).takeUnretainedValue()
                if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                    if let tap = monitor.eventTap { CGEvent.tapEnable(tap: tap, enable: true) }
                    monitor.recoverAfterTapReEnable()
                } else if type == .keyDown {
                    let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
                    if keyCode == 53 /* kVK_Escape */ {
                        DispatchQueue.main.async { [weak monitor] in monitor?.onCancel?() }
                    }
                } else {
                    monitor.handleFlags(event)
                }
                return Unmanaged.passUnretained(event)
            },
            userInfo: Unmanaged.passUnretained(self).toOpaque()
        ) else {
            NSLog("FastWord: failed to create event tap. Grant Input Monitoring + Accessibility in System Settings.")
            DispatchQueue.main.async { [weak self] in self?.onPermissionMissing?() }
            return
        }
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.eventTap = tap
        self.runLoopSource = source
        self.isActive = true
    }

    func stop() {
        if let tap = eventTap { CGEvent.tapEnable(tap: tap, enable: false) }
        if let src = runLoopSource { CFRunLoopRemoveSource(CFRunLoopGetMain(), src, .commonModes) }
        eventTap = nil
        runLoopSource = nil
    }

    private static let logQueue = DispatchQueue(label: "fastword.hotkey.log")
    private static let logURL: URL = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".fastword/hotkey.log")

    private static func dlog(_ msg: String) {
        let line = "[\(Date())] \(msg)\n"
        logQueue.async {
            guard let data = line.data(using: .utf8) else { return }
            if let h = try? FileHandle(forWritingTo: logURL) {
                try? h.seekToEnd()
                try? h.write(contentsOf: data)
                try? h.close()
            } else {
                try? data.write(to: logURL)
            }
        }
    }

    private func handleFlags(_ event: CGEvent) {
        let keyCode = event.getIntegerValueField(.keyboardEventKeycode)

        // Update per-key down states based on which keycode just changed.
        // We can't trust just the global flags mask because two distinct
        // modifiers can share a CGEventFlag (e.g. left/right Option both
        // set .maskAlternate). Checking keyCode tells us which specific
        // key fired the flagsChanged event.
        if keyCode == triggerKeyCode {
            primaryDown = event.flags.contains(triggerFlag)
        }
        if let secCode = secondaryKeyCode, let secFlag = secondaryFlag,
           keyCode == secCode {
            secondaryDown = event.flags.contains(secFlag)
        }

        // If a third modifier fires and ours is no longer present in the
        // global flags mask, treat as a release of that side.
        if keyCode != triggerKeyCode, !event.flags.contains(triggerFlag) {
            primaryDown = false
        }
        if let secFlag = secondaryFlag,
           keyCode != secondaryKeyCode, !event.flags.contains(secFlag) {
            secondaryDown = false
        }

        let nowActive: Bool
        if secondaryKeyCode == nil {
            nowActive = primaryDown
        } else {
            switch comboMode {
            case .either: nowActive = primaryDown || secondaryDown
            case .both:   nowActive = primaryDown && secondaryDown
            }
        }

        if nowActive && !combinedActive {
            combinedActive = true
            DispatchQueue.main.async { [weak self] in self?.onPressStart?() }
        } else if !nowActive && combinedActive {
            combinedActive = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

    fileprivate func recoverAfterTapReEnable() {
        // After tap timeout/disable, we may have missed a release. Force release.
        if combinedActive {
            combinedActive = false
            primaryDown = false
            secondaryDown = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

}
