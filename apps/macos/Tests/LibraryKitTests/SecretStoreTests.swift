import Foundation
import Testing
@testable import LibraryKit

@Suite struct SecretStoreTests {
    @Test func inMemoryStoreRoundTripsAndRemoves() {
        let store = InMemorySecretStore()
        #expect(store.string(for: "a") == nil)
        #expect(store.set("sk-123", for: "a"))
        #expect(store.set("sk-456", for: "a"))
        #expect(store.string(for: "a") == "sk-456")
        store.remove("a")
        #expect(store.data(for: "a") == nil)
        #expect(store.accounts.isEmpty)
    }

    @Test func dataFileLivesUnderTheGivenFolder() throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "LibraryKitTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = AppDataDirectory.file("media-index.json", in: folder)
        #expect(file.lastPathComponent == "media-index.json")
        #expect(file.deletingLastPathComponent().standardizedFileURL == folder.standardizedFileURL)
        var isDirectory: ObjCBool = false
        #expect(FileManager.default.fileExists(atPath: folder.path, isDirectory: &isDirectory) && isDirectory.boolValue)
    }

    @Test func defaultDataFolderIsInApplicationSupport() {
        #expect(AppDataDirectory.url().path.hasSuffix("Application Support/SubtitleAdder"))
    }
}
