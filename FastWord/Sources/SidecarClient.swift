import Foundation

enum SidecarError: LocalizedError {
    case notRunning
    case decodeFailed
    case remote(String)

    // Without this, `error.localizedDescription` collapses to the unhelpful
    // "The operation couldn't be completed. (SidecarError error 0.)" — which
    // is what the user actually sees in the HUD when sherpa-onnx or whisper
    // raises a real message we want to surface.
    var errorDescription: String? {
        switch self {
        case .notRunning: return "sidecar not running"
        case .decodeFailed: return "could not decode sidecar response"
        case .remote(let msg): return msg
        }
    }
}

final class SidecarClient {
    var onCrash: ((String) -> Void)?

    private var process: Process?
    private var stdinPipe: Pipe?
    private var stdoutPipe: Pipe?
    private var stderrPipe: Pipe?
    private let queue = DispatchQueue(label: "fastword.sidecar")
    private var pending: [String: CheckedContinuation<String, Error>] = [:]
    private var readBuffer = Data()
    private var stderrBuffer = Data()
    private let pendingLock = NSLock()
    /// Set to true when caller asked us to stop, so the termination handler
    /// doesn't surface a fake "crashed" message during a clean shutdown/restart.
    private var stopping = false

    func start() {
        stopping = false
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

        // Resolve the model path through ModelStorage — picks the user-selected
        // model if downloaded, otherwise the bundled fallback. As a last
        // resort, an old ~/Library/Caches/fastword copy from earlier dev builds.
        var modelPath = ModelStorage.activeModelPath()
        if modelPath == nil {
            let home = fm.homeDirectoryForCurrentUser.path
            let cached = "\(home)/Library/Caches/fastword/models/\(ModelCatalog.bundledFilename)"
            if fm.fileExists(atPath: cached) {
                modelPath = cached
            }
        }

        guard let bin = sidecarBin, let model = modelPath else {
            NSLog("FastWord: sidecar binary or model not found. Reinstall the app.")
            return
        }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: bin)
        var env = ProcessInfo.processInfo.environment
        env["FASTWORD_MODEL"] = model
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
        proc.environment = env
        let stdin = Pipe(); let stdout = Pipe(); let stderr = Pipe()
        proc.standardInput = stdin
        proc.standardOutput = stdout
        proc.standardError = stderr

        stdout.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            self?.handleStdout(data)
        }
        stderr.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
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
            guard let self else { return }
            // Don't fire onCrash during an intentional stop()/restart().
            if self.stopping { return }
            let tail = String(data: self.stderrBuffer.suffix(2_048), encoding: .utf8) ?? ""
            let summary: String
            if p.terminationReason == .uncaughtSignal {
                summary = "sidecar crashed (signal \(p.terminationStatus))\n\(tail)"
            } else if p.terminationStatus != 0 {
                summary = "sidecar exited with status \(p.terminationStatus)\n\(tail)"
            } else {
                summary = "sidecar exited unexpectedly\n\(tail)"
            }
            DispatchQueue.main.async { [weak self] in self?.onCrash?(summary) }
        }

        do {
            try proc.run()
            self.process = proc
            self.stdinPipe = stdin
            self.stdoutPipe = stdout
            self.stderrPipe = stderr
        } catch {
            let msg = error.localizedDescription
            NSLog("FastWord: failed to start sidecar: %@", msg)
            DispatchQueue.main.async { [weak self] in self?.onCrash?("failed to start sidecar: \(msg)") }
        }
    }

    func stop() {
        stopping = true
        process?.terminate()
        process = nil
        stdinPipe = nil
        stdoutPipe = nil
        stderrPipe = nil
        stderrBuffer.removeAll(keepingCapacity: false)
        // Drop pending continuations so callers don't hang forever.
        pendingLock.lock()
        let stale = pending
        pending.removeAll()
        pendingLock.unlock()
        for cont in stale.values {
            cont.resume(throwing: SidecarError.notRunning)
        }
    }

    func restart() {
        stop()
        stopping = false
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

    func warmup() async throws {
        guard let stdin = stdinPipe else { throw SidecarError.notRunning }
        let id = UUID().uuidString
        let req: [String: Any] = ["id": id, "cmd": "warmup"]
        let line = try JSONSerialization.data(withJSONObject: req) + Data([0x0A])
        _ = try await withCheckedThrowingContinuation { (cont: CheckedContinuation<String, Error>) in
            pendingLock.lock()
            pending[id] = cont
            pendingLock.unlock()
            do {
                try stdin.fileHandleForWriting.write(contentsOf: line)
            } catch {
                pendingLock.lock()
                pending.removeValue(forKey: id)
                pendingLock.unlock()
                cont.resume(throwing: error)
            }
        }
    }

    func transcribe(pcm: Data) async throws -> String {
        guard let stdin = stdinPipe else { throw SidecarError.notRunning }
        let id = UUID().uuidString
        var req: [String: Any] = [
            "id": id,
            "cmd": "transcribe",
            "sample_rate": 16000,
            "audio_b64": pcm.base64EncodedString()
        ]
        // Empty string means "auto-detect"; sidecar treats it as a flag to drop
        // the language hint and let Whisper auto-pick.
        let langCode = AppSettings.transcriptionLanguageCode
        req["language"] = langCode
        // Engine routing: GigaAM-v3 is Russian-only but much more accurate
        // for Russian than Whisper. Only enable it when the user explicitly
        // opted in, the active language is Russian, *and* the model files
        // are already installed locally. Otherwise we silently fall back to
        // Whisper so the dictation never breaks because of a missing model.
        if AppSettings.useGigaAMForRussian, langCode == "ru",
           GigaAMInstaller.isInstalled {
            req["engine"] = "gigaam"
        }
        // When the user keeps the picker on "Auto", bias Whisper toward the
        // system language by passing a short hint phrase in that language as
        // the initial_prompt. This is the documented Superwhisper trick — it
        // dramatically reduces wrong-language detections on short clips.
        if langCode.isEmpty {
            let systemCode = TranscriptionLanguage.systemDefault().code
            if let hint = TranscriptionLanguage.promptHint(forCode: systemCode) {
                req["initial_prompt"] = hint
            }
        }
        // When Skip-empty is on, push whisper's own no-speech filter slightly
        // tighter than the default 0.6 so it drops more clearly-silent clips
        // before they hallucinate. 0.0 disables the filter entirely.
        req["no_speech_thold"] = AppSettings.skipEmpty ? 0.6 : 0.0
        let line = try JSONSerialization.data(withJSONObject: req) + Data([0x0A])

        return try await withCheckedThrowingContinuation { cont in
            pendingLock.lock()
            pending[id] = cont
            pendingLock.unlock()
            do {
                try stdin.fileHandleForWriting.write(contentsOf: line)
            } catch {
                pendingLock.lock()
                pending.removeValue(forKey: id)
                pendingLock.unlock()
                cont.resume(throwing: error)
            }
        }
    }

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
        self.pendingLock.lock()
        let cont = self.pending.removeValue(forKey: id)
        self.pendingLock.unlock()
        guard let cont else { return }
        if let err = obj["error"] as? String {
            cont.resume(throwing: SidecarError.remote(err))
        } else if let text = obj["text"] as? String {
            cont.resume(returning: text)
        } else {
            cont.resume(throwing: SidecarError.decodeFailed)
        }
    }
}
