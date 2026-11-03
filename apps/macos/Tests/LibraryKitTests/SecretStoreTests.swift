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

    // MARK: Keychain write path (scripted Security calls)

    /// Records the calls `KeychainSecretStore` makes and answers each `update` from a script.
    private final class ScriptedKeychain: KeychainBackend, @unchecked Sendable {
        private let lock = NSLock()
        private var updates: [OSStatus]
        private let addStatus: OSStatus
        private let copyResult: (status: OSStatus, data: Data?)
        private var calls: [String] = []

        init(updates: [OSStatus], add: OSStatus = errSecSuccess, copy: (status: OSStatus, data: Data?) = (errSecItemNotFound, nil)) {
            self.updates = updates
            addStatus = add
            copyResult = copy
        }

        var log: [String] { lock.withLock { calls } }

        func copy(_ query: [String: Any]) -> (status: OSStatus, data: Data?) {
            lock.withLock { calls.append("copy") }
            return copyResult
        }

        func update(_ query: [String: Any], with attributes: [String: Any]) -> OSStatus {
            lock.withLock {
                calls.append("update")
                return updates.isEmpty ? errSecSuccess : updates.removeFirst()
            }
        }

        func add(_ item: [String: Any]) -> OSStatus {
            lock.withLock {
                calls.append("add")
                return addStatus
            }
        }

        func delete(_ query: [String: Any]) -> OSStatus {
            lock.withLock { calls.append("delete") }
            return errSecSuccess
        }
    }

    private func keychain(_ backend: ScriptedKeychain) -> KeychainSecretStore {
        KeychainSecretStore(service: "test", backend: backend)
    }

    @Test func aSuccessfulUpdateIsTheOnlyCall() {
        let backend = ScriptedKeychain(updates: [errSecSuccess])
        #expect(keychain(backend).set(Data("k".utf8), for: "a"))
        #expect(backend.log == ["update"])
    }

    @Test func aMissingItemIsAddedWithoutDeletingAnything() {
        let backend = ScriptedKeychain(updates: [errSecItemNotFound])
        #expect(keychain(backend).set(Data("k".utf8), for: "a"))
        #expect(backend.log == ["update", "add"])
    }

    @Test func anUpdateThatFailsForAnotherReasonFallsBackToDeleteAndAddOnce() {
        for status in [errSecAuthFailed, errSecInteractionNotAllowed, errSecDuplicateItem, errSecParam] {
            let backend = ScriptedKeychain(updates: [status])
            #expect(keychain(backend).set(Data("k".utf8), for: "a"), "\(status)")
            #expect(backend.log == ["update", "delete", "add"], "\(status)")
        }
    }

    @Test func aFailedReplacementReportsFalseAfterExactlyOneRetry() {
        let backend = ScriptedKeychain(updates: [errSecAuthFailed], add: errSecInteractionNotAllowed)
        #expect(!keychain(backend).set(Data("k".utf8), for: "a"))
        #expect(backend.log == ["update", "delete", "add"])
    }

    @Test func readingAndRemovingGoThroughTheBackend() {
        let backend = ScriptedKeychain(updates: [])
        let store = keychain(backend)
        #expect(store.data(for: "a") == nil)
        store.remove("a")
        #expect(backend.log == ["copy", "delete"])
    }

    @Test func readingTellsAnAbsentItemFromOneThatCouldNotBeRead() {
        let found = ScriptedKeychain(updates: [], copy: (errSecSuccess, Data("sk-1".utf8)))
        #expect(keychain(found).read("a") == .found(Data("sk-1".utf8)))
        #expect(keychain(found).string(for: "a") == "sk-1")

        let absent = ScriptedKeychain(updates: [], copy: (errSecItemNotFound, nil))
        #expect(keychain(absent).read("a") == .absent)
        #expect(keychain(absent).data(for: "a") == nil)

        // A refused prompt, a locked keychain or a failed lookup says nothing about whether the item exists.
        for status in [errSecAuthFailed, errSecUserCanceled, errSecInteractionNotAllowed, errSecNotAvailable, errSecParam] {
            let denied = ScriptedKeychain(updates: [], copy: (status, nil))
            #expect(keychain(denied).read("a") == .unavailable, "\(status)")
            #expect(keychain(denied).data(for: "a") == nil, "\(status)")
        }
        // Success without data is not "absent" either.
        #expect(keychain(ScriptedKeychain(updates: [], copy: (errSecSuccess, nil))).read("a") == .unavailable)
    }

    @Test func inMemoryStoreReadsFoundOrAbsent() {
        let store = InMemorySecretStore()
        #expect(store.read("a") == .absent)
        store.set("k", for: "a")
        #expect(store.read("a") == .found(Data("k".utf8)))
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
