import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    let controller: AppController = MainActor.assumeIsolated { AppController() }

    func applicationDidFinishLaunching(_ notification: Notification) {
        MainActor.assumeIsolated { controller.start() }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { controller.stop() }
    }
}

@MainActor
final class AppController: ObservableObject {
    @Published var statusText: String = NSLocalizedString("Idle", comment: "")
    @Published var history: [HistoryEntry] = []

    private let recorder = Recorder()
    private let sidecar = SidecarClient()
    private let hotkey = HotkeyMonitor()
    private let hud = HUDController()
    private let store = HistoryStore()

    private var isRecording = false
    private var pressStartedAt: Date?
    private let minHoldDuration: TimeInterval = 0.15
    private var previewTimer: Timer?
    private var previewInFlight = false
    private var previewToken: UUID?

    func start() {
        history = store.loadAll()
        sidecar.start()
        recorder.onLevel = { [weak self] level in
            self?.hud.setLevel(level)
        }
        hotkey.onPressStart = { [weak self] in
            Task { @MainActor in self?.handlePressStart() }
        }
        hotkey.onPressEnd = { [weak self] in
            Task { @MainActor in self?.handlePressEnd() }
        }
        hotkey.onPermissionMissing = { [weak self] in
            Task { @MainActor in
                self?.statusText = NSLocalizedString("status.permission_warning", comment: "")
                NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")!)
            }
        }
        hotkey.start()
        if hotkey.isActive {
            statusText = readyStatusText()
        }
        NotificationCenter.default.addObserver(
            forName: AppSettings.hotkeyChangedNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.hotkey.reloadHotkey()
            self?.statusText = self?.readyStatusText() ?? ""
        }
    }

    private func readyStatusText() -> String {
        let format = NSLocalizedString("status.ready", comment: "")
        return String(format: format, AppSettings.hotkey.displayName)
    }

    func stop() {
        sidecar.stop()
        hotkey.stop()
    }

    private func handlePressStart() {
        pressStartedAt = Date()
        startRecording()
    }

    private func handlePressEnd() {
        guard isRecording else { return }
        let held = pressStartedAt.map { Date().timeIntervalSince($0) } ?? 0
        pressStartedAt = nil
        if held < minHoldDuration {
            // Treat short tap as cancel — don't transcribe noise.
            stopPreviewTimer()
            _ = recorder.stop()
            isRecording = false
            hud.hide()
            statusText = readyStatusText()
            return
        }
        stopAndTranscribe()
    }

    private func startRecording() {
        do {
            try recorder.start()
            isRecording = true
            statusText = NSLocalizedString("Recording...", comment: "")
            hud.show(wide: AppSettings.livePreviewEnabled)
            // Pre-warm the model so the first transcription after release is instant.
            Task { try? await sidecar.warmup() }
            if AppSettings.livePreviewEnabled {
                startPreviewTimer()
            }
        } catch {
            let format = NSLocalizedString("status.mic_error", comment: "")
            statusText = String(format: format, error.localizedDescription)
        }
    }

    private func startPreviewTimer() {
        let token = UUID()
        previewToken = token
        previewInFlight = false
        previewTimer?.invalidate()
        previewTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.firePreview(token: token) }
        }
    }

    private func stopPreviewTimer() {
        previewTimer?.invalidate()
        previewTimer = nil
        previewToken = nil
        previewInFlight = false
    }

    private func firePreview(token: UUID) {
        guard isRecording, previewToken == token, !previewInFlight else { return }
        guard let pcm = recorder.snapshot(maxSeconds: 30), pcm.count >= 1600 * 4 else { return }
        previewInFlight = true
        Task {
            let text = (try? await sidecar.transcribe(pcm: pcm)) ?? ""
            await MainActor.run {
                // Drop response if recording already ended or a newer session started.
                guard self.previewToken == token else {
                    self.previewInFlight = false
                    return
                }
                self.previewInFlight = false
                let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty {
                    self.hud.setPreview(trimmed)
                }
            }
        }
    }

    private func stopAndTranscribe() {
        stopPreviewTimer()
        guard let pcm = recorder.stop() else {
            isRecording = false
            hud.hide()
            return
        }
        isRecording = false
        hud.setTranscribing()
        statusText = NSLocalizedString("Transcribing…", comment: "")

        Task {
            do {
                let text = try await sidecar.transcribe(pcm: pcm)
                await MainActor.run {
                    self.handleTranscribed(text)
                }
            } catch {
                await MainActor.run {
                    let format = NSLocalizedString("status.transcribe_failed", comment: "")
                    self.statusText = String(format: format, error.localizedDescription)
                    self.hud.hide()
                }
            }
        }
    }

    private func handleTranscribed(_ text: String) {
        hud.hide()
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            statusText = NSLocalizedString("Empty result", comment: "")
            return
        }
        let entry = HistoryEntry(id: UUID(), text: trimmed, createdAt: Date())
        store.insert(entry)
        history.insert(entry, at: 0)
        Pasteboard.copyAndPaste(trimmed)
        statusText = NSLocalizedString("Done", comment: "")
    }
}
