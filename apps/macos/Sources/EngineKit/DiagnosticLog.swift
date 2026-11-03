import Foundation

/// A small diagnostic log, `~/Library/Application Support/SubtitleAdder/diagnostics.log`, for failures that would
/// otherwise vanish (a background translation batch that failed, the engine's `[translation] ` trace lines).
///
/// Thread-safe and never blocks the caller: lines are queued and written on a serial background queue. When the file
/// grows past `maxBytes` (2 MB) it is moved to `diagnostics.log.old` (replacing the previous one) and a new file starts.
/// It never receives subtitle text, prompts or API keys: callers pass diagnostics only, and every line is scrubbed of
/// the configured API keys (and any `Bearer` token) before it is written.
public final class DiagnosticLog: Sendable {
    /// The app-wide log. A process hosting XCTest (the test runner) writes to the temporary folder instead, so
    /// running the tests never fills a developer's real log.
    public static let shared = DiagnosticLog(
        fileURL: NSClassFromString("XCTestCase") == nil
            ? DiagnosticLog.defaultFileURL
            : FileManager.default.temporaryDirectory.appending(path: "SubtitleAdder-tests-diagnostics.log")
    )

    /// Rotation threshold in bytes.
    public static let defaultMaxBytes = 2 * 1024 * 1024
    /// Longest message kept on one line, in characters.
    static let maxMessageLength = 2000
    /// Prefix of the engine's stderr lines that are copied here (the rest of stderr is free-form noise).
    public static let translationPrefix = "[translation] "

    /// Where the log is written.
    public let fileURL: URL
    private let maxBytes: Int
    private let secrets: @Sendable () -> [String]
    private let queue = DispatchQueue(label: "SubtitleAdder.DiagnosticLog", qos: .utility)

    /// `<Application Support>/SubtitleAdder/diagnostics.log`.
    public static var defaultFileURL: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.homeDirectoryForCurrentUser.appending(path: "Library/Application Support")
        return base.appending(path: "SubtitleAdder", directoryHint: .isDirectory).appending(path: "diagnostics.log")
    }

    /// `secrets` names values that must never be written, evaluated for every line; by default the API key of the
    /// external translation server the app is configured with.
    public init(
        fileURL: URL = DiagnosticLog.defaultFileURL, maxBytes: Int = DiagnosticLog.defaultMaxBytes,
        secrets: @escaping @Sendable () -> [String] = { [EngineLaunchState.shared.settings.externalLLM?.apiKey ?? ""] }
    ) {
        self.fileURL = fileURL
        self.maxBytes = maxBytes
        self.secrets = secrets
    }

    /// The rotated copy next to the log.
    public var rotatedURL: URL { fileURL.appendingPathExtension("old") }

    /// Queues one line `<ISO time> [category] message`. `extraSecrets` are scrubbed too (for example the key an engine
    /// process was launched with). Returns at once.
    public func write(_ category: String, _ message: String, redacting extraSecrets: [String] = []) {
        let line = Self.format(
            date: Date(), category: category, message: message, secrets: secrets() + extraSecrets
        )
        queue.async { self.append([line]) }
    }

    /// Queues the lines of the engine's stderr log that start with `[translation] `, as they are (they are JSON lines),
    /// scrubbed of secrets.
    public func writeEngineLines(_ lines: [String], redacting extraSecrets: [String] = []) {
        guard !lines.isEmpty else { return }
        let secrets = self.secrets() + extraSecrets
        let now = Date()
        let formatted = lines.map { Self.format(date: now, category: "engine", message: $0, secrets: secrets) }
        queue.async { self.append(formatted) }
    }

    /// Waits until everything queued so far is on disk (tests, and the end of a run).
    public func flush() {
        queue.sync {}
    }

    // MARK: - Formatting

    /// `message` without newlines, secrets and bearer tokens, cut to `maxMessageLength`.
    static func sanitize(_ message: String, secrets: [String]) -> String {
        var text = message
        for secret in secrets {
            let key = secret.trimmingCharacters(in: .whitespacesAndNewlines)
            if !key.isEmpty { text = text.replacingOccurrences(of: key, with: "[redacted]") }
        }
        text = text.replacingOccurrences(
            of: #"(?i)(bearer\s+)[^\s"',;]+"#, with: "$1[redacted]", options: .regularExpression
        )
        text = text.split(whereSeparator: \.isNewline).joined(separator: " ")
        return String(text.prefix(maxMessageLength))
    }

    static func format(date: Date, category: String, message: String, secrets: [String]) -> String {
        let time = date.ISO8601Format()
        return "\(time) [\(category)] \(sanitize(message, secrets: secrets))\n"
    }

    // MARK: - Writing (on the queue)

    private func append(_ lines: [String]) {
        let fileManager = FileManager.default
        do {
            try fileManager.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            let size = (try? fileManager.attributesOfItem(atPath: fileURL.path)[.size] as? Int) ?? 0
            if size > maxBytes {
                try? fileManager.removeItem(at: rotatedURL)
                try fileManager.moveItem(at: fileURL, to: rotatedURL)
            }
            if !fileManager.fileExists(atPath: fileURL.path) {
                fileManager.createFile(atPath: fileURL.path, contents: nil)
            }
            let handle = try FileHandle(forWritingTo: fileURL)
            defer { try? handle.close() }
            try handle.seekToEnd()
            try handle.write(contentsOf: Data(lines.joined().utf8))
        } catch {
            // A diagnostic log must never get in the way: a full disk or a read-only folder just drops the lines.
        }
    }

    // MARK: - Engine stderr

    /// At most this many bytes from the end of the stderr file are scanned.
    static let maxScannedBytes = 4 * 1024 * 1024
    /// At most this many matching lines are copied from one run.
    static let maxEngineLines = 500

    /// The `[translation] ` lines in an engine stderr file (the last `maxScannedBytes` of it), in order, at most
    /// `maxEngineLines`. Progress output uses carriage returns, so those separate lines too.
    static func translationLines(inLogAt url: URL) -> [String] {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return [] }
        defer { try? handle.close() }
        guard let size = try? handle.seekToEnd() else { return [] }
        let start = size > UInt64(maxScannedBytes) ? size - UInt64(maxScannedBytes) : 0
        guard (try? handle.seek(toOffset: start)) != nil, let data = try? handle.readToEnd() else { return [] }
        let text = String(decoding: data, as: UTF8.self)
        let lines = text.split(whereSeparator: { $0 == "\n" || $0 == "\r" })
            .filter { $0.hasPrefix(translationPrefix) }
            .map(String.init)
        return Array(lines.suffix(maxEngineLines))
    }
}
