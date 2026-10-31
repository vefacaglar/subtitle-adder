import AppKit
import SwiftUI

@main
struct SubtitleAdderApp: App {
    init() {
        // `swift run` ile bundle'sız açılınca da öne gelsin ve Dock'ta görünsün.
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate()
    }

    var body: some Scene {
        WindowGroup("Subtitle Adder") {
            ContentView()
                .frame(minWidth: 720, minHeight: 480)
        }
    }
}
