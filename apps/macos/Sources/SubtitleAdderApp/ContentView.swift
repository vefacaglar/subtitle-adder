import EngineKit
import LibraryKit
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    let explorer: FileExplorerModel
    let platform: VideoPlatformModel
    @State private var player = PlayerModel()
    /// Names this window to the platform, which follows one window's player at a time.
    @State private var windowToken = PlayerWindowToken()
    /// A card's "Oynat" starts the video even when Settings say not to play on open.
    @State private var forcePlay = false
    @State private var pickingVideo = false
    @State private var pickingSubtitle = false
    /// "Komple çevir" asked for a video first; the pick starts the run.
    @State private var runAfterPicking = false
    @State private var exportDocument: TextDocument?
    @State private var chrome = WindowChrome()

    var body: some View {
        NavigationSplitView(columnVisibility: sidebarVisibility) {
            FileExplorerSidebar(model: explorer, vm: vm, openVideo: showVideo)
                .navigationSplitViewColumnWidth(min: Self.sidebarMinWidth, ideal: 260, max: 420)
        } detail: {
            workspaceContent
                .frame(minWidth: Self.detailMinWidth)
        }
        .dropDestination(for: URL.self) { urls, _ in
            guard !urls.isEmpty else { return false }
            vm.openFiles(urls)
            return true
        }
        .toolbar { toolbar }
        // Theater mode: generate (⌘↩) and stop (⌘.) stay in the File menu while the toolbar is out of the way.
        .toolbar(vm.isTheaterMode ? .hidden : .automatic, for: .windowToolbar)
        // Lets the menu commands act on this window's view model only while it is the key window (not Settings).
        .focusedSceneValue(\.subtitleVM, vm)
        // ⌘O (File menu) runs this window's own picker action, offered only while a video can be picked. It is a
        // per-window action (not state on the shared view model), so it never reaches another open window.
        .focusedSceneValue(\.chooseVideo, chooseVideoAction)
        .background { WindowReader { chrome.attach($0) } }
        .onAppear { attachWindow() }
        // Closing the window stops its player and lets go of everything that points back at it.
        .onDisappear { detachWindow() }
        .navigationTitle(vm.video?.lastPathComponent ?? "Subtitle Adder")
        .task { await catalog.load() }
        .task { await explorer.start() }
        .task { await platform.start() }
        // The sidebar highlights the open video and keeps its folder's listing (and subtitle badge) current.
        .onChange(of: vm.video, initial: true) { _, url in Task { await explorer.sync(toVideo: url) } }
        // The toolbar's model picker lists what the active provider offers; the list is filled ahead of time (launch,
        // switching provider), after the window has had a moment to settle, so it is there when the popover opens.
        .task(id: vm.providerPrefetchKey) {
            try? await Task.sleep(for: .milliseconds(500))
            guard !Task.isCancelled else { return }
            await vm.refreshActiveProviderModelsIfNeeded()
        }
        .onChange(of: catalog.models, initial: true) { _, _ in
            vm.reconcileModelSelection()
            vm.reconcileTranslationModelSelection()
        }
        // A video opened any way (sidebar, picker, drop, Open With, a double click) plays by itself when Settings say so;
        // opening the video that is already open changes nothing, so it doesn't restart. A playlist run or any other
        // job works in sessions of its own and never loads the player.
        // The platform hears about it after the player has let go of the old video, so it can still report where that
        // was.
        .onChange(of: vm.video, initial: true) { _, url in
            player.load(url, autoPlay: vm.autoPlayVideo || forcePlay)
            forcePlay = false
            platform.playerVideoChanged(to: url, countsAsOpen: true)
        }
        .alert(
            L("İşlem tamamlanamadı"),
            isPresented: Binding(get: { platform.actionFailure != nil }, set: { if !$0 { platform.actionFailure = nil } })
        ) {
            Button(L("Tamam")) { platform.actionFailure = nil }
        } message: {
            Text(platform.actionFailure?.message ?? "")
        }
        // Leaving the editor stops the picture; coming back leaves it as it is.
        .onChange(of: vm.workspace) { _, workspace in
            if workspace != .editor { player.pause() }
        }
        .onChange(of: vm.sentences, initial: true) { _, list in player.setSentences(list) }
        .fileImporter(isPresented: $pickingVideo, allowedContentTypes: [.movie, .audiovisualContent], onCompletion: videoPicked)
        // A second importer on the same view isn't presented reliably, so it hangs off a background.
        .background {
            Color.clear.fileImporter(isPresented: $pickingSubtitle, allowedContentTypes: Self.subtitleTypes, onCompletion: subtitlePicked)
        }
        .confirmsOpening(in: vm)
        // An open the playlist run's claim on a video keeps from happening says so; there is nothing to confirm.
        .background {
            Color.clear.alert(
                L("İşlem tamamlanamadı"),
                isPresented: Binding(get: { vm.openRefusal != nil }, set: { if !$0 { vm.openRefusal = nil } })
            ) {
                Button(L("Tamam")) { vm.openRefusal = nil }
            } message: {
                Text(vm.openRefusal?.text ?? "")
            }
        }
        .fileExporter(
            isPresented: exportPresented,
            document: exportDocument,
            contentType: .plainText,
            defaultFilename: vm.exportFilename
        ) { vm.exportFinished($0) }
    }

    /// Hooks the window's keys and its player up. Undone by `detachWindow`.
    private func attachWindow() {
        chrome.onEscape = {
            guard vm.isTheaterMode else { return false }
            vm.isTheaterMode = false
            return true
        }
        let actions = PlayerShortcutActions(
            player: player, vm: vm, isFullScreen: { chrome.isFullScreen }, toggleFullScreen: { chrome.toggleFullScreen() }
        )
        chrome.onShortcut = { shortcut, isRepeat in actions.perform(shortcut, isRepeat: isRepeat) }
        chrome.isPanelHidden = { vm.hideSubtitlePanel }
        // The window closing or becoming key matters to the platform, whichever way SwiftUI tears the view down.
        chrome.onClose = { detachWindow() }
        chrome.onBecomeKey = { platform.claimPlayback(windowToken) }
        // What the platform needs from the player: where it is, and when it plays, stops, jumps or lets go.
        platform.attachPlayer(player, owner: windowToken)
    }

    /// The window is going away: its player is paused and its place reported (so it is saved), and the platform and the
    /// window's key handling let go. Those closures capture this view, and so the player; left in place they would keep
    /// a closed window's player alive, playing to nobody.
    private func detachWindow() {
        platform.detachPlayer(player, owner: windowToken)
        chrome.onEscape = nil
        chrome.onShortcut = nil
        chrome.isPanelHidden = { false }
        chrome.onClose = nil
        chrome.onBecomeKey = nil
    }

    /// Everything right of the sidebar: the workspace switcher, the editor or the playlist, and the status bar.
    private var workspaceContent: some View {
        VStack(spacing: 0) {
            // Full screen shows the picture alone.
            if !showsFullScreenPlayer {
                workspaceSwitcher.padding(.vertical, 8)
                Divider()
            }
            switch vm.workspace {
            case .editor: editor
            case .platform: PlatformView(model: platform, play: playFromPlatform)
            case .playlist: PlaylistView(vm: vm)
            }
            if !showsFullScreenPlayer {
                Divider()
                statusBar.padding(.horizontal, 16).padding(.vertical, 10)
            }
        }
    }

    /// The sidebar follows the stored choice, but a full-screen window hides it without changing that choice, so it is
    /// back when the window leaves full screen. Theater mode hides only the toolbar, not the sidebar.
    private var sidebarVisibility: Binding<NavigationSplitViewVisibility> {
        Binding(
            get: { explorer.isSidebarVisible && !chrome.isFullScreen ? .all : .detailOnly },
            set: { visibility in
                // Dragging the divider shut or using the system toggle ends here; what full screen does is not a choice.
                guard !chrome.isFullScreen else { return }
                explorer.isSidebarVisible = visibility != .detailOnly
            }
        )
    }

    /// The player and the editor side by side need about this much (see `playerMinWidth`, `editorMinWidth`).
    static let detailMinWidth: CGFloat = 600
    static let sidebarMinWidth: CGFloat = 220
    /// The narrowest window that holds the sidebar and the detail at their minimums. Narrower, SwiftUI folds the sidebar
    /// away by itself and reports it through the same binding as the user's own hiding, which would then be stored as
    /// their choice; the window can't get that narrow.
    static let minimumWindowWidth: CGFloat = sidebarMinWidth + detailMinWidth

    private var workspaceSwitcher: some View {
        Picker(L("Çalışma alanı"), selection: $vm.workspace) {
            Text(L("Player")).tag(Workspace.editor)
            Text(L("Platform")).tag(Workspace.platform)
            Text(vm.hasListRows ? L("Çeviri listesi (%lld)", vm.listRows.count) : L("Çeviri listesi"))
                .tag(Workspace.playlist)
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .fixedSize()
    }

    @ViewBuilder private var editor: some View {
        if vm.video == nil, vm.sentences.isEmpty {
            dropZone.padding(20)
        } else if vm.video == nil {
            // Only a subtitle file is open: edit and save it without a player.
            SentenceEditorView(vm: vm, player: player)
        } else {
            if !vm.showsSubtitlePanel(isFullScreen: chrome.isFullScreen) {
                // Hidden by the user, or full screen: the player fills the area and the layout comes back as it was.
                playerView.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if vm.sideBySideLayout {
                HSplitView {
                    playerView
                        .frame(minWidth: Self.playerMinWidth, idealWidth: 5 * Self.splitUnit, maxHeight: .infinity)
                    SentenceEditorView(vm: vm, player: player)
                        .frame(minWidth: Self.editorMinWidth, idealWidth: 4 * Self.splitUnit, maxHeight: .infinity)
                }
            } else {
                VSplitView {
                    playerView
                        .frame(maxWidth: .infinity, minHeight: Self.playerMinHeight, idealHeight: 5 * Self.splitUnit)
                    SentenceEditorView(vm: vm, player: player)
                        .frame(maxWidth: .infinity, minHeight: Self.editorMinHeight, idealHeight: 4 * Self.splitUnit)
                }
            }
        }
    }

    /// Full screen with a player on screen: nothing else is shown (no tab switcher, no status bar).
    private var showsFullScreenPlayer: Bool { vm.isPlayerShown && chrome.isFullScreen }

    // Player and editor start at about 5:4 and can't be dragged below these sizes.
    private static let splitUnit: CGFloat = 80
    private static let playerMinWidth: CGFloat = 260
    private static let editorMinWidth: CGFloat = 320
    private static let playerMinHeight: CGFloat = 180
    private static let editorMinHeight: CGFloat = 160

    private var playerView: some View {
        PlayerView(
            model: player,
            sentences: vm.sentences,
            translations: vm.translations,
            translationTitle: vm.translationTitle,
            display: Binding(
                get: { vm.effectiveDisplayMode },
                set: { vm.displayMode = $0 }
            ),
            sideBySide: $vm.sideBySideLayout,
            theaterMode: $vm.isTheaterMode,
            hideSubtitlePanel: $vm.hideSubtitlePanel,
            isFullScreen: chrome.isFullScreen,
            toggleFullScreen: { chrome.toggleFullScreen() },
            platform: platform
        )
    }

    /// The video picker answered. Asks first when opening would stop a running job or drop edited lines; "Komple çevir"
    /// asked for the pick, so it starts the run once the video is open.
    private func videoPicked(_ result: Result<URL, any Error>) {
        let run = runAfterPicking
        runAfterPicking = false
        guard case .success(let url) = result else { return }
        if run {
            vm.requestOpenVideo(url) { vm.generateAndTranslate() }
        } else {
            vm.requestOpenVideo(url)
        }
    }

    private func subtitlePicked(_ result: Result<URL, any Error>) {
        if case .success(let url) = result { vm.requestOpenSubtitle(url) }
    }

    /// A card's "Oynat": shows the editor with the video and starts it. Nothing here waits for the profile; the view
    /// is counted in the background once the video is open.
    private func playFromPlatform(_ url: URL) {
        showVideo(url, startPlaying: true)
    }

    /// Shows the editor with `url` (a sidebar row, a card). `startPlaying` plays it even when Settings say not to on open;
    /// otherwise the setting decides. Opening a video leaves the other videos' jobs and edits alone, so it only waits for
    /// the user's yes when it would lose lines that belong to no video (`requestOpen`), and nothing changes until it
    /// comes, not even the tab.
    private func showVideo(_ url: URL, startPlaying: Bool) {
        let alreadyOpen = vm.video.map(VideoPlatformModel.pathKey) == VideoPlatformModel.pathKey(url)
        vm.requestOpen(losing: vm.sessionsLost(opening: url)) {
            vm.workspace = .editor
            if alreadyOpen {
                if startPlaying { player.play() }
            } else {
                forcePlay = startPlaying
                vm.openVideo(url)
            }
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        // Items that come and go are toolbar items of their own: shown or hidden inside a group, they leave AppKit's
        // toolbar layout stale and the group's remaining items are drawn on top of each other.
        if vm.workspace == .editor {
            ToolbarItemGroup {
                Button(L("Video seç"), systemImage: "film") { chooseVideo() }
                    .disabled(!vm.canPickVideo)
                    .help(L("Video dosyası seç (⌘O)"))
                Button(L("Altyazı aç"), systemImage: "doc.text") { pickingSubtitle = true }
                    .disabled(vm.isWorking)
                    .help(L("Altyazı dosyası seç (.srt, .vtt, .ass) — yeniden ses ayırmadan yükle"))
            }
        }
        if vm.workspace != .platform {
            engineToolbar
        }
        if vm.workspace == .editor, !vm.sentences.isEmpty {
            ToolbarItem {
                Button(L("Çevir…"), systemImage: "translate") { vm.translate() }
                    .labelStyle(.titleAndIcon)
                    .disabled(vm.isWorking)
                    .help(L("Tüm satırları seçili hedef dile yerel modelle çevir"))
            }
        }
        if vm.workspace != .platform {
            ToolbarItem {
                TranslationModelPickerButton(vm: vm)
            }
        }
        ToolbarItemGroup(placement: .primaryAction) {
            // The playlist always saves what it translates, so the toggle only belongs to the editor.
            if vm.workspace == .editor {
                Toggle(L("Otomatik kaydet"), systemImage: "square.and.arrow.down", isOn: $vm.autoSaveSubtitle)
                    .help(L("Çeviri bittiğinde çevrilmiş altyazıyı otomatik olarak videonun klasörüne kaydet"))
            }
            if vm.canStop {
                Button(L("Durdur"), systemImage: "stop.fill") { vm.cancel() }
                    .labelStyle(.titleAndIcon)
                    .help(L("Süren altyazı ya da çeviri işini durdur (⌘.)"))
            } else if vm.workspace == .editor {
                Button(L("Komple çevir"), systemImage: "wand.and.stars") { generateAndTranslate() }
                    .labelStyle(.titleAndIcon)
                    .buttonStyle(.borderedProminent)
                    .disabled(!vm.canGenerateAndTranslate)
                    .help(L("Altyazıyı üret, seçili dile çevir ve otomatik kaydet"))
                Button(L("Altyazı üret"), systemImage: "captions.bubble") { vm.generate() }
                    .labelStyle(.titleAndIcon)
                    .disabled(vm.video == nil)
                    .help(L("Videonun sesini metne çevirip altyazı çıkar (⌘↩)"))
            }
        }
        ToolbarItem(placement: .primaryAction) {
            ProfileToolbarButton(model: platform) { vm.workspace = .platform }
        }
    }

    /// The pickers for the recognition language and model, the speakers and the translation language. The platform has
    /// no use for them.
    @ToolbarContentBuilder
    private var engineToolbar: some ToolbarContent {
        ToolbarItemGroup {
            Picker(L("Dil"), selection: $vm.language) {
                ForEach(SubtitleLanguage.allCases) { language in
                    Text(language.title).tag(language.rawValue)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .frame(width: 175)
            .help(L("Konuşma dili. Otomatik seçilirse model dili kendisi algılar; diğer modeller sabit dil listesiyle çalışır."))
            ModelPicker(selection: $vm.model, models: vm.availableModels)
            speakerMenu
            // Always there: "Komple çevir" and the playlist translate before any line exists, so the target and the
            // translation model must be visible and changeable up front. Only "Çevir…" needs lines.
            Picker(L("Çeviri dili"), selection: $vm.translationTarget) {
                ForEach(SubtitleLanguage.translationTargets) { language in
                    Text(language.targetName).tag(language.rawValue)
                        .disabled(language.rawValue == vm.translationSource)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .frame(width: 160)
            .disabled(vm.isWorking)
            .help(vm.translationSource == nil
                ? L("Çeviri hedef dili. Kaynak dil bilinmiyor; NLLB için önce konuşma dilini seç.")
                : L("Çeviri hedef dili; kaynak dille aynı olan seçilemez."))
        }
    }

    private var chooseVideoAction: ChooseVideoAction? {
        vm.canPickVideo ? ChooseVideoAction { chooseVideo() } : nil
    }

    private var exportPresented: Binding<Bool> {
        Binding(get: { exportDocument != nil }, set: { if !$0 { exportDocument = nil } })
    }

    private func chooseVideo() {
        runAfterPicking = false
        pickingVideo = true
    }

    /// With neither a video nor lines there is nothing to run on, so "Komple çevir" asks for a video and runs on it.
    private func generateAndTranslate() {
        if vm.hasSourceForGenerateAndTranslate {
            vm.generateAndTranslate()
        } else {
            runAfterPicking = true
            pickingVideo = true
        }
    }

    private static let subtitleTypes: [UTType] = SubtitleParser.extensions.compactMap { UTType(filenameExtension: $0) } + [.plainText]

    /// Speaker separation options for the next "Altyazı üret" run.
    private var speakerMenu: some View {
        Menu {
            Toggle(L("Konuşmacıları ayır"), isOn: $vm.diarize)
            Picker(L("Konuşmacı sayısı"), selection: $vm.speakerCount) {
                Text(L("Otomatik")).tag(0)
                ForEach(2...6, id: \.self) { count in
                    Text("\(count)").tag(count)
                }
            }
            .disabled(!vm.diarize)
        } label: {
            Label(L("Konuşmacılar"), systemImage: vm.diarize ? "person.2.fill" : "person.2")
        }
        .disabled(vm.isWorking)
        .help(L("Videoda birden çok kişi konuşuyorsa kimin ne dediğini ayır; ilk kullanımda küçük bir model indirilir."))
    }

    private var dropZone: some View {
        VStack(spacing: 12) {
            Image(systemName: "film.stack")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(.secondary)
            Text(L("Videoyu buraya sürükle"))
                .font(.title3)
            Text(L("ya da araç çubuğundan seç"))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 16))
        .overlay(
            RoundedRectangle(cornerRadius: 16)
                .strokeBorder(style: StrokeStyle(lineWidth: 1.5, dash: [7]))
                .foregroundStyle(.tertiary)
        )
    }

    private func playbackNoticeView(_ notice: LocalizedMessage) -> some View {
        HStack(spacing: 4) {
            Label(notice.text, systemImage: "exclamationmark.triangle.fill")
                .font(.caption)
                .foregroundStyle(.red)
                .lineLimit(1)
                .truncationMode(.tail)
                .help(notice.text)
            Button(L("Kapat"), systemImage: "xmark.circle.fill") { player.dismissFailure() }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .help(L("Kapat"))
        }
    }

    private var statusBar: some View {
        HStack(spacing: 10) {
            switch vm.status {
            case .idle where vm.isAsyncTranslating:
                // Background translation with nothing else running.
                if let fraction = vm.asyncTranslationFraction {
                    ProgressView(value: fraction).frame(width: 120)
                } else {
                    ProgressView().controlSize(.small)
                }
                Text(vm.asyncStatusText)
            case .idle:
                Text(vm.workspace != .editor || vm.video == nil || !vm.sentences.isEmpty ? L("Hazır") : L("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (⌘↩)")).foregroundStyle(.secondary)
            case .working(let label, let fraction):
                if let fraction {
                    ProgressView(value: fraction).frame(width: 120)
                } else {
                    ProgressView().controlSize(.small)
                }
                Text(label.text)
                if let detail = vm.asyncStatusDetail {
                    Text("· " + detail).foregroundStyle(.secondary)
                }
            case .finished:
                Label(vm.finishedSummary, systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .lineLimit(1)
                    .truncationMode(.middle)
            case .failed(let message):
                Label(message.text, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .lineLimit(2)
                if vm.canGoOnlineAndRetry {
                    Button(vm.offlineRetryTitle) { vm.goOnlineAndRetry() }
                }
            }
            if let notice = vm.playbackNotice(player.failureMessage) {
                // Playback only; generating subtitles still works (the engine decodes the audio itself). Shown next to
                // the status, never instead of it, and can be dismissed.
                playbackNoticeView(notice)
            }
            Spacer()
            if let notice = vm.modelNotice {
                Label(notice.text, systemImage: "info.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .help(L("Seçili model bu dil için listede yoktu; en doğru uygun model seçildi."))
            }
            if vm.workspace != .platform {
                Picker(L("Biçim"), selection: $vm.exportFormat) {
                    ForEach(SubtitleFormat.allCases) { Text($0.rawValue.uppercased()).tag($0) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 150)
            }
            if vm.workspace == .editor {
                exportControls
            }
        }
        .font(.callout)
    }

    /// Which text the file carries, and the buttons that write it.
    @ViewBuilder private var exportControls: some View {
        HStack(spacing: 10) {
            Picker(L("Dil"), selection: $vm.exportLanguage) {
                Text(L("Orijinal")).tag(ExportLanguage.original)
                Text(vm.translationTitle).tag(ExportLanguage.translated)
                    .disabled(vm.translations.isEmpty)
            }
            .pickerStyle(.menu)
            .frame(width: 180)
            .help(L("Dışa aktarılan altyazıda orijinal mi, çeviri mi kullanılsın"))
            Button(L("Altyazıyı kaydet"), systemImage: "square.and.arrow.down") {
                Task { await vm.saveSubtitle() }
            }
            .disabled(!vm.canSaveSubtitle)
            .help(L("Altyazıyı videonun bulunduğu klasöre videoyla aynı adla kaydet"))
            Button(L("Dışa aktar…")) {
                Task {
                    if let text = await vm.render(as: vm.exportFormat, translated: vm.exportsTranslation) {
                        exportDocument = TextDocument(text: text)
                    }
                }
            }
            .disabled(!vm.canExport)
        }
        .font(.callout)
    }
}

enum SubtitleTime {
    static func short(_ seconds: Double) -> String {
        String(format: "%02d:%05.2f", Int(seconds) / 60, seconds.truncatingRemainder(dividingBy: 60))
    }
}

struct TextDocument: FileDocument {
    static let readableContentTypes: [UTType] = [.plainText]
    var text: String

    init(text: String) {
        self.text = text
    }

    init(configuration: ReadConfiguration) throws {
        text = String(decoding: configuration.file.regularFileContents ?? Data(), as: UTF8.self)
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: Data(text.utf8))
    }
}
