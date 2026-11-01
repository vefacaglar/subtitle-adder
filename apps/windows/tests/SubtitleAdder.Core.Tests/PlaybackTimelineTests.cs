using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

public class PlaybackTimelineTests
{
    private static readonly DateTimeOffset Now = DateTimeOffset.UnixEpoch;

    [Fact]
    public void SubtitleSeekUpdatesDisplayImmediatelyAndIgnoresTheOldPausedFrame()
    {
        var timeline = new PlaybackTimeline();
        timeline.Observe(10, 300, Now);
        Assert.Equal(138, timeline.Seek(138, Now));
        timeline.Observe(10, 300, Now.AddMilliseconds(100));
        Assert.Equal(138, timeline.Position);
        timeline.Observe(138, 300, Now.AddMilliseconds(200));
        timeline.Observe(139, 300, Now.AddMilliseconds(300));
        Assert.Equal(139, timeline.Position);
    }

    [Fact]
    public void RapidSecondSeekWinsOverCompletionOfTheFirstSeek()
    {
        var timeline = new PlaybackTimeline();
        timeline.Observe(0, 300, Now);
        timeline.Seek(30, Now);
        timeline.Seek(180, Now.AddMilliseconds(50));
        timeline.Observe(30, 300, Now.AddMilliseconds(100));
        Assert.Equal(180, timeline.Position);
        timeline.Observe(180, 300, Now.AddMilliseconds(200));
        Assert.Equal(180, timeline.Position);
    }

    [Fact]
    public void FailedOrImpreciseSeekEventuallyUsesTheActualPlayerPosition()
    {
        var timeline = new PlaybackTimeline();
        timeline.Observe(0, 300, Now);
        timeline.Seek(138, Now);
        timeline.Observe(136, 300, Now.AddSeconds(3));
        Assert.Equal(136, timeline.Position);
    }

    [Fact]
    public void NewVideoClearsDurationAndAnyPendingSeek()
    {
        var timeline = new PlaybackTimeline();
        timeline.Observe(20, 300, Now);
        timeline.Seek(140, Now);
        timeline.Reset();
        Assert.Equal(0, timeline.Duration);
        timeline.Observe(0, 30, Now.AddMilliseconds(10));
        Assert.Equal(0, timeline.Position);
        Assert.Equal(30, timeline.Seek(100, Now));
        Assert.Equal(0, timeline.Seek(-1, Now));
        Assert.Equal(0, timeline.Seek(double.NaN, Now));
    }

    [Theory]
    [InlineData(138.9, false, "02:18")]
    [InlineData(138, true, "00:02:18")]
    [InlineData(3661, false, "01:01:01")]
    [InlineData(90000, true, "25:00:00")]
    [InlineData(-5, false, "00:00")]
    public void TimeReadoutHandlesHoursWithoutWrapping(double seconds, bool hours, string expected)
        => Assert.Equal(expected, PlaybackTimeline.FormatTime(seconds, hours));
}
