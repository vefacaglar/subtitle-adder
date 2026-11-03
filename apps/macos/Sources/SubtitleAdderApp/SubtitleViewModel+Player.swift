import Foundation

/// What the player area shows and how the subtitle panel next to it behaves.
extension SubtitleViewModel {
    /// The player is on screen: the editor tab has a video. Player keys, the video
    /// pointer actions and the panel toggle only apply then.
    var isPlayerShown: Bool { workspace == .editor && video != nil }

    /// Whether the subtitle panel is on screen next to the player. Full screen shows the player alone whatever the
    /// setting says, and without a player there is nothing to hide the panel for.
    func showsSubtitlePanel(isFullScreen: Bool) -> Bool {
        !(isPlayerShown && (hideSubtitlePanel || isFullScreen))
    }

    func toggleSubtitlePanel() {
        hideSubtitlePanel.toggle()
    }
}
