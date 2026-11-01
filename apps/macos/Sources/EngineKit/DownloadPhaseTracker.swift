import Foundation

/// Tells which model a `downloading_model` stage belongs to. With `--diarize` the engine can emit two download
/// phases back to back (transcription model, then speaker model) with no other stage in between, and a phase is
/// skipped when that model is already cached. The expected order is predicted from the catalog; a phase boundary is
/// detected when a download follows a non-download stage, or when the fraction drops back after a phase finished.
public struct DownloadPhaseTracker: Sendable, Equatable {
    public enum Kind: Sendable, Equatable {
        case transcription, diarization
    }

    private let expected: [Kind]
    private var phase = -1
    private var lastFraction: Double?
    private var inDownload = false

    /// `transcriptionDownloaded` / `diarizationDownloaded` come from the catalog; `diarize` is the toggle.
    public init(transcriptionDownloaded: Bool, diarize: Bool, diarizationDownloaded: Bool) {
        var order: [Kind] = []
        if !transcriptionDownloaded { order.append(.transcription) }
        if diarize, !diarizationDownloaded { order.append(.diarization) }
        expected = order
    }

    /// Feed every stage event; returns the model being downloaded for `downloading_model`, `nil` otherwise.
    public mutating func update(stage: EngineEvent.Stage, fraction: Double?) -> Kind? {
        guard stage == .downloadingModel else {
            inDownload = false
            lastFraction = nil
            return nil
        }
        let droppedBack = (lastFraction ?? 0) >= 0.99 && (fraction ?? 0) < (lastFraction ?? 0)
        if !inDownload || droppedBack { phase += 1 }
        inDownload = true
        lastFraction = fraction
        return phase < expected.count ? expected[phase] : .transcription
    }
}
