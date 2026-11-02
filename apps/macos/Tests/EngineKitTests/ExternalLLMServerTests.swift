import Foundation
import Testing
@testable import EngineKit

@Suite struct ExternalLLMServerTests {
    @Test func normalizesAddresses() {
        let normalize = ExternalLLMServer.normalizeBaseURL
        #expect(normalize("192.168.1.50:1234") == "http://192.168.1.50:1234")
        #expect(normalize("  http://mac-studio.local:11434/api/chat  ") == "http://mac-studio.local:11434")
        #expect(normalize("192.168.1.50:1234/v1/chat/completions") == "http://192.168.1.50:1234")
        #expect(normalize("HTTPS://host.lan/V1/Models/") == "HTTPS://host.lan")
        #expect(normalize("host:1/chat/completions") == "http://host:1")
        #expect(normalize("host:1/api/tags") == "http://host:1")
        #expect(normalize("http://host:1/") == "http://host:1")
        // A bare "/v1" is part of the address: providers such as Groq nest their API under it.
        #expect(normalize("host:1/v1") == "http://host:1/v1")
        #expect(normalize("host:1/v1/v1") == "http://host:1/v1/v1")
        // Only one suffix is stripped.
        #expect(normalize("host:1/chat/completions/chat/completions") == "http://host:1/chat/completions")
        #expect(normalize("") == nil)
        #expect(normalize("   ") == nil)
        #expect(normalize("http://") == nil)
    }

    @Test func aPublicAddressTypedWithoutASchemeGetsHTTPSSoTheKeyIsNeverSentInTheClear() {
        let normalize = ExternalLLMServer.normalizeBaseURL
        #expect(normalize("api.openai.com/v1") == "https://api.openai.com/v1")
        #expect(normalize("api.openai.com") == "https://api.openai.com")
        #expect(normalize("  api.groq.com/openai/v1/chat/completions ") == "https://api.groq.com/openai/v1")
        #expect(normalize("openrouter.ai/api/v1/models") == "https://openrouter.ai/api/v1")
        #expect(normalize("llm.example.com:8443") == "https://llm.example.com:8443")
        #expect(normalize("8.8.8.8:1234") == "https://8.8.8.8:1234")
        // 172.32 is outside 172.16/12 and 100.128 outside 100.64/10: those are public addresses.
        #expect(normalize("172.32.0.1:80") == "https://172.32.0.1:80")
        #expect(normalize("100.128.0.1") == "https://100.128.0.1")
    }

    @Test func localAddressesTypedWithoutASchemeKeepPlainHTTP() {
        let normalize = ExternalLLMServer.normalizeBaseURL
        #expect(normalize("localhost:11434") == "http://localhost:11434")
        #expect(normalize("127.0.0.1:1234/v1") == "http://127.0.0.1:1234/v1")
        #expect(normalize("[::1]:8080") == "http://[::1]:8080")
        #expect(normalize("10.0.0.2:8080") == "http://10.0.0.2:8080")
        #expect(normalize("172.16.5.5:1234") == "http://172.16.5.5:1234")
        #expect(normalize("100.100.1.1:1234") == "http://100.100.1.1:1234")
        #expect(normalize("mac-studio.local:11434") == "http://mac-studio.local:11434")
        #expect(normalize("nas.lan") == "http://nas.lan")
        #expect(normalize("my-mac:1234") == "http://my-mac:1234")
    }

    @Test func aSchemeTheUserTypedIsNeverChanged() {
        let normalize = ExternalLLMServer.normalizeBaseURL
        #expect(normalize("http://api.openai.com/v1") == "http://api.openai.com/v1")
        #expect(normalize("https://localhost:1234") == "https://localhost:1234")
        #expect(normalize("HTTP://example.com") == "HTTP://example.com")
    }

    @Test func aBareAddressWithoutAHostIsStillRejected() {
        #expect(ExternalLLMServer.normalizeBaseURL("/v1") == nil)
        #expect(ExternalLLMServer.normalizeBaseURL("a b") == nil)
        #expect(ExternalLLMServer.normalizeBaseURL("https://") == nil)
    }

    @Test func presetAddressesSurviveNormalization() {
        for preset in TranslationProviderPreset.all where !preset.baseURL.isEmpty {
            #expect(ExternalLLMServer.normalizeBaseURL(preset.baseURL) == preset.baseURL, "\(preset.id)")
        }
    }

    @Test func pastedEndpointsKeepTheVersionPathOfNestedProviders() {
        let normalize = ExternalLLMServer.normalizeBaseURL
        // The engine appends "/chat/completions" to ".../openai/v1"; ".../openai" alone would be another endpoint.
        #expect(normalize("https://api.groq.com/openai/v1/chat/completions") == "https://api.groq.com/openai/v1")
        #expect(normalize("https://api.groq.com/openai/v1/models/") == "https://api.groq.com/openai/v1")
        #expect(normalize("https://openrouter.ai/api/v1/chat/completions") == "https://openrouter.ai/api/v1")
        #expect(normalize("https://opencode.ai/zen/go/v1/models") == "https://opencode.ai/zen/go/v1")
        let gemini = "https://generativelanguage.googleapis.com/v1beta/openai"
        #expect(normalize(gemini + "/chat/completions") == gemini)
        // At the root of a server the version is implied and dropped, as before.
        #expect(normalize("https://api.openai.com/v1/chat/completions") == "https://api.openai.com")
    }

    @Test func parsesOllamaAndOpenAIModelLists() {
        let ollama = Data(#"{"models": [{"name": "gemma4:e4b"}, {"name": "nomic-embed-text:latest"}, {"name": "qwen3:14b"}]}"#.utf8)
        #expect(ExternalLLMServer.parseSuitableModels(ollama) == ["gemma4:e4b", "qwen3:14b"])
        let openAI = Data(#"{"object": "list", "data": [{"id": "qwen/qwen3-8b"}, {"id": "all-MiniLM-L6"}, {"id": "bge-m3"}, {"id": " QWEN/qwen3-8b "}, {"name": "named"}, {"id": ""}]}"#.utf8)
        #expect(ExternalLLMServer.parseSuitableModels(openAI) == ["qwen/qwen3-8b", "named"])
        #expect(ExternalLLMServer.parseSuitableModels(Data("[]".utf8)).isEmpty)
        #expect(ExternalLLMServer.parseSuitableModels(Data("nope".utf8)).isEmpty)
    }

    // MARK: Endpoints

    @Test func modelEndpointsFollowTheShapeOfTheAddress() {
        let endpoints = ExternalLLMServer.modelEndpoints
        // A bare server: OpenAI's list, the unprefixed one, then Ollama's own.
        #expect(endpoints("http://host:1") == ["http://host:1/v1/models", "http://host:1/models", "http://host:1/api/tags"])
        #expect(endpoints("http://host:1/") == endpoints("http://host:1"))
        // OpenAI-style roots have a single candidate.
        #expect(endpoints("https://api.openai.com/v1") == ["https://api.openai.com/v1/models"])
        #expect(endpoints("https://api.groq.com/openai/v1") == ["https://api.groq.com/openai/v1/models"])
        let gemini = "https://generativelanguage.googleapis.com/v1beta/openai"
        #expect(endpoints(gemini) == [gemini + "/models"])
        #expect(endpoints("HTTPS://X.test/V1") == ["HTTPS://X.test/V1/models"])
        // A pasted chat endpoint is reduced to its root, a pasted list is the list.
        #expect(endpoints("https://x.test/v1/chat/completions") == ["https://x.test/v1/models"])
        #expect(endpoints("https://x.test/chat/completions") == endpoints("https://x.test"))
        #expect(endpoints("https://x.test/v1beta/models") == ["https://x.test/v1beta/models"])
    }

    @Test func chatEndpointFollowsTheEnginesRule() {
        let chat = ExternalLLMServer.chatEndpoint
        #expect(chat("http://host:1") == "http://host:1/v1/chat/completions")
        #expect(chat("http://host:1/") == "http://host:1/v1/chat/completions")
        #expect(chat("https://api.openai.com/v1") == "https://api.openai.com/v1/chat/completions")
        #expect(chat("https://api.groq.com/openai/v1") == "https://api.groq.com/openai/v1/chat/completions")
        let gemini = "https://generativelanguage.googleapis.com/v1beta/openai"
        #expect(chat(gemini) == gemini + "/chat/completions")
        #expect(chat("https://x.test/v1/chat/completions") == "https://x.test/v1/chat/completions")
    }

    // MARK: Connection test

    /// Records the requests a fake fetch was asked for and answers from a table of `path -> (status, body)`.
    private final class Recorder: @unchecked Sendable {
        private let lock = NSLock()
        private var all: [URLRequest] = []
        func record(_ request: URLRequest) {
            lock.lock()
            all.append(request)
            lock.unlock()
        }
        var requests: [URLRequest] {
            lock.lock()
            defer { lock.unlock() }
            return all
        }
        var urls: [String] { requests.map { $0.url?.absoluteString ?? "" } }
        var methods: [String] { requests.map { $0.httpMethod ?? "" } }
    }

    /// Unlisted paths answer 404; with `down` every request fails at the transport level.
    private func fetch(
        _ recorder: Recorder, _ responses: [String: (Int, String)] = [:], down: (any Error)? = nil
    ) -> ExternalLLMServer.Fetch {
        { request in
            recorder.record(request)
            if let down { throw down }
            let (status, body) = responses[request.url?.path ?? ""] ?? (404, "")
            return (Data(body.utf8), status)
        }
    }

    private let openAIList = (200, #"{"object": "list", "data": [{"id": "qwen3-8b"}, {"id": "gemma-3"}]}"#)

    @Test func stopsAtFirstEndpointWithModels() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "192.168.1.50:11434/api/chat", fetch: fetch(recorder, ["/v1/models": openAIList])
        )
        guard case .reachable(let host, let latency, let models) = result else { Issue.record("\(result)"); return }
        #expect(host == "192.168.1.50")
        #expect(latency >= 1)
        #expect(models == ["qwen3-8b", "gemma-3"])
        #expect(recorder.urls == ["http://192.168.1.50:11434/v1/models"])
        #expect(recorder.requests.map(\.timeoutInterval) == [8])
    }

    @Test func fallsBackToOllamasOwnList() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "http://mac.local:11434",
            fetch: fetch(recorder, ["/api/tags": (200, #"{"models": [{"name": "gemma4:e4b"}]}"#)])
        )
        #expect(result.models == ["gemma4:e4b"])
        #expect(recorder.urls == [
            "http://mac.local:11434/v1/models", "http://mac.local:11434/models", "http://mac.local:11434/api/tags",
        ])
        #expect(recorder.methods == ["GET", "GET", "GET"])
    }

    @Test func anOpenAIRootIsAskedOnlyForItsModelList() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "https://api.groq.com/openai/v1", apiKey: "gsk-1",
            fetch: fetch(recorder, ["/openai/v1/models": openAIList])
        )
        #expect(result.models == ["qwen3-8b", "gemma-3"])
        #expect(recorder.urls == ["https://api.groq.com/openai/v1/models"])
    }

    @Test func everyRequestCarriesTheHeadersAndTheKeyOnlyWhenThereIsOne() async throws {
        let recorder = Recorder()
        _ = await ExternalLLMServer.testConnection(rawURL: "https://x.test/v1", apiKey: "  sk-secret \n", fetch: fetch(recorder))
        // The model list is not exposed, so the chat probe follows: every request is signed.
        #expect(recorder.requests.count == 2)
        for request in recorder.requests {
            #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer sk-secret")
            #expect(request.value(forHTTPHeaderField: "User-Agent") == "SubtitleAdder/1.0")
            #expect(request.value(forHTTPHeaderField: "Accept") == "application/json")
            #expect(request.timeoutInterval == 8)
        }

        let keyless = Recorder()
        _ = await ExternalLLMServer.testConnection(rawURL: "http://host:1", apiKey: "   ", fetch: fetch(keyless))
        #expect(keyless.requests.count == 4)
        for request in keyless.requests {
            #expect(request.value(forHTTPHeaderField: "Authorization") == nil)
            #expect(request.value(forHTTPHeaderField: "User-Agent") == "SubtitleAdder/1.0")
        }
    }

    @Test func aGatewayWithoutAModelListIsProbedWithOneChatRequest() async throws {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "https://opencode.ai/inference/go/openai/v1", apiKey: "sk-oc", model: "kimi-k3.1-pro",
            presetID: "opencode-go",
            fetch: fetch(recorder, ["/inference/go/openai/v1/chat/completions": (200, #"{"choices": []}"#)])
        )
        guard case .reachable(let host, _, let models) = result else { Issue.record("\(result)"); return }
        #expect(host == "opencode.ai")
        #expect(models.isEmpty)
        #expect(recorder.urls == [
            "https://opencode.ai/inference/go/v1/models", "https://opencode.ai/inference/go/openai/v1/chat/completions",
        ])
        #expect(recorder.methods == ["GET", "POST"])
        let probe = recorder.requests[1]
        #expect(probe.value(forHTTPHeaderField: "Authorization") == "Bearer sk-oc")
        #expect(probe.value(forHTTPHeaderField: "Content-Type") == "application/json")
        let body = try #require(probe.httpBody)
        let json = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
        #expect(json["model"] as? String == "kimi-k3.1-pro")
        #expect(json["max_tokens"] as? Int == 1)
        #expect(json["stream"] as? Bool == false)
        let messages = try #require(json["messages"] as? [[String: String]])
        #expect(messages == [["role": "user", "content": "hi"]])
    }

    // MARK: OpenCode Go v2

    @Test func recognizesTheFiveOpenCodeGoAddresses() {
        for url in [
            "https://opencode.ai/zen/go/v1", "https://opencode.ai/zen/go/v1/chat/completions",
            "https://opencode.ai/inference/go/v1", "https://opencode.ai/inference/go/openai/v1",
            "https://opencode.ai/inference/go/openai/v1/chat/completions",
            "https://OpenCode.ai/inference/go/openai/v1/",
        ] {
            #expect(ExternalLLMServer.isOpenCodeGo(url), "\(url)")
        }
    }

    @Test func otherAddressesAreNotOpenCodeGo() {
        for url in [
            "http://opencode.ai/inference/go/openai/v1", "https://opencode.ai/v1", "https://opencode.ai/zen/go/v1?x=1",
            "https://opencode.ai/zen/go/v1#top", "https://opencode.ai:8443/zen/go/v1",
            "https://proxy.example.com/inference/go/openai/v1", "https://api.openai.com/v1", "not a url", "",
        ] {
            #expect(!ExternalLLMServer.isOpenCodeGo(url), "\(url)")
        }
        // A non-OpenCode /v1 proxy keeps the generic routes.
        #expect(ExternalLLMServer.modelEndpoints(for: "https://proxy.example.com/zen/go/v1")
            == ["https://proxy.example.com/zen/go/v1/models"])
        #expect(ExternalLLMServer.chatEndpoint(for: "https://proxy.example.com/zen/go/v1")
            == "https://proxy.example.com/zen/go/v1/chat/completions")
    }

    @Test func everyOpenCodeGoAddressUsesTheTwoV2Routes() {
        for url in [
            "https://opencode.ai/zen/go/v1", "https://opencode.ai/zen/go/v1/chat/completions",
            "https://opencode.ai/inference/go/v1", "https://opencode.ai/inference/go/openai/v1",
            "https://opencode.ai/inference/go/openai/v1/chat/completions",
        ] {
            #expect(ExternalLLMServer.modelEndpoints(for: url) == ["https://opencode.ai/inference/go/v1/models"], "\(url)")
            #expect(
                ExternalLLMServer.chatEndpoint(for: url) == "https://opencode.ai/inference/go/openai/v1/chat/completions",
                "\(url)"
            )
        }
    }

    @Test func theSessionHeaderIsTheSameOnDiscoveryAndChatAndAbsentElsewhere() async {
        let recorder = Recorder()
        _ = await ExternalLLMServer.testConnection(
            rawURL: "https://opencode.ai/zen/go/v1", apiKey: "sk-oc", fetch: fetch(recorder)
        )
        #expect(recorder.requests.count == 2)
        let sessions = recorder.requests.map { $0.value(forHTTPHeaderField: "x-opencode-session") }
        #expect(sessions[0] == ExternalLLMServer.openCodeSessionID)
        #expect(sessions[0] == sessions[1])
        #expect(UUID(uuidString: sessions[0] ?? "") != nil)

        let other = Recorder()
        _ = await ExternalLLMServer.testConnection(rawURL: "https://x.test/v1", apiKey: "k", fetch: fetch(other))
        for request in other.requests { #expect(request.value(forHTTPHeaderField: "x-opencode-session") == nil) }
    }

    @Test func gatewayErrorMessagesAreReadFromTheBody() {
        let detail = ExternalLLMServer.httpErrorDetail(status:body:apiKey:)
        #expect(detail(429, Data(#"{"error": {"message": "Rate limited,\n slow  down"}}"#.utf8), "")
            == "HTTP 429: Rate limited, slow down")
        #expect(detail(402, Data(#"{"error": "Out of credits"}"#.utf8), "") == "HTTP 402: Out of credits")
        #expect(detail(500, Data(#"{"message": "boom"}"#.utf8), "") == "HTTP 500: boom")
        // HTML, empty or non-string messages give just the status.
        #expect(detail(502, Data("<html><body>Bad gateway</body></html>".utf8), "") == "HTTP 502")
        #expect(detail(503, Data(), "") == "HTTP 503")
        #expect(detail(400, Data(#"{"error": {"message": 5}}"#.utf8), "") == "HTTP 400")
        #expect(detail(400, Data(#"{"error": {"message": "  "}}"#.utf8), "") == "HTTP 400")
        // The key is redacted when present, and the text is capped.
        #expect(detail(401, Data(#"{"error": {"message": "bad key sk-secret"}}"#.utf8), " sk-secret ")
            == "HTTP 401: bad key [redacted]")
        let long = Data(("{\"error\": \"" + String(repeating: "x", count: 3000) + "\"}").utf8)
        #expect(detail(500, long, "").count == 1000)
    }

    @Test func aFailedRequestReportsTheGatewayMessage() async {
        let result = await ExternalLLMServer.testConnection(
            rawURL: "https://opencode.ai/inference/go/openai/v1", apiKey: "sk-oc",
            fetch: fetch(Recorder(), [
                "/inference/go/v1/models": (500, #"{"error": {"message": "upstream down"}}"#),
                "/inference/go/openai/v1/chat/completions": (503, #"{"error": {"message": "model sk-oc offline"}}"#),
            ])
        )
        #expect(result == .unreachable(reason: "HTTP 503: model [redacted] offline"))
    }

    @Test func theProbeModelIsTheChosenOneThenThePresetsFirstThenAGenericOne() async throws {
        func probedModel(model: String, presetID: String?) async throws -> String? {
            let recorder = Recorder()
            _ = await ExternalLLMServer.testConnection(
                rawURL: "http://host:1", model: model, presetID: presetID, fetch: fetch(recorder, ["/v1/chat/completions": (200, "{}")])
            )
            let body = try #require(recorder.requests.last?.httpBody)
            return (try JSONSerialization.jsonObject(with: body) as? [String: Any])?["model"] as? String
        }
        #expect(try await probedModel(model: " glm-5 ", presetID: "deepseek") == "glm-5")
        #expect(try await probedModel(model: "", presetID: "deepseek") == "deepseek-chat")
        #expect(try await probedModel(model: "", presetID: "custom") == "gpt-4o-mini")
        #expect(try await probedModel(model: "", presetID: nil) == "gpt-4o-mini")
        #expect(try await probedModel(model: "", presetID: "no-such-preset") == "gpt-4o-mini")
    }

    @Test func aBareServersProbeGoesToTheV1ChatEndpoint() async {
        let recorder = Recorder()
        _ = await ExternalLLMServer.testConnection(rawURL: "host:1234", fetch: fetch(recorder))
        #expect(recorder.urls.last == "http://host:1234/v1/chat/completions")
        #expect(recorder.methods == ["GET", "GET", "GET", "POST"])
    }

    @Test func aRefusedKeyStopsTheTestAtOnce() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "http://host:1", apiKey: "bad", fetch: fetch(recorder, ["/v1/models": (401, #"{"error": "no"}"#)])
        )
        #expect(result == .unauthorized(status: 401))
        #expect(recorder.requests.count == 1)

        // A later endpoint answering 403 also stops the test; no chat probe follows.
        let second = Recorder()
        let forbidden = await ExternalLLMServer.testConnection(
            rawURL: "http://host:1", fetch: fetch(second, ["/models": (403, "")])
        )
        #expect(forbidden == .unauthorized(status: 403))
        #expect(second.urls == ["http://host:1/v1/models", "http://host:1/models"])
    }

    @Test func theChatProbeCanRefuseTheKeyToo() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "https://x.test/v1", apiKey: "bad", fetch: fetch(recorder, ["/v1/chat/completions": (401, "")])
        )
        #expect(result == .unauthorized(status: 401))
        #expect(recorder.methods == ["GET", "POST"])
    }

    @Test func theRefusalMessageIsLocalized() {
        #expect(ExternalLLMServer.unauthorizedMessage(status: 401, language: .tr) == "API anahtarı geçersiz veya yetkisiz (HTTP 401).")
        #expect(ExternalLLMServer.unauthorizedMessage(status: 403, language: .en) == "Invalid or unauthorized API key (HTTP 403).")
    }

    @Test func reachableWithoutSuitableModels() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "host:1",
            fetch: fetch(recorder, ["/v1/models": (200, #"{"data": [{"id": "nomic-embed-text"}]}"#), "/api/tags": (200, #"{"models": []}"#)])
        )
        guard case .reachable(_, _, let models) = result else { Issue.record("\(result)"); return }
        #expect(models.isEmpty)
        // The server answered, so there is no chat probe.
        #expect(recorder.methods == ["GET", "GET", "GET"])
    }

    @Test func unreachableAndInvalid() async {
        let recorder = Recorder()
        let down = await ExternalLLMServer.testConnection(rawURL: "host:1", fetch: fetch(recorder, down: URLError(.cannotConnectToHost)))
        guard case .unreachable(let reason) = down else { Issue.record("\(down)"); return }
        #expect(!reason.isEmpty)
        #expect(recorder.requests.count == 4)
        let blank = await ExternalLLMServer.testConnection(rawURL: "  ", fetch: fetch(recorder, down: URLError(.cannotConnectToHost)))
        #expect(blank == .invalidAddress)
        #expect(recorder.requests.count == 4)
    }

    @Test func anErrorStatusEverywhereIsReportedAsUnreachable() async {
        let result = await ExternalLLMServer.testConnection(
            rawURL: "https://x.test/v1", fetch: fetch(Recorder(), ["/v1/models": (500, ""), "/v1/chat/completions": (404, "")])
        )
        #expect(result == .unreachable(reason: "HTTP 404"))
    }

    @Test func aTimeoutIsReportedInTheAppsLanguage() async {
        let result = await ExternalLLMServer.testConnection(rawURL: "host:1", fetch: fetch(Recorder(), down: URLError(.timedOut)))
        guard case .unreachable(let reason) = result else { Issue.record("\(result)"); return }
        #expect(reason == "Zaman aşımı" || reason == "Timed out")
    }

    @Test func aCancelledProbeIsNotReportedAsUnreachable() async throws {
        let probe = Task {
            await ExternalLLMServer.testConnection(rawURL: "host:1", fetch: { _ in
                try await Task.sleep(for: .seconds(30))
                return (Data(), 200)
            })
        }
        try await Task.sleep(for: .milliseconds(50))
        probe.cancel()
        #expect(await probe.value == .cancelled)
    }

    // MARK: URLSession-backed fetch

    private final class StubProtocol: URLProtocol, @unchecked Sendable {
        nonisolated(unsafe) static var responses: [String: (Int, String)] = [:]
        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
        override func startLoading() {
            let (status, body) = Self.responses[request.url?.absoluteString ?? ""] ?? (404, "")
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }
        override func stopLoading() {}
    }

    @Test func urlSessionFetchReturnsTheStatusInsteadOfThrowing() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        StubProtocol.responses = [
            "http://stub.test:1/v1/models": (404, "no"),
            "http://stub.test:1/models": (401, "denied"),
            "http://stub.test:1/api/tags": (200, #"{"models": [{"name": "m1"}]}"#),
        ]
        let fetch = ExternalLLMServer.urlSessionFetch(session: URLSession(configuration: configuration))
        let missing = try await fetch(ExternalLLMServer.request(try #require(URL(string: "http://stub.test:1/v1/models")), apiKey: ""))
        #expect(missing.status == 404 && missing.data == Data("no".utf8))
        let denied = try await fetch(ExternalLLMServer.request(try #require(URL(string: "http://stub.test:1/models")), apiKey: "k"))
        #expect(denied.status == 401)

        // The whole test over a real URLSession: only the last endpoint lists models.
        StubProtocol.responses["http://stub.test:1/models"] = (404, "")
        let result = await ExternalLLMServer.testConnection(rawURL: "http://stub.test:1", fetch: fetch)
        #expect(result.models == ["m1"])
    }

    @Test func theDefaultFetchSharesOneEphemeralSession() {
        // Every default `Fetch` runs over the same session: building one per call leaked a session (and its
        // connections) each time.
        let session = ExternalLLMServer.sharedSession
        #expect(session === ExternalLLMServer.sharedSession)
        // Ephemeral: caches live in memory only, nothing about a request (the key) reaches the disk.
        #expect(session.configuration.urlCache?.diskCapacity == 0)
    }
}
