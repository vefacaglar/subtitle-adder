import AVFoundation
import AppKit
import EngineKit
import Foundation
import ImageIO
import LibraryKit
import Synchronization
import UniformTypeIdentifiers

/// The pictures of the video cards: an in-memory cache keyed by path, in front of the profile's encrypted vault, in
/// front of generating the picture from the file. Everything here runs off the main actor; callers get JPEG bytes and
/// decode them where they draw.
///
/// Two cards (or a card and a refresh) asking for the same video share one piece of work. When the last of them
/// goes away, as when a card scrolls out of view, the work is cancelled before it starts the expensive part.
final class ThumbnailService: Sendable {
    /// Reads the picture stored for a video in the vault; `nil` when there is none or nobody is logged in.
    typealias Reader = @Sendable (URL) async -> Data?
    /// Stores a generated picture in the vault; a no-op while logged out.
    typealias Writer = @Sendable (Data, URL) async -> Void
    /// Makes a picture from the file.
    typealias Generator = @Sendable (URL) async -> Data?

    private struct Flight {
        let id: UUID
        let task: Task<Data?, Never>
        var waiters: Int
    }

    private struct State {
        var pictures: [String: Data] = [:]
        var flights: [String: Flight] = [:]
    }

    /// Most pictures kept; a cache this size is a few tens of MB at most. Past it the cache starts over.
    private static let capacity = 2000

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
        state.withLock { state in
            if state.pictures.count >= Self.capacity, state.pictures[Self.key(path)] == nil { state.pictures.removeAll() }
            state.pictures[Self.key(path)] = data
        }
    }

    func cached(for path: String) -> Data? {
        state.withLock { $0.pictures[Self.key(path)] }
    }

    /// Forgets the pictures (a different profile's vault may hold different ones).
    func reset() {
        state.withLock { $0.pictures.removeAll() }
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
        var picture = await read(url)
        if picture == nil, !Task.isCancelled, !MediaFileTypes.isAudio(url) {
            // The card may scroll away while this waits for its turn; then the wait ends and nothing is generated.
            if await gate.acquire() {
                picture = Task.isCancelled ? nil : await generate(url)
                await gate.release()
                if let picture, !Task.isCancelled { await write(picture, url) }
            }
        }
        let result = Task.isCancelled ? nil : picture
        state.withLock { state in
            if state.flights[key]?.id == id { state.flights[key] = nil }
            if let result {
                if state.pictures.count >= Self.capacity { state.pictures.removeAll() }
                state.pictures[key] = result
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
@MainActor
enum CardImageCache {
    private static let images: NSCache<NSString, NSImage> = {
        let cache = NSCache<NSString, NSImage>()
        cache.countLimit = 400
        return cache
    }()

    static func image(for path: String) -> NSImage? {
        images.object(forKey: ThumbnailService.key(path) as NSString)
    }

    static func store(_ image: NSImage, for path: String) {
        images.setObject(image, forKey: ThumbnailService.key(path) as NSString)
    }

    static func removeAll() { images.removeAllObjects() }
}
