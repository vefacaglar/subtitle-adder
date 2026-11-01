import Combine
import EngineKit
import SwiftUI

/// The line list under the video: selecting moves the player to that line; text and timing are edited.
struct SentenceEditorView: View {
    @Bindable var vm: SubtitleViewModel
    let player: PlayerModel
    @Environment(\.undoManager) private var undoManager
    @FocusState private var focusedUID: UUID?
    @FocusState private var focusedTranslationUID: UUID?

    var body: some View {
        VStack(spacing: 0) {
            if vm.sentences.isEmpty {
                ContentUnavailableView(
                    vm.isWorking ? "Altyazı üretiliyor…" : "Henüz altyazı yok",
                    systemImage: "captions.bubble",
                    description: vm.isWorking ? nil : Text("Araç çubuğundan “Altyazı üret”e bas.")
                )
            } else {
                editBar
                Divider()
                list
            }
        }
        .onAppear { vm.undoManager = undoManager }
        .onChange(of: undoManager) { _, new in vm.undoManager = new }
        .onChange(of: focusedUID) { old, new in
            if let new, let sentence = vm.sentences.first(where: { $0.uid == new }) {
                vm.beginEditing(sentence)
            } else if old != nil {
                vm.commitDraft()
            }
        }
        .onChange(of: focusedTranslationUID) { old, new in
            if let new, let sentence = vm.sentences.first(where: { $0.uid == new }) {
                vm.beginTranslationEditing(sentence)
            } else if old != nil {
                vm.commitTranslationDraft()
            }
        }
        .onChange(of: vm.selection) { _, uid in
            if let uid, let sentence = vm.sentences.first(where: { $0.uid == uid }) {
                player.seek(to: sentence.start)
            }
        }
        // Before undo/redo starts, write the focused line's draft and release focus.
        .onReceive(NotificationCenter.default.publisher(for: .NSUndoManagerWillUndoChange)) { note in
            guard let manager = note.object as? UndoManager, manager === undoManager else { return }
            finishEditing()
        }
        .onReceive(NotificationCenter.default.publisher(for: .NSUndoManagerWillRedoChange)) { note in
            guard let manager = note.object as? UndoManager, manager === undoManager else { return }
            finishEditing()
        }
    }

    private var list: some View {
        ScrollViewReader { proxy in
            List(selection: $vm.selection) {
                ForEach(vm.sentences, id: \.uid) { sentence in
                    SentenceRow(
                        sentence: sentence,
                        isActive: sentence.uid == player.activeID,
                        focus: $focusedUID,
                        translationFocus: $focusedTranslationUID,
                        showsSpeaker: showsSpeakers,
                        vm: vm
                    )
                }
            }
            .onChange(of: player.activeID) { _, uid in
                // Don't scroll the list while the user is typing or has paused.
                guard let uid, player.isPlaying, focusedUID == nil, focusedTranslationUID == nil else { return }
                withAnimation { proxy.scrollTo(uid, anchor: .center) }
            }
            .onDeleteCommand {
                finishEditing()
                vm.deleteSelected()
            }
            .onKeyPress(.space) {
                // Let the space key type a space while the text field is focused.
                guard focusedUID == nil, focusedTranslationUID == nil else { return .ignored }
                player.togglePlayback()
                return .handled
            }
        }
    }

    /// Speaker controls appear as soon as any line has a speaker (even if diarization found only one),
    /// so more speakers can be added; the dash rule separately needs two.
    private var showsSpeakers: Bool { !vm.sentences.speakers.isEmpty }

    private var editBar: some View {
        HStack(spacing: 16) {
            timingControls("Başlangıç", edge: .start, value: vm.selectedSentence?.start)
            timingControls("Bitiş", edge: .end, value: vm.selectedSentence?.end)
            Spacer(minLength: 0)
            Button("Böl", systemImage: "scissors") {
                finishEditing()
                vm.splitSelected(at: player.currentTime)
            }
            .disabled(!vm.canSplitSelected(at: player.currentTime))
            .help("Satırı oynatma konumundan ikiye böl")
            Button("Sonrakiyle birleştir", systemImage: "arrow.triangle.merge") {
                finishEditing()
                vm.mergeSelectedWithNext()
            }
            .disabled(!vm.canMergeSelected)
            Button("Sil", systemImage: "trash") {
                finishEditing()
                vm.deleteSelected()
            }
            .disabled(vm.selection == nil)
        }
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }

    private func timingControls(_ title: String, edge: TimingEdge, value: Double?) -> some View {
        let step = SentenceEditing.nudgeStep
        return HStack(spacing: 4) {
            Text(title).foregroundStyle(.secondary)
            Button("\(title) 0,1 sn geri", systemImage: "minus") { vm.nudge(edge, by: -step) }
                .labelStyle(.iconOnly)
            Text(value.map(SubtitleTime.short) ?? "––:––.––")
                .monospacedDigit()
                .frame(minWidth: 52)
            Button("\(title) 0,1 sn ileri", systemImage: "plus") { vm.nudge(edge, by: step) }
                .labelStyle(.iconOnly)
            Button("Şimdiki an", systemImage: "scope") { vm.setSelected(edge, to: player.currentTime) }
                .labelStyle(.iconOnly)
                .help("\(title) zamanını oynatma konumuna ayarla")
        }
        .disabled(vm.selection == nil)
    }

    /// Releases focused text fields and writes the drafts to the lines.
    private func finishEditing() {
        focusedUID = nil
        focusedTranslationUID = nil
        vm.commitDraft()
        vm.commitTranslationDraft()
    }
}

private struct SentenceRow: View {
    let sentence: Sentence
    let isActive: Bool
    var focus: FocusState<UUID?>.Binding
    var translationFocus: FocusState<UUID?>.Binding
    let showsSpeaker: Bool
    let vm: SubtitleViewModel

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text("\(sentence.id)")
                .monospacedDigit()
                .foregroundStyle(isActive ? Color.accentColor : .secondary)
                .frame(width: 28, alignment: .trailing)
            Text("\(SubtitleTime.short(sentence.start)) → \(SubtitleTime.short(sentence.end))")
                .monospacedDigit()
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(width: 124, alignment: .leading)
            if showsSpeaker {
                speakerMenu
                    .frame(width: 124, alignment: .leading)
            }
            VStack(alignment: .leading, spacing: 3) {
                TextField("Cümle", text: Binding(
                    get: { vm.draftText(for: sentence) },
                    set: { vm.updateDraft($0, for: sentence.uid) }
                ), axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...4)
                .focused(focus, equals: sentence.uid)
                .onSubmit { vm.commitDraft() }
                if vm.translation(for: sentence) != nil {
                    translationField
                }
            }
        }
        .padding(.vertical, 3)
        .padding(.horizontal, 4)
        .background(isActive ? Color.accentColor.opacity(0.16) : .clear, in: RoundedRectangle(cornerRadius: 5))
        .contextMenu {
            if showsSpeaker {
                speakerChoices
            }
        }
    }

    /// Compact indicator: color dot and "Konuşmacı N"; clicking opens the speaker choices.
    private var speakerMenu: some View {
        Menu {
            speakerChoices
        } label: {
            if let speaker = sentence.speaker {
                HStack(spacing: 5) {
                    Circle()
                        .fill(SpeakerPalette.color(for: speaker))
                        .frame(width: 8, height: 8)
                    Text("Konuşmacı \(speaker + 1)")
                        .font(.caption)
                }
            } else {
                Text("Konuşmacı yok")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Bu satırı söyleyen kişiyi değiştir")
        .accessibilityHint("Konuşmacıyı değiştir")
    }

    @ViewBuilder
    private var speakerChoices: some View {
        ForEach(vm.sentences.speakers, id: \.self) { speaker in
            Button {
                vm.setSpeaker(speaker, for: sentence.uid)
            } label: {
                if speaker == sentence.speaker {
                    Label("Konuşmacı \(speaker + 1)", systemImage: "checkmark")
                } else {
                    Text("Konuşmacı \(speaker + 1)")
                }
            }
        }
        Divider()
        Button("Yeni konuşmacı") {
            vm.assignNewSpeaker(for: sentence.uid)
        }
        Button("Konuşmacıyı kaldır") {
            vm.setSpeaker(nil, for: sentence.uid)
        }
        .disabled(sentence.speaker == nil)
    }

    private var translationField: some View {
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            Image(systemName: "character.bubble")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Çeviri")
            TextField("Çeviri", text: Binding(
                get: { vm.translationDraftText(for: sentence) },
                set: { vm.updateTranslationDraft($0, for: sentence.uid) }
            ), axis: .vertical)
            .textFieldStyle(.plain)
            .lineLimit(1...3)
            .foregroundStyle(.secondary)
            .focused(translationFocus, equals: sentence.uid)
            .onSubmit { vm.commitTranslationDraft() }
            if vm.isTranslationStale(for: sentence) {
                Image(systemName: "clock.arrow.circlepath")
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .help("Orijinal metin değişti; çeviri eski olabilir. “Çevir…” ile yenile.")
                    .accessibilityLabel("Çeviri eski")
            }
        }
    }
}
