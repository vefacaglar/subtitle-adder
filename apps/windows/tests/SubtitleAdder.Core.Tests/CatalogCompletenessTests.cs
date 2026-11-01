using System.Net;
using System.Text.RegularExpressions;
using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

/// <summary>
/// Every key the Windows code passes to the localization helpers exists with both tr and en in one of the catalogs
/// (Windows.xcstrings or the shared macOS app catalog). Scanned usages:
/// C#: <c>L("…")</c>, <c>K("…")</c>, <c>Strings.L("…")</c>, <c>LocalizedMessage.Key("…")</c>, <c>Localize(lang, "…")</c>;
/// XAML: <c>loc:Localize.&lt;Property&gt;="…"</c> attributes and <c>[…]</c> indexer paths on <c>LocalizedStrings</c>
/// (<c>{Binding [key], Source=…Strings…}</c>).
/// </summary>
public partial class CatalogCompletenessTests
{
    [GeneratedRegex("""(?<![\w.])(?:Strings\.)?(?:L|K)\(\s*"((?:[^"\\]|\\.)*)"|LocalizedMessage\.Key\(\s*"((?:[^"\\]|\\.)*)"|Strings\.Localize\([^,()]+,\s*"((?:[^"\\]|\\.)*)"|\bLocalize\(\s*UILanguage\.\w+\s*,\s*"((?:[^"\\]|\\.)*)"|LocalizedStrings\.Shared\[\s*"((?:[^"\\]|\\.)*)"\s*\]""")]
    private static partial Regex CSharpUsage();

    [GeneratedRegex("""loc:Localize\.\w+\s*=\s*"([^"]*)"|\{x:Bind\s+[\w.]*\[([^\]]*)\]|\{Binding\s+(?:Path=)?\[([^\]]*)\]""")]
    private static partial Regex XamlUsage();

    private static string WindowsRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !File.Exists(Path.Combine(dir.FullName, "SubtitleAdder.slnx"))) dir = dir.Parent;
        return dir?.FullName ?? throw new InvalidOperationException("apps/windows not found");
    }

    private static string Unescape(string literal) => Regex.Unescape(literal);

    private static IEnumerable<(string Key, string File)> UsedKeys()
    {
        var root = WindowsRoot();
        var sources = new[] { Path.Combine(root, "src", "SubtitleAdder.Core"), Path.Combine(root, "src", "SubtitleAdder") }
            .Where(Directory.Exists)
            .SelectMany(dir => Directory.EnumerateFiles(dir, "*.*", SearchOption.AllDirectories))
            .Where(f => !f.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}")
                        && !f.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}"));
        foreach (var file in sources)
        {
            if (file.EndsWith(".cs", StringComparison.OrdinalIgnoreCase))
            {
                // Comment lines (doc comments mention the helpers) are not usages.
                var code = string.Join(Environment.NewLine, File.ReadAllLines(file).Where(line => !line.TrimStart().StartsWith("//", StringComparison.Ordinal)));
                foreach (Match match in CSharpUsage().Matches(code))
                {
                    var group = match.Groups.Cast<Group>().Skip(1).First(g => g.Success);
                    yield return (Unescape(group.Value), file);
                }
            }
            else if (file.EndsWith(".xaml", StringComparison.OrdinalIgnoreCase))
            {
                foreach (Match match in XamlUsage().Matches(File.ReadAllText(file)))
                {
                    var group = match.Groups.Cast<Group>().Skip(1).First(g => g.Success);
                    yield return (WebUtility.HtmlDecode(group.Value), file);
                }
            }
        }
    }

    [Fact]
    public void ScannerFindsCoreUsages()
    {
        var keys = UsedKeys().Select(k => k.Key).ToHashSet();
        Assert.Contains("Başlıyor…", keys);
        Assert.Contains("Satırı Sil", keys);
        Assert.Contains("%@ İşlemini Geri Al", keys);
        Assert.Contains("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (Ctrl+Enter)", keys);
        Assert.True(keys.Count > 60, $"only {keys.Count} keys found");
    }

    [Fact]
    public void EveryUsedKeyHasTurkishAndEnglish()
    {
        var missing = UsedKeys()
            .Where(k => !Strings.Catalogs.Any(c => c.Lookup(k.Key, UILanguage.Tr) is not null && c.Lookup(k.Key, UILanguage.En) is not null))
            .Select(k => $"{k.Key}  ({Path.GetFileName(k.File)})")
            .Distinct()
            .ToList();
        Assert.True(missing.Count == 0, "Keys without tr+en:\n" + string.Join("\n", missing));
    }

    [Fact]
    public void WindowsCatalogEntriesHaveBothLanguages()
    {
        Assert.NotEmpty(Strings.Windows.Keys);
        foreach (var key in Strings.Windows.Keys)
        {
            Assert.NotNull(Strings.Windows.Lookup(key, UILanguage.Tr));
            Assert.NotNull(Strings.Windows.Lookup(key, UILanguage.En));
        }
    }

    [Fact]
    public void ScannerRecognizesXamlForms()
    {
        var xaml = """
            <TextBlock loc:Localize.Text="Altyazı üret" />
            <TextBlock Text="{Binding [Hazır], Source={StaticResource Strings}}" />
            <TextBlock Text="{x:Bind Strings[Dil]}" />
            <TextBlock Text="{Binding Path=[Sil], Source={StaticResource Strings}}" />
            """;
        var found = XamlUsage().Matches(xaml).Select(m => m.Groups.Cast<Group>().Skip(1).First(g => g.Success).Value).ToList();
        Assert.Equal(["Altyazı üret", "Hazır", "Dil", "Sil"], found);
    }
}
