import AppKit
import SwiftUI

final class HUDController {
    private var window: NSPanel?
    let state = HUDState()

    @MainActor
    func show(wide: Bool) {
        state.mode = .recording
        state.level = 0
        state.preview = ""
        state.wide = wide
        if window == nil { buildWindow() }
        resizeWindow(wide: wide)
        window?.orderFrontRegardless()
        positionWindow()
    }

    @MainActor
    func setPreview(_ text: String) {
        state.preview = text
    }

    @MainActor
    func setTranscribing() {
        state.mode = .transcribing
    }

    @MainActor
    func setLevel(_ level: Float) {
        state.level = level
    }

    @MainActor
    func hide() {
        window?.orderOut(nil)
        state.mode = .idle
        state.level = 0
        state.preview = ""
    }

    @MainActor
    private func buildWindow() {
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 360, height: 80),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.isFloatingPanel = true
        panel.level = .statusBar
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = true
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
        panel.contentView = NSHostingView(rootView: HUDView().environmentObject(state))
        window = panel
    }

    @MainActor
    private func resizeWindow(wide: Bool) {
        guard let window else { return }
        let size = wide ? NSSize(width: 360, height: 80) : NSSize(width: 220, height: 64)
        var frame = window.frame
        frame.size = size
        window.setFrame(frame, display: false, animate: false)
    }

    @MainActor
    private func positionWindow() {
        guard let window, let screen = NSScreen.main else { return }
        let frame = screen.visibleFrame
        let size = window.frame.size
        let origin = NSPoint(
            x: frame.midX - size.width / 2,
            y: frame.minY + 60
        )
        window.setFrameOrigin(origin)
    }
}

enum HUDMode { case idle, recording, transcribing }

final class HUDState: ObservableObject {
    @Published var mode: HUDMode = .idle
    @Published var level: Float = 0
    @Published var preview: String = ""
    @Published var wide: Bool = false
}

struct HUDView: View {
    @EnvironmentObject var state: HUDState

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            EqualizerView(mode: state.mode, level: state.level)
                .frame(width: 56, height: 32)
            VStack(alignment: .leading, spacing: 2) {
                Text(label)
                    .foregroundStyle(.primary)
                    .font(.system(size: 14, weight: .semibold))
                if !state.preview.isEmpty {
                    Text(state.preview)
                        .foregroundStyle(.secondary)
                        .font(.system(size: 12))
                        .lineLimit(2)
                        .truncationMode(.head)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(
            width: state.wide ? 360 : 220,
            height: state.wide ? 80 : 64,
            alignment: .leading
        )
        .modifier(LiquidGlassBackground(tint: tint))
    }

    private var tint: Color {
        switch state.mode {
        case .recording: return .red
        case .transcribing: return .yellow
        case .idle: return .clear
        }
    }
}

private struct LiquidGlassBackground: ViewModifier {
    let tint: Color

    func body(content: Content) -> some View {
        if #available(macOS 26.0, *) {
            content.glassEffect(
                .regular.tint(tint.opacity(0.18)).interactive(),
                in: .rect(cornerRadius: 18)
            )
        } else {
            content.background(
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .fill(.ultraThinMaterial)
                    .overlay(
                        RoundedRectangle(cornerRadius: 18, style: .continuous)
                            .strokeBorder(Color.white.opacity(0.15), lineWidth: 0.5)
                    )
            )
        }
    }
}

private extension HUDView {
    var label: String {
        switch state.mode {
        case .recording: return NSLocalizedString("Listening…", comment: "")
        case .transcribing: return NSLocalizedString("Transcribing…", comment: "")
        case .idle: return ""
        }
    }
}

private struct EqualizerView: View {
    let mode: HUDMode
    let level: Float

    private let barCount = 5
    @State private var seeds: [Double] = (0..<5).map { _ in Double.random(in: 0...1) }
    @State private var phase: Double = 0
    private let timer = Timer.publish(every: 1.0/30.0, on: .main, in: .common).autoconnect()

    var body: some View {
        HStack(alignment: .center, spacing: 4) {
            ForEach(0..<barCount, id: \.self) { i in
                RoundedRectangle(cornerRadius: 2, style: .continuous)
                    .fill(barColor)
                    .frame(width: 6, height: barHeight(for: i))
                    .animation(.spring(response: 0.18, dampingFraction: 0.55), value: level)
                    .animation(.spring(response: 0.18, dampingFraction: 0.55), value: phase)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onReceive(timer) { _ in
            phase += 1.0/30.0
            // Refresh seeds occasionally so wave shape feels organic.
            if Int(phase * 30) % 6 == 0 {
                seeds = seeds.map { _ in Double.random(in: 0...1) }
            }
        }
    }

    private var barColor: Color {
        switch mode {
        case .recording: return .red
        case .transcribing: return .yellow
        case .idle: return .secondary
        }
    }

    private func barHeight(for index: Int) -> CGFloat {
        let minH: CGFloat = 4
        let maxH: CGFloat = 28
        if mode == .transcribing {
            // Indeterminate wave while transcribing.
            let p = phase * 4 + Double(index) * 0.6
            let v = (sin(p) + 1.0) / 2.0
            return minH + CGFloat(v) * (maxH - minH) * 0.7
        }
        if mode == .idle { return minH }
        // Recording: bar height driven by level + per-bar oscillation for liveliness.
        let oscillation = (sin(phase * 6 + Double(index) * 0.9 + seeds[index] * 6) + 1.0) / 2.0
        let mixed = Double(level) * (0.6 + 0.4 * oscillation)
        return minH + CGFloat(mixed) * (maxH - minH)
    }
}
