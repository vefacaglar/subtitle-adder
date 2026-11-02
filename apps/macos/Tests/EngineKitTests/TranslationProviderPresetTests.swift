import Foundation
import Testing
@testable import EngineKit

@Suite struct TranslationProviderPresetTests {
    @Test func listsTheWindowsPresetsInOrder() {
        #expect(TranslationProviderPreset.all.map(\.id) == [
            "opencode-go", "openrouter", "openai", "deepseek", "groq", "gemini", "lan", "custom",
        ])
        #expect(TranslationProviderPreset.all.last == .custom)
    }

    @Test func presetsCarryAddressKeyRequirementAndSuggestedModels() throws {
        let opencode = TranslationProviderPreset.preset(id: "opencode-go")
        #expect(opencode.name == "OpenCode Go")
        #expect(opencode.baseURL == "https://opencode.ai/zen/go/v1")
        #expect(opencode.requiresAPIKey)
        #expect(opencode.suggestedModels == ["glm-5", "kimi-k2.5", "minimax-m2.5", "deepseek-v3", "qwen3-235b"])
        #expect(opencode.defaultModel == "glm-5")

        let table: [(id: String, name: String, url: String, models: [String])] = [
            ("openrouter", "OpenRouter", "https://openrouter.ai/api/v1",
             ["google/gemma-3-27b-it", "deepseek/deepseek-chat-v3-0324", "qwen/qwen-2.5-72b-instruct", "openai/gpt-4o-mini"]),
            ("openai", "OpenAI", "https://api.openai.com/v1", ["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"]),
            ("deepseek", "DeepSeek", "https://api.deepseek.com/v1", ["deepseek-chat"]),
            ("groq", "Groq", "https://api.groq.com/openai/v1", ["llama-3.3-70b-versatile", "gemma2-9b-it"]),
            ("gemini", "Google Gemini", "https://generativelanguage.googleapis.com/v1beta/openai", ["gemini-2.5-flash", "gemini-2.5-pro"]),
        ]
        for row in table {
            let preset = TranslationProviderPreset.preset(id: row.id)
            #expect(preset.id == row.id)
            #expect(preset.name == row.name)
            #expect(preset.baseURL == row.url)
            #expect(preset.requiresAPIKey)
            #expect(preset.suggestedModels == row.models)
        }

        let lan = TranslationProviderPreset.preset(id: "lan")
        #expect(lan.baseURL == "http://127.0.0.1:11434")
        #expect(!lan.requiresAPIKey)
        #expect(lan.suggestedModels == ["gemma4:e4b", "qwen3.5:4b"])
        let custom = TranslationProviderPreset.custom
        #expect(custom.baseURL.isEmpty)
        #expect(custom.requiresAPIKey)
        #expect(custom.suggestedModels.isEmpty)
        #expect(custom.defaultModel.isEmpty)
    }

    @Test func onlyTheGenericPresetsAreLocalized() {
        let lan = TranslationProviderPreset.preset(id: "lan")
        #expect(lan.name(language: .tr) == "Yerel Ağ (Ollama / LM Studio)")
        #expect(lan.name(language: .en) == "Local LAN (Ollama / LM Studio)")
        let custom = TranslationProviderPreset.custom
        #expect(custom.name(language: .tr) == "Özel (OpenAI Uyumlu)")
        #expect(custom.name(language: .en) == "Custom (OpenAI Compatible)")
        for preset in TranslationProviderPreset.all where preset.id != "lan" && preset.id != "custom" {
            #expect(preset.name(language: .tr) == preset.name(language: .en), "\(preset.id)")
        }
    }

    @Test func anUnknownOrMissingIdIsCustom() {
        #expect(TranslationProviderPreset.preset(id: "no-such-provider") == .custom)
        #expect(TranslationProviderPreset.preset(id: nil) == .custom)
        #expect(TranslationProviderPreset.preset(id: "") == .custom)
        #expect(TranslationProviderPreset.preset(id: "OpenAI").id == "openai")
        #expect(TranslationProviderPreset.preset(id: "LAN").id == "lan")
    }
}
