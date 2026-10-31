import AppKit
import Foundation
import SwiftUI

@main
struct SubtitleAdderApp: App {
    init() {
        // Motor erken çıkarsa stdin'e yazmak uygulamayı SIGPIPE ile öldürmesin.
        signal(SIGPIPE, SIG_IGN)
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
