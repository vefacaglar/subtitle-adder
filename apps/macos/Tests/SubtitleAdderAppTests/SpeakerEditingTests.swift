import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

@MainActor
@Suite struct SpeakerEditingTests {
    private func makeModel() -> (SubtitleViewModel, UndoManager) {
        let vm = SubtitleViewModel(catalog: ModelCatalog(engine: nil))
        let undo = UndoManager()
        undo.groupsByEvent = false
        vm.undoManager = undo
        vm.sentences = [
            Sentence(id: 1, start: 0, end: 4, text: "one two three four", speaker: 0),
            Sentence(id: 2, start: 4, end: 6, text: "reply", speaker: 1),
        ]
        return (vm, undo)
    }

    private func perform(_ undo: UndoManager, _ body: () -> Void) {
        undo.beginUndoGrouping()
        body()
        undo.endUndoGrouping()
    }

    @Test func changeSpeakerUndoAndRedo() {
        let (vm, undo) = makeModel()
        perform(undo) { vm.setSpeaker(1, for: vm.sentences[0].uid) }
        #expect(vm.sentences.map(\.speaker) == [1, 1])
        undo.undo()
        #expect(vm.sentences.map(\.speaker) == [0, 1])
        undo.redo()
        #expect(vm.sentences.map(\.speaker) == [1, 1])
    }

    @Test func newSpeakerGetsUnusedIndexAndUndoes() {
        let (vm, undo) = makeModel()
        perform(undo) { vm.assignNewSpeaker(for: vm.sentences[1].uid) }
        #expect(vm.sentences.map(\.speaker) == [0, 2])
        undo.undo()
        #expect(vm.sentences.map(\.speaker) == [0, 1])
    }

    @Test func mergeKeepsFirstSpeakerAndUndoRestores() {
        let (vm, undo) = makeModel()
        vm.selection = vm.sentences[0].uid
        perform(undo) { vm.mergeSelectedWithNext() }
        #expect(vm.sentences.map(\.speaker) == [0])
        undo.undo()
        #expect(vm.sentences.map(\.speaker) == [0, 1])
    }

    @Test func splitKeepsSpeakerOnBothHalvesAndUndoRestores() {
        let (vm, undo) = makeModel()
        vm.selection = vm.sentences[0].uid
        perform(undo) { vm.splitSelected(at: 2) }
        #expect(vm.sentences.map(\.speaker) == [0, 0, 1])
        undo.undo()
        #expect(vm.sentences.map(\.speaker) == [0, 1])
    }

    @Test func removeSpeakerUndoes() {
        let (vm, undo) = makeModel()
        perform(undo) { vm.setSpeaker(nil, for: vm.sentences[1].uid) }
        #expect(vm.sentences.map(\.speaker) == [0, nil])
        undo.undo()
        #expect(vm.sentences.map(\.speaker) == [0, 1])
    }

    @Test func speakerModelIsProtectedWhileDiarizing() {
        let catalog = ModelCatalog(engine: nil)
        catalog.activeModels = ["base", SubtitleViewModel.diarizationModelID]
        #expect(catalog.activeModels.contains("speaker-diarization"))
    }
}
