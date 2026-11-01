import AVKit
import EngineKit
import SwiftUI

/// Video preview with the active subtitle line overlaid.
struct PlayerView: View {
    let model: PlayerModel
    let sentences: [Sentence]
    let translations: [UUID: TranslationEntry]
    /// Label of the translation segment, e.g. "Çeviri (es)".
    let translationTitle: String
    @Binding var display: SubtitleDisplay
    /// Window layout switches that live in the overlay next to the display picker.
    @Binding var sideBySide: Bool
    @Binding var theaterMode: Bool
    let isFullScreen: Bool
    let toggleFullScreen: () -> Void
    private let overlay = OverlayStyleStorage()

    var body: some View {
        GeometryReader { geometry in
            let rect = videoSize(in: geometry.size)
            ZStack {
                Color.black
                NativePlayerView(player: model.player)
                SubtitleOverlay(text: activeText, style: overlay.style, videoHeight: rect.height)
                    .frame(width: rect.width, height: rect.height)
                    .allowsHitTesting(false)
                VStack {
                    controls
                        .frame(maxWidth: .infinity, alignment: .trailing)
                    Spacer()
                }
                .padding(10)
            }
        }
    }

    /// The overlay panel: what the video shows, then the window layout switches. In a narrow player the caption goes
    /// first and then the segmented picker becomes a menu.
    private var controls: some View {
        ViewThatFits(in: .horizontal) {
            panel(captioned: true, compact: false)
            panel(captioned: false, compact: false)
            panel(captioned: false, compact: true)
        }
    }

    private func panel(captioned: Bool, compact: Bool) -> some View {
        HStack(spacing: 8) {
            displayPicker(captioned: captioned, compact: compact)
            Divider().frame(height: 16).overlay(.white.opacity(0.35))
            layoutButton(
                symbol: "rectangle.righthalf.inset.filled", isOn: sideBySide,
                title: L("Altyazıları sağa / alta al"), help: L("Altyazıları sağa / alta al")
            ) { sideBySide.toggle() }
            layoutButton(
                symbol: "rectangle.center.inset.filled", isOn: theaterMode,
                title: L("Tiyatro modu"), help: L("Tiyatro modu — üst menüleri gizle")
            ) { theaterMode.toggle() }
            layoutButton(
                symbol: isFullScreen ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right",
                isOn: isFullScreen, title: L("Tam ekran"), help: L("Tam ekran (⌃⌘F)")
            ) { toggleFullScreen() }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(.black.opacity(0.45), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .fixedSize()
    }

    private func displayPicker(captioned: Bool, compact: Bool) -> some View {
        HStack(spacing: 8) {
            if captioned {
                Text(L("Gösterilen"))
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.85))
            }
            if compact {
                displayChoices.pickerStyle(.menu).labelsHidden().fixedSize()
            } else {
                displayChoices.pickerStyle(.segmented).labelsHidden().frame(width: 250)
            }
        }
        .help(translations.isEmpty
            ? L("Önce altyazıları çevir; sonra orijinali, çeviriyi ya da ikisini birden gösterebilirsin.")
            : L("Video üstünde gösterilecek metin"))
    }

    private var displayChoices: some View {
        Picker(L("Gösterilen"), selection: $display) {
            ForEach(SubtitleDisplay.allCases) { mode in
                Text(mode == .translation ? translationTitle : mode.title).tag(mode)
                    .disabled(mode != .original && translations.isEmpty)
            }
        }
    }

    private func layoutButton(
        symbol: String, isOn: Bool, title: String, help: String, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .frame(width: 24, height: 22)
                .foregroundStyle(isOn ? Color.accentColor : .white)
                .background(isOn ? Color.white.opacity(0.18) : .clear, in: RoundedRectangle(cornerRadius: 5, style: .continuous))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(title)
        .accessibilityAddTraits(isOn ? .isSelected : [])
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
    let style: OverlayStyle
    let videoHeight: CGFloat

    var body: some View {
        VStack {
            Spacer(minLength: 0)
            if let text {
                SubtitleOverlayLabel(text: text, style: style, videoHeight: videoHeight)
                    .padding(.horizontal, 16)
            }
        }
        // Stay above the player's inline controls.
        .padding(.bottom, max(44, videoHeight * 0.1))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
