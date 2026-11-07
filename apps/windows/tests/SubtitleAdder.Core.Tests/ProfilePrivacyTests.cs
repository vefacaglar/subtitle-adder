using System.Text;
using System.Text.Json.Nodes;

namespace SubtitleAdder.Core.Tests;

/// <summary>
/// Watch history must never be visible without an unlocked profile: remember-me is opt-in, legacy
/// pre-ticked sessions are dropped, a logout always wins, and other profiles' events are ignored.
/// </summary>
public sealed class ProfilePrivacyTests : IDisposable
{
    private readonly string _tempDir;
    private readonly string _profilesPath;
    private readonly string _scope = "privacy-tests-" + Guid.NewGuid().ToString("N");

    public ProfilePrivacyTests()
    {
        _tempDir = Path.Combine(Path.GetTempPath(), "SubtitleAdderPrivacyTests_" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tempDir);
        _profilesPath = Path.Combine(_tempDir, "profiles.json");
    }

    public void Dispose()
    {
        try
        {
            if (File.Exists(_profilesPath)) File.SetAttributes(_profilesPath, FileAttributes.Normal);
            if (Directory.Exists(_tempDir)) Directory.Delete(_tempDir, recursive: true);
        }
        catch (IOException)
        {
        }
        catch (UnauthorizedAccessException)
        {
        }
    }

    [Fact]
    public void RememberMe_IsOffByDefault_SoARestartStartsLoggedOut()
    {
        var video = Path.Combine(_tempDir, "private.mp4");
        using var broker = new StateSyncBroker(_scope);
        using (var first = new LocalProfileManager(_profilesPath, broker))
        {
            Assert.False(first.RememberMe);
            Assert.True(first.Register("emre", "pass123"));
            Assert.False(first.RememberMe);
            first.RecordWatch(video, positionSeconds: 10, durationSeconds: 100);
            Assert.True(first.FlushPendingWrites(TimeSpan.FromSeconds(5)));

            var json = File.ReadAllText(_profilesPath);
            Assert.DoesNotContain("rememberedProtectedKeyBase64", json, StringComparison.Ordinal);
            Assert.DoesNotContain("rememberMeOptIn", json, StringComparison.Ordinal);
        }

        using var restarted = new LocalProfileManager(_profilesPath, broker);
        Assert.False(restarted.IsLoggedIn);
        Assert.False(restarted.RememberMe);
        Assert.Empty(restarted.GetFeed(VideoFeedFilter.History));
        Assert.Null(restarted.GetStats(video));
    }

    [Fact]
    public void RememberMe_TickedByUser_WritesOptInMarkerAndRestores()
    {
        using var broker = new StateSyncBroker(_scope);
        using (var first = new LocalProfileManager(_profilesPath, broker))
        {
            Assert.True(first.Register("emre", "pass123", rememberMe: true));
            var json = JsonNode.Parse(File.ReadAllText(_profilesPath))!;
            Assert.True(json["rememberMeOptIn"]!.GetValue<bool>());
        }

        using var restarted = new LocalProfileManager(_profilesPath, broker);
        Assert.True(restarted.IsLoggedIn);
        Assert.True(restarted.RememberMe);
        Assert.Equal("emre", restarted.CurrentUsername);
    }

    [Fact]
    public void LegacyDocument_WithoutOptInMarker_DropsRememberedSessionOnceAndPersists()
    {
        var video = Path.Combine(_tempDir, "legacy.mp4");
        using var broker = new StateSyncBroker(_scope);
        using (var seed = new LocalProfileManager(_profilesPath, broker))
        {
            Assert.True(seed.Register("emre", "pass123", rememberMe: true));
            seed.RecordWatch(video, positionSeconds: 5, durationSeconds: 50);
            Assert.True(seed.FlushPendingWrites(TimeSpan.FromSeconds(5)));
        }

        // Simulate a document written by the previous version (remember-me pre-ticked, no marker).
        var node = JsonNode.Parse(File.ReadAllText(_profilesPath))!.AsObject();
        Assert.True(node.Remove("rememberMeOptIn"));
        Assert.NotNull(node["rememberedProtectedKeyBase64"]);
        File.WriteAllText(_profilesPath, node.ToJsonString());

        using (var upgraded = new LocalProfileManager(_profilesPath, broker))
        {
            Assert.False(upgraded.IsLoggedIn);
            Assert.False(upgraded.RememberMe);
            Assert.Empty(upgraded.GetFeed(VideoFeedFilter.History));
            Assert.Equal("emre", upgraded.LastUsername);
        }

        var cleaned = JsonNode.Parse(File.ReadAllText(_profilesPath))!.AsObject();
        Assert.Null(cleaned["rememberedUsername"]);
        Assert.Null(cleaned["rememberedProtectedKeyBase64"]);
        Assert.False(cleaned["rememberMe"]!.GetValue<bool>());

        // History is intact behind the password.
        using var again = new LocalProfileManager(_profilesPath, broker);
        Assert.False(again.IsLoggedIn);
        Assert.True(again.Login("emre", "pass123"));
        Assert.Equal(1, again.GetStats(video)?.ViewCount);
    }

    [Fact]
    public void Logout_ResetsRememberMeChoiceInViewModelAndManager()
    {
        using var broker = new StateSyncBroker(_scope);
        var manager = new LocalProfileManager(_profilesPath, broker);
        using var vm = new VideoPlatformViewModel(manager, ui: new ImmediateDispatcher());
        Assert.False(vm.RememberMe);

        Assert.True(vm.TryRegister("emre", "pass123", rememberMe: true));
        Assert.True(vm.RememberMe);
        vm.Logout();
        Assert.False(vm.RememberMe);
        Assert.False(manager.RememberMe);

        using var restarted = new LocalProfileManager(_profilesPath, broker);
        Assert.False(restarted.IsLoggedIn);
        manager.Dispose();
    }

    [Fact]
    public void Logout_BlocksRememberedSessionRestoreForTheRestOfTheSession()
    {
        var scope = "privacy-two-windows-" + Guid.NewGuid().ToString("N");
        using var brokerA = new StateSyncBroker(scope);
        using var brokerB = new StateSyncBroker(scope);
        using var windowA = new LocalProfileManager(_profilesPath, brokerA);
        using var windowB = new LocalProfileManager(_profilesPath, brokerB);

        Assert.True(windowA.Register("emre", "pass123", rememberMe: true));
        windowA.Logout();
        Assert.False(windowA.IsLoggedIn);

        // Another window logs in with remember-me: window A, which the user logged out, stays locked.
        Assert.True(windowB.Login("emre", "pass123", rememberMe: true));
        PublishProfilesEvent(brokerA, windowA, StateSyncEventKind.ProfileLogin, "emre");
        Assert.False(windowA.IsLoggedIn);
        Assert.Empty(windowA.GetFeed(VideoFeedFilter.History));

        // An explicit login in window A works as before.
        Assert.True(windowA.Login("emre", "pass123"));
        Assert.True(windowA.IsLoggedIn);
    }

    [Fact]
    public void Logout_WhenHeaderWriteFails_NeverRestoresAndPersistsClearedHeaderLater()
    {
        var video = Path.Combine(_tempDir, "secret.mp4");
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("emre", "pass123", rememberMe: true));
        manager.RecordWatch(video, positionSeconds: 3, durationSeconds: 30);
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));

        // Block the atomic replace of profiles.json so the logout header cannot be written.
        File.SetAttributes(_profilesPath, FileAttributes.ReadOnly);
        manager.Logout();
        Assert.False(manager.IsLoggedIn);
        Assert.Contains("rememberedProtectedKeyBase64", File.ReadAllText(_profilesPath), StringComparison.Ordinal);

        // A forced reload (broker event from another window) must not log back in from the stale key.
        PublishProfilesEvent(broker, manager, StateSyncEventKind.ListChanged, null);
        Assert.False(manager.IsLoggedIn);
        Assert.Null(manager.GetStats(video));

        // Once the file is writable again, the next reload writes the cleared header.
        File.SetAttributes(_profilesPath, FileAttributes.Normal);
        PublishProfilesEvent(broker, manager, StateSyncEventKind.ListChanged, null);
        Assert.False(manager.IsLoggedIn);
        Assert.DoesNotContain("rememberedProtectedKeyBase64", File.ReadAllText(_profilesPath), StringComparison.Ordinal);

        using var restarted = new LocalProfileManager(_profilesPath, broker);
        Assert.False(restarted.IsLoggedIn);
    }

    [Fact]
    public void BrokerWatchProgressAndThumbnailEvents_OfAnotherProfile_AreIgnored()
    {
        var watched = Path.Combine(_tempDir, "watched.mp4");
        var unseen = Path.Combine(_tempDir, "unseen.mp4");
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));
        manager.RecordWatch(watched, positionSeconds: 10, durationSeconds: 100);

        var otherThumb = Encoding.UTF8.GetBytes("BOB_THUMBNAIL");
        broker.Publish(MakeEvent(manager, StateSyncTopics.WatchProgress, StateSyncEventKind.ProgressUpdated, "bob", watched, position: 99, duration: 100));
        broker.Publish(MakeEvent(manager, StateSyncTopics.WatchProgress, StateSyncEventKind.ProgressUpdated, "bob", unseen, position: 50, duration: 100));
        broker.Publish(MakeEvent(manager, StateSyncTopics.Thumbnail, StateSyncEventKind.ThumbnailCaptured, "bob", unseen, payload: otherThumb));
        broker.Publish(MakeEvent(manager, StateSyncTopics.Thumbnail, StateSyncEventKind.ThumbnailCaptured, null, watched, payload: otherThumb));

        Assert.Equal(10, manager.GetStatsFastInMemory(watched)?.LastPositionSeconds);
        Assert.Null(manager.GetStatsFastInMemory(unseen));
        Assert.Null(manager.GetThumbnail(unseen));
        Assert.Null(manager.GetThumbnail(watched));

        // The same profile's events (case-insensitive username) still apply silently.
        var ownThumb = Encoding.UTF8.GetBytes("ALICE_THUMBNAIL");
        broker.Publish(MakeEvent(manager, StateSyncTopics.WatchProgress, StateSyncEventKind.ProgressUpdated, "ALICE", watched, position: 42, duration: 100));
        broker.Publish(MakeEvent(manager, StateSyncTopics.Thumbnail, StateSyncEventKind.ThumbnailCaptured, "Alice", watched, payload: ownThumb));
        Assert.Equal(42, manager.GetStatsFastInMemory(watched)?.LastPositionSeconds);
        Assert.Equal(ownThumb, manager.GetThumbnail(watched));
    }

    private static void PublishProfilesEvent(StateSyncBroker broker, LocalProfileManager target, StateSyncEventKind kind, string? username) =>
        broker.Publish(MakeEvent(target, StateSyncTopics.Profiles, kind, username, videoPath: null));

    private static StateSyncEvent MakeEvent(
        LocalProfileManager target,
        string topic,
        StateSyncEventKind kind,
        string? username,
        string? videoPath,
        double? position = null,
        double? duration = null,
        byte[]? payload = null) =>
        new()
        {
            Topic = topic,
            Kind = kind,
            SenderInstanceId = "other-window-" + Guid.NewGuid().ToString("N"),
            ScopePath = target.FilePath,
            Username = username,
            VideoPath = videoPath,
            PositionSeconds = position,
            DurationSeconds = duration,
            PayloadBase64 = payload is null ? null : Convert.ToBase64String(payload),
        };

    private sealed class ImmediateDispatcher : IUiDispatcher
    {
        public bool HasThreadAccess => true;
        public void Post(Action action) => action();
    }
}
