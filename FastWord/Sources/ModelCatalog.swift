import Foundation

/// A whisper.cpp ggml model that FastWord knows about. We ship one bundled
/// inside the .app; the rest can be downloaded on demand into the user's
/// Application Support directory.
struct WhisperModel: Identifiable, Hashable {
    /// The on-disk filename, used as both the unique id and the URL leaf.
    let filename: String
    /// Short human-friendly name shown in the picker.
    let displayName: String
    /// Approximate size in bytes (used for the download progress estimate
    /// and the list UI).
    let sizeBytes: Int64
    /// One-line description for the row.
    let descriptionKey: String

    var id: String { filename }

    /// Where this model is fetched from when the user chooses to download it.
    var downloadURL: URL {
        URL(string: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/\(filename)")!
    }

    /// Human-friendly size string ("547 MB", "1.5 GB").
    var sizeString: String {
        let formatter = ByteCountFormatter()
        formatter.allowedUnits = [.useMB, .useGB]
        formatter.countStyle = .file
        return formatter.string(fromByteCount: sizeBytes)
    }
}

enum ModelCatalog {
    /// Filename of the model shipped inside .app/Contents/Resources/models/.
    /// The catalog is also a source of truth for "is this the bundled model".
    static let bundledFilename = "ggml-large-v3-turbo-q5_0.bin"

    /// Sentinel `activeModel` value meaning "use the GigaAM engine".
    /// GigaAM isn't a whisper.cpp model so it doesn't have a .bin filename —
    /// we just need a stable identifier for the user's selection.
    static let gigaAMSentinel = "gigaam-v3-ctc"

    /// Multilingual ggml models suitable for FastWord's three-language audience.
    /// English-only `.en` variants are deliberately excluded — they would
    /// confuse users who dictate in Russian or Chinese.
    static let all: [WhisperModel] = [
        WhisperModel(
            filename: "ggml-tiny.bin",
            displayName: "Whisper Tiny",
            sizeBytes: 77_700_000,
            descriptionKey: "model.tiny.desc"),
        WhisperModel(
            filename: "ggml-base.bin",
            displayName: "Whisper Base",
            sizeBytes: 147_900_000,
            descriptionKey: "model.base.desc"),
        WhisperModel(
            filename: "ggml-small.bin",
            displayName: "Whisper Small",
            sizeBytes: 487_600_000,
            descriptionKey: "model.small.desc"),
        WhisperModel(
            filename: "ggml-medium.bin",
            displayName: "Whisper Medium",
            sizeBytes: 1_530_000_000,
            descriptionKey: "model.medium.desc"),
        WhisperModel(
            filename: "ggml-large-v3-turbo-q5_0.bin",
            displayName: "Whisper Large v3 Turbo Q5",
            sizeBytes: 547_000_000,
            descriptionKey: "model.turbo_q5.desc"),
        WhisperModel(
            filename: "ggml-large-v3-turbo-q8_0.bin",
            displayName: "Whisper Large v3 Turbo Q8",
            sizeBytes: 874_000_000,
            descriptionKey: "model.turbo_q8.desc"),
        WhisperModel(
            filename: "ggml-large-v3-turbo.bin",
            displayName: "Whisper Large v3 Turbo (full)",
            sizeBytes: 1_620_000_000,
            descriptionKey: "model.turbo_full.desc"),
        WhisperModel(
            filename: "ggml-large-v3-q5_0.bin",
            displayName: "Whisper Large v3 Q5",
            sizeBytes: 1_080_000_000,
            descriptionKey: "model.large_q5.desc"),
        WhisperModel(
            filename: "ggml-large-v3.bin",
            displayName: "Whisper Large v3 (full)",
            sizeBytes: 3_100_000_000,
            descriptionKey: "model.large_full.desc"),
    ]

    static func model(for filename: String) -> WhisperModel? {
        all.first { $0.filename == filename }
    }
}

/// On-disk locations for downloaded models, and helpers for finding
/// the active one.
enum ModelStorage {
    /// `~/Library/Application Support/FastWord/Models/`
    static var applicationSupportDirectory: URL {
        let fm = FileManager.default
        let base = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base
            .appendingPathComponent("FastWord", isDirectory: true)
            .appendingPathComponent("Models", isDirectory: true)
        try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Where the bundled model lives inside the app, if present.
    /// In a release DMG this is .app/Contents/Resources/models/.
    /// In a debug build the app bundle has no models — fall back to the
    /// legacy ~/Library/Caches/fastword/models/ path that `scripts/` and
    /// older builds populated, so the dev experience matches release.
    static var bundledModelPath: String? {
        let fm = FileManager.default
        if let resources = Bundle.main.resourcePath {
            let inAppPath = "\(resources)/models/\(ModelCatalog.bundledFilename)"
            if fm.fileExists(atPath: inAppPath) { return inAppPath }
        }
        let home = fm.homeDirectoryForCurrentUser.path
        let cached = "\(home)/Library/Caches/fastword/models/\(ModelCatalog.bundledFilename)"
        if fm.fileExists(atPath: cached) { return cached }
        return nil
    }

    /// Returns the absolute path of a model if it's been downloaded to
    /// Application Support.
    static func downloadedPath(for model: WhisperModel) -> String? {
        let url = applicationSupportDirectory.appendingPathComponent(model.filename)
        return FileManager.default.fileExists(atPath: url.path) ? url.path : nil
    }

    /// Status from the user's perspective.
    enum Status: Equatable {
        /// Shipped inside the .app — always available, can't be deleted.
        case bundled
        /// Downloaded to ~/Library/Application Support.
        case downloaded
        /// Listed but not yet fetched.
        case notDownloaded
    }

    static func status(of model: WhisperModel) -> Status {
        if model.filename == ModelCatalog.bundledFilename, bundledModelPath != nil {
            return .bundled
        }
        if downloadedPath(for: model) != nil {
            return .downloaded
        }
        return .notDownloaded
    }

    /// Returns the absolute filesystem path that should be fed to the
    /// sidecar via FASTWORD_MODEL — looking at the user's active selection,
    /// falling back to the bundled model if anything goes wrong.
    static func activeModelPath() -> String? {
        let requested = AppSettings.activeModelFilename
        if !requested.isEmpty, let model = ModelCatalog.model(for: requested) {
            if let downloaded = downloadedPath(for: model) {
                return downloaded
            }
            if model.filename == ModelCatalog.bundledFilename, let bundled = bundledModelPath {
                return bundled
            }
        }
        // No active selection, or the selected one isn't present — fall back.
        return bundledModelPath
    }

    /// Deletes a downloaded model. No-op for bundled.
    @discardableResult
    static func delete(_ model: WhisperModel) -> Bool {
        guard let path = downloadedPath(for: model) else { return false }
        do {
            try FileManager.default.removeItem(atPath: path)
            return true
        } catch {
            NSLog("FastWord: failed to delete model %@: %@", model.filename, error.localizedDescription)
            return false
        }
    }
}
