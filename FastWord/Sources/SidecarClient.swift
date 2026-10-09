import Foundation

enum SidecarError: LocalizedError {
    case notRunning
    case decodeFailed
    case remote(String)
    case binaryNotFound
    case modelNotFound

    // Without this, `error.localizedDescription` collapses to the unhelpful
    // "The operation couldn't be completed. (SidecarError error 0.)" — which
    // is what the user actually sees in the HUD when sherpa-onnx or whisper
    // raises a real message we want to surface.
    var errorDescription: String? {
        switch self {
        case .notRunning: return "sidecar not running"
        case .decodeFailed: return "could not decode sidecar response"
        case .remote(let msg): return msg
        case .binaryNotFound:
            return NSLocalizedString("The transcription engine (fastword-sidecar) wasn't found. Reinstall FastWord.", comment: "")
        case .modelNotFound:
            return NSLocalizedString("No speech model is installed. Download one in Settings → Models.", comment: "")
        }
    }
}

final class SidecarClient {
    var onCrash: ((String) -> Void)?

    /// The engine and whisper model a sidecar runs, as Settings has them
    /// right now. A file job resolves this once and keeps it for the whole
    /// job, so switching models in Settings mid-job can neither change what
    /// the job runs nor mislabel its result.
    struct Engine {
        let gigaAM: Bool
        /// Whisper model file; nil when none is installed.
        let modelPath: String?

        static func current() -> Engine {
            // ModelStorage picks the user-selected model if downloaded,
            // otherwise the bundled fallback. As a last resort, an old
            // ~/Library/Caches/fastword copy from earlier dev builds.
            var modelPath = ModelStorage.activeModelPath()
            if modelPath == nil {
                let home = FileManager.default.homeDirectoryForCurrentUser.path
                let cached = "\(home)/Library/Caches/fastword/models/\(ModelCatalog.bundledFilename)"
                if FileManager.default.fileExists(atPath: cached) {
                    modelPath = cached
                }
            }
            return Engine(gigaAM: SidecarClient.usesGigaAM, modelPath: modelPath)
        }

        var displayName: String {
            if gigaAM { return "GigaAM v3" }
            guard let modelPath else { return "Whisper" }
            let filename = (modelPath as NSString).lastPathComponent
            return ModelCatalog.model(for: filename)?.displayName ?? filename
        }
    }

    /// What a finished request resolves to. Dictation only reads `text`;
    /// `transcribe_file` replies also carry timed segments.
    private struct Reply {
        let text: String
        let segments: [TranscriptSegment]?
        var warning: String? = nil
    }

    private var process: Process?
    private var stdinPipe: Pipe?
    private var stdoutPipe: Pipe?
    private var stderrPipe: Pipe?
    private let queue = DispatchQueue(label: "fastword.sidecar")
    private var pending: [String: CheckedContinuation<Reply, Error>] = [:]
    /// Interim `{"id","progress"}` lines of long jobs, keyed like `pending`.
    private var progressHandlers: [String: (Double) -> Void] = [:]
    private var readBuffer = Data()
    private var stderrBuffer = Data()
    /// Guards `pending`, `progressHandlers`, and `process` together with
    /// `stdinPipe` (requests are sent from any thread, and the termination
    /// handler runs on a background one).
    private let stateLock = NSLock()
    /// Serializes starting and stopping the process, and with it
    /// `stdoutPipe`/`stderrPipe`: a request restarts a dead sidecar from
    /// whatever thread it runs on while Settings restarts it from the main
    /// one, and two restarts interleaving would orphan a process. Recursive
    /// because `restart()` is `stop()` plus `start()`. Taken before
    /// `stateLock`, never while holding it.
    private let lifecycleLock = NSRecursiveLock()
    /// Serializes writes to the sidecar's stdin. A request line is often
    /// longer than the pipe buffer (one second of dictation is ~85 KB of
    /// base64), so a write blocks while the sidecar is busy, and a second
    /// request written meanwhile would interleave with it into a line
    /// neither request ever gets an answer to.
    private let writeLock = NSLock()

    /// A write into the stdin of a sidecar that has already died must fail
    /// with EPIPE — which every send path handles — instead of SIGPIPE
    /// silently killing the whole app.
    private static let ignoreSIGPIPE: Void = {
        _ = signal(SIGPIPE, SIG_IGN)
    }()

    var isRunning: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return process?.isRunning ?? false
    }

    func start() {
        do {
            try launch()
        } catch SidecarError.binaryNotFound, SidecarError.modelNotFound {
            NSLog("FastWord: sidecar binary or model not found. Reinstall the app.")
        } catch {
            let msg = error.localizedDescription
            NSLog("FastWord: failed to start sidecar: %@", msg)
            DispatchQueue.main.async { [weak self] in self?.onCrash?("failed to start sidecar: \(msg)") }
        }
    }

    /// Same as `start()`, but throws instead of only logging when the
    /// sidecar can't be started — a file job needs to tell the user why.
    func launch(engine: Engine = .current()) throws {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        _ = Self.ignoreSIGPIPE
        let fm = FileManager.default
        var sidecarBin: String?

        // 1. Prefer bundled sidecar binary (production: shipped inside the .app).
        if let resources = Bundle.main.resourcePath {
            let bundled = "\(resources)/fastword-sidecar"
            if fm.fileExists(atPath: bundled) {
                sidecarBin = bundled
            }
        }

        // 2. Fall back to development build under sidecar-rust/target/release.
        if sidecarBin == nil {
            // Locate the repo by walking up from the bundle until we find Cargo.toml.
            let repoCandidate = Self.findRepoRoot()
            if let repo = repoCandidate {
                let devBin = "\(repo)/sidecar-rust/target/release/fastword-sidecar"
                if fm.fileExists(atPath: devBin) {
                    sidecarBin = devBin
                }
            }
        }

        guard let bin = sidecarBin else { throw SidecarError.binaryNotFound }
        // The sidecar loads the whisper model lazily, so a GigaAM-only setup
        // runs fine without one.
        guard engine.modelPath != nil || engine.gigaAM else { throw SidecarError.modelNotFound }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: bin)
        var env = ProcessInfo.processInfo.environment
        if let model = engine.modelPath {
            env["FASTWORD_MODEL"] = model
        }
        env["FASTWORD_IDLE_EVICT"] = String(AppSettings.idleEviction.seconds)
        // Point the sidecar at the locally-installed GigaAM v3 model
        // directory (lives in Application Support; populated on demand by
        // GigaAMInstaller when the user enables the toggle). The sidecar only
        // loads it on the first `engine=gigaam` request, so this is cheap
        // even when the user never uses GigaAM.
        let gigaamDir = GigaAMInstaller.installDirectory.path
        if fm.fileExists(atPath: "\(gigaamDir)/model.int8.onnx") {
            env["FASTWORD_GIGAAM_MODEL"] = gigaamDir
        }
        // Speaker detection models, downloaded on demand by
        // DiarizationInstaller. Only read by a `transcribe_file` request that
        // asks for speakers; without them it still transcribes, with a warning.
        if DiarizationInstaller.isInstalled {
            env["FASTWORD_DIARIZATION_MODEL"] = DiarizationInstaller.installDirectory.path
        }
        proc.environment = env
        let stdin = Pipe(); let stdout = Pipe(); let stderr = Pipe()
        proc.standardInput = stdin
        proc.standardOutput = stdout
        proc.standardError = stderr

        stdout.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            // Empty means EOF: the sidecar is gone. Left installed, the
            // handler would keep firing with empty reads — a core at 100%
            // until the next request restarts the sidecar.
            guard !data.isEmpty else { handle.readabilityHandler = nil; return }
            self?.handleStdout(data)
        }
        stderr.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { handle.readabilityHandler = nil; return }
            self?.queue.async {
                self?.stderrBuffer.append(data)
                // Cap to avoid unbounded growth; we only ever surface the tail.
                if let buf = self?.stderrBuffer, buf.count > 16_384 {
                    self?.stderrBuffer = buf.suffix(8_192)
                }
            }
            if let s = String(data: data, encoding: .utf8) {
                NSLog("FastWord sidecar: %@", s)
            }
        }

        proc.terminationHandler = { [weak self] p in
            // Queued behind any stdout already received, so a reply the
            // sidecar managed to write before dying is still delivered.
            self?.queue.async {
                guard let self else { return }
                // stop()/restart() detach the process before killing it, so
                // only a process that is still the current one died on its
                // own. (A flag set in stop() isn't enough: restart() clears it
                // again before the old process has finished exiting.) Checked
                // under the same lock that takes the pending requests, so a
                // request already sent to a replacement process is never
                // failed by the old one's death.
                self.stateLock.lock()
                guard self.process === p else {
                    self.stateLock.unlock()
                    return
                }
                let stale = self.pending
                self.pending.removeAll()
                self.progressHandlers.removeAll()
                self.stateLock.unlock()

                let tail = String(data: self.stderrBuffer.suffix(2_048), encoding: .utf8) ?? ""
                let headline: String
                if p.terminationReason == .uncaughtSignal {
                    headline = "sidecar crashed (signal \(p.terminationStatus))"
                } else if p.terminationStatus != 0 {
                    headline = "sidecar exited with status \(p.terminationStatus)"
                } else {
                    headline = "sidecar exited unexpectedly"
                }
                // Nothing will ever answer these now — fail them instead of
                // leaving a dictation or a file job waiting forever. Callers
                // put the error in a one-line status, so it gets the headline;
                // the stderr tail goes to the log through onCrash.
                for cont in stale.values {
                    cont.resume(throwing: SidecarError.remote(headline))
                }
                let summary = "\(headline)\n\(tail)"
                DispatchQueue.main.async { [weak self] in self?.onCrash?(summary) }
            }
        }

        try proc.run()
        stateLock.lock()
        self.process = proc
        self.stdinPipe = stdin
        stateLock.unlock()
        self.stdoutPipe = stdout
        self.stderrPipe = stderr
    }

    func stop() {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        stateLock.lock()
        let proc = process
        process = nil
        stdinPipe = nil
        stateLock.unlock()
        // Stop listening before the kill so a half-written line from the old
        // process can't end up glued to the first reply of the next one.
        stdoutPipe?.fileHandleForReading.readabilityHandler = nil
        stderrPipe?.fileHandleForReading.readabilityHandler = nil
        proc?.terminate()
        stdoutPipe = nil
        stderrPipe = nil
        queue.async {
            self.readBuffer.removeAll()
            self.stderrBuffer.removeAll(keepingCapacity: false)
        }
        // Drop pending continuations so callers don't hang forever.
        failAllPending(SidecarError.notRunning)
    }

    func restart() {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        stop()
        start()
    }

    /// Walk up from the running binary until we find sidecar-rust/Cargo.toml.
    /// Returns the repo root path, or nil if we are running from a bundled
    /// production build (in /Applications/...).
    private static func findRepoRoot() -> String? {
        let fm = FileManager.default
        var url = Bundle.main.bundleURL
        for _ in 0..<8 {
            url.deleteLastPathComponent()
            let cargo = url.appendingPathComponent("sidecar-rust/Cargo.toml").path
            if fm.fileExists(atPath: cargo) {
                return url.path
            }
        }
        return nil
    }

    // MARK: - Requests

    /// GigaAM is picked as the active model in the Models list (sentinel
    /// filename) and its files are actually on disk.
    static var usesGigaAM: Bool {
        AppSettings.activeModelFilename == ModelCatalog.gigaAMSentinel && GigaAMInstaller.isInstalled
    }

    /// Engine, language and filter fields shared by dictation and file jobs,
    /// so both follow exactly the same routing rules.
    private static func engineOptions(languageCode: String, gigaAM: Bool) -> [String: Any] {
        // Empty string means "auto-detect"; sidecar treats it as a flag to drop
        // the language hint and let Whisper auto-pick.
        var opts: [String: Any] = ["language": languageCode]
        // Engine routing: GigaAM is selected as the active model in the
        // Models list (sentinel filename). It's Russian-only, so we also
        // pin the language hint to "ru" regardless of what the picker says.
        if gigaAM {
            opts["engine"] = "gigaam"
            opts["language"] = "ru"
        }
        // When the user keeps the picker on "Auto", bias Whisper toward the
        // system language by passing a short hint phrase in that language as
        // the initial_prompt. This is the documented Superwhisper trick — it
        // dramatically reduces wrong-language detections on short clips.
        if languageCode.isEmpty {
            let systemCode = TranscriptionLanguage.systemDefault().code
            if let hint = TranscriptionLanguage.promptHint(forCode: systemCode) {
                opts["initial_prompt"] = hint
            }
        }
        // When Skip-empty is on, push whisper's own no-speech filter slightly
        // tighter than the default 0.6 so it drops more clearly-silent clips
        // before they hallucinate. 0.0 disables the filter entirely.
        opts["no_speech_thold"] = AppSettings.skipEmpty ? 0.6 : 0.0
        return opts
    }

    func warmup() async throws {
        let id = UUID().uuidString
        _ = try await send(["id": id, "cmd": "warmup"], id: id)
    }

    func transcribe(pcm: Data) async throws -> String {
        let id = UUID().uuidString
        var req: [String: Any] = [
            "id": id,
            "cmd": "transcribe",
            "sample_rate": 16000,
            "audio_b64": pcm.base64EncodedString()
        ]
        req.merge(Self.engineOptions(languageCode: AppSettings.transcriptionLanguageCode,
                                     gigaAM: Self.usesGigaAM)) { _, new in new }
        return try await send(req, id: id).text
    }

    /// A finished `transcribe_file` job.
    struct FileReply {
        let text: String
        /// Each with a `speaker` when speaker detection ran and worked. (The
        /// reply's `speakers` list only sums these up; the app counts talk
        /// time from the segments it keeps after cleanup.)
        let segments: [TranscriptSegment]
        /// What went wrong on the side — speaker detection failing — without
        /// costing the transcript.
        let warning: String?
    }

    /// Transcribes a whole file of raw f32le mono 16 kHz samples. The sidecar
    /// streams progress (0…1, overall — speaker detection runs alongside) to
    /// `onProgress` on a background queue, then answers once with the full
    /// text and its timed segments. `engine` must be the one this client was
    /// launched with.
    func transcribeFile(
        pcmPath: String,
        languageCode: String,
        engine: Engine,
        speakers: SpeakerMode = .off,
        onProgress: @escaping (Double) -> Void
    ) async throws -> FileReply {
        let id = UUID().uuidString
        var req: [String: Any] = [
            "id": id,
            "cmd": "transcribe_file",
            "audio_path": pcmPath,
            "sample_rate": 16000
        ]
        req.merge(Self.engineOptions(languageCode: languageCode, gigaAM: engine.gigaAM)) { _, new in new }
        // Sent only when on: the sidecar rejects fields it doesn't know, and
        // a job without speakers stays exactly the request it always was.
        switch speakers {
        case .off:
            break
        case .auto:
            req["diarize"] = true
        case .exact(let count):
            req["diarize"] = true
            req["num_speakers"] = count
        }
        let reply = try await send(req, id: id, onProgress: onProgress)
        return FileReply(text: reply.text, segments: reply.segments ?? [], warning: reply.warning)
    }

    private func send(
        _ request: [String: Any], id: String, onProgress: ((Double) -> Void)? = nil
    ) async throws -> Reply {
        guard case let (target, stdin)? = liveConnection() else { throw SidecarError.notRunning }
        let line = try JSONSerialization.data(withJSONObject: request) + Data([0x0A])
        return try await withCheckedThrowingContinuation { cont in
            stateLock.lock()
            // stop() (a cancelled file job) or the termination handler may
            // have swept `pending` since `target` was read; nothing would
            // ever answer a request filed after that sweep.
            guard process === target, target.isRunning else {
                stateLock.unlock()
                cont.resume(throwing: SidecarError.notRunning)
                return
            }
            pending[id] = cont
            if let onProgress { progressHandlers[id] = onProgress }
            stateLock.unlock()
            do {
                writeLock.lock()
                defer { writeLock.unlock() }
                try stdin.fileHandleForWriting.write(contentsOf: line)
            } catch {
                // The termination handler may have failed it already.
                stateLock.lock()
                let stillPending = pending.removeValue(forKey: id)
                progressHandlers.removeValue(forKey: id)
                stateLock.unlock()
                stillPending?.resume(throwing: error)
            }
        }
    }

    /// The process and its stdin, read together. A sidecar that crashed
    /// earlier stays dead until something restarts it; that happens here, on
    /// the next request, rather than failing every one. Checked and restarted
    /// under `lifecycleLock`, so requests that find the same dead process
    /// restart it once and all go to the replacement. (A synchronous helper
    /// because locks can't be taken directly in an async function.)
    private func liveConnection() -> (Process, Pipe)? {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        stateLock.lock()
        let dead = process.map { !$0.isRunning } ?? false
        stateLock.unlock()
        if dead {
            NSLog("FastWord: sidecar is not running, restarting it")
            restart()
        }
        stateLock.lock()
        defer { stateLock.unlock() }
        guard let process, let stdinPipe else { return nil }
        return (process, stdinPipe)
    }

    private func failAllPending(_ error: Error) {
        stateLock.lock()
        let stale = pending
        pending.removeAll()
        progressHandlers.removeAll()
        stateLock.unlock()
        for cont in stale.values {
            cont.resume(throwing: error)
        }
    }

    // MARK: - Replies

    private func handleStdout(_ data: Data) {
        queue.async {
            self.readBuffer.append(data)
            while let nl = self.readBuffer.firstIndex(of: 0x0A) {
                let lineData = self.readBuffer.subdata(in: 0..<nl)
                self.readBuffer.removeSubrange(0...nl)
                self.dispatchLine(lineData)
            }
        }
    }

    private func dispatchLine(_ data: Data) {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = obj["id"] as? String else { return }
        // Interim progress of a long job: report it and keep the request
        // waiting for its final text/error line.
        if obj["text"] == nil, obj["error"] == nil,
           let progress = (obj["progress"] as? NSNumber)?.doubleValue {
            stateLock.lock()
            let handler = progressHandlers[id]
            stateLock.unlock()
            handler?(progress)
            return
        }
        stateLock.lock()
        let cont = pending.removeValue(forKey: id)
        progressHandlers.removeValue(forKey: id)
        stateLock.unlock()
        guard let cont else { return }
        if let err = obj["error"] as? String {
            cont.resume(throwing: SidecarError.remote(err))
        } else if let text = obj["text"] as? String {
            let warning = (obj["warning"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            cont.resume(returning: Reply(
                text: text,
                segments: Self.parseSegments(obj["segments"]),
                warning: warning?.isEmpty == false ? warning : nil))
        } else {
            cont.resume(throwing: SidecarError.decodeFailed)
        }
    }

    /// `[{"start":0.0,"end":4.2,"text":"…","speaker":0}, …]` → segments,
    /// skipping any malformed or empty entry rather than failing the whole
    /// transcript. A missing or invalid speaker just leaves it unlabelled.
    private static func parseSegments(_ value: Any?) -> [TranscriptSegment]? {
        guard let raw = value as? [[String: Any]] else { return nil }
        return raw.compactMap { item in
            guard let start = (item["start"] as? NSNumber)?.doubleValue,
                  let end = (item["end"] as? NSNumber)?.doubleValue,
                  let text = (item["text"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !text.isEmpty else { return nil }
            let speaker = (item["speaker"] as? NSNumber)?.intValue
            return TranscriptSegment(start: start, end: max(start, end), text: text,
                                     speaker: speaker.flatMap { $0 >= 0 ? $0 : nil })
        }
    }
}
