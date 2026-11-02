import Foundation
import Testing
@testable import EngineKit

@Suite struct MediaFileTypesTests {
    @Test func recognizesVideoAndAudioExtensionsIgnoringCase() {
        for name in ["a.mp4", "a.MOV", "a.m4v", "a.mkv", "a.avi", "a.webm", "a.wmv", "a.mpg", "a.mpeg", "a.ts", "a.mts",
                     "a.m2ts", "a.3gp", "a.flv", "a.MP3", "a.wav", "a.m4a", "a.aac", "a.flac", "a.ogg", "a.opus", "a.wma"] {
            #expect(MediaFileTypes.isMedia(URL(filePath: "/x/\(name)")), "\(name)")
        }
        for name in ["a.txt", "a.srt", "a.json", "a.jpg", "a", "mp4", ".mp4.bak"] {
            #expect(!MediaFileTypes.isMedia(URL(filePath: "/x/\(name)")), "\(name)")
        }
    }

    @Test func audioIsASubsetOfMedia() {
        for ext in MediaFileTypes.audioExtensions {
            let url = URL(filePath: "/x/a.\(ext.uppercased())")
            #expect(MediaFileTypes.isAudio(url))
            #expect(MediaFileTypes.isMedia(url))
        }
        for ext in MediaFileTypes.videoExtensions {
            #expect(!MediaFileTypes.isAudio(URL(filePath: "/x/a.\(ext)")))
        }
        #expect(!MediaFileTypes.isAudio(URL(filePath: "/x/a.txt")))
        #expect(MediaFileTypes.mediaExtensions.count == MediaFileTypes.videoExtensions.count + MediaFileTypes.audioExtensions.count)
        #expect(Set(MediaFileTypes.mediaExtensions).count == MediaFileTypes.mediaExtensions.count)
    }
}
