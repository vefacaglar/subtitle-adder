import AVFoundation
import AppKit
import EngineKit
import Foundation
import ImageIO
import LibraryKit
import Synchronization
import UniformTypeIdentifiers

/// The pictures of the video cards: an in-memory cache keyed by path, in front of the profile's encrypted picture cache,
/// in front of generating the picture from the file. Everything here runs off the main actor; callers get JPEG bytes and
/// decode them where they draw.
///
/// Two cards (or a card and a refresh) asking for the same video share one piece of work. When the last of them
/// goes away, as when a card scrolls out of view, the work is cancelled before it starts the expensive part.
final class ThumbnailService: Sendable {
    /// Reads the picture stored for a video; `nil` when there is none or nobody is logged in.
    typealias Reader = @Sendable (URL) async -> Data?
    /// Stores a generated picture; a no-op while logged out.
    typealias Writer = @Sendable (Data, URL) async -> Void
    /// Makes a picture from the file.
    typealias Generator = @Sendable (URL) async -> Data?

    private struct Flight {
        let id: UUID
        let task: Task<Data?, Never>
        var waiters: Int
    }

    private struct Picture {
        var data: Data
        /// The tick of the last use; the oldest are dropped first.
        var used: Int
    }

    /// A video whose picture could not be made, as it was when that was tried: the same file is not tried again until
    /// it changes.
    private struct Failure: Equatable {
        let modified: Date?
    }

    private struct State {
        var pictures: [String: Picture] = [:]
        var flights: [String: Flight] = [:]
        var failures: [String: Failure] = [:]
        var tick = 0
        /// Bumped by `reset()`; work that started before it does not put its result in the cache.
        var epoch = 0

        mutating func insert(_ data: Data, forKey key: String) {
            if pictures.count >= ThumbnailService.capacity, pictures[key] == nil {
                // Drop the quarter used least recently, not everything.
                let doomed = pictures.sorted { $0.value.used < $1.value.used }.prefix(ThumbnailService.capacity / 4)
                for (stale, _) in doomed { pictures[stale] = nil }
            }
            tick += 1
            pictures[key] = Picture(data: data, used: tick)
        }
    }

    /// Most pictures kept; a cache this size is a few tens of MB at most. Past it the least recently used quarter goes.
    static let capacity = 2000
    private static let failureCapacity = 4000

    private let state = Mutex(State())
    private let read: Reader
    private let write: Writer
    private let generate: Generator
    private let gate: AsyncGate

    init(
        read: @escaping Reader = { _ in nil }, write: @escaping Writer = { _, _ in },
        generate: @escaping Generator = ThumbnailGenerator.jpegData(for:), concurrency: Int = 2
    ) {
        self.read = read
        self.write = write
        self.generate = generate
        gate = AsyncGate(limit: concurrency)
    }

    static func key(_ path: String) -> String { path.lowercased() }

    /// Remembers a picture that is already known (it came with the profile's records).
    func seed(_ data: Data, for path: String) {
        guard !data.isEmpty else { return }
        state.withLock { $0.insert(data, forKey: Self.key(path)) }
    }

    func cached(for path: String) -> Data? {
        state.withLock { state in
            let key = Self.key(path)
            guard var picture = state.pictures[key] else { return nil }
            state.tick += 1
            picture.used = state.tick
            state.pictures[key] = picture
            return picture.data
        }
    }

    /// How many pictures are in memory.
    var cachedCount: Int { state.withLock { $0.pictures.count } }

    /// Forgets the pictures (a different profile's may be different). Work under way does not put its result back.
    func reset() {
        state.withLock { state in
            state.pictures.removeAll()
            state.epoch += 1
        }
    }

    private static func modificationDate(of url: URL) -> Date? {
        // Not `URL.resourceValues`: a URL caches what it was asked, and the date must be the file's now.
        (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
    }

    /// The picture for a video, `nil` when it has none (an audio file, a file that cannot be decoded).
    func data(for url: URL) async -> Data? {
        let key = Self.key(url.path)
        if let hit = cached(for: url.path) { return hit }
        let (id, task) = join(url, key: key)
        return await withTaskCancellationHandler {
            await task.value
        } onCancel: {
            leave(key: key, id: id)
        }
    }

    private func join(_ url: URL, key: String) -> (UUID, Task<Data?, Never>) {
        state.withLock { state in
            if var flight = state.flights[key] {
                flight.waiters += 1
                state.flights[key] = flight
                return (flight.id, flight.task)
            }
            let id = UUID()
            let task = Task { await self.produce(url, key: key, id: id) }
            state.flights[key] = Flight(id: id, task: task, waiters: 1)
            return (id, task)
        }
    }

    private func leave(key: String, id: UUID) {
        state.withLock { state in
            guard var flight = state.flights[key], flight.id == id else { return }
            flight.waiters -= 1
            if flight.waiters <= 0 {
                flight.task.cancel()
                state.flights[key] = nil
            } else {
                state.flights[key] = flight
            }
        }
    }

    private func produce(_ url: URL, key: String, id: UUID) async -> Data? {
        let started = state.withLock { $0.epoch }
        let modified = Self.modificationDate(of: url)
        var picture: Data?
        var failed = false
        if state.withLock({ $0.failures[key] }) == Failure(modified: modified) {
            // This file already gave nothing, and it has not changed since.
            failed = true
        } else {
            picture = await read(url)
            if picture == nil, !Task.isCancelled, !MediaFileTypes.isAudio(url) {
                // The card may scroll away while this waits for its turn; then the wait ends and nothing is generated.
                if await gate.acquire() {
                    picture = Task.isCancelled ? nil : await generate(url)
                    await gate.release()
                    if let picture, !Task.isCancelled { await write(picture, url) }
                    failed = picture == nil && !Task.isCancelled
                }
            }
        }
        let result = Task.isCancelled ? nil : picture
        state.withLock { state in
            if state.flights[key]?.id == id { state.flights[key] = nil }
            if let result, state.epoch == started { state.insert(result, forKey: key) }
            if failed, state.epoch == started {
                if state.failures.count >= Self.failureCapacity { state.failures.removeAll() }
                state.failures[key] = Failure(modified: modified)
            }
        }
        return result
    }
}

/// Lets a few pieces of work through at a time (generating a picture decodes video, which is not free). Waiting can
/// be cancelled.
actor AsyncGate {
    private let limit: Int
    private var running = 0
    private var waiting: [(id: UUID, continuation: CheckedContinuation<Bool, Never>)] = []

    init(limit: Int) {
        self.limit = max(1, limit)
    }

    /// Waits for a turn. `false` if the task was cancelled while waiting; the gate is then not held.
    func acquire() async -> Bool {
        if running < limit {
            running += 1
            return true
        }
        let id = UUID()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
                if Task.isCancelled {
                    continuation.resume(returning: false)
                } else {
                    waiting.append((id, continuation))
                }
            }
        } onCancel: {
            Task { await self.cancelWaiting(id) }
        }
    }

    func release() {
        if waiting.isEmpty {
            running -= 1
        } else {
            // The turn goes straight to the next in line.
            waiting.removeFirst().continuation.resume(returning: true)
        }
    }

    private func cancelWaiting(_ id: UUID) {
        guard let position = waiting.firstIndex(where: { $0.id == id }) else { return }
        waiting.remove(at: position).continuation.resume(returning: false)
    }
}

enum ThumbnailGenerator {
    /// The longest side of a card picture, in pixels (twice the 192 x 108 pt card, for retina).
    static let maximumSize = CGSize(width: 384, height: 216)
    static let jpegQuality = 0.75

    /// A JPEG of the video about a tenth of the way in (at most 30 s), `nil` for a file with no picture to show.
    static func jpegData(for url: URL) async -> Data? {
        guard !MediaFileTypes.isAudio(url) else { return nil }
        let asset = AVURLAsset(url: url)
        let duration = (try? await asset.load(.duration))?.seconds ?? 0
        let seconds = duration.isFinite && duration > 0 ? min(duration * 0.1, 30) : 0
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = maximumSize
        // The nearest key frame is good enough for a card, and much faster than an exact frame.
        generator.requestedTimeToleranceBefore = .positiveInfinity
        generator.requestedTimeToleranceAfter = .positiveInfinity
        guard let (image, _) = try? await generator.image(at: CMTime(seconds: seconds, preferredTimescale: 600)) else {
            return nil
        }
        return jpeg(from: image)
    }

    static func jpeg(from image: CGImage, quality: Double = jpegQuality) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil) else {
            return nil
        }
        CGImageDestinationAddImage(
            destination, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return data as Data
    }
}

/// Decoded card pictures, so scrolling back does not decode them again. Main actor only: `NSImage` is not `Sendable`.
/// Bounded by memory (decoded pixels, not file size) as well as by count.
@MainActor
enum CardImageCache {
    /// About 48 MB of decoded pixels.
    static let costLimit = 48 * 1024 * 1024
    static let countLimit = 400

    private static let images: NSCache<NSString, NSImage> = {
        let cache = NSCache<NSString, NSImage>()
        cache.countLimit = countLimit
        cache.totalCostLimit = costLimit
        return cache
    }()

    /// What a decoded picture takes: four bytes per pixel.
    static func cost(of image: NSImage) -> Int {
        let rep = image.representations.max { $0.pixelsWide * $0.pixelsHigh < $1.pixelsWide * $1.pixelsHigh }
        if let rep, rep.pixelsWide > 0, rep.pixelsHigh > 0 { return rep.pixelsWide * rep.pixelsHigh * 4 }
        return max(1, Int(image.size.width * image.size.height) * 4)
    }

    static func image(for path: String) -> NSImage? {
        images.object(forKey: ThumbnailService.key(path) as NSString)
    }

    static func store(_ image: NSImage, for path: String) {
        images.setObject(image, forKey: ThumbnailService.key(path) as NSString, cost: cost(of: image))
    }

    static func removeAll() { images.removeAllObjects() }
}
