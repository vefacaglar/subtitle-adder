import Foundation
import Testing
@testable import LibraryKit

@Suite final class VaultContentsTests {
    let folder = TestFolder()
    let early = Date(timeIntervalSince1970: 1_700_000_000)
    let fingerprint = "fp:100:aaaaaaaaaaaaaaaaaaaaaaaa"

    private func record(
        _ path: String, views: Int = 0, liked: Bool = false, rating: Int = 0, position: Double = 0, duration: Double = 0,
        watched: Date? = nil, updated: Date, fingerprint: String? = nil
    ) -> VideoUserStats {
        VideoUserStats(
            contentFingerprint: fingerprint ?? self.fingerprint, videoPath: path, fileName: PathParts.fileName(of: path),
            directoryPath: PathParts.directory(of: path), viewCount: views, isLiked: liked, rating: rating,
            lastPositionSeconds: position, durationSeconds: duration, lastWatchedAt: watched, updatedAt: updated)
    }

    private func contents(_ videos: [VideoUserStats]) -> VaultContents {
        VaultContents(payload: VaultPayload(videos: videos, customLists: [], thumbnails: [:]))
    }

    // MARK: - Records that resolve to one key (L10)

    @Test func twoRecordsOfOneFileAreMergedInsteadOfTheLaterOverwritingTheEarlier() throws {
        let late = early.addingTimeInterval(100)
        let older = record("/a/one.mp4", views: 3, liked: true, rating: 2, position: 10, duration: 100, watched: early, updated: early)
        let newer = record("/b/two.mp4", views: 4, liked: false, rating: 5, position: 60, duration: 100, watched: late, updated: late)

        for payload in [[older, newer], [newer, older]] {
            let merged = contents(payload)
            #expect(merged.videos.count == 1)
            let stats = try #require(merged.videos[fingerprint])
            #expect(stats.viewCount == 7, "views add up")
            #expect(stats.isLiked, "liked if either was")
            #expect(stats.rating == 5, "the better rating")
            #expect(stats.lastWatchedAt == late)
            #expect(stats.lastPositionSeconds == 60, "the resume point of the latest watch")
            #expect(stats.durationSeconds == 100)
            #expect(stats.updatedAt == late)
            #expect(stats.videoPath == "/b/two.mp4", "identity from the record updated last")
            #expect(stats.contentFingerprint == fingerprint)
        }
    }

    @Test func bothOldPathsStillFindTheMergedRecord() throws {
        let merged = contents([
            record("/a/one.mp4", views: 1, updated: early),
            record("/b/two.mp4", views: 1, updated: early.addingTimeInterval(1)),
        ])
        for path in ["/a/one.mp4", "/b/two.mp4", "/B/TWO.mp4"] {
            let probe = try #require(VideoProbe(URL(filePath: path), mode: .pathOnly))
            #expect(merged.find(probe) == fingerprint, "\(path)")
        }
    }

    @Test func aWatchedRecordSuppliesTheResumePointOverAnUnwatchedNewerOne() {
        let watched = record("/a.mp4", views: 2, position: 40, duration: 90, watched: early, updated: early)
        let touched = record("/b.mp4", liked: true, position: 0, duration: 0, updated: early.addingTimeInterval(500))
        let stats = contents([watched, touched]).videos[fingerprint]
        #expect(stats?.lastPositionSeconds == 40)
        #expect(stats?.durationSeconds == 90)
        #expect(stats?.isLiked == true)
        #expect(stats?.viewCount == 2)
        #expect(stats?.videoPath == "/b.mp4")
    }

    @Test func threeRecordsMergeIntoOne() {
        let stats = contents([
            record("/1.mp4", views: 1, rating: 1, updated: early),
            record("/2.mp4", views: 2, liked: true, rating: 3, updated: early.addingTimeInterval(1)),
            record("/3.mp4", views: 4, rating: 2, updated: early.addingTimeInterval(2)),
        ]).videos[fingerprint]
        #expect(stats?.viewCount == 7)
        #expect(stats?.rating == 3)
        #expect(stats?.isLiked == true)
    }

    @Test func recordsOfDifferentFilesStaySeparateAndTheCaseOfAFingerprintDoesNotSplitOne() {
        let other = "fp:200:bbbbbbbbbbbbbbbbbbbbbbbb"
        #expect(contents([record("/1.mp4", updated: early), record("/2.mp4", updated: early, fingerprint: other)]).videos.count == 2)
        let upper = fingerprint.uppercased()
        #expect(contents([record("/1.mp4", views: 1, updated: early), record("/2.mp4", views: 1, updated: early, fingerprint: upper)]).videos.count == 1)
    }

    @Test func aMergedVaultWritesOneRecord() {
        let merged = contents([
            record("/a/one.mp4", views: 3, liked: true, updated: early),
            record("/b/two.mp4", views: 4, rating: 4, updated: early.addingTimeInterval(9)),
        ])
        #expect(merged.payload().videos.count == 1)
        #expect(merged.payload().videos.first?.viewCount == 7)
    }

    // MARK: - Loading reads no files

    @Test func loadingARecordWithoutAFingerprintDoesNotReadItsFile() throws {
        let file = folder.video("kept.mp4")
        try Data((0..<2000).map { UInt8($0 % 199) }).write(to: file)
        let path = normalized(file)
        let loaded = VaultContents(payload: VaultPayload(
            videos: [VideoUserStats(contentFingerprint: nil, videoPath: path, fileName: "", directoryPath: "", viewCount: 2, updatedAt: early)],
            customLists: [], thumbnails: [:]))

        let stats = try #require(loaded.videos[path.lowercased()], "keyed by path, as before its file was ever seen")
        #expect(stats.contentFingerprint == nil, "the file was not read to give it one")
        #expect(stats.fileName == "kept.mp4")

        // The next lookup of the file adds the fingerprint, which counts as a migration to be saved.
        var contents = loaded
        let probe = try #require(VideoProbe(file))
        let found = contents.resolve(probe, now: early)
        let resolution = try #require(found)
        #expect(resolution.migrated)
        #expect(resolution.stats.contentFingerprint == probe.fingerprint)
        #expect(resolution.stats.viewCount == 2)
        #expect(contents.resolve(probe, now: early)?.migrated == false)
    }

    @Test func resolvingNeverReadsTheRecordedPathItself() throws {
        // A copy and its original: only a fingerprint the store computed beforehand (outside its locks) can say the
        // recorded path still holds the same content. Without one, resolve treats the lookup as a move.
        let original = folder.video("original.mp4")
        try Data((0..<2000).map { UInt8($0 % 211) }).write(to: original)
        let copy = folder.video("copy.mp4")
        try FileManager.default.copyItem(at: original, to: copy)
        let originalProbe = try #require(VideoProbe(original))

        var contents = VaultContents()
        contents.setVideo(
            VideoUserStats(
                contentFingerprint: originalProbe.fingerprint, videoPath: originalProbe.path, fileName: "original.mp4",
                directoryPath: PathParts.directory(of: originalProbe.path), viewCount: 1, updatedAt: early),
            forKey: originalProbe.fingerprint!)

        let bare = try #require(VideoProbe(copy))
        #expect(contents.takeoverPath(for: bare) == originalProbe.path)
        var unprepared = contents
        #expect(unprepared.resolve(bare, now: early)?.migrated == true, "no prefetched fingerprint, so no disk access")

        let prepared = bare.withRecordedFingerprint(originalProbe.fingerprint!, forPath: originalProbe.path)
        var prefetched = contents
        #expect(prefetched.resolve(prepared, now: early)?.migrated == false, "the original still holds the content: a copy")
        #expect(contents.takeoverPath(for: originalProbe) == nil, "a lookup at the recorded path takes nothing over")
    }
}
