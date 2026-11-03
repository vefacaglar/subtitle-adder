import AppKit
import EngineKit
import LibraryKit
import SwiftUI

/// The left sidebar: browse folders, search the media index, open a video in the editor or put it on the playlist.
struct FileExplorerSidebar: View {
    let model: FileExplorerModel
    let vm: SubtitleViewModel
    /// Shows the video in the editor, after the user's yes when that stops work (see `SubtitleViewModel.requestOpen`).
    /// The flag is whether it plays at once even when Settings say not to play on open.
    let openVideo: (URL, _ startPlaying: Bool) -> Void

    var body: some View {
        VStack(spacing: 0) {
            header
            list
            Divider()
            footer
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L("Dosya Gezgini"))
    }

    // MARK: Header

    private var header: some View {
        VStack(spacing: 8) {
            HStack(spacing: 2) {
                folderMenu
                Spacer(minLength: 0)
                Button(L("Geri"), systemImage: "chevron.backward") { Task { await model.goBack() } }
                    .disabled(!model.canGoBack)
                    .help(L("Geri"))
                Button(L("Üst klasör"), systemImage: "arrow.up") { Task { await model.goUp() } }
                    .disabled(!model.canGoUp)
                    .help(L("Üst klasör"))
                Button(L("Yenile"), systemImage: "arrow.clockwise") { Task { await model.refresh() } }
                    .help(L("Yenile"))
                Button(L("Finder'da göster"), systemImage: "arrow.up.right.square") {
                    NSWorkspace.shared.open(model.currentFolderURL)
                }
                .disabled(model.availability == .volumeNotMounted)
                .help(L("Finder'da göster"))
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderless)
            searchField
        }
        .padding(.horizontal, 10)
        .padding(.top, 8)
        .padding(.bottom, 8)
    }

    /// The current folder's name; its items are the quick locations.
    private var folderMenu: some View {
        Menu {
            ForEach(model.quickLocations) { location in
                Button(location.title, systemImage: location.symbolName) {
                    Task { await model.navigate(to: location.url) }
                }
            }
        } label: {
            Label(model.currentFolderName, systemImage: "folder")
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.visible)
        .fontWeight(.semibold)
        .help(model.currentFolder)
        .accessibilityLabel(L("Hızlı konumlar"))
        .accessibilityValue(model.currentFolder)
    }

    private var searchField: some View {
        @Bindable var model = model
        return TextField(L("Klasörde veya indekste ara…"), text: $model.searchQuery)
            .textFieldStyle(.roundedBorder)
            .controlSize(.small)
            .overlay(alignment: .trailing) {
                if !model.searchQuery.isEmpty {
                    Button(L("Temizle"), systemImage: "xmark.circle.fill") { model.searchQuery = "" }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.borderless)
                        .foregroundStyle(.secondary)
                        .padding(.trailing, 4)
                        .help(L("Temizle"))
                }
            }
    }

    // MARK: List

    private var list: some View {
        List {
            ForEach(model.folders) { folder in
                Button { Task { await model.navigate(to: URL(filePath: folder.path, directoryHint: .isDirectory)) } } label: {
                    FolderRow(folder: folder)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(folder.name)
                .help(folder.path)
                .contextMenu {
                    Button(L("Finder'da aç"), systemImage: "folder") {
                        NSWorkspace.shared.open(URL(filePath: folder.path, directoryHint: .isDirectory))
                    }
                }
            }
            ForEach(model.files) { file in
                let isActive = model.isActive(file)
                Button { open(file, startPlaying: false) } label: {
                    MediaRow(file: file, isActive: isActive, showsFolder: model.isOutsideCurrentFolder(file))
                }
                .buttonStyle(.plain)
                .accessibilityLabel(file.fileName)
                .accessibilityValue(MediaRow.detail(of: file, showsFolder: model.isOutsideCurrentFolder(file)))
                .help(file.fullPath)
                .listRowBackground(isActive ? RoundedRectangle(cornerRadius: 6).fill(Color.accentColor.opacity(0.18)) : nil)
                .contextMenu {
                    Button(L("Oynat"), systemImage: "play.fill") { open(file, startPlaying: true) }
                    Button(L("Çalma listesine ekle"), systemImage: "text.badge.plus") { addToPlaylist(file) }
                    Divider()
                    Button(L("Finder'da göster"), systemImage: "magnifyingglass") {
                        NSWorkspace.shared.activateFileViewerSelecting([file.url])
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .overlay { placeholder }
    }

    /// Centered text for a folder with nothing to list; the spinner in the footer covers a scan in progress.
    @ViewBuilder private var placeholder: some View {
        if model.isEmpty, !model.isScanning {
            let content = placeholderContent
            ContentUnavailableView(content.text, systemImage: content.symbol)
                .font(.callout)
        }
    }

    private var placeholderContent: (text: String, symbol: String) {
        if model.isSearching { return (L("Eşleşen medya dosyası bulunamadı"), "magnifyingglass") }
        switch model.availability {
        case .available: return (L("Bu klasörde medya dosyası bulunamadı"), "folder")
        case .volumeNotMounted: return (L("Bu birim bağlı değil"), "externaldrive.badge.xmark")
        case .notFound: return (L("Klasör bulunamadı"), "questionmark.folder")
        case .unreadable: return (L("Klasör okunamadı"), "lock")
        }
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 6) {
            if model.isScanning {
                ProgressView().controlSize(.small)
            }
            Text(model.statusSummary)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }

    // MARK: Actions

    /// A click opens the video, and Settings decide whether it plays; "Oynat" plays it whatever they say.
    private func open(_ file: IndexedMediaFile, startPlaying: Bool) {
        openVideo(file.url, startPlaying)
    }

    private func addToPlaylist(_ file: IndexedMediaFile) {
        vm.addToPlaylist([file.url])
        vm.workspace = .playlist
    }
}

private struct FolderRow: View {
    let folder: ExplorerFolder

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "folder.fill")
                .foregroundStyle(.tint)
                .frame(width: 18)
            Text(folder.name)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 4)
            Image(systemName: "chevron.right")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
    }
}

private struct MediaRow: View {
    let file: IndexedMediaFile
    let isActive: Bool
    let showsFolder: Bool

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: MediaFileTypes.isAudio(file.url) ? "waveform" : "film")
                .foregroundStyle(isActive ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 1) {
                Text(file.fileName)
                    .fontWeight(isActive ? .semibold : .regular)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            Spacer(minLength: 4)
            if file.hasSidecarSubtitle {
                Text(badge)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 1)
                    .background(.quaternary, in: Capsule())
                    .help(L("Altyazı dosyası mevcut"))
            }
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
    }

    private var detail: String { Self.detail(of: file, showsFolder: showsFolder) }

    /// "1,2 GB", plus " · <folder>" for a file that is not in the folder being browsed.
    static func detail(of file: IndexedMediaFile, showsFolder: Bool) -> String {
        let size = ByteCountFormatter.string(fromByteCount: file.sizeBytes, countStyle: .file)
        guard showsFolder else { return size }
        return size + " · " + ExplorerPaths.name(of: file.directoryPath)
    }

    private var badge: String {
        let ext = file.sidecarExtension?.drop { $0 == "." }.uppercased() ?? ""
        return ext.isEmpty ? "CC" : ext
    }
}

/// View menu: show or hide the sidebar (⌃⌘S). It replaces the system's own item so that the label follows the app
/// language and the choice is the one stored in the settings.
struct FileExplorerCommands: Commands {
    let explorer: FileExplorerModel

    var body: some Commands {
        CommandGroup(replacing: .sidebar) {
            Button(L("Kenar çubuğunu göster / gizle"), systemImage: "sidebar.left") {
                explorer.isSidebarVisible.toggle()
            }
            .keyboardShortcut("s", modifiers: [.control, .command])
        }
    }
}
