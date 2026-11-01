import Foundation

/// Helpers for the external LAN LLM server: address normalization and the connection test. The engine has no
/// test command, so the app probes the server itself over plain HTTP (only on an explicit user action).
public enum ExternalLLMServer {
    /// Fetches a URL and returns the body; throws on a transport error or a non-2xx status.
    public typealias Fetch = @Sendable (URL, TimeInterval) async throws -> Data

    /// Per-request timeout of the connection test.
    public static let timeout: TimeInterval = 3.5

    /// Endpoints tried in order: Ollama first, then the OpenAI-compatible list.
    static let modelEndpoints = ["/api/tags", "/v1/models"]

    /// Suffixes pasted by users from client configs; one of them is stripped from the end of the address.
    private static let strippedSuffixes = [
        "/v1/chat/completions", "/chat/completions", "/api/chat", "/api/tags", "/v1/models", "/v1",
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

    public enum ConnectionResult: Sendable, Equatable {
        /// The address is blank or can't form a URL.
        case invalidAddress
        /// No endpoint answered; `reason` is the last error's description.
        case unreachable(reason: String)
        /// At least one endpoint answered. `models` can be empty (the server is up but lists nothing suitable).
        case reachable(host: String, latencyMS: Int, models: [String])
        /// The probe was cancelled (for example the Settings window closed); says nothing about the server.
        case cancelled

        public var models: [String] {
            if case .reachable(_, _, let models) = self { models } else { [] }
        }
    }

    /// Probes `GET {base}/api/tags`, then `GET {base}/v1/models` (3.5 s each) and stops at the first that lists
    /// suitable models. `latencyMS` covers all attempts and is at least 1.
    public static func testConnection(
        rawURL: String, fetch: Fetch = urlSessionFetch(), clock: ContinuousClock = ContinuousClock()
    ) async -> ConnectionResult {
        guard let base = normalizeBaseURL(rawURL), let host = URL(string: base)?.host() else {
            return .invalidAddress
        }
        let started = clock.now
        var models: [String] = []
        var reachable = false
        var lastError: String?
        for endpoint in modelEndpoints {
            guard let url = URL(string: base + endpoint) else { continue }
            do {
                let body = try await fetch(url, timeout)
                reachable = true
                models = parseSuitableModels(body)
                if !models.isEmpty { break }
            } catch {
                if Task.isCancelled { break }
                lastError = error.localizedDescription
            }
        }
        if Task.isCancelled { return .cancelled }
        guard reachable else { return .unreachable(reason: lastError ?? engineString("Zaman aşımı")) }
        let elapsed = clock.now - started
        let ms = Int(elapsed.components.seconds * 1000 + elapsed.components.attoseconds / 1_000_000_000_000_000)
        return .reachable(host: host, latencyMS: max(1, ms), models: models)
    }

    /// A `Fetch` over `URLSession` (ephemeral by default: no cache or cookies for a LAN probe).
    public static func urlSessionFetch(session: URLSession = URLSession(configuration: .ephemeral)) -> Fetch {
        { url, timeout in
            var request = URLRequest(url: url, timeoutInterval: timeout)
            request.httpMethod = "GET"
            let (data, response) = try await session.data(for: request)
            if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                throw URLError(.badServerResponse, userInfo: [
                    NSLocalizedDescriptionKey: "HTTP \(http.statusCode)",
                ])
            }
            return data
        }
    }
}
