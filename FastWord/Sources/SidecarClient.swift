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
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let venvPython = "\(home)/.fastword/venv/bin/python3"
        let scriptPath = "\(home)/.fastword/sidecar/sidecar.py"

        guard FileManager.default.fileExists(atPath: venvPython),
              FileManager.default.fileExists(atPath: scriptPath) else {
            NSLog("FastWord: sidecar not installed. Run scripts/bootstrap.sh")
            return
        }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: venvPython)
        proc.arguments = [scriptPath]
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
        let req: [String: Any] = [
            "id": id,
            "cmd": "transcribe",
            "sample_rate": 16000,
            "audio_b64": pcm.base64EncodedString()
        ]
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
