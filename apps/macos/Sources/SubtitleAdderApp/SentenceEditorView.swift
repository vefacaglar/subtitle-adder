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
    /// Width of the editor; a narrow one (side-by-side next to the player) trims the edit bar.
    @State private var width: CGFloat = 1000

    var body: some View {
        VStack(spacing: 0) {
            if vm.sentences.isEmpty {
                ContentUnavailableView(
                    vm.isWorking ? L("Altyazı üretiliyor…") : L("Henüz altyazı yok"),
                    systemImage: "captions.bubble",
                    description: vm.isWorking ? nil : Text(L("Araç çubuğundan “Altyazı üret”e bas."))
                )
            } else {
                editBar
                Divider()
                list
            }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        .onAppear { vm.undoManager = undoManager }
        .onChange(of: undoManager) { _, new in vm.undoManager = new }
        .onChange(of: focusedUID) { old, new in
            if let new, let sentence = vm.sentences.first(where: { $0.uid == new }) {
                vm.selectRow(new)
                vm.beginEditing(sentence)
            } else if old != nil {
                vm.commitDraft()
            }
        }
        .onChange(of: focusedTranslationUID) { old, new in
            if let new, let sentence = vm.sentences.first(where: { $0.uid == new }) {
                vm.selectRow(new)
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
            // ⌘-click toggles and ⇧-click extends; the model decides which selected line is the active one.
            List(selection: Binding(get: { vm.selectedUIDs }, set: { vm.updateSelection(to: $0) })) {
                ForEach(vm.sentences, id: \.uid) { sentence in
                    SentenceRow(
                        sentence: sentence,
                        isActive: sentence.uid == player.activeID,
                        focus: $focusedUID,
                        translationFocus: $focusedTranslationUID,
                        showsSpeaker: showsSpeakers,
                        vm: vm,
                        player: player
                    )
                }
            }
            .onChange(of: player.activeID) { _, uid in
                // Don't scroll the list while the user is typing or has paused.
                guard let uid, player.isPlaying, focusedUID == nil, focusedTranslationUID == nil else { return }
                withAnimation { proxy.scrollTo(uid, anchor: .center) }
            }
            .accessibilityLabel(L("Satırlar"))
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

    /// Below this width the Split/Merge/Delete buttons lose their titles, below the second one the timing captions go.
    private static let compactBarWidth: CGFloat = 900
    private static let minimalBarWidth: CGFloat = 540

    /// The bar tries three layouts and takes the first that fits: everything in one row; one row without the
    /// "Şimdiki an" buttons; two rows (timing on top, badge and actions below) for the narrowest editor.
    private var editBar: some View {
        let showsCaptions = width >= Self.minimalBarWidth
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 16) {
                timingControls(L("Başlangıç"), edge: .start, value: vm.selectedSentence?.start, showsCaption: showsCaptions, showsScope: true)
                timingControls(L("Bitiş"), edge: .end, value: vm.selectedSentence?.end, showsCaption: showsCaptions, showsScope: true)
                Spacer(minLength: 0)
                badge
                actionButtons
            }
            HStack(spacing: 8) {
                timingControls(L("Başlangıç"), edge: .start, value: vm.selectedSentence?.start, showsCaption: showsCaptions, showsScope: false)
                timingControls(L("Bitiş"), edge: .end, value: vm.selectedSentence?.end, showsCaption: showsCaptions, showsScope: false)
                Spacer(minLength: 0)
                badge
                actionButtons
            }
            VStack(spacing: 6) {
                HStack(spacing: 8) {
                    timingControls(L("Başlangıç"), edge: .start, value: vm.selectedSentence?.start, showsCaption: false, showsScope: false)
                    timingControls(L("Bitiş"), edge: .end, value: vm.selectedSentence?.end, showsCaption: false, showsScope: false)
                    Spacer(minLength: 0)
                }
                HStack(spacing: 8) {
                    Spacer(minLength: 0)
                    badge
                    actionButtons
                }
            }
        }
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        // Live snapshots from a running job replace the lines, so edits wait until it finishes.
        .disabled(vm.isWorking)
    }

    @ViewBuilder private var badge: some View {
        if let text = vm.translationBadgeText {
            TranslationBadge(text: text)
        }
    }

    private var actionButtons: some View {
        Group {
            Button(L("Böl"), systemImage: "scissors") {
                finishEditing()
                vm.splitSelected(at: player.currentTime)
            }
            .disabled(!vm.canSplitSelected(at: player.currentTime))
            .help(L("Satırı oynatma konumundan ikiye böl"))
            Button(L("Seçilenleri yeniden çevir"), systemImage: "arrow.triangle.2.circlepath") {
                finishEditing()
                vm.translateSelected()
            }
            .disabled(!vm.canTranslateSelected)
            .help(L("⌘ veya ⇧ ile satırları seç, seçilenleri yeniden çevir"))
            Button(L("Sonrakiyle birleştir"), systemImage: "arrow.triangle.merge") {
                finishEditing()
                vm.mergeSelectedWithNext()
            }
            .disabled(!vm.canMergeSelected)
            .help(L("Sonrakiyle birleştir"))
            Button(L("Sil"), systemImage: "trash") {
                finishEditing()
                vm.deleteSelected()
            }
            .disabled(vm.selection == nil)
            .help(L("Satırı Sil (⌫)"))
        }
        .labelStyle(AdaptiveLabelStyle(iconOnly: width < Self.compactBarWidth))
    }

    private func timingControls(_ title: String, edge: TimingEdge, value: Double?, showsCaption: Bool, showsScope: Bool) -> some View {
        let step = SentenceEditing.nudgeStep
        return HStack(spacing: 4) {
            if showsCaption {
                Text(title).foregroundStyle(.secondary)
            }
            Button(L("%@ 0,1 sn geri", title), systemImage: "minus") { vm.nudge(edge, by: -step) }
                .labelStyle(.iconOnly)
            Text(value.map(SubtitleTime.short) ?? "––:––.––")
                .monospacedDigit()
                .frame(minWidth: 52)
            Button(L("%@ 0,1 sn ileri", title), systemImage: "plus") { vm.nudge(edge, by: step) }
                .labelStyle(.iconOnly)
            if showsScope {
                Button(L("Şimdiki an"), systemImage: "scope") { vm.setSelected(edge, to: player.currentTime) }
                    .labelStyle(.iconOnly)
                    .help(L("%@ zamanını oynatma konumuna ayarla", title))
            }
        }
        .disabled(!vm.hasSingleSelection)
    }

    /// Releases focused text fields and writes the drafts to the lines.
    private func finishEditing() {
        focusedUID = nil
        focusedTranslationUID = nil
        vm.commitDraft()
        vm.commitTranslationDraft()
    }
}

/// Title and icon, or the icon alone when the bar is narrow.
private struct AdaptiveLabelStyle: LabelStyle {
    let iconOnly: Bool

    func makeBody(configuration: Configuration) -> some View {
        if iconOnly {
            // The built-in icon-only style keeps the title as the accessibility label.
            Label(configuration).labelStyle(.iconOnly)
        } else {
            Label(configuration)
        }
    }
}

/// Split, merge and delete in a line's context menu. They act on the row under the pointer, like retranslate: inside
/// a multi-selection delete covers all of it (split and merge need a single line), anywhere else just this row.
/// A view of its own so reading the playback time (for Split) doesn't make every row redraw while a video plays.
private struct RowEditActions: View {
    let uid: UUID
    let vm: SubtitleViewModel
    let player: PlayerModel
    /// Releases the focused text fields before the lines change.
    let finishEditing: () -> Void

    var body: some View {
        Button(L("Böl"), systemImage: "scissors") {
            finishEditing()
            vm.splitRow(uid, at: player.currentTime)
        }
        .disabled(!vm.canSplitRow(uid, at: player.currentTime))
        Button(L("Sonrakiyle birleştir"), systemImage: "arrow.triangle.merge") {
            finishEditing()
            vm.mergeRowWithNext(uid)
        }
        .disabled(!vm.canMergeRow(uid))
        Button(L("Sil"), systemImage: "trash", role: .destructive) {
            finishEditing()
            vm.deleteRow(uid)
        }
        .disabled(!vm.canDeleteRow(uid))
    }
}

/// Capsule with a small spinner and the translation progress, e.g. "Çevriliyor… 45%".
private struct TranslationBadge: View {
    let text: String

    var body: some View {
        HStack(spacing: 6) {
            ProgressView().controlSize(.mini)
            Text(text)
                .font(.caption.weight(.semibold))
                .monospacedDigit()
                .lineLimit(1)
        }
        .foregroundStyle(Color.accentColor)
        .padding(.horizontal, 10)
        .padding(.vertical, 3)
        .background(Color.accentColor.opacity(0.14), in: Capsule())
        .accessibilityElement(children: .combine)
    }
}

private struct SentenceRow: View {
    let sentence: Sentence
    let isActive: Bool
    var focus: FocusState<UUID?>.Binding
    var translationFocus: FocusState<UUID?>.Binding
    let showsSpeaker: Bool
    let vm: SubtitleViewModel
    let player: PlayerModel

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
                HStack(alignment: .firstTextBaseline, spacing: 5) {
                    TextField(L("Cümle"), text: Binding(
                        get: { vm.draftText(for: sentence) },
                        set: { vm.updateDraft($0, for: sentence.uid) }
                    ), axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...4)
                    .focused(focus, equals: sentence.uid)
                    .onSubmit { vm.commitDraft() }
                    .disabled(vm.isWorking)
                    // A line imported with the flag (JSON) has no translation row to carry the marker.
                    if vm.translation(for: sentence) == nil, vm.needsReview(for: sentence) { reviewMarker }
                }
                if vm.translation(for: sentence) != nil {
                    translationField
                }
            }
        }
        .padding(.vertical, 3)
        .padding(.horizontal, 4)
        .background(isActive ? Color.accentColor.opacity(0.16) : .clear, in: RoundedRectangle(cornerRadius: 5))
        .contextMenu {
            Button(L("Seçilenleri yeniden çevir"), systemImage: "arrow.triangle.2.circlepath") {
                // The row under the pointer joins the action: inside the selection the whole set is retranslated,
                // outside it just this line.
                vm.selectRow(sentence.uid)
                vm.commitDraft()
                vm.translateSelected()
            }
            .disabled(vm.isWorking)
            Divider()
            RowEditActions(uid: sentence.uid, vm: vm, player: player) {
                focus.wrappedValue = nil
                translationFocus.wrappedValue = nil
            }
            if showsSpeaker {
                Divider()
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
                    Text(L("Konuşmacı %lld", speaker + 1))
                        .font(.caption)
                }
            } else {
                Text(L("Konuşmacı yok"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help(L("Bu satırı söyleyen kişiyi değiştir"))
        .accessibilityHint(L("Konuşmacıyı değiştir"))
    }

    @ViewBuilder
    private var speakerChoices: some View {
        ForEach(vm.sentences.speakers, id: \.self) { speaker in
            Button {
                vm.setSpeaker(speaker, for: sentence.uid)
            } label: {
                if speaker == sentence.speaker {
                    Label(L("Konuşmacı %lld", speaker + 1), systemImage: "checkmark")
                } else {
                    Text(L("Konuşmacı %lld", speaker + 1))
                }
            }
        }
        Divider()
        Button(L("Yeni konuşmacı")) {
            vm.assignNewSpeaker(for: sentence.uid)
        }
        Button(L("Konuşmacıyı kaldır")) {
            vm.setSpeaker(nil, for: sentence.uid)
        }
        .disabled(sentence.speaker == nil)
    }

    private var translationField: some View {
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            Image(systemName: "character.bubble")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityLabel(L("Çeviri"))
            TextField(L("Çeviri"), text: Binding(
                get: { vm.translationDraftText(for: sentence) },
                set: { vm.updateTranslationDraft($0, for: sentence.uid) }
            ), axis: .vertical)
            .textFieldStyle(.plain)
            .lineLimit(1...3)
            .foregroundStyle(.secondary)
            .focused(translationFocus, equals: sentence.uid)
            .onSubmit { vm.commitTranslationDraft() }
            .disabled(vm.isWorking)
            if vm.isTranslationStale(for: sentence) {
                Image(systemName: "clock.arrow.circlepath")
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .help(L("Orijinal metin değişti; çeviri eski olabilir. “Çevir…” ile yenile."))
                    .accessibilityLabel(L("Çeviri eski"))
            }
            if vm.needsReview(for: sentence) { reviewMarker }
        }
    }

    private var reviewMarker: some View {
        Image(systemName: "exclamationmark.triangle")
            .font(.caption2)
            .foregroundStyle(.orange)
            .help(L("Bu satır çevrilemedi. Seçip yeniden çevirebilir veya elle düzeltebilirsin."))
            .accessibilityLabel(L("Çeviriyi kontrol et"))
    }
}
