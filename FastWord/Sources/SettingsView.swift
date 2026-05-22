import SwiftUI

struct SettingsView: View {
    @AppStorage(SettingsKey.livePreviewEnabled) private var livePreview: Bool = false
    @AppStorage(SettingsKey.hotkey) private var hotkeyRaw: String = HotkeyChoice.rightOption.rawValue
    @AppStorage(SettingsKey.hotkeySecondary) private var hotkeySecondaryRaw: String = HotkeyChoice.rightControl.rawValue
    @AppStorage(SettingsKey.hotkeyUseCombo) private var useCombo: Bool = false
    @AppStorage("hotkeyComboMode") private var comboModeRaw: String = HotkeyComboMode.either.rawValue
    @AppStorage(SettingsKey.language) private var languageRaw: String = LanguageChoice.system.rawValue
    @AppStorage(SettingsKey.transcriptionLanguage) private var transcriptionLangCode: String = ""
    @AppStorage(SettingsKey.idleEviction) private var idleEvictionRaw: String = IdleEvictionChoice.tenMinutes.rawValue
    @AppStorage(SettingsKey.audioHandling) private var audioHandlingRaw: String = AudioHandlingChoice.off.rawValue
    @AppStorage(SettingsKey.skipEmpty) private var skipEmpty: Bool = true
    @AppStorage(SettingsKey.cleanupEnabled) private var cleanupEnabled: Bool = true
    @AppStorage(SettingsKey.useGigaAMForRussian) private var useGigaAM: Bool = false
    @StateObject private var gigaAMInstaller = GigaAMInstaller.shared
    @State private var languageChanged = false

    private var hotkey: Binding<HotkeyChoice> {
        Binding(
            get: { HotkeyChoice(rawValue: hotkeyRaw) ?? .rightOption },
            set: { newValue in
                hotkeyRaw = newValue.rawValue
                NotificationCenter.default.post(name: AppSettings.hotkeyChangedNotification, object: nil)
            }
        )
    }

    private var hotkeySecondary: Binding<HotkeyChoice> {
        Binding(
            get: { HotkeyChoice(rawValue: hotkeySecondaryRaw) ?? .rightControl },
            set: { newValue in
                hotkeySecondaryRaw = newValue.rawValue
                NotificationCenter.default.post(name: AppSettings.hotkeyChangedNotification, object: nil)
            }
        )
    }

    private var useComboBinding: Binding<Bool> {
        Binding(
            get: { useCombo },
            set: { newValue in
                useCombo = newValue
                NotificationCenter.default.post(name: AppSettings.hotkeyChangedNotification, object: nil)
            }
        )
    }

    private var comboModeBinding: Binding<HotkeyComboMode> {
        Binding(
            get: { HotkeyComboMode(rawValue: comboModeRaw) ?? .either },
            set: { newValue in
                comboModeRaw = newValue.rawValue
                NotificationCenter.default.post(name: AppSettings.hotkeyChangedNotification, object: nil)
            }
        )
    }

    private var idleEviction: Binding<IdleEvictionChoice> {
        Binding(
            get: { IdleEvictionChoice(rawValue: idleEvictionRaw) ?? .tenMinutes },
            set: { newValue in
                idleEvictionRaw = newValue.rawValue
                NotificationCenter.default.post(name: AppSettings.idleEvictionChangedNotification, object: nil)
            }
        )
    }

    private var language: Binding<LanguageChoice> {
        Binding(
            get: { LanguageChoice(rawValue: languageRaw) ?? .system },
            set: { newValue in
                languageRaw = newValue.rawValue
                applyLanguage(newValue)
                languageChanged = true
            }
        )
    }

    var body: some View {
        Form {
            Section {
                Picker(LocalizedStringKey("Hold to dictate"), selection: hotkey) {
                    ForEach(HotkeyChoice.allCases) { choice in
                        Text(choice.displayName).tag(choice)
                    }
                }
                .pickerStyle(.menu)

                Toggle(isOn: useComboBinding) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(LocalizedStringKey("Use a second key"))
                        Text(LocalizedStringKey("Bind a second modifier. Useful when you switch between keyboards that have different modifier keys (e.g. one has ⌥, the other has ⌃)."))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                if useCombo {
                    Picker(LocalizedStringKey("Second key"), selection: hotkeySecondary) {
                        ForEach(HotkeyChoice.allCases) { choice in
                            Text(choice.displayName).tag(choice)
                        }
                    }
                    .pickerStyle(.menu)

                    Picker(LocalizedStringKey("Combo mode"), selection: comboModeBinding) {
                        ForEach(HotkeyComboMode.allCases) { mode in
                            Text(mode.displayName).tag(mode)
                        }
                    }
                    .pickerStyle(.menu)
                }
            } header: {
                Text(LocalizedStringKey("Hotkey"))
            } footer: {
                Text(LocalizedStringKey("Hold the chosen key (or combo) to record. Release to transcribe and paste."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                Picker(LocalizedStringKey("Transcription language"), selection: $transcriptionLangCode) {
                    ForEach(TranscriptionLanguage.all) { lang in
                        Text(lang.name).tag(lang.code)
                    }
                }
                .pickerStyle(.menu)

                VStack(alignment: .leading, spacing: 6) {
                    Toggle(isOn: $useGigaAM) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(LocalizedStringKey("Use GigaAM v3 for Russian (experimental)"))
                            Text(LocalizedStringKey("When the transcription language is Russian, route audio through Sber's GigaAM-v3 model (sherpa-onnx). MIT-licensed, ~50% lower WER than Whisper-large-v3 on Russian. Other languages still use Whisper."))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    if useGigaAM {
                        gigaAMRow
                    }
                }

                Toggle(isOn: $skipEmpty) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(LocalizedStringKey("Skip empty recordings"))
                        Text(LocalizedStringKey("If Whisper thinks the clip contains no speech, don't paste anything. Catches accidental short taps and silent clips."))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Toggle(isOn: $cleanupEnabled) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(LocalizedStringKey("Clean up transcripts"))
                        Text(LocalizedStringKey("Remove known Whisper hallucinations (‘Subtitles by Amara.org’, ‘Подписывайтесь на канал’), collapse repeated words, trim leading punctuation."))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Toggle(isOn: $livePreview) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(LocalizedStringKey("Live preview while recording"))
                        Text(LocalizedStringKey("Transcribe in chunks every 2s and show progress in the HUD. The pasted result still uses a full final pass for accuracy."))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            } header: {
                Text(LocalizedStringKey("Transcription"))
            } footer: {
                Text(LocalizedStringKey("Auto-detect lets Whisper pick the language. Choose a specific one if you mostly dictate in it — quality and speed improve."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                Picker(LocalizedStringKey("Unload model after"), selection: idleEviction) {
                    ForEach(IdleEvictionChoice.allCases) { choice in
                        Text(choice.displayName).tag(choice)
                    }
                }
                .pickerStyle(.menu)
            } header: {
                Text(LocalizedStringKey("Memory"))
            } footer: {
                Text(LocalizedStringKey("After this much idle time the Whisper model is dropped from RAM. Shorter saves memory, longer keeps the next dictation instant."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                ModelManagementView()
            } header: {
                Text(LocalizedStringKey("Models"))
            } footer: {
                Text(LocalizedStringKey("Download additional whisper.cpp models on demand. Larger = higher quality; smaller = faster and less RAM. The bundled model is always available offline."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                Picker(LocalizedStringKey("Background audio"), selection: $audioHandlingRaw) {
                    ForEach(AudioHandlingChoice.allCases) { choice in
                        Text(choice.displayName).tag(choice.rawValue)
                    }
                }
                .pickerStyle(.menu)
            } header: {
                Text(LocalizedStringKey("Audio"))
            } footer: {
                Text(LocalizedStringKey("audio.footer"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                Picker(LocalizedStringKey("Interface language"), selection: language) {
                    ForEach(LanguageChoice.allCases) { choice in
                        Text(choice.displayName).tag(choice)
                    }
                }
                .pickerStyle(.menu)
                if languageChanged {
                    Text(LocalizedStringKey("Restart the app to apply the language change."))
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            } header: {
                Text(LocalizedStringKey("Language"))
            }
        }
        .formStyle(.grouped)
        .frame(minWidth: 640, idealWidth: 680, minHeight: 600, idealHeight: 720)
    }

    @ViewBuilder
    private var gigaAMRow: some View {
        HStack(spacing: 8) {
            switch gigaAMInstaller.state {
            case .notInstalled:
                Image(systemName: "arrow.down.circle")
                    .foregroundStyle(.secondary)
                Text(LocalizedStringKey("GigaAM model not downloaded yet (~215 MB)"))
                    .font(.caption)
                Spacer()
                Button(LocalizedStringKey("Download")) {
                    gigaAMInstaller.startDownload()
                }
                .controlSize(.small)
            case .downloading(let progress):
                ProgressView(value: progress)
                    .frame(maxWidth: 180)
                Text(String(format: "%.0f %%", progress * 100))
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                Spacer()
                Button(LocalizedStringKey("Cancel")) {
                    gigaAMInstaller.cancel()
                }
                .controlSize(.small)
            case .installed:
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                Text(LocalizedStringKey("GigaAM model installed"))
                    .font(.caption)
                Spacer()
            case .failed(let message):
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text(message)
                    .font(.caption)
                    .lineLimit(2)
                Spacer()
                Button(LocalizedStringKey("Retry")) {
                    gigaAMInstaller.startDownload()
                }
                .controlSize(.small)
            }
        }
        .padding(.leading, 4)
    }

    private func applyLanguage(_ choice: LanguageChoice) {
        if choice == .system {
            UserDefaults.standard.removeObject(forKey: "AppleLanguages")
        } else {
            UserDefaults.standard.set([choice.rawValue], forKey: "AppleLanguages")
        }
        UserDefaults.standard.synchronize()
    }
}
