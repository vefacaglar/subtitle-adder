import Foundation

/// A serial queue for calls into `ProfileStore`. The store's methods are synchronous and can wait (for its lock, for
/// the cross-process file lock, for PBKDF2 or for a slow disk), so the app never calls them on the main actor, and
/// never on the cooperative pool either, where a blocked thread would starve unrelated tasks.
final class StoreQueue: Sendable {
    private let queue: DispatchQueue

    init(label: String, qos: DispatchQoS = .userInitiated) {
        queue = DispatchQueue(label: label, qos: qos)
    }

    /// Runs `work` on the queue, after everything submitted before it.
    func run<T: Sendable>(_ work: @escaping @Sendable () -> T) async -> T {
        await withCheckedContinuation { continuation in
            queue.async { continuation.resume(returning: work()) }
        }
    }
}

/// The queues the video platform uses. Feed reads can take a while (fingerprinting a cold library), so they must not
/// hold up a tap on a star, and the thumbnail traffic has a lane of its own.
struct StoreQueues: Sendable {
    let feed = StoreQueue(label: "SubtitleAdder.platform.feed", qos: .userInitiated)
    let actions = StoreQueue(label: "SubtitleAdder.platform.actions", qos: .userInitiated)
    let thumbnails = StoreQueue(label: "SubtitleAdder.platform.thumbnails", qos: .utility)
}
