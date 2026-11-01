import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Behavior added to match the Windows app: playback failures, export write errors, stage percents, ⌘O, delete notices,
/// model ratings and the line context menu.
@MainActor
@Suite struct MacParityTests {
    private func makeModel(engine: EngineClient? = nil) -> SubtitleViewModel {
        SubtitleViewModel(catalog: ModelCatalog(engine: engine), defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!)
    }

    private func line(_ id: Int, _ start: Double, _ end: Double, _ text: String) -> Sentence {
        Sentence(id: id, start: start, end: end, text: text)
    }

    private struct Boom: LocalizedError {
        var errorDescription: String? { "disk dolu" }
    }

    // MARK: - Playback failure

    @Test func playbackFailureIsRecordedAndStaleLoadsAreIgnored() {
        let player = PlayerModel()
        #expect(player.failureMessage == nil)
        player.reportFailure(Boom(), generation: 99)
        #expect(player.failure == nil)
        player.reportFailure(Boom())
        #expect(player.failure == .system("disk dolu"))
        #expect(player.failureMessage?.text.contains("disk dolu") == true)
        // The first failure of a load wins; a later one doesn't replace it.
        player.reportFailure(nil)
        #expect(player.failure == .system("disk dolu"))
        player.load(nil)
        #expect(player.failure == nil)
    }

    @Test func notPlayableAssetGetsAGenericReason() {
        let player = PlayerModel()
        player.reportFailure(nil)
        #expect(player.failure == .notPlayable)
    }

    @Test func genericPlaybackReasonFollowsTheUILanguage() {
        let original = LanguageState.shared.language
        defer { LanguageState.shared.language = original }
        let player = PlayerModel()
        player.reportFailure(nil)
        LanguageState.shared.language = .tr
        let turkish = player.failureMessage
        #expect(turkish?.text.contains("Bu biçim oynatılamıyor.") == true)
        LanguageState.shared.language = .en
        let english = player.failureMessage
        #expect(english?.text.contains("Bu biçim") == false)
        #expect(english?.text.contains("Couldn't play the video") == true)
        // The identity doesn't depend on the language.
        #expect(turkish?.id == english?.id)
    }

    @Test func dismissingTheFailureHidesItForTheRestOfTheLoad() {
        let player = PlayerModel()
        player.reportFailure(Boom())
        player.dismissFailure()
        #expect(player.failureMessage == nil)
        player.reportFailure(Boom())
        #expect(player.failureMessage == nil)
        // A new load can fail (and be shown) again.
        player.load(nil)
        player.reportFailure(Boom())
        #expect(player.failureMessage != nil)
    }

    @Test func unreadableVideoSurfacesAFailure() async throws {
        let url = FileManager.default.temporaryDirectory.appending(path: "broken-\(UUID().uuidString).mp4")
        try Data("this is not a video".utf8).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        let player = PlayerModel()
        player.load(url)
        for _ in 0..<100 where player.failure == nil {
            try await Task.sleep(for: .milliseconds(50))
        }
        #expect(player.failure != nil)
        // Loading something else starts clean.
        player.load(nil)
        #expect(player.failure == nil)
    }

    @Test func playbackNoticeSitsNextToACalmEditorStatus() {
        let vm = makeModel()
        let failure = LocalizedMessage.verbatim("oynatılamadı")
        #expect(vm.playbackNotice(nil) == nil)
        #expect(vm.playbackNotice(failure) == failure)
        vm.sentences = [line(1, 0, 1, "a"), line(2, 1, 2, "b")]
        vm.status = .finished
        #expect(vm.playbackNotice(failure) == failure)
        // The notice is separate: the status and its summary are untouched, so the bar can show both.
        #expect(vm.status == .finished)
        #expect(vm.finishedSummary.contains("2"))
        vm.status = .failed(.verbatim("başka hata"))
        #expect(vm.playbackNotice(failure) == nil)
        vm.status = .working(.verbatim("çalışıyor"), fraction: 0.5)
        #expect(vm.playbackNotice(failure) == nil)
        vm.status = .idle
        vm.workspace = .playlist
        #expect(vm.playbackNotice(failure) == nil)
    }

    // MARK: - Export write errors

    @Test func exportWriteFailureIsReportedLikeSave() {
        let vm = makeModel()
        vm.exportFinished(.failure(Boom()))
        guard case .failed(let message) = vm.status else {
            Issue.record("expected a failed status")
            return
        }
        #expect(message.text.contains("disk dolu"))
        #expect(message.id == "save-failed:disk dolu")
    }

    @Test func cancelledExportIsNotAnError() {
        let vm = makeModel()
        vm.exportFinished(.failure(CocoaError(.userCancelled)))
        #expect(vm.status == .idle)
        vm.exportFinished(.success(URL(filePath: "/tmp/x.srt")))
        #expect(vm.status == .idle)
    }

    @Test func successfulExportClearsAnEarlierSaveFailureOnly() {
        let vm = makeModel()
        vm.exportFinished(.failure(Boom()))
        vm.exportFinished(.success(URL(filePath: "/tmp/x.srt")))
        #expect(vm.status == .finished)
        // Another kind of error stays visible.
        vm.status = .failed(.verbatim("motor hatası"))
        vm.exportFinished(.success(URL(filePath: "/tmp/x.srt")))
        #expect(vm.status == .failed(.verbatim("motor hatası")))
    }

    // MARK: - Stage labels

    @Test func audioAndSpeechStagesShowPercent() {
        func text(_ stage: EngineEvent.Stage, _ fraction: Double?) -> String {
            SubtitleViewModel.label(for: stage, fraction: fraction, purpose: .transcription).text
        }
        // Works in either UI language: Turkish writes "%46", English "46%".
        #expect(text(.extractingAudio, 0.456).contains("46"))
        #expect(text(.transcribing, 0.999).contains("100"))
        #expect(text(.transcribing, 1.7).contains("100"))
        #expect(text(.transcribing, nil).allSatisfy { !$0.isNumber })
        #expect(text(.extractingAudio, nil).allSatisfy { !$0.isNumber })
        #expect(SubtitleViewModel.percent(-1) == 0)
        #expect(SubtitleViewModel.percent(0.005) == 1)
    }

    @Test func newStringsHaveEnglish() {
        let en = UILanguage.en
        #expect(en.string("Ses ayrılıyor… %lld%%", in: .module, [Int(7)]) == "Extracting audio… 7%")
        #expect(en.string("Konuşma metne çevriliyor… %lld%%", in: .module, [Int(7)]) == "Transcribing speech… 7%")
        #expect(UILanguage.tr.string("Ses ayrılıyor… %lld%%", in: .module, [Int(7)]) == "Ses ayrılıyor… %7")
        #expect(en.string("Video oynatılamadı: %@", in: .module, ["x"]) == "Couldn't play the video: x")
        #expect(en.string("Hız %lld/5 · Doğruluk %lld/5", in: .module, [Int(4), Int(2)]) == "Speed 4/5 · Accuracy 2/5")
        #expect(en.string("%@ boşaltıldı.", in: .module, ["1 GB"]) == "1 GB freed.")
        #expect(en.string("Satırlar", in: .module) == "Lines")
        #expect(en.string("Yazı rengini seç", in: .module) == "Choose the text color")
        #expect(en.string("Satırı Sil (⌫)", in: .module) == "Delete Line (⌫)")
        #expect(en.string("Video dosyası seç (⌘O)", in: .module) == "Choose a video file (⌘O)")
    }

    // MARK: - ⌘O

    // The picker action is published per window (`FocusedValues.chooseVideo`), so ⌘O reaches only the key window; the
    // view model keeps no shared request flag. It is offered only while a video can be picked.
    @Test func videoPickerIsAvailableOnlyInTheEditor() {
        let vm = makeModel()
        #expect(vm.canPickVideo)
        vm.workspace = .playlist
        #expect(!vm.canPickVideo)
    }

    @Test func openVideoMenuTitleHasEllipsisInBothLanguages() {
        #expect(UILanguage.tr.string("Video aç…", in: .module) == "Video aç…")
        #expect(UILanguage.en.string("Video aç…", in: .module) == "Open Video…")
    }

    // MARK: - Delete notice

    private static let downloadedModels = """
    echo '{"event": "models", "models": [{"id": "small", "size_mb": 465, "speed": 4, "accuracy": 3, "downloaded": true}]}'
    """

    @Test func deleteReportsFreedSpace() async throws {
        let engine = try FakeEngine.make(
            models: Self.downloadedModels,
            deleteModel: #"echo '{"event": "deleted", "id": "small", "freed_mb": 1536}'"#
        )
        let catalog = ModelCatalog(engine: engine)
        await catalog.load()
        let model = try #require(catalog.models.first { $0.id == "small" })
        await catalog.delete(model)
        let notice = try #require(catalog.deleteNotice)
        #expect(notice.id == "deleted-freed:small:1536")
        #expect(notice.text.contains("1") && !notice.text.isEmpty)
        catalog.dismissDeleteNotice()
        #expect(catalog.deleteNotice == nil)
    }

    @Test func deleteWithNothingFreedStillReadsSensibly() {
        let notice = ModelCatalog.deleteNotice(for: "small", freedMB: 0)
        #expect(notice.id == "deleted-shared:small")
        #expect(!notice.text.contains("0 MB"))
        #expect(notice.text.contains(ModelLabel.name(for: "small")))
    }

    @Test func deleteNoticeDisappearsOnItsOwn() async throws {
        let engine = try FakeEngine.make(
            models: Self.downloadedModels,
            deleteModel: #"echo '{"event": "deleted", "id": "small", "freed_mb": 464}'"#
        )
        let catalog = ModelCatalog(engine: engine, noticeDuration: .milliseconds(50))
        await catalog.load()
        await catalog.delete(try #require(catalog.models.first { $0.id == "small" }))
        #expect(catalog.deleteNotice != nil)
        for _ in 0..<100 where catalog.deleteNotice != nil {
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(catalog.deleteNotice == nil)
    }

    @Test func failedDeleteShowsNoNotice() async throws {
        let engine = try FakeEngine.make(
            models: Self.downloadedModels,
            deleteModel: "echo '{\"event\": \"error\", \"message\": \"olmadı\"}'; exit 1"
        )
        let catalog = ModelCatalog(engine: engine)
        await catalog.load()
        await catalog.delete(try #require(catalog.models.first { $0.id == "small" }))
        #expect(catalog.deleteNotice == nil)
        #expect(catalog.deleteErrors["small"] != nil)
    }

    // MARK: - Ratings

    @Test func ratingTextShowsBothScales() {
        let rated = EngineModel(id: "small", sizeMB: 465, speed: 4, accuracy: 3, license: "MIT", downloaded: false)
        #expect(ModelLabel.ratingText(for: rated)?.contains("4") == true)
        #expect(ModelLabel.ratingText(for: rated)?.contains("3") == true)
        let unrated = EngineModel(id: "x", sizeMB: 1, speed: 0, accuracy: 0, license: nil, downloaded: false)
        #expect(ModelLabel.ratingText(for: unrated) == nil)
        let over = EngineModel(id: "y", sizeMB: 1, speed: 9, accuracy: 5, license: nil, downloaded: false)
        #expect(ModelLabel.ratingText(for: over)?.contains("9") == false)
    }

    // MARK: - Row context menu

    private func editorModel() -> SubtitleViewModel {
        let vm = makeModel()
        vm.sentences = [line(1, 0, 4, "bir iki üç dört"), line(2, 4, 8, "beş altı"), line(3, 8, 10, "yedi sekiz")]
        return vm
    }

    @Test func rowSplitAndMergeNeedASingleLine() {
        let vm = editorModel()
        let (first, second, third) = (vm.sentences[0].uid, vm.sentences[1].uid, vm.sentences[2].uid)
        #expect(vm.canSplitRow(first, at: 2))
        #expect(!vm.canSplitRow(first, at: 6))  // the time must lie inside the row's line
        #expect(vm.canMergeRow(first))
        #expect(!vm.canMergeRow(third))  // nothing after the last line
        // A multi-selection containing the row: split and merge are off, delete covers all of it.
        vm.setSelectedLines([first, second], active: first)
        #expect(!vm.canSplitRow(first, at: 2))
        #expect(!vm.canMergeRow(second))
        #expect(vm.canDeleteRow(second))
        // A row outside the selection acts alone.
        #expect(vm.canMergeRow(first) == false)
        #expect(vm.canMergeRow(third) == false)
        #expect(vm.canSplitRow(third, at: 9))
    }

    @Test func rowActionsActOnTheRowUnderThePointer() {
        let vm = editorModel()
        let (first, third) = (vm.sentences[0].uid, vm.sentences[2].uid)
        // Another line is selected; the menu was opened on the first row, so that one is merged.
        vm.setSelectedLines([third], active: third)
        vm.mergeRowWithNext(first)
        #expect(vm.sentences.count == 2)
        #expect(vm.sentences[0].text.contains("beş"))
        #expect(vm.selection == first)
        vm.splitRow(first, at: 2)
        #expect(vm.sentences.count == 3)
    }

    @Test func rowDeleteTakesTheWholeSelectionInsideItAndOnlyTheRowOutside() {
        let vm = editorModel()
        let (first, second, third) = (vm.sentences[0].uid, vm.sentences[1].uid, vm.sentences[2].uid)
        vm.setSelectedLines([first, second], active: first)
        vm.deleteRow(third)  // outside the selection: only this row
        #expect(vm.sentences.map(\.uid) == [first, second])
        vm.setSelectedLines([first, second], active: first)
        vm.deleteRow(second)  // inside: the whole selection
        #expect(vm.sentences.isEmpty)
    }

    @Test func rowActionsAreOffWhileAJobRuns() {
        let vm = editorModel()
        vm.status = .working(.verbatim("çalışıyor"), fraction: nil)
        let uid = vm.sentences[0].uid
        #expect(!vm.canSplitRow(uid, at: 2))
        #expect(!vm.canMergeRow(uid))
        #expect(!vm.canDeleteRow(uid))
        vm.deleteRow(uid)
        #expect(vm.sentences.count == 3)
    }
}
