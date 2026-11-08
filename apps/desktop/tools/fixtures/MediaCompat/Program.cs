// Drives the C# app's own media index and video cache for the sa-media compatibility tests (see MediaCompat.csproj).
using System.Security.Cryptography;
using SubtitleAdder.Core;

static byte[] Pattern(int length)
{
    var raw = new byte[length];
    for (var i = 0; i < raw.Length; i++) raw[i] = (byte)((i * 73 + 19) & 0xFF);
    return raw;
}

static string Hex(byte[] b) => Convert.ToHexStringLower(b);

static StateSyncBroker NewBroker() => new("media-compat-" + Guid.NewGuid().ToString("N"));

Console.OutputEncoding = new System.Text.UTF8Encoding(false);
var cmd = args.Length > 0 ? args[0] : "";
switch (cmd)
{
    case "index-write":
    {
        var dir = Path.GetFullPath(args[1]);
        var tree = Path.Combine(dir, "tree");
        Directory.CreateDirectory(Path.Combine(tree, "Season 1", "Extras"));
        Directory.CreateDirectory(Path.Combine(tree, "Vidéo şarkı"));
        File.WriteAllBytes(Path.Combine(tree, "intro.mp4"), new byte[2048]);
        File.WriteAllText(Path.Combine(tree, "intro.srt"), "1\n00:00:01,000 --> 00:00:02,000\nx\n");
        File.WriteAllBytes(Path.Combine(tree, "a+b'(c)é.mkv"), new byte[10]);
        File.WriteAllBytes(Path.Combine(tree, "Season 1", "s01e01.mkv"), new byte[4096]);
        File.WriteAllText(Path.Combine(tree, "Season 1", "s01e01.vtt"), "WEBVTT\n");
        File.WriteAllBytes(Path.Combine(tree, "Season 1", "Extras", "behind.mov"), new byte[7]);
        File.WriteAllBytes(Path.Combine(tree, "Vidéo şarkı", "Işık.mp3"), new byte[3]);
        var stamp = new DateTime(2026, 11, 1, 8, 12, 44, DateTimeKind.Utc).AddTicks(1234567);
        foreach (var f in Directory.EnumerateFiles(tree, "*", SearchOption.AllDirectories)) File.SetLastWriteTimeUtc(f, stamp);
        using var broker = NewBroker();
        var index = new MediaLibraryIndex(Path.Combine(dir, "media-index.json"), broker);
        await index.ScanDirectoryAsync(tree);
        await index.CrawlSubtreeAsync(tree, maxDepth: 2);
        Console.WriteLine($"{index.TotalIndexedFiles}|{index.IndexedDirectories.Count}");
        break;
    }
    case "index-read":
    {
        using var broker = NewBroker();
        var index = new MediaLibraryIndex(Path.GetFullPath(args[1]), broker);
        Console.WriteLine($"{index.TotalIndexedFiles}|{index.IndexedDirectories.Count}");
        foreach (var f in index.GetCachedDirectory(args[2]))
            Console.WriteLine($"{f.FileName}|{f.SizeBytes}|{f.SidecarExtension ?? "null"}|{f.LastWriteTimeUtc.UtcTicks}");
        foreach (var s in index.GetCachedSubdirectories(args[2])) Console.WriteLine($"dir|{s}");
        break;
    }
    case "index-dump":
    {
        // Every folder (OrdinalIgnoreCase order) with its files and subfolders, then the empty-query search order.
        using var broker = NewBroker();
        var index = new MediaLibraryIndex(Path.GetFullPath(args[1]), broker);
        Console.WriteLine($"total|{index.TotalIndexedFiles}");
        foreach (var d in index.IndexedDirectories)
        {
            Console.WriteLine($"dir|{d}");
            foreach (var s in index.GetCachedSubdirectories(d)) Console.WriteLine($"sub|{s}");
            foreach (var f in index.GetCachedDirectory(d))
                Console.WriteLine($"file|{f.FullPath}|{f.FileName}|{f.DirectoryPath}|{f.SizeBytes}|{f.LastWriteTimeUtc.UtcTicks}|{f.HasSidecarSubtitle}|{f.SidecarExtension ?? "null"}|{f.IndexedAtUtc.UtcTicks}");
        }
        foreach (var f in index.Search(args.Length > 2 ? args[2] : "", 500)) Console.WriteLine($"search|{f.FullPath}");
        break;
    }
    case "script":
    {
        // Runs a media-index script (one `op|arg|…` per line) against <dir>\tree and <dir>\media-index.json with the C#
        // MediaLibraryIndex; csharp_compat.rs runs the same script with the Rust port and compares output and file bytes.
        //   reset                      empty tree, no index file, new index
        //   mkdir|rel  write|rel|len[|ticks]  delete|rel  stamp|rel|ticks   (files get ScriptDefaultTicks unless given)
        //   scan|rel  refresh|rel  transient|rel  crawl|rel|depth  reload
        //   dump|query                 total|n, then search|fullPath of Search(query, 500)
        //   dirs                       dir|path per IndexedDirectories
        var dir = Path.GetFullPath(args[1]);
        var tree = Path.Combine(dir, "tree");
        var indexFile = Path.Combine(dir, "media-index.json");
        using var broker = NewBroker();
        MediaLibraryIndex? index = null;
        string P(string rel) => rel == "." ? tree : Path.Combine(tree, rel.Replace('/', Path.DirectorySeparatorChar));
        void Stamp(string path, long ticks) => File.SetLastWriteTimeUtc(path, new DateTime(ticks, DateTimeKind.Utc));
        const long ScriptDefaultTicks = 638_000_000_000_000_000;
        foreach (var raw in File.ReadAllLines(args[2], new System.Text.UTF8Encoding(false)))
        {
            var line = raw.Trim();
            if (line.Length == 0 || line.StartsWith('#')) continue;
            var p = line.Split('|');
            switch (p[0])
            {
                case "reset":
                    if (Directory.Exists(tree)) Directory.Delete(tree, recursive: true);
                    Directory.CreateDirectory(tree);
                    if (File.Exists(indexFile)) File.Delete(indexFile);
                    index = new MediaLibraryIndex(indexFile, broker);
                    break;
                case "mkdir": Directory.CreateDirectory(P(p[1])); break;
                case "write":
                    File.WriteAllBytes(P(p[1]), new byte[int.Parse(p[2])]);
                    Stamp(P(p[1]), p.Length > 3 ? long.Parse(p[3]) : ScriptDefaultTicks);
                    break;
                case "delete":
                    if (Directory.Exists(P(p[1]))) Directory.Delete(P(p[1]), recursive: true); else File.Delete(P(p[1]));
                    break;
                case "stamp": Stamp(P(p[1]), long.Parse(p[2])); break;
                case "scan": index!.ScanDirectoryAsync(P(p[1])).GetAwaiter().GetResult(); break;
                case "refresh": index!.RefreshDirectoryInMemoryAsync(P(p[1])).GetAwaiter().GetResult(); break;
                case "transient": index!.ScanDirectoryTransientAsync(P(p[1])).GetAwaiter().GetResult(); break;
                case "crawl": index!.CrawlSubtreeAsync(P(p[1]), int.Parse(p[2])).GetAwaiter().GetResult(); break;
                case "reload": index = new MediaLibraryIndex(indexFile, broker); break;
                case "dump":
                    Console.WriteLine($"total|{index!.TotalIndexedFiles}");
                    foreach (var f in index.Search(p.Length > 1 ? p[1] : "", 500)) Console.WriteLine($"search|{f.FullPath}");
                    break;
                case "dirs":
                    foreach (var d in index!.IndexedDirectories) Console.WriteLine($"dir|{d}");
                    break;
                default: throw new Exception("unknown script op: " + p[0]);
            }
        }
        break;
    }
    case "identity":
    {
        // identity <path> <length> <chunkSize>: EncryptedVideoCacheStore.ComputeVideoIdentity (the cache file key input).
        Console.WriteLine(EncryptedVideoCacheStore.ComputeVideoIdentity(args[1], long.Parse(args[2]), int.Parse(args[3])));
        break;
    }
    case "cache-write":
    {
        var dir = Path.GetFullPath(args[1]);
        const int chunk = 64 * 1024;
        var raw = Pattern(3 * chunk + 1234);
        var video = Path.Combine(dir, "video.bin");
        File.WriteAllBytes(video, raw);
        using var broker = NewBroker();
        using var manager = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker);
        if (!manager.Register("alice", "pass123")) throw new Exception("register failed");
        var store = new EncryptedVideoCacheStore(Path.Combine(dir, "VideoCache"), manager, 1L << 30);
        using (var memory = new MemoryStream(raw, writable: false))
        using (var stream = new BufferedVideoStream(memory, ownsStream: false, chunkSizeBytes: chunk, maxCachedChunks: 2,
                   readAheadChunks: 0, cacheStore: store, filePath: video))
        {
            var buffer = new byte[raw.Length];
            var read = 0;
            while (read < raw.Length) read += stream.Read(buffer, read, Math.Min(16 * 1024, raw.Length - read));
        }
        if (!manager.TryGetActiveProfileVaultKey(out var user, out var key)) throw new Exception("no key");
        var identity = EncryptedVideoCacheStore.ComputeVideoIdentity(video, raw.Length, chunk);
        Console.WriteLine($"{user}|{Hex(key)}|{identity}|{chunk}|{raw.Length}");
        break;
    }
    case "cache-read":
    {
        var dir = Path.GetFullPath(args[1]);
        var identity = args[2];
        var chunk = int.Parse(args[3]);
        var length = long.Parse(args[4]);
        using var broker = NewBroker();
        using var manager = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker);
        if (!manager.Login("alice", "pass123")) throw new Exception("login failed");
        var store = new EncryptedVideoCacheStore(Path.Combine(dir, "VideoCache"), manager, 1L << 30);
        var total = (length - 1) / chunk + 1;
        var indices = store.GetCachedChunkIndices(identity, total, chunk, length);
        for (long i = 0; i < total; i++)
        {
            var expected = (int)Math.Min(chunk, length - i * chunk);
            var data = store.TryReadChunk(identity, chunk, i, expected);
            Console.WriteLine($"{i}|{indices.Contains(i)}|{(data is null ? "null" : Hex(SHA256.HashData(data)))}");
        }
        break;
    }
    case "stream-trace":
    {
        // stream-trace <dir> <length> <chunk> <maxCached> <ops>: plays <dir>\video.bin (Pattern(length)) through a
        // BufferedVideoStream over a MemoryStream (ownsStream false: synchronous SSD writes, no read-ahead) with the
        // C# EncryptedVideoCacheStore in <dir>\VideoCache and a profile alice/pass123 in <dir>\profiles.json. Ops
        // (comma-separated): sN seek to N, eN seek to length-N, cN seek by N from the current position, rN read N bytes
        // (prints r|count|sha256 of the bytes, first 16 hex), q (prints the stream state and the buffered ratio bits
        // at 0, 0.25, 0.5, 0.99, NaN and 2), x dispose and reopen, o log out, l log in, f (prints files|count|used of
        // the cache folder).
        var dir = Path.GetFullPath(args[1]);
        var length = int.Parse(args[2]);
        var chunk = int.Parse(args[3]);
        var maxCached = int.Parse(args[4]);
        var raw = Pattern(length);
        var video = Path.Combine(dir, "video.bin");
        File.WriteAllBytes(video, raw);
        using var broker = NewBroker();
        using var manager = new LocalProfileManager(Path.Combine(dir, "profiles.json"), broker);
        if (!manager.Register("alice", "pass123")) throw new Exception("register failed");
        var store = new EncryptedVideoCacheStore(Path.Combine(dir, "VideoCache"), manager, 1L << 30);
        BufferedVideoStream Open() => new(new MemoryStream(raw, writable: false), ownsStream: false, chunkSizeBytes: chunk,
            maxCachedChunks: maxCached, readAheadChunks: 0, cacheStore: store, filePath: video);
        var stream = Open();
        foreach (var op in args[5].Split(','))
        {
            var n = op.Length > 1 ? long.Parse(op[1..]) : 0;
            switch (op[0])
            {
                case 's': stream.Seek(n, SeekOrigin.Begin); break;
                case 'e': stream.Seek(-n, SeekOrigin.End); break;
                case 'c': stream.Seek(n, SeekOrigin.Current); break;
                case 'r':
                {
                    var buffer = new byte[n];
                    var got = stream.Read(buffer, 0, (int)n);
                    Console.WriteLine($"r|{got}|{Hex(SHA256.HashData(buffer.AsSpan(0, got)))[..16]}");
                    break;
                }
                case 'q':
                {
                    var bits = string.Join("|", new[] { 0.0, 0.25, 0.5, 0.99, double.NaN, 2.0 }
                        .Select(p => BitConverter.DoubleToInt64Bits(stream.GetBufferedRatio(p)).ToString()));
                    Console.WriteLine($"q|{stream.Position}|{stream.UnderlyingReadCount}|{stream.UnderlyingBytesRead}|{stream.EncryptedCacheReadCount}|{stream.CachedChunkCount}|{bits}");
                    break;
                }
                case 'x': stream.Dispose(); stream = Open(); break;
                case 'o': manager.Logout(); store.EndStaleSessions(); break;
                case 'l': if (!manager.Login("alice", "pass123")) throw new Exception("login failed"); break;
                case 'f':
                {
                    var cacheDir = Path.Combine(dir, "VideoCache");
                    var count = Directory.Exists(cacheDir) ? Directory.EnumerateFiles(cacheDir, "*.vcache", SearchOption.AllDirectories).Count() : 0;
                    Console.WriteLine($"f|{count}|{store.GetTotalUsedBytes()}");
                    break;
                }
                default: throw new Exception("unknown stream op: " + op);
            }
        }
        stream.Dispose();
        break;
    }
    case "format-size":
    {
        // format-size <bytes>...: MediaLibraryIndex.FormatFileSize per argument.
        foreach (var a in args.Skip(1)) Console.WriteLine(MediaLibraryIndex.FormatFileSize(long.Parse(a)));
        break;
    }
    case "mime":
    {
        // mime <path>...: BufferedVideoStream.GetMimeType per argument.
        foreach (var a in args.Skip(1)) Console.WriteLine(BufferedVideoStream.GetMimeType(a));
        break;
    }
    case "badge":
    {
        // badge <ext|-null>...: IndexedMediaFile.SidecarBadge with a sidecar of that extension ("-null" = null).
        foreach (var a in args.Skip(1))
        {
            var f = new IndexedMediaFile("C:\\a.mp4", "a.mp4", "C:\\", 1, default, true, a == "-null" ? null : a, default);
            Console.WriteLine(f.SidecarBadge ?? "(none)");
        }
        break;
    }
    case "probe":
    {
        foreach (var p in new[] { @"\\server\share\dir", @"\\server\share", @"\\?\C:\x", @"\\?\UNC\srv\sh\x", @"C:\", @"C:", @"C:\a\" })
            Console.WriteLine($"{p}|{Path.GetPathRoot(p)}|{MediaLibraryIndex.NormalizeDirectoryPath(p)}");
        break;
    }
    default:
        Console.Error.WriteLine("usage: script <dir> <file> | identity <path> <len> <cs> | index-write <dir> | index-read <index> <folder> | index-dump <index> [query] | cache-write <dir> | cache-read <dir> <identity> <chunk> <len> | probe");
        return 2;
}
return 0;
