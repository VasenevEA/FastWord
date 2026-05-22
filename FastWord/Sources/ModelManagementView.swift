import SwiftUI

struct ModelManagementView: View {
    @StateObject private var downloader = ModelDownloader.shared
    @StateObject private var gigaAM = GigaAMInstaller.shared
    @AppStorage(SettingsKey.activeModel) private var activeFilename: String = ""

    /// The version of ModelStorage status that depends on filesystem state,
    /// not just AppStorage. Bump to force the list to re-evaluate.
    @State private var refreshTick = 0

    /// User has explicitly picked GigaAM as the active engine.
    private var gigaAMActive: Bool {
        activeFilename == ModelCatalog.gigaAMSentinel
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(ModelCatalog.all) { model in
                ModelRow(
                    model: model,
                    status: ModelStorage.status(of: model),
                    isActive: !gigaAMActive && isActive(model),
                    downloadState: downloader.downloads[model.filename],
                    onUse: { setActive(model) },
                    onDownload: { downloader.start(model) },
                    onCancel: { downloader.cancel(model.filename) },
                    onDelete: { delete(model) }
                )
                Divider()
            }
            GigaAMRow(
                installer: gigaAM,
                isActive: gigaAMActive,
                onUse: { AppSettings.activeModelFilename = ModelCatalog.gigaAMSentinel }
            )
        }
        .padding(.vertical, 4)
        .id(refreshTick)
        .onReceive(downloader.$downloads) { _ in
            // When a download finishes, the entry is removed from `downloads` —
            // bump the refresh tick so each row re-checks its on-disk status.
            refreshTick &+= 1
        }
    }

    private func isActive(_ model: WhisperModel) -> Bool {
        // When activeFilename is empty or points at the GigaAM sentinel, the
        // "default" Whisper model is the bundled one.
        if activeFilename.isEmpty || activeFilename == ModelCatalog.gigaAMSentinel {
            return model.filename == ModelCatalog.bundledFilename
        }
        return model.filename == activeFilename
    }

    private func setActive(_ model: WhisperModel) {
        AppSettings.activeModelFilename = model.filename
    }

    private func delete(_ model: WhisperModel) {
        // Don't allow deleting the active model — switch to bundled first.
        if isActive(model) {
            AppSettings.activeModelFilename = ModelCatalog.bundledFilename
        }
        ModelStorage.delete(model)
        refreshTick &+= 1
    }
}

/// Row for the GigaAM-v3 Russian model — mirrors ModelRow visually but
/// drives the separate GigaAMInstaller. When installed, it's automatically
/// used for Russian transcription (no extra toggle).
private struct GigaAMRow: View {
    @ObservedObject var installer: GigaAMInstaller
    let isActive: Bool
    let onUse: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text("GigaAM v3 (Russian)")
                        .font(.system(size: 13, weight: .medium))
                    if case .installed = installer.state {
                        if isActive {
                            badge(LocalizedStringKey("Active"), tint: .accentColor)
                        } else {
                            badge(LocalizedStringKey("Installed"), tint: .green)
                        }
                    }
                }
                Text(LocalizedStringKey("215 MB · Sber's MIT-licensed model. ~50% lower WER than Whisper-large-v3 on Russian. Russian only."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if case .downloading(let progress) = installer.state {
                    ProgressView(value: progress)
                        .progressViewStyle(.linear)
                        .frame(maxWidth: 220)
                }
                if case .failed(let msg) = installer.state {
                    Text(msg)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
            Spacer(minLength: 0)
            actions
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private func badge(_ key: LocalizedStringKey, tint: Color) -> some View {
        Text(key)
            .font(.system(size: 10, weight: .semibold))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(tint.opacity(0.2))
            .foregroundStyle(tint)
            .clipShape(Capsule())
    }

    @ViewBuilder
    private var actions: some View {
        switch installer.state {
        case .notInstalled:
            Button(LocalizedStringKey("Download")) { installer.startDownload() }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
        case .downloading:
            Button(LocalizedStringKey("Cancel")) { installer.cancel() }
                .buttonStyle(.borderless)
        case .installed:
            HStack(spacing: 6) {
                if !isActive {
                    Button(LocalizedStringKey("Use"), action: onUse)
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                }
                Button(action: { installer.uninstall() }) {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help(LocalizedStringKey("Delete"))
            }
        case .failed:
            Button(LocalizedStringKey("Retry")) { installer.startDownload() }
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
    }
}

private struct ModelRow: View {
    let model: WhisperModel
    let status: ModelStorage.Status
    let isActive: Bool
    let downloadState: ModelDownloader.DownloadState?
    let onUse: () -> Void
    let onDownload: () -> Void
    let onCancel: () -> Void
    let onDelete: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(model.displayName)
                        .font(.system(size: 13, weight: .medium))
                    if isActive {
                        badge(LocalizedStringKey("Active"), tint: .accentColor)
                    } else if status == .bundled || status == .downloaded {
                        badge(LocalizedStringKey("Installed"), tint: .green)
                    }
                }
                Text("\(model.sizeString) · \(NSLocalizedString(model.descriptionKey, comment: ""))")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let state = downloadState {
                    if let error = state.error {
                        Text(error)
                            .font(.caption)
                            .foregroundStyle(.red)
                    } else {
                        ProgressView(value: state.progress)
                            .progressViewStyle(.linear)
                            .frame(maxWidth: 220)
                    }
                }
            }
            Spacer(minLength: 0)
            actions
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private func badge(_ key: LocalizedStringKey, tint: Color) -> some View {
        Text(key)
            .font(.system(size: 10, weight: .semibold))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(tint.opacity(0.2))
            .foregroundStyle(tint)
            .clipShape(Capsule())
    }

    @ViewBuilder
    private var actions: some View {
        HStack(spacing: 6) {
            if let _ = downloadState {
                Button(LocalizedStringKey("Cancel"), action: onCancel)
                    .buttonStyle(.borderless)
            } else {
                switch status {
                case .notDownloaded:
                    Button(LocalizedStringKey("Download"), action: onDownload)
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                case .downloaded, .bundled:
                    if !isActive {
                        Button(LocalizedStringKey("Use"), action: onUse)
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                    }
                    if status == .downloaded {
                        Button(action: onDelete) {
                            Image(systemName: "trash")
                        }
                        .buttonStyle(.borderless)
                        .help(LocalizedStringKey("Delete"))
                    }
                }
            }
        }
    }
}
