import Foundation

/// An OpenAI-compatible / Ollama server on the local network that does the translation instead of a local model.
public struct ExternalLLM: Sendable, Equatable {
    /// Address as typed by the user; see `ExternalLLMServer.normalizeBaseURL(_:)`.
    public var url: String
    /// Model name on the server; empty lets the engine pick the first one the server lists.
    public var model: String

    public init(url: String, model: String = "") {
        self.url = url
        self.model = model
    }

    /// The address the engine receives, or `nil` when `url` is blank.
    public var normalizedURL: String? { ExternalLLMServer.normalizeBaseURL(url) }
}

/// App settings that reach the engine through environment variables on every launch.
public struct EngineLaunchSettings: Sendable, Equatable {
    /// `SUBTITLE_ENGINE_OFFLINE`: strict offline mode, no network at all (not even model downloads).
    public var offline: Bool
    /// `SUBTITLE_ENGINE_EXTERNAL_LLM_URL` / `_MODEL`; `nil` translates with a local model.
    public var externalLLM: ExternalLLM?

    public init(offline: Bool = false, externalLLM: ExternalLLM? = nil) {
        self.offline = offline
        self.externalLLM = externalLLM
    }

    /// Adds the engine variables to `env`; the external LLM ones are removed when no usable server is configured.
    func apply(to env: inout [String: String]) {
        env["SUBTITLE_ENGINE_OFFLINE"] = offline ? "1" : "0"
        if let externalLLM, let url = externalLLM.normalizedURL {
            env["SUBTITLE_ENGINE_EXTERNAL_LLM_URL"] = url
            env["SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL"] = externalLLM.model.trimmingCharacters(in: .whitespacesAndNewlines)
        } else {
            env.removeValue(forKey: "SUBTITLE_ENGINE_EXTERNAL_LLM_URL")
            env.removeValue(forKey: "SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL")
        }
    }
}

/// Process-wide `EngineLaunchSettings`, read at each engine launch (the way `LanguageState` carries the UI language).
/// The app updates it whenever the matching setting changes; reading it does no I/O.
public final class EngineLaunchState: @unchecked Sendable {
    public static let shared = EngineLaunchState()

    private let lock = NSLock()
    private var value = EngineLaunchSettings()

    public init() {}

    public var settings: EngineLaunchSettings {
        get {
            lock.lock()
            defer { lock.unlock() }
            return value
        }
        set {
            lock.lock()
            value = newValue
            lock.unlock()
        }
    }
}
