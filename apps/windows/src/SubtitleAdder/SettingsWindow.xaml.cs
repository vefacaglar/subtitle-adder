using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

/// <summary>
/// Settings (port of SettingsView): General, Appearance and Models pages in a NavigationView. The model catalog is
/// shared with the main window, so a delete made here updates the toolbar picker immediately.
/// </summary>
public sealed partial class SettingsWindow : Window
{
    private readonly SubtitleViewModel _vm = App.Current.ViewModel;
    private readonly ModelCatalog _catalog = App.Current.Catalog;
    private readonly AppLanguage _language = App.Current.Language;
    private readonly AppearanceSettings _appearance = App.Current.Appearance;
    private bool _syncing;
    private int? _freedMB;   // shown in FreedBar; its text is rebuilt on a language switch

    public SettingsWindow()
    {
        InitializeComponent();
        WindowChrome.Setup(this, Root, AppTitleBar, minWidth: 640, minHeight: 520, defaultWidth: 860, defaultHeight: 760, sizeKey: "settingsWindow");

        _vm.PropertyChanged += OnChanged;
        _catalog.PropertyChanged += OnCatalogChanged;
        _appearance.PropertyChanged += OnChanged;
        _language.PropertyChanged += OnChanged;
        Closed += (_, _) =>
        {
            _vm.PropertyChanged -= OnChanged;
            _catalog.PropertyChanged -= OnCatalogChanged;
            _appearance.PropertyChanged -= OnChanged;
            _language.PropertyChanged -= OnChanged;
        };
        PreviewArea.SizeChanged += (_, _) => UpdatePreview();
        Root.Loaded += async (_, _) => await _catalog.LoadAsync();

        Refresh(refill: true);
        UpdateModels();
    }

    /// <summary>"general", "appearance" or "models".</summary>
    public void Navigate(string page)
    {
        Nav.SelectedItem = page switch
        {
            "appearance" => AppearanceItem,
            "models" => ModelsItem,
            _ => GeneralItem,
        };
    }

    /// <summary>"Hız 4/5 · Doğruluk 2/5" for a model row (relative 1–5 scales from the engine catalog).</summary>
    public static string RatingText(EngineModel model) =>
        L("Hız %lld/5 · Doğruluk %lld/5", (long)Math.Clamp(model.Speed, 0, 5), (long)Math.Clamp(model.Accuracy, 0, 5));

    private void OnChanged(object? sender, PropertyChangedEventArgs e) => Refresh(refill: string.IsNullOrEmpty(e.PropertyName) || e.PropertyName == nameof(AppLanguage.Effective));

    private static readonly HashSet<string> ModelListProperties =
    [
        nameof(ModelCatalog.TranscriptionRows), nameof(ModelCatalog.DiarizationRows), nameof(ModelCatalog.TranslationRows),
        nameof(ModelCatalog.IsLoading), nameof(ModelCatalog.LoadError), nameof(ModelCatalog.TotalDiskText),
    ];

    private void OnCatalogChanged(object? sender, PropertyChangedEventArgs e)
    {
        // One catalog change raises several notifications; only these affect the model page, and unchanged lists are
        // not rebuilt (that would drop keyboard focus from a card). Models also fires on a language switch: rebind then.
        if (string.IsNullOrEmpty(e.PropertyName) || e.PropertyName == nameof(ModelCatalog.Models))
            UpdateModels(force: true);
        else if (ModelListProperties.Contains(e.PropertyName))
            UpdateModels();
        if (e.PropertyName == nameof(ModelCatalog.Models)) Refresh(refill: false);
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var tag = (args.SelectedItem as NavigationViewItem)?.Tag as string;
        GeneralPage.Visibility = tag is "general" or null ? Visibility.Visible : Visibility.Collapsed;
        AppearancePage.Visibility = tag == "appearance" ? Visibility.Visible : Visibility.Collapsed;
        ModelsPage.Visibility = tag == "models" ? Visibility.Visible : Visibility.Collapsed;
        UpdatePageTitle();
        Scroller.ChangeView(null, 0, null, true);
        if (tag == "models") _ = _catalog.LoadAsync();
    }

    private void UpdatePageTitle()
    {
        var tag = (Nav.SelectedItem as NavigationViewItem)?.Tag as string;
        PageTitle.Text = tag switch
        {
            "appearance" => L("Görünüm"),
            "models" => L("Modeller"),
            _ => L("Genel"),
        };
        Title = L("Ayarlar") + " — Subtitle Adder";
        TitleText.Text = L("Ayarlar");
    }

    // MARK: - State -> controls

    private void Refresh(bool refill)
    {
        UpdatePageTitle();
        Fill(UiLanguageBox, LanguageChoices.All.Select(c => c.Title()), IndexOf(LanguageChoices.All, c => c == _language.Choice), refill);
        Fill(SpeechLanguageBox, SubtitleLanguage.All.Select(l => l.Title), IndexOf(SubtitleLanguage.All, l => l.Code == _vm.Language), refill);
        var models = _vm.AvailableModels;
        Fill(ModelBox, models.Select(m => Core.ModelLabel.Name(m.Id)), IndexOf(models, m => m.Id == _vm.Model), refill);
        ModelBox.IsEnabled = models.Count > 0;
        var translationModels = _catalog.TranslationModels;
        Fill(TranslationModelBox, translationModels.Select(m => Core.ModelLabel.Name(m.Id)), IndexOf(translationModels, m => m.Id == _vm.TranslationModel), refill);
        TranslationModelBox.IsEnabled = translationModels.Count > 0;
        _syncing = true;
        DiarizeSwitch.IsOn = _vm.Diarize;
        _syncing = false;
        var counts = SubtitleViewModel.SpeakerCountChoices;
        Fill(SpeakerCountBox, counts.Select(SubtitleViewModel.SpeakerCountTitle), IndexOf(counts, c => c == _vm.SpeakerCount), refill);
        SpeakerCountBox.IsEnabled = _vm.Diarize;

        Fill(ThemeBox, ThemeChoices.All.Select(t => t.Title()), IndexOf(ThemeChoices.All, t => t == _appearance.Theme), refill);
        var families = SystemFonts.Families;
        Fill(FontBox, [L("Sistem"), .. families],
            _appearance.FontFamily.Length == 0 ? 0 : IndexOf(families, f => f == _appearance.FontFamily) is var i and >= 0 ? i + 1 : 0, refill);
        Fill(BackgroundBox, OverlayBackgrounds.All.Select(b => b.Title()), IndexOf(OverlayBackgrounds.All, b => b == _appearance.Background), refill);

        var style = _appearance.Style;
        _syncing = true;
        SizeSlider.Minimum = OverlayStyle.MinSize;
        SizeSlider.Maximum = OverlayStyle.MaxSize;
        SizeSlider.Value = style.SizeFraction;
        OpacitySlider.Value = style.BackgroundOpacity;
        TextColorPicker.Color = Ui.Color(style.TextColor);
        _syncing = false;
        SizeText.Text = _appearance.SizeText;
        OpacityText.Text = _appearance.OpacityText;
        var box = style.Background == OverlayBackground.Box;
        OpacitySlider.IsEnabled = box;
        OpacityTitle.Opacity = box ? 1 : 0.5;
        ColorSwatch.Background = new SolidColorBrush(Ui.Color(style.TextColor));
        ColorHex.Text = style.TextHex.ToUpperInvariant();
        UpdatePreview();
        if (refill && _freedMB is { } freed && FreedBar.IsOpen) FreedBar.Message = FreedText(freed);
        if (refill) UpdateModels(force: true);
    }

    private static string FreedText(int mb) => L("%@ boşaltıldı.", Core.ModelLabel.SizeText(mb));

    private void UpdatePreview() =>
        PreviewLabel.Update(L("Örnek altyazı metni"), _appearance.Style, 150);

    private void UpdateModels(bool force = false)
    {
        SetRows(TranscriptionList, _catalog.TranscriptionRows, force);
        SetRows(DiarizationList, _catalog.DiarizationRows, force);
        DiarizationSection.Visibility = _catalog.DiarizationRows.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        SetRows(TranslationList, _catalog.TranslationRows, force);
        LoadingRing.IsActive = _catalog.IsLoading;
        LoadingRing.Visibility = _catalog.IsLoading ? Visibility.Visible : Visibility.Collapsed;
        TotalDisk.Text = _catalog.TotalDiskText;
        LoadErrorBar.Message = _catalog.LoadError?.Text ?? "";
        LoadErrorBar.IsOpen = _catalog.LoadError is not null;
    }

    private static void SetRows(ItemsControl list, IReadOnlyList<ModelRow> rows, bool force)
    {
        if (!force && list.ItemsSource is IReadOnlyList<ModelRow> shown && shown.SequenceEqual(rows)) return;
        list.ItemsSource = rows;
    }

    private static int IndexOf<T>(IReadOnlyList<T> list, Func<T, bool> match)
    {
        for (var i = 0; i < list.Count; i++)
        {
            if (match(list[i])) return i;
        }
        return -1;
    }

    private void Fill(ComboBox box, IEnumerable<string> titles, int selected, bool force)
    {
        var list = titles.ToList();
        var same = !force && box.Items.Count == list.Count && box.Items.Cast<object>().Zip(list).All(p => (string)p.First == p.Second);
        _syncing = true;
        try
        {
            if (!same)
            {
                box.Items.Clear();
                foreach (var title in list) box.Items.Add(title);
            }
            if (box.SelectedIndex != selected) box.SelectedIndex = selected;
        }
        finally
        {
            _syncing = false;
        }
    }

    // MARK: - Controls -> state

    private void UiLanguageBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || UiLanguageBox.SelectedIndex < 0) return;
        _language.Choice = LanguageChoices.All[UiLanguageBox.SelectedIndex];
    }

    private void SpeechLanguageBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || SpeechLanguageBox.SelectedIndex < 0) return;
        _vm.Language = SubtitleLanguage.All[SpeechLanguageBox.SelectedIndex].Code;
    }

    private void ModelBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var models = _vm.AvailableModels;
        if (_syncing || ModelBox.SelectedIndex < 0 || ModelBox.SelectedIndex >= models.Count) return;
        _vm.Model = models[ModelBox.SelectedIndex].Id;
    }

    private void TranslationModelBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var models = _catalog.TranslationModels;
        if (_syncing || TranslationModelBox.SelectedIndex < 0 || TranslationModelBox.SelectedIndex >= models.Count) return;
        _vm.TranslationModel = models[TranslationModelBox.SelectedIndex].Id;
    }

    private void DiarizeSwitch_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        _vm.Diarize = DiarizeSwitch.IsOn;
    }

    private void SpeakerCountBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || SpeakerCountBox.SelectedIndex < 0) return;
        _vm.SpeakerCount = SubtitleViewModel.SpeakerCountChoices[SpeakerCountBox.SelectedIndex];
    }

    private void ThemeBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || ThemeBox.SelectedIndex < 0) return;
        _appearance.Theme = ThemeChoices.All[ThemeBox.SelectedIndex];
    }

    private void FontBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || FontBox.SelectedIndex < 0) return;
        _appearance.FontFamily = FontBox.SelectedIndex == 0 ? "" : SystemFonts.Families[FontBox.SelectedIndex - 1];
    }

    private void BackgroundBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || BackgroundBox.SelectedIndex < 0) return;
        _appearance.Background = OverlayBackgrounds.All[BackgroundBox.SelectedIndex];
    }

    private void SizeSlider_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        if (_syncing) return;
        _appearance.Size = e.NewValue;
    }

    private void OpacitySlider_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        if (_syncing) return;
        _appearance.Opacity = e.NewValue;
    }

    private void TextColorPicker_ColorChanged(ColorPicker sender, ColorChangedEventArgs args)
    {
        if (_syncing) return;
        var c = args.NewColor;
        _appearance.TextHex = new RgbColor(c.R, c.G, c.B).Hex;
    }

    private void ResetOverlay_Click(object sender, RoutedEventArgs e) => _appearance.ResetOverlay();

    // MARK: - Models

    private async void DeleteModel_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not ModelRow row || !row.CanDelete) return;
        var dialog = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            RequestedTheme = Root.ActualTheme,
            Title = row.DeleteConfirmTitle,
            Content = row.DeleteConfirmMessage,
            PrimaryButtonText = L("Sil"),
            CloseButtonText = L("Vazgeç"),
            DefaultButton = ContentDialogButton.Close,
        };
        if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        var freed = await _catalog.DeleteAsync(row.Model);
        if (freed is { } mb)
        {
            _freedMB = mb;
            FreedBar.Message = FreedText(mb);
            FreedBar.IsOpen = true;
        }
    }
}
