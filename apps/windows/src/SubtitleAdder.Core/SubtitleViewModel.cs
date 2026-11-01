using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder.Core;

/// <summary>State of the current job, shown in the status bar.</summary>
public abstract record JobStatus
{
    public sealed record Idle : JobStatus;

    public sealed record Working(LocalizedMessage Label, double? Fraction) : JobStatus;

    public sealed record Finished : JobStatus;

    public sealed record Failed(LocalizedMessage Message) : JobStatus;

    public static JobStatus IdleStatus { get; } = new Idle();
    public static JobStatus FinishedStatus { get; } = new Finished();
}

/// <summary>Kind of <see cref="JobStatus"/> for XAML visual states.</summary>
public enum JobStatusKind
{
    Idle,
    Working,
    Finished,
    Failed,
}

public enum JobPurpose
{
    Transcription,
    Translation,
    Diarization,
}

/// <summary>Which text the export uses.</summary>
public enum ExportLanguage
{
    Original,
    Translated,
}

/// <summary>Uncommitted text of the line (or translation) being edited.</summary>
public sealed record TextDraft(Guid Uid, string Text);

/// <summary>"mm:ss.ff" for the line list and timing controls.</summary>
public static class SubtitleTime
{
    public static string Short(double seconds) =>
        string.Create(CultureInfo.InvariantCulture, $"{(int)seconds / 60:00}:{seconds % 60:00.00}");

    /// <summary>Placeholder shown when no line is selected.</summary>
    public const string Placeholder = "––:––.––";
}

/// <summary>
/// The main window's state: settings shown in the toolbar, the job (subtitle generation or translation), the lines
/// and their translations, and the editing commands with undo/redo. Call members on the UI thread; engine events are
/// marshalled through the <see cref="IUiDispatcher"/>. Every property raises <c>PropertyChanged</c>; after a UI
/// language switch <c>PropertyChanged(string.Empty)</c> (all properties) and <see cref="LocalizationChanged"/> are raised.
/// </summary>
public sealed partial class SubtitleViewModel : ObservableObject, IDisposable
{
    public const string DiarizationModelId = "speaker-diarization";
    public const string DefaultModel = "large-v3";
    public const string DefaultTranslationModel = "madlad-3b";
    public static readonly string DefaultTranslationTarget = SubtitleLanguage.English.Code;

    public const string ModelDefaultsKey = "selectedModel";
    public const string LanguageDefaultsKey = "selectedLanguage";
    public const string TranslationModelDefaultsKey = "translationModel";
    public const string TranslationTargetDefaultsKey = "translationTarget";
    public const string DisplayModeDefaultsKey = "subtitleDisplayMode";
    public const string DiarizeDefaultsKey = "diarizeSpeakers";
    public const string SpeakerCountDefaultsKey = "speakerCount";
    public const string OfflineModeDefaultsKey = "offlineMode";

    private readonly ISettingsStore _settings;
    private readonly IUiDispatcher _ui;
    private readonly LanguageState _languageState;

    private string _language;
    private string _model;
    private string _translationTarget;
    private string _translationModel;
    private SubtitleDisplay _displayMode;
    private bool _diarize;
    private int _speakerCount;
    private bool _offlineMode;
    /// <summary>The job a missing-model failure in offline mode can be retried with ("Çevrimdışı modu kapat ve indir").</summary>
    private Action? _offlineRetry;
    private JobStatus _status = JobStatus.IdleStatus;
    private string? _detectedLanguage;
    private IReadOnlyList<Sentence> _sentences = [];
    private IReadOnlyDictionary<Guid, TranslationEntry> _translations = new Dictionary<Guid, TranslationEntry>();
    private string? _translationLanguage;
    private TextDraft? _translationDraft;
    private Guid? _selection;
    private TextDraft? _draft;
    private LocalizedMessage? _modelNotice;
    private string? _video;
    private string? _loadedSubtitlePath;
    private string? _savedSubtitleName;
    private ExportLanguage _exportLanguage;
    private SubtitleFormat _exportFormat = SubtitleFormat.Srt;

    private CancellationTokenSource? _job;
    private Guid _jobId = Guid.NewGuid();  // only the current job may change the state

    public SubtitleViewModel(ModelCatalog catalog, ISettingsStore settings, IUiDispatcher? dispatcher = null, LanguageState? languageState = null)
    {
        Catalog = catalog;
        _settings = settings;
        _ui = dispatcher ?? ImmediateDispatcher.Instance;
        _languageState = languageState ?? LanguageState.Shared;

        var savedLanguage = settings.GetString(LanguageDefaultsKey) ?? "";
        // The old version accepted free text; a code not in the list falls back to automatic.
        _language = SubtitleLanguage.FromCode(savedLanguage) is null ? "" : savedLanguage;
        _model = settings.GetString(ModelDefaultsKey) ?? DefaultModel;
        var savedTarget = settings.GetString(TranslationTargetDefaultsKey) ?? "";
        _translationTarget = SubtitleLanguage.FromCode(savedTarget) is { } target && target != SubtitleLanguage.Automatic
            ? target.Code
            : DefaultTranslationTarget;
        _translationModel = settings.GetString(TranslationModelDefaultsKey) ?? DefaultTranslationModel;
        _displayMode = SubtitleDisplays.Parse(settings.GetString(DisplayModeDefaultsKey)) ?? SubtitleDisplay.Original;
        _diarize = settings.GetBool(DiarizeDefaultsKey) ?? true;
        var savedCount = settings.GetInt(SpeakerCountDefaultsKey) ?? 0;
        _speakerCount = savedCount >= 2 ? savedCount : 0;
        _offlineMode = settings.GetBool(OfflineModeDefaultsKey) ?? true;
        OfflineState.Shared.Enabled = _offlineMode;

        GenerateCommand = new RelayCommand(Generate, () => Video is not null && !IsWorking);
        CancelCommand = new RelayCommand(Cancel, () => IsWorking);
        TranslateCommand = new RelayCommand(Translate, () => Sentences.Count > 0 && !IsWorking);
        UndoCommand = new RelayCommand(Undo, () => CanUndo);
        RedoCommand = new RelayCommand(Redo, () => CanRedo);
        MergeSelectedCommand = new RelayCommand(MergeSelectedWithNext, () => CanMergeSelected);
        DeleteSelectedCommand = new RelayCommand(DeleteSelected, () => Selection is not null);

        UndoStack.Changed += (_, _) => RaiseUndoState();
        Catalog.PropertyChanged += OnCatalogChanged;
        _languageState.LanguageChanged += OnLanguageChanged;

        ReconcileModelSelection();
        ReconcileTranslationTarget();
        ReconcileTranslationModelSelection();
    }

    /// <summary>Shared model catalog; the toolbar and Settings use the same list.</summary>
    public ModelCatalog Catalog { get; }

    /// <summary>The editing history (the window's undo manager on macOS).</summary>
    public UndoStack UndoStack { get; } = new();

    /// <summary>Raised (on the UI thread) after the UI language changed and every property was re-raised.</summary>
    public event EventHandler? LocalizationChanged;

    // MARK: - Persisted choices

    /// <summary>Speech language code; empty means automatic detection. Persistent.</summary>
    public string Language
    {
        get => _language;
        set
        {
            if (!SetProperty(ref _language, value)) return;
            _settings.SetString(LanguageDefaultsKey, value);
            ModelNotice = null;
            OnPropertyChanged(nameof(AvailableModels));
            OnPropertyChanged(nameof(TranslationSource));
            OnPropertyChanged(nameof(TranslationTargetHelp));
            OnPropertyChanged(nameof(SelectedLanguage));
            ReconcileModelSelection();
            ReconcileTranslationTarget();
        }
    }

    /// <summary>The <see cref="Language"/> as a picker item.</summary>
    public SubtitleLanguage SelectedLanguage
    {
        get => SubtitleLanguage.FromCode(_language) ?? SubtitleLanguage.Automatic;
        set => Language = value?.Code ?? "";
    }

    /// <summary>Transcription model id. Persistent.</summary>
    public string Model
    {
        get => _model;
        set
        {
            if (!SetProperty(ref _model, value)) return;
            _settings.SetString(ModelDefaultsKey, value);
            ModelNotice = null;
            OnPropertyChanged(nameof(ModelName));
        }
    }

    /// <summary>The selected model's localized name (the closed toolbar picker shows only this).</summary>
    public string ModelName => ModelLabel.Name(_model);

    /// <summary>Target language used in the translation request; persistent.</summary>
    public string TranslationTarget
    {
        get => _translationTarget;
        set
        {
            if (!SetProperty(ref _translationTarget, value)) return;
            _settings.SetString(TranslationTargetDefaultsKey, value);
        }
    }

    /// <summary>Translation model (chosen in the toolbar or Settings, persistent).</summary>
    public string TranslationModel
    {
        get => _translationModel;
        set
        {
            if (!SetProperty(ref _translationModel, value)) return;
            _settings.SetString(TranslationModelDefaultsKey, value);
            OnPropertyChanged(nameof(TranslationModelName));
        }
    }

    /// <summary>The selected translation model's localized name (shown in the toolbar picker).</summary>
    public string TranslationModelName => ModelLabel.Name(_translationModel);

    /// <summary>Whether the subtitle over the video shows the original or the translation; persistent.</summary>
    public SubtitleDisplay DisplayMode
    {
        get => _displayMode;
        set
        {
            if (!SetProperty(ref _displayMode, value)) return;
            _settings.SetString(DisplayModeDefaultsKey, value.RawValue());
            if (HasTranslations)
            {
                if (value == SubtitleDisplay.Translation) ExportLanguage = ExportLanguage.Translated;
                else if (value == SubtitleDisplay.Original) ExportLanguage = ExportLanguage.Original;
            }
        }
    }

    /// <summary>Separate speakers while transcribing (<c>--diarize</c>); persistent, on by default.</summary>
    public bool Diarize
    {
        get => _diarize;
        set
        {
            if (!SetProperty(ref _diarize, value)) return;
            _settings.SetBool(DiarizeDefaultsKey, value);
        }
    }

    /// <summary>Known speaker count; 0 means automatic. Persistent.</summary>
    public int SpeakerCount
    {
        get => _speakerCount;
        set
        {
            if (!SetProperty(ref _speakerCount, value)) return;
            _settings.SetInt(SpeakerCountDefaultsKey, value);
        }
    }

    /// <summary>Choices of the speaker count picker: 0 (automatic) and 2…6 (same as macOS).</summary>
    public static IReadOnlyList<int> SpeakerCountChoices { get; } = [0, 2, 3, 4, 5, 6];

    /// <summary>
    /// Strict offline mode: the engine never uses the network, so missing models are not downloaded. Persistent, on by
    /// default; applied to every engine launch through <see cref="OfflineState.Shared"/>.
    /// </summary>
    public bool OfflineMode
    {
        get => _offlineMode;
        set
        {
            if (!SetProperty(ref _offlineMode, value)) return;
            OfflineState.Shared.Enabled = value;
            _settings.SetBool(OfflineModeDefaultsKey, value);
        }
    }

    /// <summary>The current failure is a model offline mode kept from downloading; the error bar offers to go online.</summary>
    public bool CanGoOnlineAndRetry => _offlineRetry is not null && _status is JobStatus.Failed;

    /// <summary>Turns offline mode off and runs the blocked job again, which downloads the missing model.</summary>
    public void GoOnlineAndRetry()
    {
        if (!CanGoOnlineAndRetry || _offlineRetry is not { } retry) return;
        _offlineRetry = null;
        OfflineMode = false;
        retry();
    }

    /// <summary>
    /// In offline mode a catalog model that is not on disk can't be downloaded: fails before starting, with a retry that goes
    /// online. Models the catalog doesn't know (catalog not loaded yet) are left to the engine, which reports them too.
    /// </summary>
    private bool BlockedOffline(IEnumerable<string> modelIds, Action retry)
    {
        _offlineRetry = null;
        if (!_offlineMode) return false;
        var missing = modelIds
            .Where(id => Catalog.Models.FirstOrDefault(m => m.Id == id) is { Downloaded: false })
            .ToList();
        if (missing.Count == 0) return false;
        // Set before the status, so views reacting to the status change already see the retry.
        _offlineRetry = retry;
        Status = new JobStatus.Failed(new LocalizedMessage(
            "offline:" + string.Join(",", missing),
            () => L("%@ indirilmemiş. Çevrimdışı mod açıkken model indirilemez.", string.Join(", ", missing.Select(ModelLabel.Name)))));
        OnPropertyChanged(nameof(CanGoOnlineAndRetry));
        return true;
    }

    /// <summary>Label of a speaker count choice ("Otomatik" for 0).</summary>
    public static string SpeakerCountTitle(int count) => count == 0 ? L("Otomatik") : count.ToString(CultureInfo.InvariantCulture);

    // MARK: - Job and content state

    public JobStatus Status
    {
        get => _status;
        private set
        {
            if (!SetProperty(ref _status, value)) return;
            if (value is not JobStatus.Failed) _offlineRetry = null;
            OnPropertyChanged(nameof(CanGoOnlineAndRetry));
            OnPropertyChanged(nameof(IsWorking));
            OnPropertyChanged(nameof(StatusKind));
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(StatusFraction));
            OnPropertyChanged(nameof(IsIndeterminate));
            RaiseCommands();
        }
    }

    public bool IsWorking => _status is JobStatus.Working;

    public JobStatusKind StatusKind => _status switch
    {
        JobStatus.Working => JobStatusKind.Working,
        JobStatus.Finished => JobStatusKind.Finished,
        JobStatus.Failed => JobStatusKind.Failed,
        _ => JobStatusKind.Idle,
    };

    /// <summary>Progress 0…1 while working, when known.</summary>
    public double? StatusFraction => _status is JobStatus.Working working ? working.Fraction : null;

    /// <summary>Working without a known fraction (indeterminate progress ring).</summary>
    public bool IsIndeterminate => _status is JobStatus.Working { Fraction: null };

    /// <summary>The status bar text in the current language.</summary>
    public string StatusText => _status switch
    {
        JobStatus.Working working => working.Label.Text,
        JobStatus.Finished => FinishedSummary,
        JobStatus.Failed failed => failed.Message.Text,
        _ => Video is null || Sentences.Count > 0
            ? L("Hazır")
            : L("Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (Ctrl+Enter)"),
    };

    /// <summary>"N satır · dil: xx" (plus " · çeviri: yy" after a translation and " · kaydedildi: zz" after saving).</summary>
    public string FinishedSummary
    {
        get
        {
            var summary = L("%lld satır · dil: %@", (long)Sentences.Count, DetectedLanguage ?? "?");
            if (TranslationLanguage is { } language) summary += L(" · çeviri: %@", language);
            if (_savedSubtitleName is { } saved) summary += L(" · kaydedildi: %@", saved);
            return summary;
        }
    }

    public string? DetectedLanguage
    {
        get => _detectedLanguage;
        private set
        {
            if (!SetProperty(ref _detectedLanguage, value)) return;
            OnPropertyChanged(nameof(TranslationSource));
            OnPropertyChanged(nameof(TranslationTargetHelp));
            OnPropertyChanged(nameof(StatusText));
        }
    }

    /// <summary>The lines. Replaced as a whole on every change (bind and diff by <see cref="Sentence.Uid"/>).</summary>
    public IReadOnlyList<Sentence> Sentences
    {
        get => _sentences;
        internal set
        {
            if (ReferenceEquals(_sentences, value)) return;
            _sentences = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(HasSentences));
            OnPropertyChanged(nameof(Speakers));
            OnPropertyChanged(nameof(ShowsSpeakers));
            OnPropertyChanged(nameof(StatusText));
            RaiseSelectionDerived();
            RaiseCommands();
        }
    }

    public bool HasSentences => _sentences.Count > 0;

    /// <summary>Speakers in use, in order of first appearance.</summary>
    public IReadOnlyList<int> Speakers => _sentences.Speakers();

    /// <summary>
    /// Speaker controls appear as soon as any line has a speaker (even if diarization found only one),
    /// so more speakers can be added; the dash rule separately needs two.
    /// </summary>
    public bool ShowsSpeakers => Speakers.Count > 0;

    /// <summary>Translations keyed by the line's stable <c>Uid</c>; marked "eski" (stale) when the text is edited.</summary>
    public IReadOnlyDictionary<Guid, TranslationEntry> Translations
    {
        get => _translations;
        private set
        {
            if (ReferenceEquals(_translations, value)) return;
            _translations = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(HasTranslations));
            OnPropertyChanged(nameof(ExportTranslatedTitle));
            OnPropertyChanged(nameof(ExportsTranslation));
            OnPropertyChanged(nameof(ExportFileName));
            if (_translations.Count == 0) ExportLanguage = ExportLanguage.Original;
        }
    }

    public bool HasTranslations => _translations.Count > 0;

    /// <summary>Target language of the last successful translation (for the export label).</summary>
    public string? TranslationLanguage
    {
        get => _translationLanguage;
        private set
        {
            if (!SetProperty(ref _translationLanguage, value)) return;
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(ExportTranslatedTitle));
            OnPropertyChanged(nameof(ExportFileName));
            if (value is null) ExportLanguage = ExportLanguage.Original;
        }
    }

    /// <summary>Uncommitted text of the translation being edited.</summary>
    public TextDraft? TranslationDraft
    {
        get => _translationDraft;
        private set => SetProperty(ref _translationDraft, value);
    }

    /// <summary><c>Uid</c> of the selected line.</summary>
    public Guid? Selection
    {
        get => _selection;
        set
        {
            if (!SetProperty(ref _selection, value)) return;
            RaiseSelectionDerived();
            RaiseCommands();
        }
    }

    /// <summary>Uncommitted text of the line being edited (used while focused).</summary>
    public TextDraft? Draft
    {
        get => _draft;
        private set => SetProperty(ref _draft, value);
    }

    /// <summary>Short notice when the selected model was changed automatically because it didn't match the language filter.</summary>
    public LocalizedMessage? ModelNotice
    {
        get => _modelNotice;
        private set
        {
            if (!SetProperty(ref _modelNotice, value)) return;
            OnPropertyChanged(nameof(ModelNoticeText));
        }
    }

    public string? ModelNoticeText => _modelNotice?.Text;

    /// <summary>Tooltip of the model notice.</summary>
    public static string ModelNoticeHelp => L("Seçili model bu dil için listede yoktu; en doğru uygun model seçildi.");

    /// <summary>Path of the opened video, or <c>null</c>.</summary>
    public string? Video
    {
        get => _video;
        private set
        {
            if (!SetProperty(ref _video, value)) return;
            OnPropertyChanged(nameof(WindowTitle));
            OnPropertyChanged(nameof(ExportFileName));
            OnPropertyChanged(nameof(VideoSubtitlePath));
            OnPropertyChanged(nameof(StatusText));
            RaiseCommands();
        }
    }

    /// <summary>The video file name, or "Subtitle Adder" with no video.</summary>
    public string WindowTitle => _video is null ? "Subtitle Adder" : Path.GetFileName(_video);

    /// <summary>Catalog filtered by language; the toolbar picker shows this.</summary>
    public IReadOnlyList<EngineModel> AvailableModels => Catalog.ModelsFor(_language);

    /// <summary>
    /// Source language that changes with the language code (automatic if empty); passed to the translate command as
    /// <c>--from</c>. With automatic selection, the language detected by the engine is used.
    /// </summary>
    public string? TranslationSource => _language.Length > 0 ? _language : _detectedLanguage;

    /// <summary>Tooltip of the translation target picker.</summary>
    public string TranslationTargetHelp => TranslationSource is null
        ? L("Çeviri hedef dili. Kaynak dil bilinmiyor; NLLB için önce konuşma dilini seç.")
        : L("Çeviri hedef dili; kaynak dille aynı olan seçilemez.");

    /// <summary>Whether a target language can be picked (the source language can't be a target).</summary>
    public bool IsTranslationTargetEnabled(SubtitleLanguage language) => language.Code != TranslationSource;

    // MARK: - Export

    public SubtitleFormat ExportFormat
    {
        get => _exportFormat;
        set
        {
            if (!SetProperty(ref _exportFormat, value)) return;
            OnPropertyChanged(nameof(ExportFileName));
            OnPropertyChanged(nameof(VideoSubtitlePath));
        }
    }

    /// <summary>Original or translated export; back to original whenever the video or the translation goes away.</summary>
    public ExportLanguage ExportLanguage
    {
        get => _exportLanguage;
        set
        {
            if (!SetProperty(ref _exportLanguage, value)) return;
            OnPropertyChanged(nameof(ExportsTranslation));
            OnPropertyChanged(nameof(ExportFileName));
            if (HasTranslations)
            {
                if (value == ExportLanguage.Translated && _displayMode == SubtitleDisplay.Original)
                    DisplayMode = SubtitleDisplay.Translation;
                else if (value == ExportLanguage.Original && _displayMode == SubtitleDisplay.Translation)
                    DisplayMode = SubtitleDisplay.Original;
            }
        }
    }

    /// <summary>"Çeviri (es)" once translated, otherwise "Çeviri".</summary>
    public string ExportTranslatedTitle => _translationLanguage is { } language ? L("Çeviri (%@)", language) : L("Çeviri");

    /// <summary>Suggested file name for the export, e.g. "clip.es.srt".</summary>
    public string ExportFileName
    {
        get
        {
            var sourcePath = _video ?? _loadedSubtitlePath;
            var name = sourcePath is null ? "subtitles" : Path.GetFileNameWithoutExtension(sourcePath);
            var suffix = ExportsTranslation ? "." + (_translationLanguage ?? "translated") : "";
            return $"{name}{suffix}.{_exportFormat.RawValue()}";
        }
    }

    /// <summary>Subtitle file path in the video's folder with the video's exact base name, e.g. "C:\videos\clip.srt".</summary>
    public string? VideoSubtitlePath => (_video ?? _loadedSubtitlePath) is { } basePath
        ? Path.ChangeExtension(basePath, _exportFormat.RawValue())
        : null;

    /// <summary>Whether the export uses the translation (chosen and available).</summary>
    public bool ExportsTranslation => _exportLanguage == ExportLanguage.Translated && HasTranslations;

    /// <summary>Whether "Dışa aktar…" is enabled.</summary>
    public bool CanExport => HasSentences && !IsWorking;

    /// <summary>Whether "Altyazıyı kaydet" is enabled (needs a video or subtitle path and exportable lines).</summary>
    public bool CanSaveSubtitle => CanExport && (_video is not null || _loadedSubtitlePath is not null);

    /// <summary>Renders the export with the chosen format and language; <c>null</c> on error (shown in the status).</summary>
    public Task<string?> RenderExportAsync() => RenderAsync(_exportFormat, ExportsTranslation);

    /// <summary>
    /// Renders and writes the subtitle file directly to the video's folder with the video's exact base name
    /// (<see cref="VideoSubtitlePath"/>). Returns the written path, or <c>null</c> if nothing was written.
    /// </summary>
    public async Task<string?> SaveSubtitleToVideoFolderAsync()
    {
        if (VideoSubtitlePath is not { } path) return null;
        FinishEditing();
        var text = await RenderExportAsync();
        if (text is null) return null;
        await File.WriteAllTextAsync(path, text, new System.Text.UTF8Encoding(false));
        _savedSubtitleName = Path.GetFileName(path);
        if (_status is not JobStatus.Finished) Status = JobStatus.FinishedStatus;
        else OnPropertyChanged(nameof(StatusText));
        return path;
    }

    // MARK: - Commands

    public IRelayCommand GenerateCommand { get; }
    public IRelayCommand CancelCommand { get; }
    public IRelayCommand TranslateCommand { get; }
    public IRelayCommand UndoCommand { get; }
    public IRelayCommand RedoCommand { get; }
    public IRelayCommand MergeSelectedCommand { get; }
    public IRelayCommand DeleteSelectedCommand { get; }

    /// <summary>The running job; completes when it has finished applying its last state change (for tests and shutdown).</summary>
    public Task CurrentJob { get; private set; } = Task.CompletedTask;

    // MARK: - Reconcile

    /// <summary>If the selected model isn't in the filtered list, switches to the most accurate (or, on a tie, the faster) model.</summary>
    public void ReconcileModelSelection()
    {
        var available = AvailableModels;
        if (available.Count == 0 || available.Any(m => m.Id == _model)) return;
        var best = MostAccurate(available);
        Model = best.Id;
        if (SubtitleLanguage.FromCode(_language) is { } language)
        {
            var modelId = best.Id;
            ModelNotice = new LocalizedMessage($"notice:{language.Code}:{modelId}",
                () => L("%@ için %@ seçildi.", language.ShortName, ModelLabel.Name(modelId)));
        }
    }

    /// <summary>If the translation model in Settings isn't in the catalog (an old saved value), falls back to the most accurate model.</summary>
    public void ReconcileTranslationModelSelection()
    {
        var available = Catalog.TranslationModels;
        if (available.Count == 0 || available.Any(m => m.Id == _translationModel)) return;
        TranslationModel = MostAccurate(available).Id;
    }

    /// <summary>The target language can't be the same as the source language; if it is, switches to another target.</summary>
    public void ReconcileTranslationTarget()
    {
        if (TranslationSource is not { } source || source != _translationTarget) return;
        if (SubtitleLanguage.TranslationTargets.FirstOrDefault(l => l.Code != source) is not { } next) return;
        TranslationTarget = next.Code;
    }

    /// <summary>Highest (accuracy, speed); the first one wins a tie (like Swift's <c>max(by:)</c>).</summary>
    private static EngineModel MostAccurate(IReadOnlyList<EngineModel> models)
    {
        var best = models[0];
        foreach (var model in models.Skip(1))
        {
            if ((model.Accuracy, model.Speed).CompareTo((best.Accuracy, best.Speed)) > 0) best = model;
        }
        return best;
    }

    // MARK: - Video and jobs

    /// <summary>When a new video is chosen, clears the old video's job, subtitles and undo stack (and auto-loads a sidecar subtitle if present).</summary>
    public void OpenVideo(string? path)
    {
        if (path == _video) return;
        StopJob();
        Catalog.ActiveModels = new HashSet<string>();
        Sentences = [];
        Translations = new Dictionary<Guid, TranslationEntry>();
        TranslationDraft = null;
        TranslationLanguage = null;
        Selection = null;
        DetectedLanguage = null;
        Draft = null;
        _loadedSubtitlePath = null;
        _savedSubtitleName = null;
        Status = JobStatus.IdleStatus;
        UndoStack.Clear();
        Video = path;
        ExportLanguage = ExportLanguage.Original;

        if (SubtitleParser.FindSidecarSubtitle(path) is { } sidecar)
        {
            TryLoadSubtitleFile(sidecar);
        }
    }

    /// <summary>Loads an existing subtitle file (<c>.srt</c>, <c>.vtt</c>, <c>.ass</c>, <c>.json</c>) directly without audio extraction.</summary>
    public bool TryLoadSubtitleFile(string path)
    {
        try
        {
            var parsed = SubtitleParser.ParseFile(path);
            if (parsed.Count == 0)
            {
                Status = new JobStatus.Failed(LocalizedMessage.Key("Altyazı dosyasında geçerli satır bulunamadı."));
                return false;
            }
            StopJob();
            Catalog.ActiveModels = new HashSet<string>();
            _loadedSubtitlePath = path;
            _savedSubtitleName = null;
            Translations = new Dictionary<Guid, TranslationEntry>();
            TranslationDraft = null;
            TranslationLanguage = null;
            Selection = null;
            Draft = null;
            UndoStack.Clear();
            DisplayMode = SubtitleDisplay.Original;
            ExportLanguage = ExportLanguage.Original;
            Sentences = [.. parsed];
            Status = JobStatus.FinishedStatus;
            OnPropertyChanged(nameof(ExportFileName));
            OnPropertyChanged(nameof(VideoSubtitlePath));
            RaiseCommands();
            return true;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            Status = new JobStatus.Failed(LocalizedMessage.Error(error));
            return false;
        }
    }

    public void Generate()
    {
        if (_video is not { } video) return;
        if (Catalog.Engine is not { } engine)
        {
            Status = new JobStatus.Failed(LocalizedMessage.Error(EngineException.NotFound()));
            return;
        }
        string[] needed = _diarize ? [_model, DiarizationModelId] : [_model];
        if (BlockedOffline(needed, Generate)) return;
        Sentences = [];
        Translations = new Dictionary<Guid, TranslationEntry>();
        TranslationDraft = null;
        TranslationLanguage = null;
        Selection = null;
        Draft = null;
        _savedSubtitleName = null;
        UndoStack.Clear();
        DetectedLanguage = null;
        ModelNotice = null;
        Status = new JobStatus.Working(LocalizedMessage.Key("Başlıyor…"), null);
        var outDir = Path.Combine(Path.GetTempPath(), "SubtitleAdder");
        var options = new TranscribeOptions
        {
            Language = _language.Length == 0 ? null : _language,
            Model = _model,
            Diarize = _diarize,
            Speakers = _speakerCount >= 2 ? _speakerCount : null,
        };
        var runningModel = _model;
        // With diarization, `downloading_model` can run twice back to back; the tracker tells which model it is.
        var tracker = new DownloadPhaseTracker(
            transcriptionDownloaded: Catalog.Models.FirstOrDefault(m => m.Id == runningModel)?.Downloaded ?? true,
            diarize: _diarize,
            diarizationDownloaded: Catalog.DiarizationModels.FirstOrDefault()?.Downloaded ?? true);
        var running = new HashSet<string> { runningModel };
        if (_diarize) running.Add(DiarizationModelId);
        StartJob(running, token => engine.Transcribe(video, outDir, options, token), engineEvent =>
        {
            switch (engineEvent)
            {
                case EngineEvent.Stage stage:
                    var download = tracker.Update(stage.Kind, stage.Fraction);
                    var purpose = download == DownloadPhaseTracker.Kind.Diarization ? JobPurpose.Diarization : JobPurpose.Transcription;
                    Status = new JobStatus.Working(Label(stage.Kind, stage.Fraction, purpose), stage.Fraction);
                    break;
                case EngineEvent.Result result:
                    Sentences = [.. result.Value.Sentences];
                    DetectedLanguage = result.Value.Language;
                    ReconcileTranslationTarget();
                    // The app doesn't use the extracted WAV; delete the temporary file.
                    DeleteQuietly(result.Value.Audio);
                    break;
                case EngineEvent.Error error:
                    return new JobStatus.Failed(LocalizedMessage.Verbatim(error.Message));
            }
            return null;
        });
    }

    /// <summary>Stops the running subtitle or translation job; the state goes back to idle.</summary>
    public void Cancel()
    {
        StopJob();
        Catalog.ActiveModels = new HashSet<string>();
        Status = JobStatus.IdleStatus;
    }

    /// <summary>Renders the original or translated lines in the engine; <c>null</c> on error (shown in the status) or without an engine.</summary>
    public async Task<string?> RenderAsync(SubtitleFormat format, bool translated = false)
    {
        if (Catalog.Engine is not { } engine) return null;
        try
        {
            var list = translated ? ExportSentences(true) : Sentences;
            return await engine.RenderAsync(list, format);
        }
        catch (Exception error)
        {
            Status = new JobStatus.Failed(LocalizedMessage.Error(error));
            return null;
        }
    }

    public static LocalizedMessage Label(EngineStage stage, double? fraction, JobPurpose purpose)
    {
        var fractionText = fraction?.ToString("R", CultureInfo.InvariantCulture) ?? "-";
        return new LocalizedMessage($"stage:{stage}:{fractionText}:{purpose}", () => LabelText(stage, fraction, purpose));
    }

    internal static string LabelText(EngineStage stage, double? fraction, JobPurpose purpose)
    {
        static long Percent(double value) => (long)Math.Round(Math.Clamp(value, 0, 1) * 100, MidpointRounding.AwayFromZero);
        switch (stage)
        {
            case EngineStage.ExtractingAudio:
                return L("Ses ayrılıyor…");
            case EngineStage.DownloadingModel:
                var prefix = purpose switch
                {
                    JobPurpose.Translation => L("Çeviri modeli indiriliyor…"),
                    JobPurpose.Diarization => L("Konuşmacı ayırma modeli indiriliyor…"),
                    _ => L("Model indiriliyor…"),
                };
                return fraction is { } f ? $"{prefix} {L("%lld%%", Percent(f))}" : prefix;
            case EngineStage.LoadingModel:
                return purpose == JobPurpose.Translation ? L("Çeviri modeli yükleniyor…") : L("Model yükleniyor…");
            case EngineStage.Transcribing:
                return L("Konuşma metne çevriliyor…");
            case EngineStage.Diarizing:
                return fraction is { } d ? L("Konuşmacılar ayrılıyor… %lld%%", Percent(d)) : L("Konuşmacılar ayrılıyor…");
            case EngineStage.Segmenting:
                return L("Cümleler çıkarılıyor…");
            case EngineStage.Translating:
                return fraction is { } t ? L("Çevriliyor… %lld%%", Percent(t)) : L("Çevriliyor…");
            default:
                return L("Çalışıyor…");
        }
    }

    /// <summary>
    /// Runs an engine stream as the current job. <paramref name="apply"/> runs on the UI thread for each event, only
    /// while this job is still current; it returns a failure status for an engine error event (then the job doesn't
    /// finish as successful). When the stream ends the status becomes finished and the catalog is reloaded (so a
    /// downloaded model shows as "hazır"); however it ends, the running models become deletable again.
    /// </summary>
    private void StartJob(
        IReadOnlySet<string> running,
        Func<CancellationToken, IAsyncEnumerable<EngineEvent>> stream,
        Func<EngineEvent, JobStatus?> apply)
    {
        StopJob();
        var cancellation = new CancellationTokenSource();
        var token = cancellation.Token;
        _job = cancellation;
        var id = Guid.NewGuid();
        _jobId = id;
        Catalog.ActiveModels = running;
        bool IsCurrent() => _jobId == id && !token.IsCancellationRequested;
        // Created here (the engine only starts when it is enumerated) so the request is recorded synchronously.
        var events = stream(token);

        CurrentJob = Task.Run(async () =>
        {
            try
            {
                JobStatus? failure = null;
                await foreach (var engineEvent in events.WithCancellation(token).ConfigureAwait(false))
                {
                    var current = false;
                    await _ui.InvokeAsync(() =>
                    {
                        if (!(current = IsCurrent())) return;
                        if (apply(engineEvent) is { } failed)
                        {
                            failure = failed;
                            Status = failed;
                        }
                    }).ConfigureAwait(false);
                    if (!current) return;
                }
                var finished = false;
                await _ui.InvokeAsync(() =>
                {
                    if (!(finished = IsCurrent())) return;
                    if (failure is null) Status = JobStatus.FinishedStatus;
                }).ConfigureAwait(false);
                if (finished) await _ui.InvokeAsync(() => Catalog.LoadAsync()).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                await _ui.InvokeAsync(() =>
                {
                    if (_jobId == id) Status = JobStatus.IdleStatus;
                }).ConfigureAwait(false);
            }
            catch (Exception error)
            {
                await _ui.InvokeAsync(() =>
                {
                    if (_jobId == id) Status = new JobStatus.Failed(LocalizedMessage.Error(error));
                }).ConfigureAwait(false);
            }
            finally
            {
                await _ui.InvokeAsync(() =>
                {
                    if (_jobId == id) Catalog.ActiveModels = new HashSet<string>();
                }).ConfigureAwait(false);
                cancellation.Dispose();
            }
        });
    }

    /// <summary>Cancels the running job (if any) and makes sure none of its late events change the state.</summary>
    private void StopJob()
    {
        // Retire the id before cancelling: the cancelled job's handlers may run on another thread
        // right away and must already see that they are stale.
        var job = _job;
        _job = null;
        _jobId = Guid.NewGuid();
        try
        {
            job?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // The job already finished.
        }
    }

    private static void DeleteQuietly(string path)
    {
        try
        {
            if (path.Length > 0) File.Delete(path);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException or NotSupportedException)
        {
            // A leftover temp file is harmless.
        }
    }

    // MARK: - Change notification

    private void OnCatalogChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(ModelCatalog.Models)) return;
        _ui.Post(() =>
        {
            OnPropertyChanged(nameof(AvailableModels));
            ReconcileModelSelection();
            ReconcileTranslationModelSelection();
        });
    }

    private void OnLanguageChanged(object? sender, UILanguage language) => _ui.Post(RefreshLocalization);

    /// <summary>Re-raises every property (all localized texts) of the view model and the catalog.</summary>
    public void RefreshLocalization()
    {
        OnPropertyChanged(string.Empty);
        Catalog.RefreshLocalization();
        RaiseUndoState();
        LocalizationChanged?.Invoke(this, EventArgs.Empty);
    }

    private void RaiseSelectionDerived()
    {
        OnPropertyChanged(nameof(SelectedSentence));
        OnPropertyChanged(nameof(CanMergeSelected));
        OnPropertyChanged(nameof(HasSelection));
        OnPropertyChanged(nameof(SelectedStartText));
        OnPropertyChanged(nameof(SelectedEndText));
    }

    private void RaiseCommands()
    {
        OnPropertyChanged(nameof(CanExport));
        OnPropertyChanged(nameof(CanSaveSubtitle));
        GenerateCommand?.NotifyCanExecuteChanged();
        CancelCommand?.NotifyCanExecuteChanged();
        TranslateCommand?.NotifyCanExecuteChanged();
        MergeSelectedCommand?.NotifyCanExecuteChanged();
        DeleteSelectedCommand?.NotifyCanExecuteChanged();
    }

    private void RaiseUndoState()
    {
        OnPropertyChanged(nameof(CanUndo));
        OnPropertyChanged(nameof(CanRedo));
        OnPropertyChanged(nameof(UndoTitle));
        OnPropertyChanged(nameof(RedoTitle));
        UndoCommand?.NotifyCanExecuteChanged();
        RedoCommand?.NotifyCanExecuteChanged();
    }

    public void Dispose()
    {
        StopJob();
        Catalog.PropertyChanged -= OnCatalogChanged;
        _languageState.LanguageChanged -= OnLanguageChanged;
    }
}
