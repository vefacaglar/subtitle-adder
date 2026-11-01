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
        var pendingExternalTranslations = new List<Task>();
        using var externalSemaphore = new SemaphoreSlim(1, 1);

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

                // Optional Asynchronous External LLM Mode: hand off translation + auto-save of this video to the external
                // LAN LLM in the background and immediately start transcribing the next video on the local GPU!
                if (IsExternalAsyncActive && Catalog.Engine is { } asyncEngine)
                {
                    var itemSnapshot = nextItem;
                    var sentencesSnapshot = _sentences.ToList();
                    var preTranslatedSnapshot = new Dictionary<Guid, TranslationEntry>(_translations);
                    var sourceSnapshot = TranslationSource;
                    var modelSnapshot = _translationModel;
                    var formatSnapshot = _exportFormat;
                    _activePlaylistItemId = null;
                    UpdatePlaylistItem(itemSnapshot.Id, PlaylistItemState.Working, LocalizedMessage.Key("Çeviri hazırlanıyor…"), null);
                    pendingExternalTranslations.Add(TranslateAndSavePlaylistItemExternallyAsync(
                        asyncEngine,
                        externalSemaphore,
                        itemSnapshot.Id,
                        itemSnapshot.VideoPath,
                        sentencesSnapshot,
                        preTranslatedSnapshot,
                        batchTargetLanguage,
                        sourceSnapshot,
                        modelSnapshot,
                        formatSnapshot,
                        token));
                    continue;
                }

                // Standard Sequential Mode: translate and save before moving to the next video.
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

            if (pendingExternalTranslations.Count > 0)
            {
                await Task.WhenAll(pendingExternalTranslations).ConfigureAwait(true);
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

    private async Task TranslateAndSavePlaylistItemExternallyAsync(
        IEngineClient engine,
        SemaphoreSlim semaphore,
        Guid itemId,
        string videoPath,
        IReadOnlyList<Sentence> sentences,
        IReadOnlyDictionary<Guid, TranslationEntry> preTranslated,
        string target,
        string? source,
        string model,
        SubtitleFormat format,
        CancellationToken token)
    {
        try
        {
            await semaphore.WaitAsync(token).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            await _ui.InvokeAsync(() =>
                UpdatePlaylistItem(itemId, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null)).ConfigureAwait(false);
            return;
        }

        try
        {
            var mergedEntries = new Dictionary<Guid, TranslationEntry>();
            foreach (var s in sentences)
            {
                if (preTranslated.TryGetValue(s.Uid, out var entry) && !entry.IsStale(s) && entry.Language == target && entry.Text.Length > 0)
                {
                    mergedEntries[s.Uid] = entry;
                }
            }

            var remaining = sentences.Where(s => !mergedEntries.ContainsKey(s.Uid)).ToList();
            string? engineError = null;
            if (remaining.Count > 0)
            {
                await foreach (var engineEvent in engine.Translate(remaining, target, source, model, token).ConfigureAwait(false))
                {
                    switch (engineEvent)
                    {
                        case EngineEvent.Stage stage:
                            await _ui.InvokeAsync(() =>
                                UpdatePlaylistItem(
                                    itemId,
                                    PlaylistItemState.Working,
                                    Label(stage.Kind, stage.Fraction, JobPurpose.Translation),
                                    stage.Fraction)).ConfigureAwait(false);
                            break;
                        case EngineEvent.Translation translation:
                            for (var i = 0; i < translation.Sentences.Count; i++)
                            {
                                var tr = translation.Sentences[i];
                                var orig = remaining.FirstOrDefault(s => s.Id == tr.Id) ?? (i < remaining.Count ? remaining[i] : null);
                                if (orig is not null)
                                {
                                    mergedEntries[orig.Uid] = new TranslationEntry(tr.Text, orig.Text, translation.Language);
                                }
                            }
                            await _ui.InvokeAsync(() =>
                            {
                                if (_video == videoPath)
                                {
                                    ApplyTranslation(translation.Sentences, translation.Language, remaining, mergeIntoExisting: true);
                                }
                            }).ConfigureAwait(false);
                            break;
                        case EngineEvent.Error error:
                            engineError = error.Message;
                            break;
                    }
                }
            }

            if (engineError is not null || mergedEntries.Count == 0)
            {
                var msg = engineError is not null
                    ? LocalizedMessage.Verbatim(engineError)
                    : LocalizedMessage.Key("Çeviri tamamlanamadı.");
                await _ui.InvokeAsync(() =>
                    UpdatePlaylistItem(itemId, PlaylistItemState.Failed, msg, null)).ConfigureAwait(false);
                return;
            }

            var finalTranslated = sentences
                .Select(s => mergedEntries.TryGetValue(s.Uid, out var e) && e.Text.Length > 0
                    ? s with { Text = e.Text, Words = [] }
                    : s with { Words = [] })
                .ToList();

            var rendered = await engine.RenderAsync(finalTranslated, format, token).ConfigureAwait(false);
            var outPath = Path.ChangeExtension(videoPath, format.RawValue());
            var savedName = Path.GetFileName(outPath);
            if (Path.GetDirectoryName(outPath) is { Length: > 0 } dir && Directory.Exists(dir))
            {
                await File.WriteAllTextAsync(outPath, rendered, new System.Text.UTF8Encoding(false), token).ConfigureAwait(false);
            }

            await _ui.InvokeAsync(() =>
            {
                if (_video == videoPath)
                {
                    _savedSubtitleName = savedName;
                    if (_status is not JobStatus.Working) Status = JobStatus.FinishedStatus;
                    else OnPropertyChanged(nameof(StatusText));
                }
                var doneMessage = new LocalizedMessage($"saved:{savedName}", () => L("Kaydedildi: %@", savedName));
                UpdatePlaylistItem(itemId, PlaylistItemState.Completed, doneMessage, 1.0);
            }).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            await _ui.InvokeAsync(() =>
                UpdatePlaylistItem(itemId, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null)).ConfigureAwait(false);
        }
        catch (Exception error)
        {
            await _ui.InvokeAsync(() =>
                UpdatePlaylistItem(itemId, PlaylistItemState.Failed, LocalizedMessage.Error(error), null)).ConfigureAwait(false);
        }
        finally
        {
            semaphore.Release();
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
