using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using SubtitleAdder.Core;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

public sealed partial class TranslationModelPicker : UserControl
{
    private sealed record ProviderChoice(Guid? Id, string Name);

    private TranslationProviderManager? _manager;
    private SubtitleViewModel? _viewModel;
    private Guid? _providerId;
    private List<ProviderChoice> _providers = [];
    private IReadOnlyList<TranslationModelChoice> _models = [];
    private CancellationTokenSource? _refreshCts;
    private int _revision;
    private bool _syncing;

    public event Action? ModelChosen;
    public event Action? SettingsRequested;

    public TranslationModelPicker() => InitializeComponent();

    public void Open(TranslationProviderManager manager, SubtitleViewModel viewModel)
    {
        Close();
        _manager = manager;
        _viewModel = viewModel;
        _providerId = manager.ActiveProviderId;
        _syncing = true;
        try
        {
            _providers = [new(null, L("Yerel modeller (Cihaz üzerinde)"))];
            _providers.AddRange(manager.Providers.Select(p => new ProviderChoice(p.Id, p.Name)));
            ProviderBox.ItemsSource = _providers.Select(p => p.Name).ToList();
            ProviderBox.SelectedIndex = _providers.FindIndex(p => p.Id == _providerId);
            ProviderBox.Header = L("Çeviri sağlayıcısı");
            SearchBox.PlaceholderText = L("Model ara…");
            AutomationProperties.SetName(SearchBox, L("Model ara…"));
            AutomationProperties.SetName(ProviderBox, L("Çeviri sağlayıcısı"));
            AutomationProperties.SetName(ModelList, L("Çeviri modeli"));
            RefreshButton.Content = L("Yenile");
            SettingsButton.Content = L("API sağlayıcılarını yönet…");
            SearchBox.Text = "";
        }
        finally { _syncing = false; }
        LoadModels();
    }

    public void Close()
    {
        _revision++;
        _refreshCts?.Cancel();
        _refreshCts = null; // The request's finally block owns disposal.
    }

    private void LoadModels()
    {
        if (_manager is null || _viewModel is null) return;
        _models = TranslationModelSelection.GetModels(_manager, _viewModel, _providerId);
        RefreshButton.IsEnabled = _providerId is not null;
        StatusText.Text = "";
        FilterModels();
    }

    private void FilterModels()
    {
        if (_viewModel is null || _manager is null) return;
        var query = SearchBox.Text.Trim();
        var filtered = _models.Where(m => m.Title.Contains(query, StringComparison.OrdinalIgnoreCase)).ToList();
        var selectedModel = _providerId is null
            ? _viewModel.TranslationModel
            : _manager.Providers.FirstOrDefault(p => p.Id == _providerId)?.Model;
        ModelList.ItemsSource = filtered;
        ModelList.SelectedItem = filtered.FirstOrDefault(m => m.Id == selectedModel);
        EmptyText.Text = query.Length > 0 ? L("Aramayla eşleşen model yok.") : L("Model listesi boş. Yenile düğmesini kullanın.");
        EmptyText.Visibility = filtered.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void ProviderBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || _manager is null || _viewModel is null || ProviderBox.SelectedIndex < 0 || ProviderBox.SelectedIndex >= _providers.Count) return;
        var provider = _providers[ProviderBox.SelectedIndex];
        Close();
        if (!TranslationModelSelection.SelectProvider(_manager, _viewModel, provider.Id)) return;
        _providerId = provider.Id;
        _syncing = true;
        SearchBox.Text = "";
        _syncing = false;
        LoadModels();
    }

    private void SearchBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (!_syncing) FilterModels();
    }

    private void ModelList_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (_manager is null || _viewModel is null || e.ClickedItem is not TranslationModelChoice choice) return;
        if (TranslationModelSelection.SelectModel(_manager, _viewModel, _providerId, choice.Id)) ModelChosen?.Invoke();
    }

    private async void RefreshButton_Click(object sender, RoutedEventArgs e)
    {
        if (_manager is null || _viewModel is null || _providerId is not { } providerId || _manager.IsTesting) return;
        var revision = _revision;
        var cts = new CancellationTokenSource();
        _refreshCts = cts;
        RefreshButton.IsEnabled = false;
        StatusText.Text = L("Modeller yenileniyor…");
        try
        {
            var result = await _manager.TestAndFetchModelsAsync(providerId, cts.Token);
            if (cts.IsCancellationRequested || revision != _revision) return;
            // Refresh only this panel's provider. A response must never select a provider on the user's behalf.
            if (_manager.ActiveProviderId == providerId) _manager.ApplyToViewModel(_viewModel);
            LoadModels();
            StatusText.Text = result.Message;
        }
        catch (OperationCanceledException) when (cts.IsCancellationRequested) { }
        catch (Exception error)
        {
            if (revision == _revision) StatusText.Text = L("Modeller yenilenemedi: %@", error.Message);
        }
        finally
        {
            if (ReferenceEquals(_refreshCts, cts)) _refreshCts = null;
            if (revision == _revision) RefreshButton.IsEnabled = _providerId is not null;
            cts.Dispose();
        }
    }

    private void SettingsButton_Click(object sender, RoutedEventArgs e) => SettingsRequested?.Invoke();
}
