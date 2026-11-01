import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Translation progress (foreground, background and the badge that reports it) and the layout settings.
@MainActor
@Suite struct TranslationProgressTests {
    private func makeModel(engine: EngineClient? = nil, defaults: UserDefaults? = nil) -> SubtitleViewModel {
        let defaults = defaults ?? UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine), defaults: defaults, launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        return vm
    }

    private func line(_ id: Int, _ text: String) -> Sentence {
        Sentence(id: id, start: Double(id), end: Double(id) + 1, text: text)
    }

    private func lines(_ count: Int) -> [Sentence] {
        (1...count).map { line($0, "Satır \($0).") }
    }

    private func sentenceJSON(_ id: Int, _ text: String) -> String {
        #"{"id": \#(id), "start": \#(id), "end": \#(id + 1), "text": "\#(text)", "words": []}"#
    }

    /// Polls until `condition` holds (a fake engine's events arrive on their own schedule).
    private func wait(_ condition: () -> Bool) async throws {
        for _ in 0..<200 where !condition() { try await Task.sleep(for: .milliseconds(25)) }
    }

    // MARK: - The numbers

    @Test func progressIsUnknownUntilSomethingIsDoneAndNeverReachesOne() {
        #expect(SubtitleViewModel.translationProgress(done: 0, total: 10) == nil)
        #expect(SubtitleViewModel.translationProgress(done: 3, total: 0) == nil)
        #expect(SubtitleViewModel.translationProgress(done: 5, total: 10) == 0.5)
        #expect(SubtitleViewModel.translationProgress(done: 10, total: 10) == 0.99)
        #expect(SubtitleViewModel.translationProgress(done: 12, total: 10) == 0.99)
    }

    @Test func aRequestOnTopOfFinishedLinesMapsIntoTheWholeAndIsCapped() {
        // Two of four lines were already done; the request of two reports 0, half, all.
        func progress(_ part: Double) -> Double {
            SubtitleViewModel.translationProgress(alreadyDone: 2, requested: 2, part: part, total: 4)
        }
        #expect(progress(0) == 0.5)
        #expect(progress(0.5) == 0.75)
        #expect(progress(1) == 0.99)
        #expect(progress(7) == 0.99)
        #expect(progress(-1) == 0.5)
        // Nothing done before: the engine's fraction stands, except for the cap.
        #expect(SubtitleViewModel.translationProgress(alreadyDone: 0, requested: 4, part: 0.25, total: 4) == 0.25)
        #expect(SubtitleViewModel.translationProgress(alreadyDone: 0, requested: 4, part: 1, total: 4) == 0.99)
    }

    // MARK: - Foreground translation

    @Test func aReusingTranslationStartsWhereTheBackgroundLeftOffAndTheBadgeFollows() async throws {
        let engine = try FakeEngine.make(
            translate: #"echo '{"event": "stage", "stage": "translating", "fraction": 0.5}'; exec sleep 5"#
        )
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = lines(4)
        vm.translations = [
            vm.sentences[0].uid: TranslationEntry(text: "Line 1.", sourceText: "Satır 1.", language: "en"),
            // A copy of the source doesn't count as done.
            vm.sentences[1].uid: TranslationEntry(text: "Satır 2.", sourceText: "Satır 2.", language: "en"),
            vm.sentences[2].uid: TranslationEntry(text: "Line 3.", sourceText: "Satır 3.", language: "en"),
        ]
        #expect(vm.translationBadgeText == nil && !vm.isTranslationInProgress)
        vm.translate(reuseExisting: true)
        guard case .working(let label, let initial) = vm.status else { Issue.record("expected a working status"); return }
        #expect(initial == 0.5)
        #expect(label.text == L("Çevriliyor… %lld%%", 50))
        #expect(vm.isTranslationJobRunning && vm.isTranslationInProgress && vm.isWorking)
        #expect(vm.translationBadgeText == label.text)

        // The stage event maps the engine's half of the request (two lines) onto the whole: (2 + 2 * 0.5) / 4.
        try await wait { if case .working(_, 0.75?) = vm.status { true } else { false } }
        guard case .working(_, let fraction) = vm.status else { Issue.record("expected a working status"); return }
        #expect(fraction == 0.75)
        #expect(vm.translationBadgeText == L("Çevriliyor… %lld%%", 75))

        vm.cancel()
        #expect(!vm.isTranslationJobRunning && !vm.isTranslationInProgress && vm.translationBadgeText == nil)
    }

    @Test func aFreshTranslationIsPreparingWithoutAFractionAndTheFlagEndsWithTheJob() async throws {
        let engine = try FakeEngine.make(
            translate: """
            echo '{"event": "translation", "language": "en", "sentences": [\(sentenceJSON(1, "One."))]}'
            """
        )
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.sentences = [line(1, "Bir.")]
        vm.translate()
        guard case .working(let label, let fraction) = vm.status else { Issue.record("expected a working status"); return }
        #expect(fraction == nil && label.text == L("Çeviri hazırlanıyor…"))
        #expect(vm.translationBadgeText == L("Çeviri hazırlanıyor…"))
        while let job = vm.job {
            await job.value
            if vm.job == job { break }
        }
        #expect(vm.status == .finished)
        #expect(!vm.isTranslationJobRunning && !vm.isTranslationInProgress && vm.translationBadgeText == nil)
    }

    @Test func aFailedTranslationAlsoEndsTheFlag() async throws {
        let engine = try FakeEngine.make(translate: #"echo '{"event": "error", "message": "boom"}'; exit 1"#)
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = [line(1, "Bir.")]
        vm.translate()
        #expect(vm.isTranslationJobRunning)
        await vm.job?.value
        guard case .failed = vm.status else { Issue.record("expected a failure"); return }
        #expect(!vm.isTranslationJobRunning && !vm.isTranslationInProgress)
    }

    // MARK: - Background translation

    @Test func theBackgroundFlagFollowsTheRunAndKeepsTheControlsDisabled() async throws {
        let transcribe = """
        echo '{"event": "partial", "language": "tr", "sentences": [\((1...12).map { sentenceJSON($0, "Satır \($0).") }.joined(separator: ", "))]}'
        exec sleep 5
        """
        let engine = try FakeEngine.make(transcribe: transcribe, translate: "exec sleep 5")
        let vm = makeModel(engine: engine)
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:11434"
        vm.externalLlmAsync = true
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.diarize = false
        #expect(!vm.isAsyncTranslating)
        vm.openVideo(URL(filePath: "/tmp/never-opened.mp4"))
        vm.generateAndTranslate()
        try await wait { vm.isAsyncTranslating }
        #expect(vm.isAsyncTranslating && vm.isWorking && vm.isTranslationInProgress)
        // Nothing translated yet: the share is unknown, the text says only that it runs.
        #expect(vm.asyncTranslationFraction == nil)
        #expect(vm.translationBadgeText == L("Çevriliyor…"))
        #expect(vm.asyncStatusDetail == L("Çevriliyor…"))

        vm.cancel()
        #expect(!vm.isAsyncTranslating && vm.asyncTranslationFraction == nil && !vm.isWorking)
        #expect(!vm.isTranslationInProgress && vm.translationBadgeText == nil && vm.asyncStatusDetail == nil)
    }

    @Test func theEnginesFractionForABatchIsAddedToWhatWasTranslatedBefore() async throws {
        let engine = try FakeEngine.make(
            translate: #"echo '{"event": "stage", "stage": "translating", "fraction": 0.5}'; exec sleep 5"#
        )
        let vm = makeModel()
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = lines(12)
        vm.startAsyncTranslation(engine: engine)
        try await wait { vm.asyncTranslationFraction != nil }
        // Ten lines go out of twelve: half of them is 5 / 12.
        #expect(vm.asyncTranslationFraction == 5.0 / 12.0)
        #expect(vm.isAsyncTranslating)
        #expect(vm.translationBadgeText == L("Çevriliyor… %lld%%", 42))
        vm.cancelAsyncTranslation()
        #expect(!vm.isAsyncTranslating && vm.asyncTranslationFraction == nil)
    }

    @Test func translationEventsMoveTheShareAndItStaysBelowOne() async throws {
        // Ten of twelve lines come back: 10 / 12. The last two were only context.
        let ten = (1...10).map { sentenceJSON($0, "Line \($0).") }.joined(separator: ", ")
        let engine = try FakeEngine.make(
            translate: #"echo '{"event": "translation", "language": "en", "sentences": [\#(ten)]}'"#
        )
        let vm = makeModel()
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = lines(12)
        vm.startAsyncTranslation(engine: engine)
        await vm.asyncTask?.value
        #expect(vm.translations.count == 10)
        #expect(vm.asyncTranslationFraction == 10.0 / 12.0)
        // Everything translated while lines are still coming in: below one.
        vm.sentences = lines(10)
        vm.updateAsyncFraction()
        #expect(vm.asyncTranslationFraction == 0.99)
    }

    @Test func aFailedBatchEndsTheBackgroundFlag() async throws {
        let engine = try FakeEngine.make(translate: #"echo '{"event": "error", "message": "down"}'; exit 1"#)
        let vm = makeModel()
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = lines(12)
        vm.startAsyncTranslation(engine: engine)
        await vm.asyncTask?.value
        #expect(vm.asyncFailed)
        #expect(!vm.isAsyncTranslating && vm.asyncTranslationFraction == nil && !vm.isWorking)
    }

    // MARK: - Layout

    @Test func theLayoutChoiceIsPersistedAndTheaterModeIsNot() {
        let defaults = UserDefaults(suiteName: "test-\(UUID().uuidString)")!
        let vm = makeModel(defaults: defaults)
        #expect(!vm.sideBySideLayout && !vm.isTheaterMode)
        #expect(defaults.object(forKey: "sideBySideLayout") == nil)
        vm.sideBySideLayout = true
        vm.isTheaterMode = true
        #expect(defaults.bool(forKey: "sideBySideLayout"))
        let next = makeModel(defaults: defaults)
        #expect(next.sideBySideLayout && !next.isTheaterMode)
        next.sideBySideLayout = false
        #expect(!makeModel(defaults: defaults).sideBySideLayout)
    }
}
