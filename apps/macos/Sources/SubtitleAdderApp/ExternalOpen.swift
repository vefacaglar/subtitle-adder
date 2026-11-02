import AppKit
import Foundation

/// The system's "open these files" events (Finder's Open With, a double click, a drop on the Dock icon).
///
/// Measured with a packaged build (macOS 27, `handlesExternalEvents(preferring: ["*"], allowing: ["*"])`): for a request
/// with several files SwiftUI calls `onOpenURL` for the FIRST file only and passes the remaining ones to this
/// delegate's `application(_:open:)` in one call (an empty array when the request had a single file). Neither path
/// sees the whole request, so both feed one `ExternalOpenCoalescer`, which puts the pieces back together. This holds
/// for a cold launch with files and for a request to the running app alike.
@MainActor
final class ExternalOpenDelegate: NSObject, NSApplicationDelegate {
    /// Where the files go. Set when the app starts; events that arrive before that (a cold launch with files) wait.
    static var coalescer: ExternalOpenCoalescer? {
        didSet {
            guard let coalescer, !waiting.isEmpty else { return }
            let urls = waiting
            waiting = []
            for url in urls { coalescer.receive(url) }
        }
    }

    private static var waiting: [URL] = []

    func application(_ application: NSApplication, open urls: [URL]) {
        guard let coalescer = Self.coalescer else {
            Self.waiting += urls
            return
        }
        for url in urls { coalescer.receive(url) }
    }
}

/// Gathers the files Finder hands over into one batch.
///
/// "Open With" on several videos arrives in pieces (see `ExternalOpenDelegate`) within a few milliseconds of each
/// other. Opening each piece on its own would make the last one win instead of building a playlist. Each URL restarts a
/// short quiet period; when it passes without another URL (or `maxWait` since the first one is over, so a steady
/// stream can't hold the batch back) everything received is delivered at once, in arrival order and without repeats.
@MainActor
final class ExternalOpenCoalescer {
    private var urls: [URL] = []
    private var firstReceivedAt: ContinuousClock.Instant?
    private var timer: Task<Void, Never>?
    private let quietPeriod: Duration
    private let maxWait: Duration
    private let deliver: @MainActor ([URL]) -> Void

    init(
        quietPeriod: Duration = .milliseconds(250),
        maxWait: Duration = .seconds(1),
        deliver: @escaping @MainActor ([URL]) -> Void
    ) {
        self.quietPeriod = quietPeriod
        self.maxWait = maxWait
        self.deliver = deliver
    }

    /// Adds a URL to the batch (anything that is not a file URL is dropped) and restarts the quiet period.
    func receive(_ url: URL) {
        guard url.isFileURL else { return }
        if !urls.contains(url) { urls.append(url) }
        let now = ContinuousClock.now
        let first = firstReceivedAt ?? now
        firstReceivedAt = first
        let wait = Self.delay(quietPeriod: quietPeriod, maxWait: maxWait, sinceFirst: now - first)
        timer?.cancel()
        timer = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            self?.flush()
        }
    }

    /// How long to wait for another URL: the quiet period, shortened so the batch is out `maxWait` after its first URL.
    static func delay(quietPeriod: Duration, maxWait: Duration, sinceFirst: Duration) -> Duration {
        min(quietPeriod, max(.zero, maxWait - sinceFirst))
    }

    /// Delivers what has been received so far, if anything, without waiting for the quiet period.
    func flush() {
        timer?.cancel()
        timer = nil
        firstReceivedAt = nil
        guard !urls.isEmpty else { return }
        let batch = urls
        urls = []
        deliver(batch)
    }
}
