import EngineKit
import SwiftUI

/// The Settings window (⌘,) with native tabs. Since the model catalog is shared, a delete made here
/// also updates the toolbar picker immediately.
struct SettingsView: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    @Bindable var language: AppLanguage

    var body: some View {
        TabView {
            GeneralSettingsTab(vm: vm, catalog: catalog, language: language)
                .tabItem { Label(L("Genel"), systemImage: "gearshape") }
            AppearanceSettingsTab()
                .tabItem { Label(L("Görünüm"), systemImage: "paintbrush") }
            ModelsSettingsTab(catalog: catalog)
                .tabItem { Label(L("Modeller"), systemImage: "square.stack.3d.up") }
        }
        .scenePadding()
    }
}

private struct GeneralSettingsTab: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    @Bindable var language: AppLanguage

    var body: some View {
        Form {
            Section {
                Picker(L("Arayüz dili"), selection: $language.choice) {
                    ForEach(LanguageChoice.allCases) { choice in
                        Text(choice.title).tag(choice)
                    }
                }
                if language.needsRelaunch {
                    Text(L("Menüler ve sistem diyalogları uygulama yeniden açılınca güncellenir."))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text(L("Dil"))
            }

            Section {
                Picker(L("Konuşma dili"), selection: $vm.language) {
                    ForEach(SubtitleLanguage.allCases) { language in
                        Text(language.title).tag(language.rawValue)
                    }
                }
                Picker(L("Altyazı modeli"), selection: $vm.model) {
                    ForEach(vm.availableModels) { model in
                        Text(ModelLabel.name(for: model.id)).tag(model.id)
                    }
                }
                .disabled(vm.availableModels.isEmpty)
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
                Text(L("“Çevir…” düğmesi bu modeli kullanır; seçim sonraki açılışlarda da korunur."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text(L("Altyazı ve çeviri"))
            }

            Section {
                Toggle(L("Konuşmacıları ayır"), isOn: $vm.diarize)
                Picker(L("Konuşmacı sayısı"), selection: $vm.speakerCount) {
                    Text(L("Otomatik")).tag(0)
                    ForEach(2...6, id: \.self) { count in
                        Text("\(count)").tag(count)
                    }
                }
                .disabled(!vm.diarize)
            } header: {
                Text(L("Konuşmacılar"))
            } footer: {
                Text(L("Videoda birden çok kişi konuşuyorsa kimin ne dediğini ayır; ilk kullanımda küçük bir model indirilir."))
            }

            Section {
                Toggle(L("Çevrimdışı mod"), isOn: $vm.offlineMode)
            } header: {
                Text(L("Gizlilik"))
            } footer: {
                Text(L("Açıkken internete hiç bağlanılmaz ve eksik modeller indirilmez. Ses ve metin hiçbir durumda dışarı gönderilmez."))
            }
        }
        .formStyle(.grouped)
        .frame(width: 560, height: 640)
    }
}

private struct AppearanceSettingsTab: View {
    @AppStorage(AppearanceKeys.theme) private var theme = ThemeChoice.system.rawValue
    @AppStorage(AppearanceKeys.overlayFont) private var family = ""
    @AppStorage(AppearanceKeys.overlaySize) private var size = OverlayStyle.defaultSize
    @AppStorage(AppearanceKeys.overlayTextColor) private var textHex = OverlayStyle.defaultTextHex
    @AppStorage(AppearanceKeys.overlayBackground) private var background = OverlayBackground.box.rawValue
    @AppStorage(AppearanceKeys.overlayOpacity) private var opacity = OverlayStyle.defaultOpacity

    private static let families = NSFontManager.shared.availableFontFamilies

    private var style: OverlayStyle {
        OverlayStyle(storedFamily: family, size: size, textHex: textHex, background: background, opacity: opacity)
    }

    var body: some View {
        Form {
            Section {
                Picker(L("Tema"), selection: $theme) {
                    ForEach(ThemeChoice.allCases) { choice in
                        Text(choice.title).tag(choice.rawValue)
                    }
                }
                .pickerStyle(.segmented)
            } header: {
                Text(L("Uygulama"))
            }

            Section {
                preview
                Picker(L("Yazı tipi"), selection: $family) {
                    Text(L("Sistem")).tag("")
                    Divider()
                    ForEach(Self.families, id: \.self) { Text($0).tag($0) }
                }
                LabeledContent(L("Boyut")) {
                    HStack {
                        Slider(value: $size, in: OverlayStyle.sizeRange)
                            .frame(width: 180)
                        Text(L("%lld%%", Int((style.sizeFraction * 100).rounded())))
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                            .frame(width: 36, alignment: .trailing)
                    }
                }
                ColorPicker(L("Yazı rengi"), selection: Binding(
                    get: { style.textColor },
                    set: { textHex = HexColor.hex(from: $0) }
                ), supportsOpacity: false)
                Picker(L("Arka plan"), selection: $background) {
                    ForEach(OverlayBackground.allCases) { Text($0.title).tag($0.rawValue) }
                }
                LabeledContent(L("Arka plan opaklığı")) {
                    HStack {
                        Slider(value: $opacity, in: 0...1)
                            .frame(width: 180)
                        Text(L("%lld%%", Int((style.backgroundOpacity * 100).rounded())))
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                            .frame(width: 36, alignment: .trailing)
                    }
                }
                .disabled(style.background != .box)
                Button(L("Varsayılana döndür")) {
                    family = ""
                    size = OverlayStyle.defaultSize
                    textHex = OverlayStyle.defaultTextHex
                    background = OverlayBackground.box.rawValue
                    opacity = OverlayStyle.defaultOpacity
                }
            } header: {
                Text(L("Video üstündeki altyazı"))
            } footer: {
                Text(L("Bu ayarlar yalnızca uygulamadaki önizlemeyi etkiler; dışa aktarılan altyazı dosyalarını değiştirmez."))
            }
        }
        .formStyle(.grouped)
        .frame(width: 560, height: 600)
    }

    private var preview: some View {
        ZStack(alignment: .bottom) {
            LinearGradient(colors: [.gray.opacity(0.7), .black], startPoint: .top, endPoint: .bottom)
            SubtitleOverlayLabel(text: L("Örnek altyazı metni"), style: style, videoHeight: 150)
                .padding(.bottom, 16)
        }
        .frame(height: 100)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .accessibilityHidden(true)
    }
}

private struct ModelsSettingsTab: View {
    let catalog: ModelCatalog
    @State private var pendingDeletion: EngineModel?

    var body: some View {
        Form {
            Section {
                ForEach(catalog.models.filter(\.isTranscription)) { model in
                    ModelSettingsRow(
                        model: model,
                        isDeleting: catalog.deleting.contains(model.id),
                        isActive: catalog.activeModels.contains(model.id),
                        error: catalog.deleteErrors[model.id],
                        onDelete: { pendingDeletion = model }
                    )
                }
            } header: {
                Text(L("Altyazı modelleri"))
            }

            if !catalog.diarizationModels.isEmpty {
                Section {
                    ForEach(catalog.diarizationModels) { model in
                        ModelSettingsRow(
                            model: model,
                            isDeleting: catalog.deleting.contains(model.id),
                            isActive: catalog.activeModels.contains(model.id),
                            error: catalog.deleteErrors[model.id],
                            onDelete: { pendingDeletion = model }
                        )
                    }
                } header: {
                    Text(L("Konuşmacılar"))
                }
            }

            Section {
                ForEach(catalog.translationModels) { model in
                    ModelSettingsRow(
                        model: model,
                        isDeleting: catalog.deleting.contains(model.id),
                        isActive: catalog.activeModels.contains(model.id),
                        error: catalog.deleteErrors[model.id],
                        onDelete: { pendingDeletion = model }
                    )
                }
            } header: {
                Text(L("Çeviri modelleri"))
            } footer: {
                footer
            }
        }
        .formStyle(.grouped)
        .frame(width: 560, height: 620)
        .task { await catalog.load() }
        .confirmationDialog(
            pendingDeletion.map { L("%@ modeli silinsin mi?", ModelLabel.name(for: $0.id)) } ?? "",
            isPresented: Binding(
                get: { pendingDeletion != nil },
                set: { if !$0 { pendingDeletion = nil } }
            ),
            titleVisibility: .visible,
            presenting: pendingDeletion
        ) { model in
            Button(L("Sil"), role: .destructive) {
                pendingDeletion = nil
                Task { await catalog.delete(model) }
            }
            Button(L("Vazgeç"), role: .cancel) { pendingDeletion = nil }
        } message: { model in
            Text(L("%@ boşalır. Tekrar kullanırsan yeniden indirilir.", ModelLabel.sizeText(model.diskMB ?? model.sizeMB)))
        }
    }

    @ViewBuilder
    private var footer: some View {
        VStack(alignment: .leading, spacing: 4) {
            if catalog.isLoading {
                ProgressView().controlSize(.small)
            }
            if let error = catalog.loadError {
                Label(error.text, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
            if catalog.totalDiskMB > 0 {
                Text(L("Toplam %@ disk kullanılıyor.", ModelLabel.sizeText(catalog.totalDiskMB)))
            } else {
                Text(L("Henüz indirilmiş model yok."))
            }
        }
    }
}

private struct ModelSettingsRow: View {
    let model: EngineModel
    let isDeleting: Bool
    let isActive: Bool
    let error: LocalizedMessage?
    let onDelete: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(ModelLabel.name(for: model.id))
                        .fontWeight(.medium)
                    if isActive {
                        Label(L("Kullanılıyor"), systemImage: "waveform")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Text(ModelLabel.languageSummary(for: model))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(ModelLabel.storageStatus(for: model))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    Text(ModelLabel.licenseText(for: model))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if model.hasNonCommercialLicense {
                        Label(L("Ticari olmayan lisans"), systemImage: "exclamationmark.triangle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
                if let error {
                    Text(error.text)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            if isDeleting {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text(L("Siliniyor…"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else if model.downloaded {
                Button(L("Sil…"), action: onDelete)
                    .disabled(isActive)
                    .help(isActive
                        ? L("Süren bir iş bu modeli kullanıyor; iş bitince silebilirsin.")
                        : L("Modeli diskten sil; tekrar kullanırsan yeniden indirilir."))
            }
        }
        .padding(.vertical, 4)
    }
}
