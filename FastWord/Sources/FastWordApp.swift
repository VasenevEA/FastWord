import SwiftUI

@main
struct FastWordApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        MenuBarExtra {
            MenuContent()
                .environmentObject(appDelegate.controller)
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
    }
}

struct MenuContent: View {
    @EnvironmentObject var controller: AppController
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button(LocalizedStringKey("Show History")) {
            openWindow(id: "history")
            NSApp.activate(ignoringOtherApps: true)
        }
        .keyboardShortcut("h")

        Button(LocalizedStringKey("Settings…")) {
            openWindow(id: "settings")
            NSApp.activate(ignoringOtherApps: true)
        }
        .keyboardShortcut(",")

        Divider()

        Text(controller.statusText)

        Divider()

        Button(LocalizedStringKey("Quit FastWord")) {
            NSApp.terminate(nil)
        }
        .keyboardShortcut("q")
    }
}
