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

    [Fact]
    public void EncryptedVideoCacheStore_PersistsAesGcmChunksPerProfile_ZeroDiskReadsOnSeekAndReopen_AndIsolatesProfiles()
    {
        var tempRoot = Path.Combine(Path.GetTempPath(), "SubtitleAdder-VideoCacheTest-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(tempRoot);
        try
        {
            var profilesPath = Path.Combine(tempRoot, "profiles.json");
            var cacheDir = Path.Combine(tempRoot, "VideoCache");
            var videoPath = Path.Combine(tempRoot, "episode_01.mp4");

            var totalBytes = 4 * 64 * 1024; // 256 KB (4 chunks of 64 KB)
            var raw = new byte[totalBytes];
            for (var i = 0; i < raw.Length; i++)
            {
                raw[i] = (byte)((i * 73 + 19) & 0xFF);
            }
            // Embed a recognizable plaintext marker inside chunk 0 to verify on-disk ciphertext hides it
            var marker = System.Text.Encoding.UTF8.GetBytes("PLAINTEXT_VIDEO_HEADER_MARKER_DO_NOT_LEAK_ON_DISK");
            Array.Copy(marker, 0, raw, 128, marker.Length);
            File.WriteAllBytes(videoPath, raw);

            var profileManager = new LocalProfileManager(profilesPath);
            Assert.True(profileManager.Register("alice", "alicePass123"));

            var cacheStore = new EncryptedVideoCacheStore(
                cacheDirectory: cacheDir,
                profileManager: profileManager,
                maxCacheSizeBytes: 20L * 1024 * 1024 * 1024);

            var chunkSize = 64 * 1024;
            var videoIdentity = EncryptedVideoCacheStore.ComputeVideoIdentity(videoPath, totalBytes, chunkSize);

            // 1. First open under Alice: reads 4 chunks from underlying stream and encrypts them into SSD cache
            using (var hddStream1 = new MemoryStream(raw, writable: false))
            using (var buffered1 = new BufferedVideoStream(
                hddStream1,
                ownsStream: false,
                chunkSizeBytes: chunkSize,
                maxCachedChunks: 2, // Only holds 2 chunks in RAM so backward seek must hit encrypted SSD cache!
                readAheadChunks: 0,
                cacheStore: cacheStore,
                filePath: videoPath))
            {
                var buf = new byte[totalBytes];
                var readOffset = 0;
                while (readOffset < totalBytes)
                {
                    var n = buffered1.Read(buf, readOffset, Math.Min(16 * 1024, totalBytes - readOffset));
                    Assert.True(n > 0);
                    readOffset += n;
                }
                Assert.Equal(raw, buf);
                Assert.Equal(4, buffered1.UnderlyingReadCount);

                // Seek backward to chunk 0 (evicted from 2-chunk RAM window) -> served from encrypted SSD cache with 0 HDD reads!
                buffered1.Seek(0, SeekOrigin.Begin);
                var firstChunkVerify = new byte[chunkSize];
                Assert.Equal(firstChunkVerify.Length, buffered1.Read(firstChunkVerify, 0, firstChunkVerify.Length));
                Assert.Equal(raw.AsSpan(0, firstChunkVerify.Length).ToArray(), firstChunkVerify);
                Assert.Equal(4, buffered1.UnderlyingReadCount); // Still 4! Zero additional HDD reads!
                Assert.Equal(1, buffered1.EncryptedCacheReadCount);
            }

            // 2. Verify on-disk .vcache files exist and do NOT contain the plaintext marker
            var vcacheFiles = Directory.GetFiles(cacheDir, "*.vcache", SearchOption.AllDirectories);
            Assert.Equal(4, vcacheFiles.Length);
            foreach (var vcacheFile in vcacheFiles)
            {
                var diskBytes = File.ReadAllBytes(vcacheFile);
                Assert.False(diskBytes.AsSpan().IndexOf(marker) >= 0, "Encrypted .vcache file must not contain plaintext bytes");
            }

            // 3. Reopen the video under Alice with a fresh stream -> 0 underlying HDD reads across the entire video!
            using (var hddStream2 = new MemoryStream(raw, writable: false))
            using (var buffered2 = new BufferedVideoStream(
                hddStream2,
                ownsStream: false,
                chunkSizeBytes: chunkSize,
                maxCachedChunks: 2,
                readAheadChunks: 0,
                cacheStore: cacheStore,
                filePath: videoPath))
            {
                Assert.Equal(1.0, buffered2.GetBufferedRatio(0.0), precision: 3);

                var reopenedBytes = new byte[totalBytes];
                var offset = 0;
                while (offset < totalBytes)
                {
                    var n = buffered2.Read(reopenedBytes, offset, Math.Min(32 * 1024, totalBytes - offset));
                    Assert.True(n > 0);
                    offset += n;
                }

                Assert.Equal(raw, reopenedBytes);
                Assert.Equal(0, buffered2.UnderlyingReadCount); // 0 HDD reads on reopen!
                Assert.Equal(4, buffered2.EncryptedCacheReadCount);
            }

            // 4. Switch to Bob (or log out): Bob cannot read or decrypt Alice's cached chunks
            Assert.True(profileManager.Register("bob", "bobPass456"));
            Assert.Empty(cacheStore.GetCachedChunkIndices(videoIdentity, 4, chunkSize, totalBytes));
            Assert.Null(cacheStore.TryReadChunk(videoIdentity, chunkSize, 0, chunkSize));

            // Even if Bob copies Alice's .vcache file into Bob's cache folder, AES-GCM authentication with Bob's vault key rejects it
            var bobChunkDest = new byte[chunkSize];
            Assert.True(cacheStore.TryWriteChunk(videoIdentity, chunkSize, 0, bobChunkDest));
            var bobVcacheFile = Directory.GetFiles(cacheDir, "*.vcache", SearchOption.AllDirectories)
                .First(f => !vcacheFiles.Contains(f, StringComparer.OrdinalIgnoreCase));
            File.Copy(vcacheFiles[0], bobVcacheFile, overwrite: true);
            Assert.Null(cacheStore.TryReadChunk(videoIdentity, chunkSize, 0, chunkSize));

            // Logged-out users cannot read or decrypt Alice's cached chunks either
            profileManager.Logout();
            Assert.Null(cacheStore.TryReadChunk(videoIdentity, chunkSize, 0, chunkSize));
        }
        finally
        {
            try { Directory.Delete(tempRoot, recursive: true); } catch { }
        }
    }

    [Fact]
    public void VideoCacheMaxSizeGb_DefaultsTo20GbAndPersistsInSettings()
    {
        Assert.Equal(20, EncryptedVideoCacheStore.DefaultMaxSizeGb);
        var (vm, _, settings) = Make.ViewModel();
        Assert.Equal(20, vm.VideoCacheMaxSizeGb);

        vm.VideoCacheMaxSizeGb = 50;
        Assert.Equal(50, vm.VideoCacheMaxSizeGb);
        Assert.Equal(50, settings.GetInt("videoCacheMaxSizeGb"));
        Assert.Equal(50L * 1024 * 1024 * 1024, EncryptedVideoCacheStore.Shared.MaxCacheSizeBytes);
    }

    [Fact]
    public async Task PlaylistEnforcesSingleConcurrentTranslationLimit()
    {
        Assert.Equal(1, SubtitleViewModel.MaxConcurrentTranslations);
        var rootDir = Path.Combine(Path.GetTempPath(), "SubtitleAdder-SingleTranslateGate-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(rootDir);
        try
        {
            var video1 = Path.Combine(rootDir, "Video1.mp4");
            var video2 = Path.Combine(rootDir, "Video2.mp4");
            await File.WriteAllTextAsync(video1, "");
            await File.WriteAllTextAsync(video2, "");

            var engine = new PipelinedTestEngineClient { AutoCompleteExtractAudio = false };
            var settings = new InMemorySettingsStore();
            settings.SetBool(SubtitleViewModel.OfflineModeDefaultsKey, false);
            using var vm = new SubtitleViewModel(new ModelCatalog(engine), settings);
            vm.TranslationTarget = "tr";

            vm.AddToPlaylist([video1, video2], defaultSourceLanguage: "en");
            var runTask = vm.RunPlaylistAsync();

            await Make.Eventually(() => engine.ExtractAudioCalls.Count == 1, "video 1 audio extract");
            engine.FinishExtractAudio(0, Make.Result("?", Path.Combine(rootDir, "Video1.wav")));

            await Make.Eventually(() => engine.TranscribeCalls.Count == 1 && engine.ExtractAudioCalls.Count == 2, "video 1 ASR + video 2 audio extract");
            engine.FinishExtractAudio(1, Make.Result("?", Path.Combine(rootDir, "Video2.wav")));

            // Video 1 finishes ASR -> Video 1 enters Translation, Video 2 enters ASR
            engine.FinishTranscribe(0, Make.Result("en", "", new EngineKit.Sentence(1, 0, 1, "First")));
            await Make.Eventually(() => engine.TranslateCalls.Count == 1 && engine.TranscribeCalls.Count == 2, "video 1 translating + video 2 transcribing");

            // Video 2 finishes ASR while Video 1 is STILL translating -> Video 2 must wait on the single-concurrency translation gate!
            engine.FinishTranscribe(1, Make.Result("en", "", new EngineKit.Sentence(1, 0, 1, "Second")));
            await Task.Delay(60);
            Assert.Single(engine.TranslateCalls);

            // As soon as Video 1 finishes translating, Video 2 starts translating
            var req1 = engine.TranslateCalls[0].Sentences;
            engine.FinishTranslate(0, new EngineKit.EngineEvent.Translation("tr", [new EngineKit.Sentence(req1[0].Id, req1[0].Start, req1[0].End, "Birinci")]));
            await Make.Eventually(() => engine.TranslateCalls.Count == 2, "video 2 translation starts after video 1 finishes");

            var req2 = engine.TranslateCalls[1].Sentences;
            engine.FinishTranslate(1, new EngineKit.EngineEvent.Translation("tr", [new EngineKit.Sentence(req2[0].Id, req2[0].Start, req2[0].End, "İkinci")]));
            await runTask;

            Assert.All(vm.Playlist, item => Assert.Equal(PlaylistItemState.Completed, item.State));
        }
        finally
        {
            Directory.Delete(rootDir, recursive: true);
        }
    }
}
