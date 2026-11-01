import EngineKit
import SwiftUI
import UniformTypeIdentifiers

/// The batch playlist tab: the toolbar of list actions, one row per video, and a drop zone while the list is empty.
struct PlaylistView: View {
    @Bindable var vm: SubtitleViewModel
    @State private var pickingVideos = false

    var body: some View {
        VStack(spacing: 0) {
            actions.padding(.horizontal, 16).padding(.vertical, 10)
            Divider()
            if vm.hasPlaylistItems {
                List(vm.playlist) { item in
                    PlaylistRow(vm: vm, item: item)
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
                .disabled(!vm.hasPlaylistItems)
                .help(L("Tamamlanan ve bekleyen videoları listeden temizle"))
            Spacer()
            Text(vm.playlistSummary).foregroundStyle(.secondary)
            Button(L("Tümünü Çevir ve Kaydet"), systemImage: "play.fill") { vm.startPlaylist() }
                .buttonStyle(.borderedProminent)
                .disabled(!vm.canStartPlaylist)
                .help(L("Listedeki tüm videoları sırayla metne çevir, seçili hedef dile çevir ve kendi klasörlerine kaydet"))
        }
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
    let item: PlaylistItem

    var body: some View {
        HStack(spacing: 12) {
            statusIcon.frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.fileName).fontWeight(.semibold).lineLimit(1).truncationMode(.middle)
                Text(item.folderPath)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
            Spacer(minLength: 8)
            Picker(L("Konuşma dili"), selection: Binding(
                get: { item.sourceLanguage },
                set: { vm.setPlaylistItemLanguage(item.id, to: $0) }
            )) {
                ForEach(SubtitleLanguage.allCases) { Text($0.title).tag($0.rawValue) }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .frame(width: 170)
            .disabled(!item.isEditable || vm.isPlaylistRunning)
            .help(L("Bu videonun konuşma (kaynak) dilini seç"))
            VStack(alignment: .leading, spacing: 4) {
                Text(item.statusMessage.text)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                if item.state == .working {
                    if let fraction = item.fraction {
                        ProgressView(value: fraction)
                    } else {
                        ProgressView().progressViewStyle(.linear)
                    }
                }
            }
            .frame(width: 240, alignment: .leading)
            Button(L("Listeden kaldır"), systemImage: "xmark") { vm.removeFromPlaylist(item.id) }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .disabled(!item.isEditable)
                .help(L("Listeden kaldır"))
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder private var statusIcon: some View {
        switch item.state {
        case .pending:
            Image(systemName: "clock").foregroundStyle(.secondary)
        case .working:
            ProgressView().controlSize(.small)
        case .completed:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .failed:
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
        }
    }
}
