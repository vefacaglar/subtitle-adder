namespace SubtitleAdder.EngineKit;

/// <summary>
/// Tells which model a <c>downloading_model</c> stage belongs to. With <c>--diarize</c> the engine can emit two download
/// phases back to back (transcription model, then speaker model) with no other stage in between, and a phase is
/// skipped when that model is already cached. The expected order is predicted from the catalog; a phase boundary is
/// detected when a download follows a non-download stage, or when the fraction drops back after a phase finished.
/// </summary>
public sealed class DownloadPhaseTracker
{
    public enum Kind
    {
        Transcription,
        Diarization,
    }

    private readonly IReadOnlyList<Kind> _expected;
    private int _phase = -1;
    private double? _lastFraction;
    private bool _inDownload;

    /// <summary><paramref name="transcriptionDownloaded"/> / <paramref name="diarizationDownloaded"/> come from the catalog; <paramref name="diarize"/> is the toggle.</summary>
    public DownloadPhaseTracker(bool transcriptionDownloaded, bool diarize, bool diarizationDownloaded)
    {
        var order = new List<Kind>();
        if (!transcriptionDownloaded) order.Add(Kind.Transcription);
        if (diarize && !diarizationDownloaded) order.Add(Kind.Diarization);
        _expected = order;
    }

    /// <summary>Feed every stage event; returns the model being downloaded for <c>downloading_model</c>, <c>null</c> otherwise.</summary>
    public Kind? Update(EngineStage stage, double? fraction)
    {
        if (stage != EngineStage.DownloadingModel)
        {
            _inDownload = false;
            _lastFraction = null;
            return null;
        }
        var droppedBack = (_lastFraction ?? 0) >= 0.99 && (fraction ?? 0) < (_lastFraction ?? 0);
        if (!_inDownload || droppedBack) _phase += 1;
        _inDownload = true;
        _lastFraction = fraction;
        return _phase < _expected.Count ? _expected[_phase] : Kind.Transcription;
    }
}
