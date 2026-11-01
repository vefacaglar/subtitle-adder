import EngineKit
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    @State private var player = PlayerModel()
    @State private var pickingVideo = false
    @State private var pickingSubtitle = false
    /// "Komple çevir" asked for a video first; the pick starts the run.
    @State private var runAfterPicking = false
    @State private var exportDocument: TextDocument?
    @State private var chrome = WindowChrome()

    var body: some View {
        VStack(spacing: 0) {
            workspaceSwitcher.padding(.vertical, 8)
            Divider()
            switch vm.workspace {
            case .editor: editor
            case .playlist: PlaylistView(vm: vm)
            }
            Divider()
            statusBar.padding(.horizontal, 16).padding(.vertical, 10)
        }
        .dropDestination(for: URL.self) { urls, _ in
            guard !urls.isEmpty else { return false }
            vm.openFiles(urls)
            return true
        }
        .toolbar { toolbar }
        // Theater mode: the commands live in the menu bar (⌘↩ and the others) while the toolbar is out of the way.
        .toolbar(vm.isTheaterMode ? .hidden : .automatic, for: .windowToolbar)
        .background { WindowReader { chrome.attach($0) } }
        .onAppear {
            chrome.onEscape = {
                guard vm.isTheaterMode else { return false }
                vm.isTheaterMode = false
                return true
            }
        }
        .navigationTitle(vm.video?.lastPathComponent ?? "Subtitle Adder")
        .task { await catalog.load() }
        // The toolbar's model menu lists what the server offers; a menu can't fetch when it opens, so the list is
        // filled ahead of time. Typing an address changes it on every key, hence the pause.
        .task(id: vm.externalLLM?.normalizedURL) {
            try? await Task.sleep(for: .milliseconds(800))
            guard !Task.isCancelled else { return }
            await vm.refreshExternalModelsIfNeeded()
        }
        .onChange(of: catalog.models, initial: true) { _, _ in
            vm.reconcileModelSelection()
            vm.reconcileTranslationModelSelection()
        }
        // A playlist run goes through many videos; the player stays empty until it is over.
        .onChange(of: vm.isPlaylistRunning ? nil : vm.video, initial: true) { _, url in player.load(url) }
        .onChange(of: vm.workspace) { _, _ in player.pause() }
        .onChange(of: vm.sentences, initial: true) { _, list in player.setSentences(list) }
        .fileImporter(isPresented: $pickingVideo, allowedContentTypes: [.movie, .audiovisualContent]) { result in
            let run = runAfterPicking
            runAfterPicking = false
            guard case .success(let url) = result else { return }
            vm.openVideo(url)
            if run { vm.generateAndTranslate() }
        }
        // A second importer on the same view isn't presented reliably, so it hangs off a background.
        .background {
            Color.clear.fileImporter(isPresented: $pickingSubtitle, allowedContentTypes: Self.subtitleTypes) { result in
                if case .success(let url) = result { vm.loadSubtitleFile(url) }
            }
        }
        .fileExporter(
            isPresented: Binding(get: { exportDocument != nil }, set: { if !$0 { exportDocument = nil } }),
            document: exportDocument,
            contentType: .plainText,
            defaultFilename: vm.exportFilename
        ) { _ in }
    }

    private var workspaceSwitcher: some View {
        Picker(L("Çalışma alanı"), selection: $vm.workspace) {
            Text(L("Düzenleyici")).tag(Workspace.editor)
            Text(vm.hasPlaylistItems ? L("Çalma listesi (%lld)", vm.playlist.count) : L("Çalma listesi"))
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
        } else if vm.isPlaylistRunning {
            SentenceEditorView(vm: vm, player: player)
        } else {
            if vm.sideBySideLayout {
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
            isFullScreen: chrome.isFullScreen,
            toggleFullScreen: { chrome.toggleFullScreen() }
        )
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup {
            if vm.workspace == .editor {
                Button(L("Video seç"), systemImage: "film") {
                    runAfterPicking = false
                    pickingVideo = true
                }
                .disabled(vm.isPlaylistRunning)
                .help(L("Video dosyası seç"))
                Button(L("Altyazı aç"), systemImage: "doc.text") { pickingSubtitle = true }
                    .disabled(vm.isWorking)
                    .help(L("Altyazı dosyası seç (.srt, .vtt, .ass) — yeniden ses ayırmadan yükle"))
            }
            Picker(L("Dil"), selection: $vm.language) {
                ForEach(SubtitleLanguage.allCases) { language in
                    Text(language.title).tag(language.rawValue)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .frame(width: 175)
            .disabled(vm.isPlaylistRunning)
            .help(L("Konuşma dili. Otomatik seçilirse model dili kendisi algılar; diğer modeller sabit dil listesiyle çalışır."))
            ModelPicker(selection: $vm.model, models: vm.availableModels)
                .disabled(vm.isPlaylistRunning)
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
            if vm.workspace == .editor, !vm.sentences.isEmpty {
                Button(L("Çevir…"), systemImage: "translate") { vm.translate() }
                    .labelStyle(.titleAndIcon)
                    .disabled(vm.isWorking)
                    .help(L("Tüm satırları seçili hedef dile yerel modelle çevir"))
            }
            translationModelMenu
        }
        ToolbarItemGroup(placement: .primaryAction) {
            // The playlist always saves what it translates, so the toggle only belongs to the editor.
            if vm.workspace == .editor {
                Toggle(L("Otomatik kaydet"), systemImage: "square.and.arrow.down", isOn: $vm.autoSaveSubtitle)
                    .help(L("Çeviri bittiğinde çevrilmiş altyazıyı otomatik olarak videonun klasörüne kaydet"))
            }
            if vm.isWorking {
                Button(L("Durdur"), systemImage: "stop.fill") { vm.cancel() }
                    .labelStyle(.titleAndIcon)
                    .help(L("Süren altyazı ya da çeviri işini durdur"))
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

    /// The model "Çevir…" uses: local LLMs first, then MADLAD and NLLB (the same choice Settings shows), or the models
    /// of the external server when one is set up.
    private var translationModelMenu: some View {
        Menu {
            if vm.isExternalLLMActive {
                externalModelItems
            } else {
                localModelItems
            }
        } label: {
            if vm.isExternalLLMActive {
                Label(vm.translationModelTitle, systemImage: "network")
                    .labelStyle(.titleAndIcon)
            } else {
                Text(vm.translationModelTitle)
                    .frame(minWidth: 110, alignment: .leading)
            }
        }
        .disabled(vm.isWorking)
        .accessibilityLabel(L("Çeviri modeli"))
        .help(vm.isExternalLLMActive
            ? L("Çeviriyi yerel ağdaki başka bir bilgisayara veya Mac'e (LM Studio, Ollama, llama-server) yaptırır.")
            : L("Çeviri motoru (Gemma 4, Qwen 3.5, MADLAD, NLLB); ilk kullanımda indirilir."))
    }

    @ViewBuilder private var localModelItems: some View {
        ForEach(catalog.llamaTranslationModels) { translationModelButton($0) }
        if !catalog.llamaTranslationModels.isEmpty, !catalog.classicTranslationModels.isEmpty {
            Divider()
        }
        ForEach(catalog.classicTranslationModels) { translationModelButton($0) }
    }

    /// Models the external server listed, then the refresh action.
    @ViewBuilder private var externalModelItems: some View {
        ForEach(vm.externalModels, id: \.self) { name in
            Button {
                vm.externalLlmModel = name
            } label: {
                if name == vm.externalLlmModel {
                    Label(name, systemImage: "checkmark")
                } else {
                    Text(name)
                }
            }
        }
        if !vm.externalModels.isEmpty { Divider() }
        Button(L("Uzaktaki modelleri listele"), systemImage: "arrow.clockwise") {
            Task { await vm.testExternalConnection() }
        }
        .disabled(vm.isTestingExternalConnection)
    }

    private func translationModelButton(_ model: EngineModel) -> some View {
        Button {
            vm.translationModel = model.id
        } label: {
            if model.id == vm.translationModel {
                Label(ModelLabel.translationMenuTitle(for: model), systemImage: "checkmark")
            } else {
                Text(ModelLabel.translationMenuTitle(for: model))
            }
        }
    }

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
                Text(vm.workspace == .playlist || vm.video == nil || !vm.sentences.isEmpty ? L("Hazır") : L("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (⌘↩)")).foregroundStyle(.secondary)
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
                    Button(L("Çevrimdışı modu kapat ve indir")) { vm.goOnlineAndRetry() }
                }
            }
            Spacer()
            if let notice = vm.modelNotice {
                Label(notice.text, systemImage: "info.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .help(L("Seçili model bu dil için listede yoktu; en doğru uygun model seçildi."))
            }
            Picker(L("Biçim"), selection: $vm.exportFormat) {
                ForEach(SubtitleFormat.allCases) { Text($0.rawValue.uppercased()).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 150)
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
