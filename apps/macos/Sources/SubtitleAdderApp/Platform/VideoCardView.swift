import AppKit
import SwiftUI

/// One video of the feed: a picture with its pills on the left, the title, folder, state and actions on the right.
/// Equatable on the card, so a refresh that changes other cards leaves this one alone.
struct VideoCardView: View, Equatable {
    let card: VideoCard
    let model: VideoPlatformModel
    let play: (URL) -> Void
    @State private var isHovering = false

    nonisolated static func == (lhs: VideoCardView, rhs: VideoCardView) -> Bool {
        lhs.card == rhs.card
    }

    private static let thumbnailSize = CGSize(width: 192, height: 108)

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Button { play(card.url) } label: {
                CardThumbnail(card: card, service: model.thumbnails, size: Self.thumbnailSize)
            }
            .buttonStyle(.plain)
            .help(L("Oynat"))
            .accessibilityLabel(L("Oynat"))
            details
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(isHovering ? Color.primary.opacity(0.09) : Color.primary.opacity(0.05)))
        .onHover { isHovering = $0 }
        .contextMenu {
            Button(L("Oynat"), systemImage: "play.fill") { play(card.url) }
            Button(L("Finder'da göster"), systemImage: "folder") {
                NSWorkspace.shared.activateFileViewerSelecting([card.url])
            }
        }
        .accessibilityElement(children: .contain)
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button { play(card.url) } label: {
                Text(card.title)
                    .font(.headline)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            .buttonStyle(.plain)
            .help(card.fileName)
            if !card.folderLine.isEmpty {
                Label(card.folderLine, systemImage: "folder")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(card.directoryPath)
            }
            HStack(spacing: 6) {
                ForEach(card.watchBadges, id: \.self) { text in
                    Text(text)
                    if text != card.watchBadges.last { Text("·") }
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            FlowLayout(spacing: 6) { chips }
            Spacer(minLength: 0)
            actions
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(minHeight: Self.thumbnailSize.height)
    }

    @ViewBuilder private var chips: some View {
        Chip(
            text: card.subtitleChipText, symbol: card.hasSubtitle ? "captions.bubble.fill" : "captions.bubble",
            tint: card.hasSubtitle ? .green : nil
        )
        .help(card.hasSubtitle ? L("Altyazı / çeviri dosyası mevcut") : L("Altyazı yok"))
        if card.isLiked {
            Chip(text: L("Favori"), symbol: "heart.fill", tint: .pink)
        }
        if card.rating > 0 {
            Chip(text: "\(card.rating)/5", symbol: "star.fill", tint: .yellow)
                .accessibilityLabel(L("%lld yıldız", card.rating))
        }
        ForEach(card.lists, id: \.id) { list in
            Chip(text: list.name, symbol: "list.bullet", tint: nil)
        }
    }

    private var actions: some View {
        HStack(spacing: 10) {
            Button { play(card.url) } label: {
                Label(L("Oynat"), systemImage: "play.fill")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            if model.isLoggedIn {
                Button { model.setLiked(!card.isLiked, for: card.url) } label: {
                    Label(
                        card.isLiked ? L("Favorilerde") : L("Favorilere ekle"),
                        systemImage: card.isLiked ? "heart.fill" : "heart")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(card.isLiked ? .pink : nil)
                StarRating(rating: card.rating) { model.setRating($0, for: card.url) }
                AddToListMenu(model: model, url: card.url)
                    .menuStyle(.borderlessButton)
                    .controlSize(.small)
            }
        }
    }
}

/// A small rounded label under the title.
private struct Chip: View {
    let text: String
    let symbol: String
    let tint: Color?

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: symbol).foregroundStyle(tint ?? .secondary)
            Text(text).lineLimit(1)
        }
        .font(.caption)
        .foregroundStyle(tint == nil ? Color.secondary : Color.primary)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background((tint ?? .secondary).opacity(0.16), in: Capsule())
    }
}

/// The 192 x 108 picture of a card with its pills: subtitles and views on top, length at the bottom right and the
/// red resume bar along the bottom.
private struct CardThumbnail: View {
    let card: VideoCard
    let service: ThumbnailService
    let size: CGSize
    @State private var image: NSImage?

    var body: some View {
        ZStack {
            Rectangle().fill(.quaternary)
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                Image(systemName: card.isAudio ? "waveform" : "film")
                    .font(.system(size: 30, weight: .light))
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: size.width, height: size.height)
        .overlay(alignment: .topLeading) { subtitlePill.padding(6) }
        .overlay(alignment: .topTrailing) { viewsPill.padding(6) }
        .overlay(alignment: .bottomTrailing) {
            if !card.durationText.isEmpty {
                Pill(text: card.durationText, symbol: nil, dimmed: false).padding(6)
            }
        }
        .overlay(alignment: .bottomLeading) {
            if card.hasProgress {
                Rectangle()
                    .fill(.red)
                    .frame(width: size.width * card.progress, height: 3)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .task(id: card.path) {
            if let cached = CardImageCache.image(for: card.path) {
                image = cached
                return
            }
            guard let data = await service.data(for: card.url), !Task.isCancelled, let decoded = NSImage(data: data) else {
                return
            }
            CardImageCache.store(decoded, for: card.path)
            image = decoded
        }
    }

    @ViewBuilder private var subtitlePill: some View {
        if let ext = card.subtitleExtension {
            Pill(text: ext.uppercased(), symbol: "captions.bubble.fill", dimmed: false)
        } else {
            Pill(text: L("Altyazı yok"), symbol: nil, dimmed: true)
        }
    }

    private var viewsPill: some View {
        Pill(text: "\(card.viewCount)", symbol: "eye.fill", dimmed: false)
            .help(L("%lld görüntüleme", card.viewCount))
    }
}

private struct Pill: View {
    let text: String
    let symbol: String?
    let dimmed: Bool

    var body: some View {
        HStack(spacing: 3) {
            if let symbol { Image(systemName: symbol) }
            Text(text).lineLimit(1)
        }
        .font(.system(size: 10, weight: .semibold))
        .foregroundStyle(.white)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background((dimmed ? Color.gray : Color.black).opacity(dimmed ? 0.7 : 0.62), in: Capsule())
    }
}

/// Lays its children out left to right and wraps them onto new lines.
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrange(in: proposal.width ?? .infinity, subviews: subviews).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let arrangement = arrange(in: bounds.width, subviews: subviews)
        for (subview, origin) in zip(subviews, arrangement.origins) {
            subview.place(at: CGPoint(x: bounds.minX + origin.x, y: bounds.minY + origin.y), proposal: .unspecified)
        }
    }

    private func arrange(in width: CGFloat, subviews: Subviews) -> (origins: [CGPoint], size: CGSize) {
        var origins: [CGPoint] = []
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0, maxX: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            origins.append(CGPoint(x: x, y: y))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
            maxX = max(maxX, x - spacing)
        }
        return (origins, CGSize(width: maxX, height: y + rowHeight))
    }
}
