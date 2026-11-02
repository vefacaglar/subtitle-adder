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
/// Multi-stage pipelined batch playlist queue: overlaps Stage 1 CPU audio extraction (lookahead),
/// Stage 2 GPU speech recognition / diarization, and Stage 3 translation + auto-save across queued videos.
/// </summary>
public sealed partial class SubtitleViewModel
{
    private const int MaxAudioLookahead = 1;

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
        if (_isPlaylistRunning && (_activePlaylistItemId == id || _playlist.Any(i => i.Id == id && i.State == PlaylistItemState.Working)))
        {
            return;
        }
        var updated = _playlist.Where(i => i.Id != id).ToList();
        if (updated.Count != _playlist.Count) Playlist = updated;
    }

    public void ClearPlaylist()
    {
        if (_isPlaylistRunning)
        {
            Playlist = [.. _playlist.Where(i => i.Id == _activePlaylistItemId || i.State == PlaylistItemState.Working)];
            return;
        }
        Playlist = [];
    }

    /// <summary>Starts processing all pending/failed items in the playlist using the 3-stage pipeline.</summary>
    public void StartPlaylist()
    {
        if (!CanStartPlaylist) return;
        _ = RunPlaylistAsync();
    }

    public async Task RunPlaylistAsync()
    {
        if (_isPlaylistRunning) return;
        if (Catalog.Engine is not { } engine)
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
        var outDir = Path.Combine(Path.GetTempPath(), "SubtitleAdder");
        var stage2StartedIds = new HashSet<Guid>();
        var preExtractTasks = new Dictionary<Guid, Task>();
        var preExtractedDoneIds = new HashSet<Guid>();
        var pendingTranslations = new List<Task>();
        using var audioExtractSemaphore = new SemaphoreSlim(1, 1);
        using var translationSemaphore = new SemaphoreSlim(1, 1);

        Task EnsureAudioExtractionStarted(PlaylistItem item)
        {
            if (preExtractTasks.TryGetValue(item.Id, out var existingTask))
            {
                return existingTask;
            }
            if (SubtitleParser.FindSidecarSubtitle(item.VideoPath) is not null)
            {
                preExtractedDoneIds.Add(item.Id);
                var completed = Task.CompletedTask;
                preExtractTasks[item.Id] = completed;
                return completed;
            }
            var task = PreExtractAudioForItemAsync(
                engine,
                audioExtractSemaphore,
                item.Id,
                item.VideoPath,
                outDir,
                stage2StartedIds,
                preExtractedDoneIds,
                token);
            preExtractTasks[item.Id] = task;
            return task;
        }

        void ScheduleLookahead()
        {
            if (token.IsCancellationRequested) return;
            var upcoming = _playlist
                .Where(i => !stage2StartedIds.Contains(i.Id) &&
                            (i.State is PlaylistItemState.Pending or PlaylistItemState.Failed || preExtractTasks.ContainsKey(i.Id)))
                .Take(MaxAudioLookahead)
                .ToList();
            foreach (var candidate in upcoming)
            {
                if (preExtractTasks.ContainsKey(candidate.Id)) continue;
                if (SubtitleParser.FindSidecarSubtitle(candidate.VideoPath) is not null) continue;
                _ = EnsureAudioExtractionStarted(candidate);
            }
        }

        IsPlaylistRunning = true;
        try
        {
            while (!token.IsCancellationRequested)
            {
                var nextItem = _playlist.FirstOrDefault(i =>
                    !stage2StartedIds.Contains(i.Id) &&
                    (i.State is PlaylistItemState.Pending or PlaylistItemState.Failed || preExtractTasks.ContainsKey(i.Id)));
                if (nextItem is null) break;

                // Step 0 (Stage 1 — Single-Concurrency Audio Extraction):
                // Ensure nextItem's audio extraction is 100% finished BEFORE starting lookahead extraction for the
                // following video (item[i + 1]) and BEFORE entering Stage 2 GPU ASR, so 2 audio extractions never run at once.
                var extractTask = EnsureAudioExtractionStarted(nextItem);
                if (!extractTask.IsCompleted)
                {
                    await extractTask.ConfigureAwait(true);
                }

                if (token.IsCancellationRequested)
                {
                    UpdatePlaylistItem(nextItem.Id, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null);
                    break;
                }

                stage2StartedIds.Add(nextItem.Id);
                _activePlaylistItemId = nextItem.Id;

                // Now that nextItem's audio is extracted and nextItem is entering Stage 2 (GPU ASR),
                // start lookahead audio extraction for at most 1 upcoming video (item[i + 1]).
                ScheduleLookahead();

                UpdatePlaylistItem(
                    nextItem.Id,
                    PlaylistItemState.Working,
                    LocalizedMessage.Key("Başlıyor…"),
                    preExtractedDoneIds.Contains(nextItem.Id) ? 1.0 : null);

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

                // Step 2 (Stage 2 — ASR / Diarization): Run speech recognition if sentences aren't already loaded.
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

                // Step 3 (Stage 3 — Pipelined Translation + Auto-Save):
                // Hand off translation and saving of this item to Stage 3 in the background and immediately
                // advance Stage 2 to transcribe the next video in the playlist!
                var itemSnapshot = nextItem;
                var sentencesSnapshot = _sentences.ToList();
                var preTranslatedSnapshot = new Dictionary<Guid, TranslationEntry>(_translations);
                var sourceSnapshot = TranslationSource;
                var targetSnapshot = sourceSnapshot == batchTargetLanguage ? _translationTarget : batchTargetLanguage;
                var modelSnapshot = _translationModel;
                var formatSnapshot = _exportFormat;
                _activePlaylistItemId = null;

                var initFraction = preTranslatedSnapshot.Count > 0 && sentencesSnapshot.Count > 0
                    ? Math.Clamp((double)preTranslatedSnapshot.Count / sentencesSnapshot.Count, 0.0, 0.99)
                    : (double?)null;
                var initMsg = initFraction is { } initF
                    ? Label(EngineStage.Translating, initF, JobPurpose.Translation)
                    : LocalizedMessage.Key("Çeviri hazırlanıyor…");
                UpdatePlaylistItem(itemSnapshot.Id, PlaylistItemState.Working, initMsg, initFraction);

                pendingTranslations.Add(TranslateAndSavePlaylistItemAsync(
                    engine,
                    translationSemaphore,
                    itemSnapshot.Id,
                    itemSnapshot.VideoPath,
                    sentencesSnapshot,
                    preTranslatedSnapshot,
                    targetSnapshot,
                    sourceSnapshot,
                    modelSnapshot,
                    formatSnapshot,
                    token));
            }

            if (pendingTranslations.Count > 0)
            {
                if (pendingTranslations.Any(t => !t.IsCompleted) && !token.IsCancellationRequested)
                {
                    Status = new JobStatus.Working(Label(EngineStage.Translating, null, JobPurpose.Translation), null);
                }
                await Task.WhenAll(pendingTranslations).ConfigureAwait(true);
                if (!token.IsCancellationRequested && _status is JobStatus.Working)
                {
                    Status = JobStatus.FinishedStatus;
                }
            }

            if (preExtractTasks.Count > 0)
            {
                await Task.WhenAll(preExtractTasks.Values).ConfigureAwait(true);
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

    private async Task PreExtractAudioForItemAsync(
        IEngineClient engine,
        SemaphoreSlim audioSemaphore,
        Guid itemId,
        string videoPath,
        string outDir,
        HashSet<Guid> stage2StartedIds,
        HashSet<Guid> preExtractedDoneIds,
        CancellationToken token)
    {
        try
        {
            await audioSemaphore.WaitAsync(token).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            return;
        }

        try
        {
            var shouldRun = false;
            await _ui.InvokeAsync(() =>
            {
                if (token.IsCancellationRequested || stage2StartedIds.Contains(itemId)) return;
                if (_playlist.All(i => i.Id != itemId)) return;
                shouldRun = true;
            }).ConfigureAwait(false);
            if (!shouldRun) return;

            var sawEvents = false;
            await foreach (var engineEvent in engine.ExtractAudio(videoPath, outDir, token).ConfigureAwait(false))
            {
                sawEvents = true;
                switch (engineEvent)
                {
                    case EngineEvent.Stage stage:
                        await _ui.InvokeAsync(() =>
                        {
                            if (token.IsCancellationRequested) return;
                            if (stage2StartedIds.Contains(itemId) && _activePlaylistItemId != itemId) return;
                            UpdatePlaylistItem(
                                itemId,
                                PlaylistItemState.Working,
                                Label(stage.Kind, stage.Fraction, JobPurpose.Transcription),
                                stage.Fraction);
                        }).ConfigureAwait(false);
                        break;
                    case EngineEvent.Result result:
                        await _ui.InvokeAsync(() =>
                        {
                            if (result.Value.Audio is { Length: > 0 } audioPath)
                            {
                                _sessionTempFiles.Add(audioPath);
                            }
                        }).ConfigureAwait(false);
                        break;
                }
            }

            if (sawEvents)
            {
                await _ui.InvokeAsync(() =>
                {
                    if (token.IsCancellationRequested) return;
                    preExtractedDoneIds.Add(itemId);
                    if (!stage2StartedIds.Contains(itemId))
                    {
                        UpdatePlaylistItem(
                            itemId,
                            PlaylistItemState.Pending,
                            LocalizedMessage.Key("Ses hazır · Sırada"),
                            1.0);
                    }
                }).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException)
        {
            await _ui.InvokeAsync(() =>
            {
                if (!stage2StartedIds.Contains(itemId))
                {
                    UpdatePlaylistItem(itemId, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null);
                }
            }).ConfigureAwait(false);
        }
        catch (Exception)
        {
            // Leave the item in Pending so Stage 2's Transcribe call surfaces the canonical error in playlist order.
            await _ui.InvokeAsync(() =>
            {
                if (!stage2StartedIds.Contains(itemId))
                {
                    UpdatePlaylistItem(itemId, PlaylistItemState.Pending, LocalizedMessage.Key("Sırada"), null);
                }
            }).ConfigureAwait(false);
        }
        finally
        {
            audioSemaphore.Release();
        }
    }

    private async Task TranslateAndSavePlaylistItemAsync(
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
            if (source == target)
            {
                await _ui.InvokeAsync(() =>
                    UpdatePlaylistItem(itemId, PlaylistItemState.Failed, LocalizedMessage.Key("Kaynak ve hedef dil aynı olamaz."), null)).ConfigureAwait(false);
                return;
            }
            if (model == "nllb-1.3b" && source is null && !IsExternalLlmActive)
            {
                await _ui.InvokeAsync(() =>
                    UpdatePlaylistItem(itemId, PlaylistItemState.Failed, LocalizedMessage.Key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç."), null)).ConfigureAwait(false);
                return;
            }

            var mergedEntries = new Dictionary<Guid, TranslationEntry>();
            foreach (var s in sentences)
            {
                if (preTranslated.TryGetValue(s.Uid, out var entry) && IsReusableTranslation(s, entry, target))
                {
                    mergedEntries[s.Uid] = entry;
                }
            }

            var preDoneCount = mergedEntries.Count;
            var totalCount = Math.Max(1, sentences.Count);
            var remaining = sentences.Where(s => !mergedEntries.ContainsKey(s.Uid)).ToList();
            string? engineError = null;
            if (remaining.Count > 0)
            {
                var contextualRequest = WithTranslationContext(sentences, remaining);
                await foreach (var engineEvent in engine.Translate(contextualRequest, target, source, model, token).ConfigureAwait(false))
                {
                    switch (engineEvent)
                    {
                        case EngineEvent.Stage stage:
                            var fraction = stage.Fraction;
                            if (preDoneCount > 0 && stage.Kind == EngineStage.Translating && fraction is { } f)
                            {
                                fraction = Math.Clamp((preDoneCount + remaining.Count * Math.Clamp(f, 0.0, 1.0)) / totalCount, 0.0, 1.0);
                            }
                            await _ui.InvokeAsync(() =>
                            {
                                UpdatePlaylistItem(
                                    itemId,
                                    PlaylistItemState.Working,
                                    Label(stage.Kind, fraction, JobPurpose.Translation),
                                    fraction);
                                if (_video == videoPath && _status is JobStatus.Working w &&
                                    w.Label.Id.StartsWith("stage:Translating:", StringComparison.Ordinal))
                                {
                                    Status = new JobStatus.Working(Label(stage.Kind, fraction, JobPurpose.Translation), fraction);
                                }
                            }).ConfigureAwait(false);
                            break;
                        case EngineEvent.Translation translation:
                            for (var i = 0; i < translation.Sentences.Count; i++)
                            {
                                var tr = translation.Sentences[i];
                                var orig = remaining.FirstOrDefault(s => s.Id == tr.Id);
                                if (orig is not null)
                                {
                                    mergedEntries[orig.Uid] = new TranslationEntry(tr.Text, orig.Text, translation.Language, tr.NeedsReview);
                                }
                            }
                            var batchFraction = Math.Clamp((double)mergedEntries.Count / totalCount, 0.0, 0.99);
                            await _ui.InvokeAsync(() =>
                            {
                                if (mergedEntries.Count < sentences.Count)
                                {
                                    UpdatePlaylistItem(
                                        itemId,
                                        PlaylistItemState.Working,
                                        Label(EngineStage.Translating, batchFraction, JobPurpose.Translation),
                                        batchFraction);
                                }
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
                    ? s with { Text = e.Text, Words = [], NeedsReview = e.NeedsReview }
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
