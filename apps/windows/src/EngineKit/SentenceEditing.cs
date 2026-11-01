using System.Globalization;

namespace SubtitleAdder.EngineKit;

/// <summary>
/// Editing operations on a subtitle list (port of SentenceEditing.swift). Mutating operations take a
/// <see cref="List{T}"/> and replace elements (sentences are immutable); queries take any read-only list.
/// </summary>
public static class SentenceEditing
{
    /// <summary>Shortest duration of a line (seconds).</summary>
    public const double MinimumDuration = 0.1;

    /// <summary>Step size of the adjustment buttons (seconds).</summary>
    public const double NudgeStep = 0.1;

    /// <summary>Index of the line that should be shown at <paramref name="time"/>: <c>start &lt;= time &lt; end</c>.</summary>
    public static int? ActiveIndex(this IReadOnlyList<Sentence> sentences, double time)
    {
        for (var i = 0; i < sentences.Count; i++)
            if (sentences[i].Start <= time && time < sentences[i].End) return i;
        return null;
    }

    /// <summary>Index of the line with the given stable identity (<see cref="Sentence.Uid"/>).</summary>
    public static int? IndexOfUid(this IReadOnlyList<Sentence> sentences, Guid uid)
    {
        for (var i = 0; i < sentences.Count; i++)
            if (sentences[i].Uid == uid) return i;
        return null;
    }

    /// <summary>
    /// Sets the start. Cannot go below the previous line's end (or 0), and cannot come closer than
    /// <see cref="MinimumDuration"/> to its own end. Returns <c>true</c> if something changed.
    /// If the value is already outside the valid range, only movement toward the range is allowed;
    /// it is never moved against the requested direction.
    /// </summary>
    public static bool SetStart(this List<Sentence> sentences, double value, int index)
    {
        if (StartBounds(sentences, index) is not { } bounds) return false;
        if (Clamped(value, sentences[index].Start, bounds) is not { } target) return false;
        sentences[index] = sentences[index] with { Start = target };
        return true;
    }

    /// <summary>
    /// Sets the end. Cannot exceed the next line's start, and cannot come closer than <see cref="MinimumDuration"/>
    /// to the start. Returns <c>true</c> if something changed. The direction rule is the same as <see cref="SetStart"/>.
    /// </summary>
    public static bool SetEnd(this List<Sentence> sentences, double value, int index)
    {
        if (EndBounds(sentences, index) is not { } bounds) return false;
        if (Clamped(value, sentences[index].End, bounds) is not { } target) return false;
        sentences[index] = sentences[index] with { End = target };
        return true;
    }

    public static bool NudgeStart(this List<Sentence> sentences, int index, double delta) =>
        InRange(sentences, index) && sentences.SetStart(sentences[index].Start + delta, index);

    public static bool NudgeEnd(this List<Sentence> sentences, int index, double delta) =>
        InRange(sentences, index) && sentences.SetEnd(sentences[index].End + delta, index);

    public static bool SetText(this List<Sentence> sentences, string text, int index)
    {
        if (!InRange(sentences, index) || sentences[index].Text == text) return false;
        sentences[index] = sentences[index] with { Text = text };
        return true;
    }

    /// <summary>
    /// Merges the line with the next: texts are joined with a space, the duration covers both.
    /// The first line's identity is kept.
    /// </summary>
    public static bool MergeWithNext(this List<Sentence> sentences, int index)
    {
        if (!InRange(sentences, index) || !InRange(sentences, index + 1)) return false;
        var first = sentences[index];
        var next = sentences[index + 1];
        var text = string.Join(" ", new[] { first.Text, next.Text }.Select(t => t.Trim()).Where(t => t.Length > 0));
        // Merging keeps the first line's speaker.
        sentences[index] = new Sentence(
            first.Id, first.Start, next.End, text, [.. first.Words, .. next.Words], first.Speaker, first.Uid);
        sentences.RemoveAt(index + 1);
        sentences.Renumber();
        return true;
    }

    /// <summary>Assigns a speaker (zero-based) to the line. Returns <c>true</c> if something changed.</summary>
    public static bool SetSpeaker(this List<Sentence> sentences, int? speaker, int index)
    {
        if (!InRange(sentences, index) || sentences[index].Speaker == speaker) return false;
        sentences[index] = sentences[index] with { Speaker = speaker };
        return true;
    }

    /// <summary>Distinct non-null speakers, sorted.</summary>
    public static IReadOnlyList<int> Speakers(this IReadOnlyList<Sentence> sentences) =>
        sentences.Where(s => s.Speaker is not null).Select(s => s.Speaker!.Value).Distinct().Order().ToList();

    /// <summary>An unused speaker index (one past the highest in use).</summary>
    public static int NextNewSpeaker(this IReadOnlyList<Sentence> sentences)
    {
        var speakers = sentences.Speakers();
        return (speakers.Count > 0 ? speakers[^1] : -1) + 1;
    }

    /// <summary>
    /// Which lines get the "- " dialogue prefix; mirrors the engine's <c>speaker_dashes</c>: needs at least two
    /// distinct non-null speakers, and a line gets one when it has a speaker that differs from the last
    /// known one (lines without a speaker do not reset it).
    /// </summary>
    public static IReadOnlyList<bool> DialogueDashes(this IReadOnlyList<Sentence> sentences)
    {
        if (sentences.Speakers().Count < 2) return sentences.Select(_ => false).ToList();
        int? previous = null;
        var dashes = new List<bool>(sentences.Count);
        foreach (var sentence in sentences)
        {
            dashes.Add(sentence.Speaker is not null && sentence.Speaker != previous);
            if (sentence.Speaker is { } speaker) previous = speaker;
        }
        return dashes;
    }

    /// <summary>Whether the line at <paramref name="index"/> starts a speaker turn (gets the dash).</summary>
    public static bool StartsDialogueTurn(this IReadOnlyList<Sentence> sentences, int index) =>
        InRange(sentences, index) && sentences.DialogueDashes()[index];

    /// <summary>Text as displayed: with the dialogue dash when the rule applies and the text has none yet.</summary>
    public static string DisplayText(this IReadOnlyList<Sentence> sentences, int index) =>
        DialogueDash.Apply(sentences.StartsDialogueTurn(index), sentences[index].Text);

    public static bool DeleteSentence(this List<Sentence> sentences, int index)
    {
        if (!InRange(sentences, index)) return false;
        sentences.RemoveAt(index);
        sentences.Renumber();
        return true;
    }

    /// <summary>
    /// Splits the line in two at <paramref name="time"/>. If word timestamps exist, the split point falls on the word
    /// boundary nearest the playback position; if there are no words, the text is split at the middle
    /// space and the duration is shared in proportion to the text length. The first
    /// part keeps the identity, the second gets a new one. If <paramref name="time"/> is not inside the line,
    /// or the parts would be shorter than the minimum duration, nothing changes.
    /// </summary>
    public static bool SplitSentence(this List<Sentence> sentences, int index, double time)
    {
        if (!InRange(sentences, index)) return false;
        var sentence = sentences[index];
        var playhead = Rounded(time);
        if (!(playhead > sentence.Start && playhead < sentence.End
              && sentence.End - sentence.Start >= 2 * MinimumDuration)) return false;

        var lower = sentence.Start + MinimumDuration;
        var upper = sentence.End - MinimumDuration;

        IReadOnlyList<Word> firstWords, secondWords;
        string firstText, secondText;
        double rawBoundary;

        if (sentence.Words.Count >= 2)
        {
            var leftCount = sentence.Words.TakeWhile(w => w.Midpoint < playhead).Count();
            var splitIndex = Math.Min(Math.Max(leftCount, 1), sentence.Words.Count - 1);
            var left = sentence.Words.Take(splitIndex).ToList();
            var right = sentence.Words.Skip(splitIndex).ToList();
            firstWords = left;
            secondWords = right;
            firstText = Joined(left);
            secondText = Joined(right);
            rawBoundary = (left[^1].End + right[0].Start) / 2;
        }
        else
        {
            var tokens = Tokens(sentence.Text);
            if (tokens.Length < 2) return false;
            var middle = tokens.Length / 2;
            firstText = string.Join(" ", tokens[..middle]);
            secondText = string.Join(" ", tokens[middle..]);
            // Character counts as Swift counts them (grapheme clusters).
            var leftLength = new StringInfo(firstText).LengthInTextElements;
            var total = (double)(leftLength + new StringInfo(secondText).LengthInTextElements);
            rawBoundary = sentence.Start + (sentence.End - sentence.Start) * leftLength / Math.Max(total, 1);
            var cut = Rounded(rawBoundary);
            firstWords = sentence.Words.Where(w => w.Midpoint < cut).ToList();
            secondWords = sentence.Words.Where(w => w.Midpoint >= cut).ToList();
        }

        if (firstText.Length == 0 || secondText.Length == 0) return false;
        var boundary = Rounded(Math.Min(Math.Max(rawBoundary, lower), upper));

        var first = sentence with { End = boundary, Text = firstText, Words = firstWords };
        var second = new Sentence(sentence.Id + 1, boundary, sentence.End, secondText, secondWords, sentence.Speaker);
        sentences[index] = first;
        sentences.Insert(index + 1, second);
        sentences.Renumber();
        return true;
    }

    /// <summary>
    /// Can the line be split at <paramref name="time"/>? Uses the same conditions as <see cref="SplitSentence"/> to decide
    /// whether the buttons are enabled.
    /// </summary>
    public static bool CanSplit(this IReadOnlyList<Sentence> sentences, int index, double time)
    {
        if (!InRange(sentences, index)) return false;
        var sentence = sentences[index];
        var playhead = Rounded(time);
        if (!(playhead > sentence.Start && playhead < sentence.End
              && sentence.End - sentence.Start >= 2 * MinimumDuration)) return false;
        if (sentence.Words.Count >= 2) return true;
        return Tokens(sentence.Text).Length >= 2;
    }

    /// <summary>Renumbers ids sequentially from 1; does not touch <see cref="Sentence.Uid"/> values.</summary>
    public static void Renumber(this List<Sentence> sentences)
    {
        for (var i = 0; i < sentences.Count; i++)
            if (sentences[i].Id != i + 1) sentences[i] = sentences[i] with { Id = i + 1 };
    }

    private static bool InRange(IReadOnlyList<Sentence> sentences, int index) => index >= 0 && index < sentences.Count;

    private static (double Lower, double Upper)? StartBounds(List<Sentence> sentences, int index)
    {
        if (!InRange(sentences, index)) return null;
        var lower = index > 0 ? sentences[index - 1].End : 0;
        var upper = sentences[index].End - MinimumDuration;
        return upper >= lower ? (lower, upper) : null;
    }

    private static (double Lower, double Upper)? EndBounds(List<Sentence> sentences, int index)
    {
        if (!InRange(sentences, index)) return null;
        var lower = sentences[index].Start + MinimumDuration;
        var upper = index + 1 < sentences.Count ? sentences[index + 1].Start : double.PositiveInfinity;
        return upper >= lower ? (lower, upper) : null;
    }

    /// <summary>
    /// Clamps the value to the valid range, but makes no change if the result would go against
    /// the requested direction. Values outside the range are moved only if the requested direction
    /// is toward validity.
    /// </summary>
    private static double? Clamped(double value, double current, (double Lower, double Upper) bounds)
    {
        var target = Math.Min(Math.Max(Rounded(value), bounds.Lower), bounds.Upper);
        if (target == current) return null;
        if (!((value - current) * (target - current) > 0)) return null;
        return target;
    }

    private static string[] Tokens(string text) =>
        text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);

    private static string Joined(IEnumerable<Word> words) =>
        string.Join(" ", words.Select(w => w.Text.Trim()).Where(t => t.Length > 0));

    /// <summary>Millisecond rounding, half away from zero like Swift's <c>rounded()</c>.</summary>
    private static double Rounded(double value) => Math.Round(value * 1000, MidpointRounding.AwayFromZero) / 1000;
}

/// <summary>The dialogue dash applied to displayed text; same check as the engine's <c>_has_dash</c>.</summary>
public static class DialogueDash
{
    /// <summary>Does the text already start with a dialogue dash ("- " or "– ")?</summary>
    public static bool HasDash(string text) =>
        text.StartsWith("- ", StringComparison.Ordinal) || text.StartsWith("– ", StringComparison.Ordinal);

    public static string Apply(bool dash, string text) => dash && !HasDash(text) ? "- " + text : text;
}
