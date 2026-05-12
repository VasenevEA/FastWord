import SwiftUI

struct ModelManagementView: View {
    @StateObject private var downloader = ModelDownloader.shared
    @AppStorage(SettingsKey.activeModel) private var activeFilename: String = ""

    /// The version of ModelStorage status that depends on filesystem state,
    /// not just AppStorage. Bump to force the list to re-evaluate.
    @State private var refreshTick = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(ModelCatalog.all) { model in
                ModelRow(
                    model: model,
                    status: ModelStorage.status(of: model),
                    isActive: isActive(model),
                    downloadState: downloader.downloads[model.filename],
                    onUse: { setActive(model) },
                    onDownload: { downloader.start(model) },
                    onCancel: { downloader.cancel(model.filename) },
                    onDelete: { delete(model) }
                )
                if model.id != ModelCatalog.all.last?.id {
                    Divider()
                }
            }
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
        let effective = activeFilename.isEmpty ? ModelCatalog.bundledFilename : activeFilename
        return model.filename == effective
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
