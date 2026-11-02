using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Text;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.Windows.Storage.Pickers;
using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.System;
using Windows.UI.Core;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

/// <summary>
/// The main window (port of ContentView + PlayerView + SentenceEditorView). Only view wiring lives here:
/// every decision is made by <see cref="SubtitleViewModel"/>; this class maps its state onto controls and forwards input.
/// </summary>
public sealed partial class MainWindow : Window
{
    private static readonly string[] VideoTypes =
    [
        ".mp4", ".mov", ".m4v", ".mkv", ".avi", ".webm", ".wmv", ".mpg", ".mpeg", ".ts", ".mts", ".m2ts", ".3gp", ".flv",
        ".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg", ".opus", ".wma",
    ];

    private readonly PlayerController _player;
    private readonly Dictionary<(int Speaker, bool Dark), Brush> _speakerBrushes = [];
    private bool _syncing;
    private enum WorkspaceTab { Editor, Playlist, Platform }

    private WorkspaceTab _activeTab = WorkspaceTab.Editor;
    private bool _isPlaylistTabActive => _activeTab == WorkspaceTab.Playlist;
    private bool _isExplorerOpen;
    private double _lastProgressRecordedTime = -1;

    public FileExplorerViewModel ExplorerVm { get; } = App.Current.ExplorerViewModel;
    public VideoPlatformViewModel PlatformVm { get; } = App.Current.PlatformViewModel;
    private bool _isSideBySide;
    private bool _isTheaterMode;
    private bool _isFullScreen;
    private bool _pointerOverPlayer;
    private bool _isEditorHidden;
    private bool _pointerOverDisplayPicker;
    private bool _hudIdleHidden;
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _hudHideTimer;
    private CancellationTokenSource? _videoOpenedDebounceCts;
    private LocalizedMessage? _shownError;
    private Func<string>? _saveError;   // rebuilt on a language switch

    public MainWindow()
    {
        InitializeComponent();
        TranslationPicker.ModelChosen += () => TranslationModelFlyout.Hide();
        TranslationPicker.SettingsRequested += () =>
        {
            TranslationModelFlyout.Hide();
            App.Current.ShowSettings("general");
        };
        WindowChrome.Setup(this, Root, AppTitleBar, minWidth: 760, minHeight: 580, defaultWidth: 1240, defaultHeight: 840, sizeKey: "mainWindow");

        _player = new PlayerController(DispatcherQueue);
        PlayerElement.SetMediaPlayer(_player.Player);
        _player.ActiveChanged += OnActiveChanged;
        _player.VideoSizeChanged += UpdateOverlay;
        _player.Ticked += () =>
        {
            SplitButton.IsEnabled = Vm.CanSplitSelected(_player.CurrentTime);
            PreviewControls.Synchronize(_player.CurrentTime, _player.Duration, _player.IsPlaying, _player.IsMuted, _player.Volume, _player.BufferedRatio);
            if (_player.IsPlaying && _player.Duration > 0 && Math.Abs(_player.CurrentTime - _lastProgressRecordedTime) >= 5.0)
            {
                _lastProgressRecordedTime = _player.CurrentTime;
                PlatformVm.UpdateActiveVideoProgress(_player.CurrentTime, _player.Duration);
            }
            UpdatePreviewControlVisibility();
        };
        PreviewControls.SeekRequested += _player.Seek;
        PreviewControls.PlayPauseRequested += _player.TogglePlayback;
        PreviewControls.MuteToggleRequested += _player.ToggleMute;
        PreviewControls.VolumeRequested += _player.SetVolume;
        PreviewControls.InteractionChanged += UpdatePreviewControlVisibility;
        _player.PlayEmptyRequested += () =>
        {
            var candidate = Vm.Video
                ?? FileExplorerPane.ActiveVideoPath
                ?? ExplorerVm.Files.FirstOrDefault()?.FullPath
                ?? Vm.Playlist.FirstOrDefault()?.VideoPath
                ?? PlatformVm.FeedItems.FirstOrDefault()?.VideoPath;
            if (!string.IsNullOrWhiteSpace(candidate) && File.Exists(candidate))
            {
                if (!string.Equals(Vm.Video, candidate, StringComparison.OrdinalIgnoreCase))
                {
                    Vm.OpenVideo(candidate);
                }
                _player.Load(candidate, autoPlay: true);
            }
        };
        _player.Failed += message => ShowSaveError(() => L("Video oynatılamadı: %@", message));
        Splitter.Target = PlayerRow;
        Splitter.Other = EditorRow;
        Splitter.TargetColumn = PlayerCol;
        Splitter.OtherColumn = EditorCol;
        _isSideBySide = App.Current.Settings.GetBool("sideBySideLayout") ?? false;
        _isEditorHidden = App.Current.Settings.GetBool("hideSubtitlePanel") ?? false;
        _isExplorerOpen = App.Current.Settings.GetBool("showFileExplorer") ?? true;

        FileExplorerPane.AttachViewModel(ExplorerVm);
        FileExplorerPane.FileSelected += path =>
        {
            SwitchMode(WorkspaceTab.Editor);
            Vm.OpenVideo(path);
        };
        FileExplorerPane.AddToPlaylistRequested += path =>
        {
            Vm.AddToPlaylist([path]);
            SwitchMode(WorkspaceTab.Playlist);
        };

        ProfileStatusButton.OpenPlatformHubRequested += () => SwitchMode(WorkspaceTab.Platform);
        PlatformHubView.VideoSelected += path =>
        {
            SwitchMode(WorkspaceTab.Editor);
            Vm.OpenVideo(path);
        };
        _hudHideTimer = DispatcherQueue.CreateTimer();
        _hudHideTimer.Interval = TimeSpan.FromMilliseconds(2500);
        _hudHideTimer.IsRepeating = false;
        _hudHideTimer.Tick += (_, _) =>
        {
            if (_player.IsPlaying && !PreviewControls.IsInteracting && !_pointerOverDisplayPicker)
            {
                _hudIdleHidden = true;
                UpdatePreviewControlVisibility();
            }
        };
        ApplyWorkspaceLayout();
        PlatformHubView.SetActiveTab(_activeTab == WorkspaceTab.Platform);

        // Buttons follow the commands' CanExecute (wired here rather than through Command bindings).
        foreach (var (command, button) in new (CommunityToolkit.Mvvm.Input.IRelayCommand, Button)[]
                 {
                     (Vm.GenerateCommand, GenerateButton),
                     (Vm.StartPlaylistCommand, StartPlaylistButton),
                     (Vm.CancelCommand, CancelButton), (Vm.TranslateCommand, TranslateButton),
                     (Vm.UndoCommand, UndoButton), (Vm.RedoCommand, RedoButton),
                 })
        {
            button.IsEnabled = command.CanExecute(null);
            command.CanExecuteChanged += (_, _) => button.IsEnabled = command.CanExecute(null);
        }
        GenerateAndTranslateButton.IsEnabled = !Vm.IsWorking;
        Vm.GenerateAndTranslateCommand.CanExecuteChanged += (_, _) => GenerateAndTranslateButton.IsEnabled = !Vm.IsWorking;
        Vm.PropertyChanged += OnViewModelChanged;
        Vm.LocalizationChanged += (_, _) => RefreshAll();
        Vm.Catalog.PropertyChanged += OnCatalogChanged;
        App.Current.Appearance.PropertyChanged += OnAppearanceChanged;
        ErrorBar.Closed += (_, _) => _saveError = null;
        Root.ActualThemeChanged += (_, _) =>
        {
            _speakerBrushes.Clear();
            SyncRows();
        };
        Root.SizeChanged += (_, _) => UpdateCompactLabels();
        Root.Loaded += async (_, _) => await Vm.Catalog.LoadAsync();
        Closed += (_, _) =>
        {
            Vm.PropertyChanged -= OnViewModelChanged;
            Vm.Catalog.PropertyChanged -= OnCatalogChanged;
            App.Current.Appearance.PropertyChanged -= OnAppearanceChanged;
            // Detach the XAML surface first so window destruction never waits on Media Foundation teardown.
            _hudHideTimer.Stop();
            PlayerElement.SetMediaPlayer(null);
            _player.Dispose();
        };

        RefreshAll();
        if (Vm.Video is not null) OnVideoChanged();
    }

    public SubtitleViewModel Vm { get; } = App.Current.ViewModel;

    /// <summary>The line list, kept per <see cref="Sentence.Uid"/> (see <see cref="SentenceRow"/>).</summary>
    public ObservableCollection<SentenceRow> Rows { get; } = [];

    /// <summary>The batch playlist queue rows, kept per <see cref="PlaylistItem.Id"/>.</summary>
    public ObservableCollection<PlaylistRow> PlaylistRows { get; } = [];

    // MARK: - View model -> controls

    private void OnViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(SubtitleViewModel.Video):
                OnVideoChanged();
                break;
            case nameof(SubtitleViewModel.Playlist):
            case nameof(SubtitleViewModel.IsPlaylistRunning):
                SyncPlaylistRows();
                break;
            case nameof(SubtitleViewModel.Sentences):
                _player.SetSentences(Vm.Sentences);
                DropZone.Visibility = Vm.Video is null && !Vm.HasSentences ? Visibility.Visible : Visibility.Collapsed;
                WorkArea.Visibility = Vm.Video is not null || Vm.HasSentences ? Visibility.Visible : Visibility.Collapsed;
                UpdateTabActionVisibility();
                SyncRows();
                UpdateOverlay();
                break;
            case nameof(SubtitleViewModel.Translations):
            case nameof(SubtitleViewModel.TranslationLanguage):
                SyncRows();
                UpdateOverlay();
                UpdatePickers();
                break;
            case nameof(SubtitleViewModel.Selection):
                SyncSelection();
                UpdateTimingEnabled();
                if (Vm.SelectedSentence is { } selected) _player.Seek(selected.Start);
                SplitButton.IsEnabled = Vm.CanSplitSelected(_player.CurrentTime);
                break;
            case nameof(SubtitleViewModel.SelectedUids):
                SyncSelection();
                break;
            case nameof(SubtitleViewModel.Status):
            case nameof(SubtitleViewModel.IsAsyncTranslating):
            case nameof(SubtitleViewModel.AsyncTranslationFraction):
                UpdateStatus();
                break;
            case nameof(SubtitleViewModel.DisplayMode):
                UpdatePickers();
                UpdateOverlay();
                break;
            case nameof(SubtitleViewModel.CanUndo) or nameof(SubtitleViewModel.UndoTitle) or nameof(SubtitleViewModel.RedoTitle):
                UpdateUndoTips();
                break;
            case nameof(SubtitleViewModel.Draft) or nameof(SubtitleViewModel.TranslationDraft):
                break;
            case "" or null:
                RefreshAll();
                break;
            default:
                UpdatePickers();
                UpdateSpeakerButton();
                break;
        }
    }

    private void OnCatalogChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ModelCatalog.Models)) UpdatePickers();
    }

    private void OnAppearanceChanged(object? sender, PropertyChangedEventArgs e) => UpdateOverlay();

    private void RefreshAll()
    {
        Title = Vm.WindowTitle;
        TitleFile.Text = Vm.Video is null ? "" : "— " + Vm.WindowTitle;
        DropZone.Visibility = Vm.Video is null && !Vm.HasSentences ? Visibility.Visible : Visibility.Collapsed;
        WorkArea.Visibility = Vm.Video is not null || Vm.HasSentences ? Visibility.Visible : Visibility.Collapsed;
        UpdatePickers(refill: true);
        UpdateSpeakerButton();
        UpdateStatus();
        if (_saveError is { } saveError && ErrorBar.IsOpen) ErrorBar.Message = saveError();
        UpdateUndoTips();
        UpdateTimingTips();
        UpdateTimingEnabled();
        SyncRows();
        SyncPlaylistRows();
        UpdateOverlay();
        UpdateCompactLabels();
    }

    private void OnVideoChanged()
    {
        var currentVideo = Vm.Video;
        _saveError = null;
        _player.Load(currentVideo, autoPlay: Vm.AutoPlayVideo);
        FileExplorerPane.ActiveVideoPath = currentVideo;
        Title = Vm.WindowTitle;
        TitleFile.Text = currentVideo is null ? "" : "— " + Vm.WindowTitle;
        DropZone.Visibility = currentVideo is null && !Vm.HasSentences ? Visibility.Visible : Visibility.Collapsed;
        WorkArea.Visibility = currentVideo is not null || Vm.HasSentences ? Visibility.Visible : Visibility.Collapsed;
        UpdatePickers();
        UpdateStatus();
        SyncRows();
        UpdateOverlay();
        _lastProgressRecordedTime = -1;
        _ = ExplorerVm.SyncToVideoPathAsync(currentVideo);
        PlatformVm.SetActiveVideoImmediate(currentVideo);

        _videoOpenedDebounceCts?.Cancel();
        _videoOpenedDebounceCts?.Dispose();
        _videoOpenedDebounceCts = null;
        if (!string.IsNullOrWhiteSpace(currentVideo) && !Vm.IsPlaylistRunning)
        {
            var cts = new CancellationTokenSource();
            _videoOpenedDebounceCts = cts;
            var token = cts.Token;
            _ = Task.Run(async () =>
            {
                try
                {
                    await Task.Delay(150, token).ConfigureAwait(false);
                    if (token.IsCancellationRequested) return;
                    PlatformVm.RecordWatchOnly(currentVideo);
                    DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
                    {
                        if (token.IsCancellationRequested) return;
                        if (string.Equals(Vm.Video, currentVideo, StringComparison.OrdinalIgnoreCase))
                        {
                            PlatformVm.SetActiveVideoImmediate(currentVideo);
                            if (_activeTab == WorkspaceTab.Platform)
                            {
                                PlatformVm.RefreshFeed();
                            }
                            else
                            {
                                PlatformHubView.MarkDirtyWhileHidden();
                            }
                        }
                    });
                }
                catch (OperationCanceledException)
                {
                }
            }, token);
        }
    }

    private void UpdateStatus()
    {
        UpdateTabActionVisibility();
        var kind = Vm.StatusKind;
        var showWorking = kind == JobStatusKind.Working;
        StatusSpinner.IsActive = showWorking;
        StatusSpinner.Visibility = showWorking ? Visibility.Visible : Visibility.Collapsed;
        Progress.Visibility = showWorking ? Visibility.Visible : Visibility.Collapsed;
        Progress.IsIndeterminate = Vm.IsIndeterminate;
        var statusFrac = Vm.StatusFraction ?? 0.0;
        Progress.Value = double.IsFinite(statusFrac) ? Math.Clamp(statusFrac, 0.0, 1.0) : 0.0;
        DoneIcon.Visibility = kind == JobStatusKind.Finished ? Visibility.Visible : Visibility.Collapsed;
        FailIcon.Visibility = kind == JobStatusKind.Failed ? Visibility.Visible : Visibility.Collapsed;
        EmptyTitle.Text = Vm.IsWorking ? L("Altyazı üretiliyor…") : L("Henüz altyazı yok");
        EmptyDetail.Visibility = Vm.IsWorking ? Visibility.Collapsed : Visibility.Visible;

        // Failures also go to the InfoBar; it reopens for a new failure, not for the one the user closed.
        if (Vm.Status is JobStatus.Failed failed)
        {
            if (failed.Message != _shownError || ErrorBar.IsOpen)
            {
                ErrorBar.Message = failed.Message.Text;
                ErrorBar.IsOpen = true;
            }
            _shownError = failed.Message;
            _saveError = null;
        }
        else if (_saveError is null)
        {
            ErrorBar.IsOpen = false;
            _shownError = null;
        }
        GoOnlineButton.Visibility = Vm.CanGoOnlineAndRetry && _saveError is null ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>A model offline mode kept from downloading: turn offline mode off and run the job again.</summary>
    private void GoOnlineButton_Click(object sender, RoutedEventArgs e) => Vm.GoOnlineAndRetry();

    private void ShowSaveError(Func<string> message)
    {
        _saveError = message;
        GoOnlineButton.Visibility = Visibility.Collapsed;
        ErrorBar.Message = message();
        ErrorBar.IsOpen = true;
    }

    private void UpdateUndoTips()
    {
        ToolTipService.SetToolTip(UndoButton, Vm.UndoTitle + " (Ctrl+Z)");
        AutomationProperties.SetName(UndoButton, Vm.UndoTitle);
        ToolTipService.SetToolTip(RedoButton, Vm.RedoTitle + " (Ctrl+Y)");
        AutomationProperties.SetName(RedoButton, Vm.RedoTitle);
    }

    private void UpdateTimingTips()
    {
        foreach (var (edge, minus, plus, now) in new[]
                 {
                     (TimingEdge.Start, StartMinus, StartPlus, StartNow),
                     (TimingEdge.End, EndMinus, EndPlus, EndNow),
                 })
        {
            var title = SubtitleViewModel.EdgeTitle(edge);
            SetTip(minus, L("%@ 0,1 sn geri", title));
            SetTip(plus, L("%@ 0,1 sn ileri", title));
            SetTip(now, L("%@ zamanını oynatma konumuna ayarla", title));
        }

        static void SetTip(Button button, string text)
        {
            ToolTipService.SetToolTip(button, text);
            AutomationProperties.SetName(button, text);
        }
    }

    private void UpdateTimingEnabled()
    {
        foreach (var button in new[] { StartMinus, StartPlus, StartNow, EndMinus, EndPlus, EndNow })
            button.IsEnabled = Vm.HasSelection;
    }

    private void UpdateSpeakerButton()
    {
        // Filled "people" glyph while speaker separation is on (person.2.fill on macOS).
        SpeakerIcon.Glyph = Vm.Diarize ? "" : "";
        SpeakerIcon.Opacity = Vm.Diarize ? 1 : 0.6;
    }

    /// <summary>Adapts toolbar layout, combo box widths, and button labels when the window is narrowed.</summary>
    private void UpdateCompactLabels()
    {
        var width = Root.ActualWidth;
        if (width <= 0) return;

        // When narrower than 1380px, move the right-side action buttons into Row 1 opposite the tab bar
        // so the top row has the full window width for language and model pickers.
        var wrapActions = width < 1380;
        Grid.SetRow(RightActionsPanel, wrapActions ? 1 : 0);
        Grid.SetColumnSpan(LeftToolsPanel, wrapActions ? 2 : 1);

        var compactTop = width < 1120;
        var topLabelVis = compactTop ? Visibility.Collapsed : Visibility.Visible;
        OpenLabel.Visibility = topLabelVis;
        ImportSubtitleLabel.Visibility = topLabelVis;
        SpeakerLabel.Visibility = topLabelVis;
        TranslateLabel.Visibility = width < 1180 ? Visibility.Collapsed : Visibility.Visible;

        var veryCompact = width < 920;
        var actionLabelVis = veryCompact ? Visibility.Collapsed : Visibility.Visible;
        GenerateLabel.Visibility = actionLabelVis;
        GenerateAndTranslateLabel.Visibility = actionLabelVis;
        SaveSubtitleLabel.Visibility = width < 980 ? Visibility.Collapsed : Visibility.Visible;
        ExportLabel.Visibility = width < 980 ? Visibility.Collapsed : Visibility.Visible;
        ClearPlaylistLabel.Visibility = width < 980 ? Visibility.Collapsed : Visibility.Visible;
        BulkPlaylistLanguageLabel.Visibility = width < 920 ? Visibility.Collapsed : Visibility.Visible;

        LanguageBox.Width = compactTop ? 132 : 165;
        TargetBox.Width = compactTop ? 128 : 155;
        ModelNameText.MinWidth = compactTop ? 74 : 95;
        TranslationModelButton.Width = compactTop ? 180 : 220;
    }

    // MARK: - Pickers

    private void UpdatePickers(bool refill = false)
    {
        Fill(LanguageBox, SubtitleLanguage.All.Select(l => (l.Title, true)),
            IndexOf(SubtitleLanguage.All, l => l.Code == Vm.Language), refill);

        var targets = SubtitleLanguage.TranslationTargets;
        Fill(TargetBox, targets.Select(l => (l.TargetName, Vm.IsTranslationTargetEnabled(l))),
            IndexOf(targets, l => l.Code == Vm.TranslationTarget), refill);
        ToolTipService.SetToolTip(TargetBox, Vm.TranslationTargetHelp);

        var activeDisplay = Vm.HasTranslations ? Vm.DisplayMode : SubtitleDisplay.Original;
        DisplayOriginalButton.IsChecked = activeDisplay == SubtitleDisplay.Original;
        DisplayTranslationButton.IsChecked = activeDisplay == SubtitleDisplay.Translation;
        DisplayBothButton.IsChecked = activeDisplay == SubtitleDisplay.Both;
        DisplayTranslationButton.Content = Vm.ExportTranslatedTitle;
        DisplayTranslationButton.IsEnabled = Vm.HasTranslations;
        DisplayBothButton.IsEnabled = Vm.HasTranslations;
        ToolTipService.SetToolTip(DisplayPicker, Vm.HasTranslations
            ? L("Video üstünde gösterilecek metin")
            : L("Önce altyazıları çevir; sonra orijinali, çeviriyi ya da ikisini birden gösterebilirsin."));

        Fill(FormatBox, SubtitleFormats.All.Select(f => (f.RawValue().ToUpperInvariant(), true)),
            IndexOf(SubtitleFormats.All, f => f == Vm.ExportFormat), refill);
        Fill(ExportLanguageBox, [(L("Orijinal"), true), (Vm.ExportTranslatedTitle, Vm.HasTranslations)],
            Vm.ExportLanguage == ExportLanguage.Translated ? 1 : 0, refill);

        ToolTipService.SetToolTip(NoticePanel, SubtitleViewModel.ModelNoticeHelp);
    }

    private static int IndexOf<T>(IReadOnlyList<T> list, Func<T, bool> match)
    {
        for (var i = 0; i < list.Count; i++)
        {
            if (match(list[i])) return i;
        }
        return -1;
    }

    /// <summary>Fills a combo box with plain items; rebuilds only when titles or enabled states changed.</summary>
    private void Fill(ComboBox box, IEnumerable<(string Title, bool Enabled)> items, int selected, bool force)
    {
        var list = items.ToList();
        var same = !force && box.Items.Count == list.Count && box.Items.Cast<ComboBoxItem>()
            .Zip(list).All(p => (string)p.First.Content == p.Second.Title && p.First.IsEnabled == p.Second.Enabled);
        _syncing = true;
        try
        {
            if (!same)
            {
                box.Items.Clear();
                foreach (var (title, enabled) in list) box.Items.Add(new ComboBoxItem { Content = title, IsEnabled = enabled });
            }
            if (box.SelectedIndex != selected) box.SelectedIndex = selected;
        }
        finally
        {
            _syncing = false;
        }
    }

    private void LanguageBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || LanguageBox.SelectedIndex < 0) return;
        Vm.Language = SubtitleLanguage.All[LanguageBox.SelectedIndex].Code;
    }

    private void TargetBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || TargetBox.SelectedIndex < 0) return;
        Vm.TranslationTarget = SubtitleLanguage.TranslationTargets[TargetBox.SelectedIndex].Code;
    }

    private void DisplayOriginal_Click(object sender, RoutedEventArgs e)
    {
        Vm.DisplayMode = SubtitleDisplay.Original;
        UpdatePickers();
    }

    private void DisplayTranslation_Click(object sender, RoutedEventArgs e)
    {
        if (Vm.HasTranslations) Vm.DisplayMode = SubtitleDisplay.Translation;
        UpdatePickers();
    }

    private void DisplayBoth_Click(object sender, RoutedEventArgs e)
    {
        if (Vm.HasTranslations) Vm.DisplayMode = SubtitleDisplay.Both;
        UpdatePickers();
    }

    private void ExplorerToggle_Click(object sender, RoutedEventArgs e)
    {
        _isExplorerOpen = ExplorerToggleButton.IsChecked == true;
        App.Current.Settings.SetBool("showFileExplorer", _isExplorerOpen);
        ApplyWorkspaceLayout();
    }

    private void PlayerExplorer_Click(object sender, RoutedEventArgs e)
    {
        _isExplorerOpen = PlayerExplorerButton.IsChecked == true;
        App.Current.Settings.SetBool("showFileExplorer", _isExplorerOpen);
        ApplyWorkspaceLayout();
    }

    private void ToggleFileExplorer()
    {
        _isExplorerOpen = !_isExplorerOpen;
        App.Current.Settings.SetBool("showFileExplorer", _isExplorerOpen);
        ApplyWorkspaceLayout();
    }

    private void SubtitlePanel_Click(object sender, RoutedEventArgs e)
    {
        _isEditorHidden = SubtitlePanelButton.IsChecked == false;
        App.Current.Settings.SetBool("hideSubtitlePanel", _isEditorHidden);
        ApplyWorkspaceLayout();
    }

    private void ToggleSubtitlePanel()
    {
        _isEditorHidden = !_isEditorHidden;
        App.Current.Settings.SetBool("hideSubtitlePanel", _isEditorHidden);
        ApplyWorkspaceLayout();
    }

    private void SideBySide_Click(object sender, RoutedEventArgs e)
    {
        _isSideBySide = SideBySideButton.IsChecked == true;
        App.Current.Settings.SetBool("sideBySideLayout", _isSideBySide);
        ApplyWorkspaceLayout();
    }

    private void TheaterMode_Click(object sender, RoutedEventArgs e)
    {
        _isTheaterMode = TheaterModeButton.IsChecked == true;
        ApplyWorkspaceLayout();
    }

    private void FullScreen_Click(object sender, RoutedEventArgs e)
    {
        SetFullScreen(FullScreenButton.IsChecked == true);
    }

    private void SetFullScreen(bool fullScreen)
    {
        _isFullScreen = fullScreen;
        try
        {
            AppWindow.SetPresenter(fullScreen
                ? Microsoft.UI.Windowing.AppWindowPresenterKind.FullScreen
                : Microsoft.UI.Windowing.AppWindowPresenterKind.Overlapped);
        }
        catch (Exception)
        {
            // Ignore presenter switch errors on unsupported shells.
        }
        ApplyWorkspaceLayout();
    }

    private void ApplyWorkspaceLayout()
    {
        ExplorerToggleButton.IsChecked = _isExplorerOpen;
        PlayerExplorerButton.IsChecked = _isExplorerOpen;
        var showExplorer = _isExplorerOpen && !_isFullScreen;
        FileExplorerPane.Visibility = showExplorer ? Visibility.Visible : Visibility.Collapsed;
        ExplorerCol.Width = new GridLength(showExplorer ? 280 : 0);

        SubtitlePanelButton.IsChecked = !_isEditorHidden;
        SideBySideButton.IsChecked = _isSideBySide;
        SideBySideButton.IsEnabled = !_isEditorHidden && !_isFullScreen;
        TheaterModeButton.IsChecked = _isTheaterMode;
        FullScreenButton.IsChecked = _isFullScreen;
        FullScreenIcon.Glyph = _isFullScreen ? "\uE73F" : "\uE740";

        var hideTopChrome = _isTheaterMode || _isFullScreen;
        CommandBar.Visibility = hideTopChrome ? Visibility.Collapsed : Visibility.Visible;
        TitleBarRow.Height = new GridLength(_isFullScreen ? 0 : 48);
        AppTitleBar.Visibility = _isFullScreen ? Visibility.Collapsed : Visibility.Visible;
        StatusBarGrid.Visibility = _isFullScreen ? Visibility.Collapsed : Visibility.Visible;
        ContentGrid.Margin = _isFullScreen ? new Thickness(0) : new Thickness(12, 0, 12, 0);
        PlayerBorder.CornerRadius = new CornerRadius(_isFullScreen ? 0 : 8);

        if (_isEditorHidden || _isFullScreen)
        {
            Splitter.Visibility = Visibility.Collapsed;
            EditorBorder.Visibility = Visibility.Collapsed;

            Grid.SetRow(PlayerBorder, 0);
            Grid.SetRowSpan(PlayerBorder, 3);
            Grid.SetColumn(PlayerBorder, 0);
            Grid.SetColumnSpan(PlayerBorder, 3);

            PlayerRow.MinHeight = 0;
            PlayerRow.Height = new GridLength(1, GridUnitType.Star);
            SplitterRow.Height = new GridLength(0);
            EditorRow.MinHeight = 0;
            EditorRow.Height = new GridLength(0);

            PlayerCol.MinWidth = 0;
            PlayerCol.Width = new GridLength(1, GridUnitType.Star);
            SplitterCol.Width = new GridLength(0);
            EditorCol.MinWidth = 0;
            EditorCol.Width = new GridLength(0);

            UpdateOverlay();
            return;
        }

        Splitter.Visibility = Visibility.Visible;
        EditorBorder.Visibility = Visibility.Visible;

        if (_isSideBySide)
        {
            Grid.SetRow(PlayerBorder, 0);
            Grid.SetRowSpan(PlayerBorder, 3);
            Grid.SetColumn(PlayerBorder, 0);
            Grid.SetColumnSpan(PlayerBorder, 1);

            Grid.SetRow(Splitter, 0);
            Grid.SetRowSpan(Splitter, 3);
            Grid.SetColumn(Splitter, 1);
            Grid.SetColumnSpan(Splitter, 1);

            Grid.SetRow(EditorBorder, 0);
            Grid.SetRowSpan(EditorBorder, 3);
            Grid.SetColumn(EditorBorder, 2);
            Grid.SetColumnSpan(EditorBorder, 1);

            PlayerRow.MinHeight = 0;
            PlayerRow.Height = new GridLength(1, GridUnitType.Star);
            SplitterRow.Height = new GridLength(0);
            EditorRow.MinHeight = 0;
            EditorRow.Height = new GridLength(0);

            PlayerCol.MinWidth = 260;
            PlayerCol.Width = new GridLength(2, GridUnitType.Star);
            SplitterCol.Width = new GridLength(12);
            EditorCol.MinWidth = 320;
            EditorCol.Width = new GridLength(1, GridUnitType.Star);

            Splitter.IsSideBySide = true;
            Splitter.Minimum = 260;
            Splitter.OtherMinimum = 320;
            SplitterPill.Width = 4;
            SplitterPill.Height = 40;
        }
        else
        {
            Grid.SetRow(PlayerBorder, 0);
            Grid.SetRowSpan(PlayerBorder, 1);
            Grid.SetColumn(PlayerBorder, 0);
            Grid.SetColumnSpan(PlayerBorder, 3);

            Grid.SetRow(Splitter, 1);
            Grid.SetRowSpan(Splitter, 1);
            Grid.SetColumn(Splitter, 0);
            Grid.SetColumnSpan(Splitter, 3);

            Grid.SetRow(EditorBorder, 2);
            Grid.SetRowSpan(EditorBorder, 1);
            Grid.SetColumn(EditorBorder, 0);
            Grid.SetColumnSpan(EditorBorder, 3);

            PlayerCol.MinWidth = 0;
            PlayerCol.Width = new GridLength(1, GridUnitType.Star);
            SplitterCol.Width = new GridLength(0);
            EditorCol.MinWidth = 0;
            EditorCol.Width = new GridLength(0);

            PlayerRow.MinHeight = 180;
            PlayerRow.Height = new GridLength(5, GridUnitType.Star);
            SplitterRow.Height = new GridLength(12);
            EditorRow.MinHeight = 160;
            EditorRow.Height = new GridLength(4, GridUnitType.Star);

            Splitter.IsSideBySide = false;
            Splitter.Minimum = 180;
            Splitter.OtherMinimum = 160;
            SplitterPill.Width = 40;
            SplitterPill.Height = 4;
        }

        UpdateOverlay();
    }

    private void EditorBorder_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        var w = EditorBorder.ActualWidth;
        if (w <= 0) return;
        var compactEditBar = w < 720;
        var labelVis = compactEditBar ? Visibility.Collapsed : Visibility.Visible;
        SplitLabel.Visibility = labelVis;
        MergeLabel.Visibility = labelVis;
        DeleteLabel.Visibility = labelVis;
        var veryCompact = w < 540;
        var edgeVis = veryCompact ? Visibility.Collapsed : Visibility.Visible;
        StartLabel.Visibility = edgeVis;
        EndLabel.Visibility = edgeVis;
    }

    private void FormatBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || FormatBox.SelectedIndex < 0) return;
        Vm.ExportFormat = SubtitleFormats.All[FormatBox.SelectedIndex];
    }

    private void ExportLanguageBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || ExportLanguageBox.SelectedIndex < 0) return;
        Vm.ExportLanguage = ExportLanguageBox.SelectedIndex == 1 ? ExportLanguage.Translated : ExportLanguage.Original;
    }

    /// <summary>Model menu: Whisper models (detect the language), a separator, then the fixed-language ones.</summary>
    private void ModelFlyout_Opening(object? sender, object e)
    {
        ModelFlyout.Items.Clear();
        var models = Vm.AvailableModels;
        var whisper = models.Where(m => m.DetectsLanguage).ToList();
        var others = models.Where(m => !m.DetectsLanguage).ToList();
        foreach (var model in whisper) ModelFlyout.Items.Add(ModelItem(model));
        if (others.Count > 0 && whisper.Count > 0) ModelFlyout.Items.Add(new MenuFlyoutSeparator());
        foreach (var model in others) ModelFlyout.Items.Add(ModelItem(model));

        MenuFlyoutItemBase ModelItem(EngineModel model)
        {
            var item = new RadioMenuFlyoutItem
            {
                Text = Core.ModelLabel.MenuTitle(model),
                GroupName = "model",
                IsChecked = model.Id == Vm.Model,
            };
            item.Click += (_, _) => Vm.Model = model.Id;
            return item;
        }
    }

    private void TranslationModelFlyout_Opening(object? sender, object e) =>
        TranslationPicker.Open(App.Current.Providers, Vm);

    private void TranslationModelFlyout_Closed(object? sender, object e) => TranslationPicker.Close();

    private void SpeakerFlyout_Opening(object? sender, object e)
    {
        SpeakerFlyout.Items.Clear();
        var toggle = new ToggleMenuFlyoutItem { Text = L("Konuşmacıları ayır"), IsChecked = Vm.Diarize };
        toggle.Click += (_, _) => Vm.Diarize = toggle.IsChecked;
        SpeakerFlyout.Items.Add(toggle);
        var count = new MenuFlyoutSubItem { Text = L("Konuşmacı sayısı"), IsEnabled = Vm.Diarize };
        foreach (var choice in SubtitleViewModel.SpeakerCountChoices)
        {
            var item = new RadioMenuFlyoutItem
            {
                Text = SubtitleViewModel.SpeakerCountTitle(choice),
                GroupName = "speakerCount",
                IsChecked = choice == Vm.SpeakerCount,
            };
            item.Click += (_, _) => Vm.SpeakerCount = choice;
            count.Items.Add(item);
        }
        SpeakerFlyout.Items.Add(count);
    }

    // MARK: - Lines

    private void SyncRows()
    {
        var wasSyncing = _syncing;
        _syncing = true;
        try
        {
            var list = Vm.Sentences;
            var showsSpeaker = Vm.ShowsSpeakers;
            var dark = Root.ActualTheme == ElementTheme.Dark;
            var wanted = list.Select(s => s.Uid).ToHashSet();
            for (var i = Rows.Count - 1; i >= 0; i--)
            {
                if (!wanted.Contains(Rows[i].Uid)) Rows.RemoveAt(i);
            }
            var byUid = Rows.ToDictionary(r => r.Uid);
            for (var i = 0; i < list.Count; i++)
            {
                var sentence = list[i];
                if (i < Rows.Count && Rows[i].Uid == sentence.Uid)
                {
                    // In place.
                }
                else if (byUid.TryGetValue(sentence.Uid, out var existing))
                {
                    Rows.Move(Rows.IndexOf(existing), i);
                }
                else
                {
                    var created = new SentenceRow(sentence.Uid);
                    byUid[sentence.Uid] = created;
                    Rows.Insert(i, created);
                }
                Update(Rows[i], sentence);
            }
            SyncSelection();

            void Update(SentenceRow row, Sentence sentence)
            {
                row.Number = sentence.Id.ToString(System.Globalization.CultureInfo.InvariantCulture);
                row.TimeText = $"{SubtitleTime.Short(sentence.Start)} → {SubtitleTime.Short(sentence.End)}";
                row.Text = Vm.DraftText(sentence);
                row.ShowsSpeaker = showsSpeaker;
                row.HasSpeaker = sentence.Speaker is not null;
                row.SpeakerText = SubtitleViewModel.CompactSpeakerTitle(sentence.Speaker);
                row.SpeakerBrush = sentence.Speaker is { } speaker ? SpeakerBrush(speaker, dark) : null;
                var translation = Vm.TranslationFor(sentence);
                row.HasTranslation = translation is not null;
                row.TranslationText = translation is null ? "" : Vm.TranslationDraftText(sentence);
                row.IsStale = Vm.IsTranslationStale(sentence);
                row.NeedsReview = translation?.NeedsReview ?? sentence.NeedsReview;
                row.IsActive = sentence.Uid == _player.ActiveId;
            }
        }
        finally { _syncing = wasSyncing; }
    }

    private Brush SpeakerBrush(int speaker, bool dark)
    {
        if (!_speakerBrushes.TryGetValue((speaker, dark), out var brush))
        {
            brush = new SolidColorBrush(Ui.Color(SpeakerPalette.Color(speaker, dark)));
            _speakerBrushes[(speaker, dark)] = brush;
        }
        return brush;
    }

    private SentenceRow? RowFor(Guid? uid) => uid is { } id ? Rows.FirstOrDefault(r => r.Uid == id) : null;

    private Sentence? SentenceFor(SentenceRow row) => Vm.Sentences.FirstOrDefault(s => s.Uid == row.Uid);

    private void SyncSelection()
    {
        var wanted = Rows.Where(r => Vm.SelectedUids.Contains(r.Uid)).ToHashSet();
        var wasSyncing = _syncing;
        _syncing = true;
        try
        {
            foreach (var row in Lines.SelectedItems.OfType<SentenceRow>().ToList())
                if (!wanted.Contains(row)) Lines.SelectedItems.Remove(row);
            foreach (var row in wanted)
                if (!Lines.SelectedItems.Contains(row)) Lines.SelectedItems.Add(row);
        }
        finally
        {
            _syncing = wasSyncing;
        }
    }

    private void Lines_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing) return;
        Vm.SetSelectedLines(Lines.SelectedItems.OfType<SentenceRow>().Select(r => r.Uid).ToList(),
            e.AddedItems.OfType<SentenceRow>().LastOrDefault()?.Uid);
    }

    private void OnActiveChanged()
    {
        foreach (var row in Rows) row.IsActive = row.Uid == _player.ActiveId;
        UpdateOverlay();
        // Don't scroll the list while the user is typing or has paused.
        if (_player.IsPlaying && RowFor(_player.ActiveId) is { } active && FocusManager.GetFocusedElement(Root.XamlRoot) is not TextBox)
            Lines.ScrollIntoView(active);
    }

    private void SelectRow(SentenceRow row)
    {
        if (Vm.SelectedUids.Contains(row.Uid)) Vm.SetSelectedLines(Vm.SelectedUids, row.Uid);
        else Vm.Selection = row.Uid;
    }

    private static SentenceRow? RowOf(object sender) => (sender as FrameworkElement)?.DataContext as SentenceRow;

    private void LineText_GotFocus(object sender, RoutedEventArgs e)
    {
        if (RowOf(sender) is not { } row) return;
        SelectRow(row);
        if (SentenceFor(row) is { } sentence) Vm.BeginEditing(sentence);
    }

    private void LineText_LostFocus(object sender, RoutedEventArgs e) => Vm.CommitDraft();

    private void LineText_TextChanged(object sender, TextChangedEventArgs e)
    {
        // Programmatic updates of unfocused rows are not edits.
        if (sender is not TextBox { FocusState: not FocusState.Unfocused } box || RowOf(sender) is not { } row) return;
        Vm.UpdateDraft(box.Text, row.Uid);
    }

    private void LineText_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != VirtualKey.Enter || IsDown(VirtualKey.Control)) return;
        Vm.CommitDraft();
        e.Handled = true;
    }

    private void TranslationText_GotFocus(object sender, RoutedEventArgs e)
    {
        if (RowOf(sender) is not { } row) return;
        SelectRow(row);
        if (SentenceFor(row) is { } sentence) Vm.BeginTranslationEditing(sentence);
    }

    private void TranslationText_LostFocus(object sender, RoutedEventArgs e) => Vm.CommitTranslationDraft();

    private void TranslationText_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (sender is not TextBox { FocusState: not FocusState.Unfocused } box || RowOf(sender) is not { } row) return;
        Vm.UpdateTranslationDraft(box.Text, row.Uid);
    }

    private void TranslationText_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != VirtualKey.Enter || IsDown(VirtualKey.Control)) return;
        Vm.CommitTranslationDraft();
        e.Handled = true;
    }

    private void Lines_PreviewKeyDown(object sender, KeyRoutedEventArgs e)
    {
        // Let the keys type into a focused text box.
        if (e.OriginalSource is TextBox) return;
        switch (e.Key)
        {
            case VirtualKey.Space:
                // Only when the row (or list) itself has focus; Space must still activate a focused row button.
                if (e.OriginalSource is not (ListViewItem or ListView)) return;
                _player.TogglePlayback();
                e.Handled = true;
                break;
            case VirtualKey.Delete:
                Vm.FinishEditing();
                Vm.DeleteSelected();
                e.Handled = true;
                break;
        }
    }

    /// <summary>Speaker choices for a line: the known speakers, a new one, or none (row button and context menu).</summary>
    private void AddSpeakerItems(IList<MenuFlyoutItemBase> items, SentenceRow row)
    {
        if (SentenceFor(row) is not { } sentence) return;
        foreach (var speaker in Vm.Speakers)
        {
            var item = new RadioMenuFlyoutItem
            {
                Text = SubtitleViewModel.SpeakerTitle(speaker),
                GroupName = "speaker",
                IsChecked = speaker == sentence.Speaker,
                Icon = new FontIcon { Glyph = "", Foreground = SpeakerBrush(speaker, Root.ActualTheme == ElementTheme.Dark) },
            };
            item.Click += (_, _) => Vm.SetSpeaker(speaker, row.Uid);
            items.Add(item);
        }
        if (items.Count > 0) items.Add(new MenuFlyoutSeparator());
        var added = new MenuFlyoutItem { Text = L("Yeni konuşmacı"), Icon = new FontIcon { Glyph = "" } };
        added.Click += (_, _) => Vm.AssignNewSpeaker(row.Uid);
        items.Add(added);
        var removed = new MenuFlyoutItem { Text = L("Konuşmacıyı kaldır"), IsEnabled = sentence.Speaker is not null };
        removed.Click += (_, _) => Vm.SetSpeaker(null, row.Uid);
        items.Add(removed);
    }

    private void SpeakerRow_Click(object sender, RoutedEventArgs e)
    {
        if (RowOf(sender) is not { } row) return;
        SelectRow(row);
        var menu = new MenuFlyout { Placement = FlyoutPlacementMode.BottomEdgeAlignedLeft };
        AddSpeakerItems(menu.Items, row);
        menu.ShowAt((FrameworkElement)sender);
    }

    private void RowMenu_Opening(object? sender, object e)
    {
        if (sender is not MenuFlyout menu || RowOf(menu.Target) is not { } row) return;
        SelectRow(row);
        menu.Items.Clear();
        var retranslate = new MenuFlyoutItem
        {
            Text = L("Seçilenleri yeniden çevir"), Icon = new FontIcon { Glyph = "\uF2B7" },
            IsEnabled = Vm.CanTranslateSelected,
        };
        retranslate.Click += TranslateSelected_Click;
        menu.Items.Add(retranslate);
        menu.Items.Add(new MenuFlyoutSeparator());
        if (Vm.ShowsSpeakers)
        {
            AddSpeakerItems(menu.Items, row);
            menu.Items.Add(new MenuFlyoutSeparator());
        }
        var split = new MenuFlyoutItem { Text = L("Böl"), Icon = new FontIcon { Glyph = "" }, IsEnabled = Vm.CanSplitSelected(_player.CurrentTime) };
        split.Click += Split_Click;
        var merge = new MenuFlyoutItem { Text = L("Sonrakiyle birleştir"), Icon = new FontIcon { Glyph = "" }, IsEnabled = Vm.CanMergeSelected };
        merge.Click += Merge_Click;
        var delete = new MenuFlyoutItem
        {
            Text = L("Sil"),
            Icon = new FontIcon { Glyph = "" },
            KeyboardAcceleratorTextOverride = "Delete",
        };
        delete.Click += Delete_Click;
        menu.Items.Add(split);
        menu.Items.Add(merge);
        menu.Items.Add(delete);
    }

    // MARK: - Edit bar

    private void TranslateSelected_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.TranslateSelected();
    }

    private void StartMinus_Click(object sender, RoutedEventArgs e) => Vm.Nudge(TimingEdge.Start, -SentenceEditing.NudgeStep);
    private void StartPlus_Click(object sender, RoutedEventArgs e) => Vm.Nudge(TimingEdge.Start, SentenceEditing.NudgeStep);
    private void StartNow_Click(object sender, RoutedEventArgs e) => Vm.SetSelected(TimingEdge.Start, _player.CurrentTime);
    private void EndMinus_Click(object sender, RoutedEventArgs e) => Vm.Nudge(TimingEdge.End, -SentenceEditing.NudgeStep);
    private void EndPlus_Click(object sender, RoutedEventArgs e) => Vm.Nudge(TimingEdge.End, SentenceEditing.NudgeStep);
    private void EndNow_Click(object sender, RoutedEventArgs e) => Vm.SetSelected(TimingEdge.End, _player.CurrentTime);

    private void Split_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.SplitSelected(_player.CurrentTime);
    }

    private void Merge_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.MergeSelectedWithNext();
    }

    private void Delete_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.DeleteSelected();
    }

    // MARK: - Player and overlay

    private void PlayerGrid_SizeChanged(object sender, SizeChangedEventArgs e) => UpdateOverlay();

    private void PlayerElement_Tapped(object sender, TappedRoutedEventArgs e) => _player.TogglePlayback();

    private void PlayerElement_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        // Tapped already fired on the first click and toggled playback; restore playback state and toggle full screen.
        _player.TogglePlayback();
        SetFullScreen(!_isFullScreen);
        e.Handled = true;
    }

    private void PlayerGrid_PointerMoved(object sender, PointerRoutedEventArgs e)
    {
        _pointerOverPlayer = true;
        RevealPlayerHudBriefly();
    }

    private void PlayerGrid_PointerWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        var delta = e.GetCurrentPoint(PlayerGrid).Properties.MouseWheelDelta;
        if (delta == 0) return;
        var step = delta > 0 ? 0.05 : -0.05;
        var baseVolume = _player.IsMuted ? 0.0 : _player.Volume;
        _player.SetVolume(Math.Clamp(baseVolume + step, 0.0, 1.0));
        RevealPlayerHudBriefly();
        e.Handled = true;
    }

    private void DisplayPicker_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        _pointerOverDisplayPicker = true;
        RevealPlayerHudBriefly();
    }

    private void DisplayPicker_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        _pointerOverDisplayPicker = false;
        RestartHudHideTimer();
    }

    private void RevealPlayerHudBriefly()
    {
        _hudIdleHidden = false;
        UpdatePreviewControlVisibility();
        RestartHudHideTimer();
    }

    private void RestartHudHideTimer()
    {
        _hudHideTimer.Stop();
        if (_pointerOverPlayer && !PreviewControls.IsInteracting && !_pointerOverDisplayPicker)
        {
            _hudHideTimer.Start();
        }
    }

    private void PlayerGrid_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        _pointerOverPlayer = true;
        RevealPlayerHudBriefly();
        UpdatePreviewControlVisibility();
    }

    private void PlayerGrid_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        _pointerOverPlayer = false;
        _hudHideTimer.Stop();
        _hudIdleHidden = false;
        UpdatePreviewControlVisibility();
    }

    private void UpdatePreviewControlVisibility()
    {
        var visible = (_pointerOverPlayer && !_hudIdleHidden) || PreviewControls.IsInteracting || _pointerOverDisplayPicker;
        var pickerVisible = visible || !_player.IsPlaying;
        DisplayPicker.Opacity = pickerVisible ? 1.0 : 0.0;
        DisplayPicker.IsHitTestVisible = pickerVisible;
        PlayerEngagementBar.Opacity = pickerVisible ? 1.0 : 0.0;
        PlayerEngagementBar.IsHitTestVisible = pickerVisible;
        PreviewControls.SetVisible(visible);
    }

    private void UpdateOverlay()
    {
        var (videoWidth, videoHeight) = _player.VideoSize;
        var container = new Windows.Foundation.Size(PlayerGrid.ActualWidth, PlayerGrid.ActualHeight);
        if (container.Width <= 0 || container.Height <= 0) return;
        // The size the video takes inside its container (aspect-fit).
        var aspect = videoWidth / Math.Max(videoHeight, 1);
        var width = container.Width;
        var height = width / aspect;
        if (height > container.Height)
        {
            height = container.Height;
            width = height * aspect;
        }
        OverlayArea.Width = width;
        OverlayArea.Height = height;
        // Stay above the compact transport controls.
        Overlay.Margin = new Thickness(16, 0, 16, Math.Max(82, height * 0.12));
        var text = _player.ActiveId is { } id
            ? OverlayText.Compose(Vm.Sentences, Vm.Translations, Vm.DisplayMode, id)
            : null;
        Overlay.Update(text, App.Current.Appearance.Style, height);
    }

    // MARK: - Open, export, settings

    private async void Open_Click(object sender, RoutedEventArgs e) => await PickVideoAsync();

    private async void ImportSubtitle_Click(object sender, RoutedEventArgs e) => await PickSubtitleAsync();

    private async Task PickVideoAsync()
    {
        var picker = new FileOpenPicker(AppWindow.Id)
        {
            SuggestedStartLocation = PickerLocationId.VideosLibrary,
            ViewMode = PickerViewMode.Thumbnail,
        };
        foreach (var type in VideoTypes) picker.FileTypeFilter.Add(type);
        var result = await picker.PickSingleFileAsync();
        if (result?.Path is { Length: > 0 } path) Vm.OpenVideo(path);
    }

    private async Task PickSubtitleAsync()
    {
        var picker = new FileOpenPicker(AppWindow.Id)
        {
            SuggestedStartLocation = PickerLocationId.VideosLibrary,
            ViewMode = PickerViewMode.List,
        };
        foreach (var ext in SubtitleParser.Extensions) picker.FileTypeFilter.Add(ext);
        var result = await picker.PickSingleFileAsync();
        if (result?.Path is { Length: > 0 } path)
        {
            if (Vm.Video is null && SubtitleParser.FindCompanionVideo(path) is { } companionVideo)
            {
                Vm.OpenVideo(companionVideo);
            }
            Vm.TryLoadSubtitleFile(path);
        }
    }

    private async void SaveSubtitle_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            await Vm.SaveSubtitleToVideoFolderAsync();
            VideoCardViewModel.InvalidateSidecarCache(Vm.Video);
            _ = ExplorerVm.SyncToVideoPathAsync(Vm.Video);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            var reason = error.Message;
            ShowSaveError(() => L("Dosya kaydedilemedi: %@", reason));
        }
    }

    private async void Export_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        var format = Vm.ExportFormat;
        var name = Vm.ExportFileName;
        var text = await Vm.RenderExportAsync();
        if (text is null) return;  // the error is in the status
        var extension = "." + format.RawValue();
        var picker = new FileSavePicker(AppWindow.Id)
        {
            SuggestedFileName = Path.GetFileNameWithoutExtension(name),
            DefaultFileExtension = extension,
            SuggestedStartLocation = PickerLocationId.VideosLibrary,
        };
        picker.FileTypeChoices.Add(format.RawValue().ToUpperInvariant(), [extension]);
        var result = await picker.PickSaveFileAsync();
        if (result?.Path is not { Length: > 0 } path) return;
        if (!Path.HasExtension(path)) path += extension;
        try
        {
            await File.WriteAllTextAsync(path, text, new UTF8Encoding(false));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            var reason = error.Message;
            ShowSaveError(() => L("Dosya kaydedilemedi: %@", reason));
        }
    }

    private void Settings_Click(object sender, RoutedEventArgs e) => App.Current.ShowSettings();

    private void Generate_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.Generate();
    }

    private async void GenerateAndTranslate_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        if (Vm.Video is null && !Vm.HasSentences)
        {
            await PickVideoAsync();
            if (Vm.Video is null && !Vm.HasSentences) return;
        }
        Vm.GenerateAndTranslate();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e) => Vm.Cancel();

    private void Translate_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.Translate();
    }

    private void Undo_Click(object sender, RoutedEventArgs e) => Vm.Undo();

    private void Redo_Click(object sender, RoutedEventArgs e) => Vm.Redo();

    // MARK: - Playlist tab and batch queue

    private void EditorTab_Click(object sender, RoutedEventArgs e) => SwitchMode(WorkspaceTab.Editor);

    private void PlaylistTab_Click(object sender, RoutedEventArgs e) => SwitchMode(WorkspaceTab.Playlist);

    private void PlatformTab_Click(object sender, RoutedEventArgs e) => SwitchMode(WorkspaceTab.Platform);

    private void SwitchTab(bool playlist) => SwitchMode(playlist ? WorkspaceTab.Playlist : WorkspaceTab.Editor);

    private void SwitchMode(WorkspaceTab tab)
    {
        _activeTab = tab;
        var activeBg = (Brush)Application.Current.Resources["LayerFillColorDefaultBrush"];
        var activeBorder = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        var transparent = new SolidColorBrush(Microsoft.UI.Colors.Transparent);

        var isEditor = tab == WorkspaceTab.Editor;
        var isPlaylist = tab == WorkspaceTab.Playlist;
        var isPlatform = tab == WorkspaceTab.Platform;

        EditorTabButton.Background = isEditor ? activeBg : transparent;
        EditorTabButton.BorderBrush = isEditor ? activeBorder : transparent;
        EditorTabTitle.FontWeight = isEditor ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal;
        EditorTabIndicator.Visibility = isEditor ? Visibility.Visible : Visibility.Collapsed;

        PlaylistTabButton.Background = isPlaylist ? activeBg : transparent;
        PlaylistTabButton.BorderBrush = isPlaylist ? activeBorder : transparent;
        PlaylistTabTitle.FontWeight = isPlaylist ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal;
        PlaylistTabIndicator.Visibility = isPlaylist ? Visibility.Visible : Visibility.Collapsed;

        PlatformTabButton.Background = isPlatform ? activeBg : transparent;
        PlatformTabButton.BorderBrush = isPlatform ? activeBorder : transparent;
        PlatformTabTitle.FontWeight = isPlatform ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal;
        PlatformTabIndicator.Visibility = isPlatform ? Visibility.Visible : Visibility.Collapsed;

        EditorView.Visibility = isEditor ? Visibility.Visible : Visibility.Collapsed;
        PlaylistPanel.Visibility = isPlaylist ? Visibility.Visible : Visibility.Collapsed;
        PlatformPanel.Visibility = isPlatform ? Visibility.Visible : Visibility.Collapsed;
        PlatformHubView.SetActiveTab(isPlatform);
        if (isPlatform) PlatformVm.RefreshFeed();
        UpdateTabActionVisibility();
    }

    private void UpdateTabActionVisibility()
    {
        var singleTab = _activeTab == WorkspaceTab.Editor;
        OpenButton.Visibility = singleTab ? Visibility.Visible : Visibility.Collapsed;
        ImportSubtitleButton.Visibility = singleTab ? Visibility.Visible : Visibility.Collapsed;
        LanguageBox.Visibility = singleTab ? Visibility.Visible : Visibility.Collapsed;
        AutoSaveCheckBox.Visibility = singleTab ? Visibility.Visible : Visibility.Collapsed;
        var showSingleVideoActions = singleTab && !Vm.IsWorking;
        GenerateButton.Visibility = showSingleVideoActions ? Visibility.Visible : Visibility.Collapsed;
        GenerateAndTranslateButton.Visibility = showSingleVideoActions ? Visibility.Visible : Visibility.Collapsed;
        GenerateAndTranslateButton.IsEnabled = !Vm.IsWorking;
        TranslateButton.Visibility = singleTab && Vm.HasSentences ? Visibility.Visible : Visibility.Collapsed;
    }

    private void SyncPlaylistRows()
    {
        var list = Vm.Playlist;
        var wanted = list.Select(i => i.Id).ToHashSet();
        for (var i = PlaylistRows.Count - 1; i >= 0; i--)
        {
            if (!wanted.Contains(PlaylistRows[i].Id)) PlaylistRows.RemoveAt(i);
        }
        var byId = PlaylistRows.ToDictionary(r => r.Id);
        for (var i = 0; i < list.Count; i++)
        {
            var item = list[i];
            if (i < PlaylistRows.Count && PlaylistRows[i].Id == item.Id)
            {
                // In place.
            }
            else if (byId.TryGetValue(item.Id, out var existing))
            {
                PlaylistRows.Move(PlaylistRows.IndexOf(existing), i);
            }
            else
            {
                var created = new PlaylistRow(item.Id);
                byId[item.Id] = created;
                PlaylistRows.Insert(i, created);
            }
            var row = PlaylistRows[i];
            row.FileName = item.FileName;
            row.FolderPath = item.FolderPath;
            row.SourceLanguageTitle = item.SelectedSourceLanguage.Title;
            row.StatusText = item.StatusText;
            row.IsPending = item.IsPending;
            row.IsWorking = item.IsWorking;
            row.IsCompleted = item.IsCompleted;
            row.IsFailed = item.IsFailed;
            row.IsIndeterminate = item.IsWorking && item.Fraction is null;
            var itemFrac = item.Fraction ?? 0.0;
            row.ProgressValue = double.IsFinite(itemFrac) ? Math.Clamp(itemFrac, 0.0, 1.0) : 0.0;
            row.CanEdit = item.CanEdit && !Vm.IsPlaylistRunning;
        }
    }

    private void PlaylistList_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is not PlaylistRow row) return;
        var item = Vm.Playlist.FirstOrDefault(i => i.Id == row.Id);
        if (item is null || string.IsNullOrWhiteSpace(item.VideoPath)) return;
        SwitchMode(WorkspaceTab.Editor);
        Vm.OpenVideo(item.VideoPath);
    }

    private async void AddPlaylistVideos_Click(object sender, RoutedEventArgs e) => await PickPlaylistVideosAsync();

    private async Task PickPlaylistVideosAsync()
    {
        var picker = new FileOpenPicker(AppWindow.Id)
        {
            SuggestedStartLocation = PickerLocationId.VideosLibrary,
            ViewMode = PickerViewMode.List,
        };
        foreach (var type in VideoTypes) picker.FileTypeFilter.Add(type);
        var results = await picker.PickMultipleFilesAsync();
        if (results is { Count: > 0 })
        {
            Vm.AddToPlaylist(results.Select(r => r.Path));
            SwitchTab(playlist: true);
        }
    }

    private void BulkPlaylistLanguageFlyout_Opening(object? sender, object e)
    {
        BulkPlaylistLanguageFlyout.Items.Clear();
        foreach (var language in SubtitleLanguage.All)
        {
            var code = language.Code;
            var item = new MenuFlyoutItem { Text = language.Title };
            item.Click += (_, _) => Vm.SetAllPlaylistLanguages(code);
            BulkPlaylistLanguageFlyout.Items.Add(item);
        }
    }

    private void PlaylistItemLanguage_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PlaylistRow row) return;
        var currentCode = Vm.Playlist.FirstOrDefault(i => i.Id == row.Id)?.SourceLanguage ?? "";
        var menu = new MenuFlyout { Placement = FlyoutPlacementMode.BottomEdgeAlignedLeft };
        foreach (var language in SubtitleLanguage.All)
        {
            var code = language.Code;
            var item = new RadioMenuFlyoutItem
            {
                Text = language.Title,
                GroupName = "playlistRowLang",
                IsChecked = code == currentCode,
            };
            item.Click += (_, _) => Vm.SetPlaylistItemLanguage(row.Id, code);
            menu.Items.Add(item);
        }
        menu.ShowAt((FrameworkElement)sender);
    }

    private void ClearPlaylist_Click(object sender, RoutedEventArgs e) => Vm.ClearPlaylist();

    private void StartPlaylist_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.StartPlaylist();
    }

    private void RemovePlaylistItem_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is PlaylistRow row)
        {
            Vm.RemoveFromPlaylist(row.Id);
        }
    }

    // MARK: - Drag and drop

    private void Root_DragOver(object sender, DragEventArgs e)
    {
        if (!e.DataView.Contains(StandardDataFormats.StorageItems)) return;
        e.AcceptedOperation = DataPackageOperation.Copy;
        e.DragUIOverride.IsCaptionVisible = false;
    }

    private async void Root_Drop(object sender, DragEventArgs e)
    {
        if (!e.DataView.Contains(StandardDataFormats.StorageItems)) return;
        var deferral = e.GetDeferral();
        IReadOnlyList<IStorageItem> items;
        try
        {
            items = await e.DataView.GetStorageItemsAsync();
        }
        finally
        {
            deferral.Complete();
        }
        var files = items.OfType<StorageFile>().ToList();
        var videoFiles = files.Where(f => !SubtitleParser.IsSubtitleFile(f.Path)).ToList();
        var subtitleFile = files.FirstOrDefault(f => SubtitleParser.IsSubtitleFile(f.Path));

        if (videoFiles.Count > 1 || (_isPlaylistTabActive && videoFiles.Count > 0))
        {
            Vm.AddToPlaylist(videoFiles.Select(f => f.Path));
            SwitchTab(playlist: true);
            return;
        }

        if (videoFiles.FirstOrDefault() is { } videoFile) Vm.OpenVideo(videoFile.Path);
        if (subtitleFile is not null) Vm.TryLoadSubtitleFile(subtitleFile.Path);
    }

    /// <summary>Opens one or more media or subtitle files received from command-line arguments, "Open with", or a second instance.</summary>
    public void OpenExternalFiles(IReadOnlyList<string> paths)
    {
        if (paths.Count == 0) return;
        var existing = paths.Where(File.Exists).Select(Path.GetFullPath).ToList();
        if (existing.Count == 0) return;

        var videoFiles = existing.Where(p => !SubtitleParser.IsSubtitleFile(p)).ToList();
        var subtitleFile = existing.FirstOrDefault(SubtitleParser.IsSubtitleFile);

        if (videoFiles.Count > 1)
        {
            Vm.AddToPlaylist(videoFiles);
            SwitchTab(playlist: true);
            return;
        }

        SwitchTab(playlist: false);
        if (videoFiles.Count == 1)
        {
            Vm.OpenVideo(videoFiles[0]);
            if (subtitleFile is not null) Vm.TryLoadSubtitleFile(subtitleFile);
            return;
        }

        if (subtitleFile is not null)
        {
            if (SubtitleParser.FindCompanionVideo(subtitleFile) is { } companionVideo)
            {
                Vm.OpenVideo(companionVideo);
            }
            Vm.TryLoadSubtitleFile(subtitleFile);
        }
    }

    // MARK: - Keyboard

    private static bool IsDown(VirtualKey key) =>
        InputKeyboardSource.GetKeyStateForCurrentThread(key).HasFlag(CoreVirtualKeyStates.Down);

    /// <summary>
    /// Window shortcuts. Undo/redo are taken before a focused text box sees them: like the macOS editor, the draft
    /// being typed is committed and then undone as one step.
    /// </summary>
    private async void Root_PreviewKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.F11)
        {
            e.Handled = true;
            SetFullScreen(!_isFullScreen);
            return;
        }
        if (e.Key == VirtualKey.Escape && e.OriginalSource is not TextBox && (_isFullScreen || _isTheaterMode))
        {
            e.Handled = true;
            if (_isFullScreen) SetFullScreen(false);
            else
            {
                _isTheaterMode = false;
                ApplyWorkspaceLayout();
            }
            return;
        }
        var isEditableFocus = e.OriginalSource is TextBox || e.OriginalSource is ComboBox { IsEditable: true };
        if (!isEditableFocus && !IsDown(VirtualKey.Control) && !IsDown(VirtualKey.Menu))
        {
            var shiftKey = IsDown(VirtualKey.Shift);
            switch (e.Key)
            {
                case VirtualKey.Space when e.OriginalSource is not (ButtonBase or ComboBox or ComboBoxItem or ListViewItem or ListView):
                    e.Handled = true;
                    _player.TogglePlayback();
                    RevealPlayerHudBriefly();
                    return;
                case VirtualKey.Left:
                    e.Handled = true;
                    _player.Seek(_player.CurrentTime - (shiftKey ? 10.0 : 5.0));
                    RevealPlayerHudBriefly();
                    return;
                case VirtualKey.Right:
                    e.Handled = true;
                    _player.Seek(_player.CurrentTime + (shiftKey ? 10.0 : 5.0));
                    RevealPlayerHudBriefly();
                    return;
                case VirtualKey.Up when e.OriginalSource is not (ListViewItem or ComboBox or ComboBoxItem) || _isEditorHidden || _isFullScreen:
                    e.Handled = true;
                    _player.SetVolume((_player.IsMuted ? 0.0 : _player.Volume) + 0.05);
                    RevealPlayerHudBriefly();
                    return;
                case VirtualKey.Down when e.OriginalSource is not (ListViewItem or ComboBox or ComboBoxItem) || _isEditorHidden || _isFullScreen:
                    e.Handled = true;
                    _player.SetVolume((_player.IsMuted ? 0.0 : _player.Volume) - 0.05);
                    RevealPlayerHudBriefly();
                    return;
                case VirtualKey.M:
                    e.Handled = true;
                    _player.ToggleMute();
                    RevealPlayerHudBriefly();
                    return;
                case VirtualKey.F:
                    e.Handled = true;
                    SetFullScreen(!_isFullScreen);
                    return;
                case VirtualKey.E:
                    e.Handled = true;
                    ToggleSubtitlePanel();
                    return;
            }
        }
        if (!IsDown(VirtualKey.Control) || IsDown(VirtualKey.Menu)) return;
        var shift = IsDown(VirtualKey.Shift);
        switch (e.Key)
        {
            case VirtualKey.Z when !shift:
                e.Handled = true;
                Vm.Undo();
                break;
            case VirtualKey.Y:
            case VirtualKey.Z when shift:
                e.Handled = true;
                Vm.Redo();
                break;
            case VirtualKey.O:
                e.Handled = true;
                await PickVideoAsync();
                break;
            case VirtualKey.B:
                e.Handled = true;
                ToggleFileExplorer();
                break;
            case VirtualKey.Enter:
                e.Handled = true;
                Vm.FinishEditing();
                if (Vm.GenerateCommand.CanExecute(null)) Vm.Generate();
                break;
            case VirtualKey.Left when !isEditableFocus:
                e.Handled = true;
                _player.Seek(_player.CurrentTime - 10.0);
                RevealPlayerHudBriefly();
                break;
            case VirtualKey.Right when !isEditableFocus:
                e.Handled = true;
                _player.Seek(_player.CurrentTime + 10.0);
                RevealPlayerHudBriefly();
                break;
            case VirtualKey.Up when !isEditableFocus:
                e.Handled = true;
                _player.SetVolume((_player.IsMuted ? 0.0 : _player.Volume) + 0.05);
                RevealPlayerHudBriefly();
                break;
            case VirtualKey.Down when !isEditableFocus:
                e.Handled = true;
                _player.SetVolume((_player.IsMuted ? 0.0 : _player.Volume) - 0.05);
                RevealPlayerHudBriefly();
                break;
            case (VirtualKey)188:  // VK_OEM_COMMA
                e.Handled = true;
                App.Current.ShowSettings();
                break;
        }
    }
}
