import SwiftUI

struct SettingsView: View {
    @AppStorage(SettingsKey.livePreviewEnabled) private var livePreview: Bool = false
    @AppStorage(SettingsKey.hotkey) private var hotkeyRaw: String = HotkeyChoice.rightOption.rawValue
    @AppStorage(SettingsKey.language) private var languageRaw: String = LanguageChoice.system.rawValue
    @AppStorage(SettingsKey.transcriptionLanguage) private var transcriptionLangCode: String = ""
    @AppStorage(SettingsKey.idleEviction) private var idleEvictionRaw: String = IdleEvictionChoice.tenMinutes.rawValue
    @AppStorage(SettingsKey.audioHandling) private var audioHandlingRaw: String = AudioHandlingChoice.off.rawValue
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
            } header: {
                Text(LocalizedStringKey("Hotkey"))
            } footer: {
                Text(LocalizedStringKey("Hold the chosen key to record. Release to transcribe and paste."))
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

    private func applyLanguage(_ choice: LanguageChoice) {
        if choice == .system {
            UserDefaults.standard.removeObject(forKey: "AppleLanguages")
        } else {
            UserDefaults.standard.set([choice.rawValue], forKey: "AppleLanguages")
        }
        UserDefaults.standard.synchronize()
    }
}
