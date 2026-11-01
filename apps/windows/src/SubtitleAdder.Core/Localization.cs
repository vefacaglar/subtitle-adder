using System.ComponentModel;
using CommunityToolkit.Mvvm.ComponentModel;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder.Core;

/// <summary>
/// The app's localized strings. Keys are the Turkish source strings; the Windows catalog
/// (Strings/Windows.xcstrings) is searched first, then the shared macOS app catalog, and a missing
/// translation falls back to the key.
/// </summary>
public static class Strings
{
    private static readonly Lazy<StringCatalog> LazyApp = new(() =>
        StringCatalog.FromResource(typeof(Strings).Assembly, "SubtitleAdder.Core.Localizable.xcstrings"));

    private static readonly Lazy<StringCatalog> LazyWindows = new(() =>
        StringCatalog.FromResource(typeof(Strings).Assembly, "SubtitleAdder.Core.Windows.xcstrings"));

    /// <summary>The macOS app catalog (embedded, shared with macOS).</summary>
    public static StringCatalog App => LazyApp.Value;

    /// <summary>Windows-only strings.</summary>
    public static StringCatalog Windows => LazyWindows.Value;

    /// <summary>Lookup order: Windows first, then the shared app catalog.</summary>
    public static IReadOnlyList<StringCatalog> Catalogs => [Windows, App];

    /// <summary>
    /// Localized string in the current UI language (<see cref="LanguageState.Shared"/>), formatted with printf-style
    /// <paramref name="args"/>. Calling it again after <see cref="LanguageState.LanguageChanged"/> gives the new language.
    /// </summary>
    public static string L(string key, params object?[] args) => Localize(LanguageState.Shared.Language, key, args);

    /// <summary>Like <see cref="L"/> in an explicit language.</summary>
    public static string Localize(UILanguage language, string key, params object?[] args) =>
        language.Localize(key, Catalogs, args);

    /// <summary>
    /// Marks a catalog key that is localized later (for example an undo action name); returns it unchanged.
    /// The catalog completeness test scans for both <c>L("…")</c> and <c>K("…")</c>.
    /// </summary>
    public static string K(string key) => key;
}

/// <summary>
/// Bindable string table for XAML: <c>this[key]</c> returns the localized text and the indexer is re-raised
/// (<c>Item[]</c>) when the UI language changes, so bindings refresh immediately.
/// </summary>
public sealed class LocalizedStrings : INotifyPropertyChanged
{
    public static LocalizedStrings Shared { get; } = new(LanguageState.Shared);

    private readonly LanguageState _state;

    public LocalizedStrings(LanguageState state)
    {
        _state = state;
        _state.LanguageChanged += (_, _) => Dispatcher.Post(() =>
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs("Item[]"));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
        });
    }

    /// <summary>Where change notifications are raised; the app sets it to its UI dispatcher at startup.</summary>
    public IUiDispatcher Dispatcher { get; set; } = ImmediateDispatcher.Instance;

    public event PropertyChangedEventHandler? PropertyChanged;

    public string this[string key] => Strings.Localize(_state.Language, key);
}

/// <summary>The language choice shown in Settings.</summary>
public enum LanguageChoice
{
    System,
    Turkish,
    English,
}

public static class LanguageChoices
{
    public static IReadOnlyList<LanguageChoice> All { get; } =
        [LanguageChoice.System, LanguageChoice.Turkish, LanguageChoice.English];

    /// <summary>Stored value: "system", "tr", "en" (same as macOS).</summary>
    public static string RawValue(this LanguageChoice choice) => choice switch
    {
        LanguageChoice.Turkish => "tr",
        LanguageChoice.English => "en",
        _ => "system",
    };

    public static LanguageChoice? Parse(string? raw) => raw switch
    {
        "system" => LanguageChoice.System,
        "tr" => LanguageChoice.Turkish,
        "en" => LanguageChoice.English,
        _ => null,
    };

    /// <summary>Picker label. Language names are shown in their own language except "Sistem".</summary>
    public static string Title(this LanguageChoice choice) => choice switch
    {
        LanguageChoice.Turkish => "Türkçe",
        LanguageChoice.English => "English",
        _ => Strings.L("Sistem"),
    };

    /// <summary>
    /// The UI language for this choice. "System" is Turkish when the user's first supported Windows UI language
    /// is Turkish, otherwise English.
    /// </summary>
    public static UILanguage Resolved(this LanguageChoice choice, IEnumerable<string>? preferred = null) => choice switch
    {
        LanguageChoice.Turkish => UILanguage.Tr,
        LanguageChoice.English => UILanguage.En,
        _ => UILanguages.System(preferred),
    };
}

/// <summary>
/// Holds the persisted language choice and applies it to <see cref="LanguageState"/> immediately (strings and the
/// engine's <c>SUBTITLE_ENGINE_LANG</c>). Windows needs no relaunch: every string the app shows is its own.
/// </summary>
public sealed partial class AppLanguage : ObservableObject
{
    public const string DefaultsKey = "uiLanguage";

    private readonly ISettingsStore _settings;
    private readonly LanguageState _state;
    private readonly Func<IEnumerable<string>> _systemLanguages;
    private IReadOnlyList<string> _systemPreferred;
    private LanguageChoice _choice;

    /// <param name="systemLanguages">
    /// The Windows user language list (e.g. <c>GlobalizationPreferences.Languages</c>); defaults to the current UI culture.
    /// </param>
    public AppLanguage(ISettingsStore settings, Func<IEnumerable<string>>? systemLanguages = null, LanguageState? state = null)
    {
        _settings = settings;
        _state = state ?? LanguageState.Shared;
        _systemLanguages = systemLanguages ?? (() => [System.Globalization.CultureInfo.CurrentUICulture.Name]);
        _systemPreferred = [.. _systemLanguages()];
        _choice = LanguageChoices.Parse(settings.GetString(DefaultsKey)) ?? LanguageChoice.System;
        _state.Language = Effective;
    }

    public LanguageChoice Choice
    {
        get => _choice;
        set
        {
            if (!SetProperty(ref _choice, value)) return;
            _settings.SetString(DefaultsKey, value.RawValue());
            Apply();
        }
    }

    public UILanguage Effective => _choice.Resolved(_systemPreferred);

    /// <summary>Re-reads the system language list (call when Windows reports a language change).</summary>
    public void RefreshSystemLanguages()
    {
        _systemPreferred = [.. _systemLanguages()];
        Apply();
    }

    private void Apply()
    {
        _systemPreferred = [.. _systemLanguages()];
        _state.Language = Effective;
        OnPropertyChanged(nameof(Effective));
    }
}

/// <summary>
/// A message produced by the app that is localized when it is displayed, not when it is created, so stored
/// status/error text follows a language switch. Equality compares <see cref="Id"/> only.
/// </summary>
public sealed class LocalizedMessage : IEquatable<LocalizedMessage>
{
    private readonly Func<string> _build;

    /// <summary><paramref name="id"/> must identify the content (key plus arguments) so equal ids mean equal messages.</summary>
    public LocalizedMessage(string id, Func<string> build)
    {
        Id = id;
        _build = build;
    }

    public string Id { get; }

    /// <summary>The text in the current UI language.</summary>
    public string Text => _build();

    /// <summary>App-generated text with a catalog key and no arguments.</summary>
    public static LocalizedMessage Key(string key) => new($"key:{key}", () => Strings.L(key));

    /// <summary>Text that is already final (for example from the engine).</summary>
    public static LocalizedMessage Verbatim(string text) => new($"verbatim:{text}", () => text);

    /// <summary>An error shown with its own message; "engine not found" is re-localized on each display.</summary>
    public static LocalizedMessage Error(Exception error)
    {
        if (error is EngineException { Error: EngineError.NotFound })
            return new("error:notFound", () => EngineException.NotFound().Message);
        var message = error.Message;
        return new($"error:{error.GetType().Name}:{message}", () => message);
    }

    public bool Equals(LocalizedMessage? other) => other is not null && other.Id == Id;

    public override bool Equals(object? obj) => Equals(obj as LocalizedMessage);

    public override int GetHashCode() => Id.GetHashCode(StringComparison.Ordinal);

    public override string ToString() => Text;

    public static bool operator ==(LocalizedMessage? left, LocalizedMessage? right) =>
        left is null ? right is null : left.Equals(right);

    public static bool operator !=(LocalizedMessage? left, LocalizedMessage? right) => !(left == right);
}
