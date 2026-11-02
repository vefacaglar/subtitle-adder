import SwiftUI

/// Five stars. Clicking the star that is already the rating clears it.
struct StarRating: View {
    let rating: Int
    var dimmed: Color = .secondary
    let select: (Int) -> Void

    var body: some View {
        HStack(spacing: 1) {
            ForEach(1...5, id: \.self) { star in
                Button {
                    select(star == rating ? 0 : star)
                } label: {
                    Image(systemName: star <= rating ? "star.fill" : "star")
                        .foregroundStyle(star <= rating ? Color.yellow : dimmed)
                        .frame(width: 18, height: 20)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(L("%lld yıldız", star))
                .accessibilityLabel(L("%lld yıldız", star))
                .accessibilityAddTraits(star == rating ? .isSelected : [])
            }
        }
        .accessibilityElement(children: .contain)
    }
}

/// "+ Listeye ekle": a checkmark per list (with how many videos it holds), and the shortcut to make a new list with
/// the video in it. Used on the cards and on the player.
struct AddToListMenu: View {
    let model: VideoPlatformModel
    let url: URL
    /// Icon only, for a narrow player.
    var iconOnly = false

    var body: some View {
        Menu {
            ForEach(model.lists) { list in
                Toggle(
                    "\(list.name) (\(list.videoPaths.count))",
                    isOn: Binding(
                        get: { model.isInList(list.id, url) },
                        set: { _ in model.toggleList(list.id, for: url) }))
            }
            if !model.lists.isEmpty { Divider() }
            Button(
                model.lists.isEmpty ? L("+ \"Daha Sonra İzle\" listesi oluştur ve ekle") : L("+ Yeni liste oluştur ve ekle"),
                systemImage: "plus"
            ) { model.createListAndAdd(url) }
        } label: {
            if iconOnly {
                Image(systemName: "text.badge.plus")
            } else {
                Text(L("+ Listeye ekle"))
            }
        }
        .menuIndicator(.hidden)
        .fixedSize()
        .help(L("+ Listeye ekle"))
        .accessibilityLabel(L("+ Listeye ekle"))
    }
}

/// The bar at the top left of the player while a profile is logged in: views, favorite, stars and lists. It fades
/// together with the player's overlay panel.
struct EngagementBar: View {
    let model: VideoPlatformModel

    var body: some View {
        let engagement = model.engagement
        if model.isLoggedIn, engagement.isLoaded, let url = engagement.url {
            ViewThatFits(in: .horizontal) {
                bar(url, compact: false)
                bar(url, compact: true)
            }
        }
    }

    private func bar(_ url: URL, compact: Bool) -> some View {
        let engagement = model.engagement
        return HStack(spacing: 8) {
            Label(compact ? "\(engagement.views)" : L("%lld görüntüleme", engagement.views), systemImage: "eye")
                .font(.caption)
                .help(L("%lld görüntüleme", engagement.views))
            Divider().frame(height: 16).overlay(.white.opacity(0.35))
            Button {
                model.toggleLike(url)
            } label: {
                if compact {
                    Image(systemName: engagement.isLiked ? "heart.fill" : "heart")
                } else {
                    Label(engagement.isLiked ? L("Beğenildi") : L("Beğen"), systemImage: engagement.isLiked ? "heart.fill" : "heart")
                }
            }
            .buttonStyle(.plain)
            .foregroundStyle(engagement.isLiked ? Color.pink : .white)
            .help(engagement.isLiked ? L("Beğenildi") : L("Beğen"))
            .accessibilityLabel(engagement.isLiked ? L("Beğenildi") : L("Beğen"))
            StarRating(rating: engagement.rating, dimmed: .white.opacity(0.55)) { model.setRating($0, for: url) }
            Divider().frame(height: 16).overlay(.white.opacity(0.35))
            AddToListMenu(model: model, url: url, iconOnly: compact)
                .menuStyle(.borderlessButton)
                .foregroundStyle(.white)
        }
        .font(.caption)
        .foregroundStyle(.white)
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(.black.opacity(0.45), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .fixedSize()
    }
}
