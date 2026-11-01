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
    private LocalizedMessage? _shownError;
    private string? _saveError;

    public MainWindow()
    {
        InitializeComponent();
        WindowChrome.Setup(this, Root, AppTitleBar, minWidth: 960, minHeight: 640, defaultWidth: 1240, defaultHeight: 840, sizeKey: "mainWindow");

        _player = new PlayerController(DispatcherQueue);
        PlayerElement.SetMediaPlayer(_player.Player);
        // No zoom button: the overlay is laid out for the aspect-fit video.
        PlayerElement.TransportControls.IsZoomButtonVisible = false;
        _player.ActiveChanged += OnActiveChanged;
        _player.VideoSizeChanged += UpdateOverlay;
        _player.Ticked += () => SplitButton.IsEnabled = Vm.CanSplitSelected(_player.CurrentTime);
        _player.Failed += message => ShowSaveError(L("Video oynatılamadı: %@", message));
        Splitter.Target = PlayerRow;
        Splitter.Other = EditorRow;

        // Buttons follow the commands' CanExecute (wired here rather than through Command bindings).
        foreach (var (command, button) in new (CommunityToolkit.Mvvm.Input.IRelayCommand, Button)[]
                 {
                     (Vm.GenerateCommand, GenerateButton), (Vm.CancelCommand, CancelButton), (Vm.TranslateCommand, TranslateButton),
                     (Vm.UndoCommand, UndoButton), (Vm.RedoCommand, RedoButton),
                 })
        {
            button.IsEnabled = command.CanExecute(null);
            command.CanExecuteChanged += (_, _) => button.IsEnabled = command.CanExecute(null);
        }
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
            _player.Dispose();
        };

        RefreshAll();
        if (Vm.Video is not null) OnVideoChanged();
    }

    public SubtitleViewModel Vm { get; } = App.Current.ViewModel;

    /// <summary>The line list, kept per <see cref="Sentence.Uid"/> (see <see cref="SentenceRow"/>).</summary>
    public ObservableCollection<SentenceRow> Rows { get; } = [];

    // MARK: - View model -> controls

    private void OnViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(SubtitleViewModel.Video):
                OnVideoChanged();
                break;
            case nameof(SubtitleViewModel.Sentences):
                _player.SetSentences(Vm.Sentences);
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
            case nameof(SubtitleViewModel.Status):
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
        DropZone.Visibility = Vm.Video is null ? Visibility.Visible : Visibility.Collapsed;
        WorkArea.Visibility = Vm.Video is null ? Visibility.Collapsed : Visibility.Visible;
        UpdatePickers(refill: true);
        UpdateSpeakerButton();
        UpdateStatus();
        UpdateUndoTips();
        UpdateTimingTips();
        UpdateTimingEnabled();
        SyncRows();
        UpdateOverlay();
        UpdateCompactLabels();
    }

    private void OnVideoChanged()
    {
        _player.Load(Vm.Video);
        Title = Vm.WindowTitle;
        TitleFile.Text = Vm.Video is null ? "" : "— " + Vm.WindowTitle;
        DropZone.Visibility = Vm.Video is null ? Visibility.Visible : Visibility.Collapsed;
        WorkArea.Visibility = Vm.Video is null ? Visibility.Collapsed : Visibility.Visible;
        UpdatePickers();
    }

    private void UpdateStatus()
    {
        var kind = Vm.StatusKind;
        Progress.Visibility = kind == JobStatusKind.Working ? Visibility.Visible : Visibility.Collapsed;
        Progress.IsIndeterminate = Vm.IsIndeterminate;
        Progress.Value = Vm.StatusFraction ?? 0;
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
    }

    private void ShowSaveError(string message)
    {
        _saveError = message;
        ErrorBar.Message = message;
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

    /// <summary>Hides the text of secondary toolbar buttons when the window is narrow.</summary>
    private void UpdateCompactLabels()
    {
        var compact = Root.ActualWidth is > 0 and < 1220;
        var visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        OpenLabel.Visibility = visibility;
        SpeakerLabel.Visibility = visibility;
        TranslateLabel.Visibility = Root.ActualWidth is > 0 and < 1080 ? Visibility.Collapsed : Visibility.Visible;
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

        Fill(DisplayBox, SubtitleDisplays.All.Select(d => (d.Title(), true)),
            IndexOf(SubtitleDisplays.All, d => d == Vm.DisplayMode), refill);
        DisplayBox.IsEnabled = Vm.HasTranslations;
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

    private void DisplayBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || DisplayBox.SelectedIndex < 0) return;
        Vm.DisplayMode = SubtitleDisplays.All[DisplayBox.SelectedIndex];
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
            row.SpeakerText = SubtitleViewModel.SpeakerTitle(sentence.Speaker);
            row.SpeakerBrush = sentence.Speaker is { } speaker ? SpeakerBrush(speaker, dark) : null;
            var translation = Vm.TranslationFor(sentence);
            row.HasTranslation = translation is not null;
            row.TranslationText = translation is null ? "" : Vm.TranslationDraftText(sentence);
            row.IsStale = Vm.IsTranslationStale(sentence);
            row.IsActive = sentence.Uid == _player.ActiveId;
        }
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
        var row = RowFor(Vm.Selection);
        if (ReferenceEquals(Lines.SelectedItem, row)) return;
        _syncing = true;
        try
        {
            Lines.SelectedItem = row;
        }
        finally
        {
            _syncing = false;
        }
    }

    private void Lines_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing) return;
        Vm.Selection = (Lines.SelectedItem as SentenceRow)?.Uid;
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
        if (Vm.Selection != row.Uid) Vm.Selection = row.Uid;
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
        Overlay.Margin = new Thickness(16, 0, 16, Math.Max(64, height * 0.1));
        var text = _player.ActiveId is { } id
            ? OverlayText.Compose(Vm.Sentences, Vm.Translations, Vm.DisplayMode, id)
            : null;
        Overlay.Update(text, App.Current.Appearance.Style, height);
    }

    // MARK: - Open, export, settings

    private async void Open_Click(object sender, RoutedEventArgs e) => await PickVideoAsync();

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

    private async void Export_Click(object sender, RoutedEventArgs e)
    {
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
            ShowSaveError(L("Dosya kaydedilemedi: %@", error.Message));
        }
    }

    private void Settings_Click(object sender, RoutedEventArgs e) => App.Current.ShowSettings();

    private void Generate_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.Generate();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e) => Vm.Cancel();

    private void Translate_Click(object sender, RoutedEventArgs e)
    {
        Vm.FinishEditing();
        Vm.Translate();
    }

    private void Undo_Click(object sender, RoutedEventArgs e) => Vm.Undo();

    private void Redo_Click(object sender, RoutedEventArgs e) => Vm.Redo();

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
        if (items.OfType<StorageFile>().FirstOrDefault() is { } file) Vm.OpenVideo(file.Path);
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
            case VirtualKey.Enter:
                e.Handled = true;
                Vm.FinishEditing();
                if (Vm.GenerateCommand.CanExecute(null)) Vm.Generate();
                break;
            case (VirtualKey)188:  // VK_OEM_COMMA
                e.Handled = true;
                App.Current.ShowSettings();
                break;
        }
    }
}
