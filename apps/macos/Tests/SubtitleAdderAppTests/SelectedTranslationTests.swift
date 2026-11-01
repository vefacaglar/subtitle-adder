import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// Multi-selection, "Seçilenleri yeniden çevir" and the `needs_review` marker.
@MainActor
@Suite struct SelectedTranslationTests {
    private func makeModel(engine: EngineClient? = nil) -> SubtitleViewModel {
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine),
            defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        return vm
    }

    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "selected-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func settle(_ vm: SubtitleViewModel) async {
        while let job = vm.job {
            await job.value
            if vm.job == job { break }
        }
    }

    private func lines(_ ids: ClosedRange<Int>) -> [Sentence] {
        ids.map { Sentence(id: $0, start: Double($0), end: Double($0) + 1, text: "Line \($0)") }
    }

    /// Answers every line it receives with "T-<text>", after `delay` seconds; the request is saved to `request`.
    private func echoEngine(request: URL, delay: String = "0") throws -> EngineClient {
        try FakeEngine.make(
            translate: """
            sleep \(delay)
            printf '{"event": "translation", "language": "en", "sentences": %s}\\n' \
            "$(sed 's/"text":"\\([^"]*\\)"/"text":"T-\\1"/g' '\(request.path)')"
            """,
            translateInput: "cat > '\(request.path)'"
        )
    }

    private func uids(_ vm: SubtitleViewModel, _ ids: [Int]) -> Set<UUID> {
        Set(vm.sentences.filter { ids.contains($0.id) }.map(\.uid))
    }

    private func texts(_ vm: SubtitleViewModel) -> [String?] {
        vm.sentences.map { vm.translation(for: $0)?.text }
    }

    // MARK: - Selection model

    @Test func settingTheActiveLineSelectsOnlyIt() {
        let vm = makeModel()
        vm.sentences = lines(1...4)
        vm.setSelectedLines(uids(vm, [2, 3]), active: vm.sentences[2].uid)
        #expect(vm.selectedUIDs == uids(vm, [2, 3]) && vm.selection == vm.sentences[2].uid)
        vm.selection = vm.sentences[0].uid
        #expect(vm.selectedUIDs == uids(vm, [1]))
        vm.selection = nil
        #expect(vm.selectedUIDs.isEmpty)
    }

    @Test func activeLineFallsBackToThePreviousThenTheFirstInDocumentOrder() {
        let vm = makeModel()
        vm.sentences = lines(1...5)
        vm.setSelectedLines(uids(vm, [2, 4]), active: vm.sentences[3].uid)
        // The requested active line is not selected: the previous one stays.
        vm.setSelectedLines(uids(vm, [2, 4]), active: vm.sentences[0].uid)
        #expect(vm.selection == vm.sentences[3].uid)
        // The previous one is gone: the first selected line in document order.
        vm.setSelectedLines(uids(vm, [5, 2]), active: nil)
        #expect(vm.selection == vm.sentences[1].uid)
        vm.setSelectedLines([], active: nil)
        #expect(vm.selection == nil && vm.selectedUIDs.isEmpty)
    }

    @Test func listSelectionMakesTheLastAddedLineActive() {
        let vm = makeModel()
        vm.sentences = lines(1...6)
        vm.updateSelection(to: uids(vm, [2]))
        #expect(vm.selection == vm.sentences[1].uid)
        vm.updateSelection(to: uids(vm, [2, 4, 5]))
        #expect(vm.selection == vm.sentences[4].uid)
        // Deselecting the active line moves on to the first remaining one.
        vm.updateSelection(to: uids(vm, [2, 4]))
        #expect(vm.selection == vm.sentences[1].uid)
    }

    @Test func selectingARowKeepsAMultiSelectionItBelongsTo() {
        let vm = makeModel()
        vm.sentences = lines(1...5)
        vm.setSelectedLines(uids(vm, [2, 4]), active: vm.sentences[1].uid)
        vm.selectRow(vm.sentences[3].uid)
        #expect(vm.selectedUIDs == uids(vm, [2, 4]) && vm.selection == vm.sentences[3].uid)
        vm.selectRow(vm.sentences[4].uid)
        #expect(vm.selectedUIDs == uids(vm, [5]) && vm.selection == vm.sentences[4].uid)
    }

    @Test func removedAndUnknownLinesCannotRemainSelectedOrStartAJob() async throws {
        let folder = try makeFolder()
        let request = folder.appending(path: "request.json")
        let vm = makeModel(engine: try echoEngine(request: request))
        vm.sentences = lines(1...3)
        vm.setSelectedLines(uids(vm, [2]).union([UUID()]), active: UUID())
        #expect(vm.selectedUIDs == uids(vm, [2]) && vm.selection == vm.sentences[1].uid)
        vm.sentences.remove(at: 1)
        #expect(vm.selectedUIDs.isEmpty && vm.selection == nil)
        #expect(!vm.canTranslateSelected)
        vm.translateSelected()
        await settle(vm)
        #expect(!FileManager.default.fileExists(atPath: request.path))
        #expect(vm.status == .idle)
    }

    // MARK: - Retranslating the selection

    @Test func selectedRetryKeepsOtherTranslationsAndSnapshotsTheSelection() async throws {
        let folder = try makeFolder()
        let request = folder.appending(path: "request.json")
        let vm = makeModel(engine: try echoEngine(request: request, delay: "0.4"))
        vm.sentences = lines(1...10)
        vm.applyTranslation(
            vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "Old \($0.id)") },
            language: "en", source: vm.sentences
        )
        vm.setSelectedLines(uids(vm, [2, 9]), active: vm.sentences[1].uid)
        #expect(vm.canTranslateSelected)
        vm.translateSelected()
        #expect(!vm.canTranslateSelected)
        // Changing the selection mid-job changes nothing about what the job applies.
        vm.setSelectedLines(uids(vm, [5]), active: vm.sentences[4].uid)
        await settle(vm)

        let sent = try JSONDecoder().decode([Sentence].self, from: Data(contentsOf: request))
        #expect(sent.map(\.id) == [1, 2, 3, 4, 7, 8, 9, 10])
        // Lines 1 and 3 came back as context but only 2 and 9 changed.
        #expect(texts(vm) == (1...10).map { [2, 9].contains($0) ? "T-Line \($0)" : "Old \($0)" })
        #expect(vm.status == .finished)
        #expect(vm.selectedUIDs == uids(vm, [5]) && vm.canTranslateSelected)
    }

    @Test func aSelectedRetryAlsoResendsLinesThatAreAlreadyTranslated() async throws {
        let folder = try makeFolder()
        let request = folder.appending(path: "request.json")
        let vm = makeModel(engine: try echoEngine(request: request))
        vm.sentences = lines(1...3)
        vm.applyTranslation(
            [Sentence(id: 2, start: 0, end: 1, text: "Good two.")], language: "en", source: vm.sentences
        )
        vm.setSelectedLines(uids(vm, [2]), active: nil)
        vm.translateSelected()
        await settle(vm)
        let sent = try JSONDecoder().decode([Sentence].self, from: Data(contentsOf: request))
        #expect(sent.map(\.id) == [1, 2, 3])
        #expect(texts(vm) == [nil, "T-Line 2", nil])
    }

    @Test func aSelectedRetryKeepsTranslationsInOtherLanguages() async throws {
        let folder = try makeFolder()
        let request = folder.appending(path: "request.json")
        let vm = makeModel(engine: try echoEngine(request: request))
        vm.sentences = lines(1...3)
        vm.applyTranslation(
            vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "Hola \($0.id)") },
            language: "es", source: vm.sentences
        )
        vm.setSelectedLines(uids(vm, [3]), active: nil)
        vm.translateSelected()
        await settle(vm)
        #expect(texts(vm) == ["Hola 1", "Hola 2", "T-Line 3"])
        #expect(vm.translationLanguage == "en")
    }

    @Test func importedPartialSubtitleCanBeRepairedAndSavedWithoutVideo() async throws {
        let folder = try makeFolder()
        let file = folder.appending(path: "partial.srt")
        let srt = """
        1
        00:00:00,000 --> 00:00:01,000
        Merhaba.

        2
        00:00:01,000 --> 00:00:02,000
        Half translated line.

        3
        00:00:02,000 --> 00:00:03,000
        Dünya.

        """
        try srt.write(to: file, atomically: true, encoding: .utf8)
        let request = folder.appending(path: "request.json")
        let vm = makeModel(engine: try echoEngine(request: request))
        vm.autoSaveSubtitle = true
        #expect(vm.loadSubtitleFile(file))
        #expect(vm.video == nil && vm.selection == nil && vm.selectedUIDs.isEmpty)
        vm.setSelectedLines(uids(vm, [2]), active: nil)
        vm.translateSelected()
        await settle(vm)

        #expect(texts(vm) == [nil, "T-Half translated line.", nil])
        let exported = vm.exportSentences(translated: true).map(\.text)
        #expect(exported == ["Merhaba.", "T-Half translated line.", "Dünya."])
        // Auto-save wrote the translated export over the imported file (the fake renderer echoes the lines as JSON).
        let saved = try String(contentsOf: vm.subtitleSaveURL()!, encoding: .utf8)
        #expect(saved.contains("T-Half translated line.") && saved.contains("Merhaba."))
    }

    // MARK: - needs_review

    @Test func failedRetryKeepsPreviousTextAndMarksItUntilEdited() async throws {
        let engine = try FakeEngine.make(translate: """
        echo '{"event": "translation", "language": "en", "sentences": [\
        {"id": 1, "start": 0, "end": 1, "text": "Line 1", "words": [], "needs_review": true},\
        {"id": 2, "start": 1, "end": 2, "text": "Line 2", "words": [], "needs_review": true}]}'
        """)
        let vm = makeModel(engine: engine)
        vm.sentences = lines(1...2)
        vm.applyTranslation(
            [Sentence(id: 1, start: 0, end: 1, text: "Good one.")], language: "en", source: vm.sentences
        )
        vm.setSelectedLines(uids(vm, [1, 2]), active: nil)
        vm.translateSelected()
        await settle(vm)

        // Line 1 keeps its earlier translation, line 2 shows the source; both are flagged.
        let first = try #require(vm.translation(for: vm.sentences[0]))
        let second = try #require(vm.translation(for: vm.sentences[1]))
        #expect(first.text == "Good one." && first.needsReview)
        #expect(second.text == "Line 2" && second.needsReview)
        #expect(!SubtitleViewModel.isReusableTranslation(vm.sentences[0], entry: first, target: "en"))
        #expect(vm.linesNeedingTranslation(into: "en").count == 2)
        #expect(vm.exportSentences(translated: true).map(\.needsReview) == [true, true])

        // Editing the text makes it the user's own: the marker goes. The same text keeps it.
        vm.setTranslationText("Good one.", for: vm.sentences[0].uid)
        #expect(vm.translation(for: vm.sentences[0])?.needsReview == true)
        vm.setTranslationText("Better one.", for: vm.sentences[0].uid)
        let edited = try #require(vm.translation(for: vm.sentences[0]))
        #expect(edited.text == "Better one." && !edited.needsReview)
        #expect(SubtitleViewModel.isReusableTranslation(vm.sentences[0], entry: edited, target: "en"))
        #expect(vm.exportSentences(translated: true).map(\.needsReview) == [false, true])
    }

    @Test func undoRestoresTheMarker() throws {
        let vm = makeModel()
        let undo = UndoManager()
        vm.undoManager = undo
        vm.sentences = lines(1...1)
        vm.applyTranslation(
            [Sentence(id: 1, start: 0, end: 1, text: "Line 1", needsReview: true)],
            language: "en", source: vm.sentences
        )
        #expect(vm.translation(for: vm.sentences[0])?.needsReview == true)
        vm.setTranslationText("Fixed.", for: vm.sentences[0].uid)
        #expect(vm.translation(for: vm.sentences[0])?.needsReview == false)
        undo.undo()
        #expect(vm.translation(for: vm.sentences[0])?.needsReview == true)
    }

    @Test func aFlaggedLineKeepsItsEarlierTranslationInAFullRunWhenALaterBatchFlagsIt() async throws {
        // Batch 1 holds lines 1-2; batch 2 (a longer prefix) flags line 3, which had a translation before the job.
        let engine = try FakeEngine.make(translate: """
        echo '{"event": "translation", "language": "en", "sentences": [\
        {"id": 1, "start": 0, "end": 1, "text": "New 1", "words": []},\
        {"id": 2, "start": 1, "end": 2, "text": "New 2", "words": []}]}'
        echo '{"event": "translation", "language": "en", "sentences": [\
        {"id": 1, "start": 0, "end": 1, "text": "New 1", "words": []},\
        {"id": 2, "start": 1, "end": 2, "text": "New 2", "words": []},\
        {"id": 3, "start": 2, "end": 3, "text": "Line 3", "words": [], "needs_review": true},\
        {"id": 4, "start": 3, "end": 4, "text": "New 4", "words": []}]}'
        """)
        let vm = makeModel(engine: engine)
        vm.sentences = lines(1...4)
        vm.applyTranslation(
            vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "Old \($0.id)") },
            language: "en", source: vm.sentences
        )
        vm.translate()
        await settle(vm)
        #expect(texts(vm) == ["New 1", "New 2", "Old 3", "New 4"])
        #expect(vm.sentences.map { vm.translation(for: $0)?.needsReview } == [false, false, true, false])
    }

    @Test func aFlaggedLineInAFullRunIntoAnotherLanguageShowsTheSourceNotTheOldLanguage() async throws {
        let engine = try FakeEngine.make(translate: """
        echo '{"event": "translation", "language": "en", "sentences": [\
        {"id": 1, "start": 0, "end": 1, "text": "New 1", "words": []},\
        {"id": 2, "start": 1, "end": 2, "text": "Line 2", "words": [], "needs_review": true}]}'
        """)
        let vm = makeModel(engine: engine)
        vm.sentences = lines(1...2)
        vm.applyTranslation(
            vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "Hola \($0.id)") },
            language: "es", source: vm.sentences
        )
        // Both the whole-file run and the "only what is missing" run start from the Spanish translations.
        for reuse in [false, true] {
            vm.translationTarget = "en"
            vm.translate(reuseExisting: reuse)
            await settle(vm)
            let second = try #require(vm.translation(for: vm.sentences[1]))
            #expect(second.language == "en" && second.text == "Line 2" && second.needsReview)
            #expect(vm.exportSentences(translated: true).map(\.text) == ["New 1", "Line 2"])
            vm.applyTranslation(
                vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "Hola \($0.id)") },
                language: "es", source: vm.sentences
            )
        }
    }

    @Test func aSelectedRetryFailureStillKeepsTheOldLanguageText() {
        let vm = makeModel()
        vm.sentences = lines(1...1)
        vm.applyTranslation(
            [Sentence(id: 1, start: 0, end: 1, text: "Hola")], language: "es", source: vm.sentences
        )
        vm.applyTranslation(
            [Sentence(id: 1, start: 0, end: 1, text: "Line 1", needsReview: true)],
            language: "en", source: vm.sentences, merge: true, preserveOtherLanguages: true
        )
        let entry = vm.translation(for: vm.sentences[0])
        #expect(entry?.text == "Hola" && entry?.language == "es" && entry?.needsReview == true)
    }

    @Test func importedJSONIdsAreRenumberedSoNeighboursNeverLandOnTheSelectedLine() async throws {
        let folder = try makeFolder()
        let file = folder.appending(path: "dup.json")
        let json = (1...5).map { #"{"id": 0, "start": \#($0), "end": \#($0 + 1), "text": "Line \#($0)"}"# }
        try "[\(json.joined(separator: ","))]".write(to: file, atomically: true, encoding: .utf8)
        let request = folder.appending(path: "request.json")
        let vm = makeModel(engine: try echoEngine(request: request))
        #expect(vm.loadSubtitleFile(file))
        vm.setSelectedLines([vm.sentences[2].uid], active: nil)
        vm.translateSelected()
        await settle(vm)
        #expect(texts(vm) == [nil, nil, "T-Line 3", nil, nil])
    }

    // MARK: - Imported review flag

    @Test func anImportedFlagIsShownAndClearedByEditingTheLine() throws {
        let folder = try makeFolder()
        let file = folder.appending(path: "flagged.json")
        let json = """
        [{"id": 1, "start": 0, "end": 1, "text": "Good"},
         {"id": 2, "start": 1, "end": 2, "text": "Broken", "needs_review": true}]
        """
        try json.write(to: file, atomically: true, encoding: .utf8)
        let vm = makeModel()
        #expect(vm.loadSubtitleFile(file))
        #expect(vm.sentences.map { vm.needsReview(for: $0) } == [false, true])
        #expect(vm.exportSentences(translated: false).map(\.needsReview) == [false, true])
        #expect(vm.exportSentences(translated: true).map(\.needsReview) == [false, true])

        vm.setText("Broken", for: vm.sentences[1].uid)
        #expect(vm.needsReview(for: vm.sentences[1]))
        vm.setText("Fixed", for: vm.sentences[1].uid)
        #expect(!vm.needsReview(for: vm.sentences[1]))
        #expect(vm.exportSentences(translated: false).map(\.needsReview) == [false, false])
        #expect(vm.exportSentences(translated: true).map(\.needsReview) == [false, false])
    }

    @Test func aDraftCommitClearsTheImportedFlagAndUndoRestoresIt() {
        let vm = makeModel()
        let undo = UndoManager()
        vm.undoManager = undo
        vm.sentences = [Sentence(id: 1, start: 0, end: 1, text: "Broken", needsReview: true)]
        vm.updateDraft("Fixed", for: vm.sentences[0].uid)
        vm.commitDraft()
        #expect(!vm.needsReview(for: vm.sentences[0]))
        undo.undo()
        #expect(vm.needsReview(for: vm.sentences[0]) && vm.sentences[0].text == "Broken")
    }

    // MARK: - Undo

    /// Runs `body` as its own undo step (the tests have no run loop to close the implicit groups).
    private func step(_ undo: UndoManager, _ body: () async -> Void) async {
        undo.beginUndoGrouping()
        await body()
        undo.endUndoGrouping()
    }

    @Test func aSelectedRetryIsItsOwnUndoStepBetweenTwoEdits() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try echoEngine(request: folder.appending(path: "request.json")))
        let undo = UndoManager()
        undo.groupsByEvent = false
        vm.undoManager = undo
        vm.sentences = lines(1...4)
        vm.applyTranslation(
            vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "Old \($0.id)") },
            language: "en", source: vm.sentences
        )
        await step(undo) { vm.setTranslationText("Mine 1", for: vm.sentences[0].uid) }
        await step(undo) {
            vm.setSelectedLines(uids(vm, [3, 4]), active: nil)
            vm.translateSelected()
            await settle(vm)
        }
        #expect(texts(vm) == ["Mine 1", "Old 2", "T-Line 3", "T-Line 4"])

        // The first undo takes back the retry only; the earlier edit is still there and the retry isn't wiped by it.
        undo.undo()
        #expect(texts(vm) == ["Mine 1", "Old 2", "Old 3", "Old 4"])
        undo.redo()
        #expect(texts(vm) == ["Mine 1", "Old 2", "T-Line 3", "T-Line 4"])
        undo.undo()
        undo.undo()
        #expect(texts(vm) == ["Old 1", "Old 2", "Old 3", "Old 4"])
    }

    @Test func undoingARetryBringsBackAHandEditedTranslation() async throws {
        let folder = try makeFolder()
        let vm = makeModel(engine: try echoEngine(request: folder.appending(path: "request.json")))
        let undo = UndoManager()
        undo.groupsByEvent = false
        vm.undoManager = undo
        vm.sentences = lines(1...2)
        vm.applyTranslation(
            vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "Old \($0.id)") },
            language: "en", source: vm.sentences
        )
        await step(undo) { vm.setTranslationText("My own text", for: vm.sentences[1].uid) }
        await step(undo) {
            vm.setSelectedLines(uids(vm, [2]), active: nil)
            vm.translateSelected()
            await settle(vm)
        }
        #expect(texts(vm) == ["Old 1", "T-Line 2"])
        undo.undo()
        #expect(texts(vm) == ["Old 1", "My own text"])
    }

    // MARK: - Multiple selected lines

    @Test func extendingTheRangeUpwardMakesTheClickedRowActive() {
        let vm = makeModel()
        vm.sentences = lines(1...6)
        vm.updateSelection(to: uids(vm, [5]))
        vm.updateSelection(to: uids(vm, [2, 3, 4, 5]))
        #expect(vm.selection == vm.sentences[1].uid)
        // Downward from the new active line: the far end is the clicked one.
        vm.updateSelection(to: uids(vm, [2, 3, 4, 5, 6]))
        #expect(vm.selection == vm.sentences[5].uid)
    }

    @Test func deleteRemovesEverySelectedLineInOneUndoStep() throws {
        let vm = makeModel()
        let undo = UndoManager()
        vm.undoManager = undo
        vm.sentences = lines(1...6)
        vm.applyTranslation(
            vm.sentences.map { Sentence(id: $0.id, start: 0, end: 1, text: "T \($0.id)") },
            language: "en", source: vm.sentences
        )
        vm.setSelectedLines(uids(vm, [2, 3, 5]), active: nil)
        vm.deleteSelected()
        #expect(vm.sentences.map(\.text) == ["Line 1", "Line 4", "Line 6"])
        #expect(vm.sentences.map(\.id) == [1, 2, 3])
        #expect(vm.translations.count == 3)
        // The line that followed the first removed one is selected.
        #expect(vm.selectedUIDs == [vm.sentences[1].uid])
        undo.undo()
        #expect(vm.sentences.map(\.text) == (1...6).map { "Line \($0)" })
        #expect(vm.translations.count == 6)
    }

    @Test func splitMergeAndTimingAreUnavailableForSeveralSelectedLines() {
        let vm = makeModel()
        vm.sentences = lines(1...4)
        vm.setSelectedLines(uids(vm, [2]), active: nil)
        #expect(vm.canMergeSelected && vm.canSplitSelected(at: 2.5))
        vm.setSelectedLines(uids(vm, [2, 3]), active: nil)
        #expect(!vm.canMergeSelected && !vm.canSplitSelected(at: 2.5) && !vm.hasSingleSelection)
        vm.mergeSelectedWithNext()
        vm.splitSelected(at: 2.5)
        vm.nudge(.start, by: 0.1)
        #expect(vm.sentences.count == 4 && vm.sentences[1].start == 2)
    }
}
