import AppKit
import Carbon.HIToolbox

enum Pasteboard {
    /// Inserts `text` wherever the keyboard focus currently is.
    ///
    /// We deliberately use a single, near-universal strategy: write the text
    /// to `NSPasteboard.general`, synthesise ⌘V, then restore the user's
    /// previous clipboard once the target app has had time to consume the
    /// paste.
    ///
    /// The previous build tried an Accessibility-first path
    /// (`kAXSelectedTextAttribute`) and only fell back to the clipboard if
    /// that "failed". The problem: Chromium/Electron text fields (Slack,
    /// Comet, Threads, Yahoo Finance search, basically every web input)
    /// report the attribute as settable and return `.success` from the set
    /// call, but silently drop it — so we thought we'd inserted the text and
    /// never fell back, and nothing appeared. Clipboard + ⌘V works in those
    /// apps and in native ones alike, and since we snapshot/restore the
    /// clipboard the user's data is still preserved.
    static func copyAndPaste(_ text: String) {
        let pb = NSPasteboard.general
        let snapshot = capture(pb)

        pb.clearContents()
        pb.setString(text, forType: .string)
        // changeCount is bumped on every write. Remember ours so the delayed
        // restore can tell "nobody else touched the clipboard since" from
        // "the user copied something during the paste window".
        let ourChangeCount = pb.changeCount

        synthesizePaste()

        // Electron / Chromium paste handlers are asynchronous and noticeably
        // slower than native controls — 250 ms was racing them and the old
        // clipboard came back before Slack had read our text. 700 ms clears
        // every app we've tested while still being imperceptible.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.7) {
            // If the user copied something else while the paste was in
            // flight, don't clobber their new clipboard.
            guard pb.changeCount == ourChangeCount else { return }
            restore(snapshot, into: pb)
        }
    }

    // MARK: - Clipboard snapshot / restore

    /// Snapshot of every representation currently on a pasteboard, so we can
    /// restore RTF / images / file URLs and not just plain text.
    private struct Snapshot {
        let items: [[NSPasteboard.PasteboardType: Data]]
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

    // MARK: - Synthesised ⌘V

    private static func synthesizePaste() {
        let src = CGEventSource(stateID: .combinedSessionState)
        let v = CGKeyCode(kVK_ANSI_V)

        let down = CGEvent(keyboardEventSource: src, virtualKey: v, keyDown: true)
        down?.flags = .maskCommand
        let up = CGEvent(keyboardEventSource: src, virtualKey: v, keyDown: false)
        up?.flags = .maskCommand

        // .cghidEventTap injects at the HID layer — the most reliable place
        // for Chromium/Electron apps to see a synthetic key. A short gap
        // between down and up lets slower apps register the chord.
        down?.post(tap: .cghidEventTap)
        usleep(15_000) // 15 ms
        up?.post(tap: .cghidEventTap)
    }
}
