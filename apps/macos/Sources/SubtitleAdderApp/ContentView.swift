import EngineKit
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    @State private var player = PlayerModel()
    @State private var pickingVideo = false
    @State private var exportFormat: SubtitleFormat = .srt
    @State private var exportDocument: TextDocument?

    var body: some View {
        VStack(spacing: 0) {
            if vm.video == nil {
                dropZone.padding(20)
            } else {
                VSplitView {
                    PlayerView(model: player, sentences: vm.sentences)
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
        .onChange(of: catalog.models, initial: true) { _, _ in vm.reconcileModelSelection() }
        .onChange(of: vm.video, initial: true) { _, url in player.load(url) }
        .onChange(of: vm.sentences, initial: true) { _, list in player.setSentences(list) }
        .fileImporter(isPresented: $pickingVideo, allowedContentTypes: [.movie, .audiovisualContent]) { result in
            if case .success(let url) = result { vm.openVideo(url) }
        }
        .fileExporter(
            isPresented: Binding(get: { exportDocument != nil }, set: { if !$0 { exportDocument = nil } }),
            document: exportDocument,
            contentType: .plainText,
            defaultFilename: (vm.video?.deletingPathExtension().lastPathComponent ?? "subtitles") + "." + exportFormat.rawValue
        ) { _ in }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup {
            Button("Video seç", systemImage: "film") { pickingVideo = true }
                .help("Video dosyası seç")
            Picker("Dil", selection: $vm.language) {
                ForEach(SubtitleLanguage.allCases) { language in
                    Text(language.title).tag(language.rawValue)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .frame(width: 175)
            .help("Konuşma dili. Otomatik seçilirse model dili kendisi algılar; diğer modeller sabit dil listesiyle çalışır.")
            ModelPicker(selection: $vm.model, models: vm.availableModels)
        }
        ToolbarItem(placement: .primaryAction) {
            if vm.isWorking {
                Button("Durdur", systemImage: "stop.fill") { vm.cancel() }
                    .labelStyle(.titleAndIcon)
                    .help("Altyazı üretimini durdur")
            } else {
                Button("Altyazı üret", systemImage: "captions.bubble") { vm.generate() }
                    .labelStyle(.titleAndIcon)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(vm.video == nil)
                    .help("Videonun sesini metne çevirip altyazı çıkar (⌘↩)")
            }
        }
    }

    private var dropZone: some View {
        VStack(spacing: 12) {
            Image(systemName: "film.stack")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(.secondary)
            Text("Videoyu buraya sürükle")
                .font(.title3)
            Text("ya da araç çubuğundan seç")
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
                Text(vm.video == nil || !vm.sentences.isEmpty ? "Hazır" : "Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (⌘↩)").foregroundStyle(.secondary)
            case .working(let label, let fraction):
                if let fraction {
                    ProgressView(value: fraction).frame(width: 120)
                } else {
                    ProgressView().controlSize(.small)
                }
                Text(label)
            case .finished:
                Label("\(vm.sentences.count) satır · dil: \(vm.detectedLanguage ?? "?")", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            case .failed(let message):
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }
            Spacer()
            if let notice = vm.modelNotice {
                Label(notice, systemImage: "info.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .help("Seçili model bu dil için listede yoktu; en doğru uygun model seçildi.")
            }
            Picker("Biçim", selection: $exportFormat) {
                ForEach(SubtitleFormat.allCases) { Text($0.rawValue.uppercased()).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 150)
            Button("Dışa aktar…") {
                Task {
                    if let text = await vm.render(as: exportFormat) { exportDocument = TextDocument(text: text) }
                }
            }
            .disabled(vm.sentences.isEmpty || vm.isWorking)
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
