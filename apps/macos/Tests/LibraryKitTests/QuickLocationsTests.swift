import Foundation
import Testing
@testable import LibraryKit

@Suite struct QuickLocationsTests {
    /// A file manager that reports chosen standard folders and volumes.
    private final class FakeFileManager: FileManager, @unchecked Sendable {
        var folders: [SearchPathDirectory: URL] = [:]
        var volumes: [URL] = []
        private(set) var volumeOptions: VolumeEnumerationOptions?

        override func urls(for directory: SearchPathDirectory, in domainMask: SearchPathDomainMask) -> [URL] {
            folders[directory].map { [$0] } ?? []
        }

        override func mountedVolumeURLs(includingResourceValuesForKeys propertyKeys: [URLResourceKey]?, options: VolumeEnumerationOptions = []) -> [URL]? {
            volumeOptions = options
            return volumes
        }
    }

    @Test func listsExistingStandardFoldersInOrderThenVolumes() throws {
        let box = try MediaIndexSandbox()
        let manager = FakeFileManager()
        manager.folders = [
            .moviesDirectory: try box.folder("Movies"),
            .downloadsDirectory: box.root.appending(path: "Missing Downloads", directoryHint: .isDirectory),
            .desktopDirectory: try box.folder("Desktop"),
        ]
        let usb = try box.folder("Volumes/USB")
        let nas = try box.folder("Volumes/NAS")
        manager.volumes = [URL(filePath: "/"), usb, nas]

        let locations = QuickLocations.current(fileManager: manager)
        #expect(locations.map(\.url.path) == [box.path("Movies"), box.path("Desktop"), box.path("Volumes/USB"), box.path("Volumes/NAS")])
        #expect(locations[0].kind == .movies)
        #expect(locations[1].kind == .desktop)
        for location in locations.dropFirst(2) {
            guard case .volume(let name) = location.kind else {
                Issue.record("expected a volume: \(location)")
                continue
            }
            #expect(!name.isEmpty)
        }
        #expect(manager.volumeOptions == [.skipHiddenVolumes])
    }

    @Test func downloadsComesBetweenMoviesAndDesktop() throws {
        let box = try MediaIndexSandbox()
        let manager = FakeFileManager()
        manager.folders = [
            .desktopDirectory: try box.folder("Desktop"),
            .downloadsDirectory: try box.folder("Downloads"),
            .moviesDirectory: try box.folder("Movies"),
        ]
        #expect(QuickLocations.current(fileManager: manager).map(\.kind) == [.movies, .downloads, .desktop])
    }

    @Test func eachPathAppearsOnce() throws {
        let box = try MediaIndexSandbox()
        let manager = FakeFileManager()
        let movies = try box.folder("Movies")
        manager.folders = [.moviesDirectory: movies, .desktopDirectory: movies.appending(path: "../Movies/")]
        let usb = try box.folder("USB")
        manager.volumes = [usb, usb, URL(filePath: box.path("usb")), movies]

        let locations = QuickLocations.current(fileManager: manager)
        // Movies and Desktop are the same folder here, USB is listed three times (once in another case), and a volume
        // sits at the Movies path: the first mention of each path wins.
        #expect(locations.count == 2)
        #expect(locations[0].kind == .movies)
        #expect(locations[1].volumeName != nil)
        #expect(locations.map(\.url.path) == [box.path("Movies"), box.path("USB")])
    }

    @Test func noFoldersAndNoVolumesGiveAnEmptyList() {
        let manager = FakeFileManager()
        manager.volumes = [URL(filePath: "/")]
        #expect(QuickLocations.current(fileManager: manager).isEmpty)
    }

    @Test func realMachineHasNoBootVolumeAndOnlyExistingFolders() {
        let locations = QuickLocations.current()
        #expect(Set(locations.map(\.id)).count == locations.count)
        for location in locations {
            #expect(location.url.path != "/")
            #expect(FileManager.default.fileExists(atPath: location.url.path), "\(location.url.path)")
        }
        // Folders come before volumes.
        let kinds = locations.map(\.kind)
        if let firstVolume = kinds.firstIndex(where: { if case .volume = $0 { true } else { false } }) {
            #expect(kinds[firstVolume...].allSatisfy { if case .volume = $0 { true } else { false } })
        }
    }
}

private extension QuickLocation {
    var volumeName: String? {
        if case .volume(let name) = kind { name } else { nil }
    }
}
