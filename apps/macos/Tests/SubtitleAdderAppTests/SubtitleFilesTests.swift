import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Importing subtitle files, sidecars, drops, saving, auto-save and "Komple çevir" in the view model.
@MainActor
@Suite struct SubtitleFilesTests {
    private static let transcribeScript = """
    echo '{"event": "result", "language": "tr", "duration": 2, "audio": "/nonexistent.wav", "outputs": [], "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Merhaba.", "words": []}, {"id": 2, "start": 1, "end": 2, "text": "Dünya.", "words": []}]}'
    """
    private static let translateScript = """
    echo '{"event": "translation", "language": "en", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hello.", "words": []}, {"id": 2, "start": 1, "end": 2, "text": "World.", "words": []}]}'
    """
    private static let modelsEvent = """
    echo '{"event": "models", "models": [\
    {"id": "base", "size_mb": 145, "speed": 5, "accuracy": 2, "downloaded": true},\
    {"id": "large-v3", "size_mb": 3000, "speed": 1, "accuracy": 5, "downloaded": true},\
    {"id": "madlad-3b", "kind": "translation", "backend": "ctranslate2", "size_mb": 2839, "speed": 2, "accuracy": 5, "downloaded": false}\
    ]}'
    """

    /// A folder to put videos and subtitle files in.
    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "files-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func makeModel(engine: EngineClient? = nil) -> SubtitleViewModel {
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine),
            defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        return vm
    }

    private func write(_ text: String, to url: URL) throws {
        try Data(text.utf8).write(to: url)
    }

    private static let srt = """
    1
    00:00:00,000 --> 00:00:01,000
    Merhaba.

    2
    00:00:01,000 --> 00:00:02,000
    Dünya.
    """

    /// Waits for the running job and the one it hands over to (Komple çevir).
    private func settle(_ vm: SubtitleViewModel) async {
        while let job = vm.job {
            await job.value
            if vm.job == job { break }
        }
    }

    // MARK: - Import

    @Test func importKeepsTheVideoAndResetsTranslationsAndDisplay() throws {
        let folder = try makeFolder()
        let vm = makeModel()
        let video = folder.appending(path: "clip.mp4")
        vm.openVideo(video)
        vm.detectedLanguage = "en"
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Old.")]
        vm.applyTranslation(
            [Sentence(id: 1, start: 0, end: 1, text: "Eski.")], language: "tr", source: vm.sentences
        )
        #expect(vm.displayMode == .translation && vm.exportLanguage == .translated)
        vm.draft = SubtitleViewModel.TextDraft(uid: vm.sentences[0].uid, text: "x")

        let file = folder.appending(path: "other.srt")
        try write(Self.srt, to: file)
        #expect(vm.loadSubtitleFile(file))
        #expect(vm.video == video)
        #expect(vm.loadedSubtitle == file)
        #expect(vm.sentences.map(\.text) == ["Merhaba.", "Dünya."])
        #expect(vm.translations.isEmpty && vm.translationLanguage == nil && vm.draft == nil && vm.selection == nil)
        #expect(vm.displayMode == .original && vm.exportLanguage == .original)
        #expect(vm.status == .finished)
        #expect(vm.detectedLanguage == nil)
        // The language is unknown, so the summary doesn't pretend otherwise.
        #expect(!vm.finishedSummary.contains("?"))
        #expect(vm.finishedSummary == "2 satır")
    }

    @Test func importWithoutValidLinesFailsAndKeepsTheCurrentLines() throws {
        let folder = try makeFolder()
        let vm = makeModel()
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Kalır.")]
        let file = folder.appending(path: "empty.srt")
        try write("not a subtitle", to: file)
        #expect(!vm.loadSubtitleFile(file))
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text == "Altyazı dosyasında geçerli satır bulunamadı.")
        #expect(vm.sentences.map(\.text) == ["Kalır."])
        #expect(vm.loadedSubtitle == nil)
        #expect(!vm.loadSubtitleFile(folder.appending(path: "missing.srt")))
    }

    @Test func importedFileCancelsARunningJob() throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make())
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Eski.")]
        vm.translate()
        #expect(vm.isWorking)
        let file = folder.appending(path: "a.srt")
        try write(Self.srt, to: file)
        #expect(vm.loadSubtitleFile(file))
        #expect(vm.job == nil && !vm.isWorking)
    }

    // MARK: - Sidecar and drops

    @Test func openingAVideoLoadsItsSidecarOnly() throws {
        let folder = try makeFolder()
        try write(Self.srt, to: folder.appending(path: "clip.srt"))
        try write("1\n00:00:00,000 --> 00:00:01,000\nHola.\n", to: folder.appending(path: "other.es.srt"))
        let vm = makeModel()
        vm.openVideo(folder.appending(path: "clip.mp4"))
        #expect(vm.sentences.map(\.text) == ["Merhaba.", "Dünya."])
        #expect(vm.status == .finished)
        #expect(vm.loadedSubtitle?.lastPathComponent == "clip.srt")

        // A language-suffixed translation next to a video is not a sidecar.
        vm.openVideo(folder.appending(path: "other.mp4"))
        #expect(vm.sentences.isEmpty)
        #expect(vm.loadedSubtitle == nil)
        #expect(vm.status == .idle)
    }

    @Test func dropRoutesSubtitlesAndVideosByExtension() throws {
        let folder = try makeFolder()
        let vm = makeModel()
        let subtitle = folder.appending(path: "a.srt")
        try write(Self.srt, to: subtitle)

        vm.openFiles([subtitle])
        #expect(vm.video == nil)
        #expect(vm.sentences.count == 2)

        // Video plus subtitle: the subtitle is loaded after the video, replacing any sidecar.
        let video = folder.appending(path: "b.mov")
        try write("1\n00:00:00,000 --> 00:00:01,000\nSidecar.\n", to: folder.appending(path: "b.vtt"))
        vm.openFiles([subtitle, video])
        #expect(vm.video == video)
        #expect(vm.loadedSubtitle == subtitle)
        #expect(vm.sentences.map(\.text) == ["Merhaba.", "Dünya."])

        // Several videos go to the playlist; the editor keeps its video.
        let second = folder.appending(path: "c.mp4")
        vm.openFiles([second, folder.appending(path: "d.mp4")])
        #expect(vm.video == video)
        #expect(vm.playlist.map(\.fileName) == ["c.mp4", "d.mp4"])
        #expect(vm.workspace == .playlist)
    }

    // MARK: - Saving

    @Test func fileNamesFollowTheOriginalAndTranslationRule() {
        #expect(SubtitleViewModel.subtitleFileName(stem: "Film", language: nil, format: .srt) == "Film.srt")
        #expect(SubtitleViewModel.subtitleFileName(stem: "Film", language: "es", format: .vtt) == "Film.es.vtt")
    }

    @Test func saveWritesOriginalAndTranslationNextToTheVideo() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make())
        let video = folder.appending(path: "Film.mp4")
        vm.openVideo(video)
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Merhaba.")]
        #expect(vm.canSaveSubtitle)

        let original = await vm.saveSubtitle()
        #expect(original == folder.appending(path: "Film.srt"))
        let bytes = try Data(contentsOf: folder.appending(path: "Film.srt"))
        #expect(!bytes.starts(with: [0xEF, 0xBB, 0xBF]))
        #expect(String(decoding: bytes, as: UTF8.self).contains("Merhaba."))
        #expect(vm.status == .finished)
        #expect(vm.finishedSummary.hasSuffix(" · kaydedildi: Film.srt"))

        vm.applyTranslation([Sentence(id: 1, start: 0, end: 1, text: "Hola.")], language: "es", source: vm.sentences)
        vm.exportFormat = .vtt
        let translated = await vm.saveSubtitle()
        #expect(translated == folder.appending(path: "Film.es.vtt"))
        #expect(String(decoding: try Data(contentsOf: folder.appending(path: "Film.es.vtt")), as: UTF8.self).contains("Hola."))
        // The original file was left alone.
        #expect(FileManager.default.fileExists(atPath: folder.appending(path: "Film.srt").path))
        #expect(vm.finishedSummary.hasSuffix(" · kaydedildi: Film.es.vtt"))
    }

    @Test func saveWithoutVideoUsesTheLoadedSubtitleName() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make())
        let file = folder.appending(path: "Dizi.vtt")
        try write("WEBVTT\n\n00:00.000 --> 00:01.000\nMerhaba.\n", to: file)
        vm.loadSubtitleFile(file)
        #expect(vm.canSaveSubtitle)
        #expect(vm.exportFilename == "Dizi.srt")
        vm.applyTranslation([Sentence(id: 1, start: 0, end: 1, text: "Hola.")], language: "es", source: vm.sentences)
        #expect(vm.exportFilename == "Dizi.es.srt")
        let url = await vm.saveSubtitle()
        #expect(url == folder.appending(path: "Dizi.es.srt"))
    }

    @Test func saveIsUnavailableWithoutAPlaceToWrite() {
        let vm = makeModel()
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "x")]
        #expect(vm.canExport && !vm.canSaveSubtitle)
        #expect(vm.exportFilename == "subtitles.srt")
    }

    @Test func saveFailureShowsALocalizedMessage() async throws {
        let vm = makeModel(engine: try FakeEngine.make())
        vm.openVideo(URL(filePath: "/nonexistent-folder-\(UUID().uuidString)/Film.mp4"))
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "x")]
        let url = await vm.saveSubtitle()
        #expect(url == nil)
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text.hasPrefix("Dosya kaydedilemedi: "))
    }

    // MARK: - Auto-save and Komple çevir

    @Test func autoSaveDefaultsOnAndPersists() {
        let defaults = UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let vm = SubtitleViewModel(catalog: ModelCatalog(engine: nil), defaults: defaults, launchState: EngineLaunchState())
        #expect(vm.autoSaveSubtitle)
        vm.autoSaveSubtitle = false
        #expect(defaults.bool(forKey: "autoSaveSubtitle") == false)
        let again = SubtitleViewModel(catalog: ModelCatalog(engine: nil), defaults: defaults, launchState: EngineLaunchState())
        #expect(!again.autoSaveSubtitle)
    }

    @Test func translateAutoSavesTheTranslationNextToTheVideo() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make(translate: Self.translateScript))
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Merhaba."), Sentence(id: 2, start: 1, end: 2, text: "Dünya.")]
        vm.translate()
        await settle(vm)
        #expect(vm.status == .finished)
        let saved = try String(contentsOf: folder.appending(path: "Film.en.srt"), encoding: .utf8)
        #expect(saved.contains("Hello.") && saved.contains("World."))
        #expect(vm.finishedSummary.hasSuffix(" · çeviri: en · kaydedildi: Film.en.srt"))
        #expect(!FileManager.default.fileExists(atPath: folder.appending(path: "Film.srt").path))
    }

    @Test func autoSaveSavesTheTranslationEvenWhenTheExportShowsTheOriginal() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make(translate: Self.translateScript))
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Merhaba."), Sentence(id: 2, start: 1, end: 2, text: "Dünya.")]
        vm.translate()
        await settle(vm)
        vm.exportLanguage = .original
        vm.translate()
        await settle(vm)
        #expect(vm.exportLanguage == .original)
        #expect(FileManager.default.fileExists(atPath: folder.appending(path: "Film.en.srt").path))
        #expect(!FileManager.default.fileExists(atPath: folder.appending(path: "Film.srt").path))
    }

    @Test func autoSaveOffWritesNothing() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make(translate: Self.translateScript))
        vm.autoSaveSubtitle = false
        vm.language = "tr"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Merhaba."), Sentence(id: 2, start: 1, end: 2, text: "Dünya.")]
        vm.translate()
        await settle(vm)
        #expect(vm.hasTranslations)
        #expect(try FileManager.default.contentsOfDirectory(atPath: folder.path).isEmpty)
    }

    @Test func autoSaveDoesNothingWhenTheFolderIsMissing() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "gone-\(UUID().uuidString)")
        let vm = makeModel(engine: try FakeEngine.make(translate: Self.translateScript))
        vm.language = "tr"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Merhaba."), Sentence(id: 2, start: 1, end: 2, text: "Dünya.")]
        vm.translate()
        await settle(vm)
        #expect(vm.status == .finished)
    }

    @Test func generateAloneNeverAutoSaves() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make(transcribe: Self.transcribeScript))
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.generate()
        await settle(vm)
        #expect(vm.sentences.count == 2)
        #expect(try FileManager.default.contentsOfDirectory(atPath: folder.path).isEmpty)
    }

    @Test func fullTranslateTranscribesTranslatesAndSaves() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try FakeEngine.make(transcribe: Self.transcribeScript, translate: Self.translateScript))
        vm.diarize = false
        vm.translationTarget = "en"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.generateAndTranslate()
        await settle(vm)
        #expect(vm.status == .finished)
        #expect(vm.detectedLanguage == "tr")
        #expect(vm.sentences.compactMap { vm.translation(for: $0)?.text } == ["Hello.", "World."])
        #expect(FileManager.default.fileExists(atPath: folder.appending(path: "Film.en.srt").path))
    }

    @Test func fullTranslateOnlyTranslatesWhenLinesAlreadyExist() async throws {
        let folder = try makeFolder()
        try write(Self.srt, to: folder.appending(path: "Film.srt"))
        // A transcribe call would fail the test: the fake engine's `transcribe` answers with an error event.
        let vm = makeModel(engine: try FakeEngine.make(
            transcribe: #"echo '{"event": "error", "message": "transcribed"}'"#,
            translate: Self.translateScript
        ))
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        #expect(vm.sentences.count == 2)
        vm.generateAndTranslate()
        await settle(vm)
        #expect(vm.status == .finished)
        #expect(vm.hasTranslations)
        #expect(FileManager.default.fileExists(atPath: folder.appending(path: "Film.en.srt").path))
        // The sidecar is still the source subtitle, untouched.
        #expect(try String(contentsOf: folder.appending(path: "Film.srt"), encoding: .utf8) == Self.srt)
    }

    @Test func fullTranslateCheckedOfflineIncludesTheTranslationModel() async throws {
        let folder = try makeFolder()
        let engine = try FakeEngine.make(
            models: Self.modelsEvent, transcribe: Self.transcribeScript, translate: Self.translateScript
        )
        let vm = makeModel(engine: engine)
        await vm.catalog.load()
        vm.offlineMode = true
        vm.diarize = false
        vm.model = "base"
        vm.translationModel = "madlad-3b"
        vm.openVideo(folder.appending(path: "Film.mp4"))
        vm.generateAndTranslate()
        guard case .failed(let message) = vm.status else { Issue.record("expected a failure"); return }
        #expect(message.text.hasPrefix("MADLAD-400 3B indirilmemiş."))
        #expect(vm.canGoOnlineAndRetry)
        #expect(vm.job == nil)

        // Going online runs the whole chain again, not just the transcription.
        vm.goOnlineAndRetry()
        await settle(vm)
        #expect(vm.status == .finished)
        #expect(vm.hasTranslations)
        #expect(FileManager.default.fileExists(atPath: folder.appending(path: "Film.en.srt").path))
    }

    @Test func fullTranslateAvailabilityFollowsTheWorkingState() {
        let vm = makeModel()
        #expect(vm.canGenerateAndTranslate && !vm.hasSourceForGenerateAndTranslate)
        vm.openVideo(URL(filePath: "/tmp/x.mp4"))
        #expect(vm.hasSourceForGenerateAndTranslate)
        vm.status = .working(.key("Çalışıyor…"), fraction: nil)
        #expect(!vm.canGenerateAndTranslate)
    }
}
