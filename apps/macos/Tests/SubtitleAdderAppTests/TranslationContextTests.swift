import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// A partial translation request carries neighbouring lines as context; only the requested lines are ever applied.
@MainActor
@Suite struct TranslationContextTests {
    private func makeModel(engine: EngineClient? = nil) -> SubtitleViewModel {
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine),
            defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        return vm
    }

    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "context-test-\(UUID().uuidString)")
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

    // MARK: - withTranslationContext

    @Test func asyncChunkIncludesNeighbouringDialogueWithoutRenumbering() {
        let all = lines(10...21)
        let request = SubtitleViewModel.withTranslationContext(all, requested: [all[4], all[5]])
        #expect(request.map(\.id) == [12, 13, 14, 15, 16, 17])
        #expect(request[2].uid == all[4].uid)
    }

    @Test func missingLinesKeepTheirOwnContextInTimelineOrder() {
        let all = lines(1...12)
        // The requested lines come in any order; the request follows the timeline.
        let request = SubtitleViewModel.withTranslationContext(all, requested: [all[10], all[0]])
        #expect(request.map(\.id) == [1, 2, 3, 9, 10, 11, 12])
        #expect(SubtitleViewModel.withTranslationContext(all, requested: []).isEmpty)
    }

    @Test func contextKeepsTwoNeighboursOnEachSideInOrder() {
        let all = lines(1...8)
        func ids(_ requested: [Int]) -> [Int] {
            let wanted = all.filter { requested.contains($0.id) }
            return SubtitleViewModel.withTranslationContext(all, requested: wanted).map(\.id)
        }
        #expect(ids([4]) == [2, 3, 4, 5, 6])
        #expect(ids([1]) == [1, 2, 3])
        #expect(ids([8]) == [6, 7, 8])
        #expect(ids([2, 8]) == [1, 2, 3, 4, 6, 7, 8])
        // Overlapping neighbourhoods are sent once.
        #expect(ids([3, 4]) == [1, 2, 3, 4, 5, 6])
        #expect(ids([]).isEmpty)
    }

    @Test func requestingEverythingSendsEverythingOnce() {
        let all = lines(1...6)
        #expect(SubtitleViewModel.withTranslationContext(all, requested: all).map(\.id) == [1, 2, 3, 4, 5, 6])
    }

    // MARK: - Applying results

    @Test func extraTranslationIdsCannotOverwriteUnrelatedSubtitlesByPosition() async throws {
        let engine = try FakeEngine.make(translate: """
        echo '{"event": "translation", "language": "en", "sentences": [\
        {"id": 99, "start": 0, "end": 1, "text": "Context only.", "words": []},\
        {"id": 2, "start": 1, "end": 2, "text": "Two.", "words": []}]}'
        """)
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.sentences = [
            Sentence(id: 1, start: 0, end: 1, text: "Bir."),
            Sentence(id: 2, start: 1, end: 2, text: "İki."),
        ]
        vm.translate()
        await settle(vm)
        #expect(vm.translation(for: vm.sentences[0]) == nil)
        #expect(vm.translation(for: vm.sentences[1])?.text == "Two.")
    }

    @Test func pairsMatchByIdOnlyAndDropUnknownIds() {
        let source = lines(5...7)
        let results = [
            Sentence(id: 4, start: 0, end: 1, text: "context before"),
            Sentence(id: 5, start: 5, end: 6, text: "five"),
            Sentence(id: 7, start: 7, end: 8, text: "seven"),
            Sentence(id: 99, start: 9, end: 10, text: "invented"),
        ]
        let pairs = SubtitleViewModel.pairs(results, with: source)
        #expect(pairs.map(\.result.text) == ["five", "seven"])
        #expect(pairs.map(\.original.id) == [5, 7])
    }

    @Test func aResumedTranslationSendsContextButAppliesOnlyTheMissingLines() async throws {
        let folder = try makeFolder()
        let request = folder.appending(path: "request.json")
        // The server answers every line it was sent, context included, with "T-<text>".
        let engine = try FakeEngine.make(
            translate: #"printf '{"event": "translation", "language": "en", "sentences": %s}\n' "$(sed 's/"text":"\([^"]*\)"/"text":"T-\1"/g' '\#(request.path)')""#,
            translateInput: "cat > '\(request.path)'"
        )
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.sentences = lines(1...8)
        let done = [0, 1, 2, 4, 5, 6, 7]
        vm.applyTranslation(
            done.map { Sentence(id: $0 + 1, start: 0, end: 1, text: "Done \($0 + 1)") },
            language: "en", source: vm.sentences
        )
        vm.translate(reuseExisting: true)
        await settle(vm)

        // Line 4 is missing: it goes out with two lines on each side.
        let sent = try JSONDecoder().decode([Sentence].self, from: Data(contentsOf: request))
        #expect(sent.map(\.id) == [2, 3, 4, 5, 6])
        #expect(vm.sentences.compactMap { vm.translation(for: $0)?.text }
            == ["Done 1", "Done 2", "Done 3", "T-Line 4", "Done 5", "Done 6", "Done 7", "Done 8"])
    }

    @Test func aCopyOfTheSourceIsSentAgainWithItsContext() async throws {
        let folder = try makeFolder()
        let request = folder.appending(path: "request.json")
        let engine = try FakeEngine.make(
            translate: #"printf '{"event": "translation", "language": "en", "sentences": %s}\n' "$(sed 's/"text":"\([^"]*\)"/"text":"T-\1"/g' '\#(request.path)')""#,
            translateInput: "cat > '\(request.path)'"
        )
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.sentences = lines(1...4)
        vm.applyTranslation(
            [
                Sentence(id: 1, start: 0, end: 1, text: "One"),
                // The model gave the source back unchanged.
                Sentence(id: 2, start: 0, end: 1, text: "line 2"),
                Sentence(id: 3, start: 0, end: 1, text: "Three"),
                Sentence(id: 4, start: 0, end: 1, text: "Four"),
            ],
            language: "en", source: vm.sentences
        )
        vm.translate(reuseExisting: true)
        await settle(vm)
        let sent = try JSONDecoder().decode([Sentence].self, from: Data(contentsOf: request))
        #expect(sent.map(\.id) == [1, 2, 3, 4])
        #expect(vm.sentences.compactMap { vm.translation(for: $0)?.text } == ["One", "T-Line 2", "Three", "Four"])
    }
}
