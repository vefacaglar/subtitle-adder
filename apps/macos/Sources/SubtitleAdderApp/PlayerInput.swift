import AppKit

/// A key the main window turns into a player or layout action (Space, arrows, M, F, E). Pure: `resolve` only looks at
/// the key and where the keyboard focus is, so the table is testable without a window.
enum PlayerShortcut: Equatable, Sendable {
    case togglePlayback
    /// Seek by this many seconds (negative goes back).
    case seek(Double)
    /// Change the volume by this fraction (0.05 is 5 %).
    case adjustVolume(Double)
    case toggleMute
    case toggleFullScreen
    case toggleSubtitlePanel

    static let seekStep = 5.0
    static let largeSeekStep = 10.0
    static let volumeStep = 0.05

    /// Whether holding the key repeats the action: seeking and volume are meant to, a toggle must not flip back and forth.
    var repeats: Bool {
        switch self {
        case .seek, .adjustVolume: true
        case .togglePlayback, .toggleMute, .toggleFullScreen, .toggleSubtitlePanel: false
        }
    }

    /// What has the keyboard focus, as far as the shortcuts care.
    struct Focus: Equatable, Sendable {
        /// A text field's editor or a text view: every key is typing.
        var isEditingText = false
        /// A button, pop-up or segmented control: Space presses it.
        var isOnButton = false
        /// A list or table: the up and down arrows move its selection.
        var isInList = false
        /// The subtitle panel is not on screen (hidden by the user), so there is no list to leave the arrows to.
        var panelHidden = false
        var isFullScreen = false
    }

    // Hardware key codes (kVK_*), which don't depend on the keyboard layout.
    private static let spaceKey: UInt16 = 49
    private static let leftArrow: UInt16 = 123
    private static let rightArrow: UInt16 = 124
    private static let downArrow: UInt16 = 125
    private static let upArrow: UInt16 = 126
    private static let ansiM: UInt16 = 46
    private static let ansiF: UInt16 = 3
    private static let ansiE: UInt16 = 14

    /// The action for a key press, or `nil` when the key is not ours (a modifier chord, typing, or an arrow the list
    /// needs). `character` is `charactersIgnoringModifiers`.
    static func resolve(
        keyCode: UInt16, character: String?, modifiers: NSEvent.ModifierFlags, focus: Focus
    ) -> PlayerShortcut? {
        let held = modifiers.intersection([.shift, .control, .option, .command])
        guard !focus.isEditingText, held.subtracting(.shift).isEmpty else { return nil }
        switch keyCode {
        case spaceKey:
            return focus.isOnButton ? nil : .togglePlayback
        case leftArrow:
            return .seek(-(held.contains(.shift) ? largeSeekStep : seekStep))
        case rightArrow:
            return .seek(held.contains(.shift) ? largeSeekStep : seekStep)
        case upArrow, downArrow:
            // The list needs its arrows, unless it is not there to use.
            if focus.isInList, !focus.panelHidden, !focus.isFullScreen { return nil }
            return .adjustVolume(keyCode == upArrow ? volumeStep : -volumeStep)
        default:
            switch letter(character: character, keyCode: keyCode) {
            case "m": return .toggleMute
            case "f": return .toggleFullScreen
            case "e": return .toggleSubtitlePanel
            default: return nil
            }
        }
    }

    /// The Latin letter a key stands for: what the layout types, or on a non-Latin layout (Cyrillic, Greek, …) the
    /// letter at that physical key's QWERTY position, so M, F and E keep working there.
    private static func letter(character: String?, keyCode: UInt16) -> Character? {
        if let character = character?.lowercased(), character.count == 1, let first = character.first,
           first.isASCII, first.isLetter {
            return first
        }
        switch keyCode {
        case ansiM: return "m"
        case ansiF: return "f"
        case ansiE: return "e"
        default: return nil
        }
    }
}

/// Runs shortcuts on the player and the window. Only acts while the player is on screen, so Space and the arrows
/// keep their normal meaning on the playlist and when there is no video.
@MainActor
struct PlayerShortcutActions {
    let player: PlayerModel
    let vm: SubtitleViewModel
    let isFullScreen: () -> Bool
    let toggleFullScreen: () -> Void

    /// Whether the shortcut was applicable (and so the key is used up). An auto-repeat of a toggle key is used up
    /// without doing anything.
    @discardableResult
    func perform(_ shortcut: PlayerShortcut, isRepeat: Bool = false) -> Bool {
        guard vm.isPlayerShown else { return false }
        if isRepeat, !shortcut.repeats { return true }
        switch shortcut {
        case .togglePlayback: player.togglePlayback()
        case .seek(let seconds): player.seek(by: seconds)
        case .adjustVolume(let step): player.adjustVolume(by: step)
        case .toggleMute: player.toggleMute()
        case .toggleFullScreen: toggleFullScreen()
        case .toggleSubtitlePanel:
            // Full screen shows the player alone whatever the setting says; changing it unseen would surprise on exit.
            if !isFullScreen() { vm.toggleSubtitlePanel() }
        }
        return true
    }
}

/// What a click on the video does. Playback toggles on the first click (no waiting for a possible second one); the
/// second click of a double click toggles it back and switches full screen, so the pair leaves playback as it was.
enum VideoClick: Equatable, Sendable {
    case togglePlayback
    case toggleFullScreen

    static func actions(forClickCount count: Int) -> [VideoClick] {
        switch count {
        case 1: [.togglePlayback]
        case 2: [.togglePlayback, .toggleFullScreen]
        default: []
        }
    }
}

/// Turns scroll-wheel deltas into whole volume notches. A mouse wheel gives one notch per event whatever its speed;
/// a trackpad sends many small deltas, which add up to a notch every `preciseStep` points, and its momentum tail
/// (the coasting after the fingers lift) is ignored by the caller.
struct ScrollNotches {
    static let preciseStep = 10.0
    private var remainder = 0.0

    /// Signed notches for this event: positive when scrolling up (louder).
    mutating func add(delta: Double, isPrecise: Bool) -> Int {
        guard delta.isFinite, delta != 0 else { return 0 }
        guard isPrecise else {
            remainder = 0
            return delta > 0 ? 1 : -1
        }
        // A change of direction drops what was collected the other way.
        if remainder * delta < 0 { remainder = 0 }
        remainder += delta
        let notches = Int((remainder / Self.preciseStep).rounded(.towardZero))
        remainder -= Double(notches) * Self.preciseStep
        return notches
    }

    mutating func reset() {
        remainder = 0
    }
}
