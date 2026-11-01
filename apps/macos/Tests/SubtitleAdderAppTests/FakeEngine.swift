import EngineKit
import Foundation

/// A shell script standing in for the engine; `models`, `transcribe`, `translate` and `render` answer by subcommand.
/// `translate` and `render` read stdin first; `render` defaults to echoing its input.
enum FakeEngine {
    static func make(models: String = "", transcribe: String = "", translate: String = "", render: String = "cat") throws -> EngineClient {
        let url = FileManager.default.temporaryDirectory.appending(path: "fake-engine-\(UUID().uuidString).sh")
        let script = """
        #!/bin/sh
        case "$1" in
        models)
        \(models.isEmpty ? ":" : models)
        ;;
        transcribe)
        \(transcribe.isEmpty ? ":" : transcribe)
        ;;
        translate)
        cat > /dev/null
        \(translate.isEmpty ? ":" : translate)
        ;;
        render)
        \(render)
        ;;
        esac
        """
        try script.write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return EngineClient(command: EngineCommand(executable: url), language: .tr)
    }
}
