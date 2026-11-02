import EngineKit
import LibraryKit
@testable import SubtitleAdderApp

/// The four settings the app used to keep for its single LAN server (`externalLlmEnabled`, `externalLlmUrl`,
/// `externalLlmModel`, `externalLlmAsync`), as properties over the provider store. The tests that only need "an external
/// server is set up" (playlist, offline mode, NLLB, async translation) keep their short setup lines; the provider
/// behavior itself is tested in `ProviderTests`. They edit one `lan` provider, created (blank, inactive) on first use.
extension SubtitleViewModel {
    private var legacyServer: TranslationProviderProfile? {
        providerStore.providers.first { $0.presetID == "lan" }
    }

    private func editLegacyServer(_ edit: (inout TranslationProviderProfile) -> Void) {
        var server = legacyServer
            ?? addProvider(
                TranslationProviderProfile(presetID: "lan", baseURL: "", model: "", asyncTranslation: false), key: "", activate: false
            ).profile
        edit(&server)
        updateProvider(server)
    }

    /// The provider is the active one.
    var externalLlmEnabled: Bool {
        get { legacyServer.map { providerStore.activeProviderID == $0.id } ?? false }
        set {
            editLegacyServer { _ in }
            activateProvider(newValue ? legacyServer?.id : nil)
        }
    }

    var externalLlmUrl: String {
        get { legacyServer?.baseURL ?? "" }
        set { editLegacyServer { $0.baseURL = newValue } }
    }

    var externalLlmModel: String {
        get { legacyServer?.model ?? "" }
        set { editLegacyServer { $0.model = newValue } }
    }

    var externalLlmAsync: Bool {
        get { legacyServer?.asyncTranslation ?? false }
        set { editLegacyServer { $0.asyncTranslation = newValue } }
    }
}
