import AVFoundation
import Foundation
import Testing
@testable import SubtitleCore

/// ffmpeg ile 2 sn'lik sesli test videosu üretip ayırmayı doğrular (ffmpeg yoksa atlanır).
@Test(.enabled(if: FileManager.default.isExecutableFile(atPath: "/opt/homebrew/bin/ffmpeg")))
func extractsMono16kWav() async throws {
    let dir = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: dir) }
    let video = dir.appending(path: "tone.mp4")

    let ffmpeg = Process()
    ffmpeg.executableURL = URL(filePath: "/opt/homebrew/bin/ffmpeg")
    ffmpeg.arguments = ["-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=64x64:d=2",
                        "-f", "lavfi", "-i", "sine=frequency=440:duration=2:sample_rate=44100",
                        "-ac", "2", "-shortest", "-c:v", "libx264", "-c:a", "aac", video.path]
    try ffmpeg.run()
    ffmpeg.waitUntilExit()
    #expect(ffmpeg.terminationStatus == 0)

    let wav = try await AudioExtractor.extract(from: video, to: dir.appending(path: "out.wav"))
    let file = try AVAudioFile(forReading: wav)
    #expect(file.fileFormat.sampleRate == 16_000)
    #expect(file.fileFormat.channelCount == 1)
    let seconds = Double(file.length) / file.fileFormat.sampleRate
    #expect(abs(seconds - 2) < 0.15)
}
