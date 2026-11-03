import EngineKit
import SwiftUI
import UniformTypeIdentifiers

/// The translation list tab: the toolbar of list actions, one row per video (the batch playlist's items and the single
/// videos' jobs of this run), and a drop zone while the list is empty. A click on a row opens that video in the editor.
struct PlaylistView: View {
    @Bindable var vm: SubtitleViewModel
    @State private var pickingVideos = false

    var body: some View {
        VStack(spacing: 0) {
            actions.padding(.horizontal, 16).padding(.vertical, 10)
            if let notice = vm.playlistNotice {
                noticeBar(notice)
            }
            Divider()
            if vm.hasListRows {
                List(vm.listRows) { row in
                    PlaylistRow(vm: vm, row: row)
                }
                .listStyle(.inset)
            } else {
                emptyState.padding(20)
            }
        }
        .fileImporter(
            isPresented: $pickingVideos, allowedContentTypes: [.movie, .audiovisualContent], allowsMultipleSelection: true
        ) { result in
            guard case .success(let urls) = result, !urls.isEmpty else { return }
            vm.addToPlaylist(urls)
        }
    }

    private var actions: some View {
        HStack(spacing: 10) {
            Button(L("Videoları ekle…"), systemImage: "plus") { pickingVideos = true }
                .help(L("Listeye birden fazla video dosyası ekle"))
            Menu {
                ForEach(SubtitleLanguage.allCases) { language in
                    Button(language.title) { vm.setAllPlaylistLanguages(language.rawValue) }
                }
            } label: {
                Label(L("Tüm kaynak diller"), systemImage: "globe")
            }
            .fixedSize()
            .disabled(!vm.hasPlaylistItems || vm.isPlaylistRunning)
            .help(L("Listedeki tüm videoların konuşma (kaynak) dilini tek seferde ayarla"))
            Button(L("Listeyi temizle"), systemImage: "trash") { vm.clearPlaylist() }
                .disabled(!vm.hasListRows)
                .help(L("Tamamlanan ve bekleyen videoları listeden temizle"))
            Button(L("Sıradakileri temizle"), systemImage: "text.badge.xmark") { vm.clearQueue() }
                .disabled(vm.queuedJobCount == 0)
                .help(L("Sırada bekleyen tekil video işlerini iptal et; çalışan iş sürer"))
            Spacer()
            Text(vm.playlistSummary).foregroundStyle(.secondary)
            Button(L("Tümünü Çevir ve Kaydet"), systemImage: "play.fill") { vm.startPlaylist() }
                .buttonStyle(.borderedProminent)
                .disabled(!vm.canStartPlaylist)
                .help(L("Listedeki tüm videoları sırayla metne çevir, seçili hedef dile çevir ve kendi klasörlerine kaydet"))
        }
    }

    /// Why the run did not start, with the way out when going online helps.
    private func noticeBar(_ notice: PlaylistNotice) -> some View {
        HStack(spacing: 10) {
            Label(notice.message.text, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
                .lineLimit(2)
            if notice.retry != nil {
                Button(SubtitleViewModel.offlineRetryTitle(for: notice.message)) { vm.goOnlineAndRetryPlaylist() }
            }
            Spacer()
            Button(L("Kapat"), systemImage: "xmark.circle.fill") { vm.playlistNotice = nil }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .help(L("Kapat"))
        }
        .font(.callout)
        .padding(.horizontal, 16)
        .padding(.bottom, 10)
    }

    private var emptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: "rectangle.stack.badge.plus")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(.secondary)
            Text(L("Birden fazla videoyu buraya sürükle"))
                .font(.title3)
            Text(L("Listedeki tüm videolar sırayla seçili hedef dile çevrilip kendi klasörlerine kaydedilir."))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button(L("Videoları ekle…"), systemImage: "plus") { pickingVideos = true }
                .padding(.top, 4)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 16))
        .overlay(
            RoundedRectangle(cornerRadius: 16)
                .strokeBorder(style: StrokeStyle(lineWidth: 1.5, dash: [7]))
                .foregroundStyle(.tertiary)
        )
    }
}

private struct PlaylistRow: View {
    let vm: SubtitleViewModel
    let row: ListRow

    var body: some View {
        HStack(spacing: 12) {
            statusIcon.frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.fileName).fontWeight(.semibold).lineLimit(1).truncationMode(.middle)
                Text(row.folderPath)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
            Spacer(minLength: 8)
            languageColumn
            VStack(alignment: .leading, spacing: 4) {
                Text(row.message.text)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                if row.state == .working {
                    if let fraction = row.fraction {
                        ProgressView(value: fraction)
                    } else {
                        ProgressView().progressViewStyle(.linear)
                    }
                }
            }
            .frame(width: 240, alignment: .leading)
            trailingButton
        }
        .padding(.vertical, 4)
        // A click shows the video in the editor, with the lines and progress of its own session.
        .contentShape(Rectangle())
        .onTapGesture { vm.openListRow(row) }
        .help(L("Videoyu düzenleyicide aç"))
    }

    /// The playlist's items have a spoken language of their own; a single video's job uses the toolbar's.
    @ViewBuilder private var languageColumn: some View {
        if let itemID = row.playlistItemID {
            Picker(L("Konuşma dili"), selection: Binding(
                get: { row.sourceLanguage },
                set: { vm.setPlaylistItemLanguage(itemID, to: $0) }
            )) {
                ForEach(SubtitleLanguage.allCases) { Text($0.title).tag($0.rawValue) }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .frame(width: 170)
            .disabled(row.state == .working || vm.isPlaylistRunning)
            .help(L("Bu videonun konuşma (kaynak) dilini seç"))
        }
    }

    /// Stop for a single video's job that runs or waits, the removal from the list for everything else.
    @ViewBuilder private var trailingButton: some View {
        if row.canStop {
            Button(L("Durdur"), systemImage: "stop.fill") { vm.removeListRow(row) }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .help(L("Bu videonun işini durdur"))
        } else {
            Button(L("Listeden kaldır"), systemImage: "xmark") { vm.removeListRow(row) }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .disabled(row.state == .working)
                .help(L("Listeden kaldır"))
        }
    }

    @ViewBuilder private var statusIcon: some View {
        switch row.state {
        case .pending:
            // The audio of a waiting video can already be extracted ("Ses hazır · Sırada").
            Image(systemName: row.isAudioReady ? "waveform" : "clock").foregroundStyle(.secondary)
        case .working:
            ProgressView().controlSize(.small)
        case .completed:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .failed:
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
        }
    }
}
