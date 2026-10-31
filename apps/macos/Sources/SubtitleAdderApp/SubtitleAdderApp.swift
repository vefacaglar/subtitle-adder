import AppKit
import Foundation
import SwiftUI

@main
struct SubtitleAdderApp: App {
    @State private var catalog: ModelCatalog
    @State private var vm: SubtitleViewModel

    init() {
        // If the engine exits early, writing to stdin shouldn't kill the app with SIGPIPE.
        signal(SIGPIPE, SIG_IGN)
        // Come to the front and show in the Dock even when launched without a bundle via `swift run`.
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate()
        let catalog = ModelCatalog()
        _catalog = State(initialValue: catalog)
        _vm = State(initialValue: SubtitleViewModel(catalog: catalog))
    }

    var body: some Scene {
        WindowGroup("Subtitle Adder") {
            ContentView(vm: vm, catalog: catalog)
                .frame(minWidth: 760, minHeight: 600)
                // Opening with "Open With" from Finder / dropping on the Dock: don't open a new window, hand it to the open one.
                .handlesExternalEvents(preferring: ["*"], allowing: ["*"])
                .onOpenURL { vm.openVideo($0) }
        }
        Settings {
            SettingsView(vm: vm, catalog: catalog)
        }
    }
}
