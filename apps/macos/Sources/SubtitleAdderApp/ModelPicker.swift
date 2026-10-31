import EngineKit
import SwiftUI

/// Araç çubuğundaki model seçici. Kapalıyken yalnız modelin Türkçe adını gösterir;
/// menü satırları indirme durumunu/boyutunu ikincil metinle birlikte taşır.
/// Whisper modelleriyle diğerleri (onnx) arasında bölücü vardır.
struct ModelPicker: View {
    @Binding var selection: String
    let models: [EngineModel]

    var body: some View {
        Menu {
            ForEach(whisperModels) { model in
                modelButton(model)
            }
            if !otherModels.isEmpty {
                Divider()
                ForEach(otherModels) { model in
                    modelButton(model)
                }
            }
        } label: {
            Text(ModelLabel.name(for: selection))
                .frame(minWidth: 110, alignment: .leading)
        }
        .help("Daha doğru modeller daha yavaştır; ilk kullanımda indirilir.")
    }

    private var whisperModels: [EngineModel] { models.filter(\.detectsLanguage) }
    private var otherModels: [EngineModel] { models.filter { !$0.detectsLanguage } }

    private func modelButton(_ model: EngineModel) -> some View {
        Button {
            selection = model.id
        } label: {
            if model.id == selection {
                Label(ModelLabel.menuTitle(for: model), systemImage: "checkmark")
            } else {
                Text(ModelLabel.menuTitle(for: model))
            }
        }
    }
}
