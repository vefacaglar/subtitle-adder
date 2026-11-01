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
                ? sentence with { Text = entry.Text, Words = [] }
                : sentence with { Words = [] })];
    }

    /// <summary>Translates all lines to the selected target language. While running, Stop cancels the same job.</summary>
    public void Translate()
    {
        if (_sentences.Count == 0) return;
        if (Catalog.Engine is not { } engine)
        {
            Status = new JobStatus.Failed(LocalizedMessage.Error(EngineException.NotFound()));
            return;
        }
        FinishEditing();
        var source = TranslationSource;
        var target = _translationTarget;
        if (source == target)
        {
            Status = new JobStatus.Failed(LocalizedMessage.Key("Kaynak ve hedef dil aynı olamaz."));
            return;
        }
        if (_translationModel == "nllb-1.3b" && source is null)
        {
            Status = new JobStatus.Failed(LocalizedMessage.Key("NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç."));
            return;
        }
        var request = _sentences;
        var model = _translationModel;
        Status = new JobStatus.Working(LocalizedMessage.Key("Çeviri hazırlanıyor…"), null);
        StartJob(new HashSet<string> { model }, token => engine.Translate(request, target, source, model, token), engineEvent =>
        {
            switch (engineEvent)
            {
                case EngineEvent.Stage stage:
                    Status = new JobStatus.Working(Label(stage.Kind, stage.Fraction, JobPurpose.Translation), stage.Fraction);
                    break;
                case EngineEvent.Translation translation:
                    ApplyTranslation(translation.Sentences, translation.Language, request);
                    break;
                case EngineEvent.Error error:
                    return new JobStatus.Failed(LocalizedMessage.Verbatim(error.Message));
            }
            return null;
        });
    }

    /// <summary>Maps translated lines by their stable <c>Uid</c> values; the engine preserves order and ids.</summary>
    private void ApplyTranslation(IReadOnlyList<Sentence> translated, string language, IReadOnlyList<Sentence> source)
    {
        var entries = new Dictionary<Guid, TranslationEntry>();
        for (var index = 0; index < translated.Count; index++)
        {
            var result = translated[index];
            var original = source.FirstOrDefault(s => s.Id == result.Id) ?? (index < source.Count ? source[index] : null);
            if (original is null) continue;
            entries[original.Uid] = new TranslationEntry(result.Text, original.Text, language);
        }
        Translations = entries;
        TranslationLanguage = language;
        TranslationTarget = language;
    }

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
        var updated = new Dictionary<Guid, TranslationEntry>(_translations) { [uid] = entry with { Text = text } };
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
