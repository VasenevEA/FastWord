import SwiftUI

struct SettingsView: View {
    @AppStorage(SettingsKey.livePreviewEnabled) private var livePreview: Bool = false
    @AppStorage(SettingsKey.hotkeyBinding) private var hotkeyRaw: String = ""
    @AppStorage(SettingsKey.hotkeySecondaryBinding) private var hotkeySecondaryRaw: String = ""
    @AppStorage(SettingsKey.hotkeyUseCombo) private var useCombo: Bool = false
    @AppStorage("hotkeyComboMode") private var comboModeRaw: String = HotkeyComboMode.either.rawValue
    @AppStorage(SettingsKey.language) private var languageRaw: String = LanguageChoice.system.rawValue
    @AppStorage(SettingsKey.transcriptionLanguage) private var transcriptionLangCode: String = ""
    @AppStorage(SettingsKey.idleEviction) private var idleEvictionRaw: String = IdleEvictionChoice.tenMinutes.rawValue
    @AppStorage(SettingsKey.audioHandling) private var audioHandlingRaw: String = AudioHandlingChoice.off.rawValue
    @AppStorage(SettingsKey.skipEmpty) private var skipEmpty: Bool = true
    @AppStorage(SettingsKey.cleanupEnabled) private var cleanupEnabled: Bool = true
    @State private var languageChanged = false

    private var hotkey: Binding<HotkeyBinding> {
        Binding(
            get: { HotkeyBinding.decode(hotkeyRaw) ?? AppSettings.hotkeyBinding },
            set: { newValue in
                hotkeyRaw = newValue.encoded()
                NotificationCenter.default.post(name: AppSettings.hotkeyChangedNotification, object: nil)
            }
        )
    }

    private var hotkeySecondary: Binding<HotkeyBinding> {
        Binding(
            get: { HotkeyBinding.decode(hotkeySecondaryRaw) ?? AppSettings.hotkeySecondaryBinding },
            set: { newValue in
                hotkeySecondaryRaw = newValue.encoded()
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

    /// Quick access to the single-modifier hotkeys most people want, so the
    /// common case stays one click even though any chord is now recordable.
    private func presetMenu(for target: Binding<HotkeyBinding>) -> some View {
        Menu {
            ForEach(HotkeyBinding.presets, id: \.self) { preset in
                Button(preset.displayName) { target.wrappedValue = preset }
            }
            Divider()
            Button(LocalizedStringKey("hotkey.reset")) { target.wrappedValue = .rightOption }
        } label: {
            Image(systemName: "list.bullet")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(LocalizedStringKey("hotkey.presets"))
    }

    /// A shortcut that clashes with something else fails silently — the keys
    /// simply never arrive. Say so where the user chose it.
    private func conflictRow(_ conflict: HotkeyConflict) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: conflict.isBlocking ? "exclamationmark.triangle.fill" : "info.circle")
                .foregroundStyle(conflict.isBlocking ? .orange : .secondary)
            Text(conflict.message)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// Without Accessibility the tap can't swallow a bound regular key, so the
    /// character reaches whatever the user is typing into.
    private func accessibilityRow() -> some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: 4) {
                Text(LocalizedStringKey("hotkey.needs_accessibility"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button(LocalizedStringKey("hotkey.open_accessibility")) {
                    guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") else { return }
                    NSWorkspace.shared.open(url)
                }
                .font(.caption)
            }
        }
    }

    private var accessibilityGranted: Bool { AXIsProcessTrusted() }

    var body: some View {
        Form {
            Section {
                LabeledContent(LocalizedStringKey("Hold to dictate")) {
                    HStack(spacing: 8) {
                        ShortcutRecorderField(binding: hotkey)
                        presetMenu(for: hotkey)
                    }
                }

                if let conflict = HotkeyConflictChecker.conflict(
                    for: hotkey.wrappedValue,
                    otherBinding: useCombo ? hotkeySecondary.wrappedValue : nil
                ) {
                    conflictRow(conflict)
                }

                if !accessibilityGranted, hotkey.wrappedValue.keyCode != nil {
                    accessibilityRow()
                }

                Toggle(isOn: useComboBinding) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(LocalizedStringKey("Use a second key"))
                        Text(LocalizedStringKey("Bind a second modifier. Useful when you switch between keyboards that have different modifier keys (e.g. one has ⌥, the other has ⌃)."))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                if useCombo {
                    LabeledContent(LocalizedStringKey("Second key")) {
                        HStack(spacing: 8) {
                            ShortcutRecorderField(binding: hotkeySecondary)
                            presetMenu(for: hotkeySecondary)
                        }
                    }

                    if let conflict = HotkeyConflictChecker.conflict(
                        for: hotkeySecondary.wrappedValue,
                        otherBinding: hotkey.wrappedValue
                    ) {
                        conflictRow(conflict)
                    }

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
                VStack(alignment: .leading, spacing: 2) {
                    Text(LocalizedStringKey("Hold the chosen key (or combo) to record. Release to transcribe and paste."))
                    Text(LocalizedStringKey("hotkey.custom_footer"))
                }
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

    private func applyLanguage(_ choice: LanguageChoice) {
        if choice == .system {
            UserDefaults.standard.removeObject(forKey: "AppleLanguages")
        } else {
            UserDefaults.standard.set([choice.rawValue], forKey: "AppleLanguages")
        }
        UserDefaults.standard.synchronize()
    }
}
