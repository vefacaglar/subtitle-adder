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

    private(set) var currentTime: Double = 0
    private(set) var isPlaying = false
    private(set) var videoSize = CGSize(width: 16, height: 9)
    /// Stable identity of the line currently playing; updated only when it changes (to avoid needless redraws).
    private(set) var activeID: UUID?
    private var sentences: [Sentence] = []

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
        player.replaceCurrentItem(with: AVPlayerItem(asset: asset))
        loadTask = Task {
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
