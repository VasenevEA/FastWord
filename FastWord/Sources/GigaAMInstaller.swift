import Combine
import Foundation

/// Manages the on-demand install of the GigaAM-v3 CTC model into
/// `~/Library/Application Support/FastWord/Models/gigaam-v3-ctc/`.
///
/// Unlike Whisper's single-file ggml model, GigaAM ships as a directory with
/// `model.int8.onnx` (~215 MB) and `tokens.txt` (~200 bytes). We download
/// both, atomically moving each one to the final location when complete.
@MainActor
final class GigaAMInstaller: NSObject, ObservableObject {
    static let shared = GigaAMInstaller()

    enum State: Equatable {
        case notInstalled
        case downloading(progress: Double)
        case installed
        case failed(message: String)
    }

    @Published private(set) var state: State = .notInstalled

    /// Folder name inside Application Support / Models / where the engine
    /// expects to find its files. The Rust sidecar reads
    /// `FASTWORD_GIGAAM_MODEL=<this path>`.
    static let folderName = "gigaam-v3-ctc"

    /// Approximate total bytes the user is downloading. Used to drive a
    /// progress bar even before the server tells us the real number.
    private static let expectedBytes: Int64 = 215 * 1024 * 1024

    private struct File {
        let name: String
        let url: URL
    }

    private static let files: [File] = [
        File(
            name: "model.int8.onnx",
            url: URL(string: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-giga-am-v3-russian-2025-12-16/resolve/main/model.int8.onnx")!),
        File(
            name: "tokens.txt",
            url: URL(string: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-ctc-giga-am-v3-russian-2025-12-16/resolve/main/tokens.txt")!),
    ]

    // Pure-filesystem helpers, callable from anywhere — no shared state.
    nonisolated static var installDirectory: URL {
        let base = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("FastWord", isDirectory: true)
            .appendingPathComponent("Models", isDirectory: true)
            .appendingPathComponent(folderName, isDirectory: true)
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        return base
    }

    nonisolated static var isInstalled: Bool {
        let dir = installDirectory
        let fm = FileManager.default
        return files.allSatisfy { fm.fileExists(atPath: dir.appendingPathComponent($0.name).path) }
    }

    private var session: URLSession!
    private var tasks: [String: URLSessionDownloadTask] = [:]
    private var totalReceived: Int64 = 0
    private var didFail = false

    override init() {
        super.init()
        let cfg = URLSessionConfiguration.default
        cfg.timeoutIntervalForResource = 60 * 60
        self.session = URLSession(configuration: cfg, delegate: self, delegateQueue: nil)
        refresh()
    }

    func refresh() {
        if Self.isInstalled {
            state = .installed
        } else if state != .downloading(progress: 0) {
            // Don't clobber an in-flight download state.
            if case .downloading = state {} else {
                state = .notInstalled
            }
        }
    }

    func startDownload() {
        guard tasks.isEmpty else { return }
        totalReceived = 0
        didFail = false
        state = .downloading(progress: 0)
        for file in Self.files {
            let task = session.downloadTask(with: file.url)
            task.taskDescription = file.name
            tasks[file.name] = task
            task.resume()
        }
    }

    func cancel() {
        for task in tasks.values {
            task.cancel()
        }
        tasks.removeAll()
        totalReceived = 0
        state = Self.isInstalled ? .installed : .notInstalled
    }

    fileprivate func writeReceivedFile(name: String, fromTempURL temp: URL) {
        let dest = Self.installDirectory.appendingPathComponent(name)
        let fm = FileManager.default
        do {
            if fm.fileExists(atPath: dest.path) {
                try fm.removeItem(at: dest)
            }
            try fm.moveItem(at: temp, to: dest)
            tasks.removeValue(forKey: name)
            if tasks.isEmpty {
                state = Self.isInstalled ? .installed : .failed(message: "Some files missing after download")
            }
        } catch {
            didFail = true
            tasks.removeValue(forKey: name)
            state = .failed(message: error.localizedDescription)
        }
    }

    fileprivate func updateProgress(_ deltaBytes: Int64) {
        totalReceived += deltaBytes
        let progress = min(1.0, Double(totalReceived) / Double(Self.expectedBytes))
        state = .downloading(progress: progress)
    }

    fileprivate func recordFailure(_ error: Error) {
        guard !didFail else { return }
        didFail = true
        for task in tasks.values {
            task.cancel()
        }
        tasks.removeAll()
        state = .failed(message: error.localizedDescription)
    }
}

extension GigaAMInstaller: URLSessionDownloadDelegate {
    nonisolated func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didFinishDownloadingTo location: URL
    ) {
        // The temporary file is deleted as soon as this delegate returns —
        // move it *synchronously* before exiting.
        let name = downloadTask.taskDescription ?? ""
        let fm = FileManager.default
        let staged = fm.temporaryDirectory.appendingPathComponent(
            "fastword-gigaam-\(UUID().uuidString)-\(name)"
        )
        do {
            try fm.moveItem(at: location, to: staged)
            DispatchQueue.main.async {
                Task { @MainActor in
                    self.writeReceivedFile(name: name, fromTempURL: staged)
                }
            }
        } catch {
            DispatchQueue.main.async {
                Task { @MainActor in self.recordFailure(error) }
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
        DispatchQueue.main.async {
            Task { @MainActor in self.updateProgress(bytesWritten) }
        }
    }

    nonisolated func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        guard let error else { return }
        DispatchQueue.main.async {
            Task { @MainActor in self.recordFailure(error) }
        }
    }
}
