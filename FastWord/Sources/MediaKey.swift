import AppKit

/// Pauses / resumes whatever the system thinks is the "now playing" media
/// session (Spotify, Music, YouTube in browser, podcast players, etc).
///
/// Two layered strategies:
///
///   1. `MRMediaRemoteSendCommand` from the private `MediaRemote.framework`
///      — the same call that bona-fide media-control apps (NepTunes, Sleeve,
///      Silicio…) use. Works reliably on macOS 12+, including for Spotify
///      where the documented CGEvent path sometimes silently misses.
///   2. NX_KEYTYPE_PLAY system-defined event posted via `CGEvent.post`. This
///      is the AppKit-blessed approach. We keep it as a fallback for the
///      case where the private symbol can't be resolved (e.g. a future macOS
///      that removes it).
enum MediaKey {

    static func playPause() {
        if MediaRemote.shared.sendTogglePlayPause() {
            return
        }
        // Fallback for if the private framework symbol disappears.
        postNXSystemDefined(downFlags: 0xa)
        postNXSystemDefined(downFlags: 0xb)
    }

    // MARK: - NX_KEYTYPE_PLAY via NSEvent.systemDefined (fallback)

    private static let NX_KEYTYPE_PLAY: Int = 16
    private static let NSSystemDefinedSubtypeAuxControlButtons: Int16 = 8

    private static func postNXSystemDefined(downFlags: Int) {
        let data1 = (NX_KEYTYPE_PLAY << 16) | (downFlags << 8)
        guard let event = NSEvent.otherEvent(
            with: .systemDefined,
            location: .zero,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: 0,
            context: nil,
            subtype: NSSystemDefinedSubtypeAuxControlButtons,
            data1: data1,
            data2: -1
        ) else { return }
        if let cg = event.cgEvent {
            cg.post(tap: .cghidEventTap)
        }
    }
}

/// Wrapper around the private `MediaRemote.framework`. Looks up the function
/// pointer once at startup; if the symbol can't be resolved the caller falls
/// back to the public CGEvent path.
final class MediaRemote {
    static let shared = MediaRemote()

    /// Command codes for `MRMediaRemoteSendCommand`. Source: the framework's
    /// `MRCommand` enum, observed in headers reverse-engineered by the
    /// community (e.g. NepTunes, Sleeve).
    private enum Command: Int32 {
        case play = 0
        case pause = 1
        case togglePlayPause = 2
    }

    /// `Bool MRMediaRemoteSendCommand(MRCommand cmd, NSDictionary *userInfo)`.
    private typealias SendCommandFn = @convention(c) (Int32, CFDictionary?) -> Bool

    private let sendCommand: SendCommandFn?

    private init() {
        let path = "/System/Library/PrivateFrameworks/MediaRemote.framework"
        guard let bundle = CFBundleCreate(
            kCFAllocatorDefault, NSURL(fileURLWithPath: path) as CFURL
        ) else {
            self.sendCommand = nil
            return
        }
        guard let raw = CFBundleGetFunctionPointerForName(
            bundle, "MRMediaRemoteSendCommand" as CFString
        ) else {
            self.sendCommand = nil
            return
        }
        self.sendCommand = unsafeBitCast(raw, to: SendCommandFn.self)
    }

    @discardableResult
    func sendTogglePlayPause() -> Bool {
        guard let fn = sendCommand else { return false }
        return fn(Command.togglePlayPause.rawValue, nil)
    }
}
