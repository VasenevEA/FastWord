import Accelerate
import AVFoundation
import UniformTypeIdentifiers

/// A file decoded for the sidecar: raw little-endian Float32 mono PCM at
/// 16 kHz, no header, in a temporary file the caller owns (and must delete).
struct DecodedAudio {
    let url: URL
    let sampleCount: Int
    /// True when AVFoundation couldn't handle the file and ffmpeg did.
    let usedFFmpeg: Bool

    var duration: TimeInterval { Double(sampleCount) / AudioFileDecoder.sampleRate }
}

enum AudioDecodeError: LocalizedError {
    case noAudioTrack
    case unsupportedFormat(ffmpegAvailable: Bool)
    case readFailed(String)
    case tooShort

    var errorDescription: String? {
        switch self {
        case .noAudioTrack:
            return NSLocalizedString("This file has no audio track.", comment: "")
        case .unsupportedFormat(let ffmpegAvailable):
            return ffmpegAvailable
                ? NSLocalizedString("This file format isn't supported, or the file is damaged.", comment: "")
                : NSLocalizedString("This file format isn't supported. Installing ffmpeg (brew install ffmpeg) adds OGG/Opus, WebM and more.", comment: "")
        case .readFailed(let detail):
            let format = NSLocalizedString("Couldn't read the audio: %@", comment: "")
            return String(format: format, detail)
        case .tooShort:
            return NSLocalizedString("The file is empty or too short to transcribe.", comment: "")
        }
    }
}

/// Turns any audio or video file into the sidecar's input format, streaming
/// to disk so an hour-long recording never sits in memory (an hour of
/// 16 kHz Float32 is ~230 MB; the source file is usually a tenth of that).
///
/// AVFoundation handles everything macOS can play (mp3, m4a/aac, wav, aiff,
/// caf, flac, mp4, mov, …). Formats it can't open — Telegram's ogg/opus
/// voice notes, webm, mkv — fall back to ffmpeg when the user has one
/// installed; we don't bundle it.
enum AudioFileDecoder {
    static let sampleRate: Double = 16_000
    /// Same floor as the sidecar: below 0.1 s there is nothing to transcribe.
    static let minimumSamples = 1_600

    /// Homebrew on Apple Silicon, Homebrew on Intel, then the system. A GUI
    /// app doesn't inherit the shell's PATH, so we look in known places.
    /// (A var so the decoder test harness can simulate "no ffmpeg".)
    static var ffmpegCandidates = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"]

    static var ffmpegPath: String? {
        ffmpegCandidates.first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    private static let tempFilePrefix = "fastword-file-"

    /// Deletes decoded copies a previous run left behind when it crashed or
    /// was force-quit mid-job — up to ~230 MB per hour of audio each. Only
    /// safe while no job is running.
    static func removeLeftoverTempFiles() {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory
        guard let names = try? fm.contentsOfDirectory(atPath: dir.path) else { return }
        for name in names where name.hasPrefix(tempFilePrefix) && name.hasSuffix(".f32") {
            try? fm.removeItem(at: dir.appendingPathComponent(name))
        }
    }

    /// Extensions only ffmpeg can read for us.
    private static let ffmpegOnlyExtensions = ["ogg", "oga", "opus", "webm", "mkv", "wma"]

    /// Content types for the open panel. Any audio or movie type covers what
    /// AVFoundation reads. Older macOS versions don't declare UTIs for the
    /// ffmpeg-only formats, so those are built from their extensions.
    static var supportedContentTypes: [UTType] {
        var types: [UTType] = [.audio, .movie]
        if ffmpegPath != nil {
            for type in ffmpegOnlyExtensions.compactMap({ UTType(filenameExtension: $0) })
            where !types.contains(where: { type.conforms(to: $0) }) {
                types.append(type)
            }
        }
        return types
    }

    /// Human-readable list for the drop zone caption — only formats that
    /// will actually decode on this Mac. (macOS 15+ reads Ogg/Opus natively.)
    static var supportedFormatsSummary: String {
        var names = ["MP3", "M4A", "WAV", "AIFF", "FLAC", "MP4", "MOV"]
        let hasFFmpeg = ffmpegPath != nil
        if hasFFmpeg || avFoundationOpens(extension: "ogg") {
            names += ["OGG", "Opus"]
        }
        if hasFFmpeg || avFoundationOpens(extension: "webm") {
            names.append("WebM")
        }
        return names.joined(separator: ", ")
    }

    private static func avFoundationOpens(extension ext: String) -> Bool {
        guard let type = UTType(filenameExtension: ext) else { return false }
        return AVURLAsset.audiovisualTypes().contains { $0.rawValue == type.identifier }
    }

    /// Decodes `source` into a new temp file. `progress` receives 0…1 (or
    /// nil while the amount of work is unknown, i.e. on the ffmpeg path) from
    /// a background thread. Cancelling the calling Task stops the work and
    /// removes the partial output; on any error the output is removed too.
    static func decode(_ source: URL, progress: @escaping (Double?) -> Void) async throws -> DecodedAudio {
        let output = FileManager.default.temporaryDirectory
            .appendingPathComponent("\(tempFilePrefix)\(UUID().uuidString).f32")
        let token = CancelToken()
        do {
            return try await withTaskCancellationHandler {
                try await decode(source, to: output, token: token, progress: progress)
            } onCancel: {
                token.cancel()
            }
        } catch {
            try? FileManager.default.removeItem(at: output)
            throw token.isCancelled ? CancellationError() : error
        }
    }

    private static func decode(
        _ source: URL, to output: URL, token: CancelToken, progress: @escaping (Double?) -> Void
    ) async throws -> DecodedAudio {
        let ffmpeg = ffmpegPath
        let asset = AVURLAsset(url: source)
        let tracks: [AVAssetTrack]
        let duration: CMTime
        do {
            tracks = try await asset.loadTracks(withMediaType: .audio)
            duration = try await asset.load(.duration)
        } catch {
            // AVFoundation can't even parse the container.
            guard let ffmpeg else { throw AudioDecodeError.unsupportedFormat(ffmpegAvailable: false) }
            return try await decodeWithFFmpeg(ffmpeg, source: source, to: output, token: token, progress: progress)
        }
        try token.check()

        guard !tracks.isEmpty else {
            // Either a silent video or an audio codec AVFoundation doesn't
            // expose as a track (e.g. Opus inside some containers). ffmpeg
            // tells the two apart for us.
            guard let ffmpeg else { throw AudioDecodeError.noAudioTrack }
            return try await decodeWithFFmpeg(ffmpeg, source: source, to: output, token: token, progress: progress)
        }

        let seconds = duration.isNumeric ? duration.seconds : 0
        do {
            return try await runBlocking {
                try readWithAssetReader(asset, tracks: tracks, expectedSeconds: seconds,
                                        to: output, token: token, progress: progress)
            }
        } catch AssetReaderError.couldNotStart(let reason) {
            // The container parsed but the codec inside didn't decode.
            if let ffmpeg {
                try? FileManager.default.removeItem(at: output)
                return try await decodeWithFFmpeg(ffmpeg, source: source, to: output, token: token, progress: progress)
            }
            NSLog("FastWord: AVAssetReader could not start: %@", reason)
            throw AudioDecodeError.unsupportedFormat(ffmpegAvailable: false)
        }
    }

    // MARK: - AVFoundation

    private enum AssetReaderError: Error {
        /// Nothing was decoded — worth retrying with ffmpeg.
        case couldNotStart(String)
    }

    private static func readWithAssetReader(
        _ asset: AVAsset, tracks: [AVAssetTrack], expectedSeconds: Double,
        to output: URL, token: CancelToken, progress: (Double?) -> Void
    ) throws -> DecodedAudio {
        let reader: AVAssetReader
        do {
            reader = try AVAssetReader(asset: asset)
        } catch {
            throw AssetReaderError.couldNotStart(error.localizedDescription)
        }
        // The mix output sums every audio track (a screen recording with
        // separate mic and system tracks keeps both voices), and the reader
        // resamples and downmixes straight to the sidecar's format.
        let settings: [String: Any] = [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: sampleRate,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 32,
            AVLinearPCMIsFloatKey: true,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ]
        let mix = AVAssetReaderAudioMixOutput(audioTracks: tracks, audioSettings: settings)
        mix.alwaysCopiesSampleData = false
        guard reader.canAdd(mix) else {
            throw AssetReaderError.couldNotStart("reader rejected output settings")
        }
        reader.add(mix)
        guard reader.startReading() else {
            throw AssetReaderError.couldNotStart(reader.error?.localizedDescription ?? "startReading failed")
        }

        guard FileManager.default.createFile(atPath: output.path, contents: nil),
              let handle = try? FileHandle(forWritingTo: output) else {
            reader.cancelReading()
            throw AudioDecodeError.readFailed(NSLocalizedString("Couldn't create a temporary file.", comment: ""))
        }
        defer { try? handle.close() }

        // Batch writes: sample buffers are only a few KB each.
        let flushThreshold = 1 << 20
        var pending = Data()
        pending.reserveCapacity(flushThreshold + 65_536)
        var totalBytes = 0
        var lastReported = -1.0

        while let buffer = mix.copyNextSampleBuffer() {
            if token.isCancelled {
                reader.cancelReading()
                throw CancellationError()
            }
            guard let block = CMSampleBufferGetDataBuffer(buffer) else { continue }
            let length = CMBlockBufferGetDataLength(block)
            guard length > 0 else { continue }
            let offset = pending.count
            pending.count += length
            let status = pending.withUnsafeMutableBytes { raw in
                CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length,
                                           destination: raw.baseAddress! + offset)
            }
            guard status == kCMBlockBufferNoErr else {
                reader.cancelReading()
                throw AudioDecodeError.readFailed("CMBlockBuffer error \(status)")
            }
            // A multichannel downmix sums channels and can overshoot full
            // scale (5.1 → mono easily peaks above 2.0); the engines expect
            // samples in [-1, 1].
            pending.withUnsafeMutableBytes { raw in
                let floats = (raw.baseAddress! + offset).assumingMemoryBound(to: Float.self)
                var low: Float = -1, high: Float = 1
                vDSP_vclip(floats, 1, &low, &high, floats, 1, vDSP_Length(length / 4))
            }
            totalBytes += length
            if pending.count >= flushThreshold {
                try write(pending, to: handle)
                pending.removeAll(keepingCapacity: true)
            }
            if expectedSeconds > 0 {
                let decoded = Double(totalBytes / 4) / sampleRate
                let fraction = min(1, decoded / expectedSeconds)
                if fraction - lastReported >= 0.005 {
                    lastReported = fraction
                    progress(fraction)
                }
            }
        }

        switch reader.status {
        case .completed:
            break
        case .cancelled:
            throw CancellationError()
        default:
            let reason = reader.error?.localizedDescription ?? "unknown error"
            if totalBytes == 0 { throw AssetReaderError.couldNotStart(reason) }
            throw AudioDecodeError.readFailed(reason)
        }
        if !pending.isEmpty {
            try write(pending, to: handle)
        }

        let samples = totalBytes / 4
        guard samples >= minimumSamples else { throw AudioDecodeError.tooShort }
        progress(1)
        return DecodedAudio(url: output, sampleCount: samples, usedFFmpeg: false)
    }

    private static func write(_ data: Data, to handle: FileHandle) throws {
        do {
            try handle.write(contentsOf: data)
        } catch {
            throw AudioDecodeError.readFailed(error.localizedDescription)
        }
    }

    // MARK: - ffmpeg

    private static func decodeWithFFmpeg(
        _ ffmpeg: String, source: URL, to output: URL, token: CancelToken, progress: @escaping (Double?) -> Void
    ) async throws -> DecodedAudio {
        try token.check()
        // ffmpeg doesn't report a total up front, so this path is indeterminate.
        progress(nil)
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: ffmpeg)
        // "file:" keeps ffmpeg from reading a colon in the name as a protocol.
        proc.arguments = [
            "-nostdin", "-hide_banner", "-loglevel", "error",
            "-i", "file:" + source.path,
            "-vn", "-ac", "1", "-ar", String(Int(sampleRate)), "-f", "f32le",
            "-y", "file:" + output.path,
        ]
        let stderr = Pipe()
        proc.standardInput = FileHandle.nullDevice
        proc.standardOutput = FileHandle.nullDevice
        proc.standardError = stderr

        let (status, errorText): (Int32, String) = try await runBlocking {
            do {
                try proc.run()
            } catch {
                throw AudioDecodeError.readFailed(error.localizedDescription)
            }
            token.onCancel { proc.terminate() }
            // With -loglevel error stderr stays tiny, and draining it here
            // means ffmpeg can never block on a full pipe.
            let data = stderr.fileHandleForReading.readDataToEndOfFile()
            proc.waitUntilExit()
            return (proc.terminationStatus, String(data: data, encoding: .utf8) ?? "")
        }
        try token.check()

        guard status == 0 else {
            NSLog("FastWord: ffmpeg failed (%d): %@", status, errorText)
            let lower = errorText.lowercased()
            if lower.contains("does not contain any stream") || lower.contains("matches no streams") {
                throw AudioDecodeError.noAudioTrack
            }
            if errorText.isEmpty || lower.contains("invalid data found")
                || lower.contains("could not find codec") || lower.contains("unknown format") {
                throw AudioDecodeError.unsupportedFormat(ffmpegAvailable: true)
            }
            let firstLine = errorText.split(separator: "\n").first.map(String.init) ?? "ffmpeg exited with status \(status)"
            throw AudioDecodeError.readFailed(firstLine)
        }

        let attrs = try? FileManager.default.attributesOfItem(atPath: output.path)
        let bytes = (attrs?[.size] as? NSNumber)?.intValue ?? 0
        let samples = bytes / 4
        guard samples >= minimumSamples else { throw AudioDecodeError.tooShort }
        progress(1)
        return DecodedAudio(url: output, sampleCount: samples, usedFFmpeg: true)
    }

    // MARK: - Plumbing

    /// Runs blocking I/O off the Swift concurrency pool — an hour of audio
    /// keeps a thread busy for several seconds.
    private static func runBlocking<T>(_ work: @escaping () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { cont in
            DispatchQueue.global(qos: .userInitiated).async {
                cont.resume(with: Result { try work() })
            }
        }
    }
}

/// Cancellation flag shared between the async caller and the blocking
/// worker. A handler registered after cancellation runs immediately, so a
/// cancel that races with starting ffmpeg still kills it.
private final class CancelToken {
    private let lock = NSLock()
    private var cancelled = false
    private var handler: (() -> Void)?

    var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }

    func check() throws {
        if isCancelled { throw CancellationError() }
    }

    func cancel() {
        lock.lock()
        cancelled = true
        let pending = handler
        handler = nil
        lock.unlock()
        pending?()
    }

    func onCancel(_ action: @escaping () -> Void) {
        lock.lock()
        if cancelled {
            lock.unlock()
            action()
            return
        }
        handler = action
        lock.unlock()
    }
}
