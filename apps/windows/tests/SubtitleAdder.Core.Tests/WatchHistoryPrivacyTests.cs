using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace SubtitleAdder.Core.Tests;

/// <summary>
/// Without a logged-in profile there is no watch history at all: nothing recorded, nothing kept in memory after a
/// logout or switch, and nothing history-like at rest or on the wire that can be read without the profile's key.
/// A profile's own (encrypted) history survives a logout and comes back with the next login.
/// </summary>
public sealed class WatchHistoryPrivacyTests : IDisposable
{
    private const int ChunkSize = 64 * 1024;
    private static readonly DateTime FixedCacheTime = new(2001, 1, 1, 0, 0, 0, DateTimeKind.Utc);

    private readonly string _tempDir;
    private readonly string _profilesPath;
    private readonly string _cacheDir;
    private readonly string _scope = "watch-privacy-" + Guid.NewGuid().ToString("N");

    public WatchHistoryPrivacyTests()
    {
        _tempDir = Path.Combine(Path.GetTempPath(), "SubtitleAdderWatchPrivacy_" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tempDir);
        _profilesPath = Path.Combine(_tempDir, "profiles.json");
        _cacheDir = Path.Combine(_tempDir, "VideoCache");
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
        catch (UnauthorizedAccessException)
        {
        }
    }

    // ---- Encrypted video cache -------------------------------------------------------------------------------

    [Fact]
    public void LoggedOutPlayback_WritesNothingToTheVideoCache()
    {
        var (videoPath, raw) = CreateVideo("logged_out_secret.mp4");
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        var store = new EncryptedVideoCacheStore(_cacheDir, manager, 1L << 30);

        Assert.Null(store.TryBeginSession());
        PlayThrough(raw, store, videoPath);

        var identity = EncryptedVideoCacheStore.ComputeVideoIdentity(videoPath, raw.Length, ChunkSize);
        Assert.False(store.TryWriteChunk(identity, ChunkSize, 0, raw.AsSpan(0, ChunkSize)));
        Assert.Empty(Directory.Exists(_cacheDir)
            ? Directory.GetFileSystemEntries(_cacheDir, "*", SearchOption.AllDirectories)
            : []);
    }

    [Fact]
    public void LoggedInCache_UsesKeyedNamesAndFixedTimes_SoItIsNoPlaintextWatchRecord()
    {
        var (videoPath, raw) = CreateVideo("alice_episode.mp4");
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));
        var store = new EncryptedVideoCacheStore(_cacheDir, manager, 1L << 30);

        PlayThrough(raw, store, videoPath);
        // A second playback is served from the cache and must not refresh any file time.
        PlayThrough(raw, store, videoPath);

        var identity = EncryptedVideoCacheStore.ComputeVideoIdentity(videoPath, raw.Length, ChunkSize);
        var unkeyedProfileHash = ShortSha256("SubtitleAdder.VideoCache.Profile.v1:ALICE");
        var unkeyedVideoHash = ShortSha256("SubtitleAdder.VideoCache.Video.v1:" + identity);

        var files = Directory.GetFiles(_cacheDir, "*.vcache", SearchOption.AllDirectories);
        Assert.Equal(4, files.Length);
        foreach (var file in files)
        {
            var name = Path.GetFileNameWithoutExtension(file);
            Assert.DoesNotContain("chunk", name, StringComparison.OrdinalIgnoreCase);
            Assert.Matches("^[0-9a-f]{32}$", name);
            Assert.Equal(FixedCacheTime, File.GetLastWriteTimeUtc(file));
            Assert.Equal(FixedCacheTime, File.GetCreationTimeUtc(file));
        }
        foreach (var dir in Directory.GetDirectories(_cacheDir, "*", SearchOption.AllDirectories))
        {
            var name = Path.GetFileName(dir);
            Assert.NotEqual(unkeyedProfileHash, name);
            Assert.NotEqual(unkeyedVideoHash, name);
            if (!string.Equals(name, EncryptedVideoCacheStore.LayoutFolderName, StringComparison.OrdinalIgnoreCase))
            {
                Assert.Equal(FixedCacheTime, Directory.GetLastWriteTimeUtc(dir));
            }
        }

        // The cache still works for alice after a logout and a new login (it is not deleted).
        manager.Logout();
        Assert.True(manager.Login("alice", "pass123"));
        Assert.Equal(4, store.GetCachedChunkIndices(identity, 4, ChunkSize, raw.Length).Count);
    }

    [Fact]
    public void LoggedInCache_PadsEveryChunk_InOneFlatFolder_AndReadsKeepTheAccessTime()
    {
        // A length that is not a multiple of the chunk size: the last chunk must not reveal (length mod chunk size).
        var raw = new byte[3 * ChunkSize + 1234];
        for (var i = 0; i < raw.Length; i++) raw[i] = (byte)((i * 31 + 7) & 0xFF);
        var videoPath = Path.Combine(_tempDir, "odd_length.mkv");
        File.WriteAllBytes(videoPath, raw);

        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("bob", "pass456"));
        var store = new EncryptedVideoCacheStore(_cacheDir, manager, 1L << 30);
        PlayThrough(raw, store, videoPath);
        Assert.True(manager.Register("alice", "pass123"));
        PlayThrough(raw, store, videoPath);

        var layout = Path.Combine(_cacheDir, EncryptedVideoCacheStore.LayoutFolderName);
        // No per-profile or per-video folders whose count or file counts would tell who watched how much of what.
        Assert.Empty(Directory.GetDirectories(layout));
        var files = Directory.GetFiles(layout, "*.vcache");
        Assert.Equal(8, files.Length);
        Assert.All(files, f => Assert.Equal(EncryptedVideoCacheStore.HeaderSizeBytes + ChunkSize, new FileInfo(f).Length));

        // Served back exactly from the cache, and reading leaves the fixed access time in place.
        var identity = EncryptedVideoCacheStore.ComputeVideoIdentity(videoPath, raw.Length, ChunkSize);
        Assert.Equal(4, store.GetCachedChunkIndices(identity, 4, ChunkSize, raw.Length).Count);
        Assert.Equal(raw.AsSpan(3 * ChunkSize).ToArray(), store.TryReadChunk(identity, ChunkSize, 3, 1234));
        Assert.Equal(raw.AsSpan(0, ChunkSize).ToArray(), store.TryReadChunk(identity, ChunkSize, 0, ChunkSize));
        Assert.All(files, f => Assert.Equal(FixedCacheTime, File.GetLastAccessTimeUtc(f)));
    }

    [Fact]
    public async Task CacheReads_DoNotWaitBehindAWriteOfTheSameSession()
    {
        var (videoPath, raw) = CreateVideo("concurrent.mp4");
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));
        var store = new EncryptedVideoCacheStore(_cacheDir, manager, 1L << 30);
        var session = store.TryBeginSession();
        Assert.NotNull(session);
        var identity = EncryptedVideoCacheStore.ComputeVideoIdentity(videoPath, raw.Length, ChunkSize);
        Assert.True(store.TryWriteChunk(session, identity, ChunkSize, 0, raw.AsSpan(0, ChunkSize)));

        // Many writes and reads interleaved on one session must all succeed and return the right bytes.
        var writer = Task.Run(() =>
        {
            for (var i = 0; i < 20; i++) Assert.True(store.TryWriteChunk(session, identity, ChunkSize, 1 + (i % 3), raw.AsSpan((1 + (i % 3)) * ChunkSize, ChunkSize)));
        });
        for (var i = 0; i < 20; i++)
        {
            Assert.Equal(raw.AsSpan(0, ChunkSize).ToArray(), store.TryReadChunk(session, identity, ChunkSize, 0, ChunkSize));
        }
        await writer.WaitAsync(TimeSpan.FromSeconds(30));
        Assert.True(writer.IsCompletedSuccessfully);
    }

    [Fact]
    public void CacheSession_EndsWithLogout_SoChunksNeverLandInTheNextProfile()
    {
        var (videoPath, raw) = CreateVideo("switch.mp4");
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("bob", "pass456"));
        Assert.True(manager.Register("alice", "pass123"));
        var store = new EncryptedVideoCacheStore(_cacheDir, manager, 1L << 30);
        var identity = EncryptedVideoCacheStore.ComputeVideoIdentity(videoPath, raw.Length, ChunkSize);

        var aliceSession = store.TryBeginSession();
        Assert.NotNull(aliceSession);
        using var memory = new MemoryStream(raw, writable: false);
        using var stream = new BufferedVideoStream(
            memory, ownsStream: false, chunkSizeBytes: ChunkSize, maxCachedChunks: 2, readAheadChunks: 0,
            cacheStore: store, filePath: videoPath);

        manager.Logout();
        // The open stream's session is ended at logout: its copy of alice's key is zeroed, not kept until disposal.
        store.EndStaleSessions();
        Assert.True(aliceSession.IsEnded);
        Assert.True(manager.Login("bob", "pass456"));

        Assert.False(store.TryWriteChunk(aliceSession, identity, ChunkSize, 0, raw.AsSpan(0, ChunkSize)));
        Assert.Null(store.TryReadChunk(aliceSession, identity, ChunkSize, 0, ChunkSize));
        var buffer = new byte[raw.Length];
        var read = 0;
        while (read < raw.Length) read += stream.Read(buffer, read, raw.Length - read);
        Assert.Equal(raw, buffer);

        Assert.Empty(Directory.Exists(_cacheDir) ? Directory.GetFiles(_cacheDir, "*.vcache", SearchOption.AllDirectories) : []);
        Assert.Empty(store.GetCachedChunkIndices(identity, 4, ChunkSize, raw.Length));
    }

    [Fact]
    public void LegacyCacheLayout_IsPurged_CurrentLayoutIsKept()
    {
        var legacyChunk = Path.Combine(_cacheDir, "0123456789abcdef0123456789abcdef", "fedcba9876543210fedcba9876543210", "chunk_000007.vcache");
        Directory.CreateDirectory(Path.GetDirectoryName(legacyChunk)!);
        File.WriteAllBytes(legacyChunk, new byte[64]);
        var current = Path.Combine(_cacheDir, EncryptedVideoCacheStore.LayoutFolderName, "p", "v", "c.vcache");
        Directory.CreateDirectory(Path.GetDirectoryName(current)!);
        File.WriteAllBytes(current, new byte[64]);

        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        var store = new EncryptedVideoCacheStore(_cacheDir, manager, 1L << 30);
        store.PurgeLegacyLayout();

        Assert.False(File.Exists(legacyChunk));
        Assert.Single(Directory.GetDirectories(_cacheDir));
        Assert.True(File.Exists(current));
    }

    // ---- SQLite at rest ----------------------------------------------------------------------------------------

    [Fact]
    public void Sqlite_HoldsNoPlaintextPathsCountsOrTimestamps()
    {
        var token = "PrivateClip_" + Guid.NewGuid().ToString("N");
        var small = Path.Combine(_tempDir, token + ".mp4");
        File.WriteAllBytes(small, new byte[4096]);
        var missing = Path.Combine(_tempDir, token + "_large.mkv"); // no file: keyed by path

        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));
        manager.RecordWatch(small, 10, 100);
        manager.RecordWatch(small, 20, 100);
        manager.RecordWatch(missing, 5, 50);
        manager.UpdatePlaybackProgress(small, 33, 100);
        Assert.True(manager.SetThumbnail(missing, [1, 2, 3, 4]));
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));

        var rows = manager.SqlStore.DumpPlaintextVideoColumns();
        Assert.NotEmpty(rows);
        foreach (var row in rows)
        {
            Assert.StartsWith(SqliteProfileStore.HashedVideoKeyPrefix, row.Key, StringComparison.Ordinal);
            Assert.DoesNotContain(token, row.Key, StringComparison.OrdinalIgnoreCase);
            Assert.DoesNotContain("fp:", row.Key, StringComparison.Ordinal);
            Assert.Equal(0, row.UpdatedTicks);
            Assert.Equal(-1, row.ViewCount);
        }
        Assert.All(manager.SqlStore.DumpProcessedRequests(), r =>
        {
            Assert.Equal(0, r.Ticks);
            Assert.Equal("", r.Username);
        });

        // Rows are owned by a keyed tag, not the profile name, and path-carrying blobs are padded to one size.
        var owned = manager.SqlStore.DumpOwnersAndBlobLengths();
        Assert.NotEmpty(owned);
        Assert.All(owned, r =>
        {
            Assert.StartsWith(SqliteProfileStore.OwnerTagPrefix, r.Owner, StringComparison.Ordinal);
            Assert.DoesNotContain("alice", r.Owner, StringComparison.OrdinalIgnoreCase);
        });
        Assert.Single(owned.Where(r => r.Table == "videos").Select(r => r.BlobLength).Distinct());
        Assert.Single(owned.Where(r => r.Table == "watch_progress").Select(r => r.BlobLength).Distinct());
        // A committed thumbnail is read from the database, not kept in memory.
        Assert.Equal(0, manager.SqlStore.CachedThumbnailBlobCount);

        // Neither the database file nor its WAL holds the file name anywhere.
        var needle = Encoding.UTF8.GetBytes(token);
        foreach (var file in new[] { manager.SqliteDbPath, manager.SqliteDbPath + "-wal" })
        {
            if (!File.Exists(file)) continue;
            Assert.False(ReadShared(file).AsSpan().IndexOf(needle) >= 0, $"{Path.GetFileName(file)} contains a plaintext path");
        }

        // The encrypted history is intact behind the password.
        manager.Logout();
        Assert.Null(manager.GetStats(small));
        Assert.True(manager.Login("alice", "pass123"));
        Assert.Equal(2, manager.GetStats(small)?.ViewCount);
        Assert.Equal(33, manager.GetStats(small)?.LastPositionSeconds);
        Assert.Equal(1, manager.GetStats(missing)?.ViewCount);
    }

    [Fact]
    public void ClearWatchHistory_RemovesThumbnails_AndKeepsOnlyOwnerlessRequestIds()
    {
        var video = Path.Combine(_tempDir, "cleared.mp4");
        File.WriteAllBytes(video, new byte[2048]);
        var card = Path.Combine(_tempDir, "only_shown_as_card.mp4");

        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));
        manager.RecordWatch(video, 5, 50);
        for (var i = 1; i <= 20; i++) manager.UpdatePlaybackProgress(video, 5 + i, 50);
        Assert.True(manager.SetThumbnail(video, [9, 9, 9]));
        Assert.True(manager.SetThumbnail(card, [8, 8, 8]));
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));
        Assert.True(manager.SqlStore.DumpProcessedRequests().Count >= 20);

        Assert.True(manager.ClearWatchHistory());
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));

        var counts = manager.GetSqliteTableRowCounts();
        Assert.Equal(0, counts.Thumbnails);
        Assert.Equal(0, counts.Videos);
        Assert.Equal(0, counts.WatchProgress);
        // The idempotency ids survive the clear (so a resent pre-clear write stays a duplicate) but carry no owner
        // and no time, so they are no per-profile activity count.
        Assert.All(manager.SqlStore.DumpProcessedRequests(), r =>
        {
            Assert.Equal("", r.Username);
            Assert.Equal(0, r.Ticks);
        });
    }

    [Fact]
    public void ResentWrite_AfterClearHistory_IsStillADuplicate()
    {
        var dbPath = Path.Combine(_tempDir, "resend.db");
        using var store = new SqliteProfileStore(dbPath);
        const string owner = SqliteProfileStore.OwnerTagPrefix + "0123456789abcdef";
        var watch = new SqliteProfileStore.WriteItem(
            SqliteProfileStore.WriteKind.UpsertVideo, Username: owner, Key: "hk:1", Blob: [1, 2, 3], RequestId: "watch-1");
        store.EnqueueItem(watch);
        Assert.True(store.Flush(TimeSpan.FromSeconds(5)));
        Assert.Equal(1, store.GetTableRowCounts(owner).Videos);

        store.EnqueueItem(new SqliteProfileStore.WriteItem(
            SqliteProfileStore.WriteKind.ReplaceAllVideosAndProgress, Username: owner, RequestId: "clear-1"));
        Assert.True(store.Flush(TimeSpan.FromSeconds(5)));
        Assert.Equal(0, store.GetTableRowCounts(owner).Videos);

        // The pipe dropped before the watch's ack: the broker resends it with the same id after the clear.
        store.EnqueueItem(watch);
        Assert.True(store.Flush(TimeSpan.FromSeconds(5)));
        Assert.Equal(0, store.GetTableRowCounts(owner).Videos);
        Assert.True(store.TotalDuplicateRequestsIgnored >= 1);
    }

    [Fact]
    public void PrivacyVacuum_ThatFailsOnALockedDatabase_IsRetriedAtTheNextOpen()
    {
        var dbPath = Path.Combine(_tempDir, "vacuum.db");
        using var first = new SqliteProfileStore(dbPath);
        using var second = new SqliteProfileStore(dbPath);
        Assert.True(first.IsPrivacyVacuumDoneForTests());

        // As if the earlier VACUUM had failed, then another instance holds the write lock.
        Assert.True(first.ExecuteRawForTests("DELETE FROM meta WHERE key = 'privacy_vacuum';"));
        Assert.True(second.ExecuteRawForTests("BEGIN IMMEDIATE;"));
        first.RunStartupPrivacyStepsForTests();
        Assert.False(first.IsPrivacyVacuumDoneForTests());

        Assert.True(second.ExecuteRawForTests("ROLLBACK;"));
        first.RunStartupPrivacyStepsForTests();
        Assert.True(first.IsPrivacyVacuumDoneForTests());
    }

    [Fact]
    public void RowsOwnedByAPlainProfileName_AreRetaggedAtLogin()
    {
        var video = Path.Combine(_tempDir, "legacy_owner.mp4");
        File.WriteAllBytes(video, new byte[2048]);
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));
        manager.RecordWatch(video, 10, 100);
        Assert.True(manager.SetThumbnail(video, [5, 6, 7]));
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));
        manager.Logout();

        // Earlier versions stored the profile name as the owner of every row.
        Assert.True(manager.SqlStore.ExecuteRawForTests("""
            UPDATE videos SET username = 'alice';
            UPDATE watch_progress SET username = 'alice';
            UPDATE thumbnails SET username = 'alice';
            """));

        Assert.True(manager.Login("alice", "pass123"));
        Assert.Equal(1, manager.GetStats(video)?.ViewCount);
        Assert.Equal(10, manager.GetStats(video)?.LastPositionSeconds);
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));
        var owned = manager.SqlStore.DumpOwnersAndBlobLengths();
        Assert.NotEmpty(owned);
        Assert.All(owned, r => Assert.StartsWith(SqliteProfileStore.OwnerTagPrefix, r.Owner, StringComparison.Ordinal));
        Assert.Equal(1, manager.GetSqliteTableRowCounts().Videos);
        Assert.Equal(1, manager.GetSqliteTableRowCounts().Thumbnails);
    }

    [Fact]
    public void LegacyPlaintextColumns_AreScrubbedWithoutAKey_AndRekeyedAtNextLogin()
    {
        var token = "LegacyClip_" + Guid.NewGuid().ToString("N");
        var video = Path.Combine(_tempDir, token + ".mp4");
        File.WriteAllBytes(video, new byte[4096]);

        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));
        manager.RecordWatch(video, 10, 100);
        manager.RecordWatch(video, 12, 100);
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));
        manager.Logout();

        // Rewrite the rows the way earlier versions stored them: plaintext path key, view count and timestamps.
        var escaped = video.Replace("'", "''", StringComparison.Ordinal);
        Assert.True(manager.SqlStore.ExecuteRawForTests($"""
            UPDATE videos SET video_key = '{escaped}', view_count = 2, updated_at_ticks = 638000000000000000;
            UPDATE watch_progress SET video_key = '{escaped}', updated_at_ticks = 638000000000000000;
            INSERT INTO processed_requests (request_id, username, processed_at_ticks) VALUES ('legacy', 'alice', 638000000000000000);
            """));
        manager.SqlStore.RerunPrivacyMigrationForTests();

        foreach (var row in manager.SqlStore.DumpPlaintextVideoColumns())
        {
            Assert.StartsWith(SqliteProfileStore.OpaqueLegacyVideoKeyPrefix, row.Key, StringComparison.Ordinal);
            Assert.DoesNotContain(token, row.Key, StringComparison.OrdinalIgnoreCase);
            Assert.Equal(0, row.UpdatedTicks);
            Assert.Equal(-1, row.ViewCount);
        }
        Assert.Empty(manager.SqlStore.DumpProcessedRequests());
        Assert.True(manager.SqlStore.IsPrivacyVacuumDoneForTests());

        // The next login reads the identity from the encrypted blob and stores keyed hashes again.
        Assert.True(manager.Login("alice", "pass123"));
        Assert.Equal(2, manager.GetStats(video)?.ViewCount);
        Assert.Equal(12, manager.GetStats(video)?.LastPositionSeconds);
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));
        var rekeyed = manager.SqlStore.DumpPlaintextVideoColumns();
        Assert.NotEmpty(rekeyed);
        Assert.All(rekeyed, r => Assert.StartsWith(SqliteProfileStore.HashedVideoKeyPrefix, r.Key, StringComparison.Ordinal));
    }

    // ---- Memory after logout / switch ---------------------------------------------------------------------------

    [Fact]
    public void LogoutInAnotherWindow_DropsEveryDecryptedCopyAndKey()
    {
        var video = Path.Combine(_tempDir, "two_windows.mp4");
        using var broker = new StateSyncBroker(_scope);
        using var windowA = new LocalProfileManager(_profilesPath, broker);
        using var windowB = new LocalProfileManager(_profilesPath, broker);
        Assert.True(windowA.Register("alice", "pass123"));
        Assert.True(windowB.Login("alice", "pass123"));
        windowB.RecordWatch(video, 10, 100);
        Assert.True(windowB.SetThumbnail(video, [1, 2, 3]));
        Assert.True(windowB.FlushPendingWrites(TimeSpan.FromSeconds(5)));
        Assert.True(windowB.DecryptedItemCountForTests > 0);

        windowA.Logout();

        Assert.False(windowB.IsLoggedIn);
        Assert.Equal(0, windowB.UnlockedProfileCountForTests);
        Assert.Equal(0, windowB.DecryptedItemCountForTests);
        Assert.Equal(0, windowA.DecryptedItemCountForTests);
        Assert.Null(windowB.GetStats(video));
        Assert.Equal(0, windowB.SqlStore.CachedThumbnailBlobCount);
    }

    [Fact]
    public void ProfileSwitch_EndsThePreviousSession_AndLogoutForgetsWatchedPaths()
    {
        var video = Path.Combine(_tempDir, "fingerprinted.mp4");
        File.WriteAllBytes(video, new byte[8192]);
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("bob", "pass456"));
        Assert.True(manager.Register("alice", "pass123"));
        manager.RecordWatch(video, 10, 100);
        Assert.True(LocalProfileManager.IsFingerprintCachedForTests(video));
        Assert.True(manager.FlushPendingWrites(TimeSpan.FromSeconds(5)));

        Assert.True(manager.Login("bob", "pass456"));
        Assert.Equal(1, manager.UnlockedProfileCountForTests);
        Assert.Equal(0, manager.DecryptedItemCountForTests);
        Assert.Null(manager.GetStats(video));

        manager.Logout();
        Assert.Equal(0, manager.UnlockedProfileCountForTests);
        Assert.False(LocalProfileManager.IsFingerprintCachedForTests(video));

        // Alice's encrypted history is still there for her next login.
        Assert.True(manager.Login("alice", "pass123"));
        Assert.Equal(1, manager.GetStats(video)?.ViewCount);
    }

    [Fact]
    public void ActiveVideo_IsForgottenOnLogout_AndDeferredWorkNeverLandsInTheNextProfile()
    {
        var x = Path.Combine(_tempDir, "opened_by_alice.mp4");
        var y = Path.Combine(_tempDir, "debounced.mp4");
        using var broker = new StateSyncBroker(_scope);
        var manager = new LocalProfileManager(_profilesPath, broker);
        using var vm = new VideoPlatformViewModel(manager, ui: new ImmediateDispatcher());

        // Logged out: the opened video is not even kept in memory.
        vm.SetActiveVideoImmediate(x);
        Assert.Null(vm.ActiveVideoPath);

        Assert.True(manager.Register("bob", "pass456"));
        Assert.True(vm.TryRegister("alice", "pass123"));
        vm.SetActiveVideoImmediate(x);
        Assert.NotNull(vm.ActiveVideoPath);
        var aliceToken = vm.SessionToken;

        vm.Logout();
        Assert.Null(vm.ActiveVideoPath);
        Assert.True(vm.TryLogin("bob", "pass456"));

        // Playback keeps ticking after the logout: nothing is written into bob's profile.
        vm.UpdateActiveVideoProgress(30, 100);
        vm.RecordWatchOnly(y, aliceToken);
        Assert.False(vm.SaveEncryptedThumbnail(x, [1, 2, 3], aliceToken));
        Assert.Null(manager.UpdatePlaybackProgress(x, 31, 100, aliceToken));
        Assert.Null(manager.GetStats(x));
        Assert.Null(manager.GetStats(y));

        // Work captured in bob's own session still applies.
        vm.RecordWatchOnly(y, vm.SessionToken);
        Assert.Equal(1, manager.GetStats(y)?.ViewCount);
        manager.Dispose();
    }

    [Fact]
    public void LoginWhileAVideoIsOpen_EnablesLikesForIt_ButRecordsNoWatchOrProgress()
    {
        var open = Path.Combine(_tempDir, "open_before_login.mp4");
        using var broker = new StateSyncBroker(_scope);
        var manager = new LocalProfileManager(_profilesPath, broker);
        using var vm = new VideoPlatformViewModel(manager, ui: new ImmediateDispatcher());
        vm.OpenVideoProvider = () => open;

        vm.SetActiveVideoImmediate(open);
        Assert.Null(vm.ActiveVideoPath);

        Assert.True(vm.TryRegister("alice", "pass123"));
        Assert.Equal(LocalProfileManager.NormalizeVideoPath(open), vm.ActiveVideoPath);
        vm.UpdateActiveVideoProgress(42, 100);
        Assert.Null(manager.GetStats(open));

        vm.ToggleActiveVideoLike();
        Assert.True(manager.GetStats(open)?.IsLiked);
        Assert.Equal(0, manager.GetStats(open)?.ViewCount);
        Assert.Equal(0, manager.GetStats(open)?.LastPositionSeconds);

        vm.Logout();
        Assert.Null(vm.ActiveVideoPath);
        manager.Dispose();
    }

    // ---- IPC ----------------------------------------------------------------------------------------------------

    [Fact]
    public void BrokerEvents_CarryNoPlaintextPathPositionOrUsername()
    {
        var token = "PipeClip_" + Guid.NewGuid().ToString("N");
        var video = Path.Combine(_tempDir, token + ".mp4");
        using var broker = new StateSyncBroker(_scope);
        using var manager = new LocalProfileManager(_profilesPath, broker);
        Assert.True(manager.Register("alice", "pass123"));

        var events = new[]
        {
            manager.CreateBrokerEventForTests(StateSyncTopics.WatchProgress, StateSyncEventKind.ProgressUpdated, video, 1234.5, 5000),
            manager.CreateBrokerEventForTests(StateSyncTopics.Thumbnail, StateSyncEventKind.ThumbnailCaptured, video, payloadBase64: Convert.ToBase64String(Encoding.UTF8.GetBytes(token))),
            manager.CreateBrokerEventForTests(StateSyncTopics.Profiles, StateSyncEventKind.VideoWatched, video),
            manager.CreateBrokerEventForTests(StateSyncTopics.Profiles, StateSyncEventKind.ProfileLogin),
        };
        foreach (var evt in events)
        {
            Assert.Null(evt.VideoPath);
            Assert.Null(evt.Username);
            Assert.Null(evt.PositionSeconds);
            Assert.Null(evt.DurationSeconds);
            Assert.Null(evt.ViewCount);
            Assert.Null(evt.PayloadBase64);
            var json = JsonSerializer.Serialize(evt);
            Assert.DoesNotContain(token, json, StringComparison.OrdinalIgnoreCase);
            Assert.DoesNotContain("alice", json, StringComparison.OrdinalIgnoreCase);
            Assert.DoesNotContain("1234.5", json, StringComparison.Ordinal);
        }
        Assert.NotNull(events[0].SealedPayloadBase64);
        Assert.NotNull(events[1].SealedPayloadBase64);
    }

    // ---- Plaintext files ------------------------------------------------------------------------------------------

    [Fact]
    public void RememberMe_IsWrappedWithDpapi_NotWithUserAndMachineNames()
    {
        using var broker = new StateSyncBroker(_scope);
        using (var manager = new LocalProfileManager(_profilesPath, broker))
        {
            Assert.True(manager.Register("alice", "pass123", rememberMe: true));
        }
        var header = JsonNode.Parse(File.ReadAllText(_profilesPath))!;
        Assert.StartsWith("dpapi1:", header["rememberedProtectedKeyBase64"]!.GetValue<string>(), StringComparison.Ordinal);

        using var restarted = new LocalProfileManager(_profilesPath, broker);
        Assert.True(restarted.IsLoggedIn);
    }

    [Theory]
    [InlineData(@"MediaFailed path='C:\Videos\Secret\a.mp4': boom", "MediaFailed path='<path>': boom")]
    [InlineData(@"open D:/Films/Private/clip.mkv failed", "open <path>")]
    [InlineData(@"share \\nas\media\x.mp4", "share <path>")]
    [InlineData("uri file:///C:/x/y.mp4", "uri <path>")]
    [InlineData("jobId=3 models=[small]", "jobId=3 models=[small]")]
    public void DiagnosticLog_RedactsPaths(string message, string expected) =>
        Assert.Equal(expected, DiagnosticLog.Redact(message));

    [Fact]
    public async Task Explorer_DoesNotPersistFoldersOfOpenedVideos_AndForgetsBrowsingOnSessionChange()
    {
        var privateDir = Path.Combine(_tempDir, "Private");
        Directory.CreateDirectory(privateDir);
        var clip = Path.Combine(privateDir, "clip_" + Guid.NewGuid().ToString("N") + ".mp4");
        File.WriteAllBytes(clip, new byte[512]);
        var indexPath = Path.Combine(_tempDir, "media-index.json");

        var settings = new InMemorySettingsStore();
        settings.SetString(FileExplorerViewModel.LastFolderDefaultsKey, privateDir);
        var index = new MediaLibraryIndex(indexPath);
        var vm = new FileExplorerViewModel(index, settings, ImmediateDispatcher.Instance);

        // The legacy plaintext value is removed and not used as the starting folder.
        Assert.Null(settings.GetString(FileExplorerViewModel.LastFolderDefaultsKey));
        Assert.NotEqual(MediaLibraryIndex.NormalizeDirectoryPath(privateDir), vm.CurrentFolder);

        await vm.SyncToVideoPathAsync(clip);
        Assert.Equal(MediaLibraryIndex.NormalizeDirectoryPath(privateDir), vm.CurrentFolder);
        Assert.Contains(vm.Files, f => string.Equals(f.FullPath, clip, StringComparison.OrdinalIgnoreCase));
        Assert.False(index.IsDirectoryIndexed(privateDir));
        Assert.True(!File.Exists(indexPath) || !File.ReadAllText(indexPath).Contains(Path.GetFileName(clip), StringComparison.OrdinalIgnoreCase));
        Assert.Null(settings.GetString(FileExplorerViewModel.LastFolderDefaultsKey));

        await vm.NavigateToAsync(_tempDir, crawlSubfolders: false);
        Assert.True(vm.CanGoBack);
        // Going back to the opened video's folder shows it again, still without persisting it.
        await vm.GoBackAsync();
        Assert.Equal(MediaLibraryIndex.NormalizeDirectoryPath(privateDir), vm.CurrentFolder);
        Assert.False(index.IsDirectoryIndexed(privateDir));
        Assert.True(!File.Exists(indexPath) || !File.ReadAllText(indexPath).Contains(Path.GetFileName(clip), StringComparison.OrdinalIgnoreCase));
        await vm.NavigateToAsync(_tempDir, crawlSubfolders: false);
        await vm.ResetForSessionChangeAsync();
        Assert.False(vm.CanGoBack);
        Assert.Equal("", vm.SearchQuery);
        Assert.NotEqual(MediaLibraryIndex.NormalizeDirectoryPath(privateDir), vm.CurrentFolder);
    }

    [Fact]
    public async Task OpeningAVideoInAnIndexedFolder_NeverRewritesTheIndexFile()
    {
        var browsed = Path.Combine(_tempDir, "Browsed");
        Directory.CreateDirectory(browsed);
        var clip = Path.Combine(browsed, "episode.mp4");
        File.WriteAllBytes(clip, new byte[512]);
        var indexPath = Path.Combine(_tempDir, "media-index-indexed.json");

        var index = new MediaLibraryIndex(indexPath);
        var vm = new FileExplorerViewModel(index, new InMemorySettingsStore(), ImmediateDispatcher.Instance);
        await vm.NavigateToAsync(browsed, crawlSubfolders: false);
        Assert.True(index.IsDirectoryIndexed(browsed));
        var before = File.ReadAllText(indexPath);
        // No scan or update time is recorded anywhere in the file.
        Assert.DoesNotContain("scannedAtUtc", before, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("updatedAtUtc", before, StringComparison.OrdinalIgnoreCase);
        var stamp = new DateTime(2001, 1, 1, 0, 0, 0, DateTimeKind.Utc);
        File.SetLastWriteTimeUtc(indexPath, stamp);

        // Open a video from the browsed folder (current folder) and from it while elsewhere: no write either way.
        await vm.SyncToVideoPathAsync(clip);
        await vm.NavigateToAsync(_tempDir, crawlSubfolders: false);
        File.SetLastWriteTimeUtc(indexPath, stamp);
        var afterBrowse = File.ReadAllText(indexPath);
        await vm.SyncToVideoPathAsync(clip);
        Assert.Equal(stamp, File.GetLastWriteTimeUtc(indexPath));
        Assert.Equal(afterBrowse, File.ReadAllText(indexPath));

        // A sidecar written for the opened video still shows up, in memory.
        File.WriteAllText(Path.Combine(browsed, "episode.srt"), "1\n00:00:01,000 --> 00:00:02,000\nx\n");
        await vm.SyncToVideoPathAsync(clip);
        Assert.True(index.GetCachedDirectory(browsed).Single().HasSidecarSubtitle);
        Assert.Equal(stamp, File.GetLastWriteTimeUtc(indexPath));
    }

    [Fact]
    public void LegacyMediaIndex_IsDiscardedOnce()
    {
        var indexPath = Path.Combine(_tempDir, "media-index-legacy.json");
        File.WriteAllText(indexPath, """
            {
              "version": 1,
              "updatedAtUtc": "2025-05-05T10:00:00+00:00",
              "directories": [
                {
                  "directoryPath": "D:\\Private",
                  "subdirectories": [],
                  "mediaFiles": [
                    { "fullPath": "D:\\Private\\a.mp4", "fileName": "a.mp4", "directoryPath": "D:\\Private", "sizeBytes": 1,
                      "lastWriteTimeUtc": "2025-05-05T10:00:00+00:00", "hasSidecarSubtitle": false, "sidecarExtension": null,
                      "indexedAtUtc": "2025-05-05T10:00:00+00:00" }
                  ],
                  "scannedAtUtc": "2025-05-05T10:00:00+00:00"
                }
              ]
            }
            """);

        var index = new MediaLibraryIndex(indexPath);
        Assert.Equal(0, index.TotalIndexedFiles);
        var text = File.ReadAllText(indexPath);
        Assert.DoesNotContain("Private", text, StringComparison.Ordinal);
        Assert.DoesNotContain("2025-05-05", text, StringComparison.Ordinal);
        Assert.Equal(0, new MediaLibraryIndex(indexPath).TotalIndexedFiles);
    }

    // ---- Helpers ----------------------------------------------------------------------------------------------------

    private (string Path, byte[] Raw) CreateVideo(string name)
    {
        var raw = new byte[4 * ChunkSize];
        for (var i = 0; i < raw.Length; i++) raw[i] = (byte)((i * 73 + 19) & 0xFF);
        var path = Path.Combine(_tempDir, name);
        File.WriteAllBytes(path, raw);
        return (path, raw);
    }

    private static void PlayThrough(byte[] raw, EncryptedVideoCacheStore store, string videoPath)
    {
        using var memory = new MemoryStream(raw, writable: false);
        using var stream = new BufferedVideoStream(
            memory, ownsStream: false, chunkSizeBytes: ChunkSize, maxCachedChunks: 2, readAheadChunks: 0,
            cacheStore: store, filePath: videoPath);
        var buffer = new byte[raw.Length];
        var read = 0;
        while (read < raw.Length)
        {
            var n = stream.Read(buffer, read, Math.Min(16 * 1024, raw.Length - read));
            Assert.True(n > 0);
            read += n;
        }
        Assert.Equal(raw, buffer);
    }

    private static string ShortSha256(string input) =>
        Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(input)))[..32];

    private static byte[] ReadShared(string path)
    {
        using var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        using var ms = new MemoryStream();
        fs.CopyTo(ms);
        return ms.ToArray();
    }
}
