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

    @Test func aFailedGenerationIsNotCachedAndTriesAgainLater() async {
        let calls = Calls()
        let service = ThumbnailService(generate: { _ in
            calls.note("generate")
            return nil
        })
        #expect(await service.data(for: url("broken.mkv")) == nil)
        #expect(await service.data(for: url("broken.mkv")) == nil)
        #expect(calls.count("generate") == 2)
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

        #expect(await model.register(username: "Ali", password: "pw"))
        model.thumbnails.reset()
        #expect(await model.thumbnails.data(for: video) == picture)
        #expect(calls.count("generate") == 2)
        // In the vault now (the store keeps it encrypted), so a fresh cache reads it back instead of generating.
        #expect(model.store.thumbnail(for: video) == picture)
        model.thumbnails.reset()
        #expect(await model.thumbnails.data(for: video) == picture)
        #expect(calls.count("generate") == 2)
        #expect(!calls.anyOnMainThread)
    }

    @Test func storedPicturesTravelWithTheFeedWithoutWorkOnCardDraw() async throws {
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
        // The feed seeded the cache from the records, so drawing the card finds the picture at once.
        #expect(model.thumbnails.cached(for: video.path) == Data([4, 5, 6]))
        #expect(calls.count("generate") == 0)
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
        let files = try FileManager.default.contentsOfDirectory(at: sources, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
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
