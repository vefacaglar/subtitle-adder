import Foundation
import Testing
@testable import EngineKit

@Suite struct SubtitleParserTests {
    @Test func parsesSRT() {
        let srt = """
        1
        00:00:01,000 --> 00:00:02,500
        <i>Merhaba</i> dünya.
        İkinci satır.

        2
        00:00:03,000 --> 00:00:04,000
        - Evet.

        """
        let lines = SubtitleParser.parse(srt, extension: "srt")
        #expect(lines.count == 2)
        #expect(lines[0].id == 1)
        #expect(lines[0].start == 1.0)
        #expect(lines[0].end == 2.5)
        #expect(lines[0].text == "Merhaba dünya. İkinci satır.")
        #expect(lines[1].id == 2)
        #expect(lines[1].text == "- Evet.")
    }

    @Test func srtIndexIsOptionalAndCRLFWorks() {
        let srt = "00:00:01,000 --> 00:00:02,000\r\nA\r\n\r\n00:00:03,000 --> 00:00:04,000\r\nB\r\n"
        #expect(SubtitleParser.parse(srt, extension: ".SRT").map(\.text) == ["A", "B"])
    }

    @Test func parsesVTTWithCueSettingsNotesAndShortTimestamps() {
        let vtt = """
        WEBVTT

        NOTE this is a comment
        still the comment

        STYLE
        ::cue { color: red }

        intro
        00:01.000 --> 00:02.500 align:start
        First VTT cue

        01:00:00.250 --> 01:00:01.000
        <v Roger>Late cue</v>
        """
        let lines = SubtitleParser.parse(vtt, extension: "vtt")
        #expect(lines.map(\.text) == ["First VTT cue", "Late cue"])
        #expect(lines[0].start == 1.0)
        #expect(lines[0].end == 2.5)
        #expect(lines[1].start == 3600.25)
    }

    @Test func parsesASSWithFormatLineOverridesAndCommasInText() {
        let ass = """
        [Script Info]
        Title: x

        [Events]
        Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
        Dialogue: 0,0:00:05.00,0:00:07.50,Default,,0,0,0,,{\\i1}Styled{\\i0}\\NASS line, with comma
        Comment: 0,0:00:08.00,0:00:09.00,Default,,0,0,0,,ignored
        Dialogue: 0,0:00:10.00,0:00:11.00,Default,,0,0,0,,a\\hb\\nc
        """
        let lines = SubtitleParser.parse(ass, extension: "ass")
        #expect(lines.map(\.text) == ["Styled ASS line, with comma", "a b c"])
        #expect(lines[0].start == 5.0)
        #expect(lines[0].end == 7.5)
    }

    @Test func assHonorsCustomFieldOrder() {
        let ass = "[Events]\nFormat: Start, End, Text\nDialogue: 0:00:01.00,0:00:02.00,Hello\n"
        let lines = SubtitleParser.parse(ass, extension: "ssa")
        #expect(lines.count == 1)
        #expect(lines[0].start == 1.0)
        #expect(lines[0].text == "Hello")
    }

    @Test func contentIsSniffedWithoutExtension() {
        #expect(SubtitleParser.parse("[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,S,,0,0,0,,Hi").count == 1)
        #expect(SubtitleParser.parse("00:00:01,000 --> 00:00:02,000\nHi").count == 1)
        let json = #"[{"id": 1, "start": 0.0, "end": 1.0, "text": "Selam", "words": []}]"#
        #expect(SubtitleParser.parse(json).map(\.text) == ["Selam"])
    }

    @Test func parsesEngineJSONWithSpeakerAndWords() {
        let json = """
        [{"id": 1, "start": 0.5, "end": 1.5, "text": "Merhaba.", "speaker": 1,
          "words": [{"start": 0.5, "end": 1.5, "text": "Merhaba."}]}]
        """
        let lines = SubtitleParser.parse(json, extension: "json")
        #expect(lines.count == 1)
        #expect(lines[0].speaker == 1)
        #expect(lines[0].words.count == 1)
        #expect(SubtitleParser.parse("{broken", extension: "json").isEmpty)
    }

    @Test func endBeforeStartIsDroppedAndZeroLengthGetsMinimumDuration() {
        let srt = "00:00:05,000 --> 00:00:04,000\nBackwards\n\n00:00:06,000 --> 00:00:06,000\nInstant\n"
        let lines = SubtitleParser.parse(srt, extension: "srt")
        #expect(lines.map(\.text) == ["Instant"])
        #expect(lines[0].end == 6.05)
    }

    @Test func emptyAndGarbageGiveNoLines() {
        #expect(SubtitleParser.parse("", extension: "srt").isEmpty)
        #expect(SubtitleParser.parse("  \n\n", extension: "vtt").isEmpty)
        #expect(SubtitleParser.parse("just text\nno timing", extension: "srt").isEmpty)
    }

    // MARK: Encodings

    private let sample = "00:00:01,000 --> 00:00:02,000\nÇağrı şığ İstanbul\n"

    @Test func decodesBOMEncodings() {
        var utf8 = Data([0xEF, 0xBB, 0xBF])
        utf8.append(Data(sample.utf8))
        #expect(SubtitleParser.parse(SubtitleParser.decode(utf8), extension: "srt").first?.text == "Çağrı şığ İstanbul")

        let utf16le = Data([0xFF, 0xFE]) + sample.data(using: .utf16LittleEndian)!
        #expect(SubtitleParser.decode(utf16le) == sample)
        let utf16be = Data([0xFE, 0xFF]) + sample.data(using: .utf16BigEndian)!
        #expect(SubtitleParser.decode(utf16be) == sample)
    }

    @Test func fallsBackToWindows1254WhenNotUTF8() throws {
        let data = try #require(sample.data(using: .windowsCP1254))
        #expect(String(data: data, encoding: .utf8) == nil)
        #expect(SubtitleParser.decode(data) == sample)
        #expect(SubtitleParser.decode(Data(sample.utf8)) == sample)
        #expect(SubtitleParser.decode(Data()) == "")
    }

    @Test func parseFileReadsFromDisk() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appending(path: "a.srt")
        try #require(sample.data(using: .windowsCP1254)).write(to: url)
        let lines = try SubtitleParser.parseFile(at: url)
        #expect(lines.map(\.text) == ["Çağrı şığ İstanbul"])
    }

    // MARK: Sidecar lookup

    private func makeFolder() throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appending(path: "parser-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    @Test func findsSidecarInExtensionOrderAndIgnoresLanguageSuffixes() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let video = folder.appending(path: "Film.mp4")
        try Data().write(to: video)
        #expect(SubtitleParser.findSidecar(for: video) == nil)

        // An exported translation (language-suffixed) never counts as the source subtitle.
        try Data().write(to: folder.appending(path: "Film.es.srt"))
        try Data().write(to: folder.appending(path: "Other.srt"))
        #expect(SubtitleParser.findSidecar(for: video) == nil)

        try Data().write(to: folder.appending(path: "Film.json"))
        #expect(SubtitleParser.findSidecar(for: video)?.lastPathComponent == "Film.json")
        try Data().write(to: folder.appending(path: "Film.ass"))
        #expect(SubtitleParser.findSidecar(for: video)?.lastPathComponent == "Film.ass")
        try Data().write(to: folder.appending(path: "Film.srt"))
        #expect(SubtitleParser.findSidecar(for: video)?.lastPathComponent == "Film.srt")
    }

    @Test func recognizesSubtitleExtensions() {
        for name in ["a.srt", "a.VTT", "a.ass", "a.ssa", "a.json"] {
            #expect(SubtitleParser.isSubtitleFile(URL(filePath: "/x/\(name)")))
        }
        #expect(!SubtitleParser.isSubtitleFile(URL(filePath: "/x/a.mp4")))
        #expect(!SubtitleParser.isSubtitleFile(URL(filePath: "/x/a")))
    }
}
