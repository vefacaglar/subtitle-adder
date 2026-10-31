import EngineKit
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @State private var vm = SubtitleViewModel()
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
            TextField("Dil (otomatik)", text: $vm.language)
                .frame(width: 110)
            Picker("Model", selection: $vm.model) {
                ForEach(SubtitleViewModel.models, id: \.self) { Text($0) }
            }
            .frame(width: 120)
            if vm.isWorking {
                Button("Durdur", systemImage: "stop.fill") { vm.cancel() }
            } else {
                Button("Altyazı üret", systemImage: "captions.bubble") { vm.generate() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(vm.video == nil)
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
                Text("Hazır").foregroundStyle(.secondary)
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
