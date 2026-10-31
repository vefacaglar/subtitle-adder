import EngineKit
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @State private var vm = SubtitleViewModel()
    @State private var pickingVideo = false
    @State private var exportFormat: SubtitleFormat = .srt
    @State private var exportDocument: TextDocument?

    var body: some View {
        VStack(spacing: 0) {
            if vm.sentences.isEmpty {
                dropZone.padding(20)
            } else {
                sentenceList
            }
            Divider()
            statusBar.padding(.horizontal, 16).padding(.vertical, 10)
        }
        .toolbar { toolbar }
        .navigationTitle(vm.video?.lastPathComponent ?? "Subtitle Adder")
        .fileImporter(isPresented: $pickingVideo, allowedContentTypes: [.movie, .audiovisualContent]) { result in
            if case .success(let url) = result { vm.video = url }
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
            Image(systemName: vm.video == nil ? "film.stack" : "checkmark.circle")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(vm.video == nil ? Color.secondary : Color.accentColor)
            Text(vm.video?.lastPathComponent ?? "Videoyu buraya sürükle")
                .font(.title3)
            Text(vm.video == nil ? "ya da araç çubuğundan seç" : "Altyazı üretmek için ⌘↩")
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 16))
        .overlay(
            RoundedRectangle(cornerRadius: 16)
                .strokeBorder(style: StrokeStyle(lineWidth: 1.5, dash: [7]))
                .foregroundStyle(.tertiary)
        )
        .dropDestination(for: URL.self) { urls, _ in
            vm.video = urls.first
            return urls.first != nil
        }
    }

    private var sentenceList: some View {
        Table(vm.sentences) {
            TableColumn("#") { Text("\($0.id)").monospacedDigit().foregroundStyle(.secondary) }.width(32)
            TableColumn("Zaman") {
                Text("\(SubtitleTime.short($0.start)) → \(SubtitleTime.short($0.end))")
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
            .width(130)
            TableColumn("Cümle", value: \.text)
        }
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
