using System.Text.Json;

namespace SubtitleAdder.EngineKit.Tests;

public class SentenceEditingTests
{
    private static List<Sentence> Sample() =>
    [
        new(1, 0.5, 2.0, "Merhaba.", [new Word(0.5, 2.0, "Merhaba.")]),
        new(2, 2.5, 4.0, "Nasılsın?"),
        new(3, 4.0, 6.0, "İyiyim."),
    ];

    [Fact]
    public void ActiveIndexFindsContainingSentence()
    {
        var s = Sample();
        Assert.Null(s.ActiveIndex(0.2));
        Assert.Equal(0, s.ActiveIndex(0.5));
        Assert.Equal(0, s.ActiveIndex(1.99));
        Assert.Null(s.ActiveIndex(2.2));
        Assert.Equal(2, s.ActiveIndex(4.0));
        Assert.Null(s.ActiveIndex(6.0));
    }

    [Fact]
    public void IndexOfUidFindsSentence()
    {
        var s = Sample();
        Assert.Equal(1, s.IndexOfUid(s[1].Uid));
        Assert.Null(s.IndexOfUid(Guid.NewGuid()));
    }

    [Fact]
    public void NudgeMovesByStepWithoutFloatNoise()
    {
        var s = Sample();
        Assert.True(s.NudgeStart(0, 0.1));
        Assert.Equal(0.6, s[0].Start);
        Assert.True(s.NudgeEnd(0, -0.1));
        Assert.Equal(1.9, s[0].End);
    }

    [Fact]
    public void StartClampsToZeroAndPreviousEnd()
    {
        var s = Sample();
        s.SetStart(-3, 0);
        Assert.Equal(0, s[0].Start);
        s.SetStart(0.0, 1);
        Assert.Equal(2.0, s[1].Start);
        Assert.False(s.SetStart(0.0, 1));
    }

    [Fact]
    public void StartKeepsMinimumDuration()
    {
        var s = Sample();
        s.SetStart(10, 0);
        Assert.True(Math.Abs(s[0].Start - (2.0 - SentenceEditing.MinimumDuration)) < 1e-9);
        Assert.True(s[0].Start < s[0].End);
    }

    [Fact]
    public void EndClampsToNextStartAndMinimumDuration()
    {
        var s = Sample();
        s.SetEnd(9, 0);
        Assert.Equal(2.5, s[0].End);
        s.SetEnd(0, 1);
        Assert.True(Math.Abs(s[1].End - (2.5 + SentenceEditing.MinimumDuration)) < 1e-9);
        s.SetEnd(100, 2);
        Assert.Equal(100, s[2].End);
    }

    [Fact]
    public void InvalidIndexIsIgnored()
    {
        var s = Sample();
        Assert.False(s.NudgeStart(5, 0.1));
        Assert.False(s.MergeWithNext(2));
        Assert.False(s.DeleteSentence(-1));
        Assert.Equal(Sample(), s);
    }

    [Fact]
    public void MergeJoinsTextTimesAndWordsThenRenumbers()
    {
        var s = Sample();
        var firstUid = s[0].Uid;
        Assert.True(s.MergeWithNext(0));
        Assert.Equal(2, s.Count);
        Assert.Equal("Merhaba. Nasılsın?", s[0].Text);
        Assert.Equal(0.5, s[0].Start);
        Assert.Equal(4.0, s[0].End);
        Assert.Single(s[0].Words);
        Assert.Equal(firstUid, s[0].Uid);
        Assert.Equal([1, 2], s.Select(x => x.Id));
        Assert.Equal("İyiyim.", s[1].Text);
    }

    [Fact]
    public void DeleteRemovesAndRenumbers()
    {
        var s = Sample();
        Assert.True(s.DeleteSentence(0));
        Assert.Equal(["Nasılsın?", "İyiyim."], s.Select(x => x.Text));
        Assert.Equal([1, 2], s.Select(x => x.Id));
    }

    [Fact]
    public void SetTextReportsChange()
    {
        var s = Sample();
        Assert.False(s.SetText("Merhaba.", 0));
        Assert.True(s.SetText("Selam.", 0));
        Assert.Equal("Selam.", s[0].Text);
    }

    [Fact]
    public void NudgeNeverMovesAgainstRequestedDirection()
    {
        List<Sentence> s = [new(1, 0.0, 5.20, "Önceki"), new(2, 5.05, 6.00, "Sonraki")];
        // The start is below the previous line's end; asking to go back must not push it forward.
        Assert.False(s.NudgeStart(1, -SentenceEditing.NudgeStep));
        Assert.Equal(5.05, s[1].Start);
        // Movement toward validity is accepted because it is in the requested direction.
        Assert.True(s.NudgeStart(1, SentenceEditing.NudgeStep));
        Assert.Equal(5.20, s[1].Start);
    }

    [Fact]
    public void SetStartNeverJumpsAgainstRequestedDirection()
    {
        List<Sentence> s = [new(1, 0.0, 5.20, "Önceki"), new(2, 5.05, 6.00, "Sonraki")];
        Assert.False(s.SetStart(5.0, 1));
        Assert.Equal(5.05, s[1].Start);
        Assert.True(s.SetStart(5.15, 1));
        Assert.Equal(5.20, s[1].Start);
    }

    [Fact]
    public void NudgeEndDoesNotMoveBackwardsWhenSentenceIsTooShort()
    {
        List<Sentence> s = [new(1, 2.0, 2.05, "Kısa")];
        Assert.False(s.NudgeEnd(0, -SentenceEditing.NudgeStep));
        Assert.Equal(2.05, s[0].End);
        Assert.True(s.NudgeEnd(0, SentenceEditing.NudgeStep));
        Assert.Equal(2.15, s[0].End);
    }

    [Fact]
    public void SplitUsesWordBoundaryAtPlayhead()
    {
        List<Sentence> s = [new(5, 0.0, 4.0, "Merhaba dünya", [new Word(0.0, 1.0, "Merhaba"), new Word(2.0, 4.0, "dünya")])];
        var firstUid = s[0].Uid;
        Assert.True(s.SplitSentence(0, 1.5));
        Assert.Equal(2, s.Count);
        Assert.Equal("Merhaba", s[0].Text);
        Assert.Equal("dünya", s[1].Text);
        Assert.Equal(0.0, s[0].Start);
        Assert.Equal(1.5, s[0].End);
        Assert.Equal(1.5, s[1].Start);
        Assert.Equal(4.0, s[1].End);
        Assert.Equal(firstUid, s[0].Uid);
        Assert.NotEqual(firstUid, s[1].Uid);
        Assert.Equal([1, 2], s.Select(x => x.Id));
        Assert.Equal([new Word(0.0, 1.0, "Merhaba")], s[0].Words);
        Assert.Equal([new Word(2.0, 4.0, "dünya")], s[1].Words);
    }

    [Fact]
    public void SplitInsideFirstWordStillUsesWordBoundary()
    {
        List<Sentence> s = [new(1, 0.0, 4.0, "Merhaba dünya", [new Word(0.0, 1.0, "Merhaba"), new Word(2.0, 4.0, "dünya")])];
        Assert.True(s.SplitSentence(0, 0.2));
        Assert.Equal("Merhaba", s[0].Text);
        Assert.Equal(1.5, s[0].End);
        Assert.Equal(1.5, s[1].Start);
    }

    [Fact]
    public void SplitWithoutWordsUsesMiddleWhitespaceAndProportionalTime()
    {
        List<Sentence> s = [new(1, 0.0, 4.0, "Bir iki üç dört")];
        Assert.True(s.SplitSentence(0, 3.5));
        Assert.Equal(2, s.Count);
        Assert.Equal("Bir iki", s[0].Text);
        Assert.Equal("üç dört", s[1].Text);
        Assert.Equal(2.0, s[0].End);
        Assert.Equal(2.0, s[1].Start);
        Assert.Empty(s[0].Words);
        Assert.Empty(s[1].Words);
    }

    [Fact]
    public void SplitRejectsInvalidRequestsWithoutChangingList()
    {
        var s = Sample();
        Assert.False(s.SplitSentence(0, 10.0));
        Assert.False(s.SplitSentence(9, 1.0));

        List<Sentence> singleWord = [new(1, 0.0, 2.0, "Tek")];
        Assert.False(singleWord.SplitSentence(0, 1.0));

        List<Sentence> tooShort = [new(1, 0.0, 0.15, "Bir iki")];
        Assert.False(tooShort.SplitSentence(0, 0.075));
        Assert.Equal(Sample(), s);
        Assert.Single(singleWord);
        Assert.Single(tooShort);
    }

    [Fact]
    public void CanSplitMatchesSplitPrerequisites()
    {
        List<Sentence> splittable = [new(1, 0.0, 4.0, "Bir iki")];
        Assert.True(splittable.CanSplit(0, 2.0));
        Assert.False(splittable.CanSplit(0, 5.0));
        List<Sentence> single = [new(1, 0.0, 4.0, "Tek")];
        Assert.False(single.CanSplit(0, 2.0));
    }

    [Fact]
    public void UidIsStableForSelectionAndStaysOutOfJson()
    {
        var sentence = new Sentence(1, 0.0, 1.0, "Merhaba.", [new Word(0.0, 1.0, "Merhaba.")]);
        var json = JsonSerializer.Serialize(sentence, EngineJson.Options);
        Assert.DoesNotContain("uid", json, StringComparison.OrdinalIgnoreCase);
        var decoded = JsonSerializer.Deserialize<Sentence>(json, EngineJson.Options)!;
        Assert.NotEqual(sentence.Uid, decoded.Uid);
        Assert.Equal(sentence, decoded);
        Assert.Equal("Merhaba.", decoded.Text);
    }

    [Fact]
    public void WordsDefaultToEmptyWhenMissing()
    {
        var decoded = JsonSerializer.Deserialize<Sentence>("""{"id": 1, "start": 0, "end": 1, "text": "A"}""", EngineJson.Options)!;
        Assert.Empty(decoded.Words);
        Assert.Throws<JsonException>(() => JsonSerializer.Deserialize<Sentence>("""{"id": 1, "start": 0, "text": "A"}""", EngineJson.Options));
    }

    [Fact]
    public void RoundingMatchesSwiftHalfAwayFromZero()
    {
        // 0.0005 s rounds up to the next millisecond, as Swift's rounded() does.
        List<Sentence> s = [new(1, 1.0, 3.0, "A")];
        Assert.True(s.SetStart(1.2345, 0));
        Assert.Equal(1.235, s[0].Start);
    }
}
