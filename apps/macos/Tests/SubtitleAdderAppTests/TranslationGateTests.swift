import EngineKit
import Foundation
import Testing
@testable import SubtitleAdderApp

/// The app-wide rule "at most one translation runs at a time": the gate itself, then every path that translates (the
/// editor's job, the closing pass of "Komple çevir", the external server's background batches, the playlist's stage 3)
/// against fake engines that log when each `translate` process starts and ends.
@MainActor
@Suite struct TranslationGateTests {
    private func makeFolder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "gate-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func makeModel(engine: EngineClient) -> SubtitleViewModel {
        let vm = SubtitleViewModel(
            catalog: ModelCatalog(engine: engine),
            defaults: UserDefaults(suiteName: "test-\(UUID().uuidString)")!,
            launchState: EngineLaunchState()
        )
        vm.offlineMode = false
        return vm
    }

    private func useExternalServer(_ vm: SubtitleViewModel) {
        vm.externalLlmEnabled = true
        vm.externalLlmUrl = "10.0.0.2:1234"
        vm.externalLlmAsync = true
    }

    private func line(_ id: Int, _ text: String) -> Sentence {
        Sentence(id: id, start: Double(id), end: Double(id) + 1, text: text)
    }

    private func sentenceJSON(_ id: Int, _ text: String) -> String {
        #"{"id": \#(id), "start": \#(id), "end": \#(id + 1), "text": "\#(text)", "words": []}"#
    }

    /// `count` engine-format lines "Satır 1.", "Satır 2.", ….
    private func sentenceList(_ count: Int) -> String {
        (1...count).map { sentenceJSON($0, "Satır \($0).") }.joined(separator: ", ")
    }

    private func eventually(_ condition: () -> Bool) async -> Bool {
        for _ in 0..<400 {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(25))
        }
        return condition()
    }

    private func logLines(_ log: URL) -> [String] {
        ((try? String(contentsOf: log, encoding: .utf8)) ?? "").split(separator: "\n").map(String.init)
    }

    /// Waits for the running job and the one it hands over to.
    private func settle(_ vm: SubtitleViewModel) async {
        while let job = vm.job {
            await job.value
            if vm.job == job { break }
        }
    }

    private func isAlive(_ pid: Int32) -> Bool { kill(pid, 0) == 0 }

    /// A control folder: the engine logs to `calls.log`, writes `pid-<name>` files and waits for `<name>` files that the
    /// test creates.
    private struct Control {
        let folder: URL
        var log: URL { folder.appending(path: "calls.log") }

        func release(_ names: String...) {
            for name in names { FileManager.default.createFile(atPath: folder.appending(path: name).path, contents: Data()) }
        }

        func pid(_ name: String) -> Int32? {
            (try? String(contentsOf: folder.appending(path: "pid-" + name), encoding: .utf8))
                .flatMap { Int32($0.trimmingCharacters(in: .whitespacesAndNewlines)) }
        }
    }

    private func makeControl() throws -> Control {
        let folder = try makeFolder().appending(path: "control")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return Control(folder: folder)
    }

    private func has(_ control: Control, _ prefix: String) -> Bool {
        logLines(control.log).contains { $0.hasPrefix(prefix) }
    }

    private func count(_ control: Control, _ prefix: String) -> Int {
        logLines(control.log).filter { $0.hasPrefix(prefix) }.count
    }

    private func position(_ control: Control, _ prefix: String) -> Int? {
        logLines(control.log).firstIndex { $0.hasPrefix(prefix) }
    }

    /// The most `<phase>-start` lines that were open at once (a `-start` without its `-end` yet).
    private func peakOverlap(_ control: Control, _ phase: String) -> Int {
        var open = 0
        var peak = 0
        for line in logLines(control.log) {
            if line.hasPrefix("\(phase)-start") { open += 1; peak = max(peak, open) }
            if line.hasPrefix("\(phase)-end") { open -= 1 }
        }
        return peak
    }

    // MARK: - The gate

    @Test func waitersAreServedInArrivalOrderAndNobodyJumpsTheQueue() async throws {
        let gate = TranslationGate()
        #expect(!gate.isBusy && gate.tryAcquire() && gate.isBusy)
        #expect(!gate.tryAcquire())
        var order: [Int] = []
        var tasks: [Task<Void, any Error>] = []
        for number in 1...3 {
            tasks.append(Task {
                try await gate.acquire()
                order.append(number)
                gate.release()
            })
            #expect(await eventually { gate.waitingCount == number })
        }
        // The first waiter owns it the moment it is released; a late `tryAcquire` can't take it.
        gate.release()
        #expect(gate.isBusy && !gate.tryAcquire())
        for task in tasks { try await task.value }
        #expect(order == [1, 2, 3])
        #expect(!gate.isBusy && gate.waitingCount == 0)
        #expect(gate.tryAcquire())
    }

    @Test func aCancelledWaiterLeavesTheQueueAndTheNextOneStillGetsTheGate() async throws {
        let gate = TranslationGate()
        #expect(gate.tryAcquire())
        var events: [String] = []
        let first = Task {
            do {
                try await gate.acquire()
                events.append("first")
                gate.release()
            } catch is CancellationError {
                events.append("first-cancelled")
            } catch {
                events.append("first-failed")
            }
        }
        #expect(await eventually { gate.waitingCount == 1 })
        let second = Task {
            try await gate.acquire()
            events.append("second")
            gate.release()
        }
        #expect(await eventually { gate.waitingCount == 2 })

        first.cancel()
        await first.value
        #expect(gate.waitingCount == 1 && gate.isBusy)

        gate.release()
        try await second.value
        #expect(events == ["first-cancelled", "second"])
        #expect(!gate.isBusy && gate.waitingCount == 0)
    }

    @Test func aCancelledTaskNeverTakesAFreeGate() async {
        let gate = TranslationGate()
        let task = Task { () -> Bool in
            while !Task.isCancelled { await Task.yield() }
            do {
                try await gate.acquire()
                return true
            } catch {
                return false
            }
        }
        task.cancel()
        #expect(await task.value == false)
        #expect(!gate.isBusy && gate.waitingCount == 0)
    }

    // MARK: - The editor's job

    @Test func theEditorTranslationWaitsForARunningBatchAndThenRuns() async throws {
        let control = try makeControl()
        let engine = try FakeEngine.make(translate: """
        echo "translate-start" >> '\(control.log.path)'
        echo '{"event": "translation", "language": "en", "sentences": [\(sentenceJSON(1, "One."))]}'
        """)
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.sentences = [line(1, "Bir.")]
        // Somebody else (a background batch) is translating.
        #expect(vm.translationGate.tryAcquire())

        vm.translate()
        try await Task.sleep(for: .milliseconds(300))
        // The job waits with its usual status: no process, "Çeviri hazırlanıyor…", the job flag and the badge.
        guard case .working(let label, let fraction) = vm.status else { Issue.record("expected a working status"); return }
        #expect(fraction == nil && label.text == L("Çeviri hazırlanıyor…"))
        #expect(vm.isTranslationJobRunning && vm.translationBadgeText == L("Çeviri hazırlanıyor…"))
        #expect(vm.translationGate.waitingCount == 1)
        #expect(!has(control, "translate-start") && vm.translations.isEmpty)

        vm.translationGate.release()
        await settle(vm)
        #expect(count(control, "translate-start") == 1)
        #expect(vm.translations.values.map(\.text) == ["One."])
        #expect(vm.status == .finished)
        // The holder released on its way out.
        #expect(!vm.translationGate.isBusy && vm.translationGate.waitingCount == 0)
    }

    @Test func aStoppedWaiterLeavesTheQueueAndDoesNotBlockTheNextJob() async throws {
        let control = try makeControl()
        let engine = try FakeEngine.make(translate: """
        echo "translate-start" >> '\(control.log.path)'
        echo '{"event": "translation", "language": "en", "sentences": [\(sentenceJSON(1, "One."))]}'
        """)
        let vm = makeModel(engine: engine)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.sentences = [line(1, "Bir.")]
        #expect(vm.translationGate.tryAcquire())

        vm.translate()
        #expect(await eventually { vm.translationGate.waitingCount == 1 })
        vm.cancel()
        #expect(await eventually { vm.translationGate.waitingCount == 0 })
        #expect(vm.status == .idle && !vm.isTranslationJobRunning)

        // The next job queues behind the same holder and gets the gate when it lets go.
        vm.translate()
        #expect(await eventually { vm.translationGate.waitingCount == 1 })
        #expect(!has(control, "translate-start"))
        vm.translationGate.release()
        await settle(vm)
        #expect(count(control, "translate-start") == 1)
        #expect(vm.status == .finished && vm.translations.count == 1)
        #expect(!vm.translationGate.isBusy && vm.translationGate.waitingCount == 0)
    }

    @Test func theGateIsFreeAgainAfterATranslationThatFailsOrIsStopped() async throws {
        let failing = try FakeEngine.make(translate: #"echo '{"event": "error", "message": "boom"}'; exit 1"#)
        let vm = makeModel(engine: failing)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.sentences = [line(1, "Bir.")]
        vm.translate()
        await settle(vm)
        guard case .failed = vm.status else { Issue.record("expected the failure"); return }
        #expect(!vm.translationGate.isBusy)

        let hanging = try FakeEngine.make(translate: "exec sleep 20")
        let second = makeModel(engine: hanging)
        second.language = "tr"
        second.translationTarget = "en"
        second.sentences = [line(1, "Bir.")]
        second.translate()
        #expect(await eventually { second.translationGate.isBusy })
        second.cancel()
        #expect(await eventually { !second.translationGate.isBusy })
        #expect(second.translationGate.waitingCount == 0)
    }

    @Test func theEditorTranslationTakesOverFromARunningBackgroundBatch() async throws {
        let control = try makeControl()
        let requests = try makeFolder()
        // The first request (the background batch) hangs until released; any later one answers at once.
        let engine = try FakeEngine.make(
            transcribe: """
            echo '{"event": "partial", "language": "tr", "sentences": [\(sentenceList(12))]}'
            exec sleep 20
            """,
            translate: """
            echo $$ > '\(control.folder.path)/pid-'$n
            echo "translate-start $n" >> '\(control.log.path)'
            if [ "$n" = 0 ]; then
            i=0; while [ ! -e '\(control.folder.path)/hold-0' ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done
            fi
            echo "translate-end $n" >> '\(control.log.path)'
            printf '{"event": "translation", "language": "en", "sentences": %s}\\n' "$(sed 's/"text":"\\([^"]*\\)"/"text":"T-\\1"/g' '\(requests.path)/req-'$n)"
            """,
            translateInput: "n=$(ls '\(requests.path)' | wc -l | tr -d ' '); cat > '\(requests.path)/req-'$n"
        )
        let vm = makeModel(engine: engine)
        useExternalServer(vm)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.diarize = false
        vm.openVideo(requests.appending(path: "Film.mp4"))
        vm.generate(thenTranslate: true)

        // The background batch is at the server and holds the gate.
        #expect(await eventually { has(control, "translate-start 0") })
        #expect(vm.translationGate.isBusy && vm.isAsyncTranslating)
        let batchPID = try #require(control.pid("0"))

        // "Çevir…" stops the background work and takes the gate over once the batch's process is torn down.
        vm.translate()
        #expect(await eventually { has(control, "translate-start 1") })
        #expect(await eventually { !isAlive(batchPID) })
        #expect(!has(control, "translate-end 0"))
        await settle(vm)
        #expect(vm.status == .finished)
        #expect(vm.sentences.count == 12 && vm.translations.count == 12)
        #expect(!vm.translationGate.isBusy && vm.translationGate.waitingCount == 0)
    }

    // MARK: - The background pump and the closing pass

    /// ASR that keeps sending a snapshot of 12 lines every 0.1 s until the test creates `done`, then the result with 13.
    private func streamingTranscribe(_ control: Control) -> String {
        """
        i=0
        while [ ! -e '\(control.folder.path)/done' ] && [ $i -lt 300 ]; do
        echo '{"event": "partial", "language": "tr", "sentences": [\(sentenceList(12))]}'
        sleep 0.1; i=$((i+1))
        done
        echo '{"event": "result", "language": "tr", "duration": 14, "audio": "/nonexistent.wav", "outputs": [], "sentences": [\(sentenceList(13))]}'
        """
    }

    /// A `translate` that logs each request to `requests/req-<n>` and `translate-start/end n` to the control log, and
    /// answers with the lines prefixed "T-".
    private func loggingTranslate(_ control: Control, requests: URL) -> (translate: String, input: String) {
        (
            """
            echo "translate-start $n" >> '\(control.log.path)'
            echo "translate-end $n" >> '\(control.log.path)'
            printf '{"event": "translation", "language": "en", "sentences": %s}\\n' "$(sed 's/"text":"\\([^"]*\\)"/"text":"T-\\1"/g' '\(requests.path)/req-'$n)"
            """,
            "n=$(ls '\(requests.path)' | wc -l | tr -d ' '); cat > '\(requests.path)/req-'$n"
        )
    }

    private func requestTexts(_ requests: URL) throws -> [[String]] {
        try FileManager.default.contentsOfDirectory(atPath: requests.path).sorted().map { name in
            let data = try Data(contentsOf: requests.appending(path: name))
            return try JSONDecoder().decode([Sentence].self, from: data).map(\.text)
        }
    }

    @Test func thePumpDoesNotStartABatchWhileAnotherTranslationHoldsTheGate() async throws {
        let control = try makeControl()
        let requests = try makeFolder()
        let fake = loggingTranslate(control, requests: requests)
        let engine = try FakeEngine.make(
            transcribe: streamingTranscribe(control), translate: fake.translate, translateInput: fake.input
        )
        let vm = makeModel(engine: engine)
        useExternalServer(vm)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.diarize = false
        vm.openVideo(requests.appending(path: "Film.mp4"))
        // Another translation (the playlist's stage 3 of the previous video, say) has the gate.
        #expect(vm.translationGate.tryAcquire())
        vm.generate(thenTranslate: true)

        // Snapshots keep coming, ten lines are ready, and nothing is sent.
        #expect(await eventually { vm.sentences.count == 12 })
        try await Task.sleep(for: .milliseconds(500))
        #expect(!has(control, "translate-start") && !vm.isAsyncTranslating && vm.asyncTask == nil)
        #expect(vm.translationGate.waitingCount == 0)

        // The gate frees up: the next snapshot starts the batch with the ten finished lines.
        vm.translationGate.release()
        #expect(await eventually { has(control, "translate-end 0") })
        #expect(await eventually { vm.translations.count == 10 })
        #expect(try requestTexts(requests).first?.count == 12)

        control.release("done")
        await settle(vm)
        #expect(vm.status == .finished)
        // The closing pass sent only what was left (lines 11 to 13, with context before them).
        #expect(vm.sentences.count == 13 && vm.translations.count == 13)
        #expect(try requestTexts(requests).count == 2)
        #expect(peakOverlap(control, "translate") == 1)
        #expect(!vm.translationGate.isBusy)
    }

    @Test func theClosingPassWaitsForTheGateAndPicksUpTheLinesThePumpSkipped() async throws {
        let control = try makeControl()
        let requests = try makeFolder()
        let fake = loggingTranslate(control, requests: requests)
        let engine = try FakeEngine.make(
            transcribe: streamingTranscribe(control), translate: fake.translate, translateInput: fake.input
        )
        let vm = makeModel(engine: engine)
        useExternalServer(vm)
        vm.language = "tr"
        vm.translationTarget = "en"
        vm.autoSaveSubtitle = false
        vm.diarize = false
        vm.openVideo(requests.appending(path: "Film.mp4"))
        #expect(vm.translationGate.tryAcquire())
        vm.generate(thenTranslate: true)
        #expect(await eventually { vm.sentences.count == 12 })

        // The recognition ends with the gate still taken: the closing pass queues, showing its usual status.
        control.release("done")
        #expect(await eventually { vm.sentences.count == 13 && vm.translationGate.waitingCount == 1 })
        guard case .working(let label, _) = vm.status else { Issue.record("expected a working status"); return }
        #expect(label.text == L("Çeviri hazırlanıyor…") && vm.isTranslationJobRunning)
        #expect(!has(control, "translate-start"))

        vm.translationGate.release()
        await settle(vm)
        // Nothing went out before; the one request carries every line.
        #expect(try requestTexts(requests) == [(1...13).map { "Satır \($0)." }])
        #expect(vm.status == .finished && vm.translations.count == 13)
        #expect(!vm.translationGate.isBusy && vm.translationGate.waitingCount == 0)
    }

    // MARK: - The playlist

    /// Fake engine for the playlist: `transcribe` and `translate` log `<phase>-start <stem>` / `<phase>-end <stem>`,
    /// write their pid to `pid-<phase>-<stem>` and wait for a file `<phase>-<stem>` in the control folder. The
    /// recognized text is "Metin <stem>"; `extract-audio` prints nothing (recognition extracts by itself).
    private func gatedEngine(_ control: Control) throws -> EngineClient {
        func start(_ phase: String) -> String {
            """
            echo $$ > '\(control.folder.path)/pid-\(phase)-'$stem
            echo "\(phase)-start $stem" >> '\(control.log.path)'
            """
        }
        func hold(_ phase: String) -> String {
            """
            i=0; while [ ! -e '\(control.folder.path)/\(phase)-'$stem ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done
            echo "\(phase)-end $stem" >> '\(control.log.path)'
            """
        }
        let transcribe = #"""
        stem=$(basename "$2" .mp4)
        \#(start("transcribe"))
        \#(hold("transcribe"))
        echo "{\"event\": \"result\", \"language\": \"en\", \"duration\": 1, \"audio\": \"/nonexistent.wav\", \"outputs\": [], \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Metin $stem\", \"words\": []}]}"
        """#
        let translate = #"""
        \#(start("translate"))
        \#(hold("translate"))
        echo "{\"event\": \"translation\", \"language\": \"tr\", \"sentences\": [{\"id\": 1, \"start\": 0, \"end\": 1, \"text\": \"Çeviri $stem\", \"words\": []}]}"
        """#
        return try FakeEngine.make(
            transcribe: transcribe, translate: translate,
            translateInput: #"input=$(cat); stem=$(printf '%s' "$input" | sed -n 's/.*Metin \([a-z0-9]*\).*/\1/p' | head -1)"#
        )
    }

    @Test func theBacklogIsOneItemSoRecognitionOfItemThreeWaitsForTheFirstTranslation() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let vm = makeModel(engine: try gatedEngine(control))
        vm.externalLlmEnabled = false
        vm.translationTarget = "tr"
        vm.addToPlaylist((1...3).map { root.appending(path: "v\($0).mp4") }, sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask

        // Item 1 is recognized and translates; item 2's recognition overlaps that translation.
        control.release("transcribe-v1")
        #expect(await eventually { has(control, "translate-start v1") && has(control, "transcribe-start v2") })
        #expect(!has(control, "translate-end v1"))

        // Item 2 is recognized too, but its own hand-off waits for item 1; item 3 isn't touched.
        control.release("transcribe-v2")
        #expect(await eventually { has(control, "transcribe-end v2") && vm.playlist[1].statusMessage.text == "Çeviri hazırlanıyor…" })
        try await Task.sleep(for: .milliseconds(300))
        #expect(!has(control, "transcribe-start v3") && count(control, "translate-start") == 1)
        #expect(vm.playlist[1].state == .working && vm.playlistBackgroundPending == 1)
        #expect(vm.translationGate.isBusy && vm.translationGate.waitingCount == 0)

        // Item 1's translation is over: item 2 translates and item 3 is recognized.
        control.release("translate-v1")
        #expect(await eventually { has(control, "translate-start v2") && has(control, "transcribe-start v3") })
        let firstEnd = try #require(position(control, "translate-end v1"))
        #expect(firstEnd < (try #require(position(control, "transcribe-start v3"))))
        #expect(firstEnd < (try #require(position(control, "translate-start v2"))))

        control.release("transcribe-v3", "translate-v2", "translate-v3")
        await task?.value
        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        #expect(vm.status == .finished)
        #expect(peakOverlap(control, "translate") == 1 && peakOverlap(control, "transcribe") == 1)
        #expect(!vm.translationGate.isBusy && vm.translationGate.waitingCount == 0)
        #expect(vm.playlistBackgroundPending == 0)
    }

    @Test func stoppingWhileAnItemWaitsForTheEarlierTranslationPutsEverythingBack() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        let vm = makeModel(engine: try gatedEngine(control))
        vm.translationTarget = "tr"
        vm.addToPlaylist((1...3).map { root.appending(path: "v\($0).mp4") }, sourceLanguage: "en")
        vm.startPlaylist()
        let task = vm.playlistTask
        control.release("transcribe-v1", "transcribe-v2")
        #expect(await eventually { has(control, "transcribe-end v2") && vm.playlist[1].statusMessage.text == "Çeviri hazırlanıyor…" })
        let pid = try #require(control.pid("translate-v1"))
        #expect(isAlive(pid))

        vm.cancel()
        await task?.value

        #expect(await eventually { !isAlive(pid) })
        #expect(vm.playlist.allSatisfy { $0.state == .pending && $0.statusMessage.text == "Sırada" && $0.fraction == nil })
        #expect(!has(control, "transcribe-start v3"))
        #expect(!vm.isPlaylistRunning && !vm.isWorking && vm.playlistBackgroundPending == 0 && vm.canStartPlaylist)
        #expect(await eventually { !vm.translationGate.isBusy })
        #expect(vm.translationGate.waitingCount == 0)
    }

    @Test func playlistStageThreeAndTheBackgroundPumpNeverTranslateAtTheSameTime() async throws {
        let control = try makeControl()
        let root = control.folder.deletingLastPathComponent()
        // Each video streams snapshots for about a second (the pump wants to translate its ten finished lines) while the
        // previous video's stage 3 is still busy with a half-second request.
        let engine = try FakeEngine.make(
            transcribe: """
            stem=$(basename "$2" .mp4)
            echo "transcribe-start $stem" >> '\(control.log.path)'
            for round in 1 2 3 4 5 6; do
            echo '{"event": "partial", "language": "en", "sentences": [\(sentenceList(12))]}'
            sleep 0.2
            done
            echo '{"event": "result", "language": "en", "duration": 14, "audio": "/nonexistent.wav", "outputs": [], "sentences": [\(sentenceList(13))]}'
            echo "transcribe-end $stem" >> '\(control.log.path)'
            """,
            translate: """
            echo "translate-start" >> '\(control.log.path)'
            sleep 0.5
            echo "translate-end" >> '\(control.log.path)'
            printf '{"event": "translation", "language": "tr", "sentences": %s}\\n' "$(printf '%s' "$input" | sed 's/"text":"\\([^"]*\\)"/"text":"T-\\1"/g')"
            """,
            translateInput: "input=$(cat)"
        )
        let vm = makeModel(engine: engine)
        useExternalServer(vm)
        vm.diarize = false
        vm.translationTarget = "tr"
        vm.addToPlaylist((1...3).map { root.appending(path: "v\($0).mp4") }, sourceLanguage: "en")
        vm.startPlaylist()
        await vm.playlistTask?.value

        #expect(vm.playlist.allSatisfy { $0.state == .completed })
        // Stage 3 of every video ran (and the pump got its turn whenever the gate was free), never two at once.
        #expect(count(control, "translate-start") >= 3)
        #expect(peakOverlap(control, "translate") == 1)
        #expect(peakOverlap(control, "transcribe") == 1)
        for stem in ["v1", "v2", "v3"] {
            let saved = try String(contentsOf: root.appending(path: SubtitleViewModel.subtitleFileName(stem: stem, format: .srt)), encoding: .utf8)
            #expect(saved.contains("T-Satır 13."))
        }
        #expect(!vm.translationGate.isBusy && vm.translationGate.waitingCount == 0)
    }
}
