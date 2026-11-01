using System.Text.Json;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder.EngineKit.Tests;

public class TranslationReviewTests
{
    [Fact]
    public void ReviewFlagSurvivesEventsAndJsonWhileOldSentencesDefaultToFalse()
    {
        var translation = Assert.IsType<EngineEvent.Translation>(EngineEvent.Decode("""{"event":"translation","language":"tr","sentences":[{"id":1,"start":0,"end":1,"text":"Source","needs_review":true}]}"""));
        var sentence = Assert.Single(translation.Sentences);
        Assert.True(sentence.NeedsReview);
        var json = JsonSerializer.Serialize(sentence, EngineJson.Options);
        Assert.True(JsonSerializer.Deserialize<Sentence>(json, EngineJson.Options)!.NeedsReview);
        var old = JsonSerializer.Deserialize<Sentence>("""{"id":1,"start":0,"end":1,"text":"Old"}""", EngineJson.Options)!;
        Assert.False(old.NeedsReview);
        Assert.DoesNotContain("needs_review", JsonSerializer.Serialize(old, EngineJson.Options));
    }
}
