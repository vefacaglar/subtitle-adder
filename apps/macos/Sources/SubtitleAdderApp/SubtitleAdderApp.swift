import AppKit
import EngineKit
import Foundation
import SwiftUI

@main
struct SubtitleAdderApp: App {
    @State private var catalog: ModelCatalog
    @State private var vm: SubtitleViewModel
    @State private var language: AppLanguage

    init() {
        // If the engine exits early, writing to stdin shouldn't kill the app with SIGPIPE.
        signal(SIGPIPE, SIG_IGN)
        // Come to the front and show in the Dock even when launched without a bundle via `swift run`.
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate()
        AppearanceController.shared.start()
        // Must exist before anything builds a localized string.
        _language = State(initialValue: AppLanguage())
        // Nothing runs yet, so the engine's scratch folder from earlier sessions can go.
        WorkingDirectory.cleanUp()
        let catalog = ModelCatalog()
        let model = SubtitleViewModel(catalog: catalog)
        _catalog = State(initialValue: catalog)
        _vm = State(initialValue: model)
        // On quit, clear the scratch folder again unless a job is still running (the next launch gets it then).
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main
        ) { _ in
            MainActor.assumeIsolated {
                if !model.isWorking { WorkingDirectory.cleanUp() }
            }
        }
    }

    var body: some Scene {
        WindowGroup("Subtitle Adder") {
            ContentView(vm: vm, catalog: catalog)
                .frame(minWidth: 760, minHeight: 600)
                // Opening with "Open With" from Finder / dropping on the Dock: don't open a new window, hand it to the open one.
                .handlesExternalEvents(preferring: ["*"], allowing: ["*"])
                .onOpenURL { vm.openFiles([$0]) }
                .environment(\.locale, language.effective.locale)
        }
        .commands { LayoutCommands(vm: vm) }
        Settings {
            SettingsView(vm: vm, catalog: catalog, language: language)
                .environment(\.locale, language.effective.locale)
        }
    }
}

extension FocusedValues {
    /// The view model of the focused main window; `nil` while another window (Settings) is key.
    @Entry var subtitleVM: SubtitleViewModel?
}

/// Menu commands for open video (⌘O), generate (⌘↩) and stop (⌘.), so they keep working while theater mode hides the toolbar. They act
/// on the focused main window's view model, so they do nothing while Settings is key (⌘↩ there must not restart a job).
/// Full screen (⌃⌘F) is the system's own View menu item.
private struct LayoutCommands: Commands {
    @Bindable var vm: SubtitleViewModel
    @FocusedValue(\.subtitleVM) private var focused

    var body: some Commands {
        CommandGroup(after: .newItem) {
            Button(L("Video seç"), systemImage: "film") { focused?.requestVideoPicker() }
                .keyboardShortcut("o", modifiers: .command)
                .disabled(focused?.canPickVideo != true)
            Button(L("Altyazı üret"), systemImage: "captions.bubble") { focused?.generate() }
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(focused.map { $0.video == nil || $0.isWorking || $0.workspace != .editor } ?? true)
            Button(L("Durdur"), systemImage: "stop.fill") { focused?.cancel() }
                .keyboardShortcut(".", modifiers: .command)
                .disabled(focused.map { !$0.isWorking } ?? true)
        }
        CommandGroup(after: .toolbar) {
            Toggle(L("Tiyatro modu"), isOn: $vm.isTheaterMode)
            Toggle(L("Altyazıları sağa / alta al"), isOn: $vm.sideBySideLayout)
        }
    }
}
