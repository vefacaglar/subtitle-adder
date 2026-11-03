import AVFoundation
import AppKit
import EngineKit
import Foundation
import LibraryKit
import Synchronization
import Testing
@testable import SubtitleAdderApp

/// The card pictures: one piece of work per video, the vault in front of the generator, nothing on the main thread.
@MainActor
@Suite(.serialized) struct ThumbnailServiceTests {
    private final class Calls: Sendable {
        private let counts = Mutex<[String: Int]>([:])
        private let threads = Mutex<[Bool]>([])
        func note(_ name: String) {
            counts.withLock { $0[name, default: 0] += 1 }
            threads.withLock { $0.append(Thread.isMainThread) }
        }
        func count(_ name: String) -> Int { counts.withLock { $0[name] ?? 0 } }
        var anyOnMainThread: Bool { threads.withLock { $0.contains(true) } }
    }

    private let picture = Data([0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3])
    private func url(_ name: String) -> URL { URL(filePath: "/tmp/thumbnail-tests/\(name)") }

    @Test func concurrentRequestsShareOneGeneration() async {
        let calls = Calls()
        let picture = picture
        let service = ThumbnailService(
            write: { _, _ in calls.note("write") },
            generate: { url in
                calls.note("generate")
                try? await Task.sleep(for: .milliseconds(100))
                return picture
            })
        let target = url("a.mp4")
        async let first = service.data(for: target)
        async let second = service.data(for: target)
        async let third = service.data(for: target)
        let results = await [first, second, third]
        #expect(results.allSatisfy { $0 == picture })
        #expect(calls.count("generate") == 1)
        #expect(calls.count("write") == 1)
        #expect(!calls.anyOnMainThread)

        // From then on it is in memory.
        #expect(service.cached(for: target.path) == picture)
        #expect(await service.data(for: target) == picture)
        #expect(calls.count("generate") == 1)
    }

    @Test func aStoredPictureIsUsedInsteadOfGenerating() async {
        let calls = Calls()
        let stored = Data([9, 9, 9])
        let service = ThumbnailService(
            read: { _ in stored }, write: { _, _ in calls.note("write") },
            generate: { _ in
                calls.note("generate")
                return nil
            })
        #expect(await service.data(for: url("b.mp4")) == stored)
        #expect(calls.count("generate") == 0)
        #expect(calls.count("write") == 0)
    }

    @Test func aSeededPictureNeedsNoWorkAtAll() async {
        let calls = Calls()
        let service = ThumbnailService(
            read: { _ in
                calls.note("read")
                return nil
            },
            generate: { _ in
                calls.note("generate")
                return nil
            })
        service.seed(Data([1]), for: "/Movies/Film.mp4")
        // The key ignores case, like the store's path comparison.
        #expect(await service.data(for: URL(filePath: "/movies/film.mp4")) == Data([1]))
        #expect(calls.count("read") == 0)
        #expect(calls.count("generate") == 0)
    }

    @Test func audioFilesGetNoPictureAndCostNoGeneration() async {
        let calls = Calls()
        let service = ThumbnailService(generate: { _ in
            calls.note("generate")
            return Data([1])
        })
        #expect(await service.data(for: url("song.mp3")) == nil)
        #expect(calls.count("generate") == 0)
    }

    @Test func aFailedGenerationIsRememberedUntilTheFileChanges() async throws {
        let calls = Calls()
        let service = ThumbnailService(generate: { _ in
            calls.note("generate")
            return nil
        })
        let folder = FileManager.default.temporaryDirectory.appending(path: "thumb-neg-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let broken = folder.appending(path: "broken.mkv")
        try Data("not a video".utf8).write(to: broken)

        // Scrolling past the same card again and again costs one attempt, not one per pass.
        #expect(await service.data(for: broken) == nil)
        #expect(await service.data(for: broken) == nil)
        #expect(await service.data(for: broken) == nil)
        #expect(calls.count("generate") == 1)

        // The file was replaced: it gets another chance.
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(60)], ofItemAtPath: broken.path)
        #expect(await service.data(for: broken) == nil)
        #expect(calls.count("generate") == 2)
    }

    @Test func aResetGivesFailedFilesAFreshStartToo() async throws {
        let calls = Calls()
        let service = ThumbnailService(generate: { _ in
            calls.note("generate")
            return nil
        })
        // A file that is not there has no date to compare, and is remembered as failed just the same.
        #expect(await service.data(for: url("missing.mkv")) == nil)
        #expect(await service.data(for: url("missing.mkv")) == nil)
        #expect(calls.count("generate") == 1)
    }

    @Test func workStartedBeforeAResetDoesNotFillTheNewCache() async throws {
        let picture = picture
        let service = ThumbnailService(generate: { _ in
            try? await Task.sleep(for: .milliseconds(150))
            return picture
        })
        let target = url("slow.mp4")
        let work = Task { await service.data(for: target) }
        try await Task.sleep(for: .milliseconds(50))
        service.reset()
        // The caller still gets its picture, but another profile's cache does not.
        #expect(await work.value == picture)
        #expect(service.cached(for: target.path) == nil)
    }

    @Test func aFullCacheDropsTheLeastRecentlyUsedQuarterNotEverything() {
        let service = ThumbnailService()
        let capacity = ThumbnailService.capacity
        for index in 0..<capacity { service.seed(Data([1]), for: "/v/\(index)") }
        #expect(service.cachedCount == capacity)
        // Using the oldest one makes it the newest.
        #expect(service.cached(for: "/v/0") != nil)
        service.seed(Data([2]), for: "/v/new")
        #expect(service.cachedCount == capacity - capacity / 4 + 1)
        #expect(service.cached(for: "/v/0") != nil)
        #expect(service.cached(for: "/v/1") == nil)
        #expect(service.cached(for: "/v/new") != nil)
        #expect(service.cached(for: "/v/\(capacity - 1)") != nil)
    }

    @MainActor @Test func decodedPicturesAreBoundedByMemoryAsWellAsCount() {
        #expect(CardImageCache.costLimit == 48 * 1024 * 1024)
        let small = NSImage(size: NSSize(width: 10, height: 10))
        let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 384, pixelsHigh: 216, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
            isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        let big = NSImage(size: NSSize(width: 192, height: 108))
        big.addRepresentation(rep)
        #expect(CardImageCache.cost(of: big) == 384 * 216 * 4)
        #expect(CardImageCache.cost(of: small) > 0)
        // A 4K frame costs what it decodes to, far more than one card picture.
        let huge = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 3840, pixelsHigh: 2160, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
            isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        let image = NSImage(size: NSSize(width: 192, height: 108))
        image.addRepresentation(huge)
        #expect(CardImageCache.cost(of: image) > CardImageCache.costLimit / 2)
    }

    @Test func aCardThatScrollsAwayBeforeItsTurnCostsNothing() async throws {
        let calls = Calls()
        let release = AsyncGate(limit: 1)
        _ = await release.acquire()
        let picture = picture
        // One generation at a time: the first holds the gate, so the second only waits.
        let service = ThumbnailService(
            generate: { url in
                calls.note(url.lastPathComponent)
                _ = await release.acquire()
                await release.release()
                return picture
            }, concurrency: 1)
        let first = Task { await service.data(for: url("first.mp4")) }
        try await Task.sleep(for: .milliseconds(100))
        #expect(calls.count("first.mp4") == 1)
        let second = Task { await service.data(for: url("second.mp4")) }
        try await Task.sleep(for: .milliseconds(50))
        second.cancel()
        _ = await second.value
        await release.release()
        #expect(await first.value == picture)
        try await Task.sleep(for: .milliseconds(100))
        #expect(calls.count("second.mp4") == 0)
    }

    @Test func theModelStoresGeneratedPicturesOnlyWhileLoggedIn() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let calls = Calls()
        let picture = picture
        let model = sandbox.model(thumbnailGenerator: { _ in
            calls.note("generate")
            return picture
        })
        await model.start()

        // Logged out: a picture, but nowhere to keep it.
        #expect(await model.thumbnails.data(for: video) == picture)
        #expect(model.store.thumbnail(for: video) == nil)

        // Logging in empties the memory cache by itself: the picture above belonged to nobody.
        #expect(await model.register(username: "Ali", password: "pw"))
        #expect(model.thumbnails.cached(for: video.path) == nil)
        #expect(await model.thumbnails.data(for: video) == picture)
        #expect(calls.count("generate") == 2)
        // In the profile's picture cache now (encrypted), so a fresh memory cache reads it back instead of generating.
        #expect(model.store.thumbnail(for: video) == picture)
        model.thumbnails.reset()
        #expect(await model.thumbnails.data(for: video) == picture)
        #expect(calls.count("generate") == 2)
        #expect(!calls.anyOnMainThread)
        // Storing it made no record: the video was only looked at.
        #expect(model.store.stats(for: video) == nil)
    }

    @Test func loggingOutAndDeletingAProfileClearThePicturesInMemory() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let picture = picture
        let model = sandbox.model(thumbnailGenerator: { _ in picture })
        await model.start()
        #expect(await model.register(username: "Ali", password: "pw"))
        #expect(await model.thumbnails.data(for: video) == picture)
        CardImageCache.store(NSImage(size: NSSize(width: 4, height: 4)), for: video.path)
        #expect(model.thumbnails.cached(for: video.path) != nil)

        model.logout()
        await model.settle()
        #expect(!model.isLoggedIn)
        #expect(model.thumbnails.cached(for: video.path) == nil)
        #expect(CardImageCache.image(for: video.path) == nil)

        #expect(await model.login(username: "Ali", password: "pw"))
        #expect(await model.thumbnails.data(for: video) == picture)
        CardImageCache.store(NSImage(size: NSSize(width: 4, height: 4)), for: video.path)
        #expect(await model.deleteProfile(username: "Ali", password: "pw"))
        #expect(model.thumbnails.cached(for: video.path) == nil)
        #expect(CardImageCache.image(for: video.path) == nil)
    }

    @Test func generatingManyPicturesNeverTouchesTheProfileFileOrMakesRecords() async throws {
        let sandbox = try PlatformSandbox()
        let picture = Data(repeating: 0xAB, count: 30_000)
        let model = sandbox.model(thumbnailGenerator: { _ in picture })
        await model.start()
        #expect(await model.register(username: "Ali", password: "pw"))
        let before = try Data(contentsOf: sandbox.profilesFile)
        var videos: [URL] = []
        for index in 0..<40 { videos.append(try sandbox.file("v\(index).mp4")) }
        for video in videos { #expect(await model.thumbnails.data(for: video) == picture) }
        // The write is queued behind the generation; wait until the last picture is stored.
        let last = try #require(videos.last)
        #expect(await waitFor { model.store.thumbnail(for: last) == picture })
        #expect(try Data(contentsOf: sandbox.profilesFile) == before)
        #expect(model.store.feed(.all).isEmpty)
        #expect(model.store.totals() == .zero)
    }

    @Test func aStoredPictureIsReadWhereTheCardIsDrawnWithoutGenerating() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4")
        let calls = Calls()
        let model = sandbox.model(thumbnailGenerator: { _ in
            calls.note("generate")
            return nil
        })
        model.isHubActive = true
        await model.start()
        #expect(await model.register(username: "Ali", password: "pw"))
        _ = try model.store.setThumbnail(Data([4, 5, 6]), for: video)
        model.playerVideoChanged(to: video, countsAsOpen: true)
        await model.settle()
        model.filter = .history
        await model.settle()
        // The feed does not carry pictures (a vault with hundreds would make every refresh heavy): the card asks.
        #expect(await model.thumbnails.data(for: video) == Data([4, 5, 6]))
        #expect(calls.count("generate") == 0)
        #expect(!calls.anyOnMainThread)
    }

    @Test func loggedOutNeitherReadsNorFingerprintsFilesForPictures() async throws {
        let sandbox = try PlatformSandbox()
        let video = try sandbox.file("clip.mp4", content: "unique content \(UUID().uuidString)")
        let picture = picture
        let model = sandbox.model(thumbnailGenerator: { _ in picture })
        await model.start()
        #expect(await model.thumbnails.data(for: video) == picture)
        // The generator made a picture; the profile code did not open the file to look for one.
        #expect(VideoFingerprint.cachedFingerprint(ofNormalizedPath: VideoFingerprint.normalizedPath(video)) == nil)
    }

    // MARK: Real pictures

    @Test func theGeneratorMakesASmallJpegFromARealVideo() async throws {
        let video = try await Self.makeVideo(width: 640, height: 480, seconds: 2)
        defer { try? FileManager.default.removeItem(at: video) }
        let data = try #require(await ThumbnailGenerator.jpegData(for: video))
        #expect(data.count < 512 * 1024)
        #expect(data.starts(with: [0xFF, 0xD8, 0xFF]))
        let image = try #require(NSImage(data: data))
        let rep = try #require(image.representations.first)
        #expect(rep.pixelsWide <= 384 && rep.pixelsHigh <= 216)
        #expect(rep.pixelsWide > 0 && rep.pixelsHigh > 0)
    }

    @Test func theGeneratorGivesNothingForAudioOrNonsense() async throws {
        #expect(await ThumbnailGenerator.jpegData(for: URL(filePath: "/tmp/none.mp3")) == nil)
        let fake = FileManager.default.temporaryDirectory.appending(path: "not-a-video-\(UUID().uuidString).mp4")
        try Data("not a video".utf8).write(to: fake)
        defer { try? FileManager.default.removeItem(at: fake) }
        #expect(await ThumbnailGenerator.jpegData(for: fake) == nil)
    }

    /// A short H.264 movie of a gradient, made with `AVAssetWriter`.
    private static func makeVideo(width: Int, height: Int, seconds: Int) async throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "clip-\(UUID().uuidString).mov")
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(
            mediaType: .video,
            outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: width, AVVideoHeightKey: height])
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: input,
            sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: width, kCVPixelBufferHeightKey as String: height,
            ])
        writer.add(input)
        writer.startWriting()
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<(seconds * 10) {
            while !input.isReadyForMoreMediaData { try await Task.sleep(for: .milliseconds(5)) }
            var buffer: CVPixelBuffer?
            CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, nil, &buffer)
            let pixels = try #require(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            memset(CVPixelBufferGetBaseAddress(pixels), Int32(40 + frame * 4 % 200), CVPixelBufferGetDataSize(pixels))
            CVPixelBufferUnlockBaseAddress(pixels, [])
            adaptor.append(pixels, withPresentationTime: CMTime(value: CMTimeValue(frame), timescale: 10))
        }
        input.markAsFinished()
        await writer.finishWriting()
        try #require(writer.status == .completed)
        return url
    }
}

/// Every string the platform shows has both languages, and the workspace tabs have their new names.
@MainActor
@Suite struct PlatformTextTests {
    @Test func tabsAndMessagesAreTranslated() {
        #expect(UILanguage.en.string("Çeviri listesi", in: .module) == "Translation queue")
        #expect(UILanguage.en.string("Çeviri listesi (%lld)", in: .module, [Int(3)]) == "Translation queue (3)")
        #expect(UILanguage.tr.string("Çeviri listesi (%lld)", in: .module, [Int(3)]) == "Çeviri listesi (3)")
        #expect(UILanguage.en.string("Player", in: .module) == "Player")
        #expect(UILanguage.en.string("Platform", in: .module) == "Platform")
        #expect(UILanguage.en.string("Henüz izlenmedi", in: .module) == "Not watched yet")
        #expect(UILanguage.en.string("%lld izlenen · %lld beğenilen", in: .module, [Int(2), Int(1)]) == "2 watched · 1 liked")
        #expect(UILanguage.en.string("Profil dosyası okunamıyor; değişiklikler kaydedilmedi.", in: .module)
            == "The profile file can't be read; changes weren't saved.")
        #expect(UILanguage.en.string("Kullanıcı adı veya şifre hatalı.", in: .module) == "Incorrect username or password.")
    }

    @Test func everyKeyInThePlatformSourcesIsInTheCatalogInBothLanguages() throws {
        let package = URL(filePath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let sources = package.appending(path: "Sources/SubtitleAdderApp/Platform")
        var files = try FileManager.default.contentsOfDirectory(at: sources, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
        // The window and the player host the platform too: their keys must be in the catalog the same way.
        for name in ["ContentView.swift", "PlayerView.swift"] {
            files.append(package.appending(path: "Sources/SubtitleAdderApp/\(name)"))
        }
        #expect(!files.isEmpty)
        let catalogURL = package.appending(path: "Sources/SubtitleAdderApp/Resources/Localizable.xcstrings")
        let root = try #require(JSONSerialization.jsonObject(with: Data(contentsOf: catalogURL)) as? [String: Any])
        let strings = try #require(root["strings"] as? [String: [String: Any]])

        let pattern = try NSRegularExpression(pattern: #"L\("((?:[^"\\]|\\.)*)""#)
        var checked = 0
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for match in pattern.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                let key = String(text[Range(match.range(at: 1), in: text)!]).replacingOccurrences(of: "\\\"", with: "\"")
                let localizations = strings[key]?["localizations"] as? [String: [String: [String: String]]]
                for language in ["tr", "en"] {
                    let value = localizations?[language]?["stringUnit"]?["value"]
                    #expect(value?.isEmpty == false, "\(file.lastPathComponent): \(key) has no \(language) text")
                }
                checked += 1
            }
        }
        #expect(checked > 60)
    }
}
