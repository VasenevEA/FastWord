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
        case .decoding, .loadingModel, .transcribing:
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
    let dropTargeted: Bool
    @State private var importing = false

    var body: some View {
        VStack(spacing: 14) {
            zone
            HStack(spacing: 12) {
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

                Spacer(minLength: 8)

                Button {
                    openWindow(id: "settings")
                    NSApp.activate(ignoringOtherApps: true)
                } label: {
                    // Wraps rather than truncates: in Russian at the minimum
                    // window width one line would cut the model name in half.
                    Text(String(format: NSLocalizedString("Model: %@ — change in Settings", comment: ""),
                                modelName))
                        .multilineTextAlignment(.trailing)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .buttonStyle(.link)
                .font(.caption)
            }
        }
        .padding(20)
        .fileImporter(isPresented: $importing,
                      allowedContentTypes: AudioFileDecoder.supportedContentTypes) { result in
            if case .success(let url) = result {
                transcriber.start(url: url)
            }
        }
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
                if let duration = transcriber.sourceDuration {
                    Text(TranscriptExport.clock(duration, withHours: false))
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

    private var progress: Double? {
        switch transcriber.phase {
        case .decoding(let p): return p
        case .transcribing(let p): return p
        default: return nil
        }
    }

    private var phaseLabel: String {
        switch transcriber.phase {
        case .decoding: return NSLocalizedString("Decoding audio…", comment: "")
        case .loadingModel: return NSLocalizedString("Loading model…", comment: "")
        default: return NSLocalizedString("Transcribing…", comment: "")
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
                    variant: showTimestamps,
                    makeText: { [transcript, showTimestamps] in
                        TranscriptRendering.attributed(transcript, timestamps: showTimestamps)
                    })
            }
            Divider()
            footer
        }
    }

    private var header: some View {
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
            }
            Spacer(minLength: 8)
            Toggle(LocalizedStringKey("Timestamps"), isOn: $showTimestamps)
                .toggleStyle(.switch)
                .controlSize(.small)
                .disabled(transcript.segments.isEmpty)
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
        return [
            TranscriptExport.clock(transcript.sourceDuration, withHours: false),
            transcript.modelName,
            String(format: NSLocalizedString("processed in %@", comment: ""), took),
        ].joined(separator: " · ")
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
    private static let stampFont = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .regular)

    /// Timestamps mode: one line per segment, stamp in a hanging-indent
    /// column; a blank line where the speaker paused. Otherwise the same
    /// paragraphs as the TXT export.
    static func attributed(_ transcript: FileTranscript, timestamps: Bool) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineHeightMultiple = 1.15
        let body: [NSAttributedString.Key: Any] = [
            .font: bodyFont,
            .foregroundColor: NSColor.textColor,
            .paragraphStyle: paragraph,
        ]
        guard timestamps else {
            return NSAttributedString(string: transcript.plainText, attributes: body)
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
        for (index, para) in TranscriptExport.paragraphs(transcript.segments).enumerated() {
            if index > 0 { out.append(NSAttributedString(string: "\n", attributes: line)) }
            for seg in para {
                out.append(NSAttributedString(
                    string: TranscriptExport.clock(seg.start, withHours: withHours) + "\t", attributes: stamp))
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
        return TranscriptExport.paragraphs(transcript.segments)
            .map { para in
                para.map { "[\(TranscriptExport.clock($0.start, withHours: withHours))] \($0.text)" }
                    .joined(separator: "\n")
            }
            .joined(separator: "\n\n")
    }
}

/// Read-only, selectable NSTextView. A two-hour transcript is ~100k
/// characters — far beyond what a SwiftUI Text lays out smoothly.
struct TranscriptTextView: NSViewRepresentable {
    /// Identifies the transcript; `variant` a way of showing it (timestamps
    /// on/off). The text is only rebuilt when either changes, so scroll
    /// position and selection survive unrelated SwiftUI refreshes.
    let documentID: Date
    let variant: Bool
    let makeText: () -> NSAttributedString

    final class Coordinator {
        var shownDocument: Date?
        var shownVariant: Bool?
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
        // Toggling timestamps re-flows the text; keep the reader at roughly
        // the same place instead of jumping back to the top. A different
        // transcript starts at the top.
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
