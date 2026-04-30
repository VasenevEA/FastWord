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
    // Right Option key. Identified by keyCode 61 (kVK_RightOption) on flagsChanged
    // with .maskAlternate set. Works on any keyboard, unlike fn.
    private let triggerKeyCode: Int64 = 61

    func start() {
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
        let altDown = event.flags.contains(.maskAlternate)
        Self.dlog("kc=\(keyCode) alt=\(altDown) raw=\(event.flags.rawValue) keyDown=\(keyDown)")
        guard keyCode == triggerKeyCode else {
            // If our key was held but a different modifier event fires while alt is gone,
            // treat as release.
            if !altDown && keyDown {
                keyDown = false
                DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
            }
            return
        }
        if altDown && !keyDown {
            keyDown = true
            DispatchQueue.main.async { [weak self] in self?.onPressStart?() }
        } else if !altDown && keyDown {
            keyDown = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

    fileprivate func recoverAfterTapReEnable() {
        // After tap timeout/disable, we may have missed a release. If we still
        // think the key is down but it isn't, force a release.
        let altDown = NSEvent.modifierFlags.contains(.option)
        if keyDown && !altDown {
            keyDown = false
            DispatchQueue.main.async { [weak self] in self?.onPressEnd?() }
        }
    }

}
