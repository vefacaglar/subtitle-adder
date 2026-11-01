import SwiftUI

/// Small fixed palette for speaker dots; system colors adapt to light and dark mode.
enum SpeakerPalette {
    private static let colors: [Color] = [.blue, .orange, .green, .purple, .pink, .teal]

    static func color(for speaker: Int) -> Color {
        colors[((speaker % colors.count) + colors.count) % colors.count]
    }
}
