import Combine
import EngineKit
import SwiftUI

/// Video altındaki satır listesi: seçince oynatıcıyı o satıra götürür, metin ve zamanlama düzenlenir.
struct SentenceEditorView: View {
    @Bindable var vm: SubtitleViewModel
    let player: PlayerModel
    @Environment(\.undoManager) private var undoManager
    @FocusState private var focusedUID: UUID?

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
        .onChange(of: vm.selection) { _, uid in
            if let uid, let sentence = vm.sentences.first(where: { $0.uid == uid }) {
                player.seek(to: sentence.start)
            }
        }
        // Geri alma/yineleme başlamadan önce odaklı satırın taslağını yaz ve odağı bırak.
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
                        vm: vm
                    )
                }
            }
            .onChange(of: player.activeID) { _, uid in
                // Kullanıcı yazarken ya da duraklatmışken listeyi kaydırma.
                guard let uid, player.isPlaying, focusedUID == nil else { return }
                withAnimation { proxy.scrollTo(uid, anchor: .center) }
            }
            .onDeleteCommand {
                finishEditing()
                vm.deleteSelected()
            }
            .onKeyPress(.space) {
                // Metin alanı odaktayken boşluk yazılsın.
                guard focusedUID == nil else { return .ignored }
                player.togglePlayback()
                return .handled
            }
        }
    }

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

    /// Odaklı metin alanını bırakır ve taslağı satıra yazar.
    private func finishEditing() {
        focusedUID = nil
        vm.commitDraft()
    }
}

private struct SentenceRow: View {
    let sentence: Sentence
    let isActive: Bool
    var focus: FocusState<UUID?>.Binding
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
            TextField("Cümle", text: Binding(
                get: { vm.draftText(for: sentence) },
                set: { vm.updateDraft($0, for: sentence.uid) }
            ), axis: .vertical)
            .textFieldStyle(.plain)
            .lineLimit(1...4)
            .focused(focus, equals: sentence.uid)
            .onSubmit { vm.commitDraft() }
        }
        .padding(.vertical, 3)
        .padding(.horizontal, 4)
        .background(isActive ? Color.accentColor.opacity(0.16) : .clear, in: RoundedRectangle(cornerRadius: 5))
    }
}
