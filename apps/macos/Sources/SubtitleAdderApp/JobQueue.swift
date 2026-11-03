import EngineKit
import Foundation
import Observation

/// What a single-video job does.
enum JobKind: Equatable, Sendable {
    /// Transcribes the video; with `thenTranslate` ("Komple çevir") the translation follows in the same job.
    case generate(thenTranslate: Bool)
    /// Translates the lines: all of them, only what is missing (`reuseExisting`), or only `only` (retranslate selected).
    case translate(reuseExisting: Bool, only: Set<UUID>?)
}

/// The toolbar choices a job runs with, captured when the job is requested. A job may wait in the queue or run while
/// another video is on screen, so it never reads the toolbar again: what the user chose when they pressed the button is
/// what the job does.
struct JobSettings: Equatable, Sendable {
    /// Spoken language; empty means automatic detection.
    var language: String
    var model: String
    var diarize: Bool
    /// Known speaker count; 0 means automatic.
    var speakerCount: Int
    var translationTarget: String
    var translationModel: String
    var autoSave: Bool
    var exportFormat: SubtitleFormat
}

/// The app's one lane for single-video jobs, and the arbiter between them and the batch playlist for the engine's
/// speech recognition: at most one of a single job and a batch item's recognition runs at a time.
///
/// - A job requested while the lane is busy waits here, first in first out. A new request for a video that already
///   waits replaces that entry and keeps its place.
/// - A single job holds the lane from its start (recognition, and the translation that follows it in "Komple çevir")
///   until it ends or is cancelled. Translation as such is further limited by the app-wide `TranslationGate`.
/// - The batch playlist takes the lane for one item's recognition at a time (`acquireBatchSlot`) and gives it back
///   when that item has been handed off to translation (or, on a Mac with little memory, when its translation is over).
///   Waiting single jobs go before the batch's next item, so a job the user asked for by hand never waits for a whole
///   playlist.
///
/// The queue only decides who runs when; starting a job is the owner's business (`start`).
@MainActor
@Observable
final class JobQueue {
    struct Entry: Identifiable {
        let id = UUID()
        let session: VideoSession
        let kind: JobKind
        let settings: JobSettings
    }

    enum Disposition: Equatable {
        case started, queued
    }

    /// The job that holds the lane.
    private(set) var running: Entry?
    /// Jobs waiting for the lane, in the order they will run.
    private(set) var waiting: [Entry] = []
    /// The batch playlist's recognition holds the lane.
    private(set) var batchHoldsLane = false

    /// Called, on the main actor, when an entry gets the lane. The owner launches the job and, when it ends or is
    /// cancelled, tells the queue with `jobEnded`.
    @ObservationIgnored var start: (@MainActor (Entry) -> Void)?
    @ObservationIgnored private var batchWaiter: CheckedContinuation<Bool, Never>?

    /// Anything is running, waiting, or about to run: the lane is not idle.
    var isBusy: Bool { running != nil || !waiting.isEmpty || batchHoldsLane }

    func isRunning(_ session: VideoSession) -> Bool { running?.session === session }

    func isQueued(_ session: VideoSession) -> Bool { waiting.contains { $0.session === session } }

    /// Zero-based place in the waiting line, or `nil`.
    func position(of session: VideoSession) -> Int? { waiting.firstIndex { $0.session === session } }

    /// Starts the job at once when the lane is free, otherwise puts it in the waiting line.
    @discardableResult
    func submit(_ entry: Entry) -> Disposition {
        if let index = waiting.firstIndex(where: { $0.session === entry.session }) {
            waiting[index] = entry
            return .queued
        }
        if running == nil, !batchHoldsLane, waiting.isEmpty {
            running = entry
            start?(entry)
            return .started
        }
        waiting.append(entry)
        return .queued
    }

    /// Takes the video's waiting entry out. Does nothing to a job that is already running.
    @discardableResult
    func remove(_ session: VideoSession) -> Bool {
        guard let index = waiting.firstIndex(where: { $0.session === session }) else { return false }
        waiting.remove(at: index)
        return true
    }

    /// Empties the waiting line (the running job goes on) and returns the sessions that were in it.
    @discardableResult
    func clearWaiting() -> [VideoSession] {
        let removed = waiting.map(\.session)
        waiting = []
        return removed
    }

    /// The video's job is over (finished, failed or cancelled): the lane goes to whoever is next.
    func jobEnded(_ session: VideoSession) {
        guard running?.session === session else { return }
        running = nil
        advance()
    }

    // MARK: - The batch playlist

    /// Waits until the batch may recognize an item. Returns `false` (not holding the lane) when the waiting task is
    /// cancelled. Entries that are waiting go first.
    func acquireBatchSlot() async -> Bool {
        if Task.isCancelled { return false }
        if running == nil, !batchHoldsLane, waiting.isEmpty {
            batchHoldsLane = true
            return true
        }
        let granted = await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
                if Task.isCancelled {
                    continuation.resume(returning: false)
                } else {
                    batchWaiter = continuation
                }
            }
        } onCancel: {
            Task { @MainActor in self.abandonBatchWait() }
        }
        return granted
    }

    /// The batch is done recognizing for now.
    func releaseBatchSlot() {
        guard batchHoldsLane else { return }
        batchHoldsLane = false
        advance()
    }

    private func abandonBatchWait() {
        guard let waiter = batchWaiter else { return }
        batchWaiter = nil
        waiter.resume(returning: false)
    }

    // MARK: - Handing out the lane

    private func advance() {
        while running == nil, !batchHoldsLane {
            if !waiting.isEmpty {
                let next = waiting.removeFirst()
                running = next
                start?(next)
            } else if let waiter = batchWaiter {
                batchWaiter = nil
                batchHoldsLane = true
                waiter.resume(returning: true)
            } else {
                return
            }
        }
    }
}
