using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>Translation flow: sends the lines to the engine and maps editable translation texts by <c>Uid</c>.</summary>
public sealed partial class SubtitleViewModel
{
    /// <summary>Returns the line's translation if there is one.</summary>
    public TranslationEntry? TranslationFor(Sentence sentence) => _translations.GetValueOrDefault(sentence.Uid);

    public bool IsTranslationStale(Sentence sentence) => TranslationFor(sentence)?.IsStale(sentence) ?? false;

    /// <summary>If the translation is chosen on export, the times are kept and the text is replaced by the translation.</summary>
    public IReadOnlyList<Sentence> ExportSentences(bool translated)
    {
        if (!translated) return _sentences;
        return [.. _sentences.Select(sentence =>
            _translations.TryGetValue(sentence.Uid, out var entry) && entry.Text.Length > 0
                ? sentence with { Text = entry.Text, Words = [], NeedsReview = entry.NeedsReview }
                : sentence with { Words = [] })];
    }

    /// <summary>Translates all lines to the selected target language. While running, Stop cancels the same job.</summary>
    public void Translate() => TranslateCore(reuseExistingTranslations: false);

    public void TranslateSelected()
    {
        if (!CanTranslateSelected) return;
        TranslateCore(reuseExistingTranslations: false, selectedUids: SelectedUids.ToHashSet());
    }

    private void TranslateCore(bool reuseExistingTranslations, IReadOnlySet<Guid>? selectedUids = null) =>
        TranslateCoreForVideo(_video, reuseExistingTranslations, selectedUids);

    private void TranslateCoreForVideo(
        string? jobVideo,
        bool reuseExistingTranslations,
        IReadOnlySet<Guid>? selectedUids = null,
        string? sourceLanguageOverride = null,
        string? targetLanguageOverride = null,
        string? translationModelOverride = null,
        bool? autoSaveOverride = null,
        SubtitleFormat? exportFormatOverride = null,
        TaskCompletionSource<bool>? externalCompletion = null,
        bool bypassQueueCheck = false)
    {
        var session = GetOrCreateSession(jobVideo);
        if (session.Sentences.Count == 0)
        {
            if (externalCompletion is not null)
            {
                externalCompletion.TrySetResult(false);
                AdvanceJobQueueOrFinish();
            }
            return;
        }
        if (Catalog.Engine is not { } engine)
        {
            SetSessionStatus(jobVideo, new JobStatus.Failed(LocalizedMessage.Error(EngineException.NotFound())));
            if (externalCompletion is not null)
            {
                externalCompletion.TrySetResult(false);
                AdvanceJobQueueOrFinish();
            }
            return;
        }
        if (IsForegroundVideo(jobVideo))
        {
            FinishEditing();
        }

        var rawLanguage = sourceLanguageOverride ?? _language;
        var source = rawLanguage.Length > 0 ? rawLanguage : session.DetectedLanguage;
        var target = targetLanguageOverride ?? _translationTarget;
        var model = translationModelOverride ?? _translationModel;
        var autoSave = autoSaveOverride ?? _autoSaveSubtitle;
        var exportFormat = exportFormatOverride ?? _exportFormat;

        if (source == target)
        {
            SetSessionStatus(jobVideo, new JobStatus.Failed(LocalizedMessage.Key("Kaynak ve hedef dil aynı olamaz.")));
            if (externalCompletion is not null)
            {
                externalCompletion.TrySetResult(false);
                AdvanceJobQueueOrFinish();
            }
            return;
        }
        if (model == "nllb-1.3b" && source is null && !IsExternalLlmActive)
        {
            SetSessionStatus(jobVideo, new JobStatus.Failed(LocalizedMessage.Key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç.")));
            if (externalCompletion is not null)
            {
                externalCompletion.TrySetResult(false);
                AdvanceJobQueueOrFinish();
            }
            return;
        }
        if (BlockedOffline([model], selectedUids is null ? Translate : () =>
            TranslateCoreForVideo(jobVideo, reuseExistingTranslations: false, selectedUids: selectedUids)))
        {
            if (externalCompletion is not null)
            {
                externalCompletion.TrySetResult(false);
                AdvanceJobQueueOrFinish();
            }
            return;
        }

        session.SavedSubtitleName = null;
        if (IsForegroundVideo(jobVideo))
        {
            _savedSubtitleName = null;
        }

        if (!bypassQueueCheck && !_isPlaylistRunning && _isJobRunning && !string.Equals(_activeJobVideo, jobVideo, StringComparison.OrdinalIgnoreCase))
        {
            EnqueueVideoJob(
                jobVideo,
                QueuedJobKind.Translate,
                rawLanguage,
                target,
                _model,
                model,
                _diarize,
                _speakerCount,
                autoSave,
                exportFormat,
                selectedUids);
            return;
        }

        var request = selectedUids is not null
            ? session.Sentences.Where(s => selectedUids.Contains(s.Uid)).ToList()
            : reuseExistingTranslations
            ? session.Sentences
                .Where(s => !session.Translations.TryGetValue(s.Uid, out var existing) || !IsReusableTranslation(s, existing, target))
                .ToList()
            : session.Sentences;
        if (request.Count == 0)
        {
            SetSessionStatus(jobVideo, JobStatus.FinishedStatus);
            if (autoSave && session.Translations.Count > 0 &&
                (jobVideo ?? session.LoadedSubtitlePath) is { } basePath &&
                Path.GetDirectoryName(basePath) is { Length: > 0 } autoDir &&
                Directory.Exists(autoDir))
            {
                CurrentJob = SaveSessionSubtitleToVideoFolderAsync(jobVideo, exportFormat, externalCompletion);
                return;
            }
            if (externalCompletion is not null)
            {
                externalCompletion.TrySetResult(true);
                AdvanceJobQueueOrFinish();
            }
            return;
        }
        var totalCount = Math.Max(1, session.Sentences.Count);
        var alreadyDoneCount = reuseExistingTranslations ? Math.Max(0, session.Sentences.Count - request.Count) : 0;
        var initialFraction = alreadyDoneCount > 0 ? Math.Clamp((double)alreadyDoneCount / totalCount, 0.0, 0.99) : (double?)null;
        var contextualRequest = WithTranslationContext(session.Sentences, request);
        var initStatus = initialFraction is { } initF
            ? new JobStatus.Working(Label(EngineStage.Translating, initF, JobPurpose.Translation), initF)
            : new JobStatus.Working(LocalizedMessage.Key("Çeviri hazırlanıyor…"), null);
        SetSessionStatus(jobVideo, initStatus, PlaylistItemState.Working);
        StartJob(
            jobVideo,
            new HashSet<string> { model },
            token => SerializeTranslationStream(engine.Translate(contextualRequest, target, source, model, token), token),
            engineEvent =>
        {
            switch (engineEvent)
            {
                case EngineEvent.Stage stage:
                    var fraction = stage.Fraction;
                    if (alreadyDoneCount > 0 && stage.Kind == EngineStage.Translating && fraction is { } f)
                    {
                        fraction = Math.Clamp((alreadyDoneCount + request.Count * Math.Clamp(f, 0.0, 1.0)) / totalCount, 0.0, 1.0);
                    }
                    SetSessionStatus(jobVideo, new JobStatus.Working(Label(stage.Kind, fraction, JobPurpose.Translation), fraction), PlaylistItemState.Working);
                    break;
                case EngineEvent.Translation translation:
                    ApplySessionTranslation(
                        jobVideo,
                        translation.Sentences,
                        translation.Language,
                        request,
                        mergeIntoExisting: reuseExistingTranslations || selectedUids is not null,
                        preserveOtherLanguages: selectedUids is not null);
                    if (!_isPlaylistRunning && !string.IsNullOrWhiteSpace(jobVideo))
                    {
                        var batchFrac = Math.Clamp((double)session.Translations.Count / totalCount, 0.0, 0.99);
                        EnsureOrUpdateVideoPlaylistItem(
                            jobVideo,
                            session.DetectedLanguage ?? rawLanguage,
                            PlaylistItemState.Working,
                            Label(EngineStage.Translating, batchFrac, JobPurpose.Translation),
                            batchFrac);
                    }
                    break;
                case EngineEvent.Error error:
                    return new JobStatus.Failed(LocalizedMessage.Verbatim(error.Message));
            }
            return null;
        }, async () =>
        {
            Task<string?>? saveTask = null;
            await _ui.InvokeAsync(() =>
            {
                if (autoSave && session.Translations.Count > 0 &&
                    (jobVideo ?? session.LoadedSubtitlePath) is { } basePath &&
                    Path.GetDirectoryName(basePath) is { Length: > 0 } dir &&
                    Directory.Exists(dir))
                {
                    saveTask = SaveSessionSubtitleToVideoFolderAsync(jobVideo, exportFormat);
                }
            }).ConfigureAwait(false);
            if (saveTask is not null) await saveTask.ConfigureAwait(false);
        }, externalCompletion: externalCompletion);
    }

    private async Task<string?> SaveSessionSubtitleToVideoFolderAsync(
        string? jobVideo,
        SubtitleFormat format,
        TaskCompletionSource<bool>? externalCompletion = null)
    {
        try
        {
            if (IsForegroundVideo(jobVideo))
            {
                return await SaveSubtitleToVideoFolderAsync().ConfigureAwait(false);
            }

            var session = GetOrCreateSession(jobVideo);
            var basePath = jobVideo ?? session.LoadedSubtitlePath;
            if (basePath is null || Catalog.Engine is not { } engine || session.Sentences.Count == 0)
            {
                return null;
            }

            var outPath = Path.ChangeExtension(basePath, format.RawValue());
            var useTranslated = session.ExportLanguage == ExportLanguage.Translated && session.Translations.Count > 0;
            var list = useTranslated
                ? session.Sentences
                    .Select(s => session.Translations.TryGetValue(s.Uid, out var e) && e.Text.Length > 0
                        ? s with { Text = e.Text, Words = [], NeedsReview = e.NeedsReview }
                        : s with { Words = [] })
                    .ToList()
                : session.Sentences;

            var rendered = await engine.RenderAsync(list, format).ConfigureAwait(false);
            if (rendered is null) return null;
            await File.WriteAllTextAsync(outPath, rendered, new System.Text.UTF8Encoding(false)).ConfigureAwait(false);
            var savedName = Path.GetFileName(outPath);
            VideoCardViewModel.InvalidateSidecarCache(jobVideo);

            await _ui.InvokeAsync(() =>
            {
                session.SavedSubtitleName = savedName;
                if (IsForegroundVideo(jobVideo))
                {
                    _savedSubtitleName = savedName;
                    OnPropertyChanged(nameof(StatusText));
                }
                SetSessionStatus(jobVideo, JobStatus.FinishedStatus);
            }).ConfigureAwait(false);

            return outPath;
        }
        finally
        {
            if (externalCompletion is not null)
            {
                await _ui.InvokeAsync(() =>
                {
                    externalCompletion.TrySetResult(true);
                    AdvanceJobQueueOrFinish();
                }).ConfigureAwait(false);
            }
        }
    }

    private const int MinAsyncBatchSentences = 10;

    /// <summary>Keep neighboring dialogue in resumed/async requests; only requested IDs are applied to the UI.</summary>
    internal static IReadOnlyList<Sentence> WithTranslationContext(
        IReadOnlyList<Sentence> all, IReadOnlyList<Sentence> requested)
    {
        var requestedUids = requested.Select(s => s.Uid).ToHashSet();
        var indices = new SortedSet<int>();
        for (var i = 0; i < all.Count; i++)
        {
            if (!requestedUids.Contains(all[i].Uid)) continue;
            for (var j = Math.Max(0, i - 2); j <= Math.Min(all.Count - 1, i + 2); j++) indices.Add(j);
        }
        return indices.Select(i => all[i]).ToList();
    }

    internal static bool IsReusableTranslation(Sentence sentence, TranslationEntry existing, string target)
    {
        if (existing.NeedsReview || existing.IsStale(sentence) || existing.Language != target) return false;
        var translated = existing.Text.Trim();
        if (translated.Length == 0) return false;
        var source = sentence.Text.Trim();
        if (source.Length >= 4 && translated.Equals(source, StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }
        return true;
    }

    private async IAsyncEnumerable<EngineEvent> SerializeTranslationStream(
        IAsyncEnumerable<EngineEvent> innerStream,
        [System.Runtime.CompilerServices.EnumeratorCancellation] CancellationToken token)
    {
        await _translationExecutionGate.WaitAsync(token).ConfigureAwait(false);
        try
        {
            await foreach (var ev in innerStream.WithCancellation(token).ConfigureAwait(false))
            {
                yield return ev;
            }
        }
        finally
        {
            _translationExecutionGate.Release();
        }
    }

    private async Task PumpPartialTranslationsExternallyAsync(
        IEngineClient engine,
        string? jobVideo,
        HashSet<Guid> queuedAsyncUids,
        CancellationToken token)
    {
        var session = GetOrCreateSession(jobVideo);
        while (!token.IsCancellationRequested)
        {
            if (_translationExecutionGate.CurrentCount == 0) break;

            List<Sentence>? batch = null;
            var target = "";
            string? source = null;
            var model = "";
            await _ui.InvokeAsync(() =>
            {
                if (token.IsCancellationRequested || session.Sentences.Count <= 2 || _translationExecutionGate.CurrentCount == 0) return;
                target = _translationTarget;
                source = _language.Length > 0 ? _language : session.DetectedLanguage;
                model = _translationModel;
                if (source == target) return;
                var candidates = session.Sentences
                    .Take(session.Sentences.Count - 2)
                    .Where(s => !queuedAsyncUids.Contains(s.Uid) && !string.IsNullOrWhiteSpace(s.Text))
                    .ToList();
                if (candidates.Count >= MinAsyncBatchSentences)
                {
                    batch = candidates;
                    foreach (var s in batch) queuedAsyncUids.Add(s.Uid);
                }
            }).ConfigureAwait(false);

            if (batch is null || batch.Count == 0) break;
            await TranslatePartialBatchExternallyAsync(engine, jobVideo, batch, target, source, model, token).ConfigureAwait(false);
        }
    }

    private async Task TranslatePartialBatchExternallyAsync(
        IEngineClient engine,
        string? jobVideo,
        IReadOnlyList<Sentence> batch,
        string target,
        string? source,
        string model,
        CancellationToken token)
    {
        if (batch.Count == 0 || source == target) return;
        var session = GetOrCreateSession(jobVideo);

        try
        {
            await _translationExecutionGate.WaitAsync(token).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            return;
        }

        try
        {
            var alreadyTranslatedCount = 0;
            IReadOnlyList<Sentence> contextualRequest = batch;
            await _ui.InvokeAsync(() =>
            {
                if (token.IsCancellationRequested) return;
                contextualRequest = WithTranslationContext(session.Sentences, batch);
                alreadyTranslatedCount = session.Translations.Count;
                var initFrac = alreadyTranslatedCount > 0 && session.Sentences.Count > 0
                    ? Math.Clamp((double)alreadyTranslatedCount / session.Sentences.Count, 0.0, 0.99)
                    : session.AsyncTranslationFraction;
                SetSessionAsyncTranslating(jobVideo, true, initFrac);
            }).ConfigureAwait(false);

            await foreach (var engineEvent in engine.Translate(contextualRequest, target, source, model, token).ConfigureAwait(false))
            {
                if (token.IsCancellationRequested) return;
                if (engineEvent is EngineEvent.Stage stage && stage.Kind == EngineStage.Translating && stage.Fraction is { } f)
                {
                    await _ui.InvokeAsync(() =>
                    {
                        if (token.IsCancellationRequested) return;
                        var total = Math.Max(session.Sentences.Count, alreadyTranslatedCount + batch.Count);
                        var overall = total > 0
                            ? Math.Clamp((alreadyTranslatedCount + batch.Count * Math.Clamp(f, 0.0, 1.0)) / total, 0.0, 0.99)
                            : f;
                        SetSessionAsyncTranslating(jobVideo, true, overall);
                        if (session.Status is JobStatus.Working w &&
                            w.Label.Id.StartsWith("stage:Translating:", StringComparison.Ordinal))
                        {
                            SetSessionStatus(
                                jobVideo,
                                new JobStatus.Working(
                                    Label(EngineStage.Translating, overall, JobPurpose.Translation),
                                    overall),
                                PlaylistItemState.Working);
                        }
                    }).ConfigureAwait(false);
                }
                else if (engineEvent is EngineEvent.Translation translation)
                {
                    await _ui.InvokeAsync(() =>
                    {
                        if (!token.IsCancellationRequested)
                        {
                            ApplySessionTranslation(jobVideo, translation.Sentences, translation.Language, batch, mergeIntoExisting: true);
                            var total = Math.Max(1, session.Sentences.Count);
                            var overall = Math.Clamp((double)session.Translations.Count / total, 0.0, 0.99);
                            SetSessionAsyncTranslating(jobVideo, true, overall);
                            if (session.Status is JobStatus.Working w &&
                                w.Label.Id.StartsWith("stage:Translating:", StringComparison.Ordinal))
                            {
                                SetSessionStatus(
                                    jobVideo,
                                    new JobStatus.Working(
                                        Label(EngineStage.Translating, overall, JobPurpose.Translation),
                                        overall),
                                    PlaylistItemState.Working);
                            }
                        }
                    }).ConfigureAwait(false);
                }
            }
        }
        catch (Exception)
        {
            // Any lines not translated in the partial background pass are picked up by the final TranslateCore call.
        }
        finally
        {
            _translationExecutionGate.Release();
        }
    }

    private void ApplySessionTranslation(
        string? jobVideo,
        IReadOnlyList<Sentence> translated,
        string language,
        IReadOnlyList<Sentence> source,
        bool mergeIntoExisting = false,
        bool preserveOtherLanguages = false)
    {
        var session = GetOrCreateSession(jobVideo);
        var hadTranslations = session.Translations.Count > 0;
        var entries = mergeIntoExisting && (preserveOtherLanguages || session.TranslationLanguage == language)
            ? new Dictionary<Guid, TranslationEntry>(session.Translations)
            : new Dictionary<Guid, TranslationEntry>();
        for (var index = 0; index < translated.Count; index++)
        {
            var result = translated[index];
            var original = source.FirstOrDefault(s => s.Id == result.Id);
            if (original is null) continue;
            if (result.NeedsReview && session.Translations.TryGetValue(original.Uid, out var existing))
                entries[original.Uid] = existing with { NeedsReview = true };
            else
                entries[original.Uid] = new TranslationEntry(result.Text, original.Text, language, result.NeedsReview);
        }
        session.Translations = entries;
        session.TranslationLanguage = language;
        if (!hadTranslations && entries.Count > 0)
        {
            if (session.DisplayMode == SubtitleDisplay.Original) session.DisplayMode = SubtitleDisplay.Translation;
            session.ExportLanguage = ExportLanguage.Translated;
        }

        if (IsForegroundVideo(jobVideo))
        {
            Translations = new Dictionary<Guid, TranslationEntry>(entries);
            TranslationLanguage = language;
            TranslationTarget = language;
            if (!hadTranslations && entries.Count > 0)
            {
                if (DisplayMode == SubtitleDisplay.Original) DisplayMode = SubtitleDisplay.Translation;
                ExportLanguage = ExportLanguage.Translated;
            }
        }
    }

    /// <summary>Maps translated lines by their stable <c>Uid</c> values; supports both incremental batches and final results.</summary>
    private void ApplyTranslation(
        IReadOnlyList<Sentence> translated, string language, IReadOnlyList<Sentence> source,
        bool mergeIntoExisting = false, bool preserveOtherLanguages = false) =>
        ApplySessionTranslation(_video, translated, language, source, mergeIntoExisting, preserveOtherLanguages);

    // MARK: - Editing the translation text

    public string TranslationDraftText(Sentence sentence) =>
        _translationDraft is { } draft && draft.Uid == sentence.Uid ? draft.Text : TranslationFor(sentence)?.Text ?? "";

    public void BeginTranslationEditing(Sentence sentence)
    {
        if (_translationDraft?.Uid == sentence.Uid) return;
        CommitTranslationDraft();
        if (!_translations.TryGetValue(sentence.Uid, out var entry)) return;
        TranslationDraft = new TextDraft(sentence.Uid, entry.Text);
    }

    public void UpdateTranslationDraft(string text, Guid uid)
    {
        if (_translationDraft is { } other && other.Uid != uid) CommitTranslationDraft();
        if (_translationDraft is { } draft && draft.Uid == uid)
        {
            if (draft.Text == text) return;
            TranslationDraft = draft with { Text = text };
        }
        else if (_translations.ContainsKey(uid))
        {
            TranslationDraft = new TextDraft(uid, text);
        }
    }

    /// <summary>Writes the translation draft to the line and creates an undo record.</summary>
    public void CommitTranslationDraft()
    {
        if (_translationDraft is not { } draft) return;
        TranslationDraft = null;
        if (!_translations.TryGetValue(draft.Uid, out var entry)) return;
        var text = draft.Text.Replace("\r\n", " ").Replace('\n', ' ').Replace('\r', ' ');
        if (entry.Text == text) return;
        SetTranslationText(text, draft.Uid);
    }

    public void SetTranslationText(string text, Guid uid)
    {
        if (!_translations.TryGetValue(uid, out var entry) || entry.Text == text) return;
        var updated = new Dictionary<Guid, TranslationEntry>(_translations) { [uid] = entry with { Text = text, NeedsReview = false } };
        using var group = UndoStack.BeginGroup();
        ReplaceTranslations(updated, K("Çeviriyi Değiştir"));
    }

    private void ReplaceTranslations(IReadOnlyDictionary<Guid, TranslationEntry> updated, string actionKey)
    {
        var previous = _translations;
        Translations = updated;
        UndoStack.Register(actionKey, () => RevertTranslations(updated, previous, actionKey));
    }

    private void RevertTranslations(
        IReadOnlyDictionary<Guid, TranslationEntry> from, IReadOnlyDictionary<Guid, TranslationEntry> to, string actionKey)
    {
        var current = _translations;
        Translations = RevertedTranslations(from, to);
        var applied = _translations;
        UndoStack.Register(actionKey, () => RevertTranslations(applied, current, actionKey));
    }

    /// <summary>
    /// Rolls an undo step's translation change (<paramref name="from"/> -> <paramref name="to"/>) back into the current
    /// translations, line by line and only where the line still holds the step's value, so undoing an edit made before
    /// a (re)translation never throws that translation away. Entries in another language than the current translation
    /// are not brought back. <paramref name="validUids"/> drops entries of lines that no longer exist.
    /// </summary>
    private Dictionary<Guid, TranslationEntry> RevertedTranslations(
        IReadOnlyDictionary<Guid, TranslationEntry> from, IReadOnlyDictionary<Guid, TranslationEntry> to,
        IReadOnlySet<Guid>? validUids = null)
    {
        var current = _translations;
        var result = new Dictionary<Guid, TranslationEntry>(current);
        var language = current.Values.FirstOrDefault()?.Language;
        foreach (var uid in from.Keys.Union(to.Keys))
        {
            var before = from.GetValueOrDefault(uid);
            var after = to.GetValueOrDefault(uid);
            if (Equals(before, after) || !Equals(current.GetValueOrDefault(uid), before)) continue;
            if (after is null) result.Remove(uid);
            else if (language is null || after.Language == language) result[uid] = after;
        }
        if (validUids is not null)
        {
            foreach (var uid in result.Keys.Where(uid => !validUids.Contains(uid)).ToList()) result.Remove(uid);
        }
        return result;
    }
}
