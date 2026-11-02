import Foundation

/// Gathers the files Finder hands over into one batch.
///
/// SwiftUI calls `onOpenURL` once per URL, so "Open With" on several videos arrives as several calls in quick
/// succession. Opening each on its own would make the last one win instead of building a playlist. Each URL restarts a
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
        let wait = min(quietPeriod, max(.zero, maxWait - (now - first)))
        timer?.cancel()
        timer = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            self?.flush()
        }
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
