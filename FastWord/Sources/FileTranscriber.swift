import Foundation

/// Runs one "Transcribe File" job at a time: decode → transcribe → clean up.
///
/// Each job gets its own sidecar process, started for the job and killed at
/// the end. The sidecar handles requests strictly one after another, so
/// sharing AppController's instance would leave push-to-talk waiting behind
/// an hour-long file; a second process keeps dictation instant and makes
/// cancelling a job as simple as killing it.
@MainActor
final class FileTranscriber: ObservableObject {
    enum Phase: Equatable {
        case idle
        case decoding(progress: Double?)
        case loadingModel
        case transcribing(progress: Double)
        case finished
        case failed(String)
        case cancelled
    }

    @Published private(set) var phase: Phase = .idle
    @Published private(set) var sourceURL: URL?
    @Published private(set) var sourceDuration: TimeInterval?
    @Published private(set) var result: FileTranscript?
    /// Language for the next job. Follows the Settings choice until the user
    /// picks something else in the Transcribe File window.
    @Published private(set) var languageCode: String = AppSettings.transcriptionLanguageCode

    /// Called once per successful job, after `result` is set.
    var onFinished: ((FileTranscript) -> Void)?

    private var job: Task<Void, Never>?
    private var jobID: UUID?
    private var sidecar: SidecarClient?
    private var tempURL: URL?
    private var activity: NSObjectProtocol?
    private var languageOverridden = false
    /// First progress report of the current phase — the ETA is extrapolated
    /// from the rate since then, so model loading doesn't skew it.
    private var rateAnchor: (time: Date, progress: Double)?

    var isBusy: Bool {
        switch phase {
        case .decoding, .loadingModel, .transcribing: return true
        case .idle, .finished, .failed, .cancelled: return false
        }
    }

    /// Estimated seconds left in the current phase, once there is enough
    /// progress (5%) for the rate to mean something.
    var estimatedTimeRemaining: TimeInterval? {
        let progress: Double
        switch phase {
        case .decoding(let p?): progress = p
        case .transcribing(let p): progress = p
        default: return nil
        }
        guard progress >= 0.05, progress < 1, let anchor = rateAnchor else { return nil }
        let done = progress - anchor.progress
        let elapsed = Date().timeIntervalSince(anchor.time)
        guard done > 0.01, elapsed > 1 else { return nil }
        return elapsed / done * (1 - progress)
    }

    /// "about 3 min left", once there is an estimate.
    var timeRemainingText: String? {
        guard let eta = estimatedTimeRemaining else { return nil }
        if eta < 60 {
            return NSLocalizedString("less than a minute left", comment: "")
        }
        let minutes = Int((eta / 60).rounded(.up))
        return String(format: NSLocalizedString("about %ld min left", comment: ""), minutes)
    }

    /// One line for the menu bar menu about the running job, or about the
    /// last one once it ended — with the window closed, that line is the
    /// only sign it did.
    var menuStatusLine: String? {
        switch phase {
        case .decoding:
            return NSLocalizedString("Decoding file…", comment: "")
        case .loadingModel:
            return NSLocalizedString("Transcribing file…", comment: "")
        case .transcribing(let p):
            let format = NSLocalizedString("Transcribing file… %@", comment: "")
            let detail = [Self.percent(p), timeRemainingText].compactMap { $0 }.joined(separator: " · ")
            return String(format: format, detail)
        case .finished:
            let format = NSLocalizedString("File transcribed: %@", comment: "")
            return String(format: format, Self.menuName(result?.sourceName ?? ""))
        case .failed:
            return NSLocalizedString("File transcription failed", comment: "")
        case .idle, .cancelled:
            return nil
        }
    }

    /// A menu grows to fit its widest item, so a long recording name would
    /// stretch the whole FastWord menu across the screen. Cut in the middle,
    /// like Finder does, keeping the extension.
    static func menuName(_ name: String, limit: Int = 40) -> String {
        guard name.count > limit else { return name }
        let tail = 12
        return String(name.prefix(limit - tail - 1)) + "…" + String(name.suffix(tail))
    }

    /// "42%" — or "42 %" where the locale wants a space. Truncated, so
    /// "100%" only shows up once the job is really done.
    static func percent(_ fraction: Double) -> String {
        let whole = Int((min(max(fraction, 0), 1) * 100 + 1e-9).rounded(.down))
        return (Double(whole) / 100).formatted(.percent.precision(.fractionLength(0)))
    }

    // MARK: - Language

    func setLanguage(_ code: String) {
        languageOverridden = true
        languageCode = code
    }

    /// Picks up a changed Settings language unless the user already chose
    /// one for file jobs in this session.
    func refreshDefaultLanguage() {
        guard !languageOverridden else { return }
        languageCode = AppSettings.transcriptionLanguageCode
    }

    // MARK: - Models

    /// Display name of the engine a new job would use.
    static func activeModelDisplayName() -> String {
        SidecarClient.Engine.current().displayName
    }

    // MARK: - Jobs

    func start(url: URL) {
        guard !isBusy else { return }
        let id = UUID()
        jobID = id
        sourceURL = url
        sourceDuration = nil
        result = nil
        rateAnchor = nil
        phase = .decoding(progress: nil)
        // An hour-long job must not be cut short by idle sleep, and App Nap
        // would throttle the progress updates.
        activity = ProcessInfo.processInfo.beginActivity(
            options: .userInitiated, reason: "Transcribing a file")
        job = Task { [weak self] in
            await self?.run(url: url, id: id)
        }
    }

    /// Kills the job's sidecar, stops decoding and removes the temp file.
    func cancel() {
        guard isBusy else { return }
        jobID = nil
        job?.cancel()
        job = nil
        sidecar?.stop()
        sidecar = nil
        removeTempFile()
        endActivity()
        phase = .cancelled
    }

    /// Back to the drop zone.
    func reset() {
        guard !isBusy else { return }
        phase = .idle
        sourceURL = nil
        sourceDuration = nil
        result = nil
    }

    private func run(url: URL, id: UUID) async {
        let startedAt = Date()
        // Resolved once: decoding an hour of audio takes a while, and a model
        // switched in Settings meanwhile must not change what this job runs.
        let engine = SidecarClient.Engine.current()
        let language = engine.gigaAM ? "ru" : languageCode
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }

        do {
            let decoded = try await AudioFileDecoder.decode(url) { [weak self] progress in
                Task { @MainActor [weak self] in self?.decodeProgressed(progress, id: id) }
            }
            guard jobID == id else {
                // Cancelled just as decoding finished.
                try? FileManager.default.removeItem(at: decoded.url)
                return
            }
            tempURL = decoded.url
            sourceDuration = decoded.duration
            rateAnchor = nil
            phase = .loadingModel

            let client = SidecarClient()
            try client.launch(engine: engine)
            sidecar = client
            let reply = try await client.transcribeFile(
                pcmPath: decoded.url.path, languageCode: language, engine: engine
            ) { [weak self] progress in
                Task { @MainActor [weak self] in self?.transcribeProgressed(progress, id: id) }
            }
            guard jobID == id else { return }
            client.stop()
            sidecar = nil
            removeTempFile()

            let transcript = FileTranscript(
                sourceName: url.lastPathComponent,
                sourceDuration: decoded.duration,
                modelName: engine.displayName,
                languageCode: language,
                createdAt: Date(),
                processingTime: Date().timeIntervalSince(startedAt),
                segments: Self.polish(reply.segments, text: reply.text, duration: decoded.duration)
            )
            result = transcript
            jobID = nil
            job = nil
            endActivity()
            phase = .finished
            onFinished?(transcript)
        } catch {
            // A cancelled job was already cleaned up by cancel().
            guard jobID == id else { return }
            jobID = nil
            job = nil
            sidecar?.stop()
            sidecar = nil
            removeTempFile()
            endActivity()
            phase = .failed(Self.message(for: error))
        }
    }

    private func decodeProgressed(_ progress: Double?, id: UUID) {
        guard jobID == id, case .decoding(let current) = phase else { return }
        if let progress, let current, progress <= current { return }
        if let progress, rateAnchor == nil { rateAnchor = (Date(), progress) }
        phase = .decoding(progress: progress)
    }

    private func transcribeProgressed(_ progress: Double, id: UUID) {
        guard jobID == id else { return }
        let clamped = min(max(progress, 0), 1)
        switch phase {
        case .loadingModel:
            // The first report means the model is loaded and inference began.
            rateAnchor = (Date(), clamped)
            phase = .transcribing(progress: clamped)
        case .transcribing(let current) where clamped > current:
            phase = .transcribing(progress: clamped)
        default:
            break
        }
    }

    /// Applies the same cleanup as dictation, per segment, dropping the
    /// segments it empties (a lone "Продолжение следует…" over silence).
    private static func polish(_ segments: [TranscriptSegment], text: String, duration: TimeInterval) -> [TranscriptSegment] {
        var segments = segments
        // A reply with text but no segments shouldn't lose the text.
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if segments.isEmpty, !trimmed.isEmpty {
            segments = [TranscriptSegment(start: 0, end: duration, text: trimmed)]
        }
        guard AppSettings.cleanupEnabled else { return segments }
        return segments.compactMap { seg in
            let cleaned = TranscriptionPostProcessor.clean(seg.text)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return cleaned.isEmpty ? nil : TranscriptSegment(start: seg.start, end: seg.end, text: cleaned)
        }
    }

    private static func message(for error: Error) -> String {
        if let sidecarError = error as? SidecarError, case .remote(let message) = sidecarError {
            let format = NSLocalizedString("Transcription failed: %@", comment: "")
            return String(format: format, message)
        }
        return error.localizedDescription
    }

    private func removeTempFile() {
        if let url = tempURL {
            try? FileManager.default.removeItem(at: url)
        }
        tempURL = nil
    }

    private func endActivity() {
        if let activity {
            ProcessInfo.processInfo.endActivity(activity)
        }
        activity = nil
    }
}
