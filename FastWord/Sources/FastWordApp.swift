import SwiftUI

@main
struct FastWordApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        MenuBarExtra {
            MenuContent()
                .environmentObject(appDelegate.controller)
                .environmentObject(appDelegate.controller.fileTranscriber)
        } label: {
            Image(systemName: "mic.circle")
        }
        .menuBarExtraStyle(.menu)

        WindowGroup(LocalizedStringKey("FastWord History"), id: "history") {
            HistoryView()
                .environmentObject(appDelegate.controller)
                .frame(minWidth: 700, minHeight: 500)
        }
        .windowResizability(.contentSize)

        WindowGroup(LocalizedStringKey("FastWord Settings"), id: "settings") {
            SettingsView()
        }
        .windowResizability(.contentMinSize)

        // A single window: there is only ever one file job at a time.
        Window(LocalizedStringKey("Transcribe File"), id: "transcribe") {
            TranscribeFileView()
                .environmentObject(appDelegate.controller.fileTranscriber)
        }
        .defaultSize(width: 640, height: 560)
        .windowResizability(.contentMinSize)
        // The main menu shows only while a window keeps FastWord a regular
        // app (DockPresence). SwiftUI's default Edit menu has no Find items;
        // TextEditingCommands adds ⌘F, ⌘G and ⌘E for the transcript.
        .commands {
            TextEditingCommands()
            AppMenuCommands()
        }
    }
}

/// SwiftUI's defaults don't fit a menu-bar app: File offers "New … Window"
/// duplicates of History and Settings, the app menu has no Settings item,
/// and Help has no help book behind it.
struct AppMenuCommands: Commands {
    var body: some Commands {
        CommandGroup(replacing: .appSettings) {
            OpenWindowButton(title: "Settings…", id: "settings", shortcut: ",")
        }
        // Replaced with real items, not emptied: an empty .newItem group
        // takes the whole File menu (and Close ⌘W) with it.
        CommandGroup(replacing: .newItem) {
            OpenWindowButton(title: "Transcribe File…", id: "transcribe", shortcut: "o")
            OpenWindowButton(title: "Show History", id: "history", shortcut: nil)
        }
        CommandGroup(replacing: .help) {}
    }
}

private struct OpenWindowButton: View {
    let title: LocalizedStringKey
    let id: String
    let shortcut: KeyEquivalent?
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let button = Button(title) {
            openWindow(id: id)
            NSApp.activate(ignoringOtherApps: true)
        }
        if let shortcut {
            button.keyboardShortcut(shortcut)
        } else {
            button
        }
    }
}

struct MenuContent: View {
    @EnvironmentObject var controller: AppController
    @EnvironmentObject var fileTranscriber: FileTranscriber
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button(LocalizedStringKey("Show History")) {
            show("history")
        }
        .keyboardShortcut("h")

        Button(LocalizedStringKey("Transcribe File…")) {
            show("transcribe")
        }
        .keyboardShortcut("o")

        Button(LocalizedStringKey("Settings…")) {
            show("settings")
        }
        .keyboardShortcut(",")

        Divider()

        Text(controller.statusText)
        if let fileStatus = fileTranscriber.menuStatusLine {
            if fileTranscriber.isBusy {
                Text(fileStatus)
            } else {
                // The job ended: its transcript or error is in the window.
                Button(fileStatus) {
                    show("transcribe")
                }
            }
        }

        Divider()

        Button(LocalizedStringKey("Quit FastWord")) {
            NSApp.terminate(nil)
        }
        .keyboardShortcut("q")
    }

    @MainActor
    private func show(_ id: String) {
        DockPresence.prepareToShowWindow()
        openWindow(id: id)
        NSApp.activate(ignoringOtherApps: true)
    }
}
