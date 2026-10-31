import AVFoundation

/// Adım 1: videodaki ses izini 16 kHz mono WAV'a ayırır (Whisper'ın beklediği biçim).
public enum AudioExtractor {
    public static let sampleRate: Double = 16_000

    public enum Failure: LocalizedError {
        case noAudioTrack
        case readerFailed(String)

        public var errorDescription: String? {
            switch self {
            case .noAudioTrack: "Videoda ses izi yok."
            case .readerFailed(let reason): "Ses okunamadı: \(reason)"
            }
        }
    }

    @discardableResult
    public static func extract(from video: URL, to wav: URL) async throws -> URL {
        let asset = AVURLAsset(url: video)
        guard let track = try await asset.loadTracks(withMediaType: .audio).first else {
            throw Failure.noAudioTrack
        }

        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: sampleRate,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 32,
            AVLinearPCMIsFloatKey: true,
            AVLinearPCMIsNonInterleaved: false,
            AVLinearPCMIsBigEndianKey: false,
        ])
        reader.add(output)
        guard reader.startReading() else {
            throw Failure.readerFailed(reader.error?.localizedDescription ?? "bilinmeyen hata")
        }

        let format = AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 1)!
        try? FileManager.default.removeItem(at: wav)
        try FileManager.default.createDirectory(at: wav.deletingLastPathComponent(), withIntermediateDirectories: true)
        let file = try AVAudioFile(forWriting: wav, settings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: sampleRate,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
        ], commonFormat: .pcmFormatFloat32, interleaved: false)

        while let sample = output.copyNextSampleBuffer() {
            guard let block = CMSampleBufferGetDataBuffer(sample) else { continue }
            let length = CMBlockBufferGetDataLength(block)
            let frames = AVAudioFrameCount(length / MemoryLayout<Float>.size)
            guard frames > 0, let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames) else { continue }
            buffer.frameLength = frames
            try CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: buffer.floatChannelData![0])
                .check()
            try file.write(from: buffer)
        }
        if reader.status == .failed {
            throw Failure.readerFailed(reader.error?.localizedDescription ?? "bilinmeyen hata")
        }
        return wav
    }
}

private extension OSStatus {
    func check() throws {
        if self != noErr { throw AudioExtractor.Failure.readerFailed("OSStatus \(self)") }
    }
}
