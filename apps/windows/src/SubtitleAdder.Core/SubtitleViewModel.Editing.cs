using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

public enum TimingEdge
{
    Start,
    End,
}

/// <summary>
/// Line editing operations. All of them work on <see cref="Sentences"/> and can be undone; each public command is
/// one undo group (a draft committed by the command undoes together with it).
/// </summary>
public sealed partial class SubtitleViewModel
{
    public Sentence? SelectedSentence => _selection is { } uid ? _sentences.FirstOrDefault(s => s.Uid == uid) : null;

    public bool HasSelection => _selection is not null;

    public bool CanMergeSelected =>
        _selection is { } uid && _sentences.IndexOfUid(uid) is { } index && index + 1 < _sentences.Count;

    /// <summary>Selected line's start as "mm:ss.ff", or the placeholder.</summary>
    public string SelectedStartText => SelectedSentence is { } s ? SubtitleTime.Short(s.Start) : SubtitleTime.Placeholder;

    /// <summary>Selected line's end as "mm:ss.ff", or the placeholder.</summary>
    public string SelectedEndText => SelectedSentence is { } s ? SubtitleTime.Short(s.End) : SubtitleTime.Placeholder;

    /// <summary>"Konuşmacı N" (1-based) or "Konuşmacı yok".</summary>
    public static string SpeakerTitle(int? speaker) =>
        speaker is { } index ? L("Konuşmacı %lld", (long)index + 1) : L("Konuşmacı yok");

    /// <summary>Label of a timing edge ("Başlangıç" / "Bitiş").</summary>
    public static string EdgeTitle(TimingEdge edge) => edge == TimingEdge.Start ? L("Başlangıç") : L("Bitiş");

    // MARK: - Undo / redo

    public bool CanUndo => UndoStack.CanUndo;
    public bool CanRedo => UndoStack.CanRedo;

    /// <summary>Menu title, e.g. "Satırı Sil İşlemini Geri Al" / "Undo Delete Line"; plain "Geri Al" when there is nothing.</summary>
    public string UndoTitle => UndoStack.UndoActionKey is { } key ? L("%@ İşlemini Geri Al", L(key)) : L("Geri Al");

    /// <summary>Menu title, e.g. "Satırı Sil İşlemini Yinele" / "Redo Delete Line".</summary>
    public string RedoTitle => UndoStack.RedoActionKey is { } key ? L("%@ İşlemini Yinele", L(key)) : L("Yinele");

    /// <summary>
    /// Before undo/redo starts, the focused drafts are written to the lines (the view also releases focus), like the
    /// macOS editor does on <c>NSUndoManagerWillUndoChange</c>. Undo then reverts the latest group, so an uncommitted
    /// edit is undone first.
    /// </summary>
    public void Undo()
    {
        FinishEditing();
        UndoStack.Undo();
    }

    public void Redo()
    {
        FinishEditing();
        UndoStack.Redo();
    }

    /// <summary>Writes the line and translation drafts (when a text field loses focus or before a structural operation).</summary>
    public void FinishEditing()
    {
        CommitDraft();
        CommitTranslationDraft();
    }

    // MARK: - Text draft

    /// <summary>The text to display for a line: for the focused line, the draft that hasn't been committed yet.</summary>
    public string DraftText(Sentence sentence) =>
        _draft is { } draft && draft.Uid == sentence.Uid ? draft.Text : sentence.Text;

    public void BeginEditing(Sentence sentence)
    {
        if (_draft?.Uid == sentence.Uid) return;
        CommitDraft();
        if (_sentences.FirstOrDefault(s => s.Uid == sentence.Uid) is not { } current) return;
        Draft = new TextDraft(current.Uid, current.Text);
    }

    public void UpdateDraft(string text, Guid uid)
    {
        if (_draft is { } other && other.Uid != uid) CommitDraft();
        if (_draft is { } draft && draft.Uid == uid)
        {
            if (draft.Text == text) return;
            Draft = draft with { Text = text };
        }
        else if (_sentences.Any(s => s.Uid == uid))
        {
            Draft = new TextDraft(uid, text);
        }
    }

    /// <summary>
    /// Writes the draft to the line. Called before structural operations and before undo/redo.
    /// While undoing, it writes without creating a record so the stack isn't corrupted.
    /// </summary>
    public void CommitDraft()
    {
        if (_draft is not { } draft) return;
        Draft = null;
        if (_sentences.IndexOfUid(draft.Uid) is not { } index) return;
        var text = draft.Text.Replace("\r\n", " ").Replace('\n', ' ').Replace('\r', ' ');
        if (UndoStack.IsUndoing || UndoStack.IsRedoing)
        {
            var updated = new List<Sentence>(_sentences);
            if (updated.SetText(text, index)) Sentences = updated;
        }
        else
        {
            using var group = UndoStack.BeginGroup();
            Commit(K("Metni Değiştir"), list => list.SetText(text, index));
        }
    }

    // MARK: - Operations

    public void SetText(string text, Guid uid)
    {
        if (_sentences.IndexOfUid(uid) is not { } index) return;
        using var group = UndoStack.BeginGroup();
        Commit(K("Metni Değiştir"), list => list.SetText(text, index));
    }

    public void Nudge(TimingEdge edge, double delta)
    {
        using var group = UndoStack.BeginGroup();
        CommitDraft();
        if (_selection is not { } uid || _sentences.IndexOfUid(uid) is not { } index) return;
        Commit(K("Zamanlamayı Değiştir"), list =>
            edge == TimingEdge.Start ? list.NudgeStart(index, delta) : list.NudgeEnd(index, delta));
    }

    public void SetSelected(TimingEdge edge, double time)
    {
        using var group = UndoStack.BeginGroup();
        CommitDraft();
        if (_selection is not { } uid || _sentences.IndexOfUid(uid) is not { } index) return;
        Commit(K("Zamanlamayı Değiştir"), list =>
            edge == TimingEdge.Start ? list.SetStart(time, index) : list.SetEnd(time, index));
    }

    public void MergeSelectedWithNext()
    {
        using var group = UndoStack.BeginGroup();
        CommitDraft();
        if (_selection is not { } uid || _sentences.IndexOfUid(uid) is not { } index || index + 1 >= _sentences.Count) return;
        var affected = new HashSet<Guid> { _sentences[index].Uid, _sentences[index + 1].Uid };
        Commit(K("Satırları Birleştir"), list => list.MergeWithNext(index), affected);
    }

    /// <summary>Sets a line's speaker (zero-based index); <c>null</c> clears it.</summary>
    public void SetSpeaker(int? speaker, Guid uid)
    {
        using var group = UndoStack.BeginGroup();
        CommitDraft();
        if (_sentences.IndexOfUid(uid) is not { } index) return;
        Commit(K("Konuşmacıyı Değiştir"), list => list.SetSpeaker(speaker, index));
    }

    /// <summary>Assigns an unused speaker index to the line ("Yeni konuşmacı").</summary>
    public void AssignNewSpeaker(Guid uid) => SetSpeaker(_sentences.NextNewSpeaker(), uid);

    public void DeleteSelected()
    {
        using var group = UndoStack.BeginGroup();
        CommitDraft();
        if (_selection is not { } uid || _sentences.IndexOfUid(uid) is not { } index) return;
        if (!Commit(K("Satırı Sil"), list => list.DeleteSentence(index))) return;
        Selection = _sentences.Count == 0 ? null : _sentences[Math.Min(index, _sentences.Count - 1)].Uid;
    }

    public bool CanSplitSelected(double time) =>
        _selection is { } uid && _sentences.IndexOfUid(uid) is { } index && _sentences.CanSplit(index, time);

    public void SplitSelected(double time)
    {
        using var group = UndoStack.BeginGroup();
        CommitDraft();
        if (_selection is not { } uid || _sentences.IndexOfUid(uid) is not { } index) return;
        // The first part keeps its identity but its text changes; the translation of both parts is invalid.
        Commit(K("Satırı Böl"), list => list.SplitSentence(index, time), new HashSet<Guid> { uid });
    }

    private bool Commit(string actionKey, Func<List<Sentence>, bool> change, IReadOnlySet<Guid>? clearingTranslations = null)
    {
        var updated = new List<Sentence>(_sentences);
        if (!change(updated)) return false;
        ReplaceSentences(updated, actionKey, clearedUids: clearingTranslations);
        return true;
    }

    /// <summary>
    /// Changes <see cref="Sentences"/> and records the old value on the undo stack (during undo, this call also creates
    /// the redo record). Translations of deleted/merged lines are cleared; if <paramref name="restoredTranslations"/> is
    /// given (undo/redo), that dictionary is restored as is.
    /// </summary>
    private void ReplaceSentences(
        IReadOnlyList<Sentence> updated,
        string actionKey,
        IReadOnlyDictionary<Guid, TranslationEntry>? restoredTranslations = null,
        IReadOnlySet<Guid>? clearedUids = null)
    {
        var previous = _sentences;
        var previousTranslations = _translations;
        // A change from outside (undo/redo) can invalidate the focused draft.
        if (_draft is { } draft && updated.FirstOrDefault(s => s.Uid == draft.Uid)?.Text != draft.Text) Draft = null;
        var validUids = updated.Select(s => s.Uid).ToHashSet();
        Sentences = updated;
        Translations = restoredTranslations ?? _translations
            .Where(pair => validUids.Contains(pair.Key) && !(clearedUids?.Contains(pair.Key) ?? false))
            .ToDictionary(pair => pair.Key, pair => pair.Value);
        if (_translationDraft is { } translationDraft
            && (!validUids.Contains(translationDraft.Uid) || (clearedUids?.Contains(translationDraft.Uid) ?? false)))
            TranslationDraft = null;
        if (_selection is { } selected && !validUids.Contains(selected)) Selection = null;
        UndoStack.Register(actionKey, () => ReplaceSentences(previous, actionKey, previousTranslations));
    }
}
