import LibraryKit
import SwiftUI

/// The "Platform" tab: search, filter pills, the user's lists, and the videos as cards. Logged out, a personal tab
/// shows the sign-in card instead.
struct PlatformView: View {
    let model: VideoPlatformModel
    /// Opens a video in the player and starts it.
    let play: (URL) -> Void
    /// False while a playlist run owns the player: the cards stay, but their play buttons are off.
    var canPlay = true

    @State private var newListName = ""
    @State private var confirmingClearHistory = false
    @State private var listToDelete: UserVideoList?
    @State private var listToRename: UserVideoList?
    @FocusState private var searchFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                header
                filterBar
                if model.isLoggedIn { listsBar }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            Divider()
            content
        }
        .onAppear { model.isHubActive = true }
        .onDisappear { model.isHubActive = false }
        .confirmationDialog(
            L("İzleme geçmişi temizlensin mi?"), isPresented: $confirmingClearHistory, titleVisibility: .visible
        ) {
            Button(L("Geçmişi temizle"), role: .destructive) { model.clearHistory() }
            Button(L("Vazgeç"), role: .cancel) {}
        } message: {
            Text(L("Favoriler, puanlar ve listeler korunur; görüntülenme sayıları ve kaldığın yerler silinir."))
        }
        .confirmationDialog(
            listToDelete.map { L("“%@” listesi silinsin mi?", $0.name) } ?? "",
            isPresented: Binding(get: { listToDelete != nil }, set: { if !$0 { listToDelete = nil } }),
            titleVisibility: .visible, presenting: listToDelete
        ) { list in
            Button(L("Sil"), role: .destructive) { model.deleteList(id: list.id) }
            Button(L("Vazgeç"), role: .cancel) {}
        } message: { _ in
            Text(L("Listedeki videolar silinmez; yalnızca liste kaldırılır."))
        }
    }

    // MARK: Header

    private var header: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) {
                searchBar
                profileActions
            }
            VStack(alignment: .leading, spacing: 8) {
                searchBar
                profileActions
            }
        }
    }

    private var searchBar: some View {
        @Bindable var model = model
        return HStack(spacing: 8) {
            TextField(searchPlaceholder, text: $model.searchText)
                .textFieldStyle(.roundedBorder)
                .focused($searchFocused)
                .onSubmit { model.submitSearch() }
                .overlay(alignment: .trailing) {
                    if !model.searchText.isEmpty {
                        Button(L("Aramayı temizle"), systemImage: "xmark.circle.fill") { model.clearSearch() }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.borderless)
                            .foregroundStyle(.secondary)
                            .padding(.trailing, 6)
                            .help(L("Aramayı temizle"))
                    }
                }
                .frame(minWidth: 200)
            Button(L("Ara"), systemImage: "magnifyingglass") { model.submitSearch() }
                .labelStyle(.titleAndIcon)
                .buttonStyle(.borderedProminent)
        }
    }

    /// Searching on "Keşfet" (and while logged out) looks through every indexed video; on the personal tabs the search
    /// narrows the tab, and the placeholder says so.
    private var searchPlaceholder: String {
        model.filter == .all || !model.isLoggedIn
            ? L("İndekslenen tüm videolarda ara (yazıp Enter'a basın)…")
            : L("Bu listede ara (yazıp Enter'a basın)…")
    }

    @ViewBuilder private var profileActions: some View {
        HStack(spacing: 10) {
            if let name = model.username {
                Text("\(name) · \(model.summaryText)")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                if model.canClearHistory {
                    Button(L("Geçmişi temizle")) { confirmingClearHistory = true }
                }
                Button(L("Çıkış yap")) { model.logout() }
            } else {
                Button(L("Giriş yap / Profil oluştur")) { model.requestSignIn() }
            }
        }
    }

    // MARK: Filters

    private var filterBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                pill(L("Keşfet"), filter: .all)
                pill(counted(L("İzleme geçmişi"), L("İzleme geçmişi (%lld)", model.counts.watched)), filter: .history)
                pill(counted(L("Favorilerim"), L("Favorilerim (%lld)", model.counts.liked)), filter: .liked)
                pill(counted(L("Puan verdiklerim"), L("Puan verdiklerim (%lld)", model.counts.rated)), filter: .topRated)
                pill(L("En çok izlenenler"), filter: .mostWatched)
            }
            .padding(.vertical, 1)
        }
    }

    /// Counts show only while logged in.
    private func counted(_ plain: String, _ withCount: String) -> String {
        model.isLoggedIn ? withCount : plain
    }

    private func pill(_ title: String, filter: VideoFeedFilter) -> some View {
        Button(title) { model.select(filter) }
            .buttonStyle(PillButtonStyle(isSelected: model.filter == filter))
            .accessibilityAddTraits(model.filter == filter ? .isSelected : [])
    }

    // MARK: Lists

    private var listsBar: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(L("Listelerim:"))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                if model.lists.isEmpty {
                    Text(L("Henüz özel liste oluşturulmadı"))
                        .font(.callout)
                        .foregroundStyle(.tertiary)
                    Spacer(minLength: 0)
                } else {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(model.lists) { listPill($0) }
                        }
                        .padding(.vertical, 1)
                    }
                }
                TextField(L("Yeni liste adı…"), text: $newListName)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)
                    .frame(width: 170)
                    .onSubmit(createList)
                    .onChange(of: newListName) { model.clearListFailure() }
                Button(L("+ Liste oluştur"), action: createList)
                    .controlSize(.small)
            }
            if let error = model.listError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
    }

    private func listPill(_ list: UserVideoList) -> some View {
        let selected = model.filter == .customList(list.id)
        return HStack(spacing: 2) {
            Button("\(list.name) (\(list.videoPaths.count))") { model.select(.customList(list.id)) }
                .buttonStyle(PillButtonStyle(isSelected: selected))
                .accessibilityAddTraits(selected ? .isSelected : [])
            if selected {
                Button(L("Listeyi sil"), systemImage: "xmark.circle.fill") { listToDelete = list }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
                    .help(L("Listeyi sil"))
            }
        }
        .contextMenu {
            Button(L("Yeniden adlandır"), systemImage: "pencil") {
                model.clearListFailure()
                listToRename = list
            }
            Button(L("Listeyi sil"), systemImage: "trash", role: .destructive) { listToDelete = list }
        }
        .popover(
            isPresented: Binding(get: { listToRename?.id == list.id }, set: { if !$0 { listToRename = nil } }),
            arrowEdge: .bottom
        ) {
            RenameListForm(model: model, list: list) { listToRename = nil }
        }
    }

    private func createList() {
        let name = newListName
        Task {
            if await model.createList(named: name) { newListName = "" }
        }
    }

    // MARK: Content

    @ViewBuilder private var content: some View {
        if model.showsAuthCard {
            ScrollView {
                ProfileAuthCard(model: model)
                    .padding(32)
                    .frame(maxWidth: .infinity)
            }
        } else if model.cards.isEmpty {
            if model.isRefreshing || !model.hasLoadedFeed {
                ProgressView()
                    .controlSize(.large)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                emptyState
            }
        } else {
            ScrollView {
                LazyVStack(spacing: 10) {
                    ForEach(model.cards) { card in
                        VideoCardView(card: card, model: model, canPlay: canPlay, play: play).equatable()
                    }
                }
                .padding(16)
            }
        }
    }

    private var emptyState: some View {
        let state = EmptyFeedText(
            filter: model.filter, search: model.searchQuery, listName: model.selectedList?.name)
        return ContentUnavailableView {
            Label(state.title, systemImage: state.symbol)
        } description: {
            Text(state.hint)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// What an empty feed says: the Windows wording per filter, and for a search that found nothing.
struct EmptyFeedText: Equatable {
    let title: String
    let hint: String
    let symbol: String

    init(filter: VideoFeedFilter, search: String, listName: String?) {
        if !search.isEmpty {
            title = L("\"%@\" için eşleşen video bulunamadı", search)
            hint = L("Farklı bir anahtar kelime deneyin veya klasör gezgininden yeni bir video klasörü açın.")
            symbol = "magnifyingglass"
            return
        }
        let general = L("Bir video açıp izlediğinizde, favorilere eklediğinizde veya puanladığınızda burada YouTube tarzı kartlar halinde listelenir.")
        switch filter {
        case .history:
            (title, hint, symbol) = (L("Henüz izleme geçmişi yok"), general, "clock")
        case .liked:
            (title, hint, symbol) = (L("Henüz favorilere eklenen video yok"), general, "heart")
        case .mostWatched:
            (title, hint, symbol) = (L("Henüz izlenen video yok"), general, "eye")
        case .topRated:
            (title, hint, symbol) = (L("Henüz puanlanan video yok"), general, "star")
        case .customList:
            (title, hint, symbol) = (
                L("\"%@\" listesinde henüz video yok", listName ?? L("Liste")),
                L("Herhangi bir video kartındaki veya oynatıcıdaki \"+ Listeye ekle\" düğmesiyle bu listeye video ekleyebilirsiniz."),
                "list.bullet"
            )
        case .all:
            (title, hint, symbol) = (L("Gösterilecek video bulunamadı"), general, "film.stack")
        }
    }
}

/// The small form of the rename popover; a blank or taken name stays open with the reason.
private struct RenameListForm: View {
    let model: VideoPlatformModel
    let list: UserVideoList
    let done: () -> Void
    @State private var name: String

    init(model: VideoPlatformModel, list: UserVideoList, done: @escaping () -> Void) {
        self.model = model
        self.list = list
        self.done = done
        _name = State(initialValue: list.name)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(L("Listeyi yeniden adlandır")).font(.headline)
            TextField(L("Yeni liste adı…"), text: $name)
                .textFieldStyle(.roundedBorder)
                .frame(width: 240)
                .onSubmit(rename)
                .onChange(of: name) { model.clearListFailure() }
            if let error = model.listError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .frame(width: 240, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                Button(L("Vazgeç"), action: done)
                    .keyboardShortcut(.cancelAction)
                Button(L("Yeniden adlandır"), action: rename)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(14)
        .onDisappear { model.clearListFailure() }
    }

    private func rename() {
        let newName = name
        Task {
            if await model.renameList(id: list.id, to: newName) { done() }
        }
    }
}
