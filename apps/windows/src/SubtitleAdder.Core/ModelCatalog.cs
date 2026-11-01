using CommunityToolkit.Mvvm.ComponentModel;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder.Core;

/// <summary>
/// Single source of truth for the model catalog: the toolbar picker, the Settings window and
/// subtitle generation see the same list; after a refresh/delete all of them update together.
/// Call its members on the UI thread; awaits resume on the caller's context.
/// </summary>
public sealed class ModelCatalog : ObservableObject
{
    private static readonly string[] TranslationLanguages = ["tr", "en", "pt", "es", "ru", "de"];

    private static readonly string[] EuropeanLanguages =
    [
        "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it",
        "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
    ];

    /// <summary>List shown when the engine isn't found or the catalog can't be fetched (same as the engine catalog).</summary>
    public static IReadOnlyList<EngineModel> FallbackModels { get; } =
    [
        new() { Id = "base", SizeMB = 145, Speed = 5, Accuracy = 2, License = "MIT", Downloaded = false },
        new()
        {
            Id = "gigaam-v3", Backend = "onnx", Languages = ["ru"],
            SizeMB = 216, Speed = 5, Accuracy = 4, License = "MIT", Downloaded = false,
        },
        new() { Id = "small", SizeMB = 465, Speed = 4, Accuracy = 3, License = "MIT", Downloaded = false },
        new()
        {
            Id = "parakeet-v3", Backend = "onnx", Languages = EuropeanLanguages,
            SizeMB = 640, Speed = 5, Accuracy = 4, License = "CC-BY-4.0", Downloaded = false,
        },
        new()
        {
            Id = "canary-1b-v2", Backend = "onnx", Languages = EuropeanLanguages,
            SizeMB = 983, Speed = 2, Accuracy = 4, License = "CC-BY-4.0", Downloaded = false,
        },
        new() { Id = "medium", SizeMB = 1500, Speed = 3, Accuracy = 4, License = "MIT", Downloaded = false },
        new() { Id = "large-v3-turbo", SizeMB = 1600, Speed = 2, Accuracy = 4, License = "MIT", Downloaded = false },
        new() { Id = "large-v3", SizeMB = 3000, Speed = 1, Accuracy = 5, License = "MIT", Downloaded = false },
        new()
        {
            Id = "speaker-diarization", Kind = "diarization", Backend = "sherpa",
            SizeMB = 29, Speed = 4, Accuracy = 3, License = "MIT AND Apache-2.0", Downloaded = false,
        },
        new()
        {
            Id = "nllb-1.3b", Kind = "translation", Backend = "ctranslate2", Languages = TranslationLanguages,
            SizeMB = 1336, Speed = 3, Accuracy = 4, License = "CC-BY-NC-4.0", Downloaded = false,
        },
        new()
        {
            Id = "qwen3.5-4b", Kind = "translation", Backend = "llama", Languages = TranslationLanguages,
            SizeMB = 2614, Speed = 3, Accuracy = 4, License = "Apache-2.0", Downloaded = false,
        },
        new()
        {
            Id = "madlad-3b", Kind = "translation", Backend = "ctranslate2", Languages = TranslationLanguages,
            SizeMB = 2839, Speed = 2, Accuracy = 5, License = "Apache-2.0", Downloaded = false,
        },
        new()
        {
            Id = "gemma-4-e4b", Kind = "translation", Backend = "llama", Languages = TranslationLanguages,
            SizeMB = 4747, Speed = 2, Accuracy = 5, License = "Apache-2.0", Downloaded = false,
        },
        new()
        {
            Id = "qwen3.5-9b", Kind = "translation", Backend = "llama", Languages = TranslationLanguages,
            SizeMB = 5417, Speed = 2, Accuracy = 5, License = "Apache-2.0", Downloaded = false,
        },
        new()
        {
            Id = "gemma-4-12b", Kind = "translation", Backend = "llama", Languages = TranslationLanguages,
            SizeMB = 6792, Speed = 1, Accuracy = 5, License = "Apache-2.0", Downloaded = false,
        },
    ];

    private IReadOnlyList<EngineModel> _models = FallbackModels;
    private bool _isLoading;
    private LocalizedMessage? _loadError;
    private IReadOnlySet<string> _deleting = new HashSet<string>();
    private IReadOnlyDictionary<string, LocalizedMessage> _deleteErrors = new Dictionary<string, LocalizedMessage>();
    private IReadOnlySet<string> _activeModels = new HashSet<string>();

    public ModelCatalog(IEngineClient? engine)
    {
        Engine = engine;
        PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(Models) or nameof(Deleting) or nameof(DeleteErrors) or nameof(ActiveModels))
                RaiseRows();
        };
    }

    private void RaiseRows()
    {
        OnPropertyChanged(nameof(TranscriptionRows));
        OnPropertyChanged(nameof(DiarizationRows));
        OnPropertyChanged(nameof(TranslationRows));
    }

    /// <summary>A catalog backed by the located engine (<see cref="EngineCommand.Locate"/>), or no engine if none is found.</summary>
    public static ModelCatalog Default() =>
        new(EngineCommand.Locate() is { } command ? new EngineClient(command) : null);

    /// <summary><c>null</c> when the engine wasn't found.</summary>
    public IEngineClient? Engine { get; }

    public IReadOnlyList<EngineModel> Models
    {
        get => _models;
        private set
        {
            if (!SetProperty(ref _models, value)) return;
            OnPropertyChanged(nameof(TranscriptionModels));
            OnPropertyChanged(nameof(TranslationModels));
            OnPropertyChanged(nameof(DiarizationModels));
            OnPropertyChanged(nameof(TotalDiskMB));
            OnPropertyChanged(nameof(TotalDiskText));
        }
    }

    public bool IsLoading
    {
        get => _isLoading;
        private set => SetProperty(ref _isLoading, value);
    }

    public LocalizedMessage? LoadError
    {
        get => _loadError;
        private set => SetProperty(ref _loadError, value);
    }

    /// <summary>Ids of the models being deleted.</summary>
    public IReadOnlySet<string> Deleting
    {
        get => _deleting;
        private set => SetProperty(ref _deleting, value);
    }

    /// <summary>The last delete error per model id.</summary>
    public IReadOnlyDictionary<string, LocalizedMessage> DeleteErrors
    {
        get => _deleteErrors;
        private set => SetProperty(ref _deleteErrors, value);
    }

    /// <summary>
    /// The models used by the running subtitle generation or translation (including the speaker model);
    /// those can't be deleted from Settings.
    /// </summary>
    public IReadOnlySet<string> ActiveModels
    {
        get => _activeModels;
        set => SetProperty(ref _activeModels, value);
    }

    /// <summary>Total actual disk usage in the catalog (models not downloaded are not counted).</summary>
    public int TotalDiskMB => Models.Where(m => m.Downloaded).Sum(m => m.DiskMB ?? m.SizeMB);

    /// <summary>"Toplam … disk kullanılıyor." or "Henüz indirilmiş model yok."</summary>
    public string TotalDiskText => TotalDiskMB > 0
        ? Strings.L("Toplam %@ disk kullanılıyor.", ModelLabel.SizeText(TotalDiskMB))
        : Strings.L("Henüz indirilmiş model yok.");

    /// <summary>The "Altyazı modelleri" section in Settings.</summary>
    public IReadOnlyList<EngineModel> TranscriptionModels => [.. Models.Where(m => m.IsTranscription)];

    /// <summary>The "Çeviri modelleri" section in Settings and the default translation model picker.</summary>
    public IReadOnlyList<EngineModel> TranslationModels => [.. Models.Where(m => m.IsTranslation)];

    /// <summary>Speaker separation models; shown in their own Settings section, never in the transcription picker.</summary>
    public IReadOnlyList<EngineModel> DiarizationModels => [.. Models.Where(m => m.IsDiarization)];

    /// <summary>Subtitle models the toolbar picker sees (translation models excluded).</summary>
    public IReadOnlyList<EngineModel> ModelsFor(string language)
    {
        var transcription = Models.Where(m => m.IsTranscription);
        return language.Length == 0
            ? [.. transcription.Where(m => m.DetectsLanguage)]
            : [.. transcription.Where(m => m.Supports(language))];
    }

    /// <summary>Display rows of the Settings sections (rebuilt whenever the catalog, a delete or the active jobs change).</summary>
    public IReadOnlyList<ModelRow> TranscriptionRows => Rows(TranscriptionModels);

    public IReadOnlyList<ModelRow> DiarizationRows => Rows(DiarizationModels);

    public IReadOnlyList<ModelRow> TranslationRows => Rows(TranslationModels);

    private IReadOnlyList<ModelRow> Rows(IEnumerable<EngineModel> models) =>
        [.. models.Select(m => new ModelRow(m, IsDeleting(m.Id), IsActive(m.Id), DeleteErrors.GetValueOrDefault(m.Id)))];

    public bool IsDeleting(string id) => Deleting.Contains(id);

    public bool IsActive(string id) => ActiveModels.Contains(id);

    /// <summary>Whether the Settings row's "Sil…" button is enabled.</summary>
    public bool CanDelete(EngineModel model) => model.Downloaded && !IsDeleting(model.Id) && !IsActive(model.Id);

    /// <summary>Refreshes the catalog from the engine; on error the current list and the error text are kept.</summary>
    public async Task LoadAsync(CancellationToken cancellationToken = default)
    {
        if (Engine is null)
        {
            LoadError = LocalizedMessage.Error(EngineException.NotFound());
            return;
        }
        if (IsLoading) return;
        IsLoading = true;
        try
        {
            var list = await Engine.ModelsAsync(cancellationToken);
            if (list.Count > 0) Models = list;
            LoadError = null;
        }
        catch (OperationCanceledException)
        {
            // The window closed; don't show an error, it will be retried on the next open.
        }
        catch (Exception error)
        {
            LoadError = LocalizedMessage.Error(error);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>
    /// Deletes the model; on success sets the row to the "yüklü değil" state and returns the freed MB.
    /// Returns <c>null</c> when nothing was deleted (not downloaded, busy, in use, or an error, which goes to <see cref="DeleteErrors"/>).
    /// </summary>
    public async Task<int?> DeleteAsync(EngineModel model)
    {
        if (!CanDelete(model)) return null;
        if (Engine is null)
        {
            SetDeleteError(model.Id, LocalizedMessage.Error(EngineException.NotFound()));
            return null;
        }
        Deleting = new HashSet<string>(Deleting) { model.Id };
        SetDeleteError(model.Id, null);
        try
        {
            var freed = await Engine.DeleteModelAsync(model.Id);
            Models = [.. Models.Select(m => m.Id == model.Id ? m with { Downloaded = false, DiskMB = null } : m)];
            return freed;
        }
        catch (Exception error)
        {
            SetDeleteError(model.Id, LocalizedMessage.Error(error));
            return null;
        }
        finally
        {
            var deleting = new HashSet<string>(Deleting);
            deleting.Remove(model.Id);
            Deleting = deleting;
        }
    }

    /// <summary>Re-raises the localized texts after a UI language switch.</summary>
    public void RefreshLocalization()
    {
        OnPropertyChanged(nameof(TotalDiskText));
        OnPropertyChanged(nameof(LoadError));
        OnPropertyChanged(nameof(DeleteErrors));
        // Rows show localized names/sizes computed from the models; let lists rebind.
        OnPropertyChanged(nameof(Models));
        OnPropertyChanged(nameof(TranscriptionModels));
        OnPropertyChanged(nameof(TranslationModels));
        OnPropertyChanged(nameof(DiarizationModels));
        RaiseRows();
    }

    private void SetDeleteError(string id, LocalizedMessage? message)
    {
        var errors = new Dictionary<string, LocalizedMessage>(DeleteErrors);
        if (message is null) errors.Remove(id);
        else errors[id] = message;
        DeleteErrors = errors;
    }
}

/// <summary>One model row in Settings, with every text already localized (rebuilt on a language switch).</summary>
public sealed record ModelRow(EngineModel Model, bool IsDeleting, bool IsActive, LocalizedMessage? Error)
{
    public string Id => Model.Id;
    public string Name { get; } = ModelLabel.Name(Model.Id);
    public string LanguageSummary { get; } = ModelLabel.LanguageSummary(Model);
    public string StorageStatus { get; } = ModelLabel.StorageStatus(Model);
    public string LicenseText { get; } = ModelLabel.LicenseText(Model);
    public bool IsNonCommercial => Model.HasNonCommercialLicense;
    public string? ErrorText { get; } = Error?.Text;

    /// <summary>The "Sil…" button is shown for downloaded models that aren't being deleted, and enabled when not in use.</summary>
    public bool ShowsDeleteButton => Model.Downloaded && !IsDeleting;
    public bool CanDelete => Model.Downloaded && !IsDeleting && !IsActive;

    public string DeleteHelp { get; } = IsActive
        ? Strings.L("Süren bir iş bu modeli kullanıyor; iş bitince silebilirsin.")
        : Strings.L("Modeli diskten sil; tekrar kullanırsan yeniden indirilir.");

    /// <summary>Title of the delete confirmation dialog.</summary>
    public string DeleteConfirmTitle { get; } = Strings.L("%@ modeli silinsin mi?", ModelLabel.Name(Model.Id));

    /// <summary>Body of the delete confirmation dialog.</summary>
    public string DeleteConfirmMessage { get; } =
        Strings.L("%@ boşalır. Tekrar kullanırsan yeniden indirilir.", ModelLabel.SizeText(Model.DiskMB ?? Model.SizeMB));
}
