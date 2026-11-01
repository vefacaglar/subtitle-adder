using System.Runtime.CompilerServices;
using System.Threading.Channels;
using SubtitleAdder.Core;

// Several tests switch the process-wide LanguageState; run them one at a time.
[assembly: CollectionBehavior(DisableTestParallelization = true)]

namespace SubtitleAdder.Core.Tests;

/// <summary>An in-process engine: streams are fed through channels so tests decide when each event arrives.</summary>
internal sealed class FakeEngineClient : IEngineClient
{
    public sealed record TranscribeCall(string Video, string OutDir, TranscribeOptions Options);

    public sealed record TranslateCall(IReadOnlyList<Sentence> Sentences, string To, string? From, string Model);

    public List<TranscribeCall> TranscribeCalls { get; } = [];
    public List<TranslateCall> TranslateCalls { get; } = [];
    public List<(IReadOnlyList<Sentence> Sentences, SubtitleFormat Format)> RenderCalls { get; } = [];
    public List<string> DeleteCalls { get; } = [];
    public int ModelsCalls { get; private set; }

    /// <summary>Events of the next stream; complete it (optionally with an exception) to end the stream.</summary>
    public Channel<EngineEvent> Events { get; private set; } = Channel.CreateUnbounded<EngineEvent>();

    /// <summary>Set when a stream's enumeration observed cancellation (the real client kills the engine then).</summary>
    public TaskCompletionSource StreamCancelled { get; private set; } = new(TaskCreationOptions.RunContinuationsAsynchronously);

    public IReadOnlyList<EngineModel> CatalogModels { get; set; } = ModelCatalog.FallbackModels;
    public Exception? ModelsError { get; set; }
    public Func<string, int> Delete { get; set; } = _ => 100;
    public Func<IReadOnlyList<Sentence>, SubtitleFormat, string> Render { get; set; } = (list, _) => string.Join("|", list.Select(s => s.Text));

    /// <summary>Starts a fresh channel for the next job (a channel is consumed by one stream).</summary>
    public void Reset()
    {
        Events = Channel.CreateUnbounded<EngineEvent>();
        StreamCancelled = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }

    /// <summary>Writes events and ends the stream.</summary>
    public void Finish(params EngineEvent[] events)
    {
        foreach (var e in events) Events.Writer.TryWrite(e);
        Events.Writer.TryComplete();
    }

    public IAsyncEnumerable<EngineEvent> Transcribe(
        string video, string outDir, TranscribeOptions? options = null, CancellationToken cancellationToken = default)
    {
        TranscribeCalls.Add(new(video, outDir, options ?? new TranscribeOptions()));
        return Stream(Events, StreamCancelled, cancellationToken);
    }

    public IAsyncEnumerable<EngineEvent> Translate(
        IReadOnlyList<Sentence> sentences, string to, string? from = null, string model = "madlad-3b",
        CancellationToken cancellationToken = default)
    {
        TranslateCalls.Add(new(sentences, to, from, model));
        return Stream(Events, StreamCancelled, cancellationToken);
    }

    private static async IAsyncEnumerable<EngineEvent> Stream(
        Channel<EngineEvent> channel, TaskCompletionSource cancelled, [EnumeratorCancellation] CancellationToken cancellationToken)
    {
        while (true)
        {
            EngineEvent next;
            try
            {
                if (!await channel.Reader.WaitToReadAsync(cancellationToken)) yield break;
                if (!channel.Reader.TryRead(out next!)) continue;
            }
            catch (OperationCanceledException)
            {
                cancelled.TrySetResult();
                throw;
            }
            yield return next;
        }
    }

    public Task<string> RenderAsync(IReadOnlyList<Sentence> sentences, SubtitleFormat format, CancellationToken cancellationToken = default)
    {
        RenderCalls.Add((sentences, format));
        return Task.FromResult(Render(sentences, format));
    }

    public Task<IReadOnlyList<EngineModel>> ModelsAsync(CancellationToken cancellationToken = default)
    {
        ModelsCalls++;
        return ModelsError is { } error ? Task.FromException<IReadOnlyList<EngineModel>>(error) : Task.FromResult(CatalogModels);
    }

    public Task<int> DeleteModelAsync(string id, CancellationToken cancellationToken = default)
    {
        DeleteCalls.Add(id);
        try
        {
            return Task.FromResult(Delete(id));
        }
        catch (Exception error)
        {
            return Task.FromException<int>(error);
        }
    }
}

/// <summary>Sets the process-wide UI language for a test and restores it afterwards.</summary>
internal sealed class LanguageScope : IDisposable
{
    private readonly UILanguage _previous = LanguageState.Shared.Language;

    public LanguageScope(UILanguage language) => LanguageState.Shared.Language = language;

    public void Dispose() => LanguageState.Shared.Language = _previous;
}

internal static class Make
{
    public static (SubtitleViewModel Vm, FakeEngineClient Engine, InMemorySettingsStore Settings) ViewModel(
        InMemorySettingsStore? settings = null, bool engine = true)
    {
        var fake = new FakeEngineClient();
        settings ??= new InMemorySettingsStore();
        var vm = new SubtitleViewModel(new ModelCatalog(engine ? fake : null), settings);
        return (vm, fake, settings);
    }

    public static EngineModel Model(string id, bool downloaded = false, string kind = "transcription", int accuracy = 3, int speed = 3,
        string backend = "whisper", IReadOnlyList<string>? languages = null, int? diskMB = null) => new()
        {
            Id = id, Kind = kind, Backend = backend, Languages = languages, SizeMB = 100, DiskMB = diskMB,
            Speed = speed, Accuracy = accuracy, License = "MIT", Downloaded = downloaded,
        };

    public static EngineEvent Result(params Sentence[] sentences) => Result("tr", "", sentences);

    public static EngineEvent Result(string language, string audio, params Sentence[] sentences) =>
        new EngineEvent.Result(new EngineResult
        {
            Language = language, Duration = 10, Audio = audio, Outputs = [], Sentences = sentences,
        });

    public static async Task Eventually(Func<bool> condition, string because = "condition")
    {
        for (var i = 0; i < 200; i++)
        {
            if (condition()) return;
            await Task.Delay(10);
        }
        Assert.Fail($"Timed out waiting for {because}");
    }
}
