import Synchronization

/// The app-wide rule "at most one translation runs at a time": a FIFO async gate that every `engine.translate`
/// invocation (the editor's "Çevir…" and retranslate-selected job, "Komple çevir"'s closing pass, the external-LLM
/// background batches, the playlist's stage 3) takes before it starts its engine process.
///
/// - Waiters are served in arrival order.
/// - A waiter whose task is cancelled leaves the queue at once and never holds the gate (`acquire` throws
///   `CancellationError`).
/// - Whoever got the gate must `release()` it on every exit path (error and cancel included); the callers do so with
///   `defer`.
///
/// It is a plain `Sendable` class over a mutex rather than an actor: the cancellation handler has to run synchronously
/// from any thread, and `tryAcquire` / `isBusy` have to be answered without suspending, so the main-actor callers can
/// decide and take the gate in one step.
final class TranslationGate: Sendable {
    private struct Waiter {
        let id: UInt64
        let continuation: CheckedContinuation<Void, any Error>
    }

    private struct State {
        /// Someone owns the gate. Ownership is handed straight to the first waiter on release, so `held` stays true
        /// while there is a queue and the queue is always empty while `held` is false.
        var held = false
        var nextID: UInt64 = 0
        var waiters: [Waiter] = []
    }

    private enum Outcome {
        case granted, cancelled, queued
    }

    private let state = Mutex(State())

    /// Someone holds the gate (and maybe others wait behind them). Background work that can simply try again later
    /// (the pre-translation pump) looks at this instead of queueing.
    var isBusy: Bool { state.withLock { $0.held } }

    /// How many tasks are queued behind the holder.
    var waitingCount: Int { state.withLock { $0.waiters.count } }

    /// Takes the gate if it is free, right now; never waits and never jumps ahead of a queue (there is no queue
    /// while it is free).
    func tryAcquire() -> Bool {
        state.withLock { state in
            guard !state.held else { return false }
            state.held = true
            return true
        }
    }

    /// Waits for the gate. Returns holding it; throws `CancellationError` (not holding it) if the task is cancelled
    /// before or while waiting. When the gate is free this neither suspends nor hops, so a caller on the main actor
    /// can't be overtaken between a check and the take.
    func acquire(isolation: isolated (any Actor)? = #isolation) async throws {
        try Task.checkCancellation()
        if !tryAcquire() {
            let id = state.withLock { state in
                state.nextID &+= 1
                return state.nextID
            }
            try await withTaskCancellationHandler {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                    let outcome = state.withLock { state -> Outcome in
                        // Cancelled before the handler could find the waiter in the queue.
                        if Task.isCancelled { return .cancelled }
                        if !state.held {
                            state.held = true
                            return .granted
                        }
                        state.waiters.append(Waiter(id: id, continuation: continuation))
                        return .queued
                    }
                    switch outcome {
                    case .granted: continuation.resume()
                    case .cancelled: continuation.resume(throwing: CancellationError())
                    case .queued: break
                    }
                }
            } onCancel: {
                let waiter = state.withLock { state -> Waiter? in
                    guard let index = state.waiters.firstIndex(where: { $0.id == id }) else { return nil }
                    return state.waiters.remove(at: index)
                }
                waiter?.continuation.resume(throwing: CancellationError())
            }
        }
        // Cancelled in the instant the gate was handed over: pass it on instead of holding it.
        if Task.isCancelled {
            release()
            throw CancellationError()
        }
    }

    /// Gives the gate to the first waiter, or frees it.
    func release() {
        let next = state.withLock { state -> Waiter? in
            guard state.held else { return nil }
            if state.waiters.isEmpty {
                state.held = false
                return nil
            }
            return state.waiters.removeFirst()
        }
        next?.continuation.resume()
    }
}
