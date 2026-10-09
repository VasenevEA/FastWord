import Combine
import Foundation

/// Manages the on-demand install of the speaker detection models into
/// `~/Library/Application Support/FastWord/Models/diarization/`:
/// pyannote segmentation-3.0 (finds who speaks when) and 3D-Speaker CAM++
/// (tells voices apart), ~33 MB together. Only the Speakers option of
/// Transcribe File needs them, so they aren't bundled.
@MainActor
final class DiarizationInstaller: NSObject, ObservableObject {
    static let shared = DiarizationInstaller()

    enum State: Equatable {
        case notInstalled
        case downloading(progress: Double)
        case installed
        case failed(message: String)
    }

    enum InstallError: LocalizedError {
        case download(String)

        var errorDescription: String? {
            switch self {
            case .download(let message):
                let format = NSLocalizedString("Couldn't download the speaker detection models: %@", comment: "")
                return String(format: format, message)
            }
        }
    }

    @Published private(set) var state: State = .notInstalled

    /// The Rust sidecar reads `FASTWORD_DIARIZATION_MODEL=<this folder>`.
    nonisolated static let folderName = "diarization"

    private struct File {
        let name: String
        let url: URL
        let bytes: Int64
    }

    // Saved under fixed names, so the sidecar doesn't depend on how the
    // upstream repos name their files.
    private nonisolated static let files: [File] = [
        File(
            name: "segmentation.onnx",
            url: URL(string: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx")!,
            bytes: 5_992_913),
        File(
            name: "embedding.onnx",
            url: URL(string: "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/main/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx")!,
            bytes: 28_281_164),
    ]

    private static var expectedBytes: Int64 { files.reduce(0) { $0 + $1.bytes } }

    // Pure-filesystem helpers, callable from anywhere — no shared state.
    nonisolated static var installDirectory: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("FastWord", isDirectory: true)
            .appendingPathComponent("Models", isDirectory: true)
            .appendingPathComponent(folderName, isDirectory: true)
    }

    nonisolated static var isInstalled: Bool {
        let dir = installDirectory
        let fm = FileManager.default
        return files.allSatisfy { fm.fileExists(atPath: dir.appendingPathComponent($0.name).path) }
    }

    private var session: URLSession!
    private var tasks: [String: URLSessionDownloadTask] = [:]
    private var received: [String: Int64] = [:]
    /// Jobs waiting in `install()` for the download to end.
    private var waiters: [UUID: CheckedContinuation<Void, Error>] = [:]

    override init() {
        super.init()
        let cfg = URLSessionConfiguration.default
        cfg.timeoutIntervalForResource = 60 * 60
        session = URLSession(configuration: cfg, delegate: self, delegateQueue: nil)
        refresh()
    }

    var isDownloading: Bool { !tasks.isEmpty }

    func refresh() {
        guard !isDownloading else { return }
        state = Self.isInstalled ? .installed : .notInstalled
    }

    func startDownload() {
        guard tasks.isEmpty else { return }
        removeStagedFiles()
        received = [:]
        state = .downloading(progress: 0)
        for file in Self.files {
            let task = session.downloadTask(with: file.url)
            task.taskDescription = file.name
            tasks[file.name] = task
            task.resume()
        }
    }

    /// Starts the download unless it's running, and returns once both files
    /// are in place. Throws the download's error — or CancellationError when
    /// the waiting task is cancelled; the download itself carries on, since
    /// the Speakers option will need it anyway.
    func install() async throws {
        if Self.isInstalled {
            refresh()
            return
        }
        startDownload()
        let id = UUID()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
                if Task.isCancelled {
                    cont.resume(throwing: CancellationError())
                } else {
                    waiters[id] = cont
                }
            }
        } onCancel: {
            Task { @MainActor in
                self.waiters.removeValue(forKey: id)?.resume(throwing: CancellationError())
            }
        }
    }

    /// Remove the installed model files from disk and reset state.
    func uninstall() {
        cancel()
        let dir = Self.installDirectory
        for file in Self.files {
            try? FileManager.default.removeItem(at: dir.appendingPathComponent(file.name))
        }
        state = .notInstalled
    }

    func cancel() {
        guard isDownloading else { return }
        // Forgotten first: their cancellation errors must not read as a
        // failed download.
        let running = tasks.values
        tasks.removeAll()
        for task in running {
            task.cancel()
        }
        state = Self.isInstalled ? .installed : .notInstalled
        finishWaiters(InstallError.download(NSLocalizedString("Cancelled", comment: "")))
    }

    fileprivate func writeReceivedFile(name: String, fromTempURL temp: URL, task: URLSessionTask) {
        defer { try? FileManager.default.removeItem(at: temp) }
        guard tasks[name] === task, let file = Self.files.first(where: { $0.name == name }) else { return }
        // An error page or a cut-off file would only fail much later, in the
        // middle of someone's hour-long job.
        if let http = task.response as? HTTPURLResponse, !(200...299).contains(http.statusCode) {
            fail("HTTP \(http.statusCode)")
            return
        }
        let size = (try? FileManager.default.attributesOfItem(atPath: temp.path)[.size] as? Int64) ?? 0
        guard abs(size - file.bytes) <= file.bytes / 5 else {
            fail(String(format: NSLocalizedString("%@ has an unexpected size", comment: ""), name))
            return
        }
        let fm = FileManager.default
        let dest = Self.installDirectory.appendingPathComponent(name)
        do {
            try fm.createDirectory(at: Self.installDirectory, withIntermediateDirectories: true)
            // Staged on the same volume, so this is a rename: the sidecar
            // never sees a half-written model.
            if fm.fileExists(atPath: dest.path) {
                _ = try fm.replaceItemAt(dest, withItemAt: temp)
            } else {
                try fm.moveItem(at: temp, to: dest)
            }
        } catch {
            fail(error.localizedDescription)
            return
        }
        tasks.removeValue(forKey: name)
        guard tasks.isEmpty else { return }
        if Self.isInstalled {
            state = .installed
            finishWaiters(nil)
        } else {
            fail(NSLocalizedString("Some files are missing after the download", comment: ""))
        }
    }

    fileprivate func updateProgress(name: String, totalBytesWritten: Int64, task: URLSessionTask) {
        guard tasks[name] === task else { return }
        received[name] = totalBytesWritten
        let total = received.values.reduce(0, +)
        state = .downloading(progress: min(1, Double(total) / Double(Self.expectedBytes)))
    }

    fileprivate func recordFailure(_ error: Error, task: URLSessionTask) {
        // A task we already let go of (cancelled, or failed together with
        // its sibling) has nothing more to say.
        guard let name = task.taskDescription, tasks[name] === task else { return }
        fail(error.localizedDescription)
    }

    private func fail(_ message: String) {
        let running = tasks.values
        tasks.removeAll()
        for task in running {
            task.cancel()
        }
        state = .failed(message: message)
        finishWaiters(InstallError.download(message))
    }

    /// Downloads are staged next to the install folder; one cut short by a
    /// quit stays there until the next download.
    private nonisolated static let stagingPrefix = ".fastword-diarization-"

    private func removeStagedFiles() {
        let fm = FileManager.default
        let staging = Self.installDirectory.deletingLastPathComponent()
        for name in (try? fm.contentsOfDirectory(atPath: staging.path)) ?? [] where name.hasPrefix(Self.stagingPrefix) {
            try? fm.removeItem(at: staging.appendingPathComponent(name))
        }
    }

    private func finishWaiters(_ error: Error?) {
        let waiting = waiters.values
        waiters.removeAll()
        for cont in waiting {
            if let error {
                cont.resume(throwing: error)
            } else {
                cont.resume()
            }
        }
    }
}

extension DiarizationInstaller: URLSessionDownloadDelegate {
    nonisolated func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didFinishDownloadingTo location: URL
    ) {
        // The temporary file is deleted as soon as this delegate returns —
        // move it *synchronously* before exiting. Staged next to the final
        // folder rather than in /tmp, so the last move stays on one volume.
        let name = downloadTask.taskDescription ?? ""
        let fm = FileManager.default
        let staging = Self.installDirectory.deletingLastPathComponent()
        let staged = staging.appendingPathComponent("\(Self.stagingPrefix)\(UUID().uuidString)-\(name)")
        do {
            try fm.createDirectory(at: staging, withIntermediateDirectories: true)
            try fm.moveItem(at: location, to: staged)
            DispatchQueue.main.async {
                Task { @MainActor in
                    self.writeReceivedFile(name: name, fromTempURL: staged, task: downloadTask)
                }
            }
        } catch {
            DispatchQueue.main.async {
                Task { @MainActor in self.recordFailure(error, task: downloadTask) }
            }
        }
    }

    nonisolated func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didWriteData bytesWritten: Int64,
        totalBytesWritten: Int64,
        totalBytesExpectedToWrite: Int64
    ) {
        let name = downloadTask.taskDescription ?? ""
        DispatchQueue.main.async {
            Task { @MainActor in
                self.updateProgress(name: name, totalBytesWritten: totalBytesWritten, task: downloadTask)
            }
        }
    }

    nonisolated func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        guard let error else { return }
        DispatchQueue.main.async {
            Task { @MainActor in self.recordFailure(error, task: task) }
        }
    }
}
