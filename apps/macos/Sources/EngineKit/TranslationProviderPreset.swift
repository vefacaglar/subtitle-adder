import Foundation

/// A built-in template for an OpenAI-compatible translation provider (a cloud API or a server on the local network):
/// its address, whether it needs an API key and a few models known to translate well. Same list as the Windows app.
public struct TranslationProviderPreset: Sendable, Equatable, Identifiable {
    /// Stable identifier stored in the provider list (`"openai"`, `"lan"`, `"custom"`, ...).
    public let id: String
    /// Provider name, or the Turkish catalog key for presets whose name is localized.
    private let rawName: String
    /// Address the provider is reached at; empty for `custom` (the user enters it).
    public let baseURL: String
    public let requiresAPIKey: Bool
    /// Models offered before the server has been asked; the first one is the default.
    public let suggestedModels: [String]
    /// Brand names stay as they are; the two generic presets are looked up in the EngineKit catalog.
    private let localizesName: Bool

    /// Id of the preset used for any provider the app does not know (and for addresses the user typed).
    public static let customID = "custom"

    /// Display name in the current UI language (`language` overrides it).
    public func name(language: UILanguage? = nil) -> String {
        localizesName ? engineString(rawName, language: language) : rawName
    }

    public var name: String { name() }

    /// The model a new provider starts with, empty when the preset suggests none.
    public var defaultModel: String { suggestedModels.first ?? "" }

    /// Id of the preset for a server on the local network.
    public static let lanID = "lan"

    /// Whether a new provider of this type translates in the background while transcription is still running. Only a
    /// server on the local network does: it is the Mac's own helper and costs nothing, while a cloud service bills
    /// per request, so the user switches it on knowingly.
    public var translatesInBackgroundByDefault: Bool { id == Self.lanID }

    /// All presets in display order; `custom` is last.
    public static let all: [TranslationProviderPreset] = [
        TranslationProviderPreset(
            id: "opencode-go", rawName: "OpenCode Go", baseURL: "https://opencode.ai/inference/go/openai/v1",
            requiresAPIKey: true,
            suggestedModels: ["glm-5.3-flash", "kimi-k3.1-pro", "minimax-m3.1-flash", "deepseek-v4.1-flash", "deepseek-v4-pro"],
            localizesName: false
        ),
        TranslationProviderPreset(
            id: "openrouter", rawName: "OpenRouter", baseURL: "https://openrouter.ai/api/v1", requiresAPIKey: true,
            suggestedModels: [
                "google/gemma-3-27b-it", "deepseek/deepseek-chat-v3-0324", "qwen/qwen-2.5-72b-instruct", "openai/gpt-4o-mini",
            ],
            localizesName: false
        ),
        TranslationProviderPreset(
            id: "openai", rawName: "OpenAI", baseURL: "https://api.openai.com/v1", requiresAPIKey: true,
            suggestedModels: ["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"], localizesName: false
        ),
        TranslationProviderPreset(
            id: "deepseek", rawName: "DeepSeek", baseURL: "https://api.deepseek.com/v1", requiresAPIKey: true,
            suggestedModels: ["deepseek-chat"], localizesName: false
        ),
        TranslationProviderPreset(
            id: "groq", rawName: "Groq", baseURL: "https://api.groq.com/openai/v1", requiresAPIKey: true,
            suggestedModels: ["llama-3.3-70b-versatile", "gemma2-9b-it"], localizesName: false
        ),
        TranslationProviderPreset(
            id: "gemini", rawName: "Google Gemini", baseURL: "https://generativelanguage.googleapis.com/v1beta/openai",
            requiresAPIKey: true, suggestedModels: ["gemini-2.5-flash", "gemini-2.5-pro"], localizesName: false
        ),
        TranslationProviderPreset(
            id: "lan", rawName: "Yerel Ağ (Ollama / LM Studio)", baseURL: "http://127.0.0.1:11434", requiresAPIKey: false,
            suggestedModels: ["gemma4:e4b", "qwen3.5:4b"], localizesName: true
        ),
        custom,
    ]

    /// The user-entered preset: no address, no suggestions.
    public static let custom = TranslationProviderPreset(
        id: customID, rawName: "Özel (OpenAI Uyumlu)", baseURL: "", requiresAPIKey: true,
        suggestedModels: [], localizesName: true
    )

    /// The preset with this id (case-insensitive); `custom` for an unknown or missing id.
    public static func preset(id: String?) -> TranslationProviderPreset {
        let wanted = id?.lowercased()
        return all.first { $0.id == wanted } ?? custom
    }
}
