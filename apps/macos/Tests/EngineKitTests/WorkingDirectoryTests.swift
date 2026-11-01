import Foundation
import Testing
@testable import EngineKit

@Suite struct WorkingDirectoryTests {
    private func makeTemp() throws -> URL {
        let temp = FileManager.default.temporaryDirectory.appending(path: "wd-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: temp, withIntermediateDirectories: true)
        return temp
    }

    @Test func usesSubtitleAdderFolderInTemporaryDirectory() throws {
        let temp = try makeTemp()
        defer { try? FileManager.default.removeItem(at: temp) }
        #expect(WorkingDirectory.url(temporaryDirectory: temp) == temp.appending(path: "SubtitleAdder"))
        #expect(WorkingDirectory.url().lastPathComponent == "SubtitleAdder")
        let folder = WorkingDirectory.ensureExists(temporaryDirectory: temp)
        var isDirectory: ObjCBool = false
        #expect(FileManager.default.fileExists(atPath: folder.path, isDirectory: &isDirectory) && isDirectory.boolValue)
    }

    @Test func cleanUpRemovesFolderAndStaleLogsOnly() throws {
        let temp = try makeTemp()
        defer { try? FileManager.default.removeItem(at: temp) }
        let folder = WorkingDirectory.ensureExists(temporaryDirectory: temp)
        try Data("x".utf8).write(to: folder.appending(path: "asr-1.json"))
        let log = WorkingDirectory.newLogURL(temporaryDirectory: temp)
        try Data("log".utf8).write(to: log)
        let unrelated = temp.appending(path: "keep.txt")
        let otherLog = temp.appending(path: "other.log")
        try Data().write(to: unrelated)
        try Data().write(to: otherLog)

        WorkingDirectory.cleanUp(temporaryDirectory: temp)

        #expect(!FileManager.default.fileExists(atPath: folder.path))
        #expect(!FileManager.default.fileExists(atPath: log.path))
        #expect(FileManager.default.fileExists(atPath: unrelated.path))
        #expect(FileManager.default.fileExists(atPath: otherLog.path))
        // Nothing to delete is not an error.
        WorkingDirectory.cleanUp(temporaryDirectory: temp)
    }
}
