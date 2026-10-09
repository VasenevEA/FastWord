import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The "Transcribe File" window: a drop zone, then progress, then the
/// finished transcript with copy / export.
struct TranscribeFileView: View {
    @EnvironmentObject var transcriber: FileTranscriber
    @State private var dropTargeted = false

    var body: some View {
        content
            .frame(minWidth: 560, minHeight: 480)
            .navigationSubtitle(transcriber.sourceURL?.lastPathComponent ?? "")
            // Any state but a running job takes a new file, so dropping
            // another recording on a finished transcript just starts over.
            .onDrop(of: [.fileURL], isTargeted: $dropTargeted, perform: handleDrop)
            .onAppear { transcriber.refreshDefaultLanguage() }
    }

    @ViewBuilder
    private var content: some View {
        switch transcriber.phase {
        case .idle:
            DropZoneView(dropTargeted: dropTargeted)
        case .decoding, .downloadingModels, .loadingModel, .transcribing:
            ProgressPhaseView()
        case .finished:
            if let result = transcriber.result {
                ResultView(transcript: result)
                    .overlay { if dropTargeted { DropAgainOverlay() } }
            }
        case .failed(let message):
            OutcomeView(
                symbol: "exclamationmark.triangle",
                tint: .orange,
                title: LocalizedStringKey("Couldn't transcribe the file"),
                message: message)
                .overlay { if dropTargeted { DropAgainOverlay() } }
        case .cancelled:
            OutcomeView(
                symbol: "xmark.circle",
                tint: .secondary,
                title: LocalizedStringKey("Transcription cancelled"),
                message: nil)
                .overlay { if dropTargeted { DropAgainOverlay() } }
        }
    }

    private func handleDrop(_ providers: [NSItemProvider]) -> Bool {
        guard !transcriber.isBusy,
              let provider = providers.first(where: {
                  $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier)
              }) else { return false }
        // Captured now: the completion runs later on a background queue,
        // outside any view update.
        let transcriber = transcriber
        _ = provider.loadObject(ofClass: URL.self) { url, _ in
            guard let url, url.isFileURL, !url.hasDirectoryPath else { return }
            Task { @MainActor in transcriber.start(url: url) }
        }
        return true
    }
}

// MARK: - Idle

private struct DropZoneView: View {
    @EnvironmentObject var transcriber: FileTranscriber
    @Environment(\.openWindow) private var openWindow
    @AppStorage(SettingsKey.activeModel) private var activeModel: String = ""
    @ObservedObject private var speakerModels = DiarizationInstaller.shared
    let dropTargeted: Bool
    @State private var importing = false

    var body: some View {
        VStack(spacing: 14) {
            zone
            // The model link beside the pickers when there's room for it on
            // two lines, otherwise below them: squeezed in, Russian at the
            // minimum window width cut the model name in half.
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) {
                    pickers
                    Spacer(minLength: 8)
                    modelLink
                        .frame(maxWidth: 220, alignment: .trailing)
                }
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 12) {
                        pickers
                    }
                    modelLink
                        .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }
            if transcriber.speakerMode.isOn, speakerModels.state != .installed {
                SpeakerModelsRow(installer: speakerModels)
            }
        }
        .padding(20)
        .onAppear { speakerModels.refresh() }
        .fileImporter(isPresented: $importing,
                      allowedContentTypes: AudioFileDecoder.supportedContentTypes) { result in
            if case .success(let url) = result {
                transcriber.start(url: url)
            }
        }
    }

    @ViewBuilder
    private var pickers: some View {
        Picker(LocalizedStringKey("Language"), selection: language) {
            ForEach(TranscriptionLanguage.all) { lang in
                Text(lang.code.isEmpty ? NSLocalizedString("Auto-detect", comment: "") : lang.name)
                    .tag(lang.code)
            }
        }
        .pickerStyle(.menu)
        .fixedSize()
        .disabled(gigaAMActive)
        .help(gigaAMActive
              ? LocalizedStringKey("GigaAM only recognizes Russian.")
              : LocalizedStringKey("Language spoken in the file. Picking it explicitly is faster and more accurate than auto-detect."))

        Picker(LocalizedStringKey("Speakers"), selection: speakerMode) {
            Text(SpeakerMode.off.displayName).tag(SpeakerMode.off)
            Text(SpeakerMode.auto.displayName).tag(SpeakerMode.auto)
            Divider()
            ForEach(Array(SpeakerMode.exactCounts), id: \.self) { count in
                Text(SpeakerMode.exact(count).displayName).tag(SpeakerMode.exact(count))
            }
        }
        .pickerStyle(.menu)
        .fixedSize()
        .help(LocalizedStringKey("speakers.help"))
    }

    private var modelLink: some View {
        Button {
            openWindow(id: "settings")
            NSApp.activate(ignoringOtherApps: true)
        } label: {
            Text(String(format: NSLocalizedString("Model: %@ — change in Settings", comment: ""),
                        modelName))
                .multilineTextAlignment(.trailing)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .buttonStyle(.link)
        .font(.caption)
    }

    private var zone: some View {
        VStack(spacing: 12) {
            Image(systemName: "waveform")
                .font(.system(size: 46, weight: .light))
                .foregroundStyle(dropTargeted ? Color.accentColor : Color.secondary)
                .padding(.bottom, 4)
            Text(LocalizedStringKey("Drop an audio or video file here"))
                .font(.title3.weight(.medium))
            Text(LocalizedStringKey("Get the full text with timestamps, then copy it or export TXT, SRT, VTT or Markdown."))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            Button(LocalizedStringKey("Choose File…")) { importing = true }
                .controlSize(.large)
                .keyboardShortcut("o", modifiers: .command)
                .padding(.top, 6)
            Text(String(format: NSLocalizedString("Supported: %@", comment: ""),
                        AudioFileDecoder.supportedFormatsSummary))
                .font(.caption)
                .foregroundStyle(.tertiary)
                .padding(.top, 2)
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(dropTargeted ? Color.accentColor.opacity(0.08) : Color.primary.opacity(0.025))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(style: StrokeStyle(lineWidth: 2, dash: [7, 5]))
                .foregroundStyle(dropTargeted ? Color.accentColor : Color.secondary.opacity(0.45))
        )
        .animation(.easeOut(duration: 0.12), value: dropTargeted)
    }

    private var gigaAMActive: Bool {
        // Read through the AppStorage copy so the row updates live when the
        // model changes in Settings.
        activeModel == ModelCatalog.gigaAMSentinel && GigaAMInstaller.isInstalled
    }

    private var modelName: String {
        _ = activeModel
        return FileTranscriber.activeModelDisplayName()
    }

    private var language: Binding<String> {
        Binding(
            get: { gigaAMActive ? "ru" : transcriber.languageCode },
            set: { transcriber.setLanguage($0) }
        )
    }

    private var speakerMode: Binding<SpeakerMode> {
        Binding(
            get: { transcriber.speakerMode },
            set: { transcriber.setSpeakerMode($0) }
        )
    }
}

/// Under the pickers while Speakers is on and its models aren't there yet.
/// A file dropped meanwhile waits for the download (FileTranscriber).
private struct SpeakerModelsRow: View {
    @ObservedObject var installer: DiarizationInstaller

    var body: some View {
        HStack(spacing: 8) {
            switch installer.state {
            case .notInstalled, .installed:
                Image(systemName: "person.2.wave.2")
                    .foregroundStyle(.secondary)
                Text(LocalizedStringKey("Speaker detection needs a one-time 33 MB download."))
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                Button(LocalizedStringKey("Download")) { installer.startDownload() }
                    .controlSize(.small)
            case .downloading(let progress):
                Text(LocalizedStringKey("Downloading speaker detection models…"))
                    .lineLimit(1)
                Spacer(minLength: 8)
                ProgressView(value: progress)
                    .progressViewStyle(.linear)
                    .frame(width: 110)
                Text(FileTranscriber.percent(progress))
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 34, alignment: .trailing)
                Button(LocalizedStringKey("Cancel")) { installer.cancel() }
                    .controlSize(.small)
            case .failed(let message):
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text(String(format: NSLocalizedString("Couldn't download the speaker detection models: %@", comment: ""), message))
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                Button(LocalizedStringKey("Retry")) { installer.startDownload() }
                    .controlSize(.small)
            }
        }
        .font(.callout)
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }
}

// MARK: - Busy

private struct ProgressPhaseView: View {
    @EnvironmentObject var transcriber: FileTranscriber

    var body: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: isVideo ? "film" : "waveform")
                .font(.system(size: 40, weight: .light))
                .foregroundStyle(.secondary)
            VStack(spacing: 4) {
                Text(transcriber.sourceURL?.lastPathComponent ?? "")
                    .font(.headline)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if !subtitle.isEmpty {
                    Text(subtitle)
                        .font(.subheadline)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
            VStack(spacing: 6) {
                if let progress {
                    ProgressView(value: progress)
                        .progressViewStyle(.linear)
                } else {
                    ProgressView()
                        .progressViewStyle(.linear)
                }
                HStack {
                    Text(phaseLabel)
                    Spacer()
                    Text(detail)
                        .monospacedDigit()
                }
                .font(.callout)
                .foregroundStyle(.secondary)
            }
            .frame(maxWidth: 420)
            // Deliberately no Escape shortcut: one stray keypress shouldn't
            // throw away an hour of work.
            Button(LocalizedStringKey("Cancel")) { transcriber.cancel() }
            Spacer()
            Text(LocalizedStringKey("Dictation keeps working meanwhile. You can close this window — the job continues and its progress shows in the FastWord menu."))
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var isVideo: Bool {
        guard let ext = transcriber.sourceURL?.pathExtension,
              let type = UTType(filenameExtension: ext) else { return false }
        return type.conforms(to: .movie)
    }

    /// Duration, and the job's Speakers choice — the picker is out of
    /// sight now, and a dropped file may have started without showing it.
    private var subtitle: String {
        var parts: [String] = []
        if let duration = transcriber.sourceDuration {
            parts.append(TranscriptExport.clock(duration, withHours: false))
        }
        if transcriber.jobSpeakerMode.isOn {
            let format = NSLocalizedString("Speakers: %@", comment: "")
            parts.append(String(format: format, transcriber.jobSpeakerMode.displayName))
        }
        return parts.joined(separator: " · ")
    }

    private var progress: Double? {
        switch transcriber.phase {
        case .decoding(let p): return p
        case .downloadingModels(let p): return p
        case .transcribing(let p): return p
        default: return nil
        }
    }

    private var phaseLabel: String {
        switch transcriber.phase {
        case .decoding: return NSLocalizedString("Decoding audio…", comment: "")
        case .downloadingModels: return NSLocalizedString("Downloading speaker detection models…", comment: "")
        case .loadingModel: return NSLocalizedString("Loading model…", comment: "")
        default:
            return transcriber.jobSpeakerMode.isOn
                ? NSLocalizedString("Transcribing and detecting speakers…", comment: "")
                : NSLocalizedString("Transcribing…", comment: "")
        }
    }

    private var detail: String {
        guard let progress else { return "" }
        return [FileTranscriber.percent(progress), transcriber.timeRemainingText]
            .compactMap { $0 }
            .joined(separator: " · ")
    }
}

// MARK: - Finished

private struct ResultView: View {
    @EnvironmentObject var transcriber: FileTranscriber
    @AppStorage(SettingsKey.transcriptTimestamps) private var showTimestamps = false
    let transcript: FileTranscript
    @State private var justCopied = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if transcript.segments.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "text.badge.xmark")
                        .font(.system(size: 34, weight: .light))
                        .foregroundStyle(.secondary)
                    Text(LocalizedStringKey("No speech was found in this file."))
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                TranscriptTextView(
                    documentID: transcript.createdAt,
                    variant: TextVariant(timestamps: showTimestamps, labels: transcript.labels),
                    makeText: { [transcript, showTimestamps] in
                        TranscriptRendering.attributed(transcript, timestamps: showTimestamps)
                    })
            }
            Divider()
            footer
        }
    }

    /// What the text view shows: rebuilt when either changes.
    private struct TextVariant: Hashable {
        let timestamps: Bool
        let labels: SpeakerLabels
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .center, spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(transcript.sourceName)
                        .font(.headline)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(metadata)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    if let warning = transcript.speakerWarning {
                        // Not an error: the transcript is all there, just
                        // without names.
                        Label {
                            Text(String(format: NSLocalizedString("Couldn't tell the speakers apart: %@", comment: ""),
                                        SpeakerWarning.reason(warning)))
                        } icon: {
                            Image(systemName: "exclamationmark.triangle.fill")
                        }
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .lineLimit(2)
                        .help(warning)
                    }
                }
                Spacer(minLength: 8)
                Toggle(LocalizedStringKey("Timestamps"), isOn: $showTimestamps)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .disabled(transcript.segments.isEmpty)
            }
            // Kept after the user merged everyone into one speaker: the
            // strip holds the only Reset, and ⌘Z dies with the window.
            if transcript.showsSpeakers || (transcript.hasSpeakers && !transcript.labels.isEmpty) {
                SpeakerStrip(transcript: transcript)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.thinMaterial)
    }

    private var footer: some View {
        HStack(spacing: 8) {
            Button {
                transcriber.reset()
            } label: {
                Label(LocalizedStringKey("New File"), systemImage: "plus")
            }
            Spacer()
            Button(action: copy) {
                Label(justCopied ? LocalizedStringKey("Copied") : LocalizedStringKey("Copy"),
                      systemImage: justCopied ? "checkmark" : "doc.on.doc")
            }
            .disabled(transcript.segments.isEmpty)
            Menu {
                ForEach(TranscriptExportFormat.allCases) { format in
                    Button(format.displayName) { export(format) }
                }
            } label: {
                Label(LocalizedStringKey("Export"), systemImage: "square.and.arrow.up")
            }
            .fixedSize()
            .disabled(transcript.segments.isEmpty)
        }
        .padding(12)
    }

    private var metadata: String {
        let formatter = DateComponentsFormatter()
        formatter.unitsStyle = .abbreviated
        formatter.allowedUnits = transcript.processingTime >= 3600 ? [.hour, .minute] : [.minute, .second]
        formatter.maximumUnitCount = 2
        let took = formatter.string(from: max(1, transcript.processingTime.rounded())) ?? ""
        var parts = [
            TranscriptExport.clock(transcript.sourceDuration, withHours: false),
            transcript.modelName,
            String(format: NSLocalizedString("processed in %@", comment: ""), took),
        ]
        // With one speaker found there are no names to show, but the count
        // still tells the step ran.
        if transcript.hasSpeakers {
            parts.append(String(format: NSLocalizedString("Speakers: %@", comment: ""),
                                transcript.speakerCount.formatted()))
        }
        return parts.joined(separator: " · ")
    }

    private func copy() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(TranscriptRendering.plain(transcript, timestamps: showTimestamps), forType: .string)
        justCopied = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            justCopied = false
        }
    }

    private func export(_ format: TranscriptExportFormat) {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "\(transcript.baseName).\(format.fileExtension)"
        if let type = UTType(filenameExtension: format.fileExtension) {
            panel.allowedContentTypes = [type]
        }
        // Subtitles are usually wanted right next to the video.
        if let folder = transcriber.sourceURL?.deletingLastPathComponent() {
            panel.directoryURL = folder
        }
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        let transcript = self.transcript
        let save: (NSApplication.ModalResponse) -> Void = { response in
            guard response == .OK, let url = panel.url else { return }
            do {
                try TranscriptExport.render(transcript, as: format)
                    .write(to: url, atomically: true, encoding: .utf8)
            } catch {
                NSAlert(error: error).runModal()
            }
        }
        if let window = NSApp.keyWindow {
            panel.beginSheetModal(for: window, completionHandler: save)
        } else {
            panel.begin(completionHandler: save)
        }
    }
}

// MARK: - Speakers

/// One chip per speaker under the header: color, name, talk time. A click
/// opens the name field and "Merge with".
private struct SpeakerStrip: View {
    @EnvironmentObject var transcriber: FileTranscriber
    @Environment(\.undoManager) private var undoManager
    let transcript: FileTranscript

    var body: some View {
        let speakers = transcript.speakers
        FlowLayout(spacing: 6, lineSpacing: 6) {
            ForEach(speakers) { speaker in
                SpeakerChip(
                    speaker: speaker,
                    others: speakers.filter { $0.id != speaker.id },
                    undoManager: undoManager)
            }
            if !transcript.labels.isEmpty {
                Button(LocalizedStringKey("Reset")) {
                    transcriber.resetSpeakers(undoManager: undoManager)
                }
                .buttonStyle(.link)
                .font(.caption)
                .padding(.leading, 4)
                .help(LocalizedStringKey("Undo all renames and merges"))
            }
        }
    }
}

private struct SpeakerChip: View {
    @EnvironmentObject var transcriber: FileTranscriber
    let speaker: SpeakerSummary
    let others: [SpeakerSummary]
    /// The transcript window's: a popover is a window of its own, and ⌘Z
    /// must undo a rename back in the transcript.
    let undoManager: UndoManager?
    @State private var editing = false

    var body: some View {
        Button {
            editing = true
        } label: {
            HStack(spacing: 5) {
                Circle()
                    .fill(Color(nsColor: SpeakerPalette.color(for: speaker.id)))
                    .frame(width: 8, height: 8)
                Text(speaker.name)
                    .fontWeight(.medium)
                    .lineLimit(1)
                // Rounded: a lone "uh-huh" is 0.7 s of talk, not "00:00".
                Text("— \(TranscriptExport.clock(speaker.seconds.rounded(), withHours: false)) (\(speaker.share.formatted(.percent.precision(.fractionLength(0)))))")
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .font(.caption)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Capsule().fill(Color.primary.opacity(editing ? 0.13 : 0.06)))
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help(speaker.sample.map { String(format: NSLocalizedString("speaker.sample", comment: ""), $0) } ?? "")
        .popover(isPresented: $editing, arrowEdge: .bottom) {
            SpeakerEditor(
                speaker: speaker,
                others: others,
                onRename: { transcriber.renameSpeaker(speaker.id, to: $0, undoManager: undoManager) },
                onMerge: { transcriber.mergeSpeaker(speaker.id, into: $0, undoManager: undoManager) },
                onDone: { editing = false })
        }
        .accessibilityLabel(speaker.name)
    }
}

/// The chip's popover: rename, or merge into another speaker. A name is
/// applied on Return or when the popover closes.
struct SpeakerEditor: View {
    let speaker: SpeakerSummary
    let others: [SpeakerSummary]
    let onRename: (String) -> Void
    let onMerge: (Int) -> Void
    let onDone: () -> Void

    @State private var draft: String
    @State private var finished = false
    @FocusState private var nameFocused: Bool
    private let original: String

    init(speaker: SpeakerSummary, others: [SpeakerSummary],
         onRename: @escaping (String) -> Void, onMerge: @escaping (Int) -> Void, onDone: @escaping () -> Void) {
        self.speaker = speaker
        self.others = others
        self.onRename = onRename
        self.onMerge = onMerge
        self.onDone = onDone
        // A default name stays a placeholder, so typing replaces it.
        original = speaker.name == SpeakerLabels.defaultName(speaker.id) ? "" : speaker.name
        _draft = State(initialValue: original)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            TextField(LocalizedStringKey("Name"), text: $draft,
                      prompt: Text(SpeakerLabels.defaultName(speaker.id)))
                .labelsHidden()
                .textFieldStyle(.roundedBorder)
                .frame(width: 230)
                .focused($nameFocused)
                .onSubmit {
                    commit()
                    onDone()
                }
            if let match = sameName {
                // Two speakers with one name are usually one person split in two.
                VStack(alignment: .leading, spacing: 6) {
                    Text(String(format: NSLocalizedString("“%@” is already another speaker's name.", comment: ""), match.name))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Button(String(format: NSLocalizedString("Merge with %@", comment: ""), match.name)) {
                        merge(into: match.id)
                    }
                    .controlSize(.small)
                }
                .frame(width: 230, alignment: .leading)
            }
            if !others.isEmpty {
                Divider()
                Menu {
                    ForEach(others) { other in
                        Button(other.name) { merge(into: other.id) }
                    }
                } label: {
                    Text(LocalizedStringKey("Merge with"))
                }
                .fixedSize()
                .help(LocalizedStringKey("Move all of this speaker's lines to another speaker"))
            }
        }
        .padding(14)
        .onAppear {
            // Fresh each time the popover opens, whatever SwiftUI kept.
            draft = original
            finished = false
            nameFocused = true
        }
        .onDisappear(perform: commit)
    }

    private var cleanedDraft: String { SpeakerLabels.clean(draft) }

    private var sameName: SpeakerSummary? {
        let name = cleanedDraft
        guard !name.isEmpty else { return nil }
        return others.first { $0.name.compare(name, options: [.caseInsensitive, .diacriticInsensitive]) == .orderedSame }
    }

    private func commit() {
        guard !finished else { return }
        finished = true
        if cleanedDraft != original { onRename(cleanedDraft) }
    }

    /// Without the rename: once merged, a name typed here would land on
    /// the speaker merged into.
    private func merge(into target: Int) {
        finished = true
        onMerge(target)
        onDone()
    }
}

/// Lays its views out left to right, wrapping onto new lines, each line
/// centered vertically.
private struct FlowLayout: Layout {
    var spacing: CGFloat
    var lineSpacing: CGFloat

    private func lines(_ subviews: Subviews, width: CGFloat) -> [[(index: Int, size: CGSize)]] {
        var lines: [[(index: Int, size: CGSize)]] = [[]]
        var x: CGFloat = 0
        for (index, view) in subviews.enumerated() {
            var size = view.sizeThatFits(.unspecified)
            size.width = min(size.width, width)
            if x > 0, x + size.width > width {
                lines.append([])
                x = 0
            }
            lines[lines.count - 1].append((index, size))
            x += size.width + spacing
        }
        return lines
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        let lines = lines(subviews, width: width)
        let height = lines.reduce(0) { $0 + ($1.map(\.size.height).max() ?? 0) }
            + lineSpacing * CGFloat(max(0, lines.count - 1))
        let widest = lines.map { line in
            line.reduce(0) { $0 + $1.size.width } + spacing * CGFloat(max(0, line.count - 1))
        }.max() ?? 0
        // Every line fits in `widest`, so placing in it wraps the same way.
        return CGSize(width: widest, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for line in lines(subviews, width: bounds.width) {
            let height = line.map(\.size.height).max() ?? 0
            var x = bounds.minX
            for item in line {
                subviews[item.index].place(
                    at: CGPoint(x: x, y: y + (height - item.size.height) / 2),
                    proposal: ProposedViewSize(item.size))
                x += item.size.width + spacing
            }
            y += height + lineSpacing
        }
    }
}

// MARK: - Failed / cancelled

private struct OutcomeView: View {
    @EnvironmentObject var transcriber: FileTranscriber
    let symbol: String
    let tint: Color
    let title: LocalizedStringKey
    let message: String?

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 40, weight: .light))
                .foregroundStyle(tint)
            Text(title)
                .font(.title3.weight(.medium))
            if let message, !message.isEmpty {
                Text(message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .textSelection(.enabled)
                    .lineLimit(8)
                    .frame(maxWidth: 440)
            }
            HStack(spacing: 10) {
                if let url = transcriber.sourceURL {
                    Button(LocalizedStringKey("Try Again")) { transcriber.start(url: url) }
                }
                Button(LocalizedStringKey("New File")) { transcriber.reset() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.top, 6)
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct DropAgainOverlay: View {
    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(.regularMaterial)
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(style: StrokeStyle(lineWidth: 2, dash: [7, 5]))
                .foregroundStyle(Color.accentColor)
            Label(LocalizedStringKey("Drop to transcribe this file"), systemImage: "waveform")
                .font(.title3.weight(.medium))
        }
        .padding(12)
        .allowsHitTesting(false)
    }
}

// MARK: - Transcript text

/// Builds what the transcript view shows (and Copy puts on the clipboard).
enum TranscriptRendering {
    private static let bodyFont = NSFont.systemFont(ofSize: 14)
    private static let nameFont = NSFont.systemFont(ofSize: 14, weight: .semibold)
    private static let stampFont = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .regular)

    /// Timestamps mode: one line per segment, stamp in a hanging-indent
    /// column; a blank line where the speaker paused. Otherwise the same
    /// paragraphs as the TXT export. With speakers, each turn starts with
    /// the speaker's name in their color, after the stamp if there is one.
    static func attributed(_ transcript: FileTranscript, timestamps: Bool) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineHeightMultiple = 1.15
        let body: [NSAttributedString.Key: Any] = [
            .font: bodyFont,
            .foregroundColor: NSColor.textColor,
            .paragraphStyle: paragraph,
        ]
        guard timestamps || transcript.showsSpeakers else {
            return NSAttributedString(string: transcript.plainText, attributes: body)
        }
        func name(_ para: TranscriptExport.Paragraph, style: NSParagraphStyle) -> NSAttributedString? {
            guard let name = para.turnName, let speaker = para.speaker else { return nil }
            let out = NSMutableAttributedString(string: "\(name):", attributes: [
                .font: nameFont,
                .foregroundColor: SpeakerPalette.color(for: speaker),
                .paragraphStyle: style,
            ])
            out.append(NSAttributedString(string: " ", attributes: [.font: bodyFont, .paragraphStyle: style]))
            return out
        }
        guard timestamps else {
            let out = NSMutableAttributedString()
            for (index, para) in TranscriptExport.paragraphs(transcript).enumerated() {
                if index > 0 { out.append(NSAttributedString(string: "\n\n", attributes: body)) }
                if let name = name(para, style: paragraph) { out.append(name) }
                out.append(NSAttributedString(
                    string: TranscriptExport.joinSegments(para.segments.map(\.text)), attributes: body))
            }
            return out
        }

        let withHours = transcript.sourceDuration >= 3600
        let sample = "\(TranscriptExport.clock(0, withHours: withHours))  " as NSString
        let column = ceil(sample.size(withAttributes: [.font: stampFont]).width) + 8
        let hanging = NSMutableParagraphStyle()
        hanging.lineHeightMultiple = 1.15
        hanging.tabStops = [NSTextTab(textAlignment: .left, location: column)]
        hanging.headIndent = column
        let stamp: [NSAttributedString.Key: Any] = [
            .font: stampFont,
            .foregroundColor: NSColor.secondaryLabelColor,
            .paragraphStyle: hanging,
        ]
        var line = body
        line[.paragraphStyle] = hanging

        let out = NSMutableAttributedString()
        for (index, para) in TranscriptExport.paragraphs(transcript).enumerated() {
            if index > 0 { out.append(NSAttributedString(string: "\n", attributes: line)) }
            for (position, seg) in para.segments.enumerated() {
                out.append(NSAttributedString(
                    string: TranscriptExport.clock(seg.start, withHours: withHours) + "\t", attributes: stamp))
                if position == 0, let name = name(para, style: hanging) { out.append(name) }
                out.append(NSAttributedString(string: seg.text + "\n", attributes: line))
            }
        }
        // No trailing newline: it would add an empty line to selections.
        if out.length > 0 { out.deleteCharacters(in: NSRange(location: out.length - 1, length: 1)) }
        return out
    }

    static func plain(_ transcript: FileTranscript, timestamps: Bool) -> String {
        guard timestamps else { return transcript.plainText }
        let withHours = transcript.sourceDuration >= 3600
        return TranscriptExport.paragraphs(transcript)
            .map { para in
                para.segments.enumerated().map { position, seg in
                    let name = position == 0 ? TranscriptExport.namePrefix(para) : ""
                    return "[\(TranscriptExport.clock(seg.start, withHours: withHours))] \(name)\(seg.text)"
                }
                .joined(separator: "\n")
            }
            .joined(separator: "\n\n")
    }
}

/// Read-only, selectable NSTextView. A two-hour transcript is ~100k
/// characters — far beyond what a SwiftUI Text lays out smoothly.
struct TranscriptTextView: NSViewRepresentable {
    /// Identifies the transcript; `variant` a way of showing it (timestamps
    /// on/off, speaker names). The text is only rebuilt when either changes,
    /// so scroll position and selection survive unrelated SwiftUI refreshes.
    let documentID: Date
    let variant: AnyHashable
    let makeText: () -> NSAttributedString

    final class Coordinator {
        var shownDocument: Date?
        var shownVariant: AnyHashable?
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.borderType = .noBorder
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        if let textView = scroll.documentView as? NSTextView {
            textView.isEditable = false
            textView.isSelectable = true
            textView.isRichText = false
            textView.allowsUndo = false
            textView.usesFindBar = true
            textView.isIncrementalSearchingEnabled = true
            textView.drawsBackground = true
            textView.backgroundColor = .textBackgroundColor
            textView.textContainerInset = NSSize(width: 18, height: 16)
            // Let file drags fall through to the window's drop handler, so a
            // new recording can be dropped anywhere on a finished transcript.
            textView.unregisterDraggedTypes()
        }
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let coordinator = context.coordinator
        guard coordinator.shownDocument != documentID || coordinator.shownVariant != variant,
              let textView = scroll.documentView as? NSTextView,
              let storage = textView.textStorage else { return }
        // Toggling timestamps or renaming a speaker re-flows the text; keep
        // the reader at roughly the same place instead of jumping back to the
        // top. A different transcript starts at the top.
        let sameDocument = coordinator.shownDocument == documentID
        let visible = scroll.contentView.bounds
        let oldHeight = textView.frame.height
        let fraction = oldHeight > visible.height ? visible.minY / (oldHeight - visible.height) : 0

        storage.setAttributedString(makeText())
        coordinator.shownDocument = documentID
        coordinator.shownVariant = variant

        if sameDocument, fraction > 0,
           let layout = textView.layoutManager, let container = textView.textContainer {
            layout.ensureLayout(for: container)
            let newHeight = textView.frame.height
            textView.scroll(NSPoint(x: 0, y: fraction * max(0, newHeight - visible.height)))
        } else {
            textView.scroll(.zero)
        }
        if !sameDocument {
            // Edit ▸ Find (⌘F, ⌘G, ⌘E) is sent to the first responder, so a
            // new transcript takes the focus — whether it finishes in an open
            // window or shows up when the window is reopened (a window that
            // isn't key yet keeps the choice for when it becomes key).
            // Deferred: a new view isn't in its window yet.
            DispatchQueue.main.async {
                textView.window?.makeFirstResponder(textView)
            }
        }
    }
}
