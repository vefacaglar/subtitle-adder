using System.Text;

namespace SubtitleAdder.Core.Tests;

public sealed class LocalProfileTests : IDisposable
{
    private readonly string _tempDir;
    private readonly string _profilesPath;

    public LocalProfileTests()
    {
        _tempDir = Path.Combine(Path.GetTempPath(), "SubtitleAdderProfileTests_" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tempDir);
        _profilesPath = Path.Combine(_tempDir, "profiles.json");
    }

    public void Dispose()
    {
        try
        {
            if (Directory.Exists(_tempDir)) Directory.Delete(_tempDir, recursive: true);
        }
        catch (IOException)
        {
        }
    }

    [Fact]
    public void Register_HashesPasswordWithPbkdf2_NeverStoresPlaintextOnDisk()
    {
        var manager = new LocalProfileManager(_profilesPath);
        Assert.False(manager.IsLoggedIn);
        Assert.Null(manager.CurrentUsername);

        const string secretPassword = "MySuperSecretPassword!2026";
        var registered = manager.Register("emre", secretPassword);

        Assert.True(registered);
        Assert.True(manager.IsLoggedIn);
        Assert.Equal("emre", manager.CurrentUsername);
        Assert.Contains("emre", manager.RegisteredUsernames);
        Assert.True(File.Exists(_profilesPath));

        var rawJson = File.ReadAllText(_profilesPath);
        Assert.DoesNotContain(secretPassword, rawJson, StringComparison.OrdinalIgnoreCase);
        Assert.Contains("saltBase64", rawJson, StringComparison.Ordinal);
        Assert.Contains("passwordHashBase64", rawJson, StringComparison.Ordinal);
        Assert.Contains("100000", rawJson, StringComparison.Ordinal);
    }

    [Fact]
    public void LoginAndLogout_EnforcesPasswordAndSessionState()
    {
        var manager = new LocalProfileManager(_profilesPath);
        Assert.True(manager.Register("alice", "CorrectHorseBatteryStaple"));
        manager.Logout();

        Assert.False(manager.IsLoggedIn);
        Assert.Null(manager.CurrentUsername);

        // Wrong password fails
        Assert.False(manager.Login("alice", "WrongPassword"));
        Assert.False(manager.IsLoggedIn);
        Assert.NotNull(manager.LastError);

        // Unknown user fails
        Assert.False(manager.Login("unknown_user", "CorrectHorseBatteryStaple"));
        Assert.False(manager.IsLoggedIn);

        // Correct password succeeds (case-insensitive username match, preserves registered display casing)
        Assert.True(manager.Login("ALICE", "CorrectHorseBatteryStaple"));
        Assert.True(manager.IsLoggedIn);
        Assert.Equal("alice", manager.CurrentUsername);
        Assert.Null(manager.LastError);
    }

    [Fact]
    public void LoggedOutState_DoesNotRecordWatchHistoryOrExposeStats()
    {
        var videoPath = Path.Combine(_tempDir, "lecture.mp4");
        var manager = new LocalProfileManager(_profilesPath);
        Assert.True(manager.Register("alice", "pw123"));
        manager.RecordWatch(videoPath, positionSeconds: 42, durationSeconds: 120);
        manager.SetLiked(videoPath, true);
        manager.SetRating(videoPath, 5);
        manager.Logout();

        // While logged out, stats and feeds are locked/empty and operations are no-ops
        Assert.Null(manager.GetStats(videoPath));
        Assert.Empty(manager.GetFeed(VideoFeedFilter.All));
        Assert.Empty(manager.GetFeed(VideoFeedFilter.History));
        Assert.Equal(0, manager.TotalWatchedVideosCount);
        Assert.Equal(0, manager.TotalLikedVideosCount);

        Assert.Null(manager.RecordWatch(videoPath, positionSeconds: 99, durationSeconds: 120));
        Assert.False(manager.ToggleLike(videoPath));
        Assert.Null(manager.SetRating(videoPath, 1));
        Assert.Null(manager.CreateCustomList("Test List"));

        // Log back in and verify nothing was mutated while logged out
        Assert.True(manager.Login("alice", "pw123"));
        var stats = manager.GetStats(videoPath);
        Assert.NotNull(stats);
        Assert.Equal(1, stats.ViewCount);
        Assert.True(stats.IsLiked);
        Assert.Equal(5, stats.Rating);
        Assert.Equal(42, stats.LastPositionSeconds);
    }

    [Fact]
    public void Persistence_ReloadsViewCountLikesRatingsAndLastWatchedFromDisk()
    {
        var videoPath = Path.Combine(_tempDir, "movie.mkv");

        var firstSession = new LocalProfileManager(_profilesPath);
        Assert.True(firstSession.Register("cinema_fan", "pass99", rememberMe: false));
        firstSession.RecordWatch(videoPath, positionSeconds: 15, durationSeconds: 300);
        firstSession.RecordWatch(videoPath, positionSeconds: 180, durationSeconds: 300);
        firstSession.UpdatePlaybackProgress(videoPath, positionSeconds: 210, durationSeconds: 300);
        firstSession.SetLiked(videoPath, true);
        firstSession.SetRating(videoPath, 4);

        var secondSession = new LocalProfileManager(_profilesPath);
        Assert.False(secondSession.IsLoggedIn);
        Assert.Contains("cinema_fan", secondSession.RegisteredUsernames);

        Assert.True(secondSession.Login("cinema_fan", "pass99"));
        var restored = secondSession.GetStats(videoPath);
        Assert.NotNull(restored);
        Assert.Equal(2, restored.ViewCount);
        Assert.True(restored.IsLiked);
        Assert.Equal(4, restored.Rating);
        Assert.Equal(210, restored.LastPositionSeconds);
        Assert.Equal(300, restored.DurationSeconds);
        Assert.NotNull(restored.LastWatchedAtUtc);
    }

    [Fact]
    public void RememberMe_DefaultTrue_AutoRestoresEncryptedSessionAcrossNewInstancesUntilLogout()
    {
        var videoPath = Path.Combine(_tempDir, "remembered_show.mp4");

        var firstSession = new LocalProfileManager(_profilesPath);
        Assert.True(firstSession.Register("emre", "pass2026")); // default rememberMe = true
        Assert.True(firstSession.RememberMe);
        firstSession.RecordWatch(videoPath, positionSeconds: 50, durationSeconds: 200);
        firstSession.SetLiked(videoPath, true);

        // A 2nd instance or app restart automatically unlocks the profile without re-entering the password
        var secondSession = new LocalProfileManager(_profilesPath);
        Assert.True(secondSession.IsLoggedIn);
        Assert.Equal("emre", secondSession.CurrentUsername);
        Assert.Equal("emre", secondSession.LastUsername);
        var stats = secondSession.GetStats(videoPath);
        Assert.NotNull(stats);
        Assert.Equal(1, stats.ViewCount);
        Assert.True(stats.IsLiked);

        // Logging out clears the remembered key so a 3rd instance starts logged out
        secondSession.Logout();
        var thirdSession = new LocalProfileManager(_profilesPath);
        Assert.False(thirdSession.IsLoggedIn);
        Assert.Equal("emre", thirdSession.LastUsername);
    }

    [Fact]
    public void EncryptedVaultAndThumbnails_NeverExposePlaintextVideoNamesListsOrThumbnailsOnDisk()
    {
        var secretVideoPath = Path.Combine(_tempDir, "TopSecret_Project_Alpha_2026.mkv");
        const string secretListName = "Gizli Özel Koleksiyonum";
        var sampleThumbBytes = Encoding.UTF8.GetBytes("FAKE_PNG_THUMBNAIL_PAYLOAD_987654321");

        var manager = new LocalProfileManager(_profilesPath);
        Assert.True(manager.Register("vault_user", "UltraStrongVaultPass!99", rememberMe: false));
        manager.RecordWatch(secretVideoPath, positionSeconds: 33, durationSeconds: 100);
        manager.SetLiked(secretVideoPath, true);
        manager.SetRating(secretVideoPath, 5);
        var list = manager.CreateCustomList(secretListName);
        Assert.NotNull(list);
        Assert.True(manager.AddVideoToCustomList(list.Id, secretVideoPath));
        Assert.True(manager.SetThumbnail(secretVideoPath, sampleThumbBytes));

        // Inspect raw profiles.json on disk: must NOT contain video filename, folder path, custom list name, or thumbnail payload
        var rawJson = File.ReadAllText(_profilesPath);
        Assert.Contains("encryptedVaultBase64", rawJson, StringComparison.Ordinal);
        Assert.DoesNotContain("TopSecret_Project_Alpha_2026", rawJson, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain(secretListName, rawJson, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain(Convert.ToBase64String(sampleThumbBytes), rawJson, StringComparison.Ordinal);

        // Reload in a fresh manager, log in with password, and verify everything + thumbnail bytes decrypt cleanly
        var reloaded = new LocalProfileManager(_profilesPath);
        Assert.False(reloaded.IsLoggedIn);
        Assert.Null(reloaded.GetThumbnail(secretVideoPath));

        Assert.True(reloaded.Login("vault_user", "UltraStrongVaultPass!99"));
        var stats = reloaded.GetStats(secretVideoPath);
        Assert.NotNull(stats);
        Assert.Equal(1, stats.ViewCount);
        Assert.True(stats.IsLiked);
        Assert.Equal(5, stats.Rating);
        Assert.NotNull(stats.ThumbnailBytes);
        Assert.Equal(sampleThumbBytes, stats.ThumbnailBytes);
        Assert.Equal(sampleThumbBytes, reloaded.GetThumbnail(secretVideoPath));

        var feed = reloaded.GetFeed(VideoFeedFilter.History);
        Assert.Single(feed);
        Assert.Equal(sampleThumbBytes, feed[0].ThumbnailBytes);
    }

    [Fact]
    public void CrossInstanceLiveSync_TwoConcurrentInstancesLoggedIntoSameProfileMergeAllHistoryLikesRatingsAndLists()
    {
        var video1 = Path.Combine(_tempDir, "Instance1_Episode01.mp4");
        var video2 = Path.Combine(_tempDir, "Instance2_Episode02.mp4");

        // Instance 1 registers and logs in (with RememberMe = true)
        var instance1 = new LocalProfileManager(_profilesPath);
        Assert.True(instance1.Register("shared_user", "shared_pass_123"));

        // Instance 2 opens concurrently while Instance 1 is still open and auto-restores the same profile
        var instance2 = new LocalProfileManager(_profilesPath);
        Assert.True(instance2.IsLoggedIn);
        Assert.Equal("shared_user", instance2.CurrentUsername);

        // Instance 1 watches video1 and likes it
        instance1.RecordWatch(video1, positionSeconds: 30, durationSeconds: 120);
        instance1.SetLiked(video1, true);

        // Instance 2 watches video2, rates it 5 stars, and creates a custom list containing video2
        instance2.RecordWatch(video2, positionSeconds: 90, durationSeconds: 180);
        instance2.SetRating(video2, 5);
        var listFrom2 = instance2.CreateCustomList("İki Pencere Ortak Liste");
        Assert.NotNull(listFrom2);
        Assert.True(instance2.AddVideoToCustomList(listFrom2.Id, video2));

        // Instance 1 watches video1 a second time AND adds video1 to the custom list created by Instance 2
        instance1.RecordWatch(video1, positionSeconds: 60, durationSeconds: 120);
        Assert.True(instance1.AddVideoToCustomList(listFrom2.Id, video1));

        // Verify BOTH Instance 1 and Instance 2 see BOTH video1 (2 views, liked) and video2 (1 view, 5 stars) and the merged custom list
        var history1 = instance1.GetFeed(VideoFeedFilter.History);
        var history2 = instance2.GetFeed(VideoFeedFilter.History);
        Assert.Equal(2, history1.Count);
        Assert.Equal(2, history2.Count);

        var v1OnInst2 = instance2.GetStats(video1);
        Assert.NotNull(v1OnInst2);
        Assert.Equal(2, v1OnInst2.ViewCount);
        Assert.True(v1OnInst2.IsLiked);

        var v2OnInst1 = instance1.GetStats(video2);
        Assert.NotNull(v2OnInst1);
        Assert.Equal(1, v2OnInst1.ViewCount);
        Assert.Equal(5, v2OnInst1.Rating);

        var listsOn1 = instance1.GetCustomLists();
        var listsOn2 = instance2.GetCustomLists();
        Assert.Single(listsOn1);
        Assert.Single(listsOn2);
        Assert.Equal(2, listsOn1[0].VideoPaths.Count);
        Assert.Equal(2, listsOn2[0].VideoPaths.Count);
    }

    [Fact]
    public async Task SilentBackgroundUpdates_PlaybackProgressAndThumbnailsAndUnchangedDiscoveredPathsNeverRebuildFeedItems()
    {
        var video1 = Path.Combine(_tempDir, "Playing_Movie.mp4");
        var manager = new LocalProfileManager(_profilesPath);
        using var vm = new VideoPlatformViewModel(manager);
        Assert.True(vm.TryRegister("emre", "pass123"));

        vm.SetDiscoveredPaths([video1]);
        vm.OnVideoOpened(video1);

        var initialFeedInstance = vm.FeedItems;
        var stateChangedCount = 0;
        var feedPropertyChangedCount = 0;
        manager.StateChanged += (_, _) => Interlocked.Increment(ref stateChangedCount);
        vm.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(VideoPlatformViewModel.FeedItems))
            {
                Interlocked.Increment(ref feedPropertyChangedCount);
            }
        };

        // 1. Periodic playback progress updates while watching a video must never fire StateChanged or replace FeedItems
        vm.UpdateActiveVideoProgress(15.0, 300.0);
        vm.UpdateActiveVideoProgress(30.0, 300.0);
        vm.UpdateActiveVideoProgress(45.0, 300.0);

        // 2. Saving encrypted thumbnails from video cards must never fire StateChanged or replace FeedItems
        var thumbBytes = Encoding.UTF8.GetBytes("SAMPLE_THUMBNAIL_BYTES");
        Assert.True(vm.SaveEncryptedThumbnail(video1, thumbBytes));
        Assert.True(vm.SaveEncryptedThumbnail(video1, thumbBytes));

        // 3. Passing the identical discovered paths list must be a no-op and not replace FeedItems
        vm.SetDiscoveredPaths([video1]);

        // Wait briefly to confirm FileSystemWatcher self-write suppression also prevents any delayed callback
        await Task.Delay(120);

        Assert.Equal(0, stateChangedCount);
        Assert.Equal(0, feedPropertyChangedCount);
        Assert.Same(initialFeedInstance, vm.FeedItems);
        Assert.Equal(45.0, manager.GetStats(video1)?.LastPositionSeconds);
        Assert.Equal(thumbBytes, vm.GetEncryptedThumbnail(video1));
    }

    [Fact]
    public async Task StateSyncBroker_FiveConcurrentInstancesCommunicateViaNamedPipeAndLastWindowClosesEngine()
    {
        var channelScope = $"test-broker-5-instances-{Guid.NewGuid():N}";
        Assert.False(StateSyncBroker.IsChannelEngineRunning(channelScope));

        // 1. Open 1st application window -> starts the shared background Pub/Sub Engine
        var firstBroker = new StateSyncBroker(channelScope);
        Assert.True(StateSyncBroker.IsChannelEngineRunning(channelScope));
        Assert.Equal(1, firstBroker.ActiveInstanceCount);
        Assert.True(firstBroker.IsLastInstance);

        // 2. Open 2nd, 3rd, 4th, 5th application windows -> all connect to the shared Engine
        var brokers = new List<StateSyncBroker> { firstBroker };
        for (var i = 1; i < 5; i++)
        {
            brokers.Add(new StateSyncBroker(channelScope));
        }

        var managers = new List<LocalProfileManager>();

        try
        {
            foreach (var b in brokers)
            {
                Assert.True(await b.WaitUntilReadyAsync(TimeSpan.FromSeconds(5)));
            }

            await WaitUntilAsync(() => brokers[0].ConnectedClientCount == 5, timeoutMs: 3000);
            Assert.Equal(5, brokers[0].ActiveInstanceCount);
            Assert.All(brokers, b =>
            {
                Assert.True(b.IsEngineRunning);
                Assert.True(b.IsConnectedToLeader);
                Assert.True(b.HasOtherActiveInstances);
                Assert.False(b.IsLastInstance);
            });

            // Create 5 LocalProfileManager instances, each bound to its own StateSyncBroker node
            for (var i = 0; i < 5; i++)
            {
                managers.Add(new LocalProfileManager(_profilesPath, brokers[i]));
            }

            var ep1 = Path.Combine(_tempDir, "Episode_01.mp4");
            var ep2 = Path.Combine(_tempDir, "Episode_02.mp4");

            // Instance 0 registers and logs in -> broadcast over Named Pipe Engine to Instances 1..4
            Assert.True(managers[0].Register("five_window_user", "Pass!12345"));
            await WaitUntilAsync(() => managers.All(m => m.IsLoggedIn && m.CurrentUsername == "five_window_user"));

            // Instance 2 watches ep1 and likes it -> broadcast over Named Pipe Engine to all 5 instances
            managers[2].RecordWatch(ep1, positionSeconds: 10, durationSeconds: 100);
            managers[2].SetLiked(ep1, true);
            await WaitUntilAsync(() => managers.All(m => m.GetStats(ep1) is { ViewCount: 1, IsLiked: true }));

            // Instance 3 updates watch-progress and thumbnail -> delivered silently in memory to all 5 without firing StateChanged
            var stateChangedEvents = 0;
            foreach (var m in managers)
            {
                m.StateChanged += (_, _) => Interlocked.Increment(ref stateChangedEvents);
            }

            var sampleThumb = Encoding.UTF8.GetBytes("ENCRYPTED_THUMB_FROM_INSTANCE_3");
            managers[3].UpdatePlaybackProgress(ep1, positionSeconds: 77, durationSeconds: 100);
            managers[3].SetThumbnail(ep1, sampleThumb);

            await WaitUntilAsync(() => managers.All(m =>
                m.GetStats(ep1)?.LastPositionSeconds == 77 &&
                m.GetThumbnail(ep1) is { Length: > 0 }));
            Assert.Equal(0, stateChangedEvents);

            // 3. Close the 1st window (the one that originally started the Engine!):
            // Because Windows 2, 3, 4, 5 are still open, the Engine MUST NOT close!
            managers[0].Dispose();
            brokers[0].Dispose();
            Assert.False(brokers[0].ClosedEngineOnDispose);
            Assert.True(StateSyncBroker.IsChannelEngineRunning(channelScope));
            Assert.Equal(4, brokers[1].ActiveInstanceCount);
            await WaitUntilAsync(() => brokers[1].ConnectedClientCount == 4, timeoutMs: 3000);

            // Verify the remaining 4 open instances continue communicating via the Engine without interruption
            managers[2].RecordWatch(ep2, positionSeconds: 25, durationSeconds: 120);
            var sharedList = managers[2].CreateCustomList("Beş Pencere Ortak Liste");
            Assert.NotNull(sharedList);
            Assert.True(managers[2].AddVideoToCustomList(sharedList.Id, ep2));

            await WaitUntilAsync(() => managers.Skip(1).All(m =>
                m.GetStats(ep2)?.ViewCount == 1 &&
                m.GetCustomLists().Any(l => l.Name == "Beş Pencere Ortak Liste" && l.VideoPaths.Contains(ep2))));

            // 4. Close Windows 2, 3, and 4 one by one -> Engine stays alive while at least 1 window remains open
            for (var i = 1; i <= 3; i++)
            {
                managers[i].Dispose();
                brokers[i].Dispose();
                Assert.False(brokers[i].ClosedEngineOnDispose);
                Assert.True(StateSyncBroker.IsChannelEngineRunning(channelScope));
                Assert.Equal(5 - (i + 1), brokers[4].ActiveInstanceCount);
            }

            // Window 5 (index 4) is now the last remaining open window
            Assert.True(brokers[4].IsLastInstance);
            Assert.False(brokers[4].HasOtherActiveInstances);
            Assert.True(brokers[4].IsEngineRunning);

            // 5. Close Window 5 (the LAST window) -> IT shuts down the Engine!
            managers[4].Dispose();
            brokers[4].Dispose();
            Assert.True(brokers[4].ClosedEngineOnDispose);
            Assert.False(brokers[4].IsEngineRunning);
            Assert.False(StateSyncBroker.IsChannelEngineRunning(channelScope));
        }
        finally
        {
            foreach (var m in managers) m.Dispose();
            foreach (var b in brokers) b.Dispose();
        }
    }

    private static async Task WaitUntilAsync(Func<bool> condition, int timeoutMs = 3000)
    {
        var deadline = Environment.TickCount64 + timeoutMs;
        while (Environment.TickCount64 < deadline)
        {
            if (condition()) return;
            await Task.Delay(15);
        }
        Assert.True(condition());
    }

    [Fact]
    public void MultipleProfiles_HaveCompletelyIsolatedHistoryLikesRatingsAndCustomLists()
    {
        var videoA = Path.Combine(_tempDir, "ep1.mp4");
        var videoB = Path.Combine(_tempDir, "ep2.mp4");

        var manager = new LocalProfileManager(_profilesPath);

        // User 1: watches ep1 3 times, likes it, rates 5 stars, creates custom list "Bilim Kurgu"
        Assert.True(manager.Register("user_one", "pass1"));
        manager.RecordWatch(videoA);
        manager.RecordWatch(videoA);
        manager.RecordWatch(videoA);
        manager.SetLiked(videoA, true);
        manager.SetRating(videoA, 5);
        var u1List = manager.CreateCustomList("Bilim Kurgu");
        Assert.NotNull(u1List);
        Assert.True(manager.AddVideoToCustomList(u1List.Id, videoA));
        manager.Logout();

        // User 2: watches ep1 once (unliked, rates 2 stars) and watches ep2 twice (likes, rates 4 stars), creates "Komedi"
        Assert.True(manager.Register("user_two", "pass2"));
        Assert.Null(manager.GetStats(videoA));
        Assert.Empty(manager.GetCustomLists());
        manager.RecordWatch(videoA);
        manager.SetRating(videoA, 2);
        manager.RecordWatch(videoB);
        manager.RecordWatch(videoB);
        manager.SetLiked(videoB, true);
        manager.SetRating(videoB, 4);
        var u2List = manager.CreateCustomList("Komedi");
        Assert.NotNull(u2List);
        Assert.True(manager.AddVideoToCustomList(u2List.Id, videoB));
        manager.Logout();

        // Verify User 1's isolated state from a fresh manager reload
        var reloaded = new LocalProfileManager(_profilesPath);
        Assert.True(reloaded.Login("user_one", "pass1"));
        var u1VideoA = reloaded.GetStats(videoA);
        Assert.NotNull(u1VideoA);
        Assert.Equal(3, u1VideoA.ViewCount);
        Assert.True(u1VideoA.IsLiked);
        Assert.Equal(5, u1VideoA.Rating);
        Assert.Null(reloaded.GetStats(videoB));
        var u1Lists = reloaded.GetCustomLists();
        Assert.Single(u1Lists);
        Assert.Equal("Bilim Kurgu", u1Lists[0].Name);
        Assert.Equal([videoA], u1Lists[0].VideoPaths);

        // Verify User 2's isolated state
        Assert.True(reloaded.Login("user_two", "pass2"));
        var u2VideoA = reloaded.GetStats(videoA);
        var u2VideoB = reloaded.GetStats(videoB);
        Assert.NotNull(u2VideoA);
        Assert.Equal(1, u2VideoA.ViewCount);
        Assert.False(u2VideoA.IsLiked);
        Assert.Equal(2, u2VideoA.Rating);
        Assert.NotNull(u2VideoB);
        Assert.Equal(2, u2VideoB.ViewCount);
        Assert.True(u2VideoB.IsLiked);
        Assert.Equal(4, u2VideoB.Rating);
        var u2Lists = reloaded.GetCustomLists();
        Assert.Single(u2Lists);
        Assert.Equal("Komedi", u2Lists[0].Name);
        Assert.Equal([videoB], u2Lists[0].VideoPaths);
    }

    [Fact]
    public void CustomListsAndClearWatchHistory_WorkAndPreserveFavoritesAndRatings()
    {
        var v1 = Path.Combine(_tempDir, "Sezon_1_Bolum_01.mkv");
        var v2 = Path.Combine(_tempDir, "Sezon_2_Bolum_05.mkv");
        var v3 = Path.Combine(_tempDir, "Belgesel_Uzay.mp4");

        var manager = new LocalProfileManager(_profilesPath);
        using var vm = new VideoPlatformViewModel(manager);
        Assert.True(vm.TryRegister("emre", "pw123"));

        // Create two custom lists
        var watchLater = vm.CreateCustomList("Daha Sonra İzle");
        var bestEpisodes = vm.CreateCustomList("En İyi Bölümler");
        Assert.NotNull(watchLater);
        Assert.NotNull(bestEpisodes);
        Assert.Null(vm.CreateCustomList("daha sonra izle")); // Duplicate name prevented

        // Add v2 (even before watching!) and v3 to custom lists
        Assert.True(vm.ToggleVideoInCustomList(watchLater.Id, v2));
        Assert.True(vm.ToggleVideoInCustomList(bestEpisodes.Id, v1));
        Assert.True(vm.ToggleVideoInCustomList(bestEpisodes.Id, v2));

        // Watch v1 and v2, favorite v1, rate v2 5 stars, watch v3 without liking/rating
        vm.OnVideoOpened(v1);
        vm.ToggleActiveVideoLike();
        vm.OnVideoOpened(v2);
        vm.SetActiveVideoRating(5);
        vm.OnVideoOpened(v3);

        // Select custom list "En İyi Bölümler" -> contains v1 and v2 with their custom list badges
        vm.SelectCustomList(bestEpisodes.Id);
        Assert.Equal(VideoFeedFilter.CustomList, vm.ActiveFilter);
        Assert.Equal("En İyi Bölümler", vm.ActiveCustomListName);
        Assert.Equal(2, vm.FeedItems.Count);
        var v2Card = vm.FeedItems.First(c => c.VideoPath == v2);
        Assert.Equal(5, v2Card.Rating);
        Assert.Contains("Daha Sonra İzle", v2Card.CustomListNames);
        Assert.Contains("En İyi Bölümler", v2Card.CustomListNames);

        // ClearWatchHistory resets ViewCount and LastWatchedAtUtc while preserving Favorites, Ratings, and Custom Lists
        Assert.True(vm.ClearWatchHistory());
        Assert.Equal(0, vm.TotalWatchedVideosCount);
        Assert.Equal(1, vm.TotalLikedVideosCount);
        Assert.Equal(1, vm.TotalRatedVideosCount);

        vm.ActiveFilter = VideoFeedFilter.History;
        Assert.Empty(vm.FeedItems);

        vm.ActiveFilter = VideoFeedFilter.Liked;
        Assert.Single(vm.FeedItems);
        Assert.Equal(v1, vm.FeedItems[0].VideoPath);
        Assert.Equal(0, vm.FeedItems[0].ViewCount);

        vm.ActiveFilter = VideoFeedFilter.TopRated;
        Assert.Single(vm.FeedItems);
        Assert.Equal(v2, vm.FeedItems[0].VideoPath);
        Assert.Equal(5, vm.FeedItems[0].Rating);

        vm.SelectCustomList(bestEpisodes.Id);
        Assert.Equal(2, vm.FeedItems.Count);
    }

    [Fact]
    public void VideoPlatformViewModel_FiltersMultiTokenSearchAndEngagementActionsWorkCorrectly()
    {
        var v1 = Path.Combine(_tempDir, "Alpha_Tutorial.mp4");
        var v2 = Path.Combine(_tempDir, "Beta_Documentary.mkv");
        var v3 = Path.Combine(_tempDir, "Gamma_Clip.mov");
        var v4Discovered = Path.Combine(_tempDir, "Sezon_2_Bolum_05.mp4");
        File.WriteAllText(Path.Combine(_tempDir, "Alpha_Tutorial.srt"), "1\n00:00:01,000 --> 00:00:02,000\nMerhaba\n");
        File.WriteAllText(Path.Combine(_tempDir, "Sezon_2_Bolum_05.tr.srt"), "1\n00:00:01,000 --> 00:00:02,000\nSelam\n");

        var manager = new LocalProfileManager(_profilesPath);
        using var vm = new VideoPlatformViewModel(manager);

        Assert.False(vm.IsLoggedIn);
        Assert.Empty(vm.FeedItems);

        Assert.True(vm.TryRegister("emre", "secret123"));
        Assert.True(vm.IsLoggedIn);
        Assert.Equal("emre", vm.CurrentUsername);

        vm.SetDiscoveredPaths([v1, v2, v3, v4Discovered]);

        // Verify subtitle detection on cards (v1 has exact .srt, v4 has .tr.srt, v2 & v3 have no subtitle)
        var cardV1 = vm.FeedItems.First(c => c.VideoPath == v1);
        var cardV2 = vm.FeedItems.First(c => c.VideoPath == v2);
        var cardV4 = vm.FeedItems.First(c => c.VideoPath == v4Discovered);
        Assert.True(cardV1.HasSubtitle);
        Assert.Equal("SRT", cardV1.SubtitleBadgeText);
        Assert.False(cardV2.HasSubtitle);
        Assert.Null(cardV2.SubtitleBadgeText);
        Assert.True(cardV4.HasSubtitle);
        Assert.Equal("SRT", cardV4.SubtitleBadgeText);

        // Watch v1 once, like it, rate 3 stars
        vm.OnVideoOpened(v1);
        vm.ToggleActiveVideoLike();
        vm.SetActiveVideoRating(3);
        Assert.Equal(1, vm.ActiveVideoViewCount);
        Assert.True(vm.ActiveVideoIsLiked);
        Assert.Equal(3, vm.ActiveVideoRating);

        // Watch v2 three times, rate 5 stars (not liked)
        vm.OnVideoOpened(v2);
        vm.OnVideoOpened(v2);
        vm.OnVideoOpened(v2);
        vm.SetActiveVideoRating(5);

        // Watch v3 twice, like it, rate 4 stars
        vm.OnVideoOpened(v3);
        vm.OnVideoOpened(v3);
        vm.ToggleItemLike(v3);
        vm.SetItemRating(v3, 4);

        // Filter: MostWatched -> v2 (3 views), v3 (2 views), v1 (1 view)
        vm.ActiveFilter = VideoFeedFilter.MostWatched;
        Assert.Equal(3, vm.FeedItems.Count);
        Assert.Equal([v2, v3, v1], vm.FeedItems.Select(i => i.VideoPath).ToArray());

        // Filter: TopRated -> v2 (5 stars), v3 (4 stars), v1 (3 stars)
        vm.ActiveFilter = VideoFeedFilter.TopRated;
        Assert.Equal(3, vm.FeedItems.Count);
        Assert.Equal([v2, v3, v1], vm.FeedItems.Select(i => i.VideoPath).ToArray());

        // Filter: Liked -> v3 and v1
        vm.ActiveFilter = VideoFeedFilter.Liked;
        Assert.Equal(2, vm.FeedItems.Count);
        Assert.Contains(vm.FeedItems, i => i.VideoPath == v1);
        Assert.Contains(vm.FeedItems, i => i.VideoPath == v3);

        // Filter: History -> all 3 watched videos (v4Discovered is unwatched so excluded)
        vm.ActiveFilter = VideoFeedFilter.History;
        Assert.Equal(3, vm.FeedItems.Count);
        Assert.DoesNotContain(vm.FeedItems, i => i.VideoPath == v4Discovered);

        // Filter: All -> includes all 4 (watched + discovered unwatched)
        vm.ActiveFilter = VideoFeedFilter.All;
        Assert.Equal(4, vm.FeedItems.Count);

        // Multi-token search query on All ("sezon 2 bolum" matches "Sezon_2_Bolum_05.mp4")
        vm.SearchQuery = "sezon 2 bolum";
        Assert.Single(vm.FeedItems);
        Assert.Equal(v4Discovered, vm.FeedItems[0].VideoPath);

        vm.SearchQuery = "";
        Assert.Equal(4, vm.FeedItems.Count);

        // Logout clears active user stats and locks personal tabs while still allowing anonymous SSD search on "All"
        vm.Logout();
        Assert.False(vm.IsLoggedIn);
        Assert.Equal(0, vm.ActiveVideoViewCount);
        Assert.False(vm.ActiveVideoIsLiked);
        Assert.Equal(0, vm.ActiveVideoRating);
        Assert.All(vm.FeedItems, item =>
        {
            Assert.Equal(0, item.ViewCount);
            Assert.False(item.IsLiked);
            Assert.Equal(0, item.Rating);
        });

        vm.ActiveFilter = VideoFeedFilter.History;
        Assert.True(vm.RequiresAuthForCurrentTab);
        Assert.Empty(vm.FeedItems);
    }

    [Fact]
    public void MovingOrRenamingVideoFile_PreservesHistoryLikesRatingsAndCustomLists()
    {
        var originalPath = Path.Combine(_tempDir, "video_v1.mp4");
        var sampleBytes = new byte[150_000];
        Random.Shared.NextBytes(sampleBytes);
        File.WriteAllBytes(originalPath, sampleBytes);

        var expectedFp = LocalProfileManager.ComputeVideoId(originalPath);
        Assert.StartsWith("fp:150000:", expectedFp, StringComparison.Ordinal);

        var manager = new LocalProfileManager(_profilesPath);
        Assert.True(manager.Register("emre", "pass123"));

        // Watch twice, like, rate 5 stars, and add to custom list at originalPath
        manager.RecordWatch(originalPath, positionSeconds: 65, durationSeconds: 200);
        manager.RecordWatch(originalPath, positionSeconds: 120, durationSeconds: 200);
        manager.SetLiked(originalPath, true);
        manager.SetRating(originalPath, 5);
        var favList = manager.CreateCustomList("Arşivim");
        Assert.NotNull(favList);
        Assert.True(manager.AddVideoToCustomList(favList.Id, originalPath));

        // Move and rename the video on disk to subfolder/renamed_video.mp4
        var subFolder = Path.Combine(_tempDir, "subfolder");
        Directory.CreateDirectory(subFolder);
        var movedPath = Path.Combine(subFolder, "renamed_video.mp4");
        File.Move(originalPath, movedPath);

        Assert.Equal(expectedFp, LocalProfileManager.ComputeVideoId(movedPath));

        // Querying GetStats(movedPath) recognizes the file by its fast content fingerprint and migrates its path
        var migratedStats = manager.GetStats(movedPath);
        Assert.NotNull(migratedStats);
        Assert.Equal(expectedFp, migratedStats.ContentFingerprint);
        Assert.Equal(movedPath, migratedStats.VideoPath);
        Assert.Equal("renamed_video.mp4", migratedStats.FileName);
        Assert.Equal(2, migratedStats.ViewCount);
        Assert.True(migratedStats.IsLiked);
        Assert.Equal(5, migratedStats.Rating);
        Assert.Equal(120, migratedStats.LastPositionSeconds);

        // Verify custom list membership and feed items were migrated to movedPath as well
        Assert.True(manager.IsVideoInCustomList(favList.Id, movedPath));
        var customListFeed = manager.GetFeed(VideoFeedFilter.CustomList, customListId: favList.Id);
        Assert.Single(customListFeed);
        Assert.Equal(movedPath, customListFeed[0].VideoPath);
        Assert.Equal(2, customListFeed[0].ViewCount);
        Assert.True(customListFeed[0].IsLiked);
        Assert.Equal(5, customListFeed[0].Rating);
    }
}
