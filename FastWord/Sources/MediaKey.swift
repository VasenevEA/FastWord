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

    /// Resume playback (only meaningful if we previously paused something).
    static func play() {
        if MediaRemote.shared.sendPlay() { return }
        // Best-effort fallback: a toggle keystroke. If nothing was playing this
        // is a no-op; if the user manually paused / resumed in between we may
        // get it wrong, but that's already the case for the fallback path.
        postNXSystemDefined(downFlags: 0xa)
        postNXSystemDefined(downFlags: 0xb)
    }

    /// Pause whatever is currently playing.
    static func pause() {
        if MediaRemote.shared.sendPause() { return }
        postNXSystemDefined(downFlags: 0xa)
        postNXSystemDefined(downFlags: 0xb)
    }

    /// Asks the system whether any app is currently playing audio that the
    /// 'Now Playing' system recognises (Spotify, Music, Safari/Chrome web
    /// media, podcast apps). Used to avoid starting playback from silence
    /// when the user toggles dictation while nothing is actually playing.
    static func isNowPlaying() async -> Bool {
        await MediaRemote.shared.isNowPlaying()
    }

    /// Sends a raw system-defined play/pause keystroke. This is the only
    /// thing that reaches media playing inside a web browser (YouTube,
    /// SoundCloud) because browsers don't register as the system Now Playing
    /// client — they only listen to HID media keys. Caller must remember
    /// that this is a **toggle** and resume by sending the same key again.
    static func toggleViaSystemDefined() {
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
            // Session tap reaches both the global media-key receiver
            // (Spotify, Music) and per-session listeners (Safari/Chrome
            // HTML5 audio bridges); HID tap is the other way around. Session
            // tap is the strictly broader choice.
            cg.post(tap: .cgSessionEventTap)
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
    /// `void MRMediaRemoteGetNowPlayingApplicationIsPlaying(dispatch_queue_t, void (^)(BOOL))`.
    /// `@escaping` is critical — MediaRemote invokes the callback asynchronously
    /// from its own queue. Without it the Swift runtime trips
    /// "non-escaping closure has escaped" and the process is killed with SIGTRAP.
    private typealias IsPlayingFn = @convention(c) (DispatchQueue, @escaping @convention(block) (Bool) -> Void) -> Void

    private let sendCommand: SendCommandFn?
    private let getIsPlaying: IsPlayingFn?

    private init() {
        let path = "/System/Library/PrivateFrameworks/MediaRemote.framework"
        guard let bundle = CFBundleCreate(
            kCFAllocatorDefault, NSURL(fileURLWithPath: path) as CFURL
        ) else {
            self.sendCommand = nil
            self.getIsPlaying = nil
            return
        }
        if let raw = CFBundleGetFunctionPointerForName(bundle, "MRMediaRemoteSendCommand" as CFString) {
            self.sendCommand = unsafeBitCast(raw, to: SendCommandFn.self)
        } else {
            self.sendCommand = nil
        }
        if let raw = CFBundleGetFunctionPointerForName(
            bundle, "MRMediaRemoteGetNowPlayingApplicationIsPlaying" as CFString
        ) {
            self.getIsPlaying = unsafeBitCast(raw, to: IsPlayingFn.self)
        } else {
            self.getIsPlaying = nil
        }
    }

    @discardableResult
    func sendPlay() -> Bool {
        guard let fn = sendCommand else { return false }
        return fn(Command.play.rawValue, nil)
    }

    @discardableResult
    func sendPause() -> Bool {
        guard let fn = sendCommand else { return false }
        return fn(Command.pause.rawValue, nil)
    }

    /// Async wrapper over the callback-based isPlaying query. Returns false
    /// if the private symbol can't be resolved.
    func isNowPlaying() async -> Bool {
        guard let fn = getIsPlaying else { return false }
        return await withCheckedContinuation { continuation in
            fn(DispatchQueue.global(qos: .userInteractive)) { playing in
                continuation.resume(returning: playing)
            }
        }
    }
}
