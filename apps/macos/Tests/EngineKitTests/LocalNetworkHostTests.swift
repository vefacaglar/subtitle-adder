import Testing
@testable import EngineKit

/// Whether a translation server keeps the text on this computer / network or sends it over the internet.
@Suite struct LocalNetworkHostTests {
    @Test func localHostsAreRecognized() {
        for host in [
            "localhost", "127.0.0.1", "127.8.9.10", "10.0.0.2", "192.168.1.50", "172.16.0.1", "172.31.255.255",
            "169.254.3.4", "100.64.0.1", "100.127.1.1", "mac-mini", "mac.local", "Mac.LOCAL", "nas.lan", "box.home.arpa",
            "app.localhost", "::1", "fe80::1", "fd12:3456::1", "fc00::2", "[::1]",
        ] {
            #expect(ExternalLLMServer.isLocalNetworkHost(host), "\(host) should be local")
        }
    }

    @Test func internetHostsAreNot() {
        for host in [
            "", "api.openai.com", "openrouter.ai", "generativelanguage.googleapis.com", "8.8.8.8", "172.32.0.1",
            "172.15.0.1", "100.63.0.1", "100.128.0.1", "192.169.1.1", "11.0.0.1", "2001:db8::1", "999.1.1.1", "1.2.3",
            "evil-local.com",
        ] {
            // "1.2.3" is a dotted name that isn't an address, so it is a domain.
            #expect(!ExternalLLMServer.isLocalNetworkHost(host), "\(host) should not be local")
        }
    }

    @Test func theServerAddressDecides() {
        #expect(ExternalLLM(url: "10.0.0.2:11434").isOnLocalNetwork)
        #expect(ExternalLLM(url: "http://localhost:1234/v1/models").isOnLocalNetwork)
        #expect(ExternalLLM(url: "https://api.openai.com/v1").isOnLocalNetwork == false)
        #expect(ExternalLLM(url: "https://generativelanguage.googleapis.com/v1beta/openai").isOnLocalNetwork == false)
        // No usable address: nothing is sent anywhere, but it isn't "local" either.
        #expect(ExternalLLM(url: "  ").isOnLocalNetwork == false)
    }
}
