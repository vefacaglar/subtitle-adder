import SubtitleCore
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @State private var vm = SubtitleViewModel()
    @State private var pickingVideo = false
    @State private var exportFormat: SubtitleFormat = .srt
    @State private var exporting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            controls
            Divider()
            if vm.sentences.isEmpty {
                dropZone
            } else {
                sentenceList
            }
            statusBar
        }
        .padding()
        .fileImporter(isPresented: $pickingVideo, allowedContentTypes: [.movie, .audiovisualContent]) { result in
            if case .success(let url) = result { vm.video = url }
        }
        .fileExporter(
            isPresented: $exporting,
            document: SubtitleDocument(sentences: vm.sentences, format: exportFormat),
            contentType: .plainText,
            defaultFilename: (vm.video?.deletingPathExtension().lastPathComponent ?? "subtitles") + "." + exportFormat.rawValue
        ) { _ in }
    }

    private var controls: some View {
        HStack {
            Button("Video seç…") { pickingVideo = true }
            Text(vm.video?.lastPathComponent ?? "Video seçilmedi")
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer()
            TextField("Dil (boş = otomatik)", text: $vm.language)
                .frame(width: 140)
            Picker("Model", selection: $vm.model) {
                ForEach(SubtitleViewModel.models, id: \.self) { Text($0) }
            }
            .frame(width: 200)
            Button("Altyazı üret") { Task { await vm.generate() } }
                .keyboardShortcut(.defaultAction)
                .disabled(vm.video == nil || vm.isWorking)
        }
    }

    private var dropZone: some View {
        RoundedRectangle(cornerRadius: 12)
            .strokeBorder(style: StrokeStyle(lineWidth: 2, dash: [8]))
            .foregroundStyle(.tertiary)
            .overlay(Text("Videoyu buraya sürükle").foregroundStyle(.secondary))
            .dropDestination(for: URL.self) { urls, _ in
                vm.video = urls.first
                return urls.first != nil
            }
    }

    private var sentenceList: some View {
        Table(vm.sentences) {
            TableColumn("#") { Text("\($0.id)").monospacedDigit() }.width(36)
            TableColumn("Başlangıç") { Text(SubtitleTime.short($0.start)).monospacedDigit() }.width(80)
            TableColumn("Bitiş") { Text(SubtitleTime.short($0.end)).monospacedDigit() }.width(80)
            TableColumn("Cümle", value: \.text)
        }
    }

    private var statusBar: some View {
        HStack {
            switch vm.status {
            case .idle: Text("Hazır")
            case .working(let label): ProgressView().controlSize(.small); Text(label)
            case .finished: Text("\(vm.sentences.count) satır · dil: \(vm.detectedLanguage ?? "?")")
            case .failed(let message): Text(message).foregroundStyle(.red)
            }
            Spacer()
            Picker("Biçim", selection: $exportFormat) {
                ForEach(SubtitleFormat.allCases) { Text($0.rawValue.uppercased()).tag($0) }
            }
            .frame(width: 140)
            Button("Dışa aktar…") { exporting = true }
                .disabled(vm.sentences.isEmpty)
        }
        .font(.callout)
    }
}

enum SubtitleTime {
    static func short(_ seconds: Double) -> String {
        String(format: "%02d:%05.2f", Int(seconds) / 60, seconds.truncatingRemainder(dividingBy: 60))
    }
}

struct SubtitleDocument: FileDocument {
    static let readableContentTypes: [UTType] = [.plainText]
    var sentences: [Sentence]
    var format: SubtitleFormat

    init(sentences: [Sentence], format: SubtitleFormat) {
        self.sentences = sentences
        self.format = format
    }

    init(configuration: ReadConfiguration) throws {
        throw CocoaError(.featureUnsupported)
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: Data(try SubtitleWriter.render(sentences, as: format).utf8))
    }
}
