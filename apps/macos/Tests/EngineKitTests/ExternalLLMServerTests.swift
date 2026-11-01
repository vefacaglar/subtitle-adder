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
        #expect(normalize("host:1/v1") == "http://host:1")
        #expect(normalize("http://host:1/") == "http://host:1")
        // Only one suffix is stripped.
        #expect(normalize("host:1/v1/v1") == "http://host:1/v1")
        #expect(normalize("") == nil)
        #expect(normalize("   ") == nil)
        #expect(normalize("http://") == nil)
    }

    @Test func parsesOllamaAndOpenAIModelLists() {
        let ollama = Data(#"{"models": [{"name": "gemma4:e4b"}, {"name": "nomic-embed-text:latest"}, {"name": "qwen3:14b"}]}"#.utf8)
        #expect(ExternalLLMServer.parseSuitableModels(ollama) == ["gemma4:e4b", "qwen3:14b"])
        let openAI = Data(#"{"object": "list", "data": [{"id": "qwen/qwen3-8b"}, {"id": "all-MiniLM-L6"}, {"id": "bge-m3"}, {"id": " QWEN/qwen3-8b "}, {"name": "named"}, {"id": ""}]}"#.utf8)
        #expect(ExternalLLMServer.parseSuitableModels(openAI) == ["qwen/qwen3-8b", "named"])
        #expect(ExternalLLMServer.parseSuitableModels(Data("[]".utf8)).isEmpty)
        #expect(ExternalLLMServer.parseSuitableModels(Data("nope".utf8)).isEmpty)
    }

    /// Records the URLs a fake fetch was asked for and answers from a table.
    private final class Recorder: @unchecked Sendable {
        private let lock = NSLock()
        private(set) var requests: [String] = []
        private(set) var timeouts: [TimeInterval] = []
        func record(_ url: URL, _ timeout: TimeInterval) {
            lock.lock()
            requests.append(url.absoluteString)
            timeouts.append(timeout)
            lock.unlock()
        }
    }

    private func fetch(_ recorder: Recorder, _ responses: [String: String]) -> ExternalLLMServer.Fetch {
        { url, timeout in
            recorder.record(url, timeout)
            guard let body = responses[url.path] else { throw URLError(.cannotConnectToHost) }
            return Data(body.utf8)
        }
    }

    @Test func stopsAtFirstEndpointWithModels() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "192.168.1.50:11434/api/chat",
            fetch: fetch(recorder, ["/api/tags": #"{"models": [{"name": "gemma4:e4b"}]}"#])
        )
        guard case .reachable(let host, let latency, let models) = result else { Issue.record("\(result)"); return }
        #expect(host == "192.168.1.50")
        #expect(latency >= 1)
        #expect(models == ["gemma4:e4b"])
        #expect(recorder.requests == ["http://192.168.1.50:11434/api/tags"])
        #expect(recorder.timeouts == [3.5])
    }

    @Test func fallsBackToOpenAIModelsEndpoint() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "http://mac.local:1234",
            fetch: fetch(recorder, ["/v1/models": #"{"data": [{"id": "qwen3-8b"}]}"#])
        )
        #expect(result.models == ["qwen3-8b"])
        #expect(recorder.requests == ["http://mac.local:1234/api/tags", "http://mac.local:1234/v1/models"])
    }

    @Test func reachableWithoutSuitableModels() async {
        let recorder = Recorder()
        let result = await ExternalLLMServer.testConnection(
            rawURL: "host:1",
            fetch: fetch(recorder, ["/api/tags": #"{"models": [{"name": "nomic-embed-text"}]}"#, "/v1/models": #"{"data": []}"#])
        )
        guard case .reachable(_, _, let models) = result else { Issue.record("\(result)"); return }
        #expect(models.isEmpty)
        #expect(recorder.requests.count == 2)
    }

    @Test func unreachableAndInvalid() async {
        let recorder = Recorder()
        let down = await ExternalLLMServer.testConnection(rawURL: "host:1", fetch: fetch(recorder, [:]))
        guard case .unreachable(let reason) = down else { Issue.record("\(down)"); return }
        #expect(!reason.isEmpty)
        #expect(recorder.requests.count == 2)
        let blank = await ExternalLLMServer.testConnection(rawURL: "  ", fetch: fetch(recorder, [:]))
        #expect(blank == .invalidAddress)
        #expect(recorder.requests.count == 2)
    }

    @Test func aCancelledProbeIsNotReportedAsUnreachable() async throws {
        let probe = Task {
            await ExternalLLMServer.testConnection(rawURL: "host:1", fetch: { _, _ in
                try await Task.sleep(for: .seconds(30))
                return Data()
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

    @Test func urlSessionFetchTreatsNon2xxAsFailure() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        StubProtocol.responses = [
            "http://stub.test:1/api/tags": (404, "no"),
            "http://stub.test:1/v1/models": (200, #"{"data": [{"id": "m1"}]}"#),
        ]
        let fetch = ExternalLLMServer.urlSessionFetch(session: URLSession(configuration: configuration))
        let result = await ExternalLLMServer.testConnection(rawURL: "stub.test:1", fetch: fetch)
        #expect(result.models == ["m1"])
    }
}
