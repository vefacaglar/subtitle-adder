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
        Assert.True(firstSession.Register("cinema_fan", "pass99"));
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
    public void MultipleProfiles_HaveCompletelyIsolatedHistoryLikesAndRatings()
    {
        var videoA = Path.Combine(_tempDir, "ep1.mp4");
        var videoB = Path.Combine(_tempDir, "ep2.mp4");

        var manager = new LocalProfileManager(_profilesPath);

        // User 1: watches ep1 3 times, likes it, rates 5 stars
        Assert.True(manager.Register("user_one", "pass1"));
        manager.RecordWatch(videoA);
        manager.RecordWatch(videoA);
        manager.RecordWatch(videoA);
        manager.SetLiked(videoA, true);
        manager.SetRating(videoA, 5);
        manager.Logout();

        // User 2: watches ep1 once (dislikes/unliked, rates 2 stars) and watches ep2 twice (likes, rates 4 stars)
        Assert.True(manager.Register("user_two", "pass2"));
        Assert.Null(manager.GetStats(videoA));
        manager.RecordWatch(videoA);
        manager.SetRating(videoA, 2);
        manager.RecordWatch(videoB);
        manager.RecordWatch(videoB);
        manager.SetLiked(videoB, true);
        manager.SetRating(videoB, 4);
        manager.Logout();

        // Verify User 1's isolated state
        Assert.True(manager.Login("user_one", "pass1"));
        var u1VideoA = manager.GetStats(videoA);
        Assert.NotNull(u1VideoA);
        Assert.Equal(3, u1VideoA.ViewCount);
        Assert.True(u1VideoA.IsLiked);
        Assert.Equal(5, u1VideoA.Rating);
        Assert.Null(manager.GetStats(videoB));

        // Verify User 2's isolated state
        Assert.True(manager.Login("user_two", "pass2"));
        var u2VideoA = manager.GetStats(videoA);
        var u2VideoB = manager.GetStats(videoB);
        Assert.NotNull(u2VideoA);
        Assert.Equal(1, u2VideoA.ViewCount);
        Assert.False(u2VideoA.IsLiked);
        Assert.Equal(2, u2VideoA.Rating);
        Assert.NotNull(u2VideoB);
        Assert.Equal(2, u2VideoB.ViewCount);
        Assert.True(u2VideoB.IsLiked);
        Assert.Equal(4, u2VideoB.Rating);
    }

    [Fact]
    public void VideoPlatformViewModel_FiltersSearchAndEngagementActionsWorkCorrectly()
    {
        var v1 = Path.Combine(_tempDir, "Alpha_Tutorial.mp4");
        var v2 = Path.Combine(_tempDir, "Beta_Documentary.mkv");
        var v3 = Path.Combine(_tempDir, "Gamma_Clip.mov");
        var v4Discovered = Path.Combine(_tempDir, "Delta_Unwatched.mp4");

        var manager = new LocalProfileManager(_profilesPath);
        using var vm = new VideoPlatformViewModel(manager);

        Assert.False(vm.IsLoggedIn);
        Assert.Empty(vm.FeedItems);

        Assert.True(vm.TryRegister("emre", "secret123"));
        Assert.True(vm.IsLoggedIn);
        Assert.Equal("emre", vm.CurrentUsername);

        vm.SetDiscoveredPaths([v1, v2, v3, v4Discovered]);

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

        // Search query on All
        vm.SearchQuery = "Delta";
        Assert.Single(vm.FeedItems);
        Assert.Equal(v4Discovered, vm.FeedItems[0].VideoPath);

        vm.SearchQuery = "";
        Assert.Equal(4, vm.FeedItems.Count);

        // Logout clears feed and active video stats
        vm.Logout();
        Assert.False(vm.IsLoggedIn);
        Assert.Empty(vm.FeedItems);
        Assert.Equal(0, vm.ActiveVideoViewCount);
        Assert.False(vm.ActiveVideoIsLiked);
        Assert.Equal(0, vm.ActiveVideoRating);
    }
}
