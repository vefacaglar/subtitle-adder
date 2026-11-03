import AppKit
import AVFoundation
import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Player behavior ported from the Windows app: seek settling, auto-play, volume, the player keys and the pointer
/// actions on the video.
@MainActor
@Suite struct PlayerControlsTests {
    /// A short silent WAV: the smallest thing `AVPlayer` loads as a real item, without needing a video encoder.
    private func makeSilentWAV(seconds: Int = 3) throws -> URL {
        let sampleRate = 8000
        let dataSize = seconds * sampleRate * 2
        var data = Data()
        func append32(_ value: Int) { withUnsafeBytes(of: UInt32(value).littleEndian) { data.append(contentsOf: $0) } }
        func append16(_ value: Int) { withUnsafeBytes(of: UInt16(value).littleEndian) { data.append(contentsOf: $0) } }
        data.append(contentsOf: Array("RIFF".utf8)); append32(36 + dataSize)
        data.append(contentsOf: Array("WAVEfmt ".utf8)); append32(16)
        append16(1); append16(1); append32(sampleRate); append32(sampleRate * 2); append16(2); append16(16)
        data.append(contentsOf: Array("data".utf8)); append32(dataSize)
        data.append(Data(count: dataSize))
        let url = FileManager.default.temporaryDirectory.appending(path: "silence-\(UUID().uuidString).wav")
        try data.write(to: url)
        return url
    }

    private func waitUntil(timeout: Double = 5, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { try await Task.sleep(for: .milliseconds(25)) }
    }

    // MARK: - Seek settling (PlaybackTimeline)

    @Test func seekHoldsTheRequestedPositionWhilePlayerStillReportsTheOldTime() {
        var timeline = PlaybackTimeline()
        let start = ContinuousClock.now
        timeline.observe(position: 10, duration: 60, now: start)
        #expect(timeline.position == 10)

        #expect(timeline.seek(to: 30, now: start) == 30)
        #expect(timeline.position == 30)
        // The pre-seek time keeps arriving for a few ticks.
        timeline.observe(position: 10.1, duration: 60, now: start + .milliseconds(100))
        timeline.observe(position: 10.2, duration: 60, now: start + .milliseconds(200))
        #expect(timeline.position == 30)
        #expect(timeline.isSettling)
        // The player arrives within the tolerance: from then on its time is believed.
        timeline.observe(position: 30.2, duration: 60, now: start + .milliseconds(300))
        #expect(timeline.position == 30.2)
        #expect(!timeline.isSettling)
        timeline.observe(position: 30.6, duration: 60, now: start + .milliseconds(400))
        #expect(timeline.position == 30.6)
    }

    @Test func toleranceIsInclusiveAtAQuarterSecond() {
        var timeline = PlaybackTimeline()
        let start = ContinuousClock.now
        timeline.observe(position: 0, duration: 60, now: start)
        timeline.seek(to: 20, now: start)
        timeline.observe(position: 19.75, duration: 60, now: start + .milliseconds(50))
        #expect(timeline.position == 19.75)
        timeline.seek(to: 20, now: start + .milliseconds(100))
        timeline.observe(position: 19.7, duration: 60, now: start + .milliseconds(150))
        #expect(timeline.position == 20)
    }

    @Test func requestedPositionIsGivenUpAfterTwoSeconds() {
        var timeline = PlaybackTimeline()
        let start = ContinuousClock.now
        timeline.seek(to: 40, now: start)
        timeline.observe(position: 5, duration: 60, now: start + .milliseconds(1_900))
        #expect(timeline.position == 40)
        // A decoder that can't reach the exact timestamp doesn't pin the position forever.
        timeline.observe(position: 5, duration: 60, now: start + .seconds(2))
        #expect(timeline.position == 5)
        #expect(!timeline.isSettling)
    }

    @Test func aNewerSeekReplacesThePendingOne() {
        var timeline = PlaybackTimeline()
        let start = ContinuousClock.now
        timeline.seek(to: 10, now: start)
        timeline.seek(to: 50, now: start + .milliseconds(500))
        // Still reporting near the first request does not count as arrived at the second.
        timeline.observe(position: 10.1, duration: 60, now: start + .milliseconds(600))
        #expect(timeline.position == 50)
        // The window runs from the newer request.
        timeline.observe(position: 10.1, duration: 60, now: start + .milliseconds(2_400))
        #expect(timeline.position == 50)
        timeline.observe(position: 10.1, duration: 60, now: start + .milliseconds(2_500))
        #expect(timeline.position == 10.1)
    }

    @Test func resetClearsThePendingSeekAndTheDuration() {
        var timeline = PlaybackTimeline()
        let start = ContinuousClock.now
        timeline.observe(position: 0, duration: 60, now: start)
        timeline.seek(to: 30, now: start)
        timeline.reset()
        #expect(timeline.position == 0 && timeline.duration == 0 && !timeline.isSettling)
        timeline.observe(position: 2, duration: 10, now: start + .milliseconds(10))
        #expect(timeline.position == 2)
    }

    @Test func positionStaysInsideTheKnownDuration() {
        var timeline = PlaybackTimeline()
        let start = ContinuousClock.now
        // Unknown duration: only the lower bound applies.
        #expect(timeline.seek(to: 500, now: start) == 500)
        #expect(timeline.seek(to: -5, now: start) == 0)
        timeline.observe(position: 0, duration: 60, now: start + .seconds(3))
        #expect(timeline.seek(to: 500, now: start + .seconds(3)) == 60)
        #expect(timeline.seek(to: .nan, now: start + .seconds(3)) == 0)
        // A non-finite report (an item that isn't ready) keeps what was shown; a non-finite duration means unknown.
        timeline.observe(position: 12, duration: 60, now: start + .seconds(6))
        timeline.observe(position: .nan, duration: .nan, now: start + .seconds(6))
        #expect(timeline.position == 12 && timeline.duration == 0)
    }

    @Test func playerModelKeepsTheRequestedSeekAgainstStaleTicks() throws {
        // Long enough that a duration which happens to be known by now doesn't clamp the seeks below.
        let url = try makeSilentWAV(seconds: 120)
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.load(url)
        let start = ContinuousClock.now
        model.observe(playerTime: 1, now: start)
        model.seek(to: 2)
        model.observe(playerTime: 1.1, now: start + .milliseconds(100))
        #expect(model.currentTime == 2)
        // Held arrow keys add up from the requested position, not from the stale player time.
        model.seek(by: 5)
        #expect(model.currentTime == 7)
        model.observe(playerTime: 1.2, now: start + .milliseconds(200))
        #expect(model.currentTime == 7)
        model.observe(playerTime: 7.1, now: start + .milliseconds(300))
        #expect(model.currentTime == 7.1)
        // Seeking back past the start stops at 0.
        model.seek(by: -60)
        #expect(model.currentTime == 0)
    }

    @Test func loadingAnotherVideoDropsAPendingSeek() throws {
        let url = try makeSilentWAV()
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.load(url)
        let start = ContinuousClock.now
        model.seek(to: 2)
        model.load(url)
        #expect(model.currentTime == 0)
        model.observe(playerTime: 0.5, now: start + .milliseconds(100))
        #expect(model.currentTime == 0.5)
    }

    @Test func seekingWithoutAVideoDoesNothing() {
        let model = PlayerModel()
        model.seek(by: 5)
        #expect(model.currentTime == 0)
    }

    // MARK: - Auto-play

    // `isPlaying` follows the player's `timeControlStatus` through KVO, one main-actor hop behind it. Where a test needs
    // the answer of the player itself right after an action it reads `timeControlStatus`; where it needs `isPlaying` it waits.

    @Test func autoPlayStartsWhenTheItemIsReady() async throws {
        let url = try makeSilentWAV(seconds: 30)
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.load(url, autoPlay: true)
        #expect(!model.isPlaying)
        // The item's own readiness starts it; asking again for the same load changes nothing.
        model.itemReady(generation: model.loadGeneration)
        try await waitUntil { model.isPlaying }
        #expect(model.isPlaying)
        model.pause()
        try await waitUntil { !model.isPlaying }
        #expect(!model.isPlaying)
    }

    @Test func noAutoPlayWhenNotAskedForOrForAStaleLoad() async throws {
        let url = try makeSilentWAV(seconds: 30)
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.load(url)
        model.itemReady(generation: model.loadGeneration)
        #expect(model.player.timeControlStatus == .paused)

        model.load(url, autoPlay: true)
        let stale = model.loadGeneration
        model.load(url, autoPlay: true)
        // The first load's item became ready after the second replaced it: only the current load may start playback.
        model.itemReady(generation: stale)
        #expect(model.player.timeControlStatus == .paused)
        model.itemReady(generation: model.loadGeneration)
        #expect(model.player.timeControlStatus != .paused)
        try await waitUntil { model.isPlaying }
        #expect(model.isPlaying)
        model.pause()
        try await waitUntil { !model.isPlaying }

        model.load(nil, autoPlay: true)
        model.itemReady(generation: model.loadGeneration)
        #expect(model.player.timeControlStatus == .paused)
        #expect(!model.isPlaying)
    }

    @Test func pausingBeforeTheItemIsReadyCancelsAutoPlay() async throws {
        let url = try makeSilentWAV(seconds: 30)
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.load(url, autoPlay: true)
        model.pause()
        model.itemReady(generation: model.loadGeneration)
        #expect(model.player.timeControlStatus == .paused)
        // Even once the item really is ready, nothing starts it.
        try await waitUntil { model.player.currentItem?.status == .readyToPlay }
        try await Task.sleep(for: .milliseconds(150))
        #expect(!model.isPlaying && model.player.timeControlStatus == .paused)
    }

    @Test func failureCancelsAutoPlay() {
        let model = PlayerModel()
        model.load(URL(filePath: "/nonexistent/clip.mp4"), autoPlay: true)
        model.reportFailure(nil)
        model.itemReady(generation: model.loadGeneration)
        #expect(model.player.timeControlStatus == .paused)
        #expect(!model.isPlaying)
    }

    @Test func aRealItemAutoPlaysOnceReadyAndOtherwiseStaysPaused() async throws {
        let url = try makeSilentWAV(seconds: 30)
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.load(url, autoPlay: true)
        try await waitUntil { model.isPlaying }
        #expect(model.isPlaying)
        model.pause()
        // `isPlaying` is the KVO view of the player: it drops when the player has really stopped.
        try await waitUntil { !model.isPlaying }
        #expect(!model.isPlaying && model.player.timeControlStatus == .paused)

        model.load(url)
        try await waitUntil { model.player.currentItem?.status == .readyToPlay }
        try await Task.sleep(for: .milliseconds(150))
        #expect(model.player.currentItem?.status == .readyToPlay)
        #expect(!model.isPlaying && model.player.rate == 0)
    }

    @Test func isPlayingNeverOutlivesAPausedPlayer() async throws {
        let url = try makeSilentWAV(seconds: 30)
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.load(url)
        try await waitUntil { model.player.currentItem?.status == .readyToPlay }
        for round in 0..<5 {
            model.play()
            try await waitUntil { model.isPlaying }
            #expect(model.isPlaying, "round \(round)")
            // Periodic ticks arriving around the pause must not leave `isPlaying` on.
            model.observe(playerTime: model.player.currentTime().seconds)
            model.pause()
            model.observe(playerTime: model.player.currentTime().seconds)
            try await waitUntil { !model.isPlaying }
            try await Task.sleep(for: .milliseconds(60))
            #expect(!model.isPlaying && model.player.timeControlStatus == .paused, "round \(round)")
        }
    }

    @Test func theVideoPlatformHearsStartAndPauseOncePerChange() async throws {
        let url = try makeSilentWAV(seconds: 30)
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        final class Events { var list: [String] = [] }
        let events = Events()
        model.onPlaybackEvent = { event in
            switch event {
            case .started: events.list.append("started")
            case .paused: events.list.append("paused")
            case .leaving: events.list.append("leaving")
            case .seeked: events.list.append("seeked")
            }
        }
        model.load(url)
        try await waitUntil { model.player.currentItem?.status == .readyToPlay }
        model.play()
        try await waitUntil { model.isPlaying }
        model.pause()
        try await waitUntil { !model.isPlaying }
        try await Task.sleep(for: .milliseconds(150))
        // Whatever the player does on the way (AVPlayer can flip its status more than once around a pause), the platform
        // hears a start and a pause alternately, never the same one twice, and the first one is a start.
        func alternates(_ list: [String]) -> Bool {
            list.first == "started" && zip(list, list.dropFirst()).allSatisfy { $0 != $1 }
        }
        #expect(alternates(events.list) && events.list.last == "paused", "\(events.list)")

        // Playing and then replacing the video reports it leaving, and no "paused" after that, which would carry the new
        // video's position.
        model.play()
        try await waitUntil { model.isPlaying }
        try await Task.sleep(for: .milliseconds(100))
        let before = events.list.count
        model.load(nil)
        try await waitUntil { !model.isPlaying }
        try await Task.sleep(for: .milliseconds(150))
        let afterLoad = Array(events.list.dropFirst(before))
        #expect(afterLoad == ["leaving"], "\(events.list)")
    }

    // MARK: - Volume

    @Test func volumeMovesInWholeFivePercentSteps() {
        let model = PlayerModel()
        model.player.volume = 0.5
        for _ in 0..<5 { model.adjustVolume(by: 0.05) }
        #expect(model.volume == 0.75)
        for _ in 0..<20 { model.adjustVolume(by: 0.05) }
        #expect(model.volume == 1)
        for _ in 0..<30 { model.adjustVolume(by: -0.05) }
        #expect(model.volume == 0)
    }

    @Test func volumeCountsFromSilenceWhileMutedAndUnmutesAboveZero() {
        let model = PlayerModel()
        model.player.volume = 0.6
        model.toggleMute()
        #expect(model.isMuted)
        // Down while muted stays silent and muted.
        model.adjustVolume(by: -0.05)
        #expect(model.isMuted && model.volume == 0)
        // Up from silence: 5 %, audible again.
        model.adjustVolume(by: 0.05)
        #expect(!model.isMuted && abs(model.volume - 0.05) < 0.001)
        model.toggleMute()
        model.toggleMute()
        #expect(!model.isMuted)
    }

    @Test func volumeAndMuteLiveOnThePlayerAndSurviveANewVideo() throws {
        let url = try makeSilentWAV()
        defer { try? FileManager.default.removeItem(at: url) }
        let model = PlayerModel()
        model.player.volume = 1
        model.adjustVolume(by: -0.25)
        model.toggleMute()
        // AVKit's own volume control reads these two properties.
        #expect(model.player.volume == 0.75 && model.player.isMuted)
        model.load(url)
        #expect(model.player.volume == 0.75 && model.player.isMuted)
    }

    // MARK: - Keys

    private func resolve(
        _ keyCode: UInt16, _ character: String? = nil, _ modifiers: NSEvent.ModifierFlags = [],
        focus: PlayerShortcut.Focus = .init()
    ) -> PlayerShortcut? {
        PlayerShortcut.resolve(keyCode: keyCode, character: character, modifiers: modifiers, focus: focus)
    }

    @Test func keysMapToPlayerActions() {
        #expect(resolve(49, " ") == .togglePlayback)
        #expect(resolve(123) == .seek(-5))
        #expect(resolve(124) == .seek(5))
        #expect(resolve(123, nil, .shift) == .seek(-10))
        #expect(resolve(124, nil, .shift) == .seek(10))
        #expect(resolve(126) == .adjustVolume(0.05))
        #expect(resolve(125) == .adjustVolume(-0.05))
        #expect(resolve(46, "m") == .toggleMute)
        #expect(resolve(3, "f") == .toggleFullScreen)
        #expect(resolve(14, "e") == .toggleSubtitlePanel)
        // Caps Lock and Shift only change the case.
        #expect(resolve(46, "M", .shift) == .toggleMute)
        #expect(resolve(14, "E") == .toggleSubtitlePanel)
        #expect(resolve(0, "a") == nil)
        #expect(resolve(53) == nil)
    }

    /// The key position stands in only for letters of another script. Punctuation, digits and Latin letters with marks
    /// belong to the layout that typed them.
    @Test func aKeyThatTypesPunctuationOrAMarkedLatinLetterIsNotAShortcutWhateverItsPosition() {
        // AZERTY: "," and ";" sit on the QWERTY M position; the keys at F and E type other things than f and e as well.
        #expect(resolve(46, ",") == nil)
        #expect(resolve(46, ";") == nil)
        #expect(resolve(46, "?") == nil)
        // Turkish Q and F layouts: ğ, ı, ş, ç, ö, ü and İ sit on the positions of F, E, M and their neighbours.
        for (code, typed) in [(3, "ğ"), (3, "Ğ"), (14, "ı"), (46, "ş"), (46, "ç"), (14, "ö"), (3, "ü"), (14, "İ")] as [(UInt16, String)] {
            #expect(resolve(code, typed) == nil, "\(typed) at \(code)")
        }
        // Other Latin letters with marks, digits and symbols.
        for (code, typed) in [(14, "é"), (3, "ñ"), (46, "ø"), (14, "ß"), (3, "1"), (46, "€"), (14, "ẹ")] as [(UInt16, String)] {
            #expect(resolve(code, typed) == nil, "\(typed) at \(code)")
        }
        // Shift and Caps Lock don't change that.
        #expect(resolve(46, ",", .shift) == nil)
        // A key that reports no character at all is still read by position.
        #expect(resolve(46, nil) == .toggleMute)
        #expect(resolve(46, "") == .toggleMute)
    }

    @Test func lettersOfOtherScriptsStillFallBackToTheirKeyPosition() {
        // Cyrillic, Greek, Hebrew and Arabic layouts type their own letters on the M, F and E positions.
        #expect(resolve(46, "ь") == .toggleMute)
        #expect(resolve(46, "Ь", .shift) == .toggleMute)
        #expect(resolve(3, "а") == .toggleFullScreen)
        #expect(resolve(14, "у") == .toggleSubtitlePanel)
        #expect(resolve(46, "μ") == .toggleMute)
        #expect(resolve(3, "φ") == .toggleFullScreen)
        #expect(resolve(14, "ε") == .toggleSubtitlePanel)
        #expect(resolve(14, "ק") == .toggleSubtitlePanel)
        #expect(resolve(3, "ب") == .toggleFullScreen)
        // A key position that is none of ours stays nothing, in any script.
        #expect(resolve(0, "ф") == nil)
        // A Latin letter wins over the position, whichever key it is on.
        #expect(resolve(17, "f") == .toggleFullScreen)
        #expect(resolve(3, "u") == nil)
    }

    @Test func shortcutsIgnoreChordsAndTyping() {
        for modifier: NSEvent.ModifierFlags in [.command, .control, .option, [.command, .shift], [.option, .shift]] {
            #expect(resolve(49, " ", modifier) == nil)
            #expect(resolve(123, nil, modifier) == nil)
            #expect(resolve(126, nil, modifier) == nil)
            #expect(resolve(46, "m", modifier) == nil)
            #expect(resolve(3, "f", modifier) == nil)
            #expect(resolve(14, "e", modifier) == nil)
        }
        let typing = PlayerShortcut.Focus(isEditingText: true)
        for (code, character) in [(49, " "), (123, ""), (124, ""), (125, ""), (126, ""), (46, "m"), (3, "f"), (14, "e")] as [(UInt16, String)] {
            #expect(resolve(code, character, focus: typing) == nil)
        }
    }

    @Test func spaceBelongsToAFocusedButton() {
        #expect(resolve(49, " ", focus: .init(isOnButton: true)) == nil)
        #expect(resolve(124, focus: .init(isOnButton: true)) == .seek(5))
    }

    @Test func upAndDownGoToTheListUnlessItIsOutOfTheWay() {
        let list = PlayerShortcut.Focus(isInList: true)
        #expect(resolve(126, focus: list) == nil)
        #expect(resolve(125, focus: list) == nil)
        // Left and right and the letters still reach the player from the list.
        #expect(resolve(124, focus: list) == .seek(5))
        #expect(resolve(49, " ", focus: list) == .togglePlayback)
        #expect(resolve(46, "m", focus: list) == .toggleMute)
        // Panel hidden or full screen: there is no list to leave the arrows to.
        #expect(resolve(126, focus: .init(isInList: true, panelHidden: true)) == .adjustVolume(0.05))
        #expect(resolve(125, focus: .init(isInList: true, isFullScreen: true)) == .adjustVolume(-0.05))
        #expect(resolve(126, focus: .init(isInList: false)) == .adjustVolume(0.05))
    }

    @Test func lettersWorkOnOtherKeyboardLayouts() {
        // Cyrillic layout: the characters are not Latin, so the physical key decides.
        #expect(resolve(46, "ь") == .toggleMute)
        #expect(resolve(3, "а") == .toggleFullScreen)
        #expect(resolve(14, "у") == .toggleSubtitlePanel)
        // Dvorak: the key at the QWERTY F position types "u", and "f" lives elsewhere.
        #expect(resolve(3, "u") == nil)
        #expect(resolve(17, "f") == .toggleFullScreen)
        #expect(resolve(0, nil) == nil)
    }

    @Test func holdingAKeyRepeatsOnlySeekingAndVolume() {
        #expect(PlayerShortcut.seek(5).repeats && PlayerShortcut.adjustVolume(0.05).repeats)
        for shortcut: PlayerShortcut in [.togglePlayback, .toggleMute, .toggleFullScreen, .toggleSubtitlePanel] {
            #expect(!shortcut.repeats)
        }
    }

    // MARK: - Shortcut actions

    private func makeModel() -> SubtitleViewModel {
        SubtitleViewModel(
            catalog: ModelCatalog(engine: nil),
            defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
    }

    private final class Window {
        var isFullScreen = false
        var toggles = 0
    }

    @Test func shortcutsActOnlyWhileAPlayerIsOnScreen() async throws {
        let url = try makeSilentWAV()
        defer { try? FileManager.default.removeItem(at: url) }
        let vm = makeModel()
        let player = PlayerModel()
        let window = Window()
        let actions = PlayerShortcutActions(
            player: player, vm: vm, isFullScreen: { window.isFullScreen }, toggleFullScreen: { window.toggles += 1 }
        )
        // No video: every key goes on to the focused view.
        #expect(!actions.perform(.togglePlayback))
        #expect(!actions.perform(.toggleMute))
        #expect(!player.isMuted)

        vm.openVideo(url)
        player.load(url)
        #expect(actions.perform(.toggleMute))
        #expect(player.isMuted)
        #expect(actions.perform(.adjustVolume(-0.25)))
        #expect(player.volume == 0)
        #expect(actions.perform(.togglePlayback))
        try await waitUntil { player.isPlaying }
        #expect(player.isPlaying)
        #expect(actions.perform(.togglePlayback))
        try await waitUntil { !player.isPlaying }
        #expect(!player.isPlaying)
        #expect(actions.perform(.toggleFullScreen))
        #expect(window.toggles == 1)

        // On the playlist tab the player is out of sight.
        vm.workspace = .playlist
        #expect(!actions.perform(.togglePlayback))
        vm.workspace = .editor
        vm.isPlaylistRunning = true
        #expect(!actions.perform(.togglePlayback))
        vm.isPlaylistRunning = false
    }

    @Test func toggleKeysIgnoreAutoRepeatAndFullScreenKeepsThePanelSetting() throws {
        let url = try makeSilentWAV()
        defer { try? FileManager.default.removeItem(at: url) }
        let vm = makeModel()
        vm.openVideo(url)
        let player = PlayerModel()
        player.load(url)
        let window = Window()
        let actions = PlayerShortcutActions(
            player: player, vm: vm, isFullScreen: { window.isFullScreen }, toggleFullScreen: { window.toggles += 1 }
        )
        #expect(actions.perform(.toggleMute, isRepeat: true))
        #expect(!player.isMuted)
        #expect(actions.perform(.toggleFullScreen, isRepeat: true))
        #expect(window.toggles == 0)
        #expect(actions.perform(.adjustVolume(-0.05), isRepeat: true))
        #expect(player.volume < 1)

        #expect(actions.perform(.toggleSubtitlePanel))
        #expect(vm.hideSubtitlePanel)
        window.isFullScreen = true
        // The key is used up, but the setting isn't changed where the result can't be seen.
        #expect(actions.perform(.toggleSubtitlePanel))
        #expect(vm.hideSubtitlePanel)
        window.isFullScreen = false
        #expect(actions.perform(.toggleSubtitlePanel))
        #expect(!vm.hideSubtitlePanel)
    }

    // MARK: - Video pointer

    @Test func clickTogglesPlaybackAndDoubleClickAddsFullScreenWithNoNetPlaybackChange() {
        #expect(VideoClick.actions(forClickCount: 1) == [.togglePlayback])
        let double = VideoClick.actions(forClickCount: 2)
        #expect(double == [.togglePlayback, .toggleFullScreen])
        // The first click of the pair already toggled; the second toggles back.
        #expect(double.filter { $0 == .togglePlayback }.count == 1)
        #expect(VideoClick.actions(forClickCount: 3).isEmpty)
        #expect(VideoClick.actions(forClickCount: 0).isEmpty)
    }

    @Test func mouseWheelGivesOneNotchPerEventWhateverItsSpeed() {
        var notches = ScrollNotches()
        #expect(notches.add(delta: 1, isPrecise: false) == 1)
        #expect(notches.add(delta: 7, isPrecise: false) == 1)
        #expect(notches.add(delta: -0.1, isPrecise: false) == -1)
        #expect(notches.add(delta: 0, isPrecise: false) == 0)
        #expect(notches.add(delta: .nan, isPrecise: false) == 0)
    }

    @Test func trackpadScrollAddsUpToNotches() {
        var notches = ScrollNotches()
        #expect(notches.add(delta: 4, isPrecise: true) == 0)
        #expect(notches.add(delta: 4, isPrecise: true) == 0)
        #expect(notches.add(delta: 4, isPrecise: true) == 1)
        // 2 points are left over; 25 more make two notches and 7 left.
        #expect(notches.add(delta: 25, isPrecise: true) == 2)
        // Changing direction drops the leftover.
        #expect(notches.add(delta: -6, isPrecise: true) == 0)
        #expect(notches.add(delta: -5, isPrecise: true) == -1)
        notches.reset()
        #expect(notches.add(delta: -9, isPrecise: true) == 0)
        // Reversing discards the -9 collected so far: 25 points are two notches, 5 left over.
        #expect(notches.add(delta: 25, isPrecise: true) == 2)
        #expect(notches.add(delta: 5, isPrecise: true) == 1)
    }
}
