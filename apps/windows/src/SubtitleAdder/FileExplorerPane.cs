using System.ComponentModel;
using System.Diagnostics;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

/// <summary>
/// Left File Explorer sidebar component for browsing local video/audio folders and searching the persistent
/// SSD media index (<see cref="MediaLibraryIndex"/>).
/// </summary>
public sealed class FileExplorerPane : Grid
{
    private sealed record RenderedMediaRow(string FullPath, FontIcon Icon, TextBlock FileNameText)
    {
        public bool IsHighlighted { get; set; }
    }

    private readonly DropDownButton _quickLocationsButton;
    private readonly TextBlock _currentFolderText;
    private readonly MenuFlyout _quickLocationsFlyout;
    private readonly Button _backButton;
    private readonly Button _upButton;
    private readonly Button _refreshButton;
    private readonly Button _openExplorerButton;
    private readonly TextBox _searchBox;
    private readonly ListView _listView;
    private readonly StackPanel _emptyStatePanel;
    private readonly TextBlock _emptyStateText;
    private readonly ProgressRing _scanRing;
    private readonly TextBlock _statusText;
    private readonly List<RenderedMediaRow> _renderedMediaRows = [];
    private readonly List<string> _renderedFolderPaths = [];
    private readonly List<(string FullPath, long SizeBytes, string? SidecarBadge)> _renderedFileSignatures = [];

    private FileExplorerViewModel _viewModel;
    private bool _updatingSearchBox;
    private string? _activeVideoPath;
    private string? _renderedCurrentFolder;

    /// <summary>Raised when the user clicks a media file or chooses "Oynat" from its context menu.</summary>
    public event Action<string>? FileSelected;

    /// <summary>Raised when the user chooses "Çalma listesine ekle" from a media file's context menu.</summary>
    public event Action<string>? AddToPlaylistRequested;

    public FileExplorerPane()
        : this(CreateDefaultViewModel())
    {
    }

    public FileExplorerPane(FileExplorerViewModel viewModel)
    {
        _viewModel = viewModel;

        CornerRadius = new CornerRadius(8);
        BorderThickness = new Thickness(1);
        Background = Application.Current.Resources.TryGetValue("LayerFillColorDefaultBrush", out var bg) && bg is Brush bgBrush
            ? bgBrush
            : new SolidColorBrush(Colors.Transparent);
        BorderBrush = Application.Current.Resources.TryGetValue("CardStrokeColorDefaultBrush", out var stroke) && stroke is Brush strokeBrush
            ? strokeBrush
            : new SolidColorBrush(Colors.Transparent);

        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        // Row 0: Top navigation bar (Quick locations dropdown + Back / Up / Refresh / Open in Explorer)
        var topBar = new Grid
        {
            Padding = new Thickness(8, 8, 8, 6),
            ColumnSpacing = 4,
        };
        topBar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        topBar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        _quickLocationsFlyout = new MenuFlyout { Placement = Microsoft.UI.Xaml.Controls.Primitives.FlyoutPlacementMode.BottomEdgeAlignedLeft };
        _quickLocationsFlyout.Opening += QuickLocationsFlyout_Opening;

        _currentFolderText = new TextBlock
        {
            FontWeight = FontWeights.SemiBold,
            FontSize = 13,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxLines = 1,
        };

        var quickLocationsContent = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
        };
        quickLocationsContent.Children.Add(new FontIcon { Glyph = "\uE8B7", FontSize = 14 });
        quickLocationsContent.Children.Add(_currentFolderText);

        _quickLocationsButton = new DropDownButton
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Left,
            Padding = new Thickness(8, 5, 8, 5),
            Content = quickLocationsContent,
            Flyout = _quickLocationsFlyout,
        };
        Grid.SetColumn(_quickLocationsButton, 0);
        topBar.Children.Add(_quickLocationsButton);

        var navButtons = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 2,
            VerticalAlignment = VerticalAlignment.Center,
        };

        _backButton = CreateIconButton("\uE72B", async () => await _viewModel.GoBackAsync());
        _upButton = CreateIconButton("\uE74A", async () => await _viewModel.GoUpAsync());
        _refreshButton = CreateIconButton("\uE72C", async () => await _viewModel.RefreshAsync());
        _openExplorerButton = CreateIconButton("\uE8DA", OpenCurrentFolderInExplorer);

        navButtons.Children.Add(_backButton);
        navButtons.Children.Add(_upButton);
        navButtons.Children.Add(_refreshButton);
        navButtons.Children.Add(_openExplorerButton);

        Grid.SetColumn(navButtons, 1);
        topBar.Children.Add(navButtons);

        Grid.SetRow(topBar, 0);
        Children.Add(topBar);

        // Row 1: Search box
        var searchContainer = new Grid
        {
            Padding = new Thickness(8, 0, 8, 6),
        };
        _searchBox = new TextBox
        {
            FontSize = 12.5,
            Padding = new Thickness(8, 5, 8, 5),
        };
        _searchBox.TextChanged += SearchBox_TextChanged;
        searchContainer.Children.Add(_searchBox);

        Grid.SetRow(searchContainer, 1);
        Children.Add(searchContainer);

        // Row 2: Combined folder + media file list & empty state
        var listContainer = new Grid();

        _listView = new ListView
        {
            SelectionMode = ListViewSelectionMode.None,
            IsItemClickEnabled = true,
            Padding = new Thickness(4, 0, 4, 4),
        };
        _listView.ItemClick += ListView_ItemClick;
        listContainer.Children.Add(_listView);

        _emptyStateText = new TextBlock
        {
            FontSize = 12.5,
            TextAlignment = TextAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
            HorizontalAlignment = HorizontalAlignment.Center,
            Foreground = GetThemeBrush("TextFillColorSecondaryBrush", Colors.Gray),
        };
        _emptyStatePanel = new StackPanel
        {
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Spacing = 8,
            Margin = new Thickness(16),
            Visibility = Visibility.Collapsed,
            IsHitTestVisible = false,
        };
        _emptyStatePanel.Children.Add(new FontIcon
        {
            Glyph = "\uE8B7",
            FontSize = 28,
            Foreground = GetThemeBrush("TextFillColorSecondaryBrush", Colors.Gray),
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        _emptyStatePanel.Children.Add(_emptyStateText);
        listContainer.Children.Add(_emptyStatePanel);

        Grid.SetRow(listContainer, 2);
        Children.Add(listContainer);

        // Row 3: Bottom status bar (ProgressRing + StatusSummary)
        var statusBar = new Grid
        {
            Padding = new Thickness(10, 6, 10, 6),
            ColumnSpacing = 8,
            BorderThickness = new Thickness(0, 1, 0, 0),
            BorderBrush = GetThemeBrush("DividerStrokeColorDefaultBrush", Colors.Transparent),
        };
        statusBar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        statusBar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        _scanRing = new ProgressRing
        {
            Width = 13,
            Height = 13,
            IsActive = false,
            Visibility = Visibility.Collapsed,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(_scanRing, 0);
        statusBar.Children.Add(_scanRing);

        _statusText = new TextBlock
        {
            FontSize = 11.5,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            Foreground = GetThemeBrush("TextFillColorSecondaryBrush", Colors.Gray),
        };
        Grid.SetColumn(_statusText, 1);
        statusBar.Children.Add(_statusText);

        Grid.SetRow(statusBar, 3);
        Children.Add(statusBar);

        _viewModel.PropertyChanged += OnViewModelPropertyChanged;
        Loaded += OnLoaded;

        RefreshLocalization();
        SyncHeaderAndStatus();
        RebuildListItems(force: true);
    }

    public FileExplorerViewModel ViewModel => _viewModel;

    /// <summary>Optional path of the currently loaded video so the explorer can highlight it in-place without rebuilding rows.</summary>
    public string? ActiveVideoPath
    {
        get => _activeVideoPath;
        set
        {
            if (string.Equals(_activeVideoPath, value, StringComparison.OrdinalIgnoreCase)) return;
            _activeVideoPath = value;
            UpdateActiveVideoHighlight();
        }
    }

    /// <summary>Replaces the bound <see cref="FileExplorerViewModel"/> instance.</summary>
    public void AttachViewModel(FileExplorerViewModel viewModel)
    {
        if (ReferenceEquals(_viewModel, viewModel)) return;
        _viewModel.PropertyChanged -= OnViewModelPropertyChanged;
        _viewModel = viewModel;
        _viewModel.PropertyChanged += OnViewModelPropertyChanged;
        SyncHeaderAndStatus();
        RebuildListItems(force: true);
    }

    /// <summary>Updates all tooltips, placeholders, and localized labels after a UI language switch.</summary>
    public void RefreshLocalization()
    {
        ToolTipService.SetToolTip(_quickLocationsButton, L("Hızlı konumlar"));
        AutomationProperties.SetName(_quickLocationsButton, L("Hızlı konumlar"));

        ToolTipService.SetToolTip(_backButton, L("Geri"));
        AutomationProperties.SetName(_backButton, L("Geri"));

        ToolTipService.SetToolTip(_upButton, L("Üst klasör"));
        AutomationProperties.SetName(_upButton, L("Üst klasör"));

        ToolTipService.SetToolTip(_refreshButton, L("Yenile"));
        AutomationProperties.SetName(_refreshButton, L("Yenile"));

        ToolTipService.SetToolTip(_openExplorerButton, L("Dosya Gezgini'nde aç"));
        AutomationProperties.SetName(_openExplorerButton, L("Dosya Gezgini'nde aç"));

        _searchBox.PlaceholderText = L("Klasörde veya SSD indeksinde ara…");
        AutomationProperties.SetName(_searchBox, L("Klasörde veya SSD indeksinde ara…"));

        _emptyStateText.Text = L("Bu klasörde medya dosyası bulunamadı");

        SyncHeaderAndStatus();
        RebuildListItems(force: true);
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrWhiteSpace(_viewModel.CurrentFolder) ||
            (_viewModel.Folders.Count == 0 && _viewModel.Files.Count == 0 && !_viewModel.IsScanning))
        {
            await _viewModel.RefreshAsync();
        }
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(FileExplorerViewModel.CurrentFolder):
            case nameof(FileExplorerViewModel.CurrentFolderName):
            case nameof(FileExplorerViewModel.CanGoBack):
            case nameof(FileExplorerViewModel.CanGoUp):
            case nameof(FileExplorerViewModel.IsScanning):
            case nameof(FileExplorerViewModel.StatusSummary):
            case nameof(FileExplorerViewModel.TotalIndexedFiles):
                SyncHeaderAndStatus();
                break;

            case nameof(FileExplorerViewModel.SearchQuery):
                if (!_updatingSearchBox && _searchBox.Text != _viewModel.SearchQuery)
                {
                    _updatingSearchBox = true;
                    _searchBox.Text = _viewModel.SearchQuery;
                    _updatingSearchBox = false;
                }
                break;

            case nameof(FileExplorerViewModel.Folders):
            case nameof(FileExplorerViewModel.Files):
                SyncHeaderAndStatus();
                RebuildListItems(force: false);
                break;

            case "" or null:
                RefreshLocalization();
                break;
        }
    }

    private void SyncHeaderAndStatus()
    {
        _currentFolderText.Text = _viewModel.CurrentFolderName;
        if (!string.IsNullOrWhiteSpace(_viewModel.CurrentFolder))
        {
            ToolTipService.SetToolTip(_currentFolderText, _viewModel.CurrentFolder);
        }

        _backButton.IsEnabled = _viewModel.CanGoBack;
        _upButton.IsEnabled = _viewModel.CanGoUp;
        _openExplorerButton.IsEnabled = !string.IsNullOrWhiteSpace(_viewModel.CurrentFolder);

        _scanRing.IsActive = _viewModel.IsScanning;
        _scanRing.Visibility = _viewModel.IsScanning ? Visibility.Visible : Visibility.Collapsed;
        _statusText.Text = _viewModel.StatusSummary;

        var isEmpty = _viewModel.Folders.Count == 0 && _viewModel.Files.Count == 0 && !_viewModel.IsScanning;
        _emptyStatePanel.Visibility = isEmpty ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateActiveVideoHighlight()
    {
        if (_renderedMediaRows.Count == 0) return;

        var activeBrush = GetThemeBrush("AccentTextFillColorPrimaryBrush", Colors.DodgerBlue);
        var inactiveBrush = GetThemeBrush("TextFillColorSecondaryBrush", Colors.Gray);

        foreach (var row in _renderedMediaRows)
        {
            var isCurrentVideo = !string.IsNullOrWhiteSpace(_activeVideoPath) &&
                                 string.Equals(_activeVideoPath, row.FullPath, StringComparison.OrdinalIgnoreCase);
            if (row.IsHighlighted == isCurrentVideo) continue;

            row.IsHighlighted = isCurrentVideo;
            row.Icon.Foreground = isCurrentVideo ? activeBrush : inactiveBrush;
            row.FileNameText.FontWeight = isCurrentVideo ? FontWeights.SemiBold : FontWeights.Normal;
        }
    }

    private bool HasSameRenderedRows(IReadOnlyList<ExplorerFolderItem> folders, IReadOnlyList<IndexedMediaFile> files)
    {
        if (!string.Equals(_renderedCurrentFolder, _viewModel.CurrentFolder, StringComparison.OrdinalIgnoreCase))
            return false;
        if (_renderedFolderPaths.Count != folders.Count || _renderedFileSignatures.Count != files.Count)
            return false;

        for (var i = 0; i < folders.Count; i++)
        {
            if (!string.Equals(_renderedFolderPaths[i], folders[i].FullPath, StringComparison.OrdinalIgnoreCase))
                return false;
        }

        for (var i = 0; i < files.Count; i++)
        {
            var sig = _renderedFileSignatures[i];
            var file = files[i];
            if (!string.Equals(sig.FullPath, file.FullPath, StringComparison.OrdinalIgnoreCase) ||
                sig.SizeBytes != file.SizeBytes ||
                !string.Equals(sig.SidecarBadge, file.SidecarBadge, StringComparison.Ordinal))
            {
                return false;
            }
        }

        return true;
    }

    private void RebuildListItems(bool force = false)
    {
        var folders = _viewModel.Folders;
        var files = _viewModel.Files;

        var isEmpty = folders.Count == 0 && files.Count == 0 && !_viewModel.IsScanning;
        _emptyStatePanel.Visibility = isEmpty ? Visibility.Visible : Visibility.Collapsed;

        if (!force && HasSameRenderedRows(folders, files))
        {
            UpdateActiveVideoHighlight();
            return;
        }

        _listView.Items.Clear();
        _renderedMediaRows.Clear();
        _renderedFolderPaths.Clear();
        _renderedFileSignatures.Clear();
        _renderedCurrentFolder = _viewModel.CurrentFolder;

        foreach (var folder in folders)
        {
            _renderedFolderPaths.Add(folder.FullPath);
            _listView.Items.Add(CreateFolderListViewItem(folder));
        }

        foreach (var file in files)
        {
            _renderedFileSignatures.Add((file.FullPath, file.SizeBytes, file.SidecarBadge));
            _listView.Items.Add(CreateMediaFileListViewItem(file));
        }
    }

    private ListViewItem CreateFolderListViewItem(ExplorerFolderItem folder)
    {
        var row = new Grid
        {
            ColumnSpacing = 8,
            Padding = new Thickness(4, 4, 4, 4),
            Tag = folder,
        };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var icon = new FontIcon
        {
            Glyph = "\uE8B7",
            FontSize = 15,
            Foreground = GetThemeBrush("AccentTextFillColorPrimaryBrush", Colors.Goldenrod),
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(icon, 0);
        row.Children.Add(icon);

        var nameText = new TextBlock
        {
            Text = folder.Name,
            FontSize = 13,
            FontWeight = FontWeights.Medium,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxLines = 1,
        };
        Grid.SetColumn(nameText, 1);
        row.Children.Add(nameText);

        var chevron = new FontIcon
        {
            Glyph = "\uE76C",
            FontSize = 10,
            Foreground = GetThemeBrush("TextFillColorSecondaryBrush", Colors.Gray),
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(chevron, 2);
        row.Children.Add(chevron);

        var flyout = new MenuFlyout();
        var openInExplorerItem = new MenuFlyoutItem
        {
            Text = L("Dosya Gezgini'nde aç"),
            Icon = new FontIcon { Glyph = "\uE8DA" },
        };
        openInExplorerItem.Click += (_, _) => OpenPathInExplorer(folder.FullPath, selectFile: false);
        flyout.Items.Add(openInExplorerItem);

        var item = new ListViewItem
        {
            Content = row,
            Tag = folder,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Padding = new Thickness(6, 2, 6, 2),
            MinHeight = 34,
            ContextFlyout = flyout,
        };
        ToolTipService.SetToolTip(item, folder.FullPath);
        AutomationProperties.SetName(item, folder.Name);
        return item;
    }

    private ListViewItem CreateMediaFileListViewItem(IndexedMediaFile file)
    {
        var isCurrentVideo = !string.IsNullOrWhiteSpace(_activeVideoPath) &&
                             string.Equals(_activeVideoPath, file.FullPath, StringComparison.OrdinalIgnoreCase);

        var row = new Grid
        {
            ColumnSpacing = 8,
            Padding = new Thickness(4, 4, 4, 4),
            Tag = file,
        };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var icon = new FontIcon
        {
            Glyph = "\uE714",
            FontSize = 14,
            Foreground = isCurrentVideo
                ? GetThemeBrush("AccentTextFillColorPrimaryBrush", Colors.DodgerBlue)
                : GetThemeBrush("TextFillColorSecondaryBrush", Colors.Gray),
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(icon, 0);
        row.Children.Add(icon);

        var textStack = new StackPanel
        {
            Spacing = 1,
            VerticalAlignment = VerticalAlignment.Center,
        };

        var fileNameText = new TextBlock
        {
            Text = file.FileName,
            FontSize = 12.5,
            FontWeight = isCurrentVideo ? FontWeights.SemiBold : FontWeights.Normal,
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxLines = 1,
        };
        textStack.Children.Add(fileNameText);

        var isFromAnotherFolder = !string.IsNullOrWhiteSpace(_viewModel.CurrentFolder) &&
                                  !string.Equals(_viewModel.CurrentFolder, file.DirectoryPath, StringComparison.OrdinalIgnoreCase);
        var folderSuffix = isFromAnotherFolder
            ? $" · {Path.GetFileName(file.DirectoryPath.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar))}"
            : "";

        var metaText = new TextBlock
        {
            Text = file.FormattedSize + folderSuffix,
            FontSize = 11,
            Foreground = GetThemeBrush("TextFillColorSecondaryBrush", Colors.Gray),
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxLines = 1,
        };
        textStack.Children.Add(metaText);

        Grid.SetColumn(textStack, 1);
        row.Children.Add(textStack);

        if (file.HasSidecarSubtitle)
        {
            var badgeText = new TextBlock
            {
                Text = file.SidecarBadge ?? "CC",
                FontSize = 10,
                FontWeight = FontWeights.SemiBold,
                Foreground = GetThemeBrush("AccentTextFillColorPrimaryBrush", Colors.MediumSeaGreen),
            };
            var badge = new Border
            {
                CornerRadius = new CornerRadius(4),
                Padding = new Thickness(5, 1, 5, 1),
                VerticalAlignment = VerticalAlignment.Center,
                Background = GetThemeBrush("SubtleFillColorSecondaryBrush", Colors.Transparent),
                BorderBrush = GetThemeBrush("CardStrokeColorDefaultBrush", Colors.Transparent),
                BorderThickness = new Thickness(1),
                Child = badgeText,
            };
            ToolTipService.SetToolTip(badge, L("Altyazı dosyası mevcut"));
            Grid.SetColumn(badge, 2);
            row.Children.Add(badge);
        }

        _renderedMediaRows.Add(new RenderedMediaRow(file.FullPath, icon, fileNameText)
        {
            IsHighlighted = isCurrentVideo,
        });

        var flyout = new MenuFlyout();

        var playItem = new MenuFlyoutItem
        {
            Text = L("Oynat"),
            Icon = new FontIcon { Glyph = "\uE768" },
        };
        playItem.Click += (_, _) => FileSelected?.Invoke(file.FullPath);
        flyout.Items.Add(playItem);

        var addToPlaylistItem = new MenuFlyoutItem
        {
            Text = L("Çalma listesine ekle"),
            Icon = new FontIcon { Glyph = "\uE90B" },
        };
        addToPlaylistItem.Click += (_, _) => AddToPlaylistRequested?.Invoke(file.FullPath);
        flyout.Items.Add(addToPlaylistItem);

        flyout.Items.Add(new MenuFlyoutSeparator());

        var revealItem = new MenuFlyoutItem
        {
            Text = L("Dosya konumunu aç"),
            Icon = new FontIcon { Glyph = "\uE8DA" },
        };
        revealItem.Click += (_, _) => OpenPathInExplorer(file.FullPath, selectFile: true);
        flyout.Items.Add(revealItem);

        var item = new ListViewItem
        {
            Content = row,
            Tag = file,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Padding = new Thickness(6, 2, 6, 2),
            MinHeight = 40,
            ContextFlyout = flyout,
        };
        ToolTipService.SetToolTip(item, file.FullPath);
        AutomationProperties.SetName(item, $"{file.FileName} ({file.FormattedSize})");
        return item;
    }

    private async void ListView_ItemClick(object sender, ItemClickEventArgs e)
    {
        var payload = (e.ClickedItem as FrameworkElement)?.Tag ?? e.ClickedItem;
        switch (payload)
        {
            case ExplorerFolderItem folder:
                await _viewModel.NavigateToAsync(folder.FullPath);
                break;
            case IndexedMediaFile file:
                FileSelected?.Invoke(file.FullPath);
                break;
        }
    }

    private void SearchBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_updatingSearchBox) return;
        _updatingSearchBox = true;
        _viewModel.SearchQuery = _searchBox.Text;
        _updatingSearchBox = false;
    }

    private void QuickLocationsFlyout_Opening(object? sender, object e)
    {
        _quickLocationsFlyout.Items.Clear();
        foreach (var location in _viewModel.QuickLocations)
        {
            var menuItem = new MenuFlyoutItem
            {
                Text = location.Name,
                Icon = new FontIcon { Glyph = location.IconGlyph },
            };
            var targetPath = location.FullPath;
            menuItem.Click += async (_, _) => await _viewModel.NavigateToAsync(targetPath);
            _quickLocationsFlyout.Items.Add(menuItem);
        }
    }

    private void OpenCurrentFolderInExplorer()
    {
        if (!string.IsNullOrWhiteSpace(_viewModel.CurrentFolder))
        {
            OpenPathInExplorer(_viewModel.CurrentFolder, selectFile: false);
        }
    }

    private static void OpenPathInExplorer(string path, bool selectFile)
    {
        if (string.IsNullOrWhiteSpace(path)) return;
        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = selectFile ? $"/select,\"{path}\"" : $"\"{path}\"",
                UseShellExecute = true,
            };
            Process.Start(psi);
        }
        catch (Exception)
        {
        }
    }

    private static Button CreateIconButton(string glyph, Action onClick)
    {
        var button = new Button
        {
            Padding = new Thickness(6),
            MinWidth = 28,
            MinHeight = 28,
            Background = new SolidColorBrush(Colors.Transparent),
            BorderBrush = new SolidColorBrush(Colors.Transparent),
            VerticalAlignment = VerticalAlignment.Center,
            Content = new FontIcon { Glyph = glyph, FontSize = 13 },
        };
        button.Click += (_, _) => onClick();
        return button;
    }

    private static Brush GetThemeBrush(string key, Windows.UI.Color fallback)
    {
        if (Application.Current?.Resources is { } res && res.TryGetValue(key, out var val) && val is Brush brush)
        {
            return brush;
        }
        return new SolidColorBrush(fallback);
    }

    private static FileExplorerViewModel CreateDefaultViewModel()
    {
        var settings = (Application.Current as App)?.Settings ?? new InMemorySettingsStore();
        IUiDispatcher dispatcher = SynchronizationContext.Current is not null
            ? new SynchronizationContextDispatcher()
            : ImmediateDispatcher.Instance;
        return new FileExplorerViewModel(new MediaLibraryIndex(), settings, dispatcher);
    }
}
