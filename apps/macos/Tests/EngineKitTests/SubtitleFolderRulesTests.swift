import Foundation
import Testing
@testable import EngineKit

/// The folder-listing rules next to `findSidecar(for:)`: the badge extension and the companion video lookup.
@Suite struct SubtitleFolderRulesTests {
    // MARK: Badge extension

    @Test func badgeFollowsExtensionOrderNotListingOrder() {
        let names = ["Film.json", "Film.ssa", "Film.vtt", "Film.mp4"]
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: names) == "vtt")
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: names + ["Film.srt"]) == "srt")
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: ["Film.json", "Film.ssa"]) == "ssa")
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: ["Film.json"]) == "json")
    }

    @Test func badgeAcceptsLanguageTaggedFiles() {
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: ["Film.tr.srt"]) == "srt")
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: ["Film.tr.forced.vtt"]) == "vtt")
        // A tagged srt still beats a plain vtt: only the extension order decides.
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: ["Film.vtt", "Film.es.srt"]) == "srt")
    }

    @Test func badgeIgnoresOtherStemsAndOtherExtensions() {
        let names = ["Film2.srt", "Film 2.srt", "Filmed.vtt", "Other.srt", "Film.txt", "Film.srt.bak", "Film", "srt", ".srt"]
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: names) == nil)
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "", in: ["a.srt", ".srt", "..srt"]) == nil)
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: []) == nil)
        // An empty middle part (`Film..srt`) is not a tag.
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: ["Film..srt"]) == nil)
    }

    @Test func badgeIsCaseInsensitiveAndReturnsLowerCase() {
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "FILM", in: ["film.SRT"]) == "srt")
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "film", in: ["FILM.Tr.Ass"]) == "ass")
    }

    @Test func badgeComparesNamesAfterNFCNormalization() {
        let nfc = "Şarkı Çalışması"
        let nfd = nfc.decomposedStringWithCanonicalMapping
        #expect(nfc.unicodeScalars.count != nfd.unicodeScalars.count)
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: nfc, in: [nfd + ".srt"]) == "srt")
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: nfd, in: [nfc + ".tr.vtt"]) == "vtt")
    }

    @Test func lookupAnswersManyStemsFromOneListing() {
        let lookup = SubtitleParser.SubtitleBadgeLookup(fileNames: ["A.srt", "B.tr.vtt", "C.mp4", "notes.txt"])
        #expect(lookup.badgeExtension(forVideoStem: "A") == "srt")
        #expect(lookup.badgeExtension(forVideoStem: "B") == "vtt")
        #expect(lookup.badgeExtension(forVideoStem: "C") == nil)
        #expect(lookup.badgeExtension(forVideoStem: "notes") == nil)
    }

    @Test func badgeAndSidecarAutoLoadRulesStayDistinct() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let video = folder.appending(path: "Film.mp4")
        try touch("Film.mp4", "Film.tr.srt", in: folder)
        let names = try FileManager.default.contentsOfDirectory(atPath: folder.path)
        #expect(SubtitleParser.subtitleBadgeExtension(forVideoStem: "Film", in: names) == "srt")
        #expect(SubtitleParser.findSidecar(for: video) == nil)
    }

    // MARK: Companion video

    private func makeFolder() throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appending(path: "companion-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    private func touch(_ names: String..., in folder: URL) throws {
        for name in names { try Data().write(to: folder.appending(path: name)) }
    }

    @Test func companionFindsTheSameStem() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        try touch("Episode01.mkv", "Episode01.srt", "Other.mp4", in: folder)
        #expect(SubtitleParser.findCompanionVideo(for: folder.appending(path: "Episode01.srt"))?.lastPathComponent == "Episode01.mkv")
        #expect(SubtitleParser.findCompanionVideo(for: folder.appending(path: "Lonely.srt")) == nil)
    }

    @Test func companionStripsOneLanguageSuffix() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        try touch("Episode01.mp4", "Show.S01E02.mkv", in: folder)
        func found(_ name: String) -> String? {
            SubtitleParser.findCompanionVideo(for: folder.appending(path: name))?.lastPathComponent
        }
        #expect(found("Episode01.tr.srt") == "Episode01.mp4")   // ".tr" is 3 characters
        #expect(found("Episode01.zh-CN.srt") == "Episode01.mp4")   // ".zh-CN" is 6
        #expect(found("Episode01.forced.srt") == nil)   // ".forced" is 7: not a language tag
        #expect(found("Episode01.t.srt") == nil)   // ".t" is 2
        // The full stem is tried first, and only one suffix is stripped.
        #expect(found("Show.S01E02.srt") == "Show.S01E02.mkv")
        #expect(found("Episode01.tr.forced.srt") == nil)
        #expect(found(".tr.srt") == nil)
    }

    @Test func companionPrefersExactStemOverStrippedOne() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        try touch("Film.tr.mp4", "Film.mkv", in: folder)
        #expect(SubtitleParser.findCompanionVideo(for: folder.appending(path: "Film.tr.srt"))?.lastPathComponent == "Film.tr.mp4")
    }

    @Test func companionIsCaseInsensitiveAndKeepsTheRealName() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        try touch("EPISODE01.MP4", in: folder)
        let found = SubtitleParser.findCompanionVideo(for: folder.appending(path: "episode01.tr.srt"))
        #expect(found?.lastPathComponent == "EPISODE01.MP4")
        #expect(found.map { FileManager.default.fileExists(atPath: $0.path) } == true)
    }

    @Test func companionTriesExtensionsInMediaOrderAndAcceptsAudio() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        try touch("Talk.mp3", "Talk.mkv", in: folder)
        #expect(SubtitleParser.findCompanionVideo(for: folder.appending(path: "Talk.srt"))?.lastPathComponent == "Talk.mkv")
        try FileManager.default.removeItem(at: folder.appending(path: "Talk.mkv"))
        #expect(SubtitleParser.findCompanionVideo(for: folder.appending(path: "Talk.srt"))?.lastPathComponent == "Talk.mp3")
    }

    @Test func companionComparesNamesAfterNFCNormalization() throws {
        let folder = try makeFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let nfc = "Çalışma"
        try touch(nfc + ".mp4", in: folder)
        let found = SubtitleParser.findCompanionVideo(for: folder.appending(path: nfc.decomposedStringWithCanonicalMapping + ".srt"))
        #expect(found != nil)
    }

    @Test func companionIgnoresMissingFolder() {
        let missing = FileManager.default.temporaryDirectory.appending(path: "nope-\(UUID().uuidString)/Film.srt")
        #expect(SubtitleParser.findCompanionVideo(for: missing) == nil)
    }
}
