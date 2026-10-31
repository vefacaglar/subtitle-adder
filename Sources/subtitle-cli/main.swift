import Foundation
import SubtitleCore
import WhisperTranscriber

// Kullanım: subtitle-cli <video> [-o çıktı-dizini] [-l dil] [-m model] [-f srt,vtt,json]
var args = Array(CommandLine.arguments.dropFirst())
@MainActor func option(_ flag: String) -> String? {
    guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
    defer { args.removeSubrange(i...(i + 1)) }
    return args[i + 1]
}

let outDir = URL(filePath: option("-o") ?? "output")
let language = option("-l")
let model = option("-m") ?? "small"
let formats = (option("-f") ?? "srt").split(separator: ",").compactMap { SubtitleFormat(rawValue: String($0)) }
guard let videoPath = args.first else {
    FileHandle.standardError.write(Data("kullanım: subtitle-cli <video> [-o dizin] [-l tr] [-m small] [-f srt,vtt,json]\n".utf8))
    exit(2)
}
let video = URL(filePath: videoPath)

let pipeline = SubtitlePipeline(transcriber: WhisperKitTranscriber(model: model))
do {
    let result = try await pipeline.run(video: video, workDir: outDir, language: language) { stage in
        print("…", stage)
    }
    print("dil: \(result.language) | \(result.sentences.count) altyazı satırı")
    for format in formats {
        let url = outDir.appending(path: video.deletingPathExtension().lastPathComponent + "." + format.rawValue)
        try SubtitleWriter.write(result.sentences, to: url, as: format)
        print("->", url.path)
    }
} catch {
    FileHandle.standardError.write(Data("hata: \(error.localizedDescription)\n".utf8))
    exit(1)
}
