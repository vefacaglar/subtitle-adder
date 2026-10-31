import EngineKit
import Foundation
import SwiftUI

/// Araç çubuğundaki model seçici. Kapalıyken yalnız modelin Türkçe adını gösterir;
/// menü satırları indirme durumunu/boyutunu ikincil metinle birlikte taşır.
struct ModelPicker: View {
    @Binding var selection: String
    let models: [EngineModel]

    var body: some View {
        Menu {
            Picker("Model", selection: $selection) {
                ForEach(models) { model in
                    Text(Self.title(for: model)).tag(model.id)
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
        } label: {
            Text(Self.name(for: selection))
        }
        .frame(minWidth: 110, alignment: .leading)
        .help("Daha doğru modeller daha yavaştır; ilk kullanımda indirilir.")
    }

    private static func title(for model: EngineModel) -> String {
        "\(name(for: model.id)) · \(detail(for: model))"
    }

    private static func detail(for model: EngineModel) -> String {
        model.downloaded ? "hazır" : "ilk kullanımda ~\(sizeText(model.sizeMB)) indirilir"
    }

    /// Katalogda olmayan bir kimlik (eski bir tercih vb.) doğrudan gösterilir.
    private static func name(for id: String) -> String {
        switch id {
        case "base": "Hızlı"
        case "small": "Dengeli"
        case "medium": "Doğru"
        case "large-v3-turbo": "Çok doğru (turbo)"
        case "large-v3": "En doğru"
        default: id
        }
    }

    private static func sizeText(_ sizeMB: Int) -> String {
        guard sizeMB >= 1000 else { return "\(sizeMB) MB" }
        var text = String(format: "%.1f", Double(sizeMB) / 1000)
            .replacingOccurrences(of: ".", with: ",")
        if text.hasSuffix(",0") { text = String(text.dropLast(2)) }
        return "\(text) GB"
    }
}
