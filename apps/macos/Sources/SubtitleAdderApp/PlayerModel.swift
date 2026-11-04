import AVFoundation
import EngineKit
import Foundation
import Observation

/// Preview player: wraps `AVPlayer`, tracks the ~10 Hz playback time and the current line.
@MainActor
@Observable
final class PlayerModel {
    @ObservationIgnored let player = AVPlayer()
    @ObservationIgnored private var timeObserver: Any?
    @ObservationIgnored private var loadTask: Task<Void, Never>?
    @ObservationIgnored private let preparePreview: (@Sendable (URL) async throws -> PlaybackPreview)?
    @ObservationIgnored private var preview: PlaybackPreview?
    @ObservationIgnored private var pendingPreviewSource: URL?
    @ObservationIgnored private var pendingPreviewPosition: Double?
    private(set) var isPreparingPreview = false
    /// Identifies the current `load`; callbacks of an earlier item compare it and drop out.
    @ObservationIgnored private(set) var loadGeneration = 0
    @ObservationIgnored private var statusObservation: NSKeyValueObservation?
    @ObservationIgnored private var failureObserver: (any NSObjectProtocol)?
    /// Whether the current load has already reported a failure (even if the user dismissed it), so a later failure of
    /// the same load doesn't bring the notice back.
    @ObservationIgnored private var failureReported = false
    /// What the position shows while a seek is still settling (see `PlaybackTimeline`).
    @ObservationIgnored private var timeline = PlaybackTimeline()
    /// The current load should start playing as soon as its item is ready (Settings > "Video açıldığında otomatik oynat").
    @ObservationIgnored private var autoPlayWhenReady = false

    /// Reports playback to the video platform: started, stopped at a position, jumped, or about to be replaced. Not
    /// observed by any view.
    @ObservationIgnored var onPlaybackEvent: (@MainActor (PlaybackEvent) -> Void)?

    /// `.started` has been reported for the current load and not yet answered by a `.paused`.
    @ObservationIgnored private var reportedPlaying = false
    @ObservationIgnored private var timeControlObservation: NSKeyValueObservation?

    private(set) var currentTime: Double = 0
    /// Whether the player is playing (or about to: waiting for the item to be ready). Follows `AVPlayer.timeControlStatus`
    /// and nothing else: only the KVO handler writes it, so it can neither be left `true` on a paused player by a late
    /// periodic tick nor claim playback the player has not started. It trails the player by a main-actor hop, so code that
    /// starts or stops playback and then looks at it has to wait.
    private(set) var isPlaying = false {
        didSet {
            guard isPlaying != oldValue else { return }
            if isPlaying {
                reportedPlaying = true
                onPlaybackEvent?(.started)
            } else if reportedPlaying {
                // A pause that only follows the video being replaced (`load` reported `.leaving`) is not reported again,
                // as it would carry the new video's position.
                reportedPlaying = false
                onPlaybackEvent?(.paused(position))
            }
        }
    }
    private(set) var videoSize = CGSize(width: 16, height: 9)
    /// Stable identity of the line currently playing; updated only when it changes (to avoid needless redraws).
    private(set) var activeID: UUID?
    /// Why the current video can't be played, `nil` while it can (or after the user dismissed the notice). Playback
    /// only; subtitle generation doesn't depend on it (the engine decodes the audio itself). Cleared on every `load`.
    /// Kept as a cause, not as text, so the message follows a UI language switch.
    private(set) var failure: PlaybackFailure?
    private var sentences: [Sentence] = []

    /// Why playback failed. The generic causes are localized when displayed; a system error keeps its own text.
    enum PlaybackFailure: Equatable, Sendable {
        case notPlayable
        case unknown
        case system(String)

        /// Stable identity, independent of the UI language.
        var id: String {
            switch self {
            case .notPlayable: "not-playable"
            case .unknown: "unknown"
            case .system(let text): "system:\(text)"
            }
        }

        /// Readable cause in the current UI language.
        var text: String {
            switch self {
            case .notPlayable: L("Bu biçim oynatılamıyor.")
            case .unknown: L("Bilinmeyen hata")
            case .system(let text): text
            }
        }
    }

    /// "Video oynatılamadı: …" for the status bar; `nil` while playback works.
    var failureMessage: LocalizedMessage? {
        failure.map { cause in
            LocalizedMessage("playback-failed:\(cause.id)") { L("Video oynatılamadı: %@", cause.text) }
        }
    }

    // `isolated` so the main-actor observer token can be removed when a closed window drops its player.
    isolated deinit {
        loadTask?.cancel()
        stopObservingItem()
        player.replaceCurrentItem(with: nil)
    }

    init(preparePreview: (@Sendable (URL) async throws -> PlaybackPreview)? = nil) {
        self.preparePreview = preparePreview
        timeObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 0.1, preferredTimescale: 600),
            queue: .main
        ) { [weak self] time in
            MainActor.assumeIsolated { self?.observe(playerTime: time.seconds) }
        }
        // The single source of `isPlaying`. The handler reads the status when it runs rather than the value it was called
        // with, so however the changes pile up the last one to run leaves the current state.
        timeControlObservation = player.observe(\.timeControlStatus, options: []) { [weak self] _, _ in
            Task { @MainActor in self?.syncPlaybackState() }
        }
    }

    private func syncPlaybackState() {
        let playing = player.timeControlStatus != .paused
        if playing != isPlaying { isPlaying = playing }
    }

    /// Replaces the video. With `autoPlay` playback starts once the new item is ready (never for `nil`, and not when
    /// the video turns out to be unplayable).
    func load(_ url: URL?, autoPlay: Bool = false) {
        // Where the video being left was, before anything resets.
        if player.currentItem != nil { onPlaybackEvent?(.leaving(position)) }
        reportedPlaying = false
        loadTask?.cancel()
        loadGeneration += 1
        let generation = loadGeneration
        stopObservingItem()
        isPreparingPreview = false
        pendingPreviewPosition = nil
        pendingPreviewSource = url.flatMap { $0.isFileURL && MediaFileTypes.isMedia($0) ? $0 : nil }
        failure = nil
        failureReported = false
        autoPlayWhenReady = autoPlay && url != nil
        player.pause()
        timeline.reset()
        currentTime = 0
        // Don't use the old video's aspect ratio until the new video size arrives.
        videoSize = CGSize(width: 16, height: 9)
        updateActive()
        player.replaceCurrentItem(with: nil)
        preview = nil
        guard let url else { return }
        install(url, generation: generation)
    }

    private func install(_ url: URL, generation: Int) {
        let asset = AVURLAsset(url: url)
        let item = AVPlayerItem(asset: asset)
        observe(item, generation: generation)
        player.replaceCurrentItem(with: item)
        loadTask = Task {
            // Not decodable on this Mac (codec/container); the item's own status may stay "unknown" for such files.
            do {
                let playable = try await asset.load(.isPlayable)
                if !playable {
                    itemFailed(nil, generation: generation)
                    return
                }
            } catch {
                if !Task.isCancelled { itemFailed(error, generation: generation) }
                return
            }
            guard let track = try? await asset.loadTracks(withMediaType: .video).first,
                  let (natural, transform) = try? await track.load(.naturalSize, .preferredTransform),
                  !Task.isCancelled, generation == loadGeneration
            else { return }
            let size = natural.applying(transform)
            if abs(size.width) > 1, abs(size.height) > 1 {
                videoSize = CGSize(width: abs(size.width), height: abs(size.height))
            }
        }
    }

    /// The native player rejected the format. Try once using an MP4 in scratch storage, keeping the selected video
    /// as the source for subtitles, history and export.
    private func itemFailed(_ error: (any Error)?, generation: Int) {
        guard generation == loadGeneration, !failureReported, !isPreparingPreview else { return }
        if let source = pendingPreviewSource, let preparePreview, Self.canTryPreview(after: error) {
            pendingPreviewSource = nil
            pendingPreviewPosition = currentTime
            autoPlayWhenReady = autoPlayWhenReady || isPlaying
            isPreparingPreview = true
            loadTask?.cancel()
            stopObservingItem()
            player.replaceCurrentItem(with: nil)
            loadGeneration += 1  // Ignore callbacks already queued by the original item.
            let attempt = loadGeneration
            loadTask = Task { [weak self] in
                do {
                    let copy = try await preparePreview(source)
                    guard let self, !Task.isCancelled, attempt == loadGeneration else { return }
                    isPreparingPreview = false
                    preview = copy
                    install(copy.url, generation: attempt)
                } catch {
                    guard let self, !Task.isCancelled, attempt == loadGeneration else { return }
                    isPreparingPreview = false
                    reportFailure(error, generation: attempt)
                }
            }
        } else {
            reportFailure(error, generation: generation)
        }
    }

    private static func canTryPreview(after error: (any Error)?) -> Bool {
        guard let error else { return true }
        let reason = error as NSError
        guard reason.domain == AVFoundationErrorDomain else { return false }
        return [
            AVError.fileFormatNotRecognized.rawValue, AVError.fileFailedToParse.rawValue,
            AVError.decoderNotFound.rawValue, AVError.operationNotSupportedForAsset.rawValue,
        ].contains(reason.code)
    }

    func cancelPreview() {
        if isPreparingPreview { loadTask?.cancel() }
    }

    /// Records that playback of the item loaded as `generation` failed; a stale generation is ignored. A missing error
    /// (an asset that is simply not playable) gets a generic reason. The first failure of a load wins.
    func reportFailure(_ error: (any Error)?, generation: Int? = nil) {
        guard generation == nil || generation == loadGeneration, !failureReported else { return }
        failureReported = true
        failure = Self.cause(for: error)
        autoPlayWhenReady = false
        player.pause()
    }

    /// Hides the playback notice for the current video; a later failure of the same load doesn't bring it back.
    func dismissFailure() {
        failure = nil
        failureReported = true
    }

    /// Cause of a playback failure; no error means the asset is simply not playable.
    static func cause(for error: (any Error)?) -> PlaybackFailure {
        guard let error else { return .notPlayable }
        let text = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? .unknown : .system(text)
    }

    /// Watches the item for a failed status and for failing to play to the end.
    private func observe(_ item: AVPlayerItem, generation: Int) {
        statusObservation = item.observe(\.status, options: [.new]) { [weak self] item, _ in
            switch item.status {
            case .failed:
                let error = item.error
                Task { @MainActor in self?.itemFailed(error, generation: generation) }
            case .readyToPlay:
                Task { @MainActor in self?.itemReady(generation: generation) }
            default:
                break
            }
        }
        failureObserver = NotificationCenter.default.addObserver(
            forName: AVPlayerItem.failedToPlayToEndTimeNotification, object: item, queue: .main
        ) { [weak self] note in
            let error = note.userInfo?[AVPlayerItemFailedToPlayToEndTimeErrorKey] as? any Error
            MainActor.assumeIsolated { self?.itemFailed(error, generation: generation) }
        }
    }

    private func stopObservingItem() {
        statusObservation = nil
        if let failureObserver { NotificationCenter.default.removeObserver(failureObserver) }
        failureObserver = nil
    }

    func setSentences(_ sentences: [Sentence]) {
        self.sentences = sentences
        updateActive()
    }

    /// The item of load `generation` can play: starts it when that load asked for auto-play. A stale generation (the
    /// video was replaced meanwhile), a failed load and a load that already started are ignored.
    func itemReady(generation: Int) {
        guard generation == loadGeneration, failure == nil else { return }
        if let position = pendingPreviewPosition {
            pendingPreviewPosition = nil
            if position > 0 { seek(to: position) }
        }
        if autoPlayWhenReady { play() }
    }

    /// Moves to `seconds` (kept inside the video). The new position shows at once and holds while the player catches up.
    func seek(to seconds: Double) {
        if isPreparingPreview {
            pendingPreviewPosition = max(0, seconds)
            return
        }
        let target = timeline.seek(to: seconds)
        currentTime = target
        updateActive()
        player.seek(to: CMTime(seconds: target, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
        if player.currentItem != nil { onPlaybackEvent?(.seeked(position)) }
    }

    /// Moves by `seconds` from the shown position, which is the requested one while a seek settles, so held arrow keys
    /// keep adding up instead of restarting from the stale player time. Does nothing without a video.
    func seek(by seconds: Double) {
        guard player.currentItem != nil else { return }
        seek(to: currentTime + seconds)
    }

    func play() {
        if isPreparingPreview {
            autoPlayWhenReady = true
            return
        }
        guard player.currentItem != nil else { return }
        autoPlayWhenReady = false
        player.play()
    }

    /// Pauses, and also cancels an auto-play that is still waiting for the item.
    func pause() {
        autoPlayWhenReady = false
        player.pause()
    }

    func togglePlayback() {
        if isPreparingPreview {
            autoPlayWhenReady.toggle()
            return
        }
        guard player.currentItem != nil else { return }
        if player.timeControlStatus == .paused {
            if let duration = player.currentItem?.duration.seconds, duration.isFinite, currentTime >= duration - 0.05 {
                seek(to: 0)
            }
            play()
        } else {
            pause()
        }
    }

    /// The length of the loaded item in seconds; 0 while unknown (or for a stream without an end).
    var duration: Double {
        let seconds = player.currentItem?.duration.seconds ?? 0
        return seconds.isFinite && seconds > 0 ? seconds : 0
    }

    /// Where playback is now, as the platform records it.
    var position: PlaybackPosition { PlaybackPosition(seconds: currentTime, duration: duration) }

    // MARK: - Volume

    /// Volume of the player (0...1). It lives on the `AVPlayer`, so AVKit's own volume control follows every change
    /// and the choice lasts for the session, across videos.
    var volume: Double { Double(player.volume) }
    var isMuted: Bool { player.isMuted }

    /// Moves the volume by `step` (0.05 is 5 %), counted from silence while muted, and unmutes when it ends above 0.
    func adjustVolume(by step: Double) {
        let base = player.isMuted ? 0 : Double(player.volume)
        // Whole percents, so repeated 5 % steps don't drift.
        let next = (min(max(base + step, 0), 1) * 100).rounded() / 100
        player.volume = Float(next)
        if next > 0, player.isMuted { player.isMuted = false }
    }

    func toggleMute() {
        player.isMuted.toggle()
    }

    // MARK: - Time

    /// The periodic observer's time. `now` is injectable for tests.
    func observe(playerTime seconds: Double, now: ContinuousClock.Instant = .now) {
        let duration = player.currentItem?.duration.seconds ?? 0
        timeline.observe(position: seconds, duration: duration, now: now)
        if currentTime != timeline.position { currentTime = timeline.position }
        updateActive()
    }

    private func updateActive() {
        let id = sentences.activeIndex(at: currentTime).map { sentences[$0].uid }
        if id != activeID { activeID = id }
    }
}
