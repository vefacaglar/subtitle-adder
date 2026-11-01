using CommunityToolkit.Mvvm.Input;
using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

public enum PlaylistItemState
{
    Pending,
    Working,
    Completed,
    Failed,
}

public sealed record PlaylistItem(
    Guid Id,
    string VideoPath,
    string SourceLanguage,
    PlaylistItemState State,
    LocalizedMessage StatusMessage,
    double? Fraction = null)
{
    public string FileName => Path.GetFileName(VideoPath);
    public string FolderPath => Path.GetDirectoryName(VideoPath) ?? "";
    public string StatusText => StatusMessage.Text;
    public SubtitleLanguage SelectedSourceLanguage =>
        SubtitleLanguage.FromCode(SourceLanguage) ?? SubtitleLanguage.Automatic;
    public bool IsPending => State == PlaylistItemState.Pending;
    public bool IsWorking => State == PlaylistItemState.Working;
    public bool IsCompleted => State == PlaylistItemState.Completed;
    public bool IsFailed => State == PlaylistItemState.Failed;
    public bool CanEdit => State != PlaylistItemState.Working;
}

/// <summary>
/// Batch playlist queue: users can queue multiple videos, choose each file's source language individually
/// (or set all to automatic), and translate + save all of them sequentially into their respective folders
/// using the selected target translation language.
/// </summary>
public sealed partial class SubtitleViewModel
{
    private IReadOnlyList<PlaylistItem> _playlist = [];
    private bool _isPlaylistRunning;
    private Guid? _activePlaylistItemId;
    private CancellationTokenSource? _playlistCts;

    public IReadOnlyList<PlaylistItem> Playlist
    {
        get => _playlist;
        private set
        {
            if (!SetProperty(ref _playlist, value)) return;
            OnPropertyChanged(nameof(HasPlaylistItems));
            OnPropertyChanged(nameof(PlaylistCount));
            OnPropertyChanged(nameof(CanStartPlaylist));
            OnPropertyChanged(nameof(PlaylistSummaryText));
            StartPlaylistCommand?.NotifyCanExecuteChanged();
        }
    }

    public bool HasPlaylistItems => _playlist.Count > 0;

    public int PlaylistCount => _playlist.Count;

    public bool IsPlaylistRunning
    {
        get => _isPlaylistRunning;
        private set
        {
            if (!SetProperty(ref _isPlaylistRunning, value)) return;
            OnPropertyChanged(nameof(IsWorking));
            OnPropertyChanged(nameof(CanStartPlaylist));
            OnPropertyChanged(nameof(PlaylistSummaryText));
            RaiseCommands();
        }
    }

    public bool CanStartPlaylist =>
        !IsWorking && _playlist.Any(i => i.State is PlaylistItemState.Pending or PlaylistItemState.Failed);

    public string PlaylistSummaryText
    {
        get
        {
            if (_playlist.Count == 0) return L("Listede henüz video yok.");
            var completed = _playlist.Count(i => i.State == PlaylistItemState.Completed);
            return L("%lld / %lld tamamlandı", (long)completed, (long)_playlist.Count);
        }
    }

    /// <summary>Adds one or more video files to the batch playlist (defaulting each item's source language to the current toolbar selection).</summary>
    public void AddToPlaylist(IEnumerable<string> videoPaths, string? defaultSourceLanguage = null)
    {
        var sourceLang = defaultSourceLanguage ?? _language;
        if (SubtitleLanguage.FromCode(sourceLang) is null) sourceLang = "";
        var updated = new List<PlaylistItem>(_playlist);
        var changed = false;
        foreach (var rawPath in videoPaths)
        {
            if (string.IsNullOrWhiteSpace(rawPath)) continue;
            if (updated.Any(i => string.Equals(i.VideoPath, rawPath, StringComparison.OrdinalIgnoreCase) &&
                                 i.State is PlaylistItemState.Pending or PlaylistItemState.Working))
            {
                continue;
            }
            updated.Add(new PlaylistItem(
                Guid.NewGuid(),
                rawPath,
                sourceLang,
                PlaylistItemState.Pending,
                LocalizedMessage.Key("Sırada")));
            changed = true;
        }
        if (changed) Playlist = updated;
    }

    /// <summary>Sets the source language for a single playlist item.</summary>
    public void SetPlaylistItemLanguage(Guid id, string languageCode)
    {
        var normalized = SubtitleLanguage.FromCode(languageCode)?.Code ?? "";
        var index = -1;
        for (var i = 0; i < _playlist.Count; i++)
        {
            if (_playlist[i].Id == id)
            {
                index = i;
                break;
            }
        }
        if (index < 0 || _playlist[index].SourceLanguage == normalized) return;
        if (_playlist[index].State == PlaylistItemState.Working) return;
        var updated = new List<PlaylistItem>(_playlist)
        {
            [index] = _playlist[index] with { SourceLanguage = normalized },
        };
        Playlist = updated;
    }

    /// <summary>Sets the source language for all non-working items in the playlist (e.g. <c>""</c> to make all automatic).</summary>
    public void SetAllPlaylistLanguages(string languageCode)
    {
        var normalized = SubtitleLanguage.FromCode(languageCode)?.Code ?? "";
        var changed = false;
        var updated = new List<PlaylistItem>(_playlist.Count);
        foreach (var item in _playlist)
        {
            if (item.State != PlaylistItemState.Working && item.SourceLanguage != normalized)
            {
                updated.Add(item with { SourceLanguage = normalized });
                changed = true;
            }
            else
            {
                updated.Add(item);
            }
        }
        if (changed) Playlist = updated;
    }

    public void RemoveFromPlaylist(Guid id)
    {
        if (_activePlaylistItemId == id && _isPlaylistRunning) return;
        var updated = _playlist.Where(i => i.Id != id).ToList();
        if (updated.Count != _playlist.Count) Playlist = updated;
    }

    public void ClearPlaylist()
    {
        if (_isPlaylistRunning)
        {
            Playlist = [.. _playlist.Where(i => i.Id == _activePlaylistItemId)];
            return;
        }
        Playlist = [];
    }

    /// <summary>Starts processing all pending/failed items in the playlist sequentially.</summary>
    public void StartPlaylist()
    {
        if (!CanStartPlaylist) return;
        _ = RunPlaylistAsync();
    }

    public async Task RunPlaylistAsync()
    {
        if (_isPlaylistRunning) return;
        if (Catalog.Engine is null)
        {
            Status = new JobStatus.Failed(LocalizedMessage.Error(EngineException.NotFound()));
            return;
        }
        var neededList = new List<string> { _model, _translationModel };
        if (_diarize) neededList.Add(DiarizationModelId);
        if (BlockedOffline(neededList, StartPlaylist)) return;

        _playlistCts?.Cancel();
        _playlistCts?.Dispose();
        var cts = new CancellationTokenSource();
        _playlistCts = cts;
        var token = cts.Token;

        // Lock the user's chosen target translation language for the entire batch run.
        var batchTargetLanguage = _translationTarget;
        var originalSourceLanguage = _language;

        IsPlaylistRunning = true;
        try
        {
            while (!token.IsCancellationRequested)
            {
                var nextItem = _playlist.FirstOrDefault(i => i.State is PlaylistItemState.Pending or PlaylistItemState.Failed);
                if (nextItem is null) break;

                _activePlaylistItemId = nextItem.Id;
                UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Working, LocalizedMessage.Key("Başlıyor…"), null);

                // Apply this item's specific source language (or "" for automatic detection) and the shared target language.
                Language = nextItem.SourceLanguage;
                TranslationTarget = batchTargetLanguage;

                // Step 1: Open the video cleanly (auto-loads sidecar subtitle if one already exists).
                if (_video == nextItem.VideoPath) _video = null;
                OpenVideo(nextItem.VideoPath);
                if (token.IsCancellationRequested)
                {
                    UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null);
                    break;
                }

                // Step 2: Run speech recognition if sentences aren't already loaded.
                if (_sentences.Count == 0)
                {
                    GenerateCore(chainTranslate: false);
                    await CurrentJob.ConfigureAwait(true);
                }

                if (token.IsCancellationRequested)
                {
                    UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null);
                    break;
                }

                if (_status is JobStatus.Failed || _sentences.Count == 0)
                {
                    var errMsg = _status is JobStatus.Failed f1 ? f1.Message : LocalizedMessage.Verbatim(StatusText);
                    UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Failed, errMsg, null);
                    continue;
                }

                // Step 3: Translate into the user's selected target language.
                TranslationTarget = batchTargetLanguage;
                Translate();
                await CurrentJob.ConfigureAwait(true);

                if (token.IsCancellationRequested)
                {
                    UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null);
                    break;
                }

                if (_status is JobStatus.Failed || _translations.Count == 0)
                {
                    var errMsg = _status is JobStatus.Failed f2 ? f2.Message : LocalizedMessage.Verbatim(StatusText);
                    UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Failed, errMsg, null);
                    continue;
                }

                // Step 4: Always save the translated subtitle to the video's folder in playlist mode.
                if (_savedSubtitleName is null && VideoSubtitlePath is not null && HasSentences)
                {
                    try
                    {
                        ExportLanguage = ExportLanguage.Translated;
                        await SaveSubtitleToVideoFolderAsync().ConfigureAwait(true);
                    }
                    catch (Exception error) when (error is IOException or UnauthorizedAccessException)
                    {
                        UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Failed, LocalizedMessage.Error(error), null);
                        continue;
                    }
                }

                var doneMessage = _savedSubtitleName is { } saved
                    ? new LocalizedMessage($"saved:{saved}", () => L("Kaydedildi: %@", saved))
                    : LocalizedMessage.Key("Tamamlandı");
                UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Completed, doneMessage, 1.0);
            }
        }
        finally
        {
            _activePlaylistItemId = null;
            TranslationTarget = batchTargetLanguage;
            Language = originalSourceLanguage;
            IsPlaylistRunning = false;
        }
    }

    private void SyncActivePlaylistItemProgress()
    {
        if (!_isPlaylistRunning || _activePlaylistItemId is not { } activeId) return;
        if (_status is JobStatus.Working working)
        {
            UpdatePlaylistItem(activeId, PlaylistItemState.Working, working.Label, working.Fraction);
        }
    }

    private void UpdatePlaylistItem(Guid id, PlaylistItemState state, LocalizedMessage statusMessage, double? fraction)
    {
        var index = -1;
        for (var i = 0; i < _playlist.Count; i++)
        {
            if (_playlist[i].Id == id)
            {
                index = i;
                break;
            }
        }
        if (index < 0) return;
        var updated = new List<PlaylistItem>(_playlist)
        {
            [index] = _playlist[index] with { State = state, StatusMessage = statusMessage, Fraction = fraction },
        };
        Playlist = updated;
    }
}
