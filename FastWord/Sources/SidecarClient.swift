import Foundation

enum SidecarError: Error {
    case notRunning
    case decodeFailed
    case remote(String)
}

final class SidecarClient {
    private var process: Process?
    private var stdinPipe: Pipe?
    private var stdoutPipe: Pipe?
    private var stderrPipe: Pipe?
    private let queue = DispatchQueue(label: "fastword.sidecar")
    private var pending: [String: CheckedContinuation<String, Error>] = [:]
    private var readBuffer = Data()
    private let pendingLock = NSLock()

    func start() {
        let fm = FileManager.default
        var venvPython: String?
        var scriptPath: String?

        // 1. Prefer bundled venv (production: shipped inside the .app).
        if let resources = Bundle.main.resourcePath {
            let bundledPython = "\(resources)/python/bin/python3"
            let bundledScript = "\(resources)/python/sidecar.py"
            if fm.fileExists(atPath: bundledPython), fm.fileExists(atPath: bundledScript) {
                venvPython = bundledPython
                scriptPath = bundledScript
            }
        }

        // 2. Fall back to ~/.fastword/venv (development: bootstrap.sh).
        if venvPython == nil {
            let home = fm.homeDirectoryForCurrentUser.path
            let homePython = "\(home)/.fastword/venv/bin/python3"
            let homeScript = "\(home)/.fastword/sidecar/sidecar.py"
            if fm.fileExists(atPath: homePython), fm.fileExists(atPath: homeScript) {
                venvPython = homePython
                scriptPath = homeScript
            }
        }

        guard let pythonExe = venvPython, let script = scriptPath else {
            NSLog("FastWord: sidecar not installed (no bundled venv, no ~/.fastword/venv). Run scripts/bootstrap.sh")
            return
        }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: pythonExe)
        proc.arguments = [script]

        // If a bundled model is present, point the sidecar at it (offline-first).
        if let resources = Bundle.main.resourcePath {
            let modelsDir = URL(fileURLWithPath: resources).appendingPathComponent("models")
            if let entries = try? fm.contentsOfDirectory(atPath: modelsDir.path),
               let first = entries.first(where: { !$0.hasPrefix(".") }) {
                let fullPath = modelsDir.appendingPathComponent(first).path
                var env = ProcessInfo.processInfo.environment
                env["FASTWORD_MODEL"] = fullPath
                proc.environment = env
            }
        }
        let stdin = Pipe(); let stdout = Pipe(); let stderr = Pipe()
        proc.standardInput = stdin
        proc.standardOutput = stdout
        proc.standardError = stderr

        stdout.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            self?.handleStdout(data)
        }
        stderr.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            if !data.isEmpty, let s = String(data: data, encoding: .utf8) {
                NSLog("FastWord sidecar: %@", s)
            }
        }

        do {
            try proc.run()
            self.process = proc
            self.stdinPipe = stdin
            self.stdoutPipe = stdout
            self.stderrPipe = stderr
        } catch {
            NSLog("FastWord: failed to start sidecar: %@", error.localizedDescription)
        }
    }

    func stop() {
        process?.terminate()
        process = nil
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
        req["language"] = AppSettings.transcriptionLanguageCode
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
