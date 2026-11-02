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

    /// Trims the address, drops trailing slashes, adds a scheme when there is none and strips one API suffix. `nil`
    /// when nothing usable (no host) is left.
    ///
    /// An address typed without a scheme gets `http://` when its host is on this computer or the local network (see
    /// `isLocalNetworkHost(_:)`; LM Studio, Ollama and friends rarely have TLS) and `https://` otherwise, so that
    /// `api.openai.com/v1` never sends its bearer token in cleartext. A scheme the user typed is kept as it is.
    public static func normalizeBaseURL(_ raw: String) -> String? {
        var url = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while url.hasSuffix("/") { url.removeLast() }
        // What is left of a bare "http://" once its slashes are gone.
        guard !url.isEmpty, !["http:", "https:"].contains(url.lowercased()) else { return nil }
        let lower = url.lowercased()
        if !lower.hasPrefix("http://") && !lower.hasPrefix("https://") {
            // Judge the host by parsing it as if it were plain HTTP; an unparsable one stays "http://" and fails below.
            let host = URL(string: "http://" + url)?.host()
            url = (host.map(isLocalNetworkHost) ?? true ? "http://" : "https://") + url
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

    /// Whether `host` (a URL host, IPv6 without brackets) names this computer or a machine on the local network:
    /// `localhost`, `.local` / `.lan` / `.home.arpa` names, a bare host name (no dot), loopback, link-local and private
    /// IPv4 ranges (including the 100.64/10 block overlay networks such as Tailscale use) and IPv6 loopback, link-local
    /// and unique-local addresses. Anything else is taken to be on the internet.
    public static func isLocalNetworkHost(_ host: String) -> Bool {
        let host = host.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        if host.isEmpty { return false }
        if host.contains(":") {
            return host == "::1" || host.hasPrefix("fe80:") || host.hasPrefix("fc") || host.hasPrefix("fd")
        }
        let octets = host.split(separator: ".", omittingEmptySubsequences: false)
        if octets.count == 4, octets.allSatisfy({ !$0.isEmpty && $0.allSatisfy(\.isASCII) && $0.allSatisfy(\.isNumber) }) {
            let n = octets.compactMap { Int($0) }
            guard n.count == 4, n.allSatisfy({ (0...255).contains($0) }) else { return false }
            switch (n[0], n[1]) {
            case (10, _), (127, _), (192, 168), (169, 254): return true
            case (172, 16...31), (100, 64...127): return true
            default: return false
            }
        }
        if !host.contains(".") { return true }
        return ["localhost", "local", "lan"].contains { host.hasSuffix("." + $0) } || host.hasSuffix(".home.arpa")
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

    /// The session every default `Fetch` shares: ephemeral (no cache, cookies or credential storage, so a key is never
    /// kept by the session) and created once, because a `URLSession` holds connections and threads until it is
    /// invalidated and the settings window builds a `Fetch` per view model.
    static let sharedSession = URLSession(configuration: .ephemeral)

    /// A `Fetch` over `URLSession`: the shared ephemeral session unless one is given (tests pass a stubbed one, which
    /// the caller owns).
    public static func urlSessionFetch(session: URLSession? = nil) -> Fetch {
        let session = session ?? sharedSession
        return { request in
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
            return (data, http.statusCode)
        }
    }
}
