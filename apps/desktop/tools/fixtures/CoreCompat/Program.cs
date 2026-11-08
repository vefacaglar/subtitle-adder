// Drives the C# app's own core helpers for the sa-core behavior comparison (see CoreCompat.csproj).
// Every output line is one JSON array, so strings with any characters survive the trip.
//
//   labels                       model labels, rows, language/theme/background/display titles, overlay percent texts,
//                                undo titles and catalog totals, in tr and en
//   sizes <from> <to>            ModelLabel.SizeText(mb) for mb in [from, to], tr and en
//   bytes <file>                 FormatByteSize(long) per input line, under tr-TR and en-US
//   times <file>                 SubtitleTime.Short(double) per input line ("R" doubles)
//   hex <file>                   HexColor.Parse / ParseRgb and OverlayStyle(text hex) per input line (JSON strings)
//   style <file>                 OverlayStyle(size, opacity) sanitizing and derived values per input line
//   parse <dir>                  SubtitleParser.ParseFile per file (sorted by name)
//   decode <dir>                 SubtitleParser.DecodeBytes per file (UTF-16 code units)
//   atomic-read <dir>            AtomicFileStore.ReadAllTextShared per file (UTF-16 code units)
//   redact <file>                DiagnosticLog.Redact per input line (JSON strings); run with TMP/TEMP pointing at a
//                                scratch folder, because touching DiagnosticLog scrubs %TEMP%\SubtitleAdder-*.log
//   paths <file>                 IsSubtitleFile / FindSidecarSubtitle / FindCompanionVideo per input path
//   settings-read <path>         typed reads of every key of a settings.json through JsonFileSettingsStore
//   settings-write <path>        writes a known set of values through JsonFileSettingsStore and disposes it
//   undo <file>                  replays an operation script (set/begin/end/undo/redo/clear) on UndoStack
//   scrub                        starts DiagnosticLog (startup scrub of the TMP folder), writes one line, waits for it
using System.Globalization;
using System.Reflection;
using System.Text.Encodings.Web;
using System.Text.Json;
using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;

var json = new JsonSerializerOptions { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
Console.OutputEncoding = new System.Text.UTF8Encoding(false);
var stdout = new StreamWriter(Console.OpenStandardOutput(), new System.Text.UTF8Encoding(false)) { NewLine = "\n" };

void Emit(params object?[] fields) => stdout.WriteLine(JsonSerializer.Serialize(fields, json));
static string R(double v) => v.ToString("R", CultureInfo.InvariantCulture);
static string Units(string s) => string.Join(" ", s.Select(c => ((int)c).ToString("X4", CultureInfo.InvariantCulture)));
static StateSyncBroker NewBroker() => new("core-compat-" + Guid.NewGuid().ToString("N"));
static IEnumerable<string> InputLines(string path) =>
    File.ReadAllText(path, new System.Text.UTF8Encoding(false)).Split('\n').Where(l => l.Length > 0);

var cmd = args.Length > 0 ? args[0] : "";
switch (cmd)
{
    case "labels":
    {
        foreach (var lang in new[] { UILanguage.Tr, UILanguage.En })
        {
            LanguageState.Shared.Language = lang;
            var code = lang.Code();
            foreach (var baseModel in ModelCatalog.FallbackModels)
            {
                foreach (var model in new[] { baseModel, baseModel with { Downloaded = true }, baseModel with { Downloaded = true, DiskMB = 1234 } })
                {
                    Emit("label", code, model.Id, model.Downloaded, model.DiskMB, ModelLabel.Name(model.Id), ModelLabel.MenuTitle(model),
                        ModelLabel.Detail(model), ModelLabel.LanguageSummary(model), ModelLabel.StorageStatus(model),
                        ModelLabel.LicenseText(model));
                    foreach (var (deleting, active) in new[] { (false, false), (true, false), (false, true), (true, true) })
                    {
                        var row = new ModelRow(model, deleting, active, null);
                        Emit("row", code, model.Id, model.Downloaded, model.DiskMB, deleting, active, row.Name, row.LanguageSummary,
                            row.StorageStatus, row.LicenseText, row.IsNonCommercial, row.ShowsDeleteButton, row.CanDelete,
                            row.DeleteHelp, row.DeleteConfirmTitle, row.DeleteConfirmMessage);
                    }
                }
            }
            var unknown = new EngineModel { Id = "custom-x", SizeMB = 12, Speed = 0, Accuracy = 0, License = null, Downloaded = false };
            Emit("label", code, unknown.Id, unknown.Downloaded, unknown.DiskMB, ModelLabel.Name(unknown.Id), ModelLabel.MenuTitle(unknown),
                ModelLabel.Detail(unknown), ModelLabel.LanguageSummary(unknown), ModelLabel.StorageStatus(unknown),
                ModelLabel.LicenseText(unknown));
            foreach (var l in SubtitleLanguage.All) Emit("language", code, l.Code, l.Title, l.ShortName, l.TargetName);
            foreach (var t in ThemeChoices.All) Emit("theme", code, t.RawValue(), t.Title());
            foreach (var b in OverlayBackgrounds.All) Emit("background", code, b.RawValue(), b.Title());
            foreach (var d in SubtitleDisplays.All) Emit("display", code, d.RawValue(), d.Title());
            var store = new InMemorySettingsStore();
            var appearance = new AppearanceSettings(store);
            foreach (var v in new[] { 0.0, 0.03, 0.045, 0.05, 0.055, 0.0549999, 0.09, 0.5, 0.6, 0.625, 0.005, 0.995, 1.0, 2.0 })
            {
                appearance.Size = v;
                appearance.Opacity = v;
                Emit("percent", code, R(v), appearance.SizeText, appearance.OpacityText);
            }
            foreach (var key in new[] { "Satırı Sil", "Metni Değiştir", "Satırları Birleştir", "Konuşmacıyı Değiştir", "Zamanlamayı Değiştir" })
                Emit("undo", code, key, Strings.L("%@ İşlemini Geri Al", Strings.L(key)), Strings.L("%@ İşlemini Yinele", Strings.L(key)));
            Emit("undo-empty", code, Strings.L("Geri Al"), Strings.L("Yinele"));
            Emit("total", code, Strings.L("Toplam %@ disk kullanılıyor.", ModelLabel.SizeText(4339)), Strings.L("Henüz indirilmiş model yok."));
        }
        break;
    }
    case "sizes":
    {
        var from = int.Parse(args[1], CultureInfo.InvariantCulture);
        var to = int.Parse(args[2], CultureInfo.InvariantCulture);
        for (var mb = from; mb <= to; mb++)
        {
            LanguageState.Shared.Language = UILanguage.Tr;
            var tr = ModelLabel.SizeText(mb);
            LanguageState.Shared.Language = UILanguage.En;
            Emit(mb, tr, ModelLabel.SizeText(mb));
        }
        break;
    }
    case "bytes":
    {
        foreach (var line in InputLines(args[1]))
        {
            var bytes = long.Parse(line, CultureInfo.InvariantCulture);
            CultureInfo.CurrentUICulture = new CultureInfo("tr-TR");
            var tr = SubtitleViewModel.FormatByteSize(bytes);
            CultureInfo.CurrentUICulture = new CultureInfo("en-US");
            Emit(line, tr, SubtitleViewModel.FormatByteSize(bytes));
        }
        break;
    }
    case "times":
    {
        foreach (var line in InputLines(args[1]))
            Emit(line, SubtitleTime.Short(double.Parse(line, CultureInfo.InvariantCulture)));
        break;
    }
    case "hex":
    {
        foreach (var line in InputLines(args[1]))
        {
            var input = JsonSerializer.Deserialize<string>(line)!;
            var parsed = HexColor.Parse(input);
            var rgb = HexColor.ParseRgb(input);
            var style = new OverlayStyle("", 0.05, input, OverlayBackground.Box, 0.6);
            Emit(input, parsed is { } p ? new[] { R(p.R), R(p.G), R(p.B) } : null, rgb?.Hex, style.TextHex, style.TextColor.Hex);
        }
        break;
    }
    case "style":
    {
        foreach (var line in InputLines(args[1]))
        {
            var v = double.Parse(line, CultureInfo.InvariantCulture);
            var style = new OverlayStyle("", v, "#FFFFFF", "nope", v);
            Emit(line, R(style.SizeFraction), R(style.BackgroundOpacity), style.Background.RawValue(), R(style.FontSize(720)),
                R(style.OutlineThickness(720)), R(style.OutlineThickness(100)), R(style.ShadowOpacity),
                HexColor.Format(v, v / 2, 1 - v));
        }
        break;
    }
    case "parse":
    {
        foreach (var file in Directory.GetFiles(args[1]).OrderBy(f => f, StringComparer.Ordinal))
        {
            var sentences = SubtitleParser.ParseFile(file);
            Emit(Path.GetFileName(file), sentences.Select(s => new object?[]
            {
                s.Id, R(s.Start), R(s.End), s.Text, s.Words.Select(w => new object[] { R(w.Start), R(w.End), w.Text }).ToArray(),
                s.Speaker, s.NeedsReview,
            }).ToArray());
        }
        break;
    }
    case "decode":
    {
        var decode = typeof(SubtitleParser).GetMethod("DecodeBytes", BindingFlags.NonPublic | BindingFlags.Static)!;
        foreach (var file in Directory.GetFiles(args[1]).OrderBy(f => f, StringComparer.Ordinal))
            Emit(Path.GetFileName(file), Units((string)decode.Invoke(null, [File.ReadAllBytes(file)])!));
        break;
    }
    case "atomic-read":
    {
        foreach (var file in Directory.GetFiles(args[1]).OrderBy(f => f, StringComparer.Ordinal))
            Emit(Path.GetFileName(file), AtomicFileStore.ReadAllTextShared(file) is { } text ? Units(text) : null);
        break;
    }
    case "redact":
    {
        foreach (var line in InputLines(args[1]))
            Emit(DiagnosticLog.Redact(JsonSerializer.Deserialize<string>(line)));
        break;
    }
    case "paths":
    {
        foreach (var line in InputLines(args[1]))
        {
            var path = JsonSerializer.Deserialize<string>(line)!;
            Emit(path, SubtitleParser.IsSubtitleFile(path), SubtitleParser.FindSidecarSubtitle(path), SubtitleParser.FindCompanionVideo(path));
        }
        break;
    }
    case "settings-read":
    {
        using var broker = NewBroker();
        using var store = new JsonFileSettingsStore(args[1], TimeSpan.FromMilliseconds(50), broker);
        var root = JsonDocument.Parse(File.ReadAllText(args[1])).RootElement;
        foreach (var property in root.EnumerateObject())
        {
            var key = property.Name;
            Emit(key, store.GetString(key), store.GetBool(key), store.GetInt(key), store.GetDouble(key) is { } d ? R(d) : null);
        }
        break;
    }
    case "settings-write":
    {
        using var broker = NewBroker();
        var store = new JsonFileSettingsStore(args[1], TimeSpan.FromMilliseconds(50), broker);
        store.SetString("uiLanguage", "en");
        store.SetString("explorerLastFolder", "C:\\Users\\Özcan\\Videos <&> \"q\"");
        store.SetDouble("overlayFontSize", 0.07);
        store.SetDouble("tiny", 1e-5);
        store.SetDouble("whole", 3.0);
        store.SetDouble("nan", double.NaN);
        store.SetInt("speakerCount", 3);
        store.SetInt("negative", -42);
        store.SetBool("offlineMode", false);
        store.SetString("emoji", "😀 ğüşıöç İ");
        store.Dispose();
        Emit("ok");
        break;
    }
    case "settings-ops":
    {
        // Replays typed writes on an InMemorySettingsStore (values held as in-memory JsonValue primitives, not parsed
        // from a file) and prints, after every operation, how many Changed events fired so far and the typed reads of
        // the touched key. Ops: "int k v", "double k v" (R syntax, "NaN"), "string k v", "bool k true|false",
        // "remove k".
        var store = new InMemorySettingsStore();
        var changed = new List<string>();
        store.Changed += (_, key) => changed.Add(key);
        foreach (var line in InputLines(args[1]))
        {
            var op = line.Split(' ', 3);
            var key = op[1];
            switch (op[0])
            {
                case "int": store.SetInt(key, int.Parse(op[2], CultureInfo.InvariantCulture)); break;
                case "double": store.SetDouble(key, double.Parse(op[2], CultureInfo.InvariantCulture)); break;
                case "string": store.SetString(key, op[2]); break;
                case "bool": store.SetBool(key, op[2] == "true"); break;
                case "remove": store.Remove(key); break;
            }
            Emit(line, changed.Count, store.GetString(key), store.GetBool(key), store.GetInt(key),
                store.GetDouble(key) is { } d ? R(d) : null);
        }
        break;
    }
    case "undo":
    {
        // Replays an operation script on the real UndoStack; after every operation prints the observable state.
        var stack = new UndoStack();
        var cells = new int[4];
        var changed = 0;
        stack.Changed += (_, _) => changed++;
        var scopes = new Stack<IDisposable>();
        static string Swap(string key) => key.EndsWith('~') ? key[..^1] : key + "~";
        void Set(int i, int v, string key)
        {
            var old = cells[i];
            cells[i] = v;
            stack.Register(key, () => Set(i, old, Swap(key)));
        }
        foreach (var line in InputLines(args[1]))
        {
            var op = line.Split(' ');
            switch (op[0])
            {
                case "set": Set(int.Parse(op[1]), int.Parse(op[2]), op[3]); break;
                case "begin": scopes.Push(stack.BeginGroup()); break;
                case "end": if (scopes.Count > 0) scopes.Pop().Dispose(); break;
                case "undo": stack.Undo(); break;
                case "redo": stack.Redo(); break;
                case "clear": stack.Clear(); break;
            }
            Emit(line, cells, stack.CanUndo, stack.CanRedo, stack.UndoActionKey, stack.RedoActionKey, stack.IsUndoing, stack.IsRedoing, changed);
        }
        break;
    }
    case "scrub":
    {
        // TMP/TEMP point at the folder to scrub. Starts the log (which scrubs first), writes one line and waits for it.
        DiagnosticLog.Initialize();
        DiagnosticLog.Write("t", "x C:\\a\\b.mp4");
        var deadline = DateTime.UtcNow.AddSeconds(20);
        while (DateTime.UtcNow < deadline)
        {
            try
            {
                using var fs = new FileStream(DiagnosticLog.LogFilePath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                if (new StreamReader(fs).ReadToEnd().Contains("[t] x <path>", StringComparison.Ordinal)) break;
            }
            catch (IOException)
            {
            }
            Thread.Sleep(20);
        }
        Emit(DiagnosticLog.LogFilePath);
        break;
    }
    default:
        Console.Error.WriteLine($"unknown command '{cmd}'");
        return 2;
}
stdout.Flush();
return 0;
