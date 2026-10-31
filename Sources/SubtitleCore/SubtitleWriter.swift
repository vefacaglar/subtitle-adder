import Foundation

/// Adım 4: cümleleri SRT / VTT / JSON olarak yazar.
public enum SubtitleFormat: String, CaseIterable, Sendable, Identifiable {
    case srt, vtt, json
    public var id: String { rawValue }
}

public enum SubtitleWriter {
    public static func render(_ sentences: [Sentence], as format: SubtitleFormat) throws -> String {
        switch format {
        case .srt:
            sentences.enumerated().map { i, s in
                "\(i + 1)\n\(timestamp(s.start, ",")) --> \(timestamp(s.end, ","))\n\(s.text)\n"
            }.joined(separator: "\n")
        case .vtt:
            "WEBVTT\n\n" + sentences.map { s in
                "\(timestamp(s.start, ".")) --> \(timestamp(s.end, "."))\n\(s.text)\n"
            }.joined(separator: "\n")
        case .json:
            try String(decoding: jsonEncoder.encode(sentences), as: UTF8.self)
        }
    }

    @discardableResult
    public static func write(_ sentences: [Sentence], to url: URL, as format: SubtitleFormat) throws -> URL {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try render(sentences, as: format).write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    static func timestamp(_ seconds: Double, _ separator: String) -> String {
        let ms = Int((seconds * 1000).rounded())
        return String(format: "%02d:%02d:%02d%@%03d", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, separator, ms % 1000)
    }

    private static let jsonEncoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .withoutEscapingSlashes]
        return encoder
    }()
}
