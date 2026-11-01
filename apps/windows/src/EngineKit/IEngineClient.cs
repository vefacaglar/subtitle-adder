namespace SubtitleAdder.EngineKit;

/// <summary>
/// The engine operations the app uses. <see cref="EngineClient"/> is the real implementation; view model tests
/// substitute an in-process fake.
/// </summary>
public interface IEngineClient
{
    /// <inheritdoc cref="EngineClient.Transcribe"/>
    IAsyncEnumerable<EngineEvent> Transcribe(
        string video, string outDir, TranscribeOptions? options = null, CancellationToken cancellationToken = default);

    /// <inheritdoc cref="EngineClient.Translate"/>
    IAsyncEnumerable<EngineEvent> Translate(
        IReadOnlyList<Sentence> sentences, string to, string? from = null, string model = "madlad-3b",
        CancellationToken cancellationToken = default);

    /// <inheritdoc cref="EngineClient.RenderAsync"/>
    Task<string> RenderAsync(IReadOnlyList<Sentence> sentences, SubtitleFormat format, CancellationToken cancellationToken = default);

    /// <inheritdoc cref="EngineClient.ModelsAsync"/>
    Task<IReadOnlyList<EngineModel>> ModelsAsync(CancellationToken cancellationToken = default);

    /// <inheritdoc cref="EngineClient.DeleteModelAsync"/>
    Task<int> DeleteModelAsync(string id, CancellationToken cancellationToken = default);
}
