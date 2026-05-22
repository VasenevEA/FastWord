import SwiftUI

struct HistoryView: View {
    @EnvironmentObject var controller: AppController
    @State private var search = ""
    @State private var selection: HistoryEntry.ID?
    @State private var pendingDelete: HistoryEntry?
    @State private var confirmingClearAll = false

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField(LocalizedStringKey("Search transcriptions"), text: $search)
                    .textFieldStyle(.plain)
                Spacer(minLength: 8)
                if !controller.history.isEmpty {
                    Button(role: .destructive) {
                        confirmingClearAll = true
                    } label: {
                        Label(LocalizedStringKey("Clear all"), systemImage: "trash")
                    }
                    .controlSize(.small)
                    .help(LocalizedStringKey("Delete every transcription in the history"))
                }
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
        .alert(
            LocalizedStringKey("Delete all transcriptions?"),
            isPresented: $confirmingClearAll
        ) {
            Button(LocalizedStringKey("Delete All"), role: .destructive) {
                controller.clearAllHistory()
                confirmingClearAll = false
            }
            Button(LocalizedStringKey("Cancel"), role: .cancel) {
                confirmingClearAll = false
            }
        } message: {
            Text(LocalizedStringKey("This permanently removes every transcription stored locally. It can't be undone."))
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
                Text(justCopied
                     ? NSLocalizedString("Copied", comment: "")
                     : entry.text)
                    .lineLimit(3)
                    // textSelection is incompatible with .onTapGesture on the
                    // whole row — drag-to-select would swallow the tap. We
                    // pick the more discoverable action (single tap = copy)
                    // and keep selection available via the context menu.
                    .foregroundStyle(justCopied ? .green : .primary)
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
        .onTapGesture {
            copy()
        }
        .help(LocalizedStringKey("Click to copy"))
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
