import AppKit
import Foundation
import SwiftUI

@main
struct SubtitleAdderApp: App {
    @State private var catalog: ModelCatalog
    @State private var vm: SubtitleViewModel

    init() {
        // Motor erken çıkarsa stdin'e yazmak uygulamayı SIGPIPE ile öldürmesin.
        signal(SIGPIPE, SIG_IGN)
        // `swift run` ile bundle'sız açılınca da öne gelsin ve Dock'ta görünsün.
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
        }
        Settings {
            SettingsView(vm: vm, catalog: catalog)
        }
    }
}
