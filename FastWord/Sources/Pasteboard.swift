import AppKit
import ApplicationServices
import Carbon.HIToolbox

enum Pasteboard {
    /// Inserts `text` into whatever is currently focused.
    ///
    /// Tries two strategies in order:
    ///   1. Accessibility — replace the selection of the focused text element
    ///      via `kAXSelectedTextAttribute`. The user's clipboard is never
    ///      touched. Works for standard `NSTextField`/`NSTextView`-backed
    ///      inputs and most native macOS controls.
    ///   2. Clipboard — fall back to writing to `NSPasteboard.general`,
    ///      synthesising `Cmd+V`, then restoring the previous clipboard
    ///      contents (all types, not just plain text) shortly after.
    static func copyAndPaste(_ text: String) {
        if insertViaAccessibility(text) {
            return
        }
        copyAndPasteViaClipboard(text)
    }

    // MARK: - Accessibility path

    private static func insertViaAccessibility(_ text: String) -> Bool {
        guard AXIsProcessTrusted() else { return false }

        let systemWide = AXUIElementCreateSystemWide()

        var focused: CFTypeRef?
        let copyErr = AXUIElementCopyAttributeValue(
            systemWide, kAXFocusedUIElementAttribute as CFString, &focused
        )
        guard copyErr == .success, let focusedRef = focused else { return false }
        // CFTypeRef returned here is an AXUIElement.
        let element = focusedRef as! AXUIElement

        // Only attempt the AX insert if the focused element advertises a
        // settable selected-text attribute. Otherwise fall back to clipboard.
        var settable = DarwinBoolean(false)
        let probeErr = AXUIElementIsAttributeSettable(
            element, kAXSelectedTextAttribute as CFString, &settable
        )
        guard probeErr == .success, settable.boolValue else {
            return false
        }

        let setErr = AXUIElementSetAttributeValue(
            element, kAXSelectedTextAttribute as CFString, text as CFString
        )
        return setErr == .success
    }

    // MARK: - Clipboard fallback

    /// Snapshot of every type currently on a pasteboard.
    private struct Snapshot {
        let items: [[NSPasteboard.PasteboardType: Data]]
    }

    private static func copyAndPasteViaClipboard(_ text: String) {
        let pb = NSPasteboard.general
        let snapshot = capture(pb)

        pb.clearContents()
        pb.setString(text, forType: .string)
        synthesizePaste()

        // Wait long enough for the receiving app to consume the paste, then
        // put the user's previous clipboard back. 250 ms is conservative.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
            restore(snapshot, into: pb)
        }
    }

    private static func capture(_ pb: NSPasteboard) -> Snapshot {
        let items: [[NSPasteboard.PasteboardType: Data]] = (pb.pasteboardItems ?? []).map { item in
            var dict: [NSPasteboard.PasteboardType: Data] = [:]
            for type in item.types {
                if let data = item.data(forType: type) {
                    dict[type] = data
                }
            }
            return dict
        }
        return Snapshot(items: items)
    }

    private static func restore(_ snapshot: Snapshot, into pb: NSPasteboard) {
        pb.clearContents()
        guard !snapshot.items.isEmpty else { return }
        let restored: [NSPasteboardItem] = snapshot.items.map { dict in
            let item = NSPasteboardItem()
            for (type, data) in dict {
                item.setData(data, forType: type)
            }
            return item
        }
        pb.writeObjects(restored)
    }

    private static func synthesizePaste() {
        let src = CGEventSource(stateID: .combinedSessionState)
        let vDown = CGEvent(keyboardEventSource: src, virtualKey: CGKeyCode(kVK_ANSI_V), keyDown: true)
        let vUp = CGEvent(keyboardEventSource: src, virtualKey: CGKeyCode(kVK_ANSI_V), keyDown: false)
        vDown?.flags = .maskCommand
        vUp?.flags = .maskCommand
        vDown?.post(tap: .cgAnnotatedSessionEventTap)
        vUp?.post(tap: .cgAnnotatedSessionEventTap)
    }
}
