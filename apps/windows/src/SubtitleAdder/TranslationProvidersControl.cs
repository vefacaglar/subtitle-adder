using System.ComponentModel;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

/// <summary>
/// Self-contained WinUI 3 settings panel for managing multiple API translation providers
/// (OpenCode Go, OpenRouter, OpenAI, DeepSeek, Groq, Google Gemini, Local LAN, Custom OpenAI-compatible)
/// and switching between local on-device models and cloud/LAN API providers.
/// </summary>
public sealed class TranslationProvidersControl : StackPanel
{
    private readonly TranslationProviderManager _manager;
    private readonly SubtitleViewModel? _viewModel;

    private readonly TextBlock _sectionTitle;
    private readonly StackPanel _providersList;
    private readonly InfoBar _statusBar;

    private readonly TextBlock _formHeader;
    private readonly ComboBox _presetBox;
    private readonly TextBlock _nameLabel;
    private readonly TextBox _nameBox;
    private readonly TextBlock _urlLabel;
    private readonly TextBox _urlBox;
    private readonly TextBlock _apiKeyLabel;
    private readonly PasswordBox _apiKeyBox;
    private readonly TextBlock _modelLabel;
    private readonly ComboBox _modelBox;
    private readonly TextBlock _asyncTitle;
    private readonly TextBlock _asyncSubtitle;
    private readonly ToggleSwitch _asyncSwitch;
    private readonly Button _saveButton;
    private readonly Button _testDraftButton;
    private readonly Button _cancelEditButton;

    private Guid? _editingProviderId;
    private bool _syncing;

    public TranslationProvidersControl()
        : this(TranslationProviderManager.Shared, TryGetAppViewModel())
    {
    }

    public TranslationProvidersControl(TranslationProviderManager manager, SubtitleViewModel? viewModel = null)
    {
        _manager = manager;
        _viewModel = viewModel;
        Spacing = 6;

        _sectionTitle = new TextBlock
        {
            Style = TryFindStyle("SectionHeaderStyle") ?? TryFindStyle("BodyStrongTextBlockStyle"),
            Margin = new Thickness(1, 24, 0, 6),
        };
        Children.Add(_sectionTitle);

        _providersList = new StackPanel { Spacing = 4 };
        Children.Add(_providersList);

        _statusBar = new InfoBar
        {
            IsOpen = false,
            IsClosable = true,
            Margin = new Thickness(0, 2, 0, 2),
        };
        Children.Add(_statusBar);

        // Add / Edit Provider Form Card
        var formCard = CreateCard();
        var formStack = new StackPanel { Spacing = 10 };

        _formHeader = new TextBlock
        {
            FontWeight = FontWeights.SemiBold,
            FontSize = 14,
        };
        formStack.Children.Add(_formHeader);

        // Preset + Name row
        var presetNameGrid = new Grid { ColumnSpacing = 12 };
        presetNameGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        presetNameGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var presetPanel = new StackPanel { Spacing = 4 };
        var presetLabel = new TextBlock { FontSize = 12, Opacity = 0.8 };
        Localize.SetText(presetLabel, "Sağlayıcı türü");
        _presetBox = new ComboBox { HorizontalAlignment = HorizontalAlignment.Stretch };
        _presetBox.SelectionChanged += PresetBox_SelectionChanged;
        presetPanel.Children.Add(presetLabel);
        presetPanel.Children.Add(_presetBox);
        Grid.SetColumn(presetPanel, 0);
        presetNameGrid.Children.Add(presetPanel);

        var namePanel = new StackPanel { Spacing = 4 };
        _nameLabel = new TextBlock { FontSize = 12, Opacity = 0.8 };
        _nameBox = new TextBox { HorizontalAlignment = HorizontalAlignment.Stretch };
        namePanel.Children.Add(_nameLabel);
        namePanel.Children.Add(_nameBox);
        Grid.SetColumn(namePanel, 1);
        presetNameGrid.Children.Add(namePanel);

        formStack.Children.Add(presetNameGrid);

        // Base URL + API Key row
        var urlKeyGrid = new Grid { ColumnSpacing = 12 };
        urlKeyGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        urlKeyGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var urlPanel = new StackPanel { Spacing = 4 };
        _urlLabel = new TextBlock { FontSize = 12, Opacity = 0.8 };
        _urlBox = new TextBox
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            PlaceholderText = "https://opencode.ai/zen/go/v1",
        };
        urlPanel.Children.Add(_urlLabel);
        urlPanel.Children.Add(_urlBox);
        Grid.SetColumn(urlPanel, 0);
        urlKeyGrid.Children.Add(urlPanel);

        var keyPanel = new StackPanel { Spacing = 4 };
        _apiKeyLabel = new TextBlock { FontSize = 12, Opacity = 0.8 };
        _apiKeyBox = new PasswordBox { HorizontalAlignment = HorizontalAlignment.Stretch };
        keyPanel.Children.Add(_apiKeyLabel);
        keyPanel.Children.Add(_apiKeyBox);
        Grid.SetColumn(keyPanel, 1);
        urlKeyGrid.Children.Add(keyPanel);

        formStack.Children.Add(urlKeyGrid);

        // Model row
        var modelPanel = new StackPanel { Spacing = 4 };
        _modelLabel = new TextBlock { FontSize = 12, Opacity = 0.8 };
        _modelBox = new ComboBox
        {
            IsEditable = true,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        modelPanel.Children.Add(_modelLabel);
        modelPanel.Children.Add(_modelBox);
        formStack.Children.Add(modelPanel);

        // Async switch row
        var asyncGrid = new Grid { ColumnSpacing = 12 };
        asyncGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        asyncGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var asyncTextStack = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        _asyncTitle = new TextBlock { FontSize = 13 };
        _asyncSubtitle = new TextBlock { FontSize = 11, Opacity = 0.7, TextWrapping = TextWrapping.Wrap };
        asyncTextStack.Children.Add(_asyncTitle);
        asyncTextStack.Children.Add(_asyncSubtitle);
        Grid.SetColumn(asyncTextStack, 0);
        asyncGrid.Children.Add(asyncTextStack);

        _asyncSwitch = new ToggleSwitch
        {
            IsOn = true,
            MinWidth = 0,
            OnContent = "",
            OffContent = "",
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(_asyncSwitch, 1);
        asyncGrid.Children.Add(_asyncSwitch);
        formStack.Children.Add(asyncGrid);

        // Action buttons row
        var buttonsPanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Margin = new Thickness(0, 4, 0, 0),
        };

        _saveButton = new Button();
        if (TryFindStyle("AccentButtonStyle") is { } accentStyle)
            _saveButton.Style = accentStyle;
        _saveButton.Click += SaveButton_Click;

        _testDraftButton = new Button();
        _testDraftButton.Click += TestDraftButton_Click;

        _cancelEditButton = new Button { Visibility = Visibility.Collapsed };
        _cancelEditButton.Click += CancelEditButton_Click;

        buttonsPanel.Children.Add(_saveButton);
        buttonsPanel.Children.Add(_testDraftButton);
        buttonsPanel.Children.Add(_cancelEditButton);
        formStack.Children.Add(buttonsPanel);

        formCard.Child = formStack;
        Children.Add(formCard);

        Loaded += OnLoaded;
        Unloaded += OnUnloaded;

        PopulatePresets();
        UpdateLocalizedLabels();
        RebuildProvidersList();
    }

    private static SubtitleViewModel? TryGetAppViewModel()
    {
        try
        {
            return App.Current?.ViewModel;
        }
        catch (Exception)
        {
            return null;
        }
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        _manager.Changed += OnManagerChanged;
        LocalizedStrings.Shared.PropertyChanged += OnLocalizationChanged;
        PopulatePresets();
        UpdateLocalizedLabels();
        RebuildProvidersList();
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _manager.Changed -= OnManagerChanged;
        LocalizedStrings.Shared.PropertyChanged -= OnLocalizationChanged;
    }

    private void OnManagerChanged(object? sender, EventArgs e)
    {
        _manager.ApplyToViewModel(_viewModel);
        RebuildProvidersList();
    }

    private void OnLocalizationChanged(object? sender, PropertyChangedEventArgs e)
    {
        PopulatePresets();
        UpdateLocalizedLabels();
        RebuildProvidersList();
    }

    private void UpdateLocalizedLabels()
    {
        _sectionTitle.Text = L("Çeviri sağlayıcıları ve API anahtarları");
        _formHeader.Text = _editingProviderId is null
            ? L("API Sağlayıcısı Ekle")
            : L("API Sağlayıcısını Düzenle");
        _nameLabel.Text = L("Sağlayıcı adı");
        _nameBox.PlaceholderText = L("Örn. OpenCode Go");
        _urlLabel.Text = L("Sunucu adresi (Base URL)");
        _apiKeyLabel.Text = L("API anahtarı");
        _apiKeyBox.PlaceholderText = L("sk-... (Yerel ağ için boş bırakılabilir)");
        _modelLabel.Text = L("Model");
        _modelBox.PlaceholderText = L("Model seçin veya yazın (örn. glm-5, gpt-4o-mini)");
        _asyncTitle.Text = L("Asenkron çeviri");
        _asyncSubtitle.Text = L("Açıkken konuşma metne dökülürken aynı anda arka planda çeviri yapılır.");
        _saveButton.Content = _editingProviderId is null
            ? L("Kaydet ve Etkinleştir")
            : L("Güncelle");
        _testDraftButton.Content = L("Bağlantıyı test et");
        _cancelEditButton.Content = L("Vazgeç");
    }

    private void PopulatePresets()
    {
        _syncing = true;
        try
        {
            var selectedIdx = Math.Max(0, _presetBox.SelectedIndex);
            _presetBox.Items.Clear();
            foreach (var preset in TranslationProviderManager.Presets)
            {
                _presetBox.Items.Add(preset.Name);
            }
            if (_presetBox.Items.Count > 0)
            {
                _presetBox.SelectedIndex = Math.Clamp(selectedIdx, 0, _presetBox.Items.Count - 1);
            }
        }
        finally
        {
            _syncing = false;
        }
        if (_editingProviderId is null)
        {
            ApplySelectedPresetDefaults();
        }
    }

    private TranslationProviderPreset SelectedPreset()
    {
        var idx = _presetBox.SelectedIndex;
        if (idx >= 0 && idx < TranslationProviderManager.Presets.Count)
            return TranslationProviderManager.Presets[idx];
        return TranslationProviderManager.Presets[0];
    }

    private void PresetBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing) return;
        ApplySelectedPresetDefaults();
    }

    private void ApplySelectedPresetDefaults()
    {
        var preset = SelectedPreset();
        _syncing = true;
        try
        {
            _nameBox.Text = preset.Name;
            _urlBox.Text = preset.BaseUrl;
            _modelBox.Items.Clear();
            foreach (var m in preset.SuggestedModels)
            {
                _modelBox.Items.Add(m);
            }
            var firstModel = preset.SuggestedModels.FirstOrDefault() ?? "";
            _modelBox.SelectedItem = firstModel.Length > 0 ? firstModel : null;
            _modelBox.Text = firstModel;
        }
        finally
        {
            _syncing = false;
        }
    }

    private void RebuildProvidersList()
    {
        _providersList.Children.Clear();

        // 0. Local on-device models option card
        var localCard = CreateCard();
        var localGrid = new Grid { ColumnSpacing = 12 };
        localGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var localRadio = new RadioButton
        {
            GroupName = "ActiveTranslationProviderGroup",
            IsChecked = !_manager.IsExternalProviderActive,
            Content = new StackPanel
            {
                Spacing = 2,
                Children =
                {
                    new TextBlock
                    {
                        Text = L("Yerel modeller (Cihaz üzerinde)"),
                        FontWeight = FontWeights.SemiBold,
                    },
                    new TextBlock
                    {
                        Text = L("Çeviri tamamen bu bilgisayarda indirilen yerel modellerle yapılır."),
                        FontSize = 12,
                        Opacity = 0.7,
                        TextWrapping = TextWrapping.Wrap,
                    },
                },
            },
        };
        localRadio.Checked += (_, _) =>
        {
            if (_syncing) return;
            _manager.SetActiveProvider(null);
            _manager.ApplyToViewModel(_viewModel);
        };
        localGrid.Children.Add(localRadio);
        localCard.Child = localGrid;
        _providersList.Children.Add(localCard);

        // 1..N. Saved API provider cards
        foreach (var profile in _manager.Providers)
        {
            _providersList.Children.Add(CreateProviderCard(profile));
        }
    }

    private Border CreateProviderCard(TranslationProviderProfile profile)
    {
        var card = CreateCard();
        var isActive = _manager.ActiveProviderId == profile.Id;

        var rootStack = new StackPanel { Spacing = 8 };

        // Top row: RadioButton (Name + Preset badge + URL/Key summary) + Action buttons (Test, Edit, Delete)
        var topGrid = new Grid { ColumnSpacing = 12 };
        topGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        topGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var headerRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        headerRow.Children.Add(new TextBlock
        {
            Text = profile.Name,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        });

        var badge = new Border
        {
            CornerRadius = new CornerRadius(4),
            Padding = new Thickness(6, 2, 6, 2),
            VerticalAlignment = VerticalAlignment.Center,
            BorderThickness = new Thickness(1),
            Child = new TextBlock
            {
                Text = profile.PresetName,
                FontSize = 11,
                Opacity = 0.85,
            },
        };
        headerRow.Children.Add(badge);

        var radioContent = new StackPanel { Spacing = 2 };
        radioContent.Children.Add(headerRow);
        radioContent.Children.Add(new TextBlock
        {
            Text = $"{profile.BaseUrl} · {profile.MaskedApiKey}",
            FontSize = 12,
            Opacity = 0.7,
            TextWrapping = TextWrapping.Wrap,
        });

        var radio = new RadioButton
        {
            GroupName = "ActiveTranslationProviderGroup",
            IsChecked = isActive,
            Content = radioContent,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var capturedId = profile.Id;
        radio.Checked += (_, _) =>
        {
            if (_syncing) return;
            _manager.SetActiveProvider(capturedId);
            _manager.ApplyToViewModel(_viewModel);
        };
        Grid.SetColumn(radio, 0);
        topGrid.Children.Add(radio);

        var actionsRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };

        var testBtn = new Button { Content = L("Test et") };
        AutomationProperties.SetName(testBtn, L("Test et"));
        testBtn.Click += async (_, _) =>
        {
            testBtn.IsEnabled = false;
            try
            {
                var (ok, msg, _) = await _manager.TestAndFetchModelsAsync(capturedId);
                ShowStatus(ok, msg);
            }
            finally
            {
                testBtn.IsEnabled = true;
            }
        };

        var editBtn = new Button { Content = L("Düzenle") };
        AutomationProperties.SetName(editBtn, L("Düzenle"));
        editBtn.Click += (_, _) => BeginEditProvider(profile);

        var deleteBtn = new Button { Content = L("Sil") };
        AutomationProperties.SetName(deleteBtn, L("Sil"));
        deleteBtn.Click += (_, _) =>
        {
            if (_editingProviderId == capturedId) ResetEditForm();
            _manager.RemoveProvider(capturedId);
            _manager.ApplyToViewModel(_viewModel);
        };

        actionsRow.Children.Add(testBtn);
        actionsRow.Children.Add(editBtn);
        actionsRow.Children.Add(deleteBtn);
        Grid.SetColumn(actionsRow, 1);
        topGrid.Children.Add(actionsRow);

        rootStack.Children.Add(topGrid);

        // Bottom row: Quick Model Picker for this provider
        var modelRow = new Grid { ColumnSpacing = 8, Margin = new Thickness(28, 0, 0, 0) };
        modelRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        modelRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var modelCaption = new TextBlock
        {
            Text = L("Model:"),
            VerticalAlignment = VerticalAlignment.Center,
            FontSize = 12,
            Opacity = 0.8,
        };
        Grid.SetColumn(modelCaption, 0);
        modelRow.Children.Add(modelCaption);

        var modelCombo = new ComboBox
        {
            IsEditable = true,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            PlaceholderText = L("Model seçin veya yazın"),
        };
        foreach (var m in profile.AvailableModels)
        {
            modelCombo.Items.Add(m);
        }
        modelCombo.SelectedItem = profile.Model;
        modelCombo.Text = profile.Model;

        modelCombo.SelectionChanged += (_, _) =>
        {
            if (_syncing) return;
            if (modelCombo.SelectedItem is string selected && !string.IsNullOrWhiteSpace(selected))
            {
                _manager.UpdateProviderModel(capturedId, selected);
                _manager.ApplyToViewModel(_viewModel);
            }
        };
        modelCombo.TextSubmitted += (_, args) =>
        {
            if (_syncing) return;
            if (!string.IsNullOrWhiteSpace(args.Text))
            {
                _manager.UpdateProviderModel(capturedId, args.Text);
                _manager.ApplyToViewModel(_viewModel);
            }
        };

        Grid.SetColumn(modelCombo, 1);
        modelRow.Children.Add(modelCombo);

        rootStack.Children.Add(modelRow);

        card.Child = rootStack;
        return card;
    }

    private void BeginEditProvider(TranslationProviderProfile profile)
    {
        _editingProviderId = profile.Id;
        _syncing = true;
        try
        {
            var presetIdx = 0;
            for (var i = 0; i < TranslationProviderManager.Presets.Count; i++)
            {
                if (string.Equals(TranslationProviderManager.Presets[i].Id, profile.PresetId, StringComparison.OrdinalIgnoreCase))
                {
                    presetIdx = i;
                    break;
                }
            }
            _presetBox.SelectedIndex = presetIdx;
            _nameBox.Text = profile.Name;
            _urlBox.Text = profile.BaseUrl;
            _apiKeyBox.Password = profile.ApiKey;
            _modelBox.Items.Clear();
            foreach (var m in profile.AvailableModels)
            {
                _modelBox.Items.Add(m);
            }
            _modelBox.SelectedItem = profile.Model;
            _modelBox.Text = profile.Model;
            _asyncSwitch.IsOn = profile.AsyncTranslation;
            _cancelEditButton.Visibility = Visibility.Visible;
        }
        finally
        {
            _syncing = false;
        }
        UpdateLocalizedLabels();
    }

    private void ResetEditForm()
    {
        _editingProviderId = null;
        _apiKeyBox.Password = "";
        _asyncSwitch.IsOn = true;
        _cancelEditButton.Visibility = Visibility.Collapsed;
        ApplySelectedPresetDefaults();
        UpdateLocalizedLabels();
    }

    private void CancelEditButton_Click(object sender, RoutedEventArgs e) => ResetEditForm();

    private void SaveButton_Click(object sender, RoutedEventArgs e)
    {
        var preset = SelectedPreset();
        var name = _nameBox.Text.Trim();
        var url = _urlBox.Text.Trim();
        var apiKey = _apiKeyBox.Password.Trim();
        var model = (_modelBox.SelectedItem as string ?? _modelBox.Text ?? "").Trim();
        var asyncTrans = _asyncSwitch.IsOn;

        if (string.IsNullOrWhiteSpace(url))
        {
            ShowStatus(false, L("Lütfen geçerli bir sunucu adresi girin."));
            return;
        }

        if (_editingProviderId is { } editId)
        {
            _manager.UpdateProvider(editId, name, url, apiKey, model, asyncTrans);
            _manager.SetActiveProvider(editId);
            _manager.ApplyToViewModel(_viewModel);
            ResetEditForm();
            ShowStatus(true, L("Sağlayıcı güncellendi ve etkinleştirildi."));
        }
        else
        {
            var added = _manager.AddProvider(
                presetId: preset.Id,
                name: name,
                baseUrl: url,
                apiKey: apiKey,
                model: model,
                asyncTranslation: asyncTrans,
                setActive: true);
            _manager.ApplyToViewModel(_viewModel);
            ResetEditForm();
            ShowStatus(true, L("%@ kaydedildi ve etkinleştirildi.", added.Name));
        }
    }

    private async void TestDraftButton_Click(object sender, RoutedEventArgs e)
    {
        var preset = SelectedPreset();
        var url = _urlBox.Text.Trim();
        var apiKey = _apiKeyBox.Password.Trim();
        var currentModel = (_modelBox.SelectedItem as string ?? _modelBox.Text ?? "").Trim();

        _testDraftButton.IsEnabled = false;
        try
        {
            var (ok, msg, models) = await _manager.TestAndFetchModelsAsync(preset.Id, url, apiKey, currentModel);
            _syncing = true;
            try
            {
                _modelBox.Items.Clear();
                foreach (var m in models)
                {
                    _modelBox.Items.Add(m);
                }
                var chosen = !string.IsNullOrWhiteSpace(currentModel) ? currentModel : (models.FirstOrDefault() ?? "");
                _modelBox.SelectedItem = chosen;
                _modelBox.Text = chosen;
            }
            finally
            {
                _syncing = false;
            }
            ShowStatus(ok, msg);
        }
        finally
        {
            _testDraftButton.IsEnabled = true;
        }
    }

    private void ShowStatus(bool success, string message)
    {
        _statusBar.Severity = success ? InfoBarSeverity.Success : InfoBarSeverity.Error;
        _statusBar.Message = message;
        _statusBar.IsOpen = true;
    }

    private static Border CreateCard()
    {
        if (TryFindStyle("CardStyle") is { } cardStyle)
            return new Border { Style = cardStyle };

        return new Border
        {
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(16, 12, 16, 12),
            BorderThickness = new Thickness(1),
        };
    }

    private static Style? TryFindStyle(string key)
    {
        try
        {
            if (Application.Current?.Resources.TryGetValue(key, out var val) == true && val is Style style)
                return style;
        }
        catch (Exception)
        {
        }
        return null;
    }
}
