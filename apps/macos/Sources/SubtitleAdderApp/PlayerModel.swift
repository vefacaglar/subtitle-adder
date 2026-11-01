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
    /// Identifies the current `load`; callbacks of an earlier item compare it and drop out.
    @ObservationIgnored private var loadGeneration = 0
    @ObservationIgnored private var statusObservation: NSKeyValueObservation?
    @ObservationIgnored private var failureObserver: (any NSObjectProtocol)?

    private(set) var currentTime: Double = 0
    private(set) var isPlaying = false
    private(set) var videoSize = CGSize(width: 16, height: 9)
    /// Stable identity of the line currently playing; updated only when it changes (to avoid needless redraws).
    private(set) var activeID: UUID?
    /// Why the current video can't be played, `nil` while it can. Playback only; subtitle generation doesn't depend on
    /// it (the engine decodes the audio itself). Cleared on every `load`.
    private(set) var failureReason: String?
    private var sentences: [Sentence] = []

    /// "Video oynatılamadı: …" for the status bar; `nil` while playback works.
    var failureMessage: LocalizedMessage? {
        failureReason.map { reason in
            LocalizedMessage("playback-failed:\(reason)") { L("Video oynatılamadı: %@", reason) }
        }
    }

    init() {
        timeObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 0.1, preferredTimescale: 600),
            queue: .main
        ) { [weak self] time in
            MainActor.assumeIsolated { self?.tick(time.seconds) }
        }
    }

    func load(_ url: URL?) {
        loadTask?.cancel()
        loadGeneration += 1
        let generation = loadGeneration
        stopObservingItem()
        failureReason = nil
        player.pause()
        isPlaying = false
        currentTime = 0
        // Don't use the old video's aspect ratio until the new video size arrives.
        videoSize = CGSize(width: 16, height: 9)
        updateActive()
        guard let url else {
            player.replaceCurrentItem(with: nil)
            return
        }
        let asset = AVURLAsset(url: url)
        let item = AVPlayerItem(asset: asset)
        observe(item, generation: generation)
        player.replaceCurrentItem(with: item)
        loadTask = Task {
            // Not decodable on this Mac (codec/container); the item's own status may stay "unknown" for such files.
            do {
                let playable = try await asset.load(.isPlayable)
                if !playable {
                    reportFailure(nil, generation: generation)
                    return
                }
            } catch {
                if !Task.isCancelled { reportFailure(error, generation: generation) }
                return
            }
            guard let track = try? await asset.loadTracks(withMediaType: .video).first,
                  let (natural, transform) = try? await track.load(.naturalSize, .preferredTransform),
                  !Task.isCancelled
            else { return }
            let size = natural.applying(transform)
            if abs(size.width) > 1, abs(size.height) > 1 {
                videoSize = CGSize(width: abs(size.width), height: abs(size.height))
            }
        }
    }

    /// Records that playback of the item loaded as `generation` failed; a stale generation is ignored. A missing error
    /// (an asset that is simply not playable) gets a generic reason. The first failure of a load wins.
    func reportFailure(_ error: (any Error)?, generation: Int? = nil) {
        guard generation == nil || generation == loadGeneration, failureReason == nil else { return }
        failureReason = Self.reason(for: error)
        player.pause()
        isPlaying = false
    }

    /// Readable cause of a playback failure.
    static func reason(for error: (any Error)?) -> String {
        guard let error else { return L("Bu biçim oynatılamıyor.") }
        let text = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? L("Bilinmeyen hata") : text
    }

    /// Watches the item for a failed status and for failing to play to the end.
    private func observe(_ item: AVPlayerItem, generation: Int) {
        statusObservation = item.observe(\.status, options: [.new]) { [weak self] item, _ in
            guard item.status == .failed else { return }
            let error = item.error
            Task { @MainActor in self?.reportFailure(error, generation: generation) }
        }
        failureObserver = NotificationCenter.default.addObserver(
            forName: AVPlayerItem.failedToPlayToEndTimeNotification, object: item, queue: .main
        ) { [weak self] note in
            let error = note.userInfo?[AVPlayerItemFailedToPlayToEndTimeErrorKey] as? any Error
            MainActor.assumeIsolated { self?.reportFailure(error, generation: generation) }
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

    func seek(to seconds: Double) {
        let target = max(0, seconds)
        currentTime = target
        updateActive()
        player.seek(to: CMTime(seconds: target, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
    }

    func pause() {
        player.pause()
        isPlaying = false
    }

    func togglePlayback() {
        guard player.currentItem != nil else { return }
        if player.timeControlStatus == .paused {
            if let duration = player.currentItem?.duration.seconds, duration.isFinite, currentTime >= duration - 0.05 {
                seek(to: 0)
            }
            player.play()
            isPlaying = true
        } else {
            player.pause()
            isPlaying = false
        }
    }

    private func tick(_ seconds: Double) {
        if seconds.isFinite { currentTime = seconds }
        let playing = player.timeControlStatus != .paused
        if playing != isPlaying { isPlaying = playing }
        updateActive()
    }

    private func updateActive() {
        let id = sentences.activeIndex(at: currentTime).map { sentences[$0].uid }
        if id != activeID { activeID = id }
    }
}
