import AppKit
import Carbon.HIToolbox

final class HotkeyMonitor {
    var onPressStart: (() -> Void)?
    var onPressEnd: (() -> Void)?
    var onPermissionMissing: (() -> Void)?

    private(set) var isActive = false
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var keyDown = false
    private var triggerKeyCode: Int64 = AppSettings.hotkey.keyCode
    private var triggerFlag: CGEventFlags = AppSettings.hotkey.modifierFlag

    func reloadHotkey() {
        let choice = AppSettings.hotkey
        triggerKeyCode = choice.keyCode
        triggerFlag = choice.modifierFlag
        // If the previously-tracked key was held under the old config, force release.
        if keyDown {
            keyDown = false
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
        keyDown = false
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
        let modDown = event.flags.contains(triggerFlag)
        guard keyCode == triggerKeyCode else {
            // Different modifier fired and ours isn't held in flags — release if needed.
            if !modDown && keyDown {
                keyDown = false
                DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
            }
            return
        }
        if modDown && !keyDown {
            keyDown = true
            DispatchQueue.main.async { [weak self] in self?.onPressStart?() }
        } else if !modDown && keyDown {
            keyDown = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

    fileprivate func recoverAfterTapReEnable() {
        // After tap timeout/disable, we may have missed a release. Force release.
        if keyDown {
            keyDown = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

}
