import AVKit
import EngineKit
import SwiftUI

/// Video önizlemesi ve üzerine bindirilmiş aktif altyazı satırı.
struct PlayerView: View {
    let model: PlayerModel
    let sentences: [Sentence]
    let translations: [UUID: TranslationEntry]
    @Binding var display: SubtitleDisplay

    var body: some View {
        GeometryReader { geometry in
            let rect = videoSize(in: geometry.size)
            ZStack {
                Color.black
                NativePlayerView(player: model.player)
                SubtitleOverlay(text: activeText, videoHeight: rect.height)
                    .frame(width: rect.width, height: rect.height)
                    .allowsHitTesting(false)
                VStack {
                    HStack {
                        Spacer()
                        displayPicker
                    }
                    Spacer()
                }
                .padding(10)
            }
        }
    }

    private var displayPicker: some View {
        HStack(spacing: 8) {
            Text("Gösterilen")
                .font(.caption)
                .foregroundStyle(.white.opacity(0.85))
            Picker("Gösterilen", selection: $display) {
                ForEach(SubtitleDisplay.allCases) { mode in
                    Text(mode.title).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 196)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(.black.opacity(0.45), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .help(translations.isEmpty
            ? "Önce altyazıları çevir; sonra orijinali, çeviriyi ya da ikisini birden gösterebilirsin."
            : "Video üstünde gösterilecek metin")
        .disabled(translations.isEmpty)
    }

    private var activeText: String? {
        guard let id = model.activeID,
              let sentence = sentences.first(where: { $0.uid == id })
        else { return nil }
        let original = sentence.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let translated = translations[id]?.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let text: String
        switch display {
        case .original:
            text = original
        case .translation:
            text = translated?.isEmpty == false ? translated ?? "" : original
        case .both:
            text = [original, translated]
                .compactMap { $0 }
                .filter { !$0.isEmpty }
                .joined(separator: "\n")
        }
        return text.isEmpty ? nil : text
    }

    /// Videonun kapsayıcı içinde (aspect-fit) kapladığı boyut.
    private func videoSize(in container: CGSize) -> CGSize {
        let aspect = model.videoSize.width / max(model.videoSize.height, 1)
        guard container.width > 0, container.height > 0 else { return .zero }
        var width = container.width
        var height = width / aspect
        if height > container.height {
            height = container.height
            width = height * aspect
        }
        return CGSize(width: width, height: height)
    }
}

/// AVKit'in kendi oynatıcısı. SwiftUI `VideoPlayer` AVKit'i bağlatmadığı için SwiftPM ikilisinde
/// `AVPlayerView` sınıfı bulunamayıp ilk çizimde çöküyordu; sınıfa doğrudan başvurmak bağlanmasını sağlar.
private struct NativePlayerView: NSViewRepresentable {
    let player: AVPlayer

    func makeNSView(context: Context) -> AVPlayerView {
        let view = AVPlayerView()
        view.player = player
        view.controlsStyle = .inline
        view.videoGravity = .resizeAspect
        view.showsFullScreenToggleButton = true
        return view
    }

    func updateNSView(_ view: AVPlayerView, context: Context) {
        if view.player !== player { view.player = player }
    }
}

private struct SubtitleOverlay: View {
    let text: String?
    let videoHeight: CGFloat

    var body: some View {
        VStack {
            Spacer(minLength: 0)
            if let text {
                Text(text)
                    .font(.system(size: max(12, videoHeight * 0.05), weight: .semibold))
                    .foregroundStyle(.white)
                    .multilineTextAlignment(.center)
                    .lineLimit(3)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .padding(.horizontal, 16)
            }
        }
        // Oynatıcının satır içi denetimlerinin üstünde kalsın.
        .padding(.bottom, max(44, videoHeight * 0.1))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
