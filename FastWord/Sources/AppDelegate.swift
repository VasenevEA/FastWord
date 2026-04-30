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
    @Published var statusText: String = "Idle"
    @Published var history: [HistoryEntry] = []

    private let recorder = Recorder()
    private let sidecar = SidecarClient()
    private let hotkey = HotkeyMonitor()
    private let hud = HUDController()
    private let store = HistoryStore()

    private var isRecording = false
    private var pressStartedAt: Date?
    private let minHoldDuration: TimeInterval = 0.15

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
                self?.statusText = "⚠ Grant Input Monitoring + Accessibility, then quit & relaunch"
                NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")!)
            }
        }
        hotkey.start()
        if hotkey.isActive {
            statusText = "Ready (hold right ⌥ to dictate)"
        }
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
            _ = recorder.stop()
            isRecording = false
            hud.hide()
            statusText = "Ready"
            return
        }
        stopAndTranscribe()
    }

    private func startRecording() {
        do {
            try recorder.start()
            isRecording = true
            statusText = "Recording..."
            hud.show()
            // Pre-warm the model so the first transcription after release is instant.
            Task { try? await sidecar.warmup() }
        } catch {
            statusText = "Mic error: \(error.localizedDescription)"
        }
    }

    private func stopAndTranscribe() {
        guard let pcm = recorder.stop() else {
            isRecording = false
            hud.hide()
            return
        }
        isRecording = false
        hud.setTranscribing()
        statusText = "Transcribing..."

        Task {
            do {
                let text = try await sidecar.transcribe(pcm: pcm)
                await MainActor.run {
                    self.handleTranscribed(text)
                }
            } catch {
                await MainActor.run {
                    self.statusText = "Transcribe failed: \(error.localizedDescription)"
                    self.hud.hide()
                }
            }
        }
    }

    private func handleTranscribed(_ text: String) {
        hud.hide()
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            statusText = "Empty result"
            return
        }
        let entry = HistoryEntry(id: UUID(), text: trimmed, createdAt: Date())
        store.insert(entry)
        history.insert(entry, at: 0)
        Pasteboard.copyAndPaste(trimmed)
        statusText = "Done"
    }
}
