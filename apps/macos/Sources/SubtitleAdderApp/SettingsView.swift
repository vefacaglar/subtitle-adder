import EngineKit
import SwiftUI

/// Ayarlar penceresi (⌘,). Model kataloğu paylaşıldığı için burada yapılan silme
/// araç çubuğundaki seçiciyi de anında günceller.
struct SettingsView: View {
    @Bindable var vm: SubtitleViewModel
    let catalog: ModelCatalog
    @State private var pendingDeletion: EngineModel?

    var body: some View {
        Form {
            Section {
                Picker("Varsayılan çeviri modeli", selection: $vm.translationModel) {
                    ForEach(catalog.translationModels) { model in
                        Text(ModelLabel.name(for: model.id)).tag(model.id)
                    }
                }
                .disabled(catalog.translationModels.isEmpty)
                Text("“Çevir…” düğmesi bu modeli kullanır; seçim sonraki açılışlarda da korunur.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Çeviri")
            }

            Section {
                ForEach(catalog.models.filter { !$0.isTranslation }) { model in
                    ModelSettingsRow(
                        model: model,
                        isDeleting: catalog.deleting.contains(model.id),
                        isActive: catalog.activeModel == model.id,
                        error: catalog.deleteErrors[model.id],
                        onDelete: { pendingDeletion = model }
                    )
                }
            } header: {
                Text("Altyazı modelleri")
            }

            Section {
                ForEach(catalog.translationModels) { model in
                    ModelSettingsRow(
                        model: model,
                        isDeleting: catalog.deleting.contains(model.id),
                        isActive: catalog.activeModel == model.id,
                        error: catalog.deleteErrors[model.id],
                        onDelete: { pendingDeletion = model }
                    )
                }
            } header: {
                Text("Çeviri modelleri")
            } footer: {
                footer
            }
        }
        .formStyle(.grouped)
        .frame(width: 620, height: 660)
        .task { await catalog.load() }
        .confirmationDialog(
            pendingDeletion.map { "\(ModelLabel.name(for: $0.id)) modeli silinsin mi?" } ?? "",
            isPresented: Binding(
                get: { pendingDeletion != nil },
                set: { if !$0 { pendingDeletion = nil } }
            ),
            titleVisibility: .visible,
            presenting: pendingDeletion
        ) { model in
            Button("Sil", role: .destructive) {
                pendingDeletion = nil
                Task { await catalog.delete(model) }
            }
            Button("Vazgeç", role: .cancel) { pendingDeletion = nil }
        } message: { model in
            Text("\(ModelLabel.sizeText(model.diskMB ?? model.sizeMB)) boşalır. Tekrar kullanırsan yeniden indirilir.")
        }
    }

    @ViewBuilder
    private var footer: some View {
        VStack(alignment: .leading, spacing: 4) {
            if catalog.isLoading {
                ProgressView().controlSize(.small)
            }
            if let error = catalog.loadError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
            if catalog.totalDiskMB > 0 {
                Text("Toplam \(ModelLabel.sizeText(catalog.totalDiskMB)) disk kullanılıyor.")
            } else {
                Text("Henüz indirilmiş model yok.")
            }
        }
    }
}

private struct ModelSettingsRow: View {
    let model: EngineModel
    let isDeleting: Bool
    let isActive: Bool
    let error: String?
    let onDelete: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(ModelLabel.name(for: model.id))
                        .fontWeight(.medium)
                    if isActive {
                        Label("Kullanılıyor", systemImage: "waveform")
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
                        Label("Ticari olmayan lisans", systemImage: "exclamationmark.triangle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
                if let error {
                    Text(error)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            if isDeleting {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text("Siliniyor…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else if model.downloaded {
                Button("Sil…", action: onDelete)
                    .disabled(isActive)
                    .help(isActive
                        ? "Süren bir iş bu modeli kullanıyor; iş bitince silebilirsin."
                        : "Modeli diskten sil; tekrar kullanırsan yeniden indirilir.")
            }
        }
        .padding(.vertical, 4)
    }
}
