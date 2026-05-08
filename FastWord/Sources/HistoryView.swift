import SwiftUI

struct HistoryView: View {
    @EnvironmentObject var controller: AppController
    @State private var search = ""
    @State private var selection: HistoryEntry.ID?
    @State private var pendingDelete: HistoryEntry?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField(LocalizedStringKey("Search transcriptions"), text: $search)
                    .textFieldStyle(.plain)
            }
            .padding(10)
            .background(.thinMaterial)

            Divider()

            if filtered.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "mic.slash")
                        .font(.system(size: 36))
                        .foregroundStyle(.secondary)
                    Text(LocalizedStringKey("No transcriptions yet"))
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(filtered, selection: $selection) { entry in
                    EntryRow(
                        entry: entry,
                        onCopy: { controller.copyEntryToClipboard(entry.text) },
                        onDelete: { pendingDelete = entry }
                    )
                    .tag(entry.id)
                    .contextMenu {
                        Button(LocalizedStringKey("Copy")) {
                            controller.copyEntryToClipboard(entry.text)
                        }
                        Button(LocalizedStringKey("Delete"), role: .destructive) {
                            pendingDelete = entry
                        }
                    }
                }
                .listStyle(.inset)
            }
        }
        .frame(minWidth: 700, minHeight: 500)
        .alert(
            LocalizedStringKey("Delete this transcription?"),
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }
            ),
            presenting: pendingDelete
        ) { entry in
            Button(LocalizedStringKey("Delete"), role: .destructive) {
                controller.deleteHistoryEntry(entry.id)
                pendingDelete = nil
            }
            Button(LocalizedStringKey("Cancel"), role: .cancel) {
                pendingDelete = nil
            }
        } message: { entry in
            Text(entry.text).lineLimit(3)
        }
    }

    private var filtered: [HistoryEntry] {
        guard !search.isEmpty else { return controller.history }
        return controller.history.filter { $0.text.localizedCaseInsensitiveContains(search) }
    }
}

struct EntryRow: View {
    let entry: HistoryEntry
    let onCopy: () -> Void
    let onDelete: () -> Void

    @State private var isHovering = false
    @State private var justCopied = false

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.text)
                    .lineLimit(3)
                    .textSelection(.enabled)
                Text(entry.createdAt.formatted(date: .abbreviated, time: .shortened))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            actions
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .onHover { hovering in
            isHovering = hovering
        }
    }

    @ViewBuilder
    private var actions: some View {
        HStack(spacing: 4) {
            Button(action: copy) {
                Image(systemName: justCopied ? "checkmark" : "doc.on.doc")
                    .font(.system(size: 13, weight: .medium))
                    .frame(width: 26, height: 26)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .help(LocalizedStringKey("Copy"))
            .foregroundStyle(justCopied ? .green : .secondary)

            Button(action: onDelete) {
                Image(systemName: "trash")
                    .font(.system(size: 13, weight: .medium))
                    .frame(width: 26, height: 26)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .help(LocalizedStringKey("Delete"))
            .foregroundStyle(.secondary)
        }
        .opacity(isHovering ? 1 : 0)
        .animation(.easeInOut(duration: 0.12), value: isHovering)
    }

    private func copy() {
        onCopy()
        justCopied = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
            justCopied = false
        }
    }
}
