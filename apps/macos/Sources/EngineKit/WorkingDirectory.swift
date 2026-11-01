import Foundation

/// Where the engine keeps its scratch data (`$TMPDIR/SubtitleAdder`: extracted audio, recognition and translation
/// checkpoints). The engine never deletes it, so the app clears it at launch and on quit. Everything is best effort.
public enum WorkingDirectory {
    /// `<temporaryDirectory>/SubtitleAdder`.
    public static func url(temporaryDirectory: URL = FileManager.default.temporaryDirectory) -> URL {
        temporaryDirectory.appending(path: "SubtitleAdder")
    }

    /// Creates the folder if needed and returns it.
    @discardableResult
    public static func ensureExists(
        temporaryDirectory: URL = FileManager.default.temporaryDirectory, fileManager: FileManager = .default
    ) -> URL {
        let folder = url(temporaryDirectory: temporaryDirectory)
        try? fileManager.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    /// Deletes the whole working folder and any engine stderr logs left behind by runs that didn't clean up.
    /// Call only while no engine job runs.
    public static func cleanUp(
        temporaryDirectory: URL = FileManager.default.temporaryDirectory, fileManager: FileManager = .default
    ) {
        try? fileManager.removeItem(at: url(temporaryDirectory: temporaryDirectory))
        removeStaleLogs(temporaryDirectory: temporaryDirectory, fileManager: fileManager)
    }

    /// Deletes `subtitle-engine-<uuid>.log` files (each run's stderr log; a normal run removes its own).
    public static func removeStaleLogs(
        temporaryDirectory: URL = FileManager.default.temporaryDirectory, fileManager: FileManager = .default
    ) {
        let names = (try? fileManager.contentsOfDirectory(atPath: temporaryDirectory.path)) ?? []
        for name in names where name.hasPrefix(logPrefix) && name.hasSuffix(".log") {
            try? fileManager.removeItem(at: temporaryDirectory.appending(path: name))
        }
    }

    static let logPrefix = "subtitle-engine-"

    /// A fresh stderr log path for one engine run.
    static func newLogURL(temporaryDirectory: URL = FileManager.default.temporaryDirectory) -> URL {
        temporaryDirectory.appending(path: "\(logPrefix)\(UUID().uuidString).log")
    }
}
