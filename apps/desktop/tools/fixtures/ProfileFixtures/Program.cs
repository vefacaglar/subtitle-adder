// Profile data fixtures for the Rust port: the C# app's own LocalProfileManager / SqliteProfileStore (linked from
// apps/windows) write and read profiles.json / profiles.db exactly as they do in production, so the Rust side can be
// checked in both directions (crates/sa-profiles/tests/win_fixtures.rs runs the same scenarios in Rust).
//
// Every run uses its own broker channel scope, never the default one, so it cannot reach a running app's broker, and
// only touches the directory it is given (run it with TMP/TEMP pointing at a scratch folder to keep the broker files
// and the diagnostic log out of the real temp folder).

using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using SubtitleAdder.Core;

internal static class Program
{
    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 2 && args[0] == "generate") return Generate(Path.GetFullPath(args[1]));
            if (args.Length == 2 && args[0] == "mutate") return Mutate(Path.GetFullPath(args[1]));
            if (args.Length == 2 && args[0] == "legacy") return Legacy(Path.GetFullPath(args[1]));
            if (args.Length == 2 && args[0] == "fingerprint") return PrintFingerprint(Path.GetFullPath(args[1]));
            if (args.Length >= 2 && args[0] == "dump" && args.Length % 2 == 0) return Dump(Path.GetFullPath(args[1]), args[2..]);
            if (args.Length == 1 && args[0] == "vectors") return Vectors();
            if (args.Length == 5 && args[0] == "seal-event") return SealEvent(Path.GetFullPath(args[1]), args[2], args[3], args[4]);
            if (args.Length == 6 && args[0] == "apply-event")
                return ApplyEvent(Path.GetFullPath(args[1]), args[2], args[3], Path.GetFullPath(args[4]), args[5]);
            Console.Error.WriteLine(
                "usage: generate <dir> | mutate <dir> | legacy <dir> | fingerprint <file> | dump <dir> [<user> <password>]... | " +
                "vectors | seal-event <dir> <user> <password> <videoPath> | apply-event <dir> <user> <password> <eventJsonFile> <videoPath>");
            return 64;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine(ex);
            return 2;
        }
    }

    /// <summary>Known media file names (in <c>&lt;dir&gt;\..\media</c>) shared with the Rust test.</summary>
    private static class Media
    {
        public const string A = "Yaz Tatili Ç.mp4"; // 5000 bytes
        public const string B = "missing_episode.mkv"; // no file (path-keyed row)
        public const string C = "belgesel-uzay.mp4"; // 70000 bytes
        public const string D = "zeynep_klip.mp4"; // 1234 bytes
    }

    private static void Check(bool ok, string what)
    {
        if (!ok) throw new InvalidOperationException("Fixture step failed: " + what);
    }

    /// <summary>Deterministic bytes (the same generator as the Rust test: xorshift32 seeded with <paramref name="seed"/>).</summary>
    private static byte[] Bytes(int length, uint seed)
    {
        var bytes = new byte[length];
        var x = seed == 0 ? 0x9E3779B9u : seed * 0x9E3779B9u;
        for (var i = 0; i < length; i++)
        {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            bytes[i] = (byte)x;
        }
        return bytes;
    }

    private static StateSyncBroker NewBroker() => new("profile-fixtures-" + Guid.NewGuid().ToString("N"));

    private static (string a, string b, string c, string d) MediaFiles(string dir, bool create)
    {
        // Media live next to the data folder, so a copied data folder still points at the same files.
        var media = Path.Combine(Path.GetDirectoryName(dir)!, "media");
        var a = Path.Combine(media, Media.A);
        var b = Path.Combine(media, Media.B);
        var c = Path.Combine(media, Media.C);
        var d = Path.Combine(media, Media.D);
        if (create)
        {
            Directory.CreateDirectory(media);
            File.WriteAllBytes(a, Bytes(5000, 1));
            File.WriteAllBytes(c, Bytes(70000, 2));
            File.WriteAllBytes(d, Bytes(1234, 3));
        }
        return (a, b, c, d);
    }

    private static int Generate(string dir)
    {
        Directory.CreateDirectory(dir);
        var (a, b, c, d) = MediaFiles(dir, create: true);

        using var broker = NewBroker();
        using (var m = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker))
        {
            Check(m.Register("Emre", "pässwörd-1"), "register Emre");
            m.RecordWatch(a, 10, 100);
            m.RecordWatch(a, 40, 100);
            m.UpdatePlaybackProgress(a, 55, 100);
            m.SetLiked(a, true);
            m.SetRating(a, 4);
            Check(m.SetThumbnail(a, Encoding.UTF8.GetBytes("THUMB-A-ÇĞ")), "thumbnail a");
            m.RecordWatch(b, 5, 50);
            m.SetLiked(c, true);
            var l1 = m.CreateCustomList("İzlenecekler") ?? throw new InvalidOperationException("list 1");
            Check(m.AddVideoToCustomList(l1.Id, a), "list 1 add a");
            Check(m.AddVideoToCustomList(l1.Id, b), "list 1 add b");
            var l2 = m.CreateCustomList("Belgeseller") ?? throw new InvalidOperationException("list 2");
            Check(m.AddVideoToCustomList(l2.Id, c), "list 2 add c");
            Check(m.SetThumbnail(c, Bytes(3000, 4)), "thumbnail c");
            m.Logout();

            Check(m.Register("zeynep", "z-pass", rememberMe: true), "register zeynep");
            m.RecordWatch(d, 12.5, 60);
            m.SetRating(d, 5);
            Check(m.FlushPendingWrites(TimeSpan.FromSeconds(10)), "flush");
        }
        return 0;
    }

    /// <summary>The second step on top of <see cref="Generate"/> data (written by either app).</summary>
    private static int Mutate(string dir)
    {
        var (a, b, c, d) = MediaFiles(dir, create: false);
        using var broker = NewBroker();
        using (var m = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker))
        {
            Check(m.Login("emre", "pässwörd-1"), "login emre");
            m.RecordWatch(a, 70, 100);
            Check(!m.ToggleLike(a), "unlike a");
            m.SetRating(c, 3);
            var lists = m.GetCustomLists();
            var l1 = lists.Single(l => l.Name == "İzlenecekler");
            var l2 = lists.Single(l => l.Name == "Belgeseller");
            Check(m.RemoveVideoFromCustomList(l1.Id, b), "list 1 remove b");
            Check(m.RenameCustomList(l2.Id, "Uzay Belgeselleri"), "rename list 2");
            Check(m.SetThumbnail(b, Bytes(700, 5)), "thumbnail b");
            var l3 = m.CreateCustomList("Yeni Liste") ?? throw new InvalidOperationException("list 3");
            Check(m.AddVideoToCustomList(l3.Id, d), "list 3 add d");
            Check(m.AddVideoToCustomList(l3.Id, a), "list 3 add a");
            m.Logout();

            Check(m.Login("ZEYNEP", "z-pass", rememberMe: true), "login zeynep");
            Check(m.ClearWatchHistory(), "clear history zeynep");
            m.SetLiked(c, true);
            Check(m.FlushPendingWrites(TimeSpan.FromSeconds(10)), "flush");
        }
        return 0;
    }

    private static byte[] Seal(byte[] plain, byte[] key)
    {
        var nonce = RandomNumberGenerator.GetBytes(12);
        var tag = new byte[16];
        var cipher = new byte[plain.Length];
        using (var gcm = new AesGcm(key, 16)) gcm.Encrypt(nonce, plain, cipher, tag);
        return [.. nonce, .. tag, .. cipher];
    }

    /// <summary>A version-2 header with legacy monolithic vaults (the format before profiles.db).</summary>
    private static int Legacy(string dir)
    {
        Directory.CreateDirectory(dir);
        var (a, b, c, _) = MediaFiles(dir, create: true);
        var fpA = LocalProfileManager.TryComputeFingerprintOnly(a) ?? throw new InvalidOperationException("fp a");
        var t0 = new DateTimeOffset(2025, 3, 4, 5, 6, 7, TimeSpan.Zero).AddTicks(1234567);
        var jsonOptions = new JsonSerializerOptions
        {
            WriteIndented = true,
            PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
            DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull,
        };

        JsonObject Profile(string user, string password, object vault)
        {
            var salt = RandomNumberGenerator.GetBytes(16);
            var derived = Rfc2898DeriveBytes.Pbkdf2(Encoding.UTF8.GetBytes(password), salt, 100_000, HashAlgorithmName.SHA256, 64);
            var vaultJson = JsonSerializer.SerializeToUtf8Bytes(vault, jsonOptions);
            return new JsonObject
            {
                ["username"] = user,
                ["saltBase64"] = Convert.ToBase64String(salt),
                ["passwordHashBase64"] = Convert.ToBase64String(derived[..32]),
                ["iterations"] = 100000,
                ["createdAtUtc"] = t0.ToString("O"),
                ["encryptedVaultBase64"] = Convert.ToBase64String(Seal(vaultJson, derived[32..])),
            };
        }

        var vaultEski = new
        {
            videos = new[]
            {
                new VideoUserStats
                {
                    ContentFingerprint = fpA, VideoPath = a, FileName = Path.GetFileName(a),
                    DirectoryPath = Path.GetDirectoryName(a)!, ViewCount = 3, IsLiked = true, Rating = 9,
                    LastPositionSeconds = 33.25, DurationSeconds = 100, LastWatchedAtUtc = t0.AddMinutes(5),
                    UpdatedAtUtc = t0.AddMinutes(6),
                },
                new VideoUserStats
                {
                    VideoPath = b, FileName = "", DirectoryPath = Path.GetDirectoryName(b)!, ViewCount = -2,
                    Rating = 2, UpdatedAtUtc = t0.AddMinutes(1),
                },
                new VideoUserStats
                {
                    VideoPath = c, FileName = Path.GetFileName(c), DirectoryPath = Path.GetDirectoryName(c)!,
                    ViewCount = 1, LastWatchedAtUtc = t0.AddMinutes(2), UpdatedAtUtc = t0.AddMinutes(2),
                },
            },
            customLists = new[]
            {
                new UserVideoList
                {
                    Id = "0123456789abcdef0123456789abcdef", Name = "  Eski Liste ", VideoPaths = [a, b, a.ToUpperInvariant(), ""],
                    CreatedAtUtc = t0, UpdatedAtUtc = t0.AddMinutes(3),
                },
            },
            thumbnails = new Dictionary<string, string>
            {
                [fpA] = Convert.ToBase64String(Bytes(900, 6)),
                [b] = Convert.ToBase64String(Encoding.UTF8.GetBytes("THUMB-B")),
                ["  "] = Convert.ToBase64String(Encoding.UTF8.GetBytes("ignored")),
            },
        };
        var vaultBos = new { videos = Array.Empty<object>(), customLists = Array.Empty<object>(), thumbnails = new Dictionary<string, string>() };

        var doc = new JsonObject
        {
            ["version"] = 2,
            // Pre-opt-in documents carried a pre-ticked session: it must be dropped once.
            ["rememberMe"] = true,
            ["lastUsername"] = "eski",
            ["rememberedUsername"] = "eski",
            ["rememberedProtectedKeyBase64"] = Convert.ToBase64String(RandomNumberGenerator.GetBytes(60)),
            ["profiles"] = new JsonArray(Profile("eski", "eski-şifre", vaultEski), Profile("bos", "bos-pw", vaultBos)),
        };
        File.WriteAllText(Path.Combine(dir, "profiles.json"), doc.ToJsonString(jsonOptions), new UTF8Encoding(false));
        return 0;
    }

    private static object? CallPrivate(string name, params object?[] args)
    {
        var method = typeof(LocalProfileManager).GetMethods(System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Public)
            .Single(m => m.Name == name && m.GetParameters().Length == args.Length);
        return method.Invoke(null, args);
    }

    /// <summary>Golden vectors computed by the C# primitives themselves (checked in on the Rust side).</summary>
    private static int Vectors()
    {
        var salt = Enumerable.Range(0, 16).Select(i => (byte)(i * 17 + 3)).ToArray();
        var (hash, key) = ((byte[], byte[]))CallPrivate("DeriveAuthHashAndVaultKey", "pässwörd-1 İı", salt, 100_000)!;
        var turkishPath = @"C:\Videolar\İzmir ılık Ödev ß.mp4";
        var root = new JsonObject
        {
            ["password"] = "pässwörd-1 İı",
            ["saltHex"] = Convert.ToHexStringLower(salt),
            ["hashBase64"] = Convert.ToBase64String(hash),
            ["keyHex"] = Convert.ToHexStringLower(key),
            ["turkishPath"] = turkishPath,
            ["storageVideoKey"] = LocalProfileManager.StorageVideoKey(key, "  " + turkishPath + " "),
            ["storageVideoKeyFp"] = LocalProfileManager.StorageVideoKey(key, "fp:5000:3ab99b16d24511552b0d2f52"),
            ["ownerTag"] = LocalProfileManager.StorageOwnerTag(key),
            ["listItemHash"] = (string)CallPrivate("ComputeListItemHash", "Emre", "0123456789ABCDEFabcdef0123456789", turkishPath, key)!,
            ["upperInvariant"] = "ıiİß ǅ ﬃ ΐ ſ µ ẞ ǰ".ToUpperInvariant(),
            // A blob sealed by the C# app (EncryptBytesToBlob of the vault marker; Span parameters cannot be reflected).
            ["csharpBlobBase64"] = (string)CallPrivate("EncryptVaultMarker", key)!,
            ["vaultMarkerPlainLength"] = Encoding.UTF8.GetByteCount("{\"storage\":\"sqlite-v3\"}"),
            ["pad300"] = LocalProfileManager.PadToBucket(new byte[300], 256, (byte)' ').Length,
            ["pad1025"] = LocalProfileManager.PadToBucket(new byte[1025], 1024, (byte)' ').Length,
        };
        Console.OutputEncoding = new UTF8Encoding(false);
        Console.Out.Write(root.ToJsonString(new JsonSerializerOptions { WriteIndented = true }));
        return 0;
    }

    /// <summary>Logs in and prints a watch-progress event (path 61.5 / 300) sealed by the C# app, as broker JSON.</summary>
    private static int SealEvent(string dir, string user, string password, string videoPath)
    {
        using var broker = NewBroker();
        using var m = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker);
        Check(m.Login(user, password), "login " + user);
        var evt = m.CreateBrokerEventForTests(StateSyncTopics.WatchProgress, StateSyncEventKind.ProgressUpdated, videoPath, 61.5, 300);
        Console.OutputEncoding = new UTF8Encoding(false);
        Console.Out.Write(JsonSerializer.Serialize(evt, StateSyncBroker.JsonOptions));
        return 0;
    }

    /// <summary>Logs in, delivers a broker event (JSON written by the Rust app) and prints the resulting position.</summary>
    private static int ApplyEvent(string dir, string user, string password, string eventFile, string videoPath)
    {
        using var broker = NewBroker();
        using var m = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker);
        Check(m.Login(user, password), "login " + user);
        var evt = JsonSerializer.Deserialize<StateSyncEvent>(File.ReadAllText(eventFile), StateSyncBroker.JsonOptions)
            ?? throw new InvalidOperationException("event json");
        broker.Publish(evt);
        var stats = m.GetStatsFastInMemory(videoPath);
        Console.Out.Write(stats is null ? "none" : stats.LastPositionSeconds.ToString(System.Globalization.CultureInfo.InvariantCulture));
        return 0;
    }

    private static int PrintFingerprint(string file)
    {
        Console.Out.Write(LocalProfileManager.TryComputeFingerprintOnly(file) ?? "");
        return 0;
    }

    private static int Dump(string dir, string[] credentials)
    {
        using var broker = NewBroker();
        using var m = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker);
        var root = new JsonObject
        {
            ["restoredUser"] = m.CurrentUsername,
            ["rememberMe"] = m.RememberMe,
            ["registered"] = new JsonArray(m.RegisteredUsernames.Select(u => (JsonNode?)u).ToArray()),
            ["lastUsername"] = m.LastUsername,
        };
        if (m.IsLoggedIn) root["restored"] = DumpProfile(m);
        var profiles = new JsonObject();
        for (var i = 0; i + 1 < credentials.Length; i += 2)
        {
            Check(m.Login(credentials[i], credentials[i + 1]), "login " + credentials[i]);
            profiles[credentials[i]] = DumpProfile(m);
        }
        root["profiles"] = profiles;
        Check(m.FlushPendingWrites(TimeSpan.FromSeconds(10)), "dump flush");
        Console.OutputEncoding = new UTF8Encoding(false);
        Console.Out.Write(root.ToJsonString(new JsonSerializerOptions { WriteIndented = true }));
        return 0;
    }

    private static JsonArray Paths(IEnumerable<VideoUserStats> items) =>
        new(items.Select(v => (JsonNode?)v.VideoPath).ToArray());

    private static JsonObject DumpProfile(LocalProfileManager m)
    {
        var all = m.GetFeed(VideoFeedFilter.All)
            .OrderBy(v => v.VideoPath.ToUpperInvariant(), StringComparer.Ordinal)
            .ToList();
        var videos = new JsonArray();
        foreach (var v in all)
        {
            var engaged = v.ViewCount > 0 || v.IsLiked || v.Rating > 0 || v.LastPositionSeconds > 0 || v.LastWatchedAtUtc is not null;
            var thumb = m.GetThumbnail(v.VideoPath);
            videos.Add(new JsonObject
            {
                ["videoPath"] = v.VideoPath,
                ["fileName"] = v.FileName,
                ["directoryPath"] = v.DirectoryPath,
                ["contentFingerprint"] = v.ContentFingerprint,
                ["viewCount"] = v.ViewCount,
                ["isLiked"] = v.IsLiked,
                ["rating"] = v.Rating,
                ["lastPositionSeconds"] = v.LastPositionSeconds,
                ["durationSeconds"] = v.DurationSeconds,
                ["lastWatchedTicks"] = v.LastWatchedAtUtc?.UtcTicks,
                // Rows created only from list items get "now" when loaded, so only engaged rows carry their time.
                ["updatedTicks"] = engaged ? v.UpdatedAtUtc.UtcTicks : null,
                ["thumbnailSha256"] = thumb is null ? null : Convert.ToHexStringLower(SHA256.HashData(thumb)),
            });
        }
        var lists = new JsonArray();
        // Native order: after a reload the lists come back in the DB's list_id order on both sides.
        foreach (var l in m.GetCustomLists())
        {
            lists.Add(new JsonObject
            {
                ["id"] = l.Id,
                ["name"] = l.Name,
                ["videoPaths"] = new JsonArray(l.VideoPaths.Select(p => (JsonNode?)p).ToArray()),
                ["createdTicks"] = l.CreatedAtUtc.UtcTicks,
                ["updatedTicks"] = l.UpdatedAtUtc.UtcTicks,
            });
        }
        return new JsonObject
        {
            ["username"] = m.CurrentUsername,
            ["videos"] = videos,
            ["history"] = Paths(m.GetFeed(VideoFeedFilter.History)),
            ["liked"] = Paths(m.GetFeed(VideoFeedFilter.Liked)),
            ["mostWatched"] = Paths(m.GetFeed(VideoFeedFilter.MostWatched)),
            ["topRated"] = Paths(m.GetFeed(VideoFeedFilter.TopRated)),
            ["lists"] = lists,
            ["totals"] = new JsonObject
            {
                ["watched"] = m.TotalWatchedVideosCount,
                ["views"] = m.TotalViewsCount,
                ["liked"] = m.TotalLikedVideosCount,
                ["rated"] = m.TotalRatedVideosCount,
            },
        };
    }
}
