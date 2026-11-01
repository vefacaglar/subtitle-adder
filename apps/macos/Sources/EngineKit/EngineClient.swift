import Foundation

/// Command that launches the engine. `uv run` in development, the binary inside the bundle in the packaged app.
public struct EngineCommand: Sendable {
    public var executable: URL
    public var arguments: [String]

    public init(executable: URL, arguments: [String] = []) {
        self.executable = executable
        self.arguments = arguments
    }

    /// Location of the bundled engine: `Contents/Resources/subtitle-engine/subtitle-engine` (PyInstaller onedir),
    /// otherwise `Contents/MacOS/subtitle-engine`. `nil` if not found.
    public static func bundledEngine(in bundle: Bundle = .main) -> URL? {
        if let resources = bundle.resourceURL {
            let onedir = resources.appending(path: "subtitle-engine").appending(path: "subtitle-engine")
            if FileManager.default.isExecutableFile(atPath: onedir.path) { return onedir }
        }
        return bundle.url(forAuxiliaryExecutable: "subtitle-engine")
    }

    /// In order: the `SUBTITLE_ENGINE` environment variable (engine binary), the `subtitle-engine`
    /// in the bundle, and in development the `engine/` project in the repo (`uv run`).
    public static func locate(environment: [String: String] = ProcessInfo.processInfo.environment) -> EngineCommand? {
        if let path = environment["SUBTITLE_ENGINE"] {
            return EngineCommand(executable: URL(filePath: path))
        }
        if let bundled = bundledEngine() {
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
        case .notFound: engineString("Altyazı motoru bulunamadı (uv kurulu mu, ya da SUBTITLE_ENGINE ayarlı mı?).")
        case .failed(let message): message
        }
    }
}

public struct EngineClient: Sendable {
    public var command: EngineCommand
    /// Language passed to the engine as `SUBTITLE_ENGINE_LANG` (its error messages follow it).
    /// `nil` follows the app's current UI language (`LanguageState.shared`) at each launch.
    public var language: UILanguage?

    public init(command: EngineCommand) {
        self.command = command
    }

    public init(command: EngineCommand, language: UILanguage?) {
        self.command = command
        self.language = language
    }

    /// Environment for the engine process: the parent's plus `SUBTITLE_ENGINE_LANG`.
    func processEnvironment(
        language: UILanguage, base: [String: String] = ProcessInfo.processInfo.environment
    ) -> [String: String] {
        var env = base
        env["SUBTITLE_ENGINE_LANG"] = language.rawValue
        return env
    }

    /// The explicit language, or the app's current UI language (`current`) when none was set.
    func resolvedLanguage(current: UILanguage = LanguageState.shared.language) -> UILanguage {
        language ?? current
    }

    public struct TranscribeOptions: Sendable {
        public var language: String?
        public var model: String
        public var formats: [SubtitleFormat]
        /// Separate speakers (`--diarize`).
        public var diarize: Bool
        /// Known speaker count (`--speakers`); `nil` means automatic. Only used with `diarize`.
        public var speakers: Int?

        public init(
            language: String? = nil, model: String = "small", formats: [SubtitleFormat] = [.srt],
            diarize: Bool = false, speakers: Int? = nil
        ) {
            self.diarize = diarize
            self.speakers = speakers
            self.language = language
            self.model = model
            self.formats = formats
        }
    }

    /// Processes the video; emits stage events and finally `.result` or an error. If the stream is cancelled, the engine is stopped.
    public func transcribe(video: URL, outDir: URL, options: TranscribeOptions = .init()) -> AsyncThrowingStream<EngineEvent, Error> {
        let args = Self.transcribeArguments(video: video, outDir: outDir, options: options)
        return stream(args, stdin: nil)
    }

    /// Arguments `transcribe` would pass; exposed for tests.
    static func transcribeArguments(video: URL, outDir: URL, options: TranscribeOptions) -> [String] {
        var args = ["transcribe", video.path, "--out-dir", outDir.path, "--model", options.model, "--json", "--format"]
        args += options.formats.map(\.rawValue)
        if let language = options.language { args += ["--language", language] }
        if options.diarize {
            args.append("--diarize")
            if let speakers = options.speakers { args += ["--speakers", String(speakers)] }
        }
        return args
    }

    /// Translates the sentences to the target language with a local translation model; emits stage events and finally
    /// `.translation`. If the source language is `nil`, the model detects it itself (NLLB does not accept that).
    public func translate(
        _ sentences: [Sentence],
        to language: String,
        from source: String? = nil,
        model: String = "madlad-3b"
    ) -> AsyncThrowingStream<EngineEvent, Error> {
        var args = ["translate", "--to", language, "--model", model, "--json"]
        if let source { args += ["--from", source] }
        let input: Data
        do {
            input = try JSONEncoder().encode(sentences)
        } catch {
            return AsyncThrowingStream { $0.finish(throwing: error) }
        }
        return stream(args, stdin: input)
    }

    /// Shared process stream: decodes the JSON events on stdout and throws the error at the end of the stream.
    private func stream(_ arguments: [String], stdin input: Data?) -> AsyncThrowingStream<EngineEvent, Error> {
        AsyncThrowingStream { continuation in
            let run: RunningEngine
            do {
                run = try start(arguments, stdin: input)
            } catch {
                continuation.finish(throwing: error)
                return
            }
            let task = Task {
                // On every exit path (error, invalid line, cancel) the process is terminated and the log is deleted.
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
            // Reading stdout is not cancellation-aware; if the consumer drops it, kill the process directly.
            continuation.onTermination = { _ in
                run.terminate()
                task.cancel()
            }
        }
    }

    /// Sends the sentences (possibly edited in the app) to the engine and gets text in the requested format.
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

    /// Gets the model catalog and local download status (`models --json`, a single `models` event line).
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
                throw EngineError.failed(engineString("Motor model listesi yazmadı.", language: language))
            }
            return models
        } onCancel: {
            run.terminate()
        }
    }

    /// Deletes a downloaded model from the cache and returns the freed MB (`delete-model <id> --json`).
    public func deleteModel(_ id: String) async throws -> Int {
        let run = try start(["delete-model", id, "--json"], stdin: nil)
        defer { run.cleanup() }
        return try await withTaskCancellationHandler {
            var line: String?
            for try await text in run.stdout.bytes.lines where !text.isEmpty {
                line = text
            }
            let status = await run.exitStatus()
            try Task.checkCancellation()
            let payload = line.flatMap { try? JSONDecoder().decode(DeletePayload.self, from: Data($0.utf8)) }
            if payload?.event == "error", let message = payload?.message {
                throw EngineError.failed(message)
            }
            guard status == 0 else { throw run.failure(status) }
            guard let payload, payload.event == "deleted", payload.id == id, let freed = payload.freedMB else {
                throw EngineError.failed(engineString("Motor silme sonucunu yazmadı.", language: language))
            }
            return freed
        } onCancel: {
            run.terminate()
        }
    }

    private func start(_ args: [String], stdin input: Data?) throws -> RunningEngine {
        let process = Process()
        process.executableURL = command.executable
        process.arguments = command.arguments + args
        let lang = resolvedLanguage()
        process.environment = processEnvironment(language: lang)
        let stdout = Pipe()
        process.standardOutput = stdout
        // stderr goes to a file: so model download progress can't fill the pipe buffer and hang the process.
        let log = FileManager.default.temporaryDirectory.appending(path: "subtitle-engine-\(UUID().uuidString).log")
        FileManager.default.createFile(atPath: log.path, contents: nil)
        process.standardError = try FileHandle(forWritingTo: log)
        let stdinPipe = Pipe()
        process.standardInput = input == nil ? FileHandle.nullDevice : stdinPipe
        do {
            try process.run()
        } catch {
            try? FileManager.default.removeItem(at: log)
            throw EngineError.failed(engineString("Motor başlatılamadı: %@", language: lang, error.localizedDescription))
        }
        if let input {
            // Written off the caller's thread: a large payload can block until the engine reads it, and the
            // caller (often the main actor) must not wait. If the engine exits early, writing fails
            // (SIGPIPE is ignored); the exit code is reported anyway.
            let writer = stdinPipe.fileHandleForWriting
            DispatchQueue.global().async {
                try? writer.write(contentsOf: input)
                try? writer.close()
            }
        }
        return RunningEngine(process: process, stdout: stdout.fileHandleForReading, log: log, language: lang)
    }
}

/// `models --json` output: `models` is filled on success, `error` on failure.
private struct ModelsPayload: Decodable {
    let event: String
    let models: [EngineModel]?
    let message: String?
}

/// `delete-model --json` output: `deleted` is filled on success, `error` on failure.
private struct DeletePayload: Decodable {
    let event: String
    let id: String?
    let freedMB: Int?
    let message: String?

    private enum CodingKeys: String, CodingKey {
        case event, id, message
        case freedMB = "freed_mb"
    }
}

/// The running engine process. If the waiting task is cancelled, the process is terminated.
private struct RunningEngine: @unchecked Sendable {
    let process: Process
    let stdout: FileHandle
    let log: URL
    let language: UILanguage

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

    /// Sends SIGTERM if the process is still running (`uv run` forwards it to the Python child).
    func terminate() {
        if process.isRunning { process.terminate() }
    }

    /// Terminates the process and deletes the temporary log file.
    func cleanup() {
        terminate()
        try? FileManager.default.removeItem(at: log)
    }

    func failure(_ status: Int32) -> EngineError {
        let tail = (try? String(contentsOf: log, encoding: .utf8))?
            .split(separator: "\n").suffix(5).joined(separator: "\n") ?? ""
        return .failed(engineString("Motor hata koduyla çıktı (%lld). %@", language: language, Int(status), tail))
    }
}
