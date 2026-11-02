import Foundation

/// The playback position the app shows while an asynchronous seek is still settling.
///
/// `AVPlayer.seek` returns at once and the periodic time observer can report the pre-seek time for a few more ticks,
/// which would put the position (and the highlighted line) back where it was. After a seek the requested position is
/// kept for up to `settleWindow` while the player reports a time more than `tolerance` away from it. A newer seek
/// replaces the pending one, and `reset` (a new video) clears it. Port of the Windows app's `PlaybackTimeline`.
struct PlaybackTimeline {
    /// How far the player's time may be from the requested position before it still counts as "not there yet".
    static let tolerance = 0.25
    /// How long a requested position is held; a decoder that can't reach it exactly is believed again after this.
    static let settleWindow = Duration.seconds(2)

    private(set) var position = 0.0
    private(set) var duration = 0.0
    private var pending: (target: Double, requestedAt: ContinuousClock.Instant)?

    /// A seek was requested and the player hasn't reported a position near it yet.
    var isSettling: Bool { pending != nil }

    mutating func reset() {
        position = 0
        duration = 0
        pending = nil
    }

    /// Records a seek to `seconds` (kept inside the known duration) and returns the position that will be shown.
    @discardableResult
    mutating func seek(to seconds: Double, now: ContinuousClock.Instant = .now) -> Double {
        position = Self.clamp(seconds, duration: duration)
        pending = (position, now)
        return position
    }

    /// Takes the position the player reports. A non-finite time (an item that isn't ready yet) keeps the shown
    /// position; the duration is always taken.
    mutating func observe(position reported: Double, duration reportedDuration: Double, now: ContinuousClock.Instant = .now) {
        duration = reportedDuration.isFinite ? max(0, reportedDuration) : 0
        guard reported.isFinite else { return }
        let reported = Self.clamp(reported, duration: duration)
        if let pending {
            let target = Self.clamp(pending.target, duration: duration)
            // The player may still answer with the old frame; keep the request briefly, but believe the player again
            // once the window is over so a seek it can't reach exactly doesn't pin the position.
            if abs(reported - target) > Self.tolerance, now - pending.requestedAt < Self.settleWindow {
                position = target
                return
            }
            self.pending = nil
        }
        position = reported
    }

    /// `seconds` limited to `0...duration`; with an unknown duration (0) only the lower bound applies. Non-finite is 0.
    static func clamp(_ seconds: Double, duration: Double) -> Double {
        guard seconds.isFinite else { return 0 }
        return min(max(seconds, 0), duration > 0 ? duration : .greatestFiniteMagnitude)
    }
}
