import Foundation

/// Helpers for the external LLM server (a LAN server or a cloud provider): address normalization and the connection
/// test. The engine has no test command, so the app probes the server itself over plain HTTP (only on an explicit
/// user action).
public enum ExternalLLMServer {
    /// One HTTP exchange: the response body and status code. A transport error (no route, timeout, TLS) throws; an
    /// HTTP error status does not, because the test treats 401/403 differently from "not found".
    public typealias Fetch = @Sendable (URLRequest) async throws -> (data: Data, status: Int)

    /// Per-request timeout of the connection test.
    public static let timeout: TimeInterval = 8

    /// Sent with every request of the test.
    static let userAgent = "SubtitleAdder/1.0"

    /// Model name for the chat probe when neither the profile nor its preset names one.
    static let fallbackProbeModel = "gpt-4o-mini"

    /// Suffixes pasted by users from client configs; one of them is stripped from the end of the address. A bare
    /// `/v1` is not among them: providers such as Groq (`…/openai/v1`) need it, and the engine handles it itself.
    private static let strippedSuffixes = [
        "/v1/chat/completions", "/chat/completions", "/api/chat", "/api/tags", "/v1/models",
    ]

    /// Trims the address, drops trailing slashes, adds `http://` when there is no scheme and strips one API suffix.
    /// `nil` when nothing usable (no host) is left.
    public static func normalizeBaseURL(_ raw: String) -> String? {
        var url = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while url.hasSuffix("/") { url.removeLast() }
        // What is left of a bare "http://" once its slashes are gone.
        guard !url.isEmpty, !["http:", "https:"].contains(url.lowercased()) else { return nil }
        let lower = url.lowercased()
        if !lower.hasPrefix("http://") && !lower.hasPrefix("https://") {
            url = "http://" + url
        }
        for suffix in strippedSuffixes where url.lowercased().hasSuffix(suffix) {
            url.removeLast(suffix.count)
            while url.hasSuffix("/") { url.removeLast() }
            // "/v1" is implied at the root of a server ("host:1234/v1/models" -> "host:1234"), but a provider whose
            // API lives under a path ("api.groq.com/openai/v1/chat/completions") keeps it: the engine appends
            // "/chat/completions" to "…/openai/v1", while "…/openai" alone would be a different endpoint.
            if suffix.hasPrefix("/v1/"), let path = URL(string: url)?.path, !path.isEmpty {
                url += "/v1"
            }
            break
        }
        // "http://" alone (or an address with a space) has no host to talk to.
        guard let host = URL(string: url)?.host(), !host.isEmpty else { return nil }
        return url
    }

    /// Model ids from an Ollama `/api/tags` (`models[].name`) or OpenAI `/v1/models` (`data[].id`) response, in order,
    /// de-duplicated case-insensitively, without embedding-only models (they can't translate).
    public static func parseSuitableModels(_ data: Data) -> [String] {
        guard let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return [] }
        var result: [String] = []
        var seen = Set<String>()
        for key in ["data", "models"] {
            guard let items = root[key] as? [Any] else { continue }
            for case let item as [String: Any] in items {
                let raw = (item["id"] as? String) ?? (item["name"] as? String)
                guard let id = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !id.isEmpty else { continue }
                let lower = id.lowercased()
                if lower.contains("embed") || lower.contains("minilm") || lower.contains("bge-") { continue }
                if seen.insert(lower).inserted { result.append(id) }
            }
        }
        return result
    }

    /// Model-list URLs to try for a (normalized) address, in order. A trailing `/chat/completions` is dropped; an
    /// address ending in `/models` is the list itself; one ending in `/v1` or `/openai` (OpenAI-style roots, e.g.
    /// Groq and Gemini) has a single candidate; anything else is a bare server (OpenAI, then Ollama's own list).
    static func modelEndpoints(for base: String) -> [String] {
        var clean = trimmingSlashes(base)
        if clean.lowercased().hasSuffix("/chat/completions") {
            clean = trimmingSlashes(String(clean.dropLast("/chat/completions".count)))
        } else if clean.lowercased().hasSuffix("/models") {
            return [clean]
        }
        if endsWithAPIRoot(clean) { return [clean + "/models"] }
        return [clean + "/v1/models", clean + "/models", clean + "/api/tags"]
    }

    /// The chat endpoint the probe posts to: the address itself when it already is one, `{base}/chat/completions`
    /// for an address ending in `/v1` or `/openai`, otherwise `{base}/v1/chat/completions` (the engine's own rule).
    static func chatEndpoint(for base: String) -> String {
        let clean = trimmingSlashes(base)
        if clean.lowercased().hasSuffix("/chat/completions") { return clean }
        if endsWithAPIRoot(clean) { return clean + "/chat/completions" }
        return clean + "/v1/chat/completions"
    }

    private static func endsWithAPIRoot(_ url: String) -> Bool {
        let lower = url.lowercased()
        return lower.hasSuffix("/v1") || lower.hasSuffix("/openai")
    }

    private static func trimmingSlashes(_ url: String) -> String {
        var url = url
        while url.hasSuffix("/") { url.removeLast() }
        return url
    }

    /// A request of the test: JSON in, JSON out, the key as a bearer token only when there is one.
    static func request(_ url: URL, method: String = "GET", apiKey: String, body: Data? = nil) -> URLRequest {
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = method
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let key = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        if !key.isEmpty { request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization") }
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        return request
    }

    /// The one-token chat request that shows a gateway without a model list is alive.
    static func probeBody(model: String) -> Data {
        let payload: [String: Any] = [
            "model": model,
            "messages": [["role": "user", "content": "hi"]],
            "max_tokens": 1,
            "stream": false,
        ]
        return (try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])) ?? Data()
    }

    /// "API anahtarı geçersiz veya yetkisiz (HTTP 401)." in the UI language (`language` overrides it).
    public static func unauthorizedMessage(status: Int, language: UILanguage? = nil) -> String {
        engineString("API anahtarı geçersiz veya yetkisiz (HTTP %lld).", language: language, status)
    }

    public enum ConnectionResult: Sendable, Equatable {
        /// The address is blank or can't form a URL.
        case invalidAddress
        /// The server refused the key (HTTP 401 or 403); the probe stopped there.
        case unauthorized(status: Int)
        /// No endpoint answered; `reason` is the last error's description.
        case unreachable(reason: String)
        /// At least one endpoint answered. `models` can be empty (the server is up but lists nothing suitable, or
        /// only answered the chat probe).
        case reachable(host: String, latencyMS: Int, models: [String])
        /// The probe was cancelled (for example the Settings window closed); says nothing about the server.
        case cancelled

        public var models: [String] {
            if case .reachable(_, _, let models) = self { models } else { [] }
        }
    }

    /// Asks the server for its models (`modelEndpoints(for:)`, 8 s each, the key as a bearer token when given) and
    /// stops at the first that lists suitable models; a 401/403 stops at once. When no endpoint answered at all
    /// (gateways that only expose chat), a one-token chat request decides whether the server is reachable.
    /// `model` and the preset's first suggestion name the model of that request. `latencyMS` covers all attempts
    /// and is at least 1.
    public static func testConnection(
        rawURL: String, apiKey: String = "", model: String = "", presetID: String? = nil,
        fetch: Fetch = urlSessionFetch(), clock: ContinuousClock = ContinuousClock()
    ) async -> ConnectionResult {
        guard let base = normalizeBaseURL(rawURL), let host = URL(string: base)?.host() else {
            return .invalidAddress
        }
        let started = clock.now
        var models: [String] = []
        var reachable = false
        var unauthorized: Int?
        var lastError: String?

        for endpoint in modelEndpoints(for: base) {
            guard let url = URL(string: endpoint) else { continue }
            do {
                let (body, status) = try await fetch(request(url, apiKey: apiKey))
                if status == 401 || status == 403 {
                    unauthorized = status
                    break
                }
                guard (200..<300).contains(status) else {
                    lastError = "HTTP \(status)"
                    continue
                }
                reachable = true
                models = parseSuitableModels(body)
                if !models.isEmpty { break }
            } catch {
                if Task.isCancelled { break }
                lastError = failureReason(error)
            }
        }

        if !reachable, unauthorized == nil, !Task.isCancelled, let url = URL(string: chatEndpoint(for: base)) {
            let named = model.trimmingCharacters(in: .whitespacesAndNewlines)
            let probeModel = named.isEmpty
                ? (TranslationProviderPreset.preset(id: presetID).suggestedModels.first ?? fallbackProbeModel)
                : named
            do {
                let body = probeBody(model: probeModel)
                let (_, status) = try await fetch(request(url, method: "POST", apiKey: apiKey, body: body))
                if status == 401 || status == 403 {
                    unauthorized = status
                } else if (200..<300).contains(status) {
                    reachable = true
                } else {
                    lastError = "HTTP \(status)"
                }
            } catch {
                if !Task.isCancelled { lastError = failureReason(error) }
            }
        }

        if Task.isCancelled { return .cancelled }
        if let unauthorized { return .unauthorized(status: unauthorized) }
        guard reachable else { return .unreachable(reason: lastError ?? engineString("Zaman aşımı")) }
        let elapsed = clock.now - started
        let ms = Int(elapsed.components.seconds * 1000 + elapsed.components.attoseconds / 1_000_000_000_000_000)
        return .reachable(host: host, latencyMS: max(1, ms), models: models)
    }

    /// What the user is told about a failed request: the system's description, except for a timeout (the commonest
    /// failure when the address is wrong), whose text follows the app's language.
    private static func failureReason(_ error: any Error) -> String {
        (error as? URLError)?.code == .timedOut ? engineString("Zaman aşımı") : error.localizedDescription
    }

    /// A `Fetch` over `URLSession` (ephemeral by default: no cache or cookies, so a key is never kept by the session).
    public static func urlSessionFetch(session: URLSession = URLSession(configuration: .ephemeral)) -> Fetch {
        { request in
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
            return (data, http.statusCode)
        }
    }
}
