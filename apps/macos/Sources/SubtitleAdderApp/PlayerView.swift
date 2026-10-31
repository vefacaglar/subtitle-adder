import AVKit
import EngineKit
import SwiftUI

/// Video önizlemesi ve üzerine bindirilmiş aktif altyazı satırı.
struct PlayerView: View {
    let model: PlayerModel
    let sentences: [Sentence]

    var body: some View {
        GeometryReader { geometry in
            let rect = videoSize(in: geometry.size)
            ZStack {
                Color.black
                VideoPlayer(player: model.player)
                SubtitleOverlay(text: activeText, videoHeight: rect.height)
                    .frame(width: rect.width, height: rect.height)
                    .allowsHitTesting(false)
            }
        }
    }

    private var activeText: String? {
        guard let id = model.activeID else { return nil }
        let text = sentences.first { $0.id == id }?.text ?? ""
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
