import EngineKit
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    @State private var player = PlayerModel()
    @State private var pickingVideo = false
    @State private var exportFormat: SubtitleFormat = .srt
    @State private var exportLanguage: ExportLanguage = .original
    @State private var exportDocument: TextDocument?

    var body: some View {
        VStack(spacing: 0) {
            if vm.video == nil {
                dropZone.padding(20)
            } else {
                VSplitView {
                    PlayerView(
                        model: player,
                        sentences: vm.sentences,
                        translations: vm.translations,
                        display: $vm.displayMode
                    )
                    .frame(minHeight: 200, idealHeight: 360)
                    SentenceEditorView(vm: vm, player: player)
                        .frame(minHeight: 180)
                }
            }
            Divider()
            statusBar.padding(.horizontal, 16).padding(.vertical, 10)
        }
        .dropDestination(for: URL.self) { urls, _ in
            guard let url = urls.first else { return false }
            vm.openVideo(url)
            return true
        }
        .toolbar { toolbar }
        .navigationTitle(vm.video?.lastPathComponent ?? "Subtitle Adder")
        .task { await catalog.load() }
        .onChange(of: catalog.models, initial: true) { _, _ in
            vm.reconcileModelSelection()
            vm.reconcileTranslationModelSelection()
        }
        .onChange(of: vm.video, initial: true) { _, _ in
            exportLanguage = .original
        }
        .onChange(of: vm.translationLanguage) { _, language in
            if language == nil { exportLanguage = .original }
        }
        .onChange(of: vm.video, initial: true) { _, url in player.load(url) }
        .onChange(of: vm.sentences, initial: true) { _, list in player.setSentences(list) }
        .fileImporter(isPresented: $pickingVideo, allowedContentTypes: [.movie, .audiovisualContent]) { result in
            if case .success(let url) = result { vm.openVideo(url) }
        }
        .fileExporter(
            isPresented: Binding(get: { exportDocument != nil }, set: { if !$0 { exportDocument = nil } }),
            document: exportDocument,
            contentType: .plainText,
            defaultFilename: exportFilename
        ) { _ in }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup {
            Button(L("Video seç"), systemImage: "film") { pickingVideo = true }
                .help(L("Video dosyası seç"))
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
            if !vm.sentences.isEmpty {
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
                Button(L("Çevir…"), systemImage: "translate") { vm.translate() }
                    .labelStyle(.titleAndIcon)
                    .disabled(vm.isWorking)
                    .help(L("Tüm satırları seçili hedef dile yerel modelle çevir"))
            }
        }
        ToolbarItem(placement: .primaryAction) {
            if vm.isWorking {
                Button(L("Durdur"), systemImage: "stop.fill") { vm.cancel() }
                    .labelStyle(.titleAndIcon)
                    .help(L("Süren altyazı ya da çeviri işini durdur"))
            } else {
                Button(L("Altyazı üret"), systemImage: "captions.bubble") { vm.generate() }
                    .labelStyle(.titleAndIcon)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(vm.video == nil)
                    .help(L("Videonun sesini metne çevirip altyazı çıkar (⌘↩)"))
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
            case .idle:
                Text(vm.video == nil || !vm.sentences.isEmpty ? L("Hazır") : L("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (⌘↩)")).foregroundStyle(.secondary)
            case .working(let label, let fraction):
                if let fraction {
                    ProgressView(value: fraction).frame(width: 120)
                } else {
                    ProgressView().controlSize(.small)
                }
                Text(label.text)
            case .finished:
                Label(finishedSummary, systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            case .failed(let message):
                Label(message.text, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }
            Spacer()
            if let notice = vm.modelNotice {
                Label(notice.text, systemImage: "info.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .help(L("Seçili model bu dil için listede yoktu; en doğru uygun model seçildi."))
            }
            Picker(L("Biçim"), selection: $exportFormat) {
                ForEach(SubtitleFormat.allCases) { Text($0.rawValue.uppercased()).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 150)
            Picker(L("Dil"), selection: $exportLanguage) {
                Text(L("Orijinal")).tag(ExportLanguage.original)
                Text(vm.translationLanguage.map { L("Çeviri (%@)", $0) } ?? L("Çeviri")).tag(ExportLanguage.translated)
                    .disabled(vm.translations.isEmpty)
            }
            .pickerStyle(.menu)
            .frame(width: 180)
            .help(L("Dışa aktarılan altyazıda orijinal mi, çeviri mi kullanılsın"))
            Button(L("Dışa aktar…")) {
                Task {
                    let translated = exportLanguage == .translated && !vm.translations.isEmpty
                    if let text = await vm.render(as: exportFormat, translated: translated) {
                        exportDocument = TextDocument(text: text)
                    }
                }
            }
            .disabled(vm.sentences.isEmpty || vm.isWorking)
        }
        .font(.callout)
    }

    private var finishedSummary: String {
        var summary = L("%lld satır · dil: %@", vm.sentences.count, vm.detectedLanguage ?? "?")
        if let language = vm.translationLanguage {
            summary += L(" · çeviri: %@", language)
        }
        return summary
    }

    private var exportFilename: String {
        let base = vm.video?.deletingPathExtension().lastPathComponent ?? "subtitles"
        let suffix = exportLanguage == .translated ? ".\(vm.translationLanguage ?? "translated")" : ""
        return "\(base)\(suffix).\(exportFormat.rawValue)"
    }
}

enum ExportLanguage: String, CaseIterable, Identifiable, Sendable {
    case original, translated
    var id: String { rawValue }
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
