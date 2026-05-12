import Foundation
import Combine

/// Tracks per-model download state, observable from SwiftUI.
@MainActor
final class ModelDownloader: NSObject, ObservableObject {
    /// Singleton because we want a single set of in-flight tasks and a single
    /// publisher of progress across the Settings UI.
    static let shared = ModelDownloader()

    /// State of a single download keyed by model filename.
    struct DownloadState: Equatable {
        var progress: Double  // 0…1
        var bytesReceived: Int64
        var totalBytes: Int64
        var error: String?
    }

    @Published private(set) var downloads: [String: DownloadState] = [:]

    private var tasks: [String: URLSessionDownloadTask] = [:]
    private lazy var session: URLSession = {
        let cfg = URLSessionConfiguration.default
        cfg.allowsCellularAccess = true
        cfg.timeoutIntervalForResource = 60 * 60   // 1 h — large models on slow links
        return URLSession(configuration: cfg, delegate: self, delegateQueue: nil)
    }()

    private override init() { super.init() }

    func isDownloading(_ filename: String) -> Bool {
        downloads[filename] != nil
    }

    func progress(for filename: String) -> Double {
        downloads[filename]?.progress ?? 0
    }

    func start(_ model: WhisperModel) {
        guard tasks[model.filename] == nil else { return }
        downloads[model.filename] = DownloadState(
            progress: 0, bytesReceived: 0, totalBytes: model.sizeBytes, error: nil
        )
        let task = session.downloadTask(with: model.downloadURL)
        // Stash the filename on the task description so the delegate can
        // route progress / completion events back to the right entry.
        task.taskDescription = model.filename
        tasks[model.filename] = task
        task.resume()
    }

    func cancel(_ filename: String) {
        tasks[filename]?.cancel()
        tasks.removeValue(forKey: filename)
        downloads.removeValue(forKey: filename)
    }

    fileprivate func finishedDownload(taskFilename: String, downloadedURL: URL) {
        // Move to its final home atomically.
        let dest = ModelStorage.applicationSupportDirectory.appendingPathComponent(taskFilename)
        let fm = FileManager.default
        do {
            if fm.fileExists(atPath: dest.path) {
                try fm.removeItem(at: dest)
            }
            try fm.moveItem(at: downloadedURL, to: dest)
            tasks.removeValue(forKey: taskFilename)
            downloads.removeValue(forKey: taskFilename)
        } catch {
            downloads[taskFilename]?.error = error.localizedDescription
            tasks.removeValue(forKey: taskFilename)
            NSLog("FastWord: model move failed for %@: %@",
                  taskFilename, error.localizedDescription)
        }
    }

    fileprivate func updateProgress(taskFilename: String, received: Int64, total: Int64) {
        guard var state = downloads[taskFilename] else { return }
        state.bytesReceived = received
        if total > 0 {
            state.totalBytes = total
        }
        let denom = state.totalBytes > 0 ? Double(state.totalBytes) : Double(received)
        state.progress = denom > 0 ? Double(received) / denom : 0
        downloads[taskFilename] = state
    }

    fileprivate func failedTask(taskFilename: String, error: Error?) {
        downloads[taskFilename]?.error = error?.localizedDescription ?? "unknown error"
        tasks.removeValue(forKey: taskFilename)
    }
}

extension ModelDownloader: URLSessionDownloadDelegate {
    nonisolated func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didFinishDownloadingTo location: URL
    ) {
        let name = downloadTask.taskDescription ?? ""
        // We must move the file synchronously before this delegate returns —
        // URLSession deletes the temp file when this method exits. Do the
        // copy here on the background URLSession queue, then update the main-
        // actor model on a dispatch.
        let dest = ModelStorage.applicationSupportDirectory.appendingPathComponent(name)
        let fm = FileManager.default
        do {
            if fm.fileExists(atPath: dest.path) {
                try fm.removeItem(at: dest)
            }
            try fm.moveItem(at: location, to: dest)
            DispatchQueue.main.async {
                Task { @MainActor in
                    // We've already moved the file; clear the entry directly.
                    self.tasks.removeValue(forKey: name)
                    self.downloads.removeValue(forKey: name)
                }
            }
        } catch {
            DispatchQueue.main.async {
                Task { @MainActor in
                    self.failedTask(taskFilename: name, error: error)
                }
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
                self.updateProgress(
                    taskFilename: name,
                    received: totalBytesWritten,
                    total: totalBytesExpectedToWrite
                )
            }
        }
    }

    nonisolated func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        guard let error else { return }   // success path handled above
        let name = task.taskDescription ?? ""
        DispatchQueue.main.async {
            Task { @MainActor in
                self.failedTask(taskFilename: name, error: error)
            }
        }
    }
}
