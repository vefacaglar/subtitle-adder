using System.Text.Json;

namespace SubtitleAdder.EngineKit.Tests;

public class SpeakerTests
{
    private static List<Sentence> Dialogue() =>
    [
        new(1, 0, 1, "Hello.", speaker: 0),
        new(2, 1, 2, "Hi there.", speaker: 1),
        new(3, 2, 3, "How are you?", speaker: 1),
        new(4, 3, 4, "Fine.", speaker: 0),
    ];

    private static Sentence Decode(string json) => JsonSerializer.Deserialize<Sentence>(json, EngineJson.Options)!;

    [Fact]
    public void DecodesSentenceWithSpeaker()
    {
        Assert.Equal(1, Decode("""{"id": 1, "start": 0.0, "end": 1.0, "text": "Hi.", "words": [], "speaker": 1}""").Speaker);
    }

    [Fact]
    public void DecodesSentenceWithoutSpeakerOrNull()
    {
        Assert.Null(Decode("""{"id": 1, "start": 0.0, "end": 1.0, "text": "Hi.", "words": []}""").Speaker);
        Assert.Null(Decode("""{"id": 1, "start": 0.0, "end": 1.0, "text": "Hi.", "words": [], "speaker": null}""").Speaker);
    }

    [Fact]
    public void DecodesSpeakerInResultAndTranslationEvents()
    {
        var result = Assert.IsType<EngineEvent.Result>(EngineEvent.Decode("""{"event": "result", "language": "en", "duration": 2.0, "audio": "a.wav", "outputs": [], "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hi.", "words": [], "speaker": 0}]}"""));
        Assert.Equal(0, result.Value.Sentences[0].Speaker);
        var translation = Assert.IsType<EngineEvent.Translation>(EngineEvent.Decode("""{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hola.", "words": [], "speaker": 1}]}"""));
        Assert.Equal(1, translation.Sentences[0].Speaker);
    }

    [Fact]
    public void DecodesDiarizingStage()
    {
        Assert.Equal(new EngineEvent.Stage(EngineStage.Diarizing, null),
            EngineEvent.Decode("""{"event": "stage", "stage": "diarizing", "fraction": null}"""));
    }

    [Fact]
    public void EncodesSpeakerOnlyWhenPresent()
    {
        Assert.Contains("\"speaker\":2", JsonSerializer.Serialize(new Sentence(1, 0, 1, "A", speaker: 2), EngineJson.Options));
        Assert.DoesNotContain("speaker", JsonSerializer.Serialize(new Sentence(1, 0, 1, "A"), EngineJson.Options));
    }

    [Fact]
    public async Task RenderSendsSpeakerOnStdin()
    {
        var text = await Fake.Engine("cat").RenderAsync(Dialogue(), SubtitleFormat.Srt);
        Assert.Contains("\"speaker\":1", text);
    }

    [Fact]
    public async Task TranslateSendsSpeakerOnStdin()
    {
        var inputFile = Fake.TempPath("input");
        var engine = Fake.Engine($$"""
            stdin {{inputFile}}
            out {"event": "translation", "language": "es", "sentences": []}
            """);
        await Fake.Drain(engine.Translate(Dialogue(), "es"));
        var input = Fake.Read(inputFile);
        Assert.Contains("\"speaker\":0", input);
        Assert.Contains("\"speaker\":1", input);
    }

    [Fact]
    public void TranscribeArgumentsIncludeDiarizeOptions()
    {
        var off = EngineClient.TranscribeArguments(@"C:\v.mp4", @"C:\out", new TranscribeOptions { Speakers = 3 });
        Assert.DoesNotContain("--diarize", off);
        Assert.DoesNotContain("--speakers", off);
        var auto = EngineClient.TranscribeArguments(@"C:\v.mp4", @"C:\out", new TranscribeOptions { Diarize = true });
        Assert.Contains("--diarize", auto);
        Assert.DoesNotContain("--speakers", auto);
        var two = EngineClient.TranscribeArguments(@"C:\v.mp4", @"C:\out", new TranscribeOptions { Diarize = true, Speakers = 2 });
        Assert.Equal(["--diarize", "--speakers", "2"], two.TakeLast(3));
    }

    [Fact]
    public void DecodesDiarizationModelKind()
    {
        var model = JsonSerializer.Deserialize<EngineModel>("""{"id": "speaker-diarization", "kind": "diarization", "backend": "onnx", "languages": null, "size_mb": 40, "disk_mb": null, "speed": 5, "accuracy": 3, "license": "MIT", "downloaded": false}""", EngineJson.Options)!;
        Assert.True(model.IsDiarization);
        Assert.False(model.IsTranscription);
        Assert.False(model.IsTranslation);
    }

    // MARK: - Dash rule

    [Fact]
    public void DashAppliesAtSpeakerChanges()
    {
        var s = Dialogue();
        Assert.Equal([true, true, false, true], Enumerable.Range(0, s.Count).Select(i => s.StartsDialogueTurn(i)));
        Assert.Equal("- Hello.", s.DisplayText(0));
        Assert.Equal("How are you?", s.DisplayText(2));
    }

    [Fact]
    public void DashNeverAppliesWithOneSpeakerOrNone()
    {
        List<Sentence> one = [new(1, 0, 1, "A", speaker: 0), new(2, 1, 2, "B", speaker: 0)];
        List<Sentence> none = [new(1, 0, 1, "A"), new(2, 1, 2, "B")];
        Assert.False(one.StartsDialogueTurn(0));
        Assert.False(none.StartsDialogueTurn(1));
        Assert.Equal("B", none.DisplayText(1));
        Assert.False(one.StartsDialogueTurn(99));
    }

    [Fact]
    public void DashSkipsSentencesWithoutSpeaker()
    {
        List<Sentence> s = [new(1, 0, 1, "A", speaker: 0), new(2, 1, 2, "B"), new(3, 2, 3, "C", speaker: 1)];
        Assert.False(s.StartsDialogueTurn(1));
        Assert.True(s.StartsDialogueTurn(2));
    }

    // MARK: - Editing

    [Fact]
    public void SetSpeakerChangesAndReportsNoOp()
    {
        var s = Dialogue();
        Assert.True(s.SetSpeaker(1, 0));
        Assert.Equal(1, s[0].Speaker);
        Assert.False(s.SetSpeaker(1, 0));
        Assert.False(s.SetSpeaker(1, 9));
        Assert.Equal(2, s.NextNewSpeaker());
        Assert.Equal([0, 1], s.Speakers());
    }

    [Fact]
    public void MergeKeepsFirstSpeaker()
    {
        var s = Dialogue();
        Assert.True(s.MergeWithNext(0));
        Assert.Equal(0, s[0].Speaker);
        Assert.Equal(3, s.Count);
    }

    [Fact]
    public void SplitKeepsSpeakerOnBothHalves()
    {
        List<Sentence> s = [new(1, 0, 4, "one two three four", speaker: 1)];
        Assert.True(s.SplitSentence(0, 2));
        Assert.Equal([1, 1], s.Select(x => x.Speaker));
    }
}

public class DialogueParityTests
{
    private static IReadOnlyList<bool> Dashes(params int?[] speakers) =>
        speakers.Select((speaker, i) => new Sentence(i + 1, i, i + 1, "S", speaker: speaker)).ToList().DialogueDashes();

    /// <summary>Mirrors engine/tests/test_diarize.py (speaker_dashes cases).</summary>
    [Fact]
    public void MatchesEngineSpeakerDashes()
    {
        Assert.Equal([true, false, true, true], Dashes(0, 0, 1, 0));
        Assert.Equal([false, false, false], Dashes(0, 0, 0));
        Assert.Equal([false, false, false], Dashes(null, null, null));
        Assert.Equal([true, false, false, true], Dashes(0, null, 0, 1));
        Assert.Equal([false, false, false], Dashes(0, null, 0));  // one distinct speaker: no dashes
        Assert.Empty(Dashes());
    }

    [Fact]
    public void DashNotDoubledOnAlreadyDashedText()
    {
        Assert.Equal("- Selam.", DialogueDash.Apply(true, "- Selam."));
        Assert.Equal("\u2013 Merhaba.", DialogueDash.Apply(true, "\u2013 Merhaba."));
        Assert.Equal("- -Kısa.", DialogueDash.Apply(true, "-Kısa."));
        Assert.Equal("Selam.", DialogueDash.Apply(false, "Selam."));
        Assert.Equal("- Selam.", DialogueDash.Apply(true, "Selam."));
        Assert.True(DialogueDash.HasDash("\u2013 x"));
    }
}

public class DownloadPhaseTrackerTests
{
    private static List<DownloadPhaseTracker.Kind?> Run(
        bool transcriptionDownloaded, bool diarizationDownloaded, params (EngineStage Stage, double? Fraction)[] events)
    {
        var tracker = new DownloadPhaseTracker(transcriptionDownloaded, diarize: true, diarizationDownloaded);
        return events.Select(e => tracker.Update(e.Stage, e.Fraction)).ToList();
    }

    private const DownloadPhaseTracker.Kind T = DownloadPhaseTracker.Kind.Transcription;
    private const DownloadPhaseTracker.Kind D = DownloadPhaseTracker.Kind.Diarization;

    [Fact]
    public void BothMissingBackToBack()
    {
        var kinds = Run(false, false,
            (EngineStage.ExtractingAudio, null), (EngineStage.DownloadingModel, 0), (EngineStage.DownloadingModel, 0.6),
            (EngineStage.DownloadingModel, 1), (EngineStage.DownloadingModel, 0), (EngineStage.DownloadingModel, 0.5),
            (EngineStage.DownloadingModel, 1), (EngineStage.LoadingModel, null));
        Assert.Equal([null, T, T, T, D, D, D, null], kinds);
    }

    [Fact]
    public void OnlySpeakerModelMissing()
    {
        Assert.Equal([D, D, null], Run(true, false,
            (EngineStage.DownloadingModel, 0), (EngineStage.DownloadingModel, 1), (EngineStage.LoadingModel, null)));
    }

    [Fact]
    public void OnlyTranscriptionModelMissing()
    {
        Assert.Equal([T, T, null], Run(false, true,
            (EngineStage.DownloadingModel, 0), (EngineStage.DownloadingModel, 1), (EngineStage.LoadingModel, null)));
    }

    [Fact]
    public void NothingMissingStillLabelsUnexpectedDownload()
    {
        Assert.Equal([T], Run(true, true, (EngineStage.DownloadingModel, 0.2)));
    }

    [Fact]
    public void SeparatedPhasesByAnotherStage()
    {
        Assert.Equal([T, null, D], Run(false, false,
            (EngineStage.DownloadingModel, 1), (EngineStage.LoadingModel, null), (EngineStage.DownloadingModel, 0.1)));
    }

    [Fact]
    public void DiarizeOffExpectsNoSpeakerPhase()
    {
        var tracker = new DownloadPhaseTracker(transcriptionDownloaded: false, diarize: false, diarizationDownloaded: false);
        Assert.Equal(T, tracker.Update(EngineStage.DownloadingModel, 1));
    }
}
