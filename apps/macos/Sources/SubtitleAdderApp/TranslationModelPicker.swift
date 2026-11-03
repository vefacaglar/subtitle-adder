import SwiftUI

/// The toolbar's translation-model control: a button with the current model that opens a popover to pick a provider
/// and one of its models. All decisions are in `TranslationModelPickerModel`.
struct TranslationModelPickerButton: View {
    let vm: SubtitleViewModel
    @State private var model: TranslationModelPickerModel
    @State private var isPresented = false
    @Environment(\.openSettings) private var openSettings
    @AppStorage(SettingsTab.storageKey) private var settingsTab = SettingsTab.general.rawValue

    init(vm: SubtitleViewModel) {
        self.vm = vm
        _model = State(initialValue: TranslationModelPickerModel(vm: vm))
    }

    var body: some View {
        Button {
            model.open()
            isPresented = true
        } label: {
            HStack(spacing: 4) {
                if let symbol = model.buttonSymbol {
                    Image(systemName: symbol)
                }
                Text(vm.translationModelTitle)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            .frame(minWidth: 110, maxWidth: 220, alignment: .leading)
        }
        .disabled(vm.isWorking)
        .accessibilityLabel(L("Çeviri modeli"))
        .accessibilityValue(vm.translationModelTitle)
        .help(model.helpText)
        .popover(isPresented: $isPresented, arrowEdge: .bottom) {
            TranslationModelPopover(model: model, dismiss: { isPresented = false }, manageProviders: {
                isPresented = false
                settingsTab = SettingsTab.translation.rawValue
                openSettings()
            })
        }
        .onChange(of: isPresented) { _, presented in
            if !presented { model.close() }
        }
        // A job started elsewhere (a shortcut, the queue): the choice would change the model under it.
        .onChange(of: vm.isWorking) { _, working in
            if working { isPresented = false }
        }
        .onChange(of: vm.providerStore.providers.map(\.id)) {
            model.providersChanged()
        }
    }
}

private struct TranslationModelPopover: View {
    @Bindable var model: TranslationModelPickerModel
    let dismiss: () -> Void
    let manageProviders: () -> Void
    @FocusState private var searchFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Picker(L("Çeviri sağlayıcısı"), selection: providerSelection) {
                ForEach(model.providerChoices) { choice in
                    Text(choice.name).tag(choice.id)
                }
            }
            .labelsHidden()
            .accessibilityLabel(L("Çeviri sağlayıcısı"))

            TextField(L("Model ara…"), text: $model.searchText)
                .textFieldStyle(.roundedBorder)
                .focused($searchFocused)
                .onSubmit { chooseHighlighted() }
                .onKeyPress(.upArrow) {
                    model.moveHighlight(by: -1)
                    return .handled
                }
                .onKeyPress(.downArrow) {
                    model.moveHighlight(by: 1)
                    return .handled
                }
                .onKeyPress(.escape) {
                    dismiss()
                    return .handled
                }
                .accessibilityLabel(L("Model ara…"))

            modelList
                .frame(height: 240)

            Text(model.statusText ?? " ")
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
                .help(model.statusText ?? "")
                .frame(maxWidth: .infinity, alignment: .leading)

            HStack {
                if model.browsedProvider != nil {
                    Button(L("Yenile"), systemImage: "arrow.clockwise") { model.refresh() }
                        .disabled(!model.canRefresh)
                }
                Spacer()
                Button(L("API sağlayıcılarını yönet…"), action: manageProviders)
            }
        }
        .padding(12)
        .frame(width: 340)
        .task { searchFocused = true }
    }

    private var providerSelection: Binding<UUID?> {
        Binding(get: { model.browsedProviderID }, set: { model.selectProvider($0) })
    }

    private var modelList: some View {
        ScrollViewReader { proxy in
            List(selection: $model.highlightedID) {
                ForEach(model.rows) { row in
                    Button {
                        if model.choose(row.id) { dismiss() }
                    } label: {
                        HStack(spacing: 6) {
                            Text(row.title)
                                .lineLimit(2)
                                .truncationMode(.tail)
                            Spacer(minLength: 0)
                            if row.isCurrent {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(!row.isEnabled)
                    .selectionDisabled(!row.isEnabled)
                    .tag(row.id)
                    .id(row.id)
                    .help(row.title)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(row.title)
                    .accessibilityAddTraits(row.isCurrent ? [.isButton, .isSelected] : .isButton)
                }
            }
            .listStyle(.plain)
            .accessibilityLabel(L("Çeviri modeli"))
            .onChange(of: model.highlightedID) { _, id in
                if let id { proxy.scrollTo(id) }
            }
            .onKeyPress(.return) {
                chooseHighlighted()
                return .handled
            }
            .overlay {
                if let text = model.emptyText {
                    Text(text)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .padding()
                }
            }
        }
    }

    private func chooseHighlighted() {
        if model.chooseHighlighted() { dismiss() }
    }
}
