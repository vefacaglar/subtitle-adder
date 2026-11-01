using System.Text;

namespace SubtitleAdder.EngineKit.Tests;

public class EngineKitTests
{
    [Fact]
    public void DecodesStageEvents()
    {
        Assert.Equal(new EngineEvent.Stage(EngineStage.Transcribing, 0.5),
            EngineEvent.Decode("""{"event": "stage", "stage": "transcribing", "fraction": 0.5}"""));
        Assert.Equal(new EngineEvent.Stage(EngineStage.Unknown, null),
            EngineEvent.Decode("""{"event": "stage", "stage": "yeni_asama", "fraction": null}"""));
    }

    [Fact]
    public void DecodesDownloadingModelStage()
    {
        Assert.Equal(new EngineEvent.Stage(EngineStage.DownloadingModel, 0.37),
            EngineEvent.Decode("""{"event": "stage", "stage": "downloading_model", "fraction": 0.37}"""));
        Assert.Equal(new EngineEvent.Stage(EngineStage.Translating, 0.42),
            EngineEvent.Decode("""{"event": "stage", "stage": "translating", "fraction": 0.42}"""));
        Assert.Equal("translating", EngineStage.Translating.RawValue());
    }

    [Fact]
    public void UnknownEventThrows()
    {
        Assert.ThrowsAny<System.Text.Json.JsonException>(() => EngineEvent.Decode("""{"event": "mystery"}"""));
    }

    [Fact]
    public async Task ListsModelsFromEngine()
    {
        var engine = Fake.Engine("""
            out {"event": "models", "models": [{"id": "base", "backend": "whisper", "languages": null, "size_mb": 145, "disk_mb": 141, "speed": 5, "accuracy": 2, "downloaded": true}, {"id": "gigaam-v3", "backend": "onnx", "languages": ["ru"], "size_mb": 216, "disk_mb": null, "speed": 5, "accuracy": 4, "downloaded": false}]}
            """);
        var models = await engine.ModelsAsync();
        Assert.Equal(
            [
                new EngineModel { Id = "base", Backend = "whisper", Languages = null, SizeMB = 145, DiskMB = 141, Speed = 5, Accuracy = 2, Downloaded = true },
                new EngineModel { Id = "gigaam-v3", Backend = "onnx", Languages = ["ru"], SizeMB = 216, DiskMB = null, Speed = 5, Accuracy = 4, Downloaded = false },
            ],
            models);
        Assert.Equal("transcription", models[0].Kind);
        Assert.Null(models[0].License);
        Assert.False(models[0].IsTranslation);
        Assert.False(models[0].HasNonCommercialLicense);
        Assert.True(models[0].DetectsLanguage);
        Assert.True(models[0].Supports("tr"));
        Assert.False(models[1].DetectsLanguage);
        Assert.True(models[1].Supports("ru"));
        Assert.False(models[1].Supports("tr"));
    }

    [Fact]
    public async Task DecodesModelsWithoutNewFields()
    {
        var engine = Fake.Engine("""
            out {"event": "models", "models": [{"id": "small", "size_mb": 465, "speed": 4, "accuracy": 3, "downloaded": false}]}
            """);
        var model = Assert.Single(await engine.ModelsAsync());
        Assert.Equal("transcription", model.Kind);
        Assert.Equal("whisper", model.Backend);
        Assert.Null(model.Languages);
        Assert.Null(model.DiskMB);
        Assert.Null(model.License);
    }

    [Fact]
    public async Task DecodesTranslationModelFields()
    {
        var engine = Fake.Engine("""
            out {"event": "models", "models": [{"id": "nllb-1.3b", "kind": "translation", "backend": "ctranslate2", "languages": ["tr", "en", "pt", "es", "ru"], "size_mb": 1336, "disk_mb": 1336, "speed": 3, "accuracy": 4, "license": "CC-BY-NC-4.0", "downloaded": true}]}
            """);
        var model = Assert.Single(await engine.ModelsAsync());
        Assert.Equal("translation", model.Kind);
        Assert.True(model.IsTranslation);
        Assert.Equal("CC-BY-NC-4.0", model.License);
        Assert.True(model.HasNonCommercialLicense);
        Assert.True(model.Supports("es"));
    }

    [Fact]
    public async Task ModelsWithoutOutputIsLocalizedError()
    {
        var engine = Fake.Engine("out", UILanguage.En);
        var error = await Assert.ThrowsAsync<EngineException>(() => engine.ModelsAsync());
        Assert.Equal("The engine did not output a model list.", error.Message);
    }

    [Fact]
    public async Task DeleteModelReturnsFreedSpace()
    {
        var argsFile = Fake.TempPath("args");
        var engine = Fake.Engine($$"""
            args {{argsFile}}
            out {"event": "deleted", "id": "small", "freed_mb": 464}
            """);
        Assert.Equal(464, await engine.DeleteModelAsync("small"));
        Assert.Contains("delete-model small --json", Fake.Read(argsFile));
    }

    [Fact]
    public async Task DeleteModelSurfacesEngineError()
    {
        var engine = Fake.Engine("""
            out {"event": "error", "message": "tiny modeli indirilmemiş; silinecek dosya yok."}
            exit 1
            """);
        var error = await Assert.ThrowsAsync<EngineException>(() => engine.DeleteModelAsync("tiny"));
        Assert.Equal(EngineError.Failed, error.Error);
        Assert.Equal("tiny modeli indirilmemiş; silinecek dosya yok.", error.Message);
    }

    [Fact]
    public async Task DeleteModelWithWrongIdIsError()
    {
        var engine = Fake.Engine("""
            out {"event": "deleted", "id": "base", "freed_mb": 1}
            """, UILanguage.Tr);
        var error = await Assert.ThrowsAsync<EngineException>(() => engine.DeleteModelAsync("small"));
        Assert.Equal("Motor silme sonucunu yazmadı.", error.Message);
    }

    [Fact]
    public void DecodesResultAndError()
    {
        var line = """{"event": "result", "language": "tr", "duration": 2.0, "audio": "/a.wav", "outputs": [], "sentences": [{"id": 1, "start": 0.0, "end": 1.0, "text": "Merhaba.", "words": [{"start": 0.0, "end": 1.0, "text": "Merhaba."}]}]}""";
        var result = Assert.IsType<EngineEvent.Result>(EngineEvent.Decode(line)).Value;
        Assert.Equal("tr", result.Language);
        Assert.Equal(["Merhaba."], result.Sentences.Select(s => s.Text));
        Assert.Equal(new EngineEvent.Error("olmadı"), EngineEvent.Decode("""{"event": "error", "message": "olmadı"}"""));
    }

    [Fact]
    public async Task StreamsEventsUntilResult()
    {
        var engine = Fake.Engine("""
            out {"event": "stage", "stage": "extracting_audio", "fraction": null}
            err engine noise
            out {"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": [], "sentences": []}
            """);
        var events = await Fake.Collect(engine.Transcribe(@"C:\v.mp4", Path.GetTempPath()));
        Assert.Equal(2, events.Count);
        Assert.Equal(new EngineEvent.Stage(EngineStage.ExtractingAudio, null), events[0]);
        Assert.IsType<EngineEvent.Result>(events[1]);
    }

    [Fact]
    public async Task TranscribePassesArguments()
    {
        var argsFile = Fake.TempPath("args");
        var engine = Fake.Engine($$"""
            args {{argsFile}}
            out {"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": [], "sentences": []}
            """);
        await Fake.Drain(engine.Transcribe(@"C:\videos\my video.mp4", @"C:\out",
            new TranscribeOptions { Language = "tr", Model = "base", Formats = [SubtitleFormat.Srt, SubtitleFormat.Vtt] }));
        Assert.Equal(@"transcribe C:\videos\my video.mp4 --out-dir C:\out --model base --json --format srt vtt --language tr",
            Fake.Read(argsFile));
    }

    [Fact]
    public async Task EngineErrorEventThrows()
    {
        var engine = Fake.Engine("""
            out {"event": "error", "message": "Video bulunamadı"}
            exit 1
            """);
        var error = await Assert.ThrowsAsync<EngineException>(() => Fake.Drain(engine.Transcribe(@"C:\v.mp4", Path.GetTempPath())));
        Assert.Equal("Video bulunamadı", error.Message);
    }

    [Fact]
    public async Task NonzeroExitReportsCodeAndLogTailThenDeletesLog()
    {
        var log = Fake.TempPath("subtitle-engine", ".log");
        var engine = Fake.Engine("""
            err line 1
            err line 2
            err line 3
            err line 4
            err line 5
            err line 6
            err line 7 ünlü
            exit 3
            """, UILanguage.En);
        engine.LogPathFactory = () => log;
        var error = await Assert.ThrowsAsync<EngineException>(() => Fake.Drain(engine.Transcribe(@"C:\v.mp4", Path.GetTempPath())));
        Assert.Equal("The engine exited with an error code (3). line 3\nline 4\nline 5\nline 6\nline 7 ünlü", error.Message);
        Assert.False(File.Exists(log));
    }

    [Fact]
    public async Task NonzeroExitInRenderReportsLogTail()
    {
        var engine = Fake.Engine("""
            err Traceback: boom
            exit 2
            """, UILanguage.Tr);
        var error = await Assert.ThrowsAsync<EngineException>(() => engine.RenderAsync([new Sentence(1, 0, 1, "A")], SubtitleFormat.Srt));
        Assert.Equal("Motor hata koduyla çıktı (2). Traceback: boom", error.Message);
    }

    [Fact]
    public async Task EarlyExitBeforeReadingStdinIsReportedNotCrashed()
    {
        // A payload far larger than the pipe buffer: the write fails with a broken pipe once the engine is gone.
        var big = Enumerable.Range(1, 20_000).Select(i => new Sentence(i, i, i + 0.5, new string('x', 200))).ToList();
        var engine = Fake.Engine("""
            err refusing input
            exit 4
            """, UILanguage.En);
        var error = await Assert.ThrowsAsync<EngineException>(() => engine.RenderAsync(big, SubtitleFormat.Srt));
        Assert.Equal("The engine exited with an error code (4). refusing input", error.Message);
        var translateError = await Assert.ThrowsAsync<EngineException>(() => Fake.Drain(engine.Translate(big, "es")));
        Assert.Contains("(4)", translateError.Message);
    }

    [Fact]
    public async Task MissingExecutableIsLocalizedStartFailure()
    {
        var engine = new EngineClient(new EngineCommand(@"C:\does\not\exist\subtitle-engine.exe"), UILanguage.En);
        var error = await Assert.ThrowsAsync<EngineException>(() => engine.ModelsAsync());
        Assert.StartsWith("Could not start the engine: ", error.Message);
    }

    [Fact]
    public async Task RenderSendsSentencesOnStdin()
    {
        var engine = Fake.Engine("cat");  // echoes stdin back unchanged
        var text = await engine.RenderAsync([new Sentence(1, 0, 1, "Selam.")], SubtitleFormat.Srt);
        Assert.Contains("\"Selam.\"", text);
    }

    [Fact]
    public async Task RenderPassesFormatAndKeepsUtf8()
    {
        var argsFile = Fake.TempPath("args");
        var engine = Fake.Engine($"args {argsFile}\ncat");
        var text = await engine.RenderAsync([new Sentence(1, 0, 1, "Altyazı – şöyle ğ")], SubtitleFormat.Vtt);
        Assert.Contains("Altyazı – şöyle ğ", text);
        Assert.Equal("render --format vtt", Fake.Read(argsFile));
    }

    [Fact]
    public void DecodesTranslationEvent()
    {
        var line = """{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0.0, "end": 1.0, "text": "Hola.", "words": []}]}""";
        Assert.Equal(new EngineEvent.Translation("es", [new Sentence(1, 0, 1, "Hola.")]), EngineEvent.Decode(line));
    }

    [Fact]
    public async Task TranslateStreamsEventsAndSendsRequest()
    {
        var argsFile = Fake.TempPath("args");
        var inputFile = Fake.TempPath("input");
        var engine = Fake.Engine($$"""
            stdin {{inputFile}}
            args {{argsFile}}
            out {"event": "stage", "stage": "downloading_model", "fraction": 0.12}
            out {"event": "stage", "stage": "translating", "fraction": 0.4}
            out {"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0.0, "end": 1.0, "text": "Hola.", "words": []}]}
            """);
        var source = new Sentence(1, 0, 1, "Merhaba.", [new Word(0, 1, "Merhaba.")]);
        var events = await Fake.Collect(engine.Translate([source], "es", "tr", "madlad-3b"));
        Assert.Equal(
            [
                new EngineEvent.Stage(EngineStage.DownloadingModel, 0.12),
                new EngineEvent.Stage(EngineStage.Translating, 0.4),
                new EngineEvent.Translation("es", [new Sentence(1, 0, 1, "Hola.")]),
            ],
            events);
        Assert.Contains("translate --to es --model madlad-3b --json --from tr", Fake.Read(argsFile));
        var input = Fake.Read(inputFile);
        Assert.Contains("\"Merhaba.\"", input);
        Assert.Contains("\"words\"", input);
    }

    [Fact]
    public async Task TranslateOmitsSourceWhenNull()
    {
        var argsFile = Fake.TempPath("args");
        var engine = Fake.Engine($$"""
            args {{argsFile}}
            out {"event": "translation", "language": "es", "sentences": []}
            """);
        await Fake.Drain(engine.Translate([], "es", model: "madlad-3b"));
        var arguments = Fake.Read(argsFile);
        Assert.Contains("translate --to es --model madlad-3b --json", arguments);
        Assert.DoesNotContain("--from", arguments);
    }

    [Fact]
    public async Task TranslateSurfacesEngineError()
    {
        var engine = Fake.Engine("""
            out {"event": "error", "message": "NLLB için kaynak dil gerekli."}
            exit 1
            """);
        var error = await Assert.ThrowsAsync<EngineException>(() => Fake.Drain(engine.Translate([], "es", null, "nllb-1.3b")));
        Assert.Equal("NLLB için kaynak dil gerekli.", error.Message);
    }

    [Fact]
    public async Task CancelKillsEngineProcessTree()
    {
        var pidFile = Fake.TempPath("pid");
        var childFile = Fake.TempPath("child");
        var engine = Fake.Engine($$"""
            pid {{pidFile}}
            child {{childFile}}
            out {"event": "stage", "stage": "transcribing", "fraction": null}
            sleep 30000
            """);
        using var cts = new CancellationTokenSource();
        var firstEvent = new TaskCompletionSource();
        var consumer = Task.Run(async () =>
        {
            await foreach (var _ in engine.Transcribe(@"C:\v.mp4", Path.GetTempPath(), cancellationToken: cts.Token))
                firstEvent.TrySetResult();
        });
        await firstEvent.Task.WaitAsync(TimeSpan.FromSeconds(20));
        var pid = await Fake.ReadPid(pidFile);
        var child = await Fake.ReadPid(childFile);
        Assert.True(Fake.IsAlive(pid));
        Assert.True(Fake.IsAlive(child));
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => consumer.WaitAsync(TimeSpan.FromSeconds(20)));
        Assert.True(await Fake.WaitUntilDead(pid));
        Assert.True(await Fake.WaitUntilDead(child));
    }

    [Fact]
    public async Task AbandonedEnumerationKillsEngine()
    {
        var pidFile = Fake.TempPath("pid");
        var engine = Fake.Engine($$"""
            pid {{pidFile}}
            out {"event": "stage", "stage": "transcribing", "fraction": null}
            sleep 30000
            """);
        await foreach (var _ in engine.Transcribe(@"C:\v.mp4", Path.GetTempPath()))
            break;
        Assert.True(await Fake.WaitUntilDead(await Fake.ReadPid(pidFile)));
    }

    [Fact]
    public async Task CancelRenderKillsEngine()
    {
        var pidFile = Fake.TempPath("pid");
        var engine = Fake.Engine($$"""
            pid {{pidFile}}
            sleep 30000
            """);
        using var cts = new CancellationTokenSource();
        var render = engine.RenderAsync([new Sentence(1, 0, 1, "A")], SubtitleFormat.Srt, cts.Token);
        var pid = await Fake.ReadPid(pidFile);
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => render.WaitAsync(TimeSpan.FromSeconds(20)));
        Assert.True(await Fake.WaitUntilDead(pid));
    }

    [Fact]
    public async Task GarbageStdoutLineKillsEngineAndThrows()
    {
        var pidFile = Fake.TempPath("pid");
        var engine = Fake.Engine($$"""
            pid {{pidFile}}
            out not JSON
            sleep 30000
            """);
        await Assert.ThrowsAnyAsync<Exception>(() => Fake.Drain(engine.Transcribe(@"C:\v.mp4", Path.GetTempPath())));
        Assert.True(await Fake.WaitUntilDead(await Fake.ReadPid(pidFile)));
    }

    [Fact]
    public async Task TranscribeDoesNotBlockCallerThread()
    {
        // Starting the stream returns immediately; the engine runs while the caller is free.
        var engine = Fake.Engine("""
            sleep 500
            out {"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": [], "sentences": []}
            """);
        var enumerator = engine.Transcribe(@"C:\v.mp4", Path.GetTempPath()).GetAsyncEnumerator();
        var moveNext = enumerator.MoveNextAsync().AsTask();
        Assert.False(moveNext.IsCompleted);
        Assert.True(await moveNext);
        Assert.False(await enumerator.MoveNextAsync());
        await enumerator.DisposeAsync();
    }

    // MARK: - Locating the engine

    private static string MakeDir(params string[] parts)
    {
        var dir = Path.Combine(parts);
        Directory.CreateDirectory(dir);
        return dir;
    }

    private static string Touch(string path)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, "");
        return path;
    }

    [Fact]
    public void LocatesRealDevEngine()
    {
        var env = EngineCommand.ProcessEnvironment();
        env.Remove("SUBTITLE_ENGINE");
        var command = EngineCommand.Locate(env);
        Assert.NotNull(command);
        Assert.Contains("subtitle-engine", command.Arguments);
    }

    [Fact]
    public void EnvironmentVariableWins()
    {
        var command = EngineCommand.Locate(new Dictionary<string, string?> { ["SUBTITLE_ENGINE"] = @"D:\engine\se.exe" }, Path.GetTempPath());
        Assert.Equal(new EngineCommand(@"D:\engine\se.exe"), command);
    }

    [Fact]
    public void FindsBundledEngineNextToApp()
    {
        var app = MakeDir(Fake.TempPath("app"));
        try
        {
            Assert.Null(EngineCommand.BundledEngine(app));
            var exe = Touch(Path.Combine(app, "subtitle-engine", "subtitle-engine.exe"));
            Assert.Equal(exe, EngineCommand.BundledEngine(app));
            Assert.Equal(new EngineCommand(exe), EngineCommand.Locate(new Dictionary<string, string?>(), app));
        }
        finally
        {
            Directory.Delete(app, true);
        }
    }

    [Fact]
    public void DevEngineUsesUvFromKnownLocationsInOrder()
    {
        var root = MakeDir(Fake.TempPath("repo"));
        try
        {
            Touch(Path.Combine(root, "engine", "pyproject.toml"));
            var baseDir = MakeDir(root, "apps", "windows", "bin", "Debug");
            var home = MakeDir(root, "home");
            var appData = MakeDir(root, "appdata");
            var pathDir = MakeDir(root, "pathdir");
            var env = new Dictionary<string, string?>
            {
                ["PATH"] = pathDir + ";" + Path.Combine(root, "missing"),
                ["USERPROFILE"] = home,
                ["APPDATA"] = appData,
            };
            Assert.Null(EngineCommand.Locate(env, baseDir));

            var py39 = Touch(Path.Combine(appData, "Python", "Python39", "Scripts", "uv.exe"));
            Assert.Equal(py39, EngineCommand.Locate(env, baseDir)!.Executable);
            var py312 = Touch(Path.Combine(appData, "Python", "Python312", "Scripts", "uv.exe"));
            Assert.Equal(py312, EngineCommand.Locate(env, baseDir)!.Executable);
            var cargo = Touch(Path.Combine(home, ".cargo", "bin", "uv.exe"));
            Assert.Equal(cargo, EngineCommand.Locate(env, baseDir)!.Executable);
            var local = Touch(Path.Combine(home, ".local", "bin", "uv.exe"));
            Assert.Equal(local, EngineCommand.Locate(env, baseDir)!.Executable);
            var onPath = Touch(Path.Combine(pathDir, "uv.exe"));
            var command = EngineCommand.Locate(env, baseDir)!;
            Assert.Equal(onPath, command.Executable);
            Assert.Equal(["run", "--quiet", "--project", Path.Combine(root, "engine"), "subtitle-engine"], command.Arguments);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void NoEngineWithoutRepo()
    {
        var dir = MakeDir(Fake.TempPath("norepo"));
        try
        {
            // Temp is not inside a repo, so the walk up finds no engine\pyproject.toml.
            Assert.Null(EngineCommand.FindRepoEngine(dir));
            Assert.Null(EngineCommand.Locate(new Dictionary<string, string?>(), dir));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void NotFoundErrorIsLocalized()
    {
        Assert.Equal(EngineError.NotFound, EngineException.NotFound(UILanguage.En).Error);
        Assert.Equal("Subtitle engine not found (is uv installed, or is SUBTITLE_ENGINE set?).", EngineException.NotFound(UILanguage.En).Message);
    }

    [Fact]
    public void SentenceJsonUsesUtf8Unescaped()
    {
        var json = Encoding.UTF8.GetString(System.Text.Json.JsonSerializer.SerializeToUtf8Bytes<IReadOnlyList<Sentence>>(
            [new Sentence(1, 0, 1, "Şöyle")], EngineJson.Options));
        Assert.Equal("""[{"id":1,"start":0,"end":1,"text":"Şöyle","words":[]}]""", json);
    }
}
