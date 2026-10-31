import Foundation

/// Adım 2: sesi kelime zaman damgalı metne çevirir. Backend'ler (WhisperKit, Apple Speech,
/// bulut API) bu protokolü uygular; pipeline backend'den habersizdir.
public protocol Transcriber: Sendable {
    /// `language` nil ise dil otomatik algılanır.
    func transcribe(audio: URL, language: String?) async throws -> Transcript
}
