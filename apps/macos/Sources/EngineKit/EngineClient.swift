import Foundation

/// Motoru başlatma komutu. Geliştirmede `uv run`, paketli uygulamada bundle içindeki ikili.
public struct EngineCommand: Sendable {
    public var executable: URL
    public var arguments: [String]

    public init(executable: URL, arguments: [String] = []) {
        self.executable = executable
        self.arguments = arguments
    }

    /// Sırayla: `SUBTITLE_ENGINE` ortam değişkeni (motor ikilisi), bundle içindeki
    /// `subtitle-engine`, geliştirme ortamında repo'daki `engine/` projesi (`uv run`).
    public static func locate(environment: [String: String] = ProcessInfo.processInfo.environment) -> EngineCommand? {
        if let path = environment["SUBTITLE_ENGINE"] {
            return EngineCommand(executable: URL(filePath: path))
        }
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "subtitle-engine") {
            return EngineCommand(executable: bundled)
        }
        let repoEngine = URL(filePath: #filePath)
            .deletingLastPathComponent()  // EngineKit
            .deletingLastPathComponent()  // Sources
            .deletingLastPathComponent()  // apps/macos
            .deletingLastPathComponent()  // apps
            .deletingLastPathComponent()  // repo
            .appending(path: "engine")
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let uvCandidates = ["\(home)/.local/bin/uv", "/opt/homebrew/bin/uv", "/usr/local/bin/uv", "\(home)/.cargo/bin/uv"]
        guard FileManager.default.fileExists(atPath: repoEngine.appending(path: "pyproject.toml").path),
              let uv = uvCandidates.first(where: FileManager.default.isExecutableFile(atPath:))
        else { return nil }
        return EngineCommand(
            executable: URL(filePath: uv),
            arguments: ["run", "--quiet", "--project", repoEngine.path, "subtitle-engine"]
        )
    }
}

public enum EngineError: LocalizedError, Equatable {
    case notFound
    case failed(String)

    public var errorDescription: String? {
        switch self {
        case .notFound: "Altyazı motoru bulunamadı (uv kurulu mu, ya da SUBTITLE_ENGINE ayarlı mı?)."
        case .failed(let message): message
        }
    }
}

public struct EngineClient: Sendable {
    public var command: EngineCommand

    public init(command: EngineCommand) {
        self.command = command
    }

    public struct TranscribeOptions: Sendable {
        public var language: String?
        public var model: String
        public var formats: [SubtitleFormat]

        public init(language: String? = nil, model: String = "small", formats: [SubtitleFormat] = [.srt]) {
            self.language = language
            self.model = model
            self.formats = formats
        }
    }

    /// Videoyu işler; aşama olaylarını ve sonunda `.result` ya da hata verir. Akış iptal edilirse motor durdurulur.
    public func transcribe(video: URL, outDir: URL, options: TranscribeOptions = .init()) -> AsyncThrowingStream<EngineEvent, Error> {
        var args = ["transcribe", video.path, "--out-dir", outDir.path, "--model", options.model, "--json", "--format"]
        args += options.formats.map(\.rawValue)
        if let language = options.language { args += ["--language", language] }
        let arguments = args

        return AsyncThrowingStream { continuation in
            let run: RunningEngine
            do {
                run = try start(arguments, stdin: nil)
            } catch {
                continuation.finish(throwing: error)
                return
            }
            let task = Task {
                // Her çıkış yolunda (hata, geçersiz satır, iptal) süreç sonlandırılır, log silinir.
                defer { run.cleanup() }
                do {
                    var engineError: String?
                    for try await line in run.stdout.bytes.lines where !line.isEmpty {
                        let event = try EngineEvent.decode(line)
                        if case .error(let message) = event {
                            engineError = message
                        } else {
                            continuation.yield(event)
                        }
                    }
                    let status = await run.exitStatus()
                    if let engineError {
                        continuation.finish(throwing: EngineError.failed(engineError))
                    } else if status != 0 {
                        continuation.finish(throwing: run.failure(status))
                    } else {
                        continuation.finish()
                    }
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            // stdout okuması iptale duyarlı değil; tüketici bırakırsa süreci doğrudan öldür.
            continuation.onTermination = { _ in
                run.terminate()
                task.cancel()
            }
        }
    }

    /// Cümleleri (uygulamada düzenlenmiş olabilir) motora verip istenen formatta metin alır.
    public func render(_ sentences: [Sentence], as format: SubtitleFormat) async throws -> String {
        let input = try JSONEncoder().encode(sentences)
        let run = try start(["render", "--format", format.rawValue], stdin: input)
        defer { run.cleanup() }
        return try await withTaskCancellationHandler {
            var output = Data()
            for try await byte in run.stdout.bytes { output.append(byte) }
            let status = await run.exitStatus()
            try Task.checkCancellation()
            guard status == 0 else { throw run.failure(status) }
            return String(decoding: output, as: UTF8.self)
        } onCancel: {
            run.terminate()
        }
    }

    /// Model kataloğunu ve yerel indirme durumunu alır (`models --json`, tek satır `models` olayı).
    public func models() async throws -> [EngineModel] {
        let run = try start(["models", "--json"], stdin: nil)
        defer { run.cleanup() }
        return try await withTaskCancellationHandler {
            var line: String?
            for try await text in run.stdout.bytes.lines where !text.isEmpty {
                line = text
            }
            let status = await run.exitStatus()
            try Task.checkCancellation()
            let payload = line.flatMap { try? JSONDecoder().decode(ModelsPayload.self, from: Data($0.utf8)) }
            if payload?.event == "error", let message = payload?.message {
                throw EngineError.failed(message)
            }
            guard status == 0 else { throw run.failure(status) }
            guard let payload, payload.event == "models", let models = payload.models else {
                throw EngineError.failed("Motor model listesi yazmadı.")
            }
            return models
        } onCancel: {
            run.terminate()
        }
    }

    private func start(_ args: [String], stdin input: Data?) throws -> RunningEngine {
        let process = Process()
        process.executableURL = command.executable
        process.arguments = command.arguments + args
        let stdout = Pipe()
        process.standardOutput = stdout
        // stderr dosyaya: model indirme ilerlemesi pipe tamponunu doldurup süreci kilitlemesin.
        let log = FileManager.default.temporaryDirectory.appending(path: "subtitle-engine-\(UUID().uuidString).log")
        FileManager.default.createFile(atPath: log.path, contents: nil)
        process.standardError = try FileHandle(forWritingTo: log)
        let stdinPipe = Pipe()
        process.standardInput = input == nil ? FileHandle.nullDevice : stdinPipe
        do {
            try process.run()
        } catch {
            try? FileManager.default.removeItem(at: log)
            throw EngineError.failed("Motor başlatılamadı: \(error.localizedDescription)")
        }
        if let input {
            // Motor erken çıkarsa yazma hata verir (SIGPIPE yok sayılır); çıkış kodu zaten raporlanır.
            try? stdinPipe.fileHandleForWriting.write(contentsOf: input)
            try? stdinPipe.fileHandleForWriting.close()
        }
        return RunningEngine(process: process, stdout: stdout.fileHandleForReading, log: log)
    }
}

/// `models --json` çıktısı: başarıda `models`, hatada `error` alanı dolu gelir.
private struct ModelsPayload: Decodable {
    let event: String
    let models: [EngineModel]?
    let message: String?
}

/// Çalışan motor süreci. Bekleyen görev iptal edilirse süreç sonlandırılır.
private struct RunningEngine: @unchecked Sendable {
    let process: Process
    let stdout: FileHandle
    let log: URL

    func exitStatus() async -> Int32 {
        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                DispatchQueue.global().async {
                    process.waitUntilExit()
                    continuation.resume(returning: process.terminationStatus)
                }
            }
        } onCancel: {
            process.terminate()
        }
    }

    /// Süreç hâlâ çalışıyorsa SIGTERM gönderir (`uv run` bunu Python çocuğuna iletir).
    func terminate() {
        if process.isRunning { process.terminate() }
    }

    /// Süreci sonlandırır ve geçici log dosyasını siler.
    func cleanup() {
        terminate()
        try? FileManager.default.removeItem(at: log)
    }

    func failure(_ status: Int32) -> EngineError {
        let tail = (try? String(contentsOf: log, encoding: .utf8))?
            .split(separator: "\n").suffix(5).joined(separator: "\n") ?? ""
        return .failed("Motor hata koduyla çıktı (\(status)). \(tail)")
    }
}
