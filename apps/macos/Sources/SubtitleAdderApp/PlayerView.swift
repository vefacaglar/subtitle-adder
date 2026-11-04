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
    /// The user's choice to hide the subtitle panel (full screen hides it regardless).
    @Binding var hideSubtitlePanel: Bool
    let isFullScreen: Bool
    let toggleFullScreen: () -> Void
    /// The local video platform; its engagement bar (views, favorite, stars, lists) sits at the top left while a
    /// profile is logged in.
    var platform: VideoPlatformModel?
    private let overlay = OverlayStyleStorage()
    /// The pointer is over the player; the overlay panel stays visible then, and always while paused.
    @State private var isPointerInside = false

    private var isPanelVisible: Bool { !model.isPlaying || isPointerInside }

    var body: some View {
        GeometryReader { geometry in
            let rect = videoSize(in: geometry.size)
            ZStack {
                Color.black
                NativePlayerView(player: model.player)
                // Between AVKit's own controls (which stay reachable at the top and bottom) a click on the picture
                // toggles playback, a double click full screen, and the scroll wheel changes the volume.
                VideoPointerSurface(onClick: handleClick, onScroll: { model.adjustVolume(by: Double($0) * PlayerShortcut.volumeStep) })
                SubtitleOverlay(text: activeText, style: overlay.style, videoHeight: rect.height)
                    .frame(width: rect.width, height: rect.height)
                    .allowsHitTesting(false)
                if model.isPreparingPreview {
                    ProgressView { Text(L("Video önizlemesi hazırlanıyor…")) }
                        .tint(.white)
                        .foregroundStyle(.white)
                        .allowsHitTesting(false)
                }
                VStack {
                    overlayPanels
                        .opacity(isPanelVisible ? 1 : 0)
                        .allowsHitTesting(isPanelVisible)
                    Spacer()
                }
                .padding(10)
            }
            .animation(.easeInOut(duration: 0.25), value: isPanelVisible)
            .onHover { isPointerInside = $0 }
        }
    }

    private func handleClick(count: Int) {
        for action in VideoClick.actions(forClickCount: count) {
            switch action {
            case .togglePlayback: model.togglePlayback()
            case .toggleFullScreen: toggleFullScreen()
            }
        }
    }

    /// The engagement bar at the top left and the overlay panel at the top right; in a player too narrow for both, the bar
    /// goes on a line of its own below the panel. Both fade together.
    @ViewBuilder private var overlayPanels: some View {
        if let platform, platform.isLoggedIn, platform.engagement.isLoaded {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top, spacing: 12) {
                    EngagementBar(model: platform)
                    Spacer(minLength: 0)
                    controls
                }
                VStack(alignment: .trailing, spacing: 6) {
                    controls
                    EngagementBar(model: platform).frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        } else {
            controls.frame(maxWidth: .infinity, alignment: .trailing)
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
            LayoutButton(
                symbol: hideSubtitlePanel || isFullScreen ? "captions.bubble" : "captions.bubble.fill",
                isOn: !hideSubtitlePanel && !isFullScreen,
                title: L("Altyazı panelini gizle / göster"), help: L("Altyazı panelini gizle / göster (E)")
            ) { hideSubtitlePanel.toggle() }
            .disabled(isFullScreen)
            LayoutButton(
                symbol: "rectangle.righthalf.inset.filled", isOn: sideBySide,
                title: L("Altyazıları sağa / alta al"), help: L("Altyazıları sağa / alta al")
            ) { sideBySide.toggle() }
            .disabled(hideSubtitlePanel || isFullScreen)
            LayoutButton(
                symbol: "rectangle.center.inset.filled", isOn: theaterMode,
                title: L("Tiyatro modu"), help: L("Tiyatro modu — üst menüleri gizle")
            ) { theaterMode.toggle() }
            LayoutButton(
                symbol: isFullScreen ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right",
                isOn: isFullScreen, title: L("Tam ekran"), help: L("Tam ekran (F, ⌃⌘F)")
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

/// A window layout switch in the overlay panel; dims when disabled.
private struct LayoutButton: View {
    let symbol: String
    let isOn: Bool
    let title: String
    let help: String
    let action: () -> Void
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .frame(width: 24, height: 22)
                .foregroundStyle(isOn ? Color.accentColor : .white)
                .background(isOn ? Color.white.opacity(0.18) : .clear, in: RoundedRectangle(cornerRadius: 5, style: .continuous))
                .contentShape(Rectangle())
                .opacity(isEnabled ? 1 : 0.4)
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(title)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }
}

/// A transparent layer over the picture that reports clicks and scroll-wheel notches, and leaves AVKit's own controls
/// alone: AVKit draws its control bar along the bottom and small buttons along the top, so those strips are not part
/// of the surface and their clicks, drags and scrolling go to AVKit as before. A click also moves the keyboard focus
/// here, out of any text field, so the player keys work right after clicking the video.
private struct VideoPointerSurface: NSViewRepresentable {
    /// Called with the click count (1 for a click, 2 for a double click).
    let onClick: (Int) -> Void
    /// Called with signed notches; positive is scrolling up.
    let onScroll: (Int) -> Void

    func makeNSView(context: Context) -> SurfaceView {
        let view = SurfaceView()
        view.onClick = onClick
        view.onScroll = onScroll
        return view
    }

    func updateNSView(_ view: SurfaceView, context: Context) {
        view.onClick = onClick
        view.onScroll = onScroll
    }

    final class SurfaceView: NSView {
        /// Heights of the strips AVKit's controls occupy (its bar is about 36 pt tall, 6 pt above the edge).
        static let bottomInset: CGFloat = 56
        static let topInset: CGFloat = 48

        var onClick: ((Int) -> Void)?
        var onScroll: ((Int) -> Void)?
        private var notches = ScrollNotches()

        override var acceptsFirstResponder: Bool { true }
        override var mouseDownCanMoveWindow: Bool { false }

        override func hitTest(_ point: NSPoint) -> NSView? {
            let local = convert(point, from: superview)
            guard bounds.contains(local), local.y >= Self.bottomInset, local.y <= bounds.height - Self.topInset else {
                return nil
            }
            return self
        }

        override func mouseDown(with event: NSEvent) {
            window?.makeFirstResponder(self)
            onClick?(event.clickCount)
        }

        override func scrollWheel(with event: NSEvent) {
            // The coasting after the fingers lift would keep changing the volume.
            guard event.momentumPhase.isEmpty else { return }
            if event.phase.contains(.began) { notches.reset() }
            let count = notches.add(delta: Double(event.scrollingDeltaY), isPrecise: event.hasPreciseScrollingDeltas)
            if count != 0 { onScroll?(count) }
        }
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
