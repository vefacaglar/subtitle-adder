import EngineKit
import SwiftUI

/// Ayarlar penceresi (⌘,). Model kataloğu paylaşıldığı için burada yapılan silme
/// araç çubuğundaki seçiciyi de anında günceller.
struct SettingsView: View {
    let catalog: ModelCatalog
    @State private var pendingDeletion: EngineModel?

    var body: some View {
        Form {
            Section {
                ForEach(catalog.models) { model in
                    ModelSettingsRow(
                        model: model,
                        isDeleting: catalog.deleting.contains(model.id),
                        isActive: catalog.activeModel == model.id,
                        error: catalog.deleteErrors[model.id],
                        onDelete: { pendingDeletion = model }
                    )
                }
            } header: {
                Text("Modeller")
            } footer: {
                footer
            }
        }
        .formStyle(.grouped)
        .frame(width: 600, height: 540)
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
                        ? "Altyazı üretimi bu modeli kullanıyor; iş bitince silebilirsin."
                        : "Modeli diskten sil; tekrar kullanırsan yeniden indirilir.")
            }
        }
        .padding(.vertical, 4)
    }
}
