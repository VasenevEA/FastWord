import AppKit
import Carbon.HIToolbox

enum Pasteboard {
    /// Snapshot every type currently on the general pasteboard so we can restore
    /// it after we synthesize Cmd+V.
    private struct Snapshot {
        let items: [[NSPasteboard.PasteboardType: Data]]
    }

    static func copyAndPaste(_ text: String) {
        let pb = NSPasteboard.general
        let snapshot = capture(pb)

        pb.clearContents()
        pb.setString(text, forType: .string)
        synthesizePaste()

        // Wait long enough for the receiving app to consume the paste, then put
        // the user's previous clipboard back. 250 ms is a conservative value
        // that survives slow apps without being noticeable.
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
