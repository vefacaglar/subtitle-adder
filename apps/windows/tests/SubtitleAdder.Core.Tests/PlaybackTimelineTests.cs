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

    [Fact]
    public void BufferedVideoStreamCoalescesSmallReadsIntoLargeSequentialChunksAndCachesSeeks()
    {
        var totalBytes = 1024 * 1024; // 1 MB
        var raw = new byte[totalBytes];
        for (var i = 0; i < raw.Length; i++)
        {
            raw[i] = (byte)((i * 31 + 17) & 0xFF);
        }

        using var memory = new MemoryStream(raw, writable: false);
        using var buffered = new BufferedVideoStream(
            memory,
            ownsStream: false,
            chunkSizeBytes: 256 * 1024, // 256 KB per chunk (4 chunks total)
            maxCachedChunks: 4,
            readAheadChunks: 0);

        // Perform 256 small 4 KB reads across the entire 1 MB stream
        var scratch = new byte[4096];
        var offset = 0;
        while (offset < totalBytes)
        {
            var read = buffered.Read(scratch, 0, scratch.Length);
            Assert.Equal(scratch.Length, read);
            Assert.Equal(raw.AsSpan(offset, read).ToArray(), scratch);
            offset += read;
        }

        // 256 small reads coalesced into exactly 4 chunk reads from underlying stream
        Assert.Equal(4, buffered.UnderlyingReadCount);
        Assert.Equal(totalBytes, buffered.UnderlyingBytesRead);
        Assert.Equal(1.0, buffered.GetBufferedRatio(0.25));

        // Seeking backward and forward within cached chunks performs 0 extra disk reads
        buffered.Seek(128 * 1024, SeekOrigin.Begin);
        var verify = new byte[8192];
        var verifyRead = buffered.Read(verify, 0, verify.Length);
        Assert.Equal(verify.Length, verifyRead);
        Assert.Equal(raw.AsSpan(128 * 1024, verify.Length).ToArray(), verify);
        Assert.Equal(4, buffered.UnderlyingReadCount);
    }

    [Fact]
    public void BufferedVideoStreamReportsProgressiveBufferedRatioAheadOfPlayback()
    {
        var totalBytes = 4 * 256 * 1024; // 4 chunks of 256 KB
        var raw = new byte[totalBytes];
        using var memory = new MemoryStream(raw, writable: false);
        using var buffered = new BufferedVideoStream(
            memory,
            ownsStream: false,
            chunkSizeBytes: 256 * 1024,
            maxCachedChunks: 4,
            readAheadChunks: 0);

        Assert.Equal(0.0, buffered.GetBufferedRatio(0.0));

        // Read first byte -> loads chunk 0 (25% of file)
        var oneByte = new byte[1];
        Assert.Equal(1, buffered.Read(oneByte, 0, 1));
        Assert.Equal(1, buffered.UnderlyingReadCount);
        Assert.Equal(0.25, buffered.GetBufferedRatio(0.0), precision: 3);

        // Read into chunk 1 -> chunks 0 and 1 loaded (50% of file)
        buffered.Seek(256 * 1024, SeekOrigin.Begin);
        Assert.Equal(1, buffered.Read(oneByte, 0, 1));
        Assert.Equal(2, buffered.UnderlyingReadCount);
        Assert.Equal(0.50, buffered.GetBufferedRatio(0.10), precision: 3);
    }
}
