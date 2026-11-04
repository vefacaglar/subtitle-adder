import EngineKit
import LibraryKit
import SwiftUI

extension ProviderDraft: Identifiable {}

/// Settings > Çeviri: where translation runs. The first row is the local models, the others are the saved providers
/// (cloud APIs and servers on the local network); the selected row is the active one.
struct TranslationSettingsTab: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    @State private var editor: ProviderDraft?
    @State private var pendingDeletion: TranslationProviderProfile?

    var body: some View {
        Form {
            Section {
                localRow
                ForEach(vm.providerStore.providers) { provider in
                    ProviderRow(
                        vm: vm, provider: provider,
                        onEdit: { editor = ProviderDraft(editing: provider, key: vm.providerKey(for: provider.id)) },
                        onDelete: { pendingDeletion = provider }
                    )
                }
                Button(L("API sağlayıcısı ekle…"), systemImage: "plus") { editor = ProviderDraft() }
                if let name = vm.activeCloudProviderName {
                    Label {
                        Text(L("%@ bir bulut hizmeti: çevrilecek metin ona gönderilir. Ses ve video gönderilmez.", name))
                    } icon: {
                        Image(systemName: "cloud").foregroundStyle(.orange)
                    }
                    .font(.callout)
                }
            } header: {
                Text(L("Çeviri sağlayıcıları ve API anahtarları"))
            } footer: {
                Text(L("Bulut sağlayıcıları etkinken çevrilecek metin internet üzerinden o sağlayıcıya gönderilir; ses ve video hiçbir durumda gönderilmez. Yerel modellerde ve yerel ağdaki sunucularda metin internete çıkmaz. Çevrimdışı mod açıkken bulut sağlayıcıları kullanılmaz."))
            }
        }
        .formStyle(.grouped)
        .frame(width: 560, height: 620)
        // The local server's models may have changed since the last visit. Clouds are only asked on request.
        .task {
            if let provider = vm.activeExternalProvider, !vm.sendsTextOverInternet(provider) {
                await vm.testConnection(of: provider.id)
            }
        }
        .sheet(item: $editor) { draft in
            ProviderEditorSheet(vm: vm, draft: draft)
        }
        .confirmationDialog(
            pendingDeletion.map { L("“%@” sağlayıcısı silinsin mi?", $0.name) } ?? "",
            isPresented: Binding(get: { pendingDeletion != nil }, set: { if !$0 { pendingDeletion = nil } }),
            titleVisibility: .visible,
            presenting: pendingDeletion
        ) { provider in
            Button(L("Sil"), role: .destructive) {
                pendingDeletion = nil
                vm.removeProvider(provider.id)
            }
            Button(L("Vazgeç"), role: .cancel) { pendingDeletion = nil }
        } message: { _ in
            Text(L("Bu oturumdaki API anahtarı da silinir."))
        }
    }

    /// The on-device models: selecting it switches translation back to them, and this is where the default is chosen.
    private var localRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                vm.activateProvider(nil)
            } label: {
                HStack(alignment: .top, spacing: 8) {
                    SelectionMark(isSelected: vm.providerStore.activeProviderID == nil)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(L("Yerel modeller (Cihaz üzerinde)")).fontWeight(.medium)
                        Text(L("Çeviri tamamen bu bilgisayarda indirilen yerel modellerle yapılır."))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(vm.providerStore.activeProviderID == nil ? .isSelected : [])
            Picker(L("Varsayılan çeviri modeli"), selection: $vm.translationModel) {
                ForEach(catalog.llamaTranslationModels) { model in
                    Text(ModelLabel.name(for: model.id)).tag(model.id)
                }
                if !catalog.llamaTranslationModels.isEmpty, !catalog.classicTranslationModels.isEmpty {
                    Divider()
                }
                ForEach(catalog.classicTranslationModels) { model in
                    Text(ModelLabel.name(for: model.id)).tag(model.id)
                }
            }
            .disabled(catalog.translationModels.isEmpty)
            .padding(.leading, 28)
            Text(L("“Çevir…” düğmesi bu modeli kullanır; seçim sonraki açılışlarda da korunur."))
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.leading, 28)
        }
        .padding(.vertical, 4)
    }
}

/// The round mark that shows which row is the active one.
private struct SelectionMark: View {
    let isSelected: Bool

    var body: some View {
        Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
            .imageScale(.large)
            .foregroundStyle(isSelected ? Color.accentColor : Color.secondary)
            .frame(width: 20)
            .accessibilityHidden(true)
    }
}

/// One saved provider: activate by clicking its name, test, edit, delete, and the model it translates with.
private struct ProviderRow: View {
    let vm: SubtitleViewModel
    let provider: TranslationProviderProfile
    let onEdit: () -> Void
    let onDelete: () -> Void

    private var isActive: Bool { vm.providerStore.activeProviderID == provider.id }
    private var isTesting: Bool { vm.testingProviders.contains(provider.id) }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .center, spacing: 8) {
                Button {
                    vm.activateProvider(provider.id)
                } label: {
                    HStack(alignment: .top, spacing: 8) {
                        SelectionMark(isSelected: isActive)
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(provider.name).fontWeight(.medium)
                                // A provider still called by its type's name needs no second label.
                                if provider.name.caseInsensitiveCompare(provider.preset.name) != .orderedSame {
                                    Text(provider.preset.name)
                                        .font(.caption2)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 1)
                                        .background(.quaternary, in: Capsule())
                                }
                            }
                            // Verbatim: an address is not a localization key. The key line comes from the cache, so
                            // drawing the row never waits for the secret store.
                            Text(verbatim: "\(provider.baseURL) · \(vm.providerKeyCaption(for: provider.id))")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer(minLength: 0)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(isActive ? .isSelected : [])
                if isTesting { ProgressView().controlSize(.small) }
                Button(L("Test et")) { Task { await vm.testConnection(of: provider.id) } }
                    .disabled(isTesting)
                    .help(L("Sunucuya ping at ve modelleri getir"))
                Button(L("Düzenle"), action: onEdit)
                Button(L("Sil"), action: onDelete)
            }
            .controlSize(.small)
            HStack(spacing: 6) {
                Text(L("Model:")).font(.caption).foregroundStyle(.secondary)
                ProviderModelField(vm: vm, provider: provider)
            }
            .padding(.leading, 28)
            if let notice = vm.providerNotices[provider.id] {
                NoticeLabel(notice: notice).padding(.leading, 28)
            }
        }
        .padding(.vertical, 4)
    }
}

/// The model a provider translates with: typed (any id the server knows) or picked from what is known; saved when the
/// field is submitted or loses focus, not on every key.
private struct ProviderModelField: View {
    let vm: SubtitleViewModel
    let provider: TranslationProviderProfile
    @State private var text = ""
    @FocusState private var isFocused: Bool

    var body: some View {
        HStack(spacing: 4) {
            TextField(L("Model"), text: $text, prompt: Text(L("Model seçin veya yazın")))
                .labelsHidden()
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .focused($isFocused)
                .onSubmit(commit)
            Menu {
                ForEach(provider.availableModels, id: \.self) { name in
                    Button(name) {
                        text = name
                        vm.setProviderModel(name, for: provider.id)
                    }
                }
                if !provider.availableModels.isEmpty { Divider() }
                Button(L("Uzaktaki modelleri listele")) { Task { await vm.testConnection(of: provider.id) } }
                    .disabled(vm.testingProviders.contains(provider.id))
            } label: {
                Label(L("Modelleri seç"), systemImage: "list.bullet")
            }
            .menuStyle(.borderlessButton)
            .labelStyle(.iconOnly)
            .fixedSize()
            .help(L("Modelleri seç"))
        }
        .onAppear { text = provider.model }
        .onChange(of: provider.model) { _, model in text = model }
        .onChange(of: isFocused) { _, focused in if !focused { commit() } }
    }

    private func commit() {
        let model = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard model != provider.model else { return }
        vm.setProviderModel(model, for: provider.id)
    }
}

/// A connection test's result, green with a check mark or red with a warning.
private struct NoticeLabel: View {
    let notice: ExternalConnectionNotice

    var body: some View {
        Label(
            notice.message.text,
            systemImage: notice.isError ? "exclamationmark.triangle.fill" : "checkmark.circle.fill"
        )
        .font(.caption)
        .foregroundStyle(notice.isError ? Color.red : Color.green)
    }
}

/// The sheet that adds a provider or edits one. Nothing is saved before "Kaydet ve Etkinleştir" / "Güncelle".
private struct ProviderEditorSheet: View {
    let vm: SubtitleViewModel
    @State private var draft: ProviderDraft
    @State private var notice: ExternalConnectionNotice?
    @State private var keyNotStored = false
    @State private var isTesting = false
    @State private var testTask: Task<Void, Never>?
    @Environment(\.dismiss) private var dismiss

    init(vm: SubtitleViewModel, draft: ProviderDraft) {
        self.vm = vm
        _draft = State(initialValue: draft)
    }

    var body: some View {
        VStack(spacing: 0) {
            Text(draft.isNew ? L("API Sağlayıcısı Ekle") : L("API Sağlayıcısını Düzenle"))
                .font(.headline)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding([.horizontal, .top], 20)
            Form {
                Section {
                    Picker(L("Sağlayıcı türü"), selection: presetSelection) {
                        ForEach(TranslationProviderPreset.all) { preset in
                            Text(preset.name).tag(preset.id)
                        }
                    }
                    TextField(L("Sağlayıcı adı"), text: $draft.name, prompt: Text(L("Örn. OpenCode Go")))
                    TextField(
                        L("Sunucu adresi (Base URL)"), text: $draft.baseURL,
                        prompt: Text(verbatim: draft.preset.baseURL.isEmpty ? "https://api.example.com/v1" : draft.preset.baseURL)
                    )
                    .autocorrectionDisabled()
                    if draft.preset.id == "lan" {
                        Text(L("Örn. LM Studio için 192.168.1.50:1234, Ollama için 192.168.1.50:11434"))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Section {
                    SecureField(L("API anahtarı"), text: $draft.apiKey, prompt: Text(keyPrompt))
                    if let note = storedKeyNote {
                        HStack(spacing: 8) {
                            Text(note).font(.caption).foregroundStyle(.secondary)
                            Spacer(minLength: 0)
                            if draft.canRemoveStoredKey {
                                Button(L("Kayıtlı anahtarı sil")) { draft.removeStoredKey() }
                                    .controlSize(.small)
                            }
                        }
                    }
                    caption(L("API anahtarı uygulama kapanana kadar bellekte tutulur."))
                    HStack(spacing: 4) {
                        TextField(
                            L("Model"), text: $draft.model,
                            prompt: Text(L("Model seçin veya yazın (örn. glm-5, gpt-4o-mini)"))
                        )
                        .autocorrectionDisabled()
                        Menu {
                            ForEach(draft.availableModels, id: \.self) { name in
                                Button(name) { draft.model = name }
                            }
                            if !draft.availableModels.isEmpty { Divider() }
                            Button(L("Uzaktaki modelleri listele")) { test() }
                                .disabled(isTesting || !draft.hasUsableAddress)
                        } label: {
                            Label(L("Modelleri seç"), systemImage: "list.bullet")
                        }
                        .menuStyle(.borderlessButton)
                        .labelStyle(.iconOnly)
                        .fixedSize()
                        .help(L("Modelleri seç"))
                    }
                    caption(L("Boş bırakılırsa sunucuda yüklü olan ilk model otomatik algılanır."))
                    Toggle(L("Asenkron çeviri"), isOn: $draft.asyncTranslation)
                    caption(L("Açıkken konuşma metne dökülürken aynı anda arka planda çeviri yapılır."))
                } footer: {
                    VStack(alignment: .leading, spacing: 4) {
                        if keyNotStored {
                            Label(
                                L("API anahtarı kaydedilemedi. Yeniden girip tekrar dene."),
                                systemImage: "exclamationmark.triangle.fill"
                            )
                            .foregroundStyle(.red)
                        }
                        if let notice { NoticeLabel(notice: notice) }
                    }
                }
            }
            .formStyle(.grouped)
            Divider()
            HStack(spacing: 8) {
                Button(L("Bağlantıyı test et")) { test() }
                    .disabled(isTesting || !draft.hasUsableAddress)
                if isTesting { ProgressView().controlSize(.small) }
                Spacer()
                Button(L("Vazgeç"), role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(draft.isNew ? L("Kaydet ve Etkinleştir") : L("Güncelle"), action: save)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!draft.hasUsableAddress)
            }
            .padding(12)
        }
        .frame(width: 520, height: 500)
        // The sheet opens from the cache; a key the secret store hasn't answered for (or refused) is asked again in the
        // background, never on the main thread.
        .task {
            guard !draft.isNew, !draft.storedKey.isSettled else { return }
            let key = await vm.providerKeyForUse(draft.id)
            draft.setStoredKey(key)
        }
        .onDisappear { testTask?.cancel() }
    }

    private func caption(_ text: String) -> some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
    }

    /// The key field starts empty for an existing provider: the stored key is not copied into it (and not shown), and
    /// stays as it is until something is typed or "Kayıtlı anahtarı sil" is pressed.
    private var keyPrompt: String {
        if !draft.isNew {
            switch draft.storedKey {
            case .present(let key): return L("Kayıtlı anahtar korunur: %@", TranslationProviderStore.mask(key) ?? "")
            case .loading: return L("Anahtar okunuyor…")
            case .unreadable: return L("Anahtar okunamadı; boş bırakırsan korunur")
            case .absent: break
            }
        }
        return L("sk-... (Yerel ağ için boş bırakılabilir)")
    }

    private var storedKeyNote: String? {
        guard !draft.isNew else { return nil }
        if draft.removesStoredKey { return L("Kayıtlı anahtar kaydedilince silinecek.") }
        switch draft.storedKey {
        case .present: return draft.keyEdited ? nil : L("Alan boş bırakılırsa kayıtlı anahtar korunur.")
        case .unreadable:
            return draft.keyEdited ? nil : L("API anahtarı okunamadı. Alan boş bırakılırsa anahtar silinmez.")
        case .loading, .absent: return nil
        }
    }

    /// Picking a provider type refills the name, address and models.
    private var presetSelection: Binding<String> {
        Binding(
            get: { draft.preset.id },
            set: { id in
                draft.apply(preset: .preset(id: id))
                notice = nil
            }
        )
    }

    private func test() {
        testTask?.cancel()
        let tested = draft
        isTesting = true
        notice = nil
        testTask = Task {
            let result = await vm.testConnection(of: tested)
            guard !Task.isCancelled else { return }
            isTesting = false
            // Typing another address or key meanwhile makes the answer about something else.
            guard let answer = result.notice, draft.baseURL == tested.baseURL, draft.effectiveKey == tested.effectiveKey else { return }
            notice = answer
            guard !result.models.isEmpty else { return }
            draft.discoveredModels = result.models
            if draft.model.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { draft.model = result.models[0] }
        }
    }

    private func save() {
        testTask?.cancel()
        isTesting = false
        let stored: Bool
        if draft.isNew {
            stored = vm.addProvider(draft.profile, key: draft.apiKey).keyStored
            // The provider exists now; saving again (after a refused key) updates it.
            draft.isNew = false
        } else {
            // `nil` while the key field is untouched: the stored key stays, even one that could not be read.
            stored = vm.updateProvider(draft.profile, key: draft.keyToSave)?.keyStored ?? true
        }
        keyNotStored = !stored
        if stored { dismiss() }
    }
}
