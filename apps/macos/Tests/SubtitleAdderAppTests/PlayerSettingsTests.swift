import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Settings that belong to the player and the subtitle panel layout.
@MainActor
@Suite struct PlayerSettingsTests {
    private func makeModel(defaults: UserDefaults? = nil) -> (SubtitleViewModel, UserDefaults) {
        let defaults = defaults ?? UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let vm = SubtitleViewModel(catalog: ModelCatalog(engine: nil), defaults: defaults, launchState: EngineLaunchState())
        return (vm, defaults)
    }

    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "settings-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    @Test func autoPlayIsOnByDefaultAndPersists() {
        let (vm, defaults) = makeModel()
        #expect(vm.autoPlayVideo)
        #expect(defaults.object(forKey: "autoPlayVideo") == nil)

        vm.autoPlayVideo = false
        #expect(defaults.bool(forKey: "autoPlayVideo") == false)
        #expect(defaults.object(forKey: "autoPlayVideo") != nil)
        let (again, _) = makeModel(defaults: defaults)
        #expect(!again.autoPlayVideo)

        again.autoPlayVideo = true
        let (third, _) = makeModel(defaults: defaults)
        #expect(third.autoPlayVideo)
    }

    @Test func subtitlePanelIsShownByDefaultAndTheChoicePersists() {
        let (vm, defaults) = makeModel()
        #expect(!vm.hideSubtitlePanel)
        vm.toggleSubtitlePanel()
        #expect(vm.hideSubtitlePanel)
        #expect(defaults.bool(forKey: "hideSubtitlePanel"))
        let (again, _) = makeModel(defaults: defaults)
        #expect(again.hideSubtitlePanel)
        again.toggleSubtitlePanel()
        let (third, _) = makeModel(defaults: defaults)
        #expect(!third.hideSubtitlePanel)
    }

    @Test func fullScreenHidesThePanelWithoutChangingTheSetting() throws {
        let folder = try makeFolder()
        let (vm, _) = makeModel()
        // No player yet: nothing to hide the panel for.
        #expect(!vm.isPlayerShown)
        #expect(vm.showsSubtitlePanel(isFullScreen: true))
        vm.hideSubtitlePanel = true
        #expect(vm.showsSubtitlePanel(isFullScreen: false))

        vm.openVideo(folder.appending(path: "clip.mp4"))
        #expect(vm.isPlayerShown)
        #expect(!vm.showsSubtitlePanel(isFullScreen: false))
        vm.hideSubtitlePanel = false
        #expect(vm.showsSubtitlePanel(isFullScreen: false))
        #expect(!vm.showsSubtitlePanel(isFullScreen: true))
        // Leaving full screen brings the layout back.
        #expect(!vm.hideSubtitlePanel)

        // The player is out of sight on the playlist tab; a playlist run doesn't take it away.
        vm.workspace = .playlist
        #expect(!vm.isPlayerShown && vm.showsSubtitlePanel(isFullScreen: true))
        vm.workspace = .editor
        vm.isPlaylistRunning = true
        #expect(vm.isPlayerShown)
        vm.isPlaylistRunning = false
    }
}
