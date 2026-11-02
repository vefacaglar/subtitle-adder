import Foundation

/// Where the app keeps its own data (`~/Library/Application Support/SubtitleAdder`): the media index, local profiles
/// and the translation provider list. Unlike `WorkingDirectory` (the engine's scratch folder), nothing here is ever
/// cleaned up by the app. Secrets (API keys, the remembered login) never go here; see `SecretStore`.
public enum AppDataDirectory {
    /// `<Application Support>/SubtitleAdder`.
    public static func url(fileManager: FileManager = .default) -> URL {
        let base = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? fileManager.homeDirectoryForCurrentUser.appending(path: "Library/Application Support")
        return base.appending(path: "SubtitleAdder", directoryHint: .isDirectory)
    }

    /// `<folder>/<name>`, creating `folder` (the default data folder unless given) if needed.
    public static func file(_ name: String, in folder: URL? = nil, fileManager: FileManager = .default) -> URL {
        let folder = folder ?? url(fileManager: fileManager)
        try? fileManager.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder.appending(path: name, directoryHint: .notDirectory)
    }
}
