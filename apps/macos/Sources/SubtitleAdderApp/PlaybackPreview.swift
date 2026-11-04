import EngineKit
import Foundation

/// Owns one scratch copy for as long as the player (or the conversion task) needs it.
final class PlaybackPreview: Sendable {
    let folder: URL
    var url: URL { folder.appending(path: "preview.mp4") }

    private init() throws {
        folder = WorkingDirectory.ensureExists().appending(path: "preview-\(UUID())", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(
            at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
    }

    deinit {
        try? FileManager.default.removeItem(at: folder)
    }

    static func prepare(_ source: URL) async throws -> PlaybackPreview {
        guard let command = EngineCommand.locate() else { throw EngineError.notFound }
        let preview = try PlaybackPreview()
        try await EngineClient(command: command).makePreview(of: source, at: preview.url)
        return preview
    }
}
