import AVKit
import EngineKit
import SwiftUI

/// Video preview with the active subtitle line overlaid.
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
        guard let id = model.activeID else { return nil }
        return OverlayText.compose(sentences: sentences, translations: translations, display: display, uid: id)
    }

    /// The size the video takes inside its container (aspect-fit).
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

/// AVKit's own player. SwiftUI's `VideoPlayer` doesn't get AVKit linked, so in the SwiftPM binary
/// the `AVPlayerView` class wasn't found and it crashed on first draw; referencing the class directly makes it link.
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
        // Stay above the player's inline controls.
        .padding(.bottom, max(44, videoHeight * 0.1))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
