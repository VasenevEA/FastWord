import SwiftUI

struct HistoryView: View {
    @EnvironmentObject var controller: AppController
    @State private var search = ""
    @State private var selection: HistoryEntry.ID?

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
                    EntryRow(entry: entry)
                        .tag(entry.id)
                        .contextMenu {
                            Button(LocalizedStringKey("Copy")) { copy(entry.text) }
                        }
                }
                .listStyle(.inset)
            }
        }
        .frame(minWidth: 700, minHeight: 500)
    }

    private var filtered: [HistoryEntry] {
        guard !search.isEmpty else { return controller.history }
        return controller.history.filter { $0.text.localizedCaseInsensitiveContains(search) }
    }

    private func copy(_ text: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(text, forType: .string)
    }
}

struct EntryRow: View {
    let entry: HistoryEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(entry.text)
                .lineLimit(3)
                .textSelection(.enabled)
            Text(entry.createdAt.formatted(date: .abbreviated, time: .shortened))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
    }
}
