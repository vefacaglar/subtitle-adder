// End-to-end data for the Rust desktop app: the C# app's own LocalProfileManager, JsonFileSettingsStore,
// MediaLibraryIndex and TranslationProviderManager (linked from apps/windows) write a data folder exactly as the C#
// app does, so the Rust app is tested against realistic user data (UNIFIED.md §2, rule 4).
//
// Every run uses its own broker channel scope and only touches the folders it is given. Run it with TMP/TEMP
// pointing at a scratch folder (broker files, diagnostic log).

using SubtitleAdder.Core;

internal static class Program
{
    public const string User = "Emre";
    public const string Password = "e2e-parola-1";
    /// <summary>The C#-written provider (custom preset on an unreachable local address: never contacted).</summary>
    public const string ProviderName = "C# anahtarlı sağlayıcı";
    public const string ProviderKey = "sk-e2e-csharp-0123456789abcdef";

    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 3 && args[0] == "make") return Make(Path.GetFullPath(args[1]), Path.GetFullPath(args[2]));
            if (args.Length == 4 && args[0] == "dump") return Dump(Path.GetFullPath(args[1]), args[2], args[3]);
            Console.Error.WriteLine("usage: make <dataDir> <mediaDir> | dump <dataDir> <user> <password>");
            return 64;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine(ex);
            return 2;
        }
    }

    private static void Check(bool ok, string what)
    {
        if (!ok) throw new InvalidOperationException("E2E data step failed: " + what);
    }

    private static StateSyncBroker NewBroker() => new("e2e-data-" + Guid.NewGuid().ToString("N"));

    /// <summary>
    /// mediaDir holds the clips (*.mp4) and, for some of them, a picture <c>thumbs\&lt;stem&gt;.png</c> used as the
    /// hub thumbnail. The first clip (by name) is watched most.
    /// </summary>
    private static int Make(string dataDir, string mediaDir)
    {
        Directory.CreateDirectory(dataDir);
        var clips = Directory.GetFiles(mediaDir, "*.mp4").OrderBy(p => p, StringComparer.Ordinal).ToArray();
        Check(clips.Length >= 2, "at least two clips in " + mediaDir);

        using (var settings = new JsonFileSettingsStore(Path.Combine(dataDir, "settings.json")))
        {
            // Never register the test exe as the user's video player; small model, Turkish source, English target.
            settings.SetBool("fileAssociationsDisabled", true);
            settings.SetString("uiLanguage", "tr");
            settings.SetString("selectedLanguage", "tr");
            settings.SetString("selectedModel", "base");
            settings.SetString("translationModel", "madlad-3b");
            settings.SetString("translationTarget", "en");
            settings.SetBool("autoPlayVideo", false);
            // The sidebar starts hidden so screenshots never show the tester's own Videos folder.
            settings.SetBool("showFileExplorer", false);
            settings.SetString("appTheme", "light");
            settings.Flush();
        }

        using var broker = NewBroker();
        var index = new MediaLibraryIndex(Path.Combine(dataDir, "media-index.json"), broker);
        index.ScanDirectoryAsync(mediaDir).GetAwaiter().GetResult();

        using (var m = new LocalProfileManager(Path.Combine(dataDir, "profiles.json"), broker))
        {
            Check(m.Register(User, Password), "register " + User);
            var first = clips[0];
            m.RecordWatch(first, 2, 8);
            m.RecordWatch(first, 5, 8);
            m.UpdatePlaybackProgress(first, 6, 8);
            m.SetLiked(first, true);
            m.SetRating(first, 5);
            m.RecordWatch(clips[1], 1, 6);
            m.SetRating(clips[1], 3);
            foreach (var clip in clips)
            {
                var thumb = Path.Combine(mediaDir, "thumbs", Path.GetFileNameWithoutExtension(clip) + ".png");
                if (File.Exists(thumb)) Check(m.SetThumbnail(clip, File.ReadAllBytes(thumb)), "thumbnail " + clip);
            }
            var list = m.CreateCustomList("İzlenecekler") ?? throw new InvalidOperationException("list");
            Check(m.AddVideoToCustomList(list.Id, clips[1]), "list add");
            m.Logout();
            Check(m.FlushPendingWrites(TimeSpan.FromSeconds(10)), "flush");
        }

        var providers = new TranslationProviderManager(Path.Combine(dataDir, "translation-providers.json"), broker: broker);
        providers.AddProvider("custom", ProviderName, "https://127.0.0.1:9/v1", ProviderKey, "e2e-model", asyncTranslation: false, setActive: false);
        return 0;
    }

    private static int Dump(string dataDir, string user, string password)
    {
        using var broker = NewBroker();
        using var m = new LocalProfileManager(Path.Combine(dataDir, "profiles.json"), broker);
        Check(m.Login(user, password), "login " + user);
        Console.WriteLine($"user={m.CurrentUsername}");
        foreach (var v in m.GetFeed(VideoFeedFilter.History))
        {
            Console.WriteLine($"history|{Path.GetFileName(v.VideoPath)}|views={v.ViewCount}|liked={v.IsLiked}|rating={v.Rating}");
        }
        foreach (var l in m.GetCustomLists()) Console.WriteLine($"list|{l.Name}");
        m.Logout();
        return 0;
    }
}
