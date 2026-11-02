using System.ComponentModel;

namespace SubtitleAdder.Core;

/// <summary>Observes settings only, coalescing bursts into one deferred UI refresh.</summary>
public sealed class SettingsRefreshSubscription : IDisposable
{
    private static readonly HashSet<string> SettingsProperties =
    [
        nameof(SubtitleViewModel.Language), nameof(SubtitleViewModel.AvailableModels),
        nameof(SubtitleViewModel.Model), nameof(SubtitleViewModel.TranslationModel),
        nameof(SubtitleViewModel.Diarize), nameof(SubtitleViewModel.SpeakerCount),
        nameof(SubtitleViewModel.OfflineMode), nameof(SubtitleViewModel.AutoPlayVideo),
        nameof(SubtitleViewModel.VideoCacheMaxSizeGb),
        nameof(SubtitleViewModel.ExternalLlmEnabled), nameof(SubtitleViewModel.ExternalLlmUrl),
        nameof(SubtitleViewModel.ExternalLlmModel), nameof(SubtitleViewModel.ExternalLlmModels),
        nameof(SubtitleViewModel.ExternalLlmAsync), nameof(SubtitleViewModel.IsLoadingExternalModels),
    ];

    private readonly SubtitleViewModel _viewModel;
    private readonly Action<Action> _enqueue;
    private readonly Action<bool> _refresh;
    private readonly Lock _gate = new();
    private bool _pending;
    private bool _refill;
    private bool _disposed;

    public SettingsRefreshSubscription(SubtitleViewModel viewModel, Action<Action> enqueue, Action<bool> refresh)
    {
        _viewModel = viewModel;
        _enqueue = enqueue;
        _refresh = refresh;
        viewModel.PropertyChanged += OnChanged;
    }

    private void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (string.IsNullOrEmpty(e.PropertyName)) RequestRefresh(refill: true);
        else if (SettingsProperties.Contains(e.PropertyName)) RequestRefresh();
    }

    public void RequestRefresh(bool refill = false)
    {
        lock (_gate)
        {
            if (_disposed) return;
            _refill |= refill;
            if (_pending) return;
            _pending = true;
        }
        _enqueue(() =>
        {
            bool refillControls;
            lock (_gate)
            {
                if (_disposed) return;
                refillControls = _refill;
                _refill = false;
                _pending = false;
            }
            _refresh(refillControls);
        });
    }

    public void Dispose()
    {
        lock (_gate) _disposed = true;
        _viewModel.PropertyChanged -= OnChanged;
    }
}
