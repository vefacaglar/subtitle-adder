import EngineKit
import Foundation
import LibraryKit
import Observation

/// State and rules of the toolbar's translation-model popover: which provider's models are listed, the search, the
/// highlighted row, and the cancellable "Yenile". The popover view is thin; everything it decides lives here.
///
/// Browsing a provider in the popover does not change what translates: only choosing one of its models does (it makes
/// that provider active with that model, and a local model makes on-device translation active).
@MainActor
@Observable
final class TranslationModelPickerModel {
    /// One selectable line of the list.
    struct Row: Identifiable, Equatable {
        /// The catalog id for a local model, the model name for a provider's.
        let id: String
        let title: String
        /// The model translation uses right now.
        let isCurrent: Bool
        /// `false` when the provider can't be used (offline mode, unusable address).
        let isEnabled: Bool
    }

    /// An entry of the provider picker; `nil` is the on-device models.
    struct ProviderChoice: Identifiable, Equatable {
        let id: UUID?
        let name: String
    }

    /// The provider whose models are listed; `nil` lists the local models.
    private(set) var browsedProviderID: UUID?
    var searchText = "" {
        didSet {
            guard searchText != oldValue else { return }
            resetHighlight()
        }
    }
    /// The row Return chooses; arrows move it. It starts on the current model.
    var highlightedID: String?
    /// Result of the last refresh (or why the browsed provider went away); replaced by the next one.
    private(set) var status: LocalizedMessage?
    private(set) var isRefreshing = false

    @ObservationIgnored private let vm: SubtitleViewModel
    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    /// Bumped whenever a refresh starts or is abandoned, so an answer that arrives late finds it is no longer wanted.
    @ObservationIgnored private var epoch = 0

    init(vm: SubtitleViewModel) {
        self.vm = vm
        browsedProviderID = vm.activeExternalProvider?.id
        resetHighlight()
    }

    // MARK: - Lifecycle

    /// The popover opened: start from what translation uses now.
    func open() {
        abandonRefresh()
        browsedProviderID = vm.activeExternalProvider?.id
        searchText = ""
        status = nil
        resetHighlight()
    }

    /// The popover closed: an answer still on its way is no longer wanted.
    func close() {
        abandonRefresh()
        status = nil
    }

    // MARK: - Providers

    /// The provider list of the popover's picker: on-device first, then the saved providers in their stored order.
    var providerChoices: [ProviderChoice] {
        [ProviderChoice(id: nil, name: L("Yerel modeller (Cihaz üzerinde)"))]
            + vm.providerStore.providers.map { ProviderChoice(id: $0.id, name: $0.name) }
    }

    /// The browsed provider, `nil` for the local models and for one that was removed meanwhile.
    var browsedProvider: TranslationProviderProfile? {
        browsedProviderID.flatMap { id in vm.providerStore.providers.first { $0.id == id } }
    }

    /// Lists another provider's models (or the local ones). Cancels a refresh under way and clears the search.
    func selectProvider(_ id: UUID?) {
        if let id, !vm.providerStore.providers.contains(where: { $0.id == id }) { return }
        guard id != browsedProviderID else { return }
        abandonRefresh()
        browsedProviderID = id
        searchText = ""
        status = nil
        resetHighlight()
    }

    /// The saved providers changed (an edit, a removal): drop the browsed one when it is gone.
    func providersChanged() {
        if browsedProviderID != nil, browsedProvider == nil {
            abandonRefresh()
            browsedProviderID = nil
            searchText = ""
            status = .key("Bu sağlayıcı artık yok.")
        }
        resetHighlight()
    }

    // MARK: - Rows

    /// A provider's model names: what it reported, else the preset's suggestions, plus the model it is set to use;
    /// trimmed, case-insensitively distinct and sorted.
    static func modelNames(for provider: TranslationProviderProfile) -> [String] {
        // `availableModels` is the store's rule (chosen model, reported ones, suggestions); a provider that reported
        // its models is listed from them alone.
        let source = provider.discoveredModels.isEmpty
            ? provider.availableModels : provider.discoveredModels + [provider.model]
        var seen = Set<String>()
        return source
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty && seen.insert($0.lowercased()).inserted }
            .sorted { $0.caseInsensitiveCompare($1) == .orderedAscending }
    }

    /// Every model of the browsed provider (or the local ones) before the search. Empty for a provider that was removed.
    var allRows: [Row] {
        guard let provider = browsedProvider else {
            if browsedProviderID != nil { return [] }
            let current = vm.activeExternalProvider == nil ? vm.translationModel : nil
            return vm.catalog.translationModels.map {
                Row(id: $0.id, title: ModelLabel.translationMenuTitle(for: $0), isCurrent: $0.id == current, isEnabled: true)
            }
        }
        let isActive = vm.activeExternalProvider?.id == provider.id
        let chosen = provider.model.trimmingCharacters(in: .whitespacesAndNewlines)
        let usable = blockReason(of: provider) == nil
        return Self.modelNames(for: provider).map {
            Row(id: $0, title: $0, isCurrent: isActive && $0.caseInsensitiveCompare(chosen) == .orderedSame, isEnabled: usable)
        }
    }

    /// The rows the search lets through (case-insensitive contains on the title).
    var rows: [Row] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return allRows }
        return allRows.filter { $0.title.range(of: query, options: .caseInsensitive) != nil }
    }

    /// What the list says when it has no rows.
    var emptyText: String? {
        guard rows.isEmpty else { return nil }
        if !allRows.isEmpty { return L("Aramayla eşleşen model yok.") }
        return L("Model listesi boş. Yenile düğmesini kullanın.")
    }

    /// The line under the list: a refresh in progress or its outcome, else why the provider can't be chosen from.
    var statusText: String? {
        if let status { return status.text }
        return browsedProvider.flatMap(blockReason(of:))?.text
    }

    /// Why a provider's models can't be chosen: offline mode keeps the text from a cloud service, and an unusable
    /// address would silently fall back to a local model.
    private func blockReason(of provider: TranslationProviderProfile) -> LocalizedMessage? {
        if ExternalLLMServer.normalizeBaseURL(provider.baseURL) == nil {
            return .key("Lütfen geçerli bir sunucu adresi girin.")
        }
        if vm.offlineBlocksServer(provider.baseURL) {
            return .key("Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz.")
        }
        return nil
    }

    // MARK: - Choosing

    /// Chooses a row: a provider's model makes that provider active with it, a local model makes on-device translation
    /// active. `false` (nothing changed) for a row that is not listed, a provider that is gone or blocked.
    @discardableResult
    func choose(_ rowID: String) -> Bool {
        if let id = browsedProviderID {
            guard let provider = browsedProvider else {
                providersChanged()
                return false
            }
            guard blockReason(of: provider) == nil,
                  let name = Self.modelNames(for: provider).first(where: { $0 == rowID })
            else { return false }
            vm.chooseProviderModel(name, for: id)
            return true
        }
        guard vm.catalog.translationModels.contains(where: { $0.id == rowID }) else { return false }
        vm.chooseLocalTranslationModel(rowID)
        return true
    }

    /// Return in the search field.
    @discardableResult
    func chooseHighlighted() -> Bool {
        guard let id = highlightedID, rows.contains(where: { $0.id == id && $0.isEnabled }) else { return false }
        return choose(id)
    }

    // MARK: - Highlight

    /// Moves the highlight over the enabled rows (clamped at both ends).
    func moveHighlight(by step: Int) {
        let ids = rows.filter(\.isEnabled).map(\.id)
        guard !ids.isEmpty else {
            highlightedID = nil
            return
        }
        guard let current = highlightedID, let index = ids.firstIndex(of: current) else {
            highlightedID = step >= 0 ? ids.first : ids.last
            return
        }
        highlightedID = ids[min(max(index + step, 0), ids.count - 1)]
    }

    /// The highlight starts on the current model, else on the model the browsed provider remembers, else the first row.
    func resetHighlight() {
        let enabled = rows.filter(\.isEnabled)
        if let current = enabled.first(where: \.isCurrent) {
            highlightedID = current.id
        } else if let remembered = browsedProvider?.model.trimmingCharacters(in: .whitespacesAndNewlines),
                  let row = enabled.first(where: { $0.id.caseInsensitiveCompare(remembered) == .orderedSame }) {
            highlightedID = row.id
        } else {
            highlightedID = enabled.first?.id
        }
    }

    // MARK: - Refresh

    /// "Yenile" is for a saved provider that can be asked, and not while a test of it is running.
    var canRefresh: Bool {
        guard let provider = browsedProvider else { return false }
        return !isRefreshing && !vm.testingProviders.contains(provider.id) && blockReason(of: provider) == nil
    }

    /// Asks the browsed provider for its models. The answer is applied only if the popover is still on that provider;
    /// a late one (another provider, a closed popover, an edit or removal) changes nothing. A failure keeps the list.
    func refresh() {
        guard canRefresh, let id = browsedProviderID else { return }
        epoch += 1
        let mine = epoch
        isRefreshing = true
        status = .key("Modeller yenileniyor…")
        // The test writes its own outcome here; whatever was there is about an earlier test.
        vm.providerNotices[id] = nil
        let vm = vm
        refreshTask = Task { [weak self] in
            await vm.testConnection(of: id)
            self?.finishRefresh(of: id, epoch: mine)
        }
    }

    /// Waits for a refresh started by `refresh()` (tests).
    func waitForRefresh() async {
        await refreshTask?.value
    }

    private func finishRefresh(of id: UUID, epoch mine: Int) {
        guard mine == epoch else { return }
        refreshTask = nil
        isRefreshing = false
        guard browsedProviderID == id, browsedProvider != nil, let notice = vm.providerNotices[id] else {
            status = nil
            resetHighlight()
            return
        }
        if notice.isError {
            let reason = notice.message
            status = LocalizedMessage("refresh-failed:\(reason.id)") { L("Modeller yenilenemedi: %@", reason.text) }
        } else {
            status = notice.message
        }
        resetHighlight()
    }

    /// Cancels a refresh under way; its answer is ignored even if the request completes anyway.
    private func abandonRefresh() {
        epoch += 1
        refreshTask?.cancel()
        refreshTask = nil
        isRefreshing = false
    }

    // MARK: - Toolbar button

    /// The button's icon: a cloud when the text leaves the local network, a network for a LAN server; none for local models.
    var buttonSymbol: String? {
        guard let active = vm.activeExternalProvider else { return nil }
        return vm.sendsTextOverInternet(active) ? "cloud" : "network"
    }

    /// The tooltip: the full model name, then where the translation runs.
    var helpText: String {
        let title = vm.translationModelTitle
        guard let active = vm.activeExternalProvider else {
            return title + "\n" + L("Çeviri motoru (Gemma 4, Qwen 3.5, MADLAD, NLLB); ilk kullanımda indirilir.")
        }
        if vm.sendsTextOverInternet(active) {
            return title + "\n" + L("Çeviri %@ sağlayıcısında yapılır: çevrilecek metin ona gönderilir, ses gönderilmez.", active.name)
        }
        return title + "\n" + L("Çeviriyi yerel ağdaki başka bir bilgisayara veya Mac'e (LM Studio, Ollama, llama-server) yaptırır.")
    }
}
