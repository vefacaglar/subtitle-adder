import EngineKit
import SwiftUI

/// Video altındaki satır listesi: seçince oynatıcıyı o satıra götürür, metin ve zamanlama düzenlenir.
struct SentenceEditorView: View {
    @Bindable var vm: SubtitleViewModel
    let player: PlayerModel
    @Environment(\.undoManager) private var undoManager
    @FocusState private var focusedID: Int?

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
        .onChange(of: vm.selection) { _, id in
            if let id, let sentence = vm.sentences.first(where: { $0.id == id }) {
                player.seek(to: sentence.start)
            }
        }
    }

    private var list: some View {
        ScrollViewReader { proxy in
            List(selection: $vm.selection) {
                ForEach(vm.sentences) { sentence in
                    SentenceRow(
                        sentence: sentence,
                        isActive: sentence.id == player.activeID,
                        focus: $focusedID
                    ) { vm.setText($0, for: sentence.id) }
                }
            }
            .onChange(of: player.activeID) { _, id in
                // Kullanıcı yazarken ya da duraklatmışken listeyi kaydırma.
                guard let id, player.isPlaying, focusedID == nil else { return }
                withAnimation { proxy.scrollTo(id, anchor: .center) }
            }
            .onDeleteCommand { vm.deleteSelected() }
            .onKeyPress(.space) {
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
            Button("Sonrakiyle birleştir", systemImage: "arrow.triangle.merge") { vm.mergeSelectedWithNext() }
                .disabled(!vm.canMergeSelected)
            Button("Sil", systemImage: "trash") { vm.deleteSelected() }
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
}

private struct SentenceRow: View {
    let sentence: Sentence
    let isActive: Bool
    var focus: FocusState<Int?>.Binding
    let commitText: (String) -> Void
    @State private var draft: String

    init(sentence: Sentence, isActive: Bool, focus: FocusState<Int?>.Binding, commitText: @escaping (String) -> Void) {
        self.sentence = sentence
        self.isActive = isActive
        self.focus = focus
        self.commitText = commitText
        _draft = State(initialValue: sentence.text)
    }

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
            TextField("Cümle", text: $draft, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...4)
                .focused(focus, equals: sentence.id)
                .onSubmit(commit)
        }
        .padding(.vertical, 3)
        .padding(.horizontal, 4)
        .background(isActive ? Color.accentColor.opacity(0.16) : .clear, in: RoundedRectangle(cornerRadius: 5))
        .onChange(of: focus.wrappedValue) { old, new in
            if old == sentence.id, new != sentence.id { commit() }
        }
        .onChange(of: sentence.text) { _, new in
            if focus.wrappedValue != sentence.id { draft = new }
        }
    }

    private func commit() {
        let text = draft.replacingOccurrences(of: "\n", with: " ")
        draft = text
        commitText(text)
    }
}
