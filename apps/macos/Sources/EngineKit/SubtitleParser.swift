import Foundation

/// Parses existing subtitle files (`.srt`, `.vtt`, `.ass`/`.ssa`, engine `.json`) into sentences so a user who
/// already has one can view, edit and translate it without extracting audio or running speech recognition.
/// Port of the Windows app's `SubtitleParser`; parsed lines carry no words and no speaker.
public enum SubtitleParser {
    /// Extensions recognized as subtitle files; also the order of the sidecar lookup.
    public static let extensions = ["srt", "vtt", "ass", "ssa", "json"]

    private static let timestamp = #"(?:\d{1,2}:)?\d{1,2}:\d{2}[,.]\d{1,3}"#

    public static func isSubtitleFile(_ url: URL) -> Bool {
        extensions.contains(url.pathExtension.lowercased())
    }

    /// `<stem>.srt/.vtt/.ass/.ssa/.json` next to the video, if one exists. Only the exact stem matches: files with
    /// another suffix, such as an exported `Film.es.srt`, are not sidecars.
    public static func findSidecar(for video: URL, fileManager: FileManager = .default) -> URL? {
        let folder = video.deletingLastPathComponent()
        let stem = video.deletingPathExtension().lastPathComponent
        guard !stem.isEmpty else { return nil }
        return extensions
            .map { folder.appending(path: stem + "." + $0) }
            .first { fileManager.fileExists(atPath: $0.path) }
    }

    /// Reads a file (BOM-aware UTF-8/UTF-16, strict UTF-8, then Windows-1254) and parses its lines.
    public static func parseFile(at url: URL) throws -> [Sentence] {
        parse(decode(try Data(contentsOf: url)), extension: url.pathExtension)
    }

    /// Parses subtitle text; `extension` (with or without the dot) picks the format, otherwise it is sniffed.
    public static func parse(_ content: String, extension ext: String? = nil) -> [Sentence] {
        guard !content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return [] }
        let trimmed = String(content.drop { $0 == "\u{FEFF}" || $0 == " " || $0 == "\t" || $0 == "\r" || $0 == "\n" || $0 == "\r\n" })
        let ext = (ext ?? "").trimmingCharacters(in: .whitespaces).lowercased().drop { $0 == "." }
        if ext == "ass" || ext == "ssa" || trimmed.range(of: "[Events]", options: .caseInsensitive) != nil {
            return parseASS(trimmed)
        }
        if ext == "json" || (trimmed.hasPrefix("[") && trimmed.trimmingCharacters(in: .whitespacesAndNewlines).hasSuffix("]")) {
            return parseJSON(trimmed)
        }
        return parseSRTOrVTT(trimmed)
    }

    // MARK: Encoding

    static func decode(_ data: Data) -> String {
        let bytes = [UInt8](data.prefix(3))
        if bytes.starts(with: [0xEF, 0xBB, 0xBF]) {
            return String(decoding: data.dropFirst(3), as: UTF8.self)
        }
        if bytes.starts(with: [0xFF, 0xFE]) {
            return String(data: data.dropFirst(2), encoding: .utf16LittleEndian) ?? ""
        }
        if bytes.starts(with: [0xFE, 0xFF]) {
            return String(data: data.dropFirst(2), encoding: .utf16BigEndian) ?? ""
        }
        // Strict: any invalid sequence means a legacy code page. Windows-1254 covers Turkish and Western files.
        return String(data: data, encoding: .utf8)
            ?? String(data: data, encoding: .windowsCP1254)
            ?? String(data: data, encoding: .isoLatin1) ?? ""
    }

    // MARK: Formats

    private static func parseJSON(_ json: String) -> [Sentence] {
        // Like SRT/VTT/ASS, the lines are numbered 1...n: a third-party file with repeated or constant ids would
        // otherwise make results (matched by id) land on the wrong line.
        var sentences = (try? JSONDecoder().decode([Sentence].self, from: Data(json.utf8))) ?? []
        sentences.renumber()
        return sentences
    }

    private static func parseSRTOrVTT(_ content: String) -> [Sentence] {
        let lines = splitLines(content)
        let timing = "^\\s*(\(timestamp))\\s*-->\\s*(\(timestamp))"
        var results: [Sentence] = []
        var i = 0
        while i < lines.count {
            let line = lines[i].trimmingCharacters(in: .whitespaces)
            if line.isEmpty || line.range(of: "WEBVTT", options: [.caseInsensitive, .anchored]) != nil {
                i += 1
                continue
            }
            if line.hasPrefix("NOTE") || line.hasPrefix("STYLE") {
                i += 1
                while i < lines.count, !isBlank(lines[i]) { i += 1 }
                continue
            }
            var match = captures(timing, in: line)
            // An optional cue index/identifier line precedes the timing line.
            if match == nil, i + 1 < lines.count, let next = captures(timing, in: lines[i + 1].trimmingCharacters(in: .whitespaces)) {
                match = next
                i += 1
            }
            guard let times = match else {
                i += 1
                continue
            }
            let start = parseTimestamp(times[0])
            let end = parseTimestamp(times[1])
            i += 1
            var textLines: [String] = []
            while i < lines.count, !isBlank(lines[i]) {
                let cleaned = cleanMarkup(lines[i].trimmingCharacters(in: .whitespaces))
                if !cleaned.isEmpty { textLines.append(cleaned) }
                i += 1
            }
            let text = textLines.joined(separator: " ").trimmingCharacters(in: .whitespaces)
            if !text.isEmpty, end >= start {
                results.append(sentence(id: results.count + 1, start: start, end: end, text: text))
            }
        }
        return results
    }

    private static func parseASS(_ content: String) -> [Sentence] {
        var results: [Sentence] = []
        var inEvents = false
        var startIndex = 1, endIndex = 2, textIndex = 9, fieldCount = 10
        for raw in splitLines(content) {
            let line = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if line.hasPrefix("["), line.hasSuffix("]") {
                inEvents = line.caseInsensitiveCompare("[Events]") == .orderedSame
                continue
            }
            guard inEvents else { continue }
            if let format = payload(of: line, after: "Format:") {
                let fields = format.split(separator: ",", omittingEmptySubsequences: false)
                    .map { $0.trimmingCharacters(in: .whitespaces).lowercased() }
                fieldCount = fields.count
                if let index = fields.firstIndex(of: "start") { startIndex = index }
                if let index = fields.firstIndex(of: "end") { endIndex = index }
                if let index = fields.firstIndex(of: "text") { textIndex = index }
                continue
            }
            guard let dialogue = payload(of: line, after: "Dialogue:") else { continue }
            // The text field may itself contain commas, so only the first `fieldCount - 1` commas separate fields.
            let parts = dialogue.trimmingCharacters(in: .whitespaces)
                .split(separator: ",", maxSplits: max(fieldCount - 1, 0), omittingEmptySubsequences: false)
            guard parts.count > max(startIndex, endIndex, textIndex) else { continue }
            let start = parseTimestamp(String(parts[startIndex]))
            let end = parseTimestamp(String(parts[endIndex]))
            let text = cleanMarkup(String(parts[textIndex])
                .replacingOccurrences(of: "\\N", with: " ")
                .replacingOccurrences(of: "\\n", with: " ")
                .replacingOccurrences(of: "\\h", with: " "))
            if !text.isEmpty, end >= start {
                results.append(sentence(id: results.count + 1, start: start, end: end, text: text))
            }
        }
        return results
    }

    // MARK: Helpers

    private static func sentence(id: Int, start: Double, end: Double, text: String) -> Sentence {
        Sentence(id: id, start: round3(start), end: round3(max(end, start + 0.05)), text: text)
    }

    private static func round3(_ value: Double) -> Double {
        (value * 1000).rounded(.toNearestOrEven) / 1000
    }

    private static func splitLines(_ content: String) -> [String] {
        content.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
    }

    private static func isBlank(_ line: String) -> Bool {
        line.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The text after a case-insensitive `prefix` (e.g. `Dialogue:`), or `nil` when the line doesn't start with it.
    private static func payload(of line: String, after prefix: String) -> String? {
        guard let range = line.range(of: prefix, options: [.caseInsensitive, .anchored]) else { return nil }
        return String(line[range.upperBound...])
    }

    /// Capture groups of the first match of `pattern` in `text`, or `nil`.
    private static func captures(_ pattern: String, in text: String) -> [String]? {
        guard let regex = try? NSRegularExpression(pattern: pattern),
              let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text))
        else { return nil }
        return (1..<match.numberOfRanges).compactMap { Range(match.range(at: $0), in: text).map { String(text[$0]) } }
    }

    /// Strips ASS `{…}` overrides and HTML-like `<…>` tags and collapses whitespace.
    private static func cleanMarkup(_ text: String) -> String {
        text.replacingOccurrences(of: #"\{[^}]*\}"#, with: "", options: .regularExpression)
            .replacingOccurrences(of: "<[^>]+>", with: "", options: .regularExpression)
            .replacingOccurrences(of: #"\s+"#, with: " ", options: .regularExpression)
            .trimmingCharacters(in: .whitespaces)
    }

    /// `hh:mm:ss,mmm`, `h:mm:ss.cc` or `mm:ss.mmm` to seconds; `0` when malformed.
    private static func parseTimestamp(_ raw: String) -> Double {
        let parts = raw.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: ",", with: ".")
            .split(separator: ":", omittingEmptySubsequences: false)
        if parts.count == 3, let h = Int(parts[0]), let m = Int(parts[1]), let s = Double(parts[2]) {
            return Double(h * 3600 + m * 60) + s
        }
        if parts.count == 2, let m = Int(parts[0]), let s = Double(parts[1]) {
            return Double(m * 60) + s
        }
        return 0
    }
}
