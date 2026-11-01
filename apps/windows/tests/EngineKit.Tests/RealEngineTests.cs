namespace SubtitleAdder.EngineKit.Tests;

/// <summary>
/// End to end with the real dev engine via uv: <c>set ENGINE_E2E=1 &amp;&amp; dotnet test</c>. Needs no downloaded models:
/// models / render / errors run without speech recognition.
/// </summary>
public class RealEngineTests
{
    private static EngineClient Engine(UILanguage language)
    {
        var env = EngineCommand.ProcessEnvironment();
        env.Remove("SUBTITLE_ENGINE");
        var command = EngineCommand.Locate(env) ?? throw EngineException.NotFound();
        return new EngineClient(command, language);
    }

    [E2EFact]
    public async Task ListsCatalog()
    {
        var models = await Engine(UILanguage.Tr).ModelsAsync();
        var ids = models.Select(m => m.Id).ToList();
        Assert.Contains("small", ids);
        Assert.Contains("parakeet-v3", ids);
        Assert.True(models.Single(m => m.Id == "speaker-diarization").IsDiarization);
        Assert.Equal("MIT AND Apache-2.0", models.Single(m => m.Id == "speaker-diarization").License);
        Assert.True(models.Single(m => m.Id == "nllb-1.3b").HasNonCommercialLicense);
        Assert.True(models.Single(m => m.Id == "madlad-3b").IsTranslation);
        Assert.Equal(["ru"], models.Single(m => m.Id == "gigaam-v3").Languages!);
        Assert.Null(models.Single(m => m.Id == "small").Languages);
    }

    [E2EFact]
    public async Task RendersSrtWithSpeakerDashes()
    {
        List<Sentence> sentences =
        [
            new(1, 0, 1.5, "Merhaba, nasılsın?", speaker: 0),
            new(2, 1.5, 3, "İyiyim, sağ ol.", speaker: 1),
            new(3, 3, 4, "Sen nasılsın?", speaker: 1),
        ];
        var srt = (await Engine(UILanguage.Tr).RenderAsync(sentences, SubtitleFormat.Srt)).Replace("\r\n", "\n");
        Assert.StartsWith("1\n00:00:00,000 --> 00:00:01,500\n- Merhaba, nasılsın?\n", srt);
        Assert.Contains("- İyiyim, sağ ol.", srt);
        Assert.Contains("\nSen nasılsın?", srt);
        Assert.DoesNotContain("- Sen nasılsın?", srt);
    }

    [E2EFact]
    public async Task RendersVttWithoutDashesForOneSpeaker()
    {
        List<Sentence> sentences = [new(1, 0, 1, "A.", speaker: 0), new(2, 1, 2, "B.", speaker: 0)];
        var vtt = await Engine(UILanguage.En).RenderAsync(sentences, SubtitleFormat.Vtt);
        Assert.StartsWith("WEBVTT", vtt);
        Assert.DoesNotContain("- ", vtt);
    }

    [E2EFact]
    public async Task DeletingUnknownModelIsLocalizedError()
    {
        var tr = await Assert.ThrowsAsync<EngineException>(() => Engine(UILanguage.Tr).DeleteModelAsync("not-a-model"));
        var en = await Assert.ThrowsAsync<EngineException>(() => Engine(UILanguage.En).DeleteModelAsync("not-a-model"));
        Assert.Equal(EngineError.Failed, tr.Error);
        Assert.NotEmpty(tr.Message);
        Assert.NotEqual(tr.Message, en.Message);
    }

    [E2EFact]
    public async Task TranscribeRejectsNonTranscriptionModelBeforeDownload()
    {
        var video = Path.Combine(Path.GetTempPath(), "missing-video.mp4");
        var error = await Assert.ThrowsAsync<EngineException>(() => Fake.Drain(Engine(UILanguage.En).Transcribe(
            video, Path.GetTempPath(), new TranscribeOptions { Model = "speaker-diarization" })));
        Assert.NotEmpty(error.Message);
        Assert.DoesNotContain("error code", error.Message);
    }
}
