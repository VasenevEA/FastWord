import AppKit

/// FastWord is a menu-bar app (LSUIElement), so macOS gives it no Dock icon
/// and no ⌘Tab entry — its History, Settings and Transcribe File windows get
/// lost behind other apps with no way to switch back. While any of those
/// windows is open we act as a regular app, and return to the menu bar when
/// the last one closes. Dropping back to accessory hands the front position
/// to the previous app by itself (TransformProcessType does it), so the next
/// dictation pastes there, not into us.
@MainActor
enum DockPresence {
    private static var observers: [NSObjectProtocol] = []

    static func start() {
        let center = NotificationCenter.default
        let names: [Notification.Name] = [
            NSWindow.didBecomeKeyNotification,
            NSWindow.didDeminiaturizeNotification,
            NSWindow.willCloseNotification,
            NSApplication.didFinishRestoringWindowsNotification,
        ]
        for name in names {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { _ in
                // willClose fires while the window is still on screen; look
                // again once it's gone.
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { update() }
                }
            })
        }
        // Windows SwiftUI restored before these observers existed.
        DispatchQueue.main.async {
            MainActor.assumeIsolated { update() }
        }
    }

    /// Call right before opening a window from the menu-bar extra. Turning
    /// regular before the activation that follows, rather than after the
    /// window is already key, is the order in which AppKit reliably hands a
    /// transformed app the menu bar.
    static func prepareToShowWindow() {
        if NSApp.activationPolicy() != .regular {
            NSApp.setActivationPolicy(.regular)
        }
        // Safety net: if no window actually appears, don't keep a Dock icon.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
            MainActor.assumeIsolated { update() }
        }
    }

    private static func update() {
        // Hidden windows report isVisible == false; don't read that as
        // "all closed" and strand them without a Dock icon to unhide from.
        guard !NSApp.isHidden else { return }
        let wanted: NSApplication.ActivationPolicy =
            NSApp.windows.contains(where: isAppWindow) ? .regular : .accessory
        guard NSApp.activationPolicy() != wanted else { return }
        NSApp.setActivationPolicy(wanted)
    }

    /// Our own titled windows. The HUD and system panels (save/open, alerts)
    /// are NSPanels; the menu-bar extra has no titled window.
    private static func isAppWindow(_ window: NSWindow) -> Bool {
        (window.isVisible || window.isMiniaturized)
            && window.styleMask.contains(.titled)
            && !(window is NSPanel)
    }
}
