import Foundation

/// One-shot first-launch migrations between major versions.
enum Migrations {
    private static let migratedToRustKey = "migratedToRustSidecar"

    /// Removes leftover artefacts from the v0.1.x Python+MLX sidecar that
    /// v0.2+ no longer uses. Idempotent — runs once per machine, controlled
    /// by a UserDefaults flag.
    ///
    /// Items removed (only if present):
    /// - ~/.fastword/venv/         (Python virtualenv from bootstrap.sh, 600-700 MB)
    /// - ~/.fastword/sidecar/      (old sidecar.py copy)
    /// - ~/.fastword/sidecar.log   (old sidecar log)
    ///
    /// Explicitly preserved:
    /// - ~/.fastword/history.sqlite  (user's transcription history)
    /// - ~/.cache/huggingface/       (may be shared with other AI apps)
    static func runIfNeeded() {
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: migratedToRustKey) else { return }

        let fm = FileManager.default
        let home = fm.homeDirectoryForCurrentUser

        let candidates: [URL] = [
            home.appendingPathComponent(".fastword/venv"),
            home.appendingPathComponent(".fastword/sidecar"),
            home.appendingPathComponent(".fastword/sidecar.log"),
        ]

        var freedBytes: Int64 = 0
        for url in candidates {
            guard fm.fileExists(atPath: url.path) else { continue }
            freedBytes += sizeOfItem(at: url, fm: fm)
            do {
                try fm.removeItem(at: url)
                NSLog("FastWord: migration removed %@", url.path)
            } catch {
                NSLog("FastWord: migration could not remove %@: %@",
                      url.path, error.localizedDescription)
            }
        }

        if freedBytes > 0 {
            let mb = Double(freedBytes) / (1024 * 1024)
            NSLog("FastWord: migration freed %.0f MB", mb)
        }

        defaults.set(true, forKey: migratedToRustKey)
    }

    private static func sizeOfItem(at url: URL, fm: FileManager) -> Int64 {
        guard let enumerator = fm.enumerator(
            at: url,
            includingPropertiesForKeys: [.isRegularFileKey, .fileSizeKey],
            options: [.skipsHiddenFiles, .skipsPackageDescendants]
        ) else {
            // Fall back to a single attribute lookup (e.g. for a flat file).
            let attrs = try? fm.attributesOfItem(atPath: url.path)
            return (attrs?[.size] as? Int64) ?? 0
        }

        var total: Int64 = 0
        for case let fileURL as URL in enumerator {
            if let values = try? fileURL.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey]),
               values.isRegularFile == true,
               let size = values.fileSize {
                total += Int64(size)
            }
        }
        return total
    }
}
