using System.ComponentModel;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using SubtitleAdder.Core;
using Windows.Foundation;
using Windows.Storage;
using Windows.Storage.FileProperties;
using Windows.Storage.Streams;
using Windows.System;
using static SubtitleAdder.Core.Strings;

namespace SubtitleAdder;

/// <summary>
/// Top-toolbar button showing the active local profile (<c>CurrentUsername</c>) or <c>"Profil / Giriş"</c> when logged out.
/// Clicking opens a <see cref="Flyout"/> for logging in, registering a new profile, viewing watch/like counts, or logging out.
/// </summary>
public sealed class ProfileStatusButton : Grid
{
    private VideoPlatformViewModel _vm;
    private readonly Button _button;
    private readonly FontIcon _statusIcon;
    private readonly TextBlock _statusLabel;
    private readonly Flyout _flyout;

    // Logged-out flyout elements
    private readonly StackPanel _loggedOutPanel;
    private readonly TextBlock _flyoutTitleLoggedOut;
    private readonly TextBlock _flyoutSubtitleLoggedOut;
    private readonly StackPanel _registeredChipsPanel;
    private readonly TextBox _usernameBox;
    private readonly PasswordBox _passwordBox;
    private readonly CheckBox _rememberCheckBox;
    private readonly TextBlock _errorText;
    private readonly Button _loginButton;
    private readonly Button _registerButton;

    // Logged-in flyout elements
    private readonly StackPanel _loggedInPanel;
    private readonly TextBlock _activeUserHeader;
    private readonly TextBlock _summaryText;
    private readonly Button _openHubButton;
    private readonly Button _logoutButton;

    public event Action? OpenPlatformHubRequested;

    public ProfileStatusButton() : this(null)
    {
    }

    public ProfileStatusButton(VideoPlatformViewModel? viewModel)
    {
        _vm = viewModel ?? VideoPlatformViewModel.Shared;

        _statusIcon = new FontIcon
        {
            Glyph = "\uE77B",
            FontSize = 13,
            VerticalAlignment = VerticalAlignment.Center,
        };

        _statusLabel = new TextBlock
        {
            FontSize = 12.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            MaxWidth = 140,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };

        var buttonContent = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        buttonContent.Children.Add(_statusIcon);
        buttonContent.Children.Add(_statusLabel);

        _button = new Button
        {
            Padding = new Thickness(10, 5, 10, 5),
            CornerRadius = new CornerRadius(6),
            VerticalAlignment = VerticalAlignment.Center,
            Content = buttonContent,
        };

        // Build Flyout
        _flyoutTitleLoggedOut = new TextBlock
        {
            FontSize = 14,
            FontWeight = FontWeights.SemiBold,
        };
        _flyoutSubtitleLoggedOut = new TextBlock
        {
            FontSize = 12,
            Opacity = 0.75,
            TextWrapping = TextWrapping.Wrap,
        };

        _registeredChipsPanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            Visibility = Visibility.Collapsed,
        };

        _usernameBox = new TextBox
        {
            Width = 248,
        };
        _passwordBox = new PasswordBox
        {
            Width = 248,
        };
        _passwordBox.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                AttemptLogin();
                e.Handled = true;
            }
        };

        _rememberCheckBox = new CheckBox
        {
            IsChecked = _vm.RememberMe,
            FontSize = 12,
            MinWidth = 0,
        };
        _rememberCheckBox.Checked += (_, _) => _vm.RememberMe = true;
        _rememberCheckBox.Unchecked += (_, _) => _vm.RememberMe = false;

        _errorText = new TextBlock
        {
            FontSize = 12,
            Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0x6B, 0x6B)),
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };

        _loginButton = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Style = Application.Current?.Resources.TryGetValue("AccentButtonStyle", out var accent) == true
                ? accent as Style
                : null,
        };
        _loginButton.Click += (_, _) => AttemptLogin();

        _registerButton = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        _registerButton.Click += (_, _) => AttemptRegister();

        _loggedOutPanel = new StackPanel
        {
            Spacing = 8,
            Width = 252,
        };
        _loggedOutPanel.Children.Add(_flyoutTitleLoggedOut);
        _loggedOutPanel.Children.Add(_flyoutSubtitleLoggedOut);
        _loggedOutPanel.Children.Add(_registeredChipsPanel);
        _loggedOutPanel.Children.Add(_usernameBox);
        _loggedOutPanel.Children.Add(_passwordBox);
        _loggedOutPanel.Children.Add(_rememberCheckBox);
        _loggedOutPanel.Children.Add(_errorText);
        _loggedOutPanel.Children.Add(_loginButton);
        _loggedOutPanel.Children.Add(_registerButton);

        // Logged-in panel
        _activeUserHeader = new TextBlock
        {
            FontSize = 15,
            FontWeight = FontWeights.SemiBold,
        };
        _summaryText = new TextBlock
        {
            FontSize = 12.5,
            Opacity = 0.8,
            TextWrapping = TextWrapping.Wrap,
        };

        _openHubButton = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        _openHubButton.Click += (_, _) =>
        {
            _flyout?.Hide();
            OpenPlatformHubRequested?.Invoke();
        };

        _logoutButton = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        _logoutButton.Click += (_, _) =>
        {
            _vm.Logout();
            _passwordBox.Password = "";
            Refresh();
        };

        _loggedInPanel = new StackPanel
        {
            Spacing = 10,
            Width = 240,
            Visibility = Visibility.Collapsed,
        };
        _loggedInPanel.Children.Add(_activeUserHeader);
        _loggedInPanel.Children.Add(_summaryText);
        _loggedInPanel.Children.Add(_openHubButton);
        _loggedInPanel.Children.Add(_logoutButton);

        var flyoutRoot = new Grid();
        flyoutRoot.Children.Add(_loggedOutPanel);
        flyoutRoot.Children.Add(_loggedInPanel);

        _flyout = new Flyout
        {
            Placement = FlyoutPlacementMode.BottomEdgeAlignedRight,
            Content = flyoutRoot,
        };
        _flyout.Opened += (_, _) => Refresh();
        _button.Flyout = _flyout;

        Children.Add(_button);

        Subscribe(_vm);
        Unloaded += (_, _) => Unsubscribe(_vm);
        Loaded += (_, _) =>
        {
            Unsubscribe(_vm);
            Subscribe(_vm);
            Refresh();
        };

        Refresh();
    }

    public VideoPlatformViewModel ViewModel
    {
        get => _vm;
        set
        {
            var next = value ?? VideoPlatformViewModel.Shared;
            if (ReferenceEquals(_vm, next)) return;
            Unsubscribe(_vm);
            _vm = next;
            Subscribe(_vm);
            Refresh();
        }
    }

    private void Subscribe(VideoPlatformViewModel vm)
    {
        vm.PropertyChanged += OnViewModelPropertyChanged;
        vm.LocalizationChanged += OnLocalizationChanged;
    }

    private void Unsubscribe(VideoPlatformViewModel vm)
    {
        vm.PropertyChanged -= OnViewModelPropertyChanged;
        vm.LocalizationChanged -= OnLocalizationChanged;
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e) => SafeRefresh();
    private void OnLocalizationChanged(object? sender, EventArgs e) => SafeRefresh();

    private void SafeRefresh()
    {
        if (DispatcherQueue is { } dq && !dq.HasThreadAccess)
        {
            dq.TryEnqueue(Refresh);
            return;
        }
        Refresh();
    }

    private void AttemptLogin()
    {
        if (_vm.TryLogin(_usernameBox.Text, _passwordBox.Password, _rememberCheckBox.IsChecked == true))
        {
            _passwordBox.Password = "";
            Refresh();
        }
        else
        {
            RefreshError();
        }
    }

    private void AttemptRegister()
    {
        if (_vm.TryRegister(_usernameBox.Text, _passwordBox.Password, _rememberCheckBox.IsChecked == true))
        {
            _passwordBox.Password = "";
            Refresh();
        }
        else
        {
            RefreshError();
        }
    }

    private void RefreshError()
    {
        if (!string.IsNullOrWhiteSpace(_vm.AuthError))
        {
            _errorText.Text = _vm.AuthError;
            _errorText.Visibility = Visibility.Visible;
        }
        else
        {
            _errorText.Text = "";
            _errorText.Visibility = Visibility.Collapsed;
        }
    }

    public void Refresh()
    {
        var loggedIn = _vm.IsLoggedIn;
        _statusIcon.Glyph = loggedIn ? "\uE77B" : "\uE72E";
        _statusLabel.Text = _vm.ProfileStatusText;
        ToolTipService.SetToolTip(_button, loggedIn
            ? L("Aktif profil: %@ (%@)", _vm.CurrentUsername ?? "", _vm.ProfileSummaryText)
            : L("İzleme geçmişi, beğeni ve puanlama için giriş yapın"));
        AutomationProperties.SetName(_button, _vm.ProfileStatusText);

        _flyoutTitleLoggedOut.Text = L("Profil / Giriş");
        _flyoutSubtitleLoggedOut.Text = L("İzleme geçmişi, görüntülenme sayısı, beğeni ve yıldız puanları profilinize özel kaydedilir.");
        _usernameBox.PlaceholderText = L("Kullanıcı adı");
        _passwordBox.PlaceholderText = L("Şifre");
        _rememberCheckBox.Content = L("Girişi hatırla");
        _rememberCheckBox.IsChecked = _vm.RememberMe;
        _loginButton.Content = L("Giriş yap");
        _registerButton.Content = L("Yeni profil oluştur");

        _openHubButton.Content = L("Video Platformu");
        _logoutButton.Content = L("Çıkış yap");

        _loggedOutPanel.Visibility = loggedIn ? Visibility.Collapsed : Visibility.Visible;
        _loggedInPanel.Visibility = loggedIn ? Visibility.Visible : Visibility.Collapsed;

        if (loggedIn)
        {
            _activeUserHeader.Text = _vm.CurrentUsername ?? "";
            _summaryText.Text = _vm.ProfileSummaryText;
        }
        else
        {
            if (string.IsNullOrWhiteSpace(_usernameBox.Text) && !string.IsNullOrWhiteSpace(_vm.LastUsername))
            {
                _usernameBox.Text = _vm.LastUsername;
            }
            RebuildRegisteredUserChips();
            RefreshError();
        }
    }

    private void RebuildRegisteredUserChips()
    {
        _registeredChipsPanel.Children.Clear();
        var users = _vm.RegisteredUsernames;
        if (users.Count == 0)
        {
            _registeredChipsPanel.Visibility = Visibility.Collapsed;
            return;
        }

        _registeredChipsPanel.Visibility = Visibility.Visible;
        foreach (var username in users.Take(4))
        {
            var captured = username;
            var chip = new Button
            {
                Content = captured,
                FontSize = 11.5,
                Padding = new Thickness(7, 2, 7, 2),
                CornerRadius = new CornerRadius(10),
            };
            chip.Click += (_, _) =>
            {
                _usernameBox.Text = captured;
                _passwordBox.Focus(FocusState.Programmatic);
            };
            _registeredChipsPanel.Children.Add(chip);
        }
    }
}

/// <summary>
/// Sleek YouTube-style engagement bar for the currently open video.
/// Visible when <c>IsLoggedIn &amp;&amp; ActiveVideoPath is not null</c>.
/// Shows view count badge (<c>\uE7B3</c>), interactive Like button (<c>\uEB51</c>/<c>\uEB52</c>),
/// interactive 5-star rating bar (<c>\uE734</c>/<c>\uE735</c>), and "+ Listeye ekle" menu button.
/// </summary>
public sealed class PlayerEngagementBar : Grid
{
    private VideoPlatformViewModel _vm;
    private readonly TextBlock _viewsText;
    private readonly Button _likeButton;
    private readonly FontIcon _likeIcon;
    private readonly TextBlock _likeText;
    private readonly Button[] _starButtons = new Button[5];
    private readonly FontIcon[] _starIcons = new FontIcon[5];
    private readonly Button _addToListButton;
    private readonly TextBlock _addToListLabel;
    private readonly MenuFlyout _addToListFlyout;

    public PlayerEngagementBar() : this(null)
    {
    }

    public PlayerEngagementBar(VideoPlatformViewModel? viewModel)
    {
        _vm = viewModel ?? VideoPlatformViewModel.Shared;

        Padding = new Thickness(12, 6, 12, 6);
        CornerRadius = new CornerRadius(10);
        Background = new SolidColorBrush(ColorHelper.FromArgb(0xD8, 0x1A, 0x1A, 0x20));
        BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x2E, 0xFF, 0xFF, 0xFF));
        BorderThickness = new Thickness(1);
        HorizontalAlignment = HorizontalAlignment.Center;
        VerticalAlignment = VerticalAlignment.Center;

        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 12,
            VerticalAlignment = VerticalAlignment.Center,
        };

        // View count badge (\uE7B3 eye icon + "N görüntüleme")
        var viewsBadge = new Border
        {
            Padding = new Thickness(8, 4, 8, 4),
            CornerRadius = new CornerRadius(6),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x1E, 0xFF, 0xFF, 0xFF)),
            VerticalAlignment = VerticalAlignment.Center,
        };
        var viewsStack = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        viewsStack.Children.Add(new FontIcon
        {
            Glyph = "\uE7B3",
            FontSize = 13,
            Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xDF, 0xFF, 0xFF, 0xFF)),
        });
        _viewsText = new TextBlock
        {
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = new SolidColorBrush(Colors.White),
        };
        viewsStack.Children.Add(_viewsText);
        viewsBadge.Child = viewsStack;
        row.Children.Add(viewsBadge);

        // Like button (\uEB51 / \uEB52 heart icon + "Beğen" / "Beğenildi")
        _likeIcon = new FontIcon
        {
            Glyph = "\uEB51",
            FontSize = 13,
            VerticalAlignment = VerticalAlignment.Center,
        };
        _likeText = new TextBlock
        {
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var likeContent = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        likeContent.Children.Add(_likeIcon);
        likeContent.Children.Add(_likeText);

        _likeButton = new Button
        {
            Padding = new Thickness(10, 4, 10, 4),
            CornerRadius = new CornerRadius(6),
            VerticalAlignment = VerticalAlignment.Center,
            Content = likeContent,
        };
        _likeButton.Click += (_, _) => _vm.ToggleActiveVideoLike();
        row.Children.Add(_likeButton);

        // 5-star interactive rating bar (\uE734 / \uE735)
        var starsRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 2,
            VerticalAlignment = VerticalAlignment.Center,
        };

        for (var i = 0; i < 5; i++)
        {
            var starNumber = i + 1;
            var icon = new FontIcon
            {
                Glyph = "\uE734",
                FontSize = 14,
            };
            var btn = new Button
            {
                Padding = new Thickness(5, 3, 5, 3),
                MinWidth = 26,
                MinHeight = 26,
                CornerRadius = new CornerRadius(5),
                Background = new SolidColorBrush(Colors.Transparent),
                BorderThickness = new Thickness(0),
                Content = icon,
            };
            btn.Click += (_, _) =>
            {
                var nextRating = _vm.ActiveVideoRating == starNumber ? 0 : starNumber;
                _vm.SetActiveVideoRating(nextRating);
            };
            _starIcons[i] = icon;
            _starButtons[i] = btn;
            starsRow.Children.Add(btn);
        }

        row.Children.Add(starsRow);

        // "+ Listeye ekle" button with dynamic MenuFlyout
        _addToListLabel = new TextBlock
        {
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var addToListContent = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 5,
            VerticalAlignment = VerticalAlignment.Center,
        };
        addToListContent.Children.Add(new FontIcon { Glyph = "\uE710", FontSize = 11 });
        addToListContent.Children.Add(_addToListLabel);

        _addToListFlyout = new MenuFlyout();
        _addToListFlyout.Opening += (_, _) => PopulateAddToListFlyout(_addToListFlyout, _vm, _vm.ActiveVideoPath);

        _addToListButton = new Button
        {
            Padding = new Thickness(10, 4, 10, 4),
            CornerRadius = new CornerRadius(6),
            VerticalAlignment = VerticalAlignment.Center,
            Content = addToListContent,
            Flyout = _addToListFlyout,
        };
        row.Children.Add(_addToListButton);

        Children.Add(row);

        Subscribe(_vm);
        Unloaded += (_, _) => Unsubscribe(_vm);
        Loaded += (_, _) =>
        {
            Unsubscribe(_vm);
            Subscribe(_vm);
            Refresh();
        };

        Refresh();
    }

    internal static void PopulateAddToListFlyout(MenuFlyout flyout, VideoPlatformViewModel vm, string? videoPath)
    {
        flyout.Items.Clear();
        if (!vm.IsLoggedIn || string.IsNullOrWhiteSpace(videoPath))
        {
            flyout.Items.Add(new MenuFlyoutItem
            {
                Text = L("Listeye eklemek için giriş yapın"),
                IsEnabled = false,
            });
            return;
        }

        var lists = vm.CustomLists;
        foreach (var list in lists)
        {
            var capturedId = list.Id;
            var item = new ToggleMenuFlyoutItem
            {
                Text = $"{list.Name} ({list.VideoPaths.Count})",
                IsChecked = vm.IsVideoInCustomList(capturedId, videoPath),
            };
            item.Click += (_, _) => vm.ToggleVideoInCustomList(capturedId, videoPath);
            flyout.Items.Add(item);
        }

        if (lists.Count > 0)
        {
            flyout.Items.Add(new MenuFlyoutSeparator());
        }

        var quickCreateItem = new MenuFlyoutItem
        {
            Text = lists.Count == 0
                ? L("+ \"Daha Sonra İzle\" listesi oluştur ve ekle")
                : L("+ Yeni liste oluştur ve ekle"),
            Icon = new FontIcon { Glyph = "\uE710" },
        };
        quickCreateItem.Click += (_, _) =>
        {
            var defaultName = lists.Count == 0
                ? L("Daha Sonra İzle")
                : $"{L("Listem")} {lists.Count + 1}";
            var created = vm.CreateCustomList(defaultName);
            if (created is not null)
            {
                vm.ToggleVideoInCustomList(created.Id, videoPath);
            }
        };
        flyout.Items.Add(quickCreateItem);
    }

    public VideoPlatformViewModel ViewModel
    {
        get => _vm;
        set
        {
            var next = value ?? VideoPlatformViewModel.Shared;
            if (ReferenceEquals(_vm, next)) return;
            Unsubscribe(_vm);
            _vm = next;
            Subscribe(_vm);
            Refresh();
        }
    }

    private void Subscribe(VideoPlatformViewModel vm)
    {
        vm.PropertyChanged += OnViewModelPropertyChanged;
        vm.LocalizationChanged += OnLocalizationChanged;
    }

    private void Unsubscribe(VideoPlatformViewModel vm)
    {
        vm.PropertyChanged -= OnViewModelPropertyChanged;
        vm.LocalizationChanged -= OnLocalizationChanged;
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e) => SafeRefresh();
    private void OnLocalizationChanged(object? sender, EventArgs e) => SafeRefresh();

    private void SafeRefresh()
    {
        if (DispatcherQueue is { } dq && !dq.HasThreadAccess)
        {
            dq.TryEnqueue(Refresh);
            return;
        }
        Refresh();
    }

    public void Refresh()
    {
        var visible = _vm.HasActiveVideo;
        Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        if (!visible) return;

        _viewsText.Text = _vm.ActiveVideoViewsText;
        var liked = _vm.ActiveVideoIsLiked;
        _likeIcon.Glyph = liked ? "\uEB52" : "\uEB51";
        _likeIcon.Foreground = liked
            ? new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0x4D, 0x6D))
            : new SolidColorBrush(Colors.White);
        _likeText.Text = _vm.ActiveVideoLikeText;
        AutomationProperties.SetName(_likeButton, _vm.ActiveVideoLikeText);

        _addToListLabel.Text = L("+ Listeye ekle");
        AutomationProperties.SetName(_addToListButton, L("+ Listeye ekle"));

        var rating = _vm.ActiveVideoRating;
        var goldBrush = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0xC8, 0x3D));
        var dimBrush = new SolidColorBrush(ColorHelper.FromArgb(0x88, 0xFF, 0xFF, 0xFF));

        for (var i = 0; i < 5; i++)
        {
            var starNumber = i + 1;
            var active = starNumber <= rating;
            _starIcons[i].Glyph = active ? "\uE735" : "\uE734";
            _starIcons[i].Foreground = active ? goldBrush : dimBrush;
            var tip = L("%lld yıldız", (long)starNumber);
            ToolTipService.SetToolTip(_starButtons[i], tip);
            AutomationProperties.SetName(_starButtons[i], tip);
        }
    }
}

/// <summary>
/// Full YouTube-style local video platform view.
/// Always shows a prominent search bar at the top (type &amp; press Enter or click Search to list matching SSD-indexed videos)
/// and category filter pills ("Keşfet", "İzleme geçmişi", "Favorilerim", "Puan verdiklerim", "En çok izlenenler").
/// When logged in, also displays the "Listelerim" custom list bar and full personal engagement on every card.
/// </summary>
public sealed class VideoPlatformHubView : Grid
{
    private VideoPlatformViewModel _vm;

    // Top search & profile header elements
    private readonly TextBox _searchBox;
    private readonly Button _searchButton;
    private readonly TextBlock _searchButtonLabel;
    private readonly Button _clearSearchButton;
    private readonly TextBlock _activeProfileBadgeText;
    private readonly Button _clearHistoryButton;
    private readonly Button _hubLogoutButton;
    private readonly Button _hubSignInPromptButton;

    // Filter pills row
    private readonly Dictionary<VideoFeedFilter, Button> _filterButtons = [];

    // Custom lists ("Listelerim") bar
    private readonly Border _customListsBar;
    private readonly TextBlock _customListsTitle;
    private readonly StackPanel _customListPillsPanel;
    private readonly TextBox _newListNameBox;
    private readonly Button _createListButton;

    // Logged-out auth gate card (shown when a personal tab or sign-in is selected while logged out)
    private readonly Border _authGateContainer;
    private readonly TextBlock _authTitle;
    private readonly TextBlock _authDescription;
    private readonly StackPanel _authExistingProfilesRow;
    private readonly TextBox _authUsernameBox;
    private readonly PasswordBox _authPasswordBox;
    private readonly CheckBox _authRememberCheckBox;
    private readonly TextBlock _authErrorText;
    private readonly Button _authLoginButton;
    private readonly Button _authRegisterButton;

    // Scrollable feed area
    private readonly ScrollViewer _feedScroller;
    private readonly StackPanel _cardsPanel;
    private readonly Border _emptyStateCard;
    private readonly TextBlock _emptyStateTitle;
    private readonly TextBlock _emptyStateSubtitle;
    private readonly Dictionary<string, BitmapImage> _thumbnailImageCache = new(StringComparer.OrdinalIgnoreCase);
    private readonly HashSet<string> _thumbnailLoadingPaths = new(StringComparer.OrdinalIgnoreCase);
    private readonly SemaphoreSlim _thumbnailExtractGate = new(1, 1);
    private readonly List<RenderedVideoCard> _renderedCards = [];
    private bool _showExplicitSignInCard;
    private bool _refreshQueued;

    private sealed class RenderedVideoCard
    {
        public required string VideoPath { get; init; }
        public required bool IsLoggedIn { get; init; }
        public required Border CardBorder { get; init; }
        public required Image ThumbImage { get; init; }
        public required TextBlock ViewsPillText { get; init; }
        public required Border DurationPill { get; init; }
        public required TextBlock DurationPillText { get; init; }
        public required Border ProgressTrack { get; init; }
        public required Border ProgressFill { get; init; }
        public required TextBlock TitleText { get; init; }
        public required TextBlock FolderText { get; init; }
        public required StackPanel BadgesRow { get; init; }
        public required StackPanel ActionsRow { get; init; }
    }

    /// <summary>Fired when the user clicks a video card in the platform feed to open and play it immediately.</summary>
    public event Action<string>? VideoSelected;

    public VideoPlatformHubView() : this(null)
    {
    }

    public VideoPlatformHubView(VideoPlatformViewModel? viewModel)
    {
        _vm = viewModel ?? VideoPlatformViewModel.Shared;
        HorizontalAlignment = HorizontalAlignment.Stretch;
        VerticalAlignment = VerticalAlignment.Stretch;
        RowSpacing = 10;
        Padding = new Thickness(16, 12, 16, 12);

        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto }); // Row 0: Search bar + Profile status
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto }); // Row 1: Category filter pills
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto }); // Row 2: "Listelerim" custom lists bar
        RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) }); // Row 3: Content area

        // Row 0: Prominent Search Bar + Profile / Session Actions
        var searchAndProfileBar = BuildSearchAndProfileBar(
            out _searchBox,
            out _searchButton,
            out _searchButtonLabel,
            out _clearSearchButton,
            out _activeProfileBadgeText,
            out _clearHistoryButton,
            out _hubLogoutButton,
            out _hubSignInPromptButton);
        Grid.SetRow(searchAndProfileBar, 0);
        Children.Add(searchAndProfileBar);

        // Row 1: Category Filter Pills
        var filterBar = BuildFilterPillsBar();
        Grid.SetRow(filterBar, 1);
        Children.Add(filterBar);

        // Row 2: Custom Lists ("Listelerim") Bar
        _customListsBar = BuildCustomListsBar(
            out _customListsTitle,
            out _customListPillsPanel,
            out _newListNameBox,
            out _createListButton);
        Grid.SetRow(_customListsBar, 2);
        Children.Add(_customListsBar);

        // Row 3: Content Area (Auth Gate Card + Empty State + Video Cards Scroller)
        var contentArea = new Grid
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Stretch,
        };

        _authGateContainer = BuildAuthGateCard(
            out _authTitle,
            out _authDescription,
            out _authExistingProfilesRow,
            out _authUsernameBox,
            out _authPasswordBox,
            out _authRememberCheckBox,
            out _authErrorText,
            out _authLoginButton,
            out _authRegisterButton);
        contentArea.Children.Add(_authGateContainer);

        _emptyStateCard = BuildEmptyStateCard(out _emptyStateTitle, out _emptyStateSubtitle);
        contentArea.Children.Add(_emptyStateCard);

        _cardsPanel = new StackPanel
        {
            Spacing = 10,
            Orientation = Orientation.Vertical,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        _feedScroller = new ScrollViewer
        {
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Stretch,
            Content = _cardsPanel,
        };
        contentArea.Children.Add(_feedScroller);

        Grid.SetRow(contentArea, 3);
        Children.Add(contentArea);

        Subscribe(_vm);
        Unloaded += (_, _) => Unsubscribe(_vm);
        Loaded += (_, _) =>
        {
            Unsubscribe(_vm);
            Subscribe(_vm);
            Refresh();
        };

        Refresh();
    }

    public VideoPlatformViewModel ViewModel
    {
        get => _vm;
        set
        {
            var next = value ?? VideoPlatformViewModel.Shared;
            if (ReferenceEquals(_vm, next)) return;
            Unsubscribe(_vm);
            _vm = next;
            Subscribe(_vm);
            Refresh();
        }
    }

    /// <summary>Focuses the top video search input box.</summary>
    public void FocusSearchBox()
    {
        _searchBox.Focus(FocusState.Programmatic);
        _searchBox.SelectAll();
    }

    private void Subscribe(VideoPlatformViewModel vm)
    {
        vm.PropertyChanged += OnViewModelPropertyChanged;
        vm.LocalizationChanged += OnLocalizationChanged;
    }

    private void Unsubscribe(VideoPlatformViewModel vm)
    {
        vm.PropertyChanged -= OnViewModelPropertyChanged;
        vm.LocalizationChanged -= OnLocalizationChanged;
    }

    private Grid BuildSearchAndProfileBar(
        out TextBox searchBox,
        out Button searchButton,
        out TextBlock searchButtonLabel,
        out Button clearSearchButton,
        out TextBlock activeProfileBadgeText,
        out Button clearHistoryButton,
        out Button logoutButton,
        out Button signInPromptButton)
    {
        var bar = new Grid
        {
            ColumnSpacing = 10,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Center,
        };
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        searchBox = new TextBox
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Center,
        };
        searchBox.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                ExecuteSearch();
                e.Handled = true;
            }
        };
        searchBox.TextChanged += (s, _) =>
        {
            // When cleared to empty, restore full list immediately
            var txt = ((TextBox)s).Text;
            if (string.IsNullOrEmpty(txt) && !string.IsNullOrEmpty(_vm.SearchQuery))
            {
                _vm.SearchQuery = "";
            }
        };
        Grid.SetColumn(searchBox, 0);
        bar.Children.Add(searchBox);

        searchButtonLabel = new TextBlock
        {
            FontSize = 12.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var searchBtnStack = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        searchBtnStack.Children.Add(new FontIcon { Glyph = "\uE721", FontSize = 13 });
        searchBtnStack.Children.Add(searchButtonLabel);

        searchButton = new Button
        {
            Padding = new Thickness(12, 6, 12, 6),
            CornerRadius = new CornerRadius(6),
            VerticalAlignment = VerticalAlignment.Center,
            Content = searchBtnStack,
            Style = Application.Current?.Resources.TryGetValue("AccentButtonStyle", out var accent) == true
                ? accent as Style
                : null,
        };
        searchButton.Click += (_, _) => ExecuteSearch();
        Grid.SetColumn(searchButton, 1);
        bar.Children.Add(searchButton);

        clearSearchButton = new Button
        {
            Padding = new Thickness(8, 6, 8, 6),
            CornerRadius = new CornerRadius(6),
            VerticalAlignment = VerticalAlignment.Center,
            Content = new FontIcon { Glyph = "\uE711", FontSize = 12 },
            Visibility = Visibility.Collapsed,
        };
        clearSearchButton.Click += (_, _) =>
        {
            _searchBox.Text = "";
            _vm.SearchQuery = "";
        };
        Grid.SetColumn(clearSearchButton, 2);
        bar.Children.Add(clearSearchButton);

        var profileRightStack = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            VerticalAlignment = VerticalAlignment.Center,
        };

        activeProfileBadgeText = new TextBlock
        {
            FontSize = 12,
            Opacity = 0.82,
            VerticalAlignment = VerticalAlignment.Center,
        };
        profileRightStack.Children.Add(activeProfileBadgeText);

        clearHistoryButton = new Button
        {
            Padding = new Thickness(10, 5, 10, 5),
            CornerRadius = new CornerRadius(6),
            FontSize = 12,
            Visibility = Visibility.Collapsed,
        };
        clearHistoryButton.Click += (_, _) => _vm.ClearWatchHistory();
        profileRightStack.Children.Add(clearHistoryButton);

        logoutButton = new Button
        {
            Padding = new Thickness(10, 5, 10, 5),
            CornerRadius = new CornerRadius(6),
            FontSize = 12,
            Visibility = Visibility.Collapsed,
        };
        logoutButton.Click += (_, _) =>
        {
            _showExplicitSignInCard = false;
            _vm.Logout();
            _authPasswordBox.Password = "";
            Refresh();
        };
        profileRightStack.Children.Add(logoutButton);

        signInPromptButton = new Button
        {
            Padding = new Thickness(10, 5, 10, 5),
            CornerRadius = new CornerRadius(6),
            FontSize = 12,
        };
        signInPromptButton.Click += (_, _) =>
        {
            _showExplicitSignInCard = !_showExplicitSignInCard;
            Refresh();
        };
        profileRightStack.Children.Add(signInPromptButton);

        Grid.SetColumn(profileRightStack, 3);
        bar.Children.Add(profileRightStack);

        return bar;
    }

    private StackPanel BuildFilterPillsBar()
    {
        var pillsPanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };

        foreach (var filter in new[]
                 {
                     VideoFeedFilter.All,
                     VideoFeedFilter.History,
                     VideoFeedFilter.Liked,
                     VideoFeedFilter.TopRated,
                     VideoFeedFilter.MostWatched,
                 })
        {
            var captured = filter;
            var pill = new Button
            {
                Padding = new Thickness(12, 6, 12, 6),
                CornerRadius = new CornerRadius(16),
                FontSize = 12.5,
                FontWeight = FontWeights.SemiBold,
            };
            pill.Click += (_, _) =>
            {
                _showExplicitSignInCard = false;
                _vm.ActiveFilter = captured;
            };
            _filterButtons[captured] = pill;
            pillsPanel.Children.Add(pill);
        }

        return pillsPanel;
    }

    private Border BuildCustomListsBar(
        out TextBlock customListsTitle,
        out StackPanel customListPillsPanel,
        out TextBox newListNameBox,
        out Button createListButton)
    {
        var barBorder = new Border
        {
            Padding = new Thickness(10, 7, 10, 7),
            CornerRadius = new CornerRadius(10),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x14, 0x80, 0x80, 0x8A)),
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x24, 0x80, 0x80, 0x8A)),
            BorderThickness = new Thickness(1),
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Visibility = Visibility.Collapsed,
        };

        var grid = new Grid
        {
            ColumnSpacing = 10,
            VerticalAlignment = VerticalAlignment.Center,
        };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var labelStack = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        labelStack.Children.Add(new FontIcon { Glyph = "\uE8FD", FontSize = 13, Opacity = 0.85 });
        customListsTitle = new TextBlock
        {
            FontSize = 12.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        };
        labelStack.Children.Add(customListsTitle);
        Grid.SetColumn(labelStack, 0);
        grid.Children.Add(labelStack);

        customListPillsPanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var pillsScroll = new ScrollViewer
        {
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
            Content = customListPillsPanel,
        };
        Grid.SetColumn(pillsScroll, 1);
        grid.Children.Add(pillsScroll);

        newListNameBox = new TextBox
        {
            Width = 170,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
        };
        newListNameBox.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                AttemptCreateCustomList();
                e.Handled = true;
            }
        };
        Grid.SetColumn(newListNameBox, 2);
        grid.Children.Add(newListNameBox);

        createListButton = new Button
        {
            Padding = new Thickness(10, 5, 10, 5),
            CornerRadius = new CornerRadius(6),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        };
        createListButton.Click += (_, _) => AttemptCreateCustomList();
        Grid.SetColumn(createListButton, 3);
        grid.Children.Add(createListButton);

        barBorder.Child = grid;
        return barBorder;
    }

    private void AttemptCreateCustomList()
    {
        var name = _newListNameBox.Text.Trim();
        if (name.Length == 0) return;
        var created = _vm.CreateCustomList(name);
        if (created is not null)
        {
            _newListNameBox.Text = "";
            _vm.SelectCustomList(created.Id);
        }
    }

    private void ExecuteSearch()
    {
        _showExplicitSignInCard = false;
        _vm.SearchQuery = _searchBox.Text;
        _vm.RefreshFeed();
        Refresh();
    }

    private Border BuildEmptyStateCard(out TextBlock emptyStateTitle, out TextBlock emptyStateSubtitle)
    {
        emptyStateTitle = new TextBlock
        {
            FontSize = 15,
            FontWeight = FontWeights.SemiBold,
            TextAlignment = TextAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center,
        };
        emptyStateSubtitle = new TextBlock
        {
            FontSize = 12.5,
            Opacity = 0.72,
            TextAlignment = TextAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 420,
        };
        var emptyStack = new StackPanel
        {
            Spacing = 6,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        emptyStack.Children.Add(new FontIcon
        {
            Glyph = "\uE714",
            FontSize = 28,
            Opacity = 0.55,
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        emptyStack.Children.Add(emptyStateTitle);
        emptyStack.Children.Add(emptyStateSubtitle);

        return new Border
        {
            Padding = new Thickness(24),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Child = emptyStack,
            Visibility = Visibility.Collapsed,
        };
    }

    private Border BuildAuthGateCard(
        out TextBlock titleBlock,
        out TextBlock descBlock,
        out StackPanel existingProfilesRow,
        out TextBox usernameBox,
        out PasswordBox passwordBox,
        out CheckBox rememberCheckBox,
        out TextBlock errorText,
        out Button loginBtn,
        out Button registerBtn)
    {
        var card = new Border
        {
            MaxWidth = 420,
            Padding = new Thickness(28, 24, 28, 24),
            CornerRadius = new CornerRadius(14),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x18, 0x80, 0x80, 0x88)),
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x30, 0x80, 0x80, 0x88)),
            BorderThickness = new Thickness(1),
            Visibility = Visibility.Collapsed,
        };

        var stack = new StackPanel
        {
            Spacing = 12,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };

        var iconBadge = new Border
        {
            Width = 48,
            Height = 48,
            CornerRadius = new CornerRadius(24),
            HorizontalAlignment = HorizontalAlignment.Center,
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x28, 0x38, 0x8B, 0xFF)),
            Child = new FontIcon
            {
                Glyph = "\uE77B",
                FontSize = 22,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            },
        };
        stack.Children.Add(iconBadge);

        titleBlock = new TextBlock
        {
            FontSize = 18,
            FontWeight = FontWeights.SemiBold,
            TextAlignment = TextAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
        };
        stack.Children.Add(titleBlock);

        descBlock = new TextBlock
        {
            FontSize = 13,
            Opacity = 0.78,
            TextAlignment = TextAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
        };
        stack.Children.Add(descBlock);

        existingProfilesRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            HorizontalAlignment = HorizontalAlignment.Center,
            Visibility = Visibility.Collapsed,
        };
        stack.Children.Add(existingProfilesRow);

        usernameBox = new TextBox
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        stack.Children.Add(usernameBox);

        passwordBox = new PasswordBox
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        passwordBox.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                AttemptInlineLogin();
                e.Handled = true;
            }
        };
        stack.Children.Add(passwordBox);

        rememberCheckBox = new CheckBox
        {
            IsChecked = _vm.RememberMe,
            FontSize = 12.5,
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        rememberCheckBox.Checked += (_, _) => _vm.RememberMe = true;
        rememberCheckBox.Unchecked += (_, _) => _vm.RememberMe = false;
        stack.Children.Add(rememberCheckBox);

        errorText = new TextBlock
        {
            FontSize = 12,
            Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0x6B, 0x6B)),
            TextAlignment = TextAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        stack.Children.Add(errorText);

        var buttonsGrid = new Grid { ColumnSpacing = 8 };
        buttonsGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        buttonsGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        loginBtn = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Style = Application.Current?.Resources.TryGetValue("AccentButtonStyle", out var accent) == true
                ? accent as Style
                : null,
        };
        loginBtn.Click += (_, _) => AttemptInlineLogin();
        Grid.SetColumn(loginBtn, 0);
        buttonsGrid.Children.Add(loginBtn);

        registerBtn = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        registerBtn.Click += (_, _) => AttemptInlineRegister();
        Grid.SetColumn(registerBtn, 1);
        buttonsGrid.Children.Add(registerBtn);

        stack.Children.Add(buttonsGrid);
        card.Child = stack;
        return card;
    }

    private void AttemptInlineLogin()
    {
        if (_vm.TryLogin(_authUsernameBox.Text, _authPasswordBox.Password, _authRememberCheckBox.IsChecked == true))
        {
            _showExplicitSignInCard = false;
            _authPasswordBox.Password = "";
            Refresh();
        }
        else
        {
            RefreshAuthGateError();
        }
    }

    private void AttemptInlineRegister()
    {
        if (_vm.TryRegister(_authUsernameBox.Text, _authPasswordBox.Password, _authRememberCheckBox.IsChecked == true))
        {
            _showExplicitSignInCard = false;
            _authPasswordBox.Password = "";
            Refresh();
        }
        else
        {
            RefreshAuthGateError();
        }
    }

    private void RefreshAuthGateError()
    {
        if (!string.IsNullOrWhiteSpace(_vm.AuthError))
        {
            _authErrorText.Text = _vm.AuthError;
            _authErrorText.Visibility = Visibility.Visible;
        }
        else
        {
            _authErrorText.Text = "";
            _authErrorText.Visibility = Visibility.Collapsed;
        }
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        // Ignore player-only engagement bar properties so active playback never triggers a hub refresh.
        if (e.PropertyName is
            nameof(VideoPlatformViewModel.ActiveVideoPath) or
            nameof(VideoPlatformViewModel.HasActiveVideo) or
            nameof(VideoPlatformViewModel.ActiveVideoViewCount) or
            nameof(VideoPlatformViewModel.ActiveVideoIsLiked) or
            nameof(VideoPlatformViewModel.ActiveVideoRating) or
            nameof(VideoPlatformViewModel.ActiveVideoViewsText) or
            nameof(VideoPlatformViewModel.ActiveVideoLikeText) or
            nameof(VideoPlatformViewModel.RememberMe) or
            nameof(VideoPlatformViewModel.AuthError))
        {
            return;
        }
        SafeRefresh();
    }

    private void OnLocalizationChanged(object? sender, EventArgs e) => SafeRefresh();

    private void SafeRefresh()
    {
        if (DispatcherQueue is { } dq)
        {
            if (_refreshQueued) return;
            _refreshQueued = true;
            dq.TryEnqueue(() =>
            {
                _refreshQueued = false;
                Refresh();
            });
            return;
        }
        Refresh();
    }

    public void Refresh()
    {
        var loggedIn = _vm.IsLoggedIn;
        if (loggedIn) _showExplicitSignInCard = false;

        // Top search bar localization & state
        _searchBox.PlaceholderText = L("İndekslenen tüm videolarda ara (yazıp Enter'a basın)…");
        if (_searchBox.Text != _vm.SearchQuery && string.IsNullOrEmpty(_vm.SearchQuery))
        {
            _searchBox.Text = "";
        }
        _searchButtonLabel.Text = L("Ara");
        ToolTipService.SetToolTip(_clearSearchButton, L("Aramayı temizle"));
        _clearSearchButton.Visibility = !string.IsNullOrWhiteSpace(_vm.SearchQuery)
            ? Visibility.Visible
            : Visibility.Collapsed;

        // Top-right profile controls
        _activeProfileBadgeText.Visibility = loggedIn ? Visibility.Visible : Visibility.Collapsed;
        _hubLogoutButton.Visibility = loggedIn ? Visibility.Visible : Visibility.Collapsed;
        _hubSignInPromptButton.Visibility = loggedIn ? Visibility.Collapsed : Visibility.Visible;
        _clearHistoryButton.Visibility = loggedIn && _vm.ActiveFilter == VideoFeedFilter.History && _vm.TotalWatchedVideosCount > 0
            ? Visibility.Visible
            : Visibility.Collapsed;

        if (loggedIn)
        {
            _activeProfileBadgeText.Text = $"{_vm.CurrentUsername} · {_vm.ProfileSummaryText}";
            _clearHistoryButton.Content = L("Geçmişi temizle");
            _hubLogoutButton.Content = L("Çıkış yap");
        }
        else
        {
            _hubSignInPromptButton.Content = L("Giriş yap / Profil oluştur");
        }

        // Category Filter Pills with counts
        UpdateFilterPill(VideoFeedFilter.All, L("Keşfet"));
        UpdateFilterPill(
            VideoFeedFilter.History,
            loggedIn ? L("İzleme geçmişi (%lld)", (long)_vm.TotalWatchedVideosCount) : L("İzleme geçmişi"));
        UpdateFilterPill(
            VideoFeedFilter.Liked,
            loggedIn ? L("Favorilerim (%lld)", (long)_vm.TotalLikedVideosCount) : L("Favorilerim"));
        UpdateFilterPill(
            VideoFeedFilter.TopRated,
            loggedIn ? L("Puan verdiklerim (%lld)", (long)_vm.TotalRatedVideosCount) : L("Puan verdiklerim"));
        UpdateFilterPill(VideoFeedFilter.MostWatched, L("En çok izlenenler"));

        // Custom Lists ("Listelerim") Bar
        _customListsBar.Visibility = loggedIn ? Visibility.Visible : Visibility.Collapsed;
        if (loggedIn)
        {
            _customListsTitle.Text = L("Listelerim:");
            _newListNameBox.PlaceholderText = L("Yeni liste adı…");
            _createListButton.Content = L("+ Liste oluştur");
            RebuildCustomListPills();
        }

        // Auth Gate vs Feed Area
        var showAuthCard = !loggedIn && (_vm.RequiresAuthForCurrentTab || _showExplicitSignInCard);
        _authGateContainer.Visibility = showAuthCard ? Visibility.Visible : Visibility.Collapsed;

        if (showAuthCard)
        {
            _feedScroller.Visibility = Visibility.Collapsed;
            _emptyStateCard.Visibility = Visibility.Collapsed;

            _authTitle.Text = L("Yerel Video Platformu");
            _authDescription.Text = L("İzleme geçmişi, görüntülenme sayısı, favoriler, yıldız puanları ve özel listeleriniz yerel profilinize kaydedilir. Devam etmek için giriş yapın veya yeni profil oluşturun.");
            _authUsernameBox.PlaceholderText = L("Kullanıcı adı");
            _authPasswordBox.PlaceholderText = L("Şifre");
            _authRememberCheckBox.Content = L("Girişi hatırla");
            _authRememberCheckBox.IsChecked = _vm.RememberMe;
            _authLoginButton.Content = L("Giriş yap");
            _authRegisterButton.Content = L("Profil oluştur");

            if (string.IsNullOrWhiteSpace(_authUsernameBox.Text) && !string.IsNullOrWhiteSpace(_vm.LastUsername))
            {
                _authUsernameBox.Text = _vm.LastUsername;
            }

            RebuildAuthExistingProfileChips();
            RefreshAuthGateError();
            return;
        }

        _feedScroller.Visibility = Visibility.Visible;
        RebuildFeedCards();
    }

    private void UpdateFilterPill(VideoFeedFilter filter, string text)
    {
        if (!_filterButtons.TryGetValue(filter, out var button)) return;
        button.Content = text;
        var active = _vm.ActiveFilter == filter;
        button.Background = active
            ? new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0x2B, 0x7A, 0xFF))
            : new SolidColorBrush(ColorHelper.FromArgb(0x1E, 0x80, 0x80, 0x88));
        button.Foreground = active
            ? new SolidColorBrush(Colors.White)
            : new SolidColorBrush(ColorHelper.FromArgb(0xDF, 0xFF, 0xFF, 0xFF));
    }

    private void RebuildCustomListPills()
    {
        _customListPillsPanel.Children.Clear();
        var lists = _vm.CustomLists;
        if (lists.Count == 0)
        {
            _customListPillsPanel.Children.Add(new TextBlock
            {
                Text = L("Henüz özel liste oluşturulmadı"),
                FontSize = 12,
                Opacity = 0.65,
                VerticalAlignment = VerticalAlignment.Center,
            });
            return;
        }

        foreach (var list in lists)
        {
            var captured = list;
            var isSelected = _vm.ActiveFilter == VideoFeedFilter.CustomList &&
                             string.Equals(_vm.ActiveCustomListId, captured.Id, StringComparison.OrdinalIgnoreCase);

            var pillRow = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 4,
                VerticalAlignment = VerticalAlignment.Center,
            };

            var selectBtn = new Button
            {
                Content = $"{captured.Name} ({captured.VideoPaths.Count})",
                Padding = new Thickness(10, 4, 10, 4),
                CornerRadius = new CornerRadius(14),
                FontSize = 12,
                FontWeight = FontWeights.SemiBold,
                Background = isSelected
                    ? new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0x2B, 0x7A, 0xFF))
                    : new SolidColorBrush(ColorHelper.FromArgb(0x22, 0x80, 0x80, 0x88)),
                Foreground = new SolidColorBrush(Colors.White),
            };
            selectBtn.Click += (_, _) => _vm.SelectCustomList(captured.Id);
            pillRow.Children.Add(selectBtn);

            if (isSelected)
            {
                var deleteBtn = new Button
                {
                    Content = "×",
                    Padding = new Thickness(6, 2, 6, 2),
                    MinWidth = 24,
                    MinHeight = 24,
                    CornerRadius = new CornerRadius(12),
                    FontSize = 12,
                    FontWeight = FontWeights.Bold,
                };
                ToolTipService.SetToolTip(deleteBtn, L("Listeyi sil"));
                deleteBtn.Click += (_, _) => _vm.DeleteCustomList(captured.Id);
                pillRow.Children.Add(deleteBtn);
            }

            _customListPillsPanel.Children.Add(pillRow);
        }
    }

    private void RebuildAuthExistingProfileChips()
    {
        _authExistingProfilesRow.Children.Clear();
        var users = _vm.RegisteredUsernames;
        if (users.Count == 0)
        {
            _authExistingProfilesRow.Visibility = Visibility.Collapsed;
            return;
        }

        _authExistingProfilesRow.Visibility = Visibility.Visible;
        foreach (var user in users.Take(5))
        {
            var captured = user;
            var btn = new Button
            {
                Content = captured,
                FontSize = 11.5,
                Padding = new Thickness(8, 3, 8, 3),
                CornerRadius = new CornerRadius(12),
            };
            btn.Click += (_, _) =>
            {
                _authUsernameBox.Text = captured;
                _authPasswordBox.Focus(FocusState.Programmatic);
            };
            _authExistingProfilesRow.Children.Add(btn);
        }
    }

    private void RebuildFeedCards()
    {
        var items = _vm.FeedItems;

        if (items.Count == 0)
        {
            _cardsPanel.Children.Clear();
            _renderedCards.Clear();
            _emptyStateCard.Visibility = Visibility.Visible;
            if (!string.IsNullOrWhiteSpace(_vm.SearchQuery))
            {
                _emptyStateTitle.Text = L("\"%@\" için eşleşen video bulunamadı", _vm.SearchQuery);
                _emptyStateSubtitle.Text = L("Farklı bir anahtar kelime deneyin veya klasör gezgininden yeni bir video klasörü açın.");
                return;
            }

            _emptyStateTitle.Text = _vm.ActiveFilter switch
            {
                VideoFeedFilter.History => L("Henüz izleme geçmişi yok"),
                VideoFeedFilter.Liked => L("Henüz favorilere eklenen video yok"),
                VideoFeedFilter.MostWatched => L("Henüz izlenen video yok"),
                VideoFeedFilter.TopRated => L("Henüz puanlanan video yok"),
                VideoFeedFilter.CustomList => L("\"%@\" listesinde henüz video yok", _vm.ActiveCustomListName ?? L("Liste")),
                _ => L("Gösterilecek video bulunamadı"),
            };
            _emptyStateSubtitle.Text = _vm.ActiveFilter == VideoFeedFilter.CustomList
                ? L("Herhangi bir video kartındaki veya oynatıcıdaki \"+ Listeye ekle\" düğmesiyle bu listeye video ekleyebilirsiniz.")
                : L("Bir video açıp izlediğinizde, favorilere eklediğinizde veya puanladığınızda burada YouTube tarzı kartlar halinde listelenir.");
            return;
        }

        _emptyStateCard.Visibility = Visibility.Collapsed;
        var loggedIn = _vm.IsLoggedIn;

        // Fast path: if the displayed video paths and auth state are in the same order, update cards in-place
        // without clearing _cardsPanel.Children so thumbnails and layout never flicker.
        if (_renderedCards.Count == items.Count)
        {
            var sameSequence = true;
            for (var i = 0; i < items.Count; i++)
            {
                if (_renderedCards[i].IsLoggedIn != loggedIn ||
                    !string.Equals(_renderedCards[i].VideoPath, items[i].VideoPath, StringComparison.OrdinalIgnoreCase))
                {
                    sameSequence = false;
                    break;
                }
            }

            if (sameSequence)
            {
                for (var i = 0; i < items.Count; i++)
                {
                    UpdateVideoCardInPlace(_renderedCards[i], items[i]);
                }
                return;
            }
        }

        _cardsPanel.Children.Clear();
        _renderedCards.Clear();
        foreach (var item in items)
        {
            var rendered = BuildVideoCard(item);
            _renderedCards.Add(rendered);
            _cardsPanel.Children.Add(rendered.CardBorder);
        }
    }

    private RenderedVideoCard BuildVideoCard(VideoCardViewModel item)
    {
        var card = new Border
        {
            CornerRadius = new CornerRadius(12),
            Padding = new Thickness(12),
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x1C, 0x80, 0x80, 0x8A)),
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x2A, 0x80, 0x80, 0x8A)),
            BorderThickness = new Thickness(1),
        };

        var layout = new Grid
        {
            ColumnSpacing = 14,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(192) });
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        // Left: Dark 16:9 cinema preview header (192x108) with encrypted thumbnail image, play glyph, CC badge, view count pill, and progress bar
        var thumbBg = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(1, 1) };
        thumbBg.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0xFF, 0x16, 0x18, 0x22), Offset = 0 });
        thumbBg.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0xFF, 0x25, 0x2A, 0x3A), Offset = 1 });

        var thumbGrid = new Grid
        {
            Width = 192,
            Height = 108,
            Background = thumbBg,
        };

        var thumbImage = new Image
        {
            Width = 192,
            Height = 108,
            Stretch = Stretch.UniformToFill,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        if (_thumbnailImageCache.TryGetValue(item.VideoPath, out var cachedBitmap))
        {
            thumbImage.Source = cachedBitmap;
        }
        else
        {
            _ = LoadOrCaptureCardThumbnailAsync(item, thumbImage);
        }
        thumbGrid.Children.Add(thumbImage);

        var playCircle = new Border
        {
            Width = 38,
            Height = 38,
            CornerRadius = new CornerRadius(19),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0xB8, 0x00, 0x00, 0x00)),
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x55, 0xFF, 0xFF, 0xFF)),
            BorderThickness = new Thickness(1),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Child = new FontIcon
            {
                Glyph = "\uE768",
                FontSize = 15,
                Foreground = new SolidColorBrush(Colors.White),
                Margin = new Thickness(2, 0, 0, 0),
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            },
        };
        thumbGrid.Children.Add(playCircle);

        if (item.HasSubtitle)
        {
            var ccStack = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 4,
                VerticalAlignment = VerticalAlignment.Center,
            };
            ccStack.Children.Add(new FontIcon
            {
                Glyph = "\uF2B7",
                FontSize = 11,
                Foreground = new SolidColorBrush(Colors.White),
            });
            ccStack.Children.Add(new TextBlock
            {
                Text = string.IsNullOrEmpty(item.SubtitleBadgeText) ? L("Altyazılı") : item.SubtitleBadgeText,
                FontSize = 10,
                FontWeight = FontWeights.Bold,
                Foreground = new SolidColorBrush(Colors.White),
            });

            var ccBadge = new Border
            {
                Margin = new Thickness(7, 7, 0, 0),
                Padding = new Thickness(6, 2, 6, 2),
                CornerRadius = new CornerRadius(4),
                Background = new SolidColorBrush(ColorHelper.FromArgb(0xE6, 0x1E, 0x8E, 0x5A)),
                HorizontalAlignment = HorizontalAlignment.Left,
                VerticalAlignment = VerticalAlignment.Top,
                Child = ccStack,
            };
            ToolTipService.SetToolTip(ccBadge, L("Altyazı / çeviri dosyası mevcut"));
            thumbGrid.Children.Add(ccBadge);
        }
        else
        {
            var noSubBadge = new Border
            {
                Margin = new Thickness(7, 7, 0, 0),
                Padding = new Thickness(6, 2, 6, 2),
                CornerRadius = new CornerRadius(4),
                Background = new SolidColorBrush(ColorHelper.FromArgb(0x98, 0x1A, 0x1A, 0x22)),
                BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x38, 0xFF, 0xFF, 0xFF)),
                BorderThickness = new Thickness(1),
                HorizontalAlignment = HorizontalAlignment.Left,
                VerticalAlignment = VerticalAlignment.Top,
                Child = new TextBlock
                {
                    Text = L("Altyazı yok"),
                    FontSize = 9.5,
                    Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xB8, 0xFF, 0xFF, 0xFF)),
                },
            };
            ToolTipService.SetToolTip(noSubBadge, L("Altyazı yok"));
            thumbGrid.Children.Add(noSubBadge);
        }

        var viewsPillText = new TextBlock
        {
            Text = item.ViewsText,
            FontSize = 10.5,
            FontWeight = FontWeights.SemiBold,
            Foreground = new SolidColorBrush(Colors.White),
        };
        var viewsPill = new Border
        {
            Margin = new Thickness(0, 7, 7, 0),
            Padding = new Thickness(6, 2, 6, 2),
            CornerRadius = new CornerRadius(4),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0xC8, 0x00, 0x00, 0x00)),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Top,
            Child = viewsPillText,
        };
        thumbGrid.Children.Add(viewsPill);

        var durationPillText = new TextBlock
        {
            Text = item.DurationText,
            FontSize = 10.5,
            Foreground = new SolidColorBrush(Colors.White),
        };
        var durationPill = new Border
        {
            Margin = new Thickness(0, 0, 7, 9),
            Padding = new Thickness(5, 1, 5, 1),
            CornerRadius = new CornerRadius(4),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0xD0, 0x00, 0x00, 0x00)),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Bottom,
            Visibility = string.IsNullOrEmpty(item.DurationText) ? Visibility.Collapsed : Visibility.Visible,
            Child = durationPillText,
        };
        thumbGrid.Children.Add(durationPill);

        var progressFill = new Border
        {
            Height = 4,
            Width = Math.Clamp(192 * item.ProgressRatio, 4, 192),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0x2E, 0x4C)),
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        var progressTrack = new Border
        {
            Height = 4,
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x55, 0xFF, 0xFF, 0xFF)),
            VerticalAlignment = VerticalAlignment.Bottom,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Visibility = item.HasProgress ? Visibility.Visible : Visibility.Collapsed,
            Child = progressFill,
        };
        thumbGrid.Children.Add(progressTrack);

        var thumbBorder = new Border
        {
            Width = 192,
            Height = 108,
            CornerRadius = new CornerRadius(8),
            Child = thumbGrid,
        };
        thumbBorder.PointerReleased += (_, e) =>
        {
            VideoSelected?.Invoke(item.VideoPath);
            e.Handled = true;
        };

        Grid.SetColumn(thumbBorder, 0);
        layout.Children.Add(thumbBorder);

        // Right: Video title, folder path, badges (Favorites, Rating, Custom Lists), and interactive action buttons
        var detailsStack = new StackPanel
        {
            Spacing = 5,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };

        var titleText = new TextBlock
        {
            Text = item.Title,
            FontSize = 14.5,
            FontWeight = FontWeights.SemiBold,
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxLines = 1,
        };
        titleText.PointerReleased += (_, e) =>
        {
            VideoSelected?.Invoke(item.VideoPath);
            e.Handled = true;
        };
        detailsStack.Children.Add(titleText);

        var folderText = new TextBlock
        {
            Text = item.FolderDisplay,
            FontSize = 11.5,
            Opacity = 0.68,
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxLines = 1,
        };
        detailsStack.Children.Add(folderText);

        // Metadata + Badges Row (Views, Last Watched, Favorite badge, Star Rating badge, Custom List badges)
        var badgesRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        detailsStack.Children.Add(badgesRow);

        // Engagement actions row on card: Play button, Heart (Favorite) button, 5-star rating buttons, and "+ Listeye ekle"
        var actionsRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Margin = new Thickness(0, 4, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        detailsStack.Children.Add(actionsRow);

        Grid.SetColumn(detailsStack, 1);
        layout.Children.Add(detailsStack);

        card.Child = layout;

        var rendered = new RenderedVideoCard
        {
            VideoPath = item.VideoPath,
            IsLoggedIn = _vm.IsLoggedIn,
            CardBorder = card,
            ThumbImage = thumbImage,
            ViewsPillText = viewsPillText,
            DurationPill = durationPill,
            DurationPillText = durationPillText,
            ProgressTrack = progressTrack,
            ProgressFill = progressFill,
            TitleText = titleText,
            FolderText = folderText,
            BadgesRow = badgesRow,
            ActionsRow = actionsRow,
        };
        PopulateCardBadgesAndActions(rendered, item);
        return rendered;
    }

    private void UpdateVideoCardInPlace(RenderedVideoCard rendered, VideoCardViewModel item)
    {
        rendered.ViewsPillText.Text = item.ViewsText;
        rendered.DurationPillText.Text = item.DurationText;
        rendered.DurationPill.Visibility = string.IsNullOrEmpty(item.DurationText) ? Visibility.Collapsed : Visibility.Visible;
        rendered.ProgressTrack.Visibility = item.HasProgress ? Visibility.Visible : Visibility.Collapsed;
        rendered.ProgressFill.Width = Math.Clamp(192 * item.ProgressRatio, 4, 192);
        rendered.TitleText.Text = item.Title;
        rendered.FolderText.Text = item.FolderDisplay;

        if (rendered.ThumbImage.Source is null)
        {
            if (_thumbnailImageCache.TryGetValue(item.VideoPath, out var cachedBitmap))
            {
                rendered.ThumbImage.Source = cachedBitmap;
            }
            else
            {
                _ = LoadOrCaptureCardThumbnailAsync(item, rendered.ThumbImage);
            }
        }

        PopulateCardBadgesAndActions(rendered, item);
    }

    private void PopulateCardBadgesAndActions(RenderedVideoCard rendered, VideoCardViewModel item)
    {
        var badgesRow = rendered.BadgesRow;
        badgesRow.Children.Clear();

        badgesRow.Children.Add(new TextBlock
        {
            Text = $"{item.ViewsText} · {item.LastWatchedText}",
            FontSize = 11.5,
            Opacity = 0.78,
            VerticalAlignment = VerticalAlignment.Center,
        });

        if (item.HasSubtitle)
        {
            var subBadgeContent = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 4,
                VerticalAlignment = VerticalAlignment.Center,
            };
            subBadgeContent.Children.Add(new FontIcon
            {
                Glyph = "\uF2B7",
                FontSize = 11,
                Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0x6E, 0xF2, 0xB4)),
            });
            subBadgeContent.Children.Add(new TextBlock
            {
                Text = item.SubtitleStatusText,
                FontSize = 10.5,
                FontWeight = FontWeights.SemiBold,
                Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0x6E, 0xF2, 0xB4)),
            });

            var subBadge = new Border
            {
                Padding = new Thickness(6, 2, 6, 2),
                CornerRadius = new CornerRadius(5),
                Background = new SolidColorBrush(ColorHelper.FromArgb(0x33, 0x1E, 0x8E, 0x5A)),
                VerticalAlignment = VerticalAlignment.Center,
                Child = subBadgeContent,
            };
            ToolTipService.SetToolTip(subBadge, L("Altyazı / çeviri dosyası mevcut"));
            badgesRow.Children.Add(subBadge);
        }
        else
        {
            var noSubMetaBadge = MakeBadge(
                L("Altyazı yok"),
                ColorHelper.FromArgb(0x22, 0x80, 0x80, 0x8A),
                ColorHelper.FromArgb(0x99, 0xFF, 0xFF, 0xFF));
            ToolTipService.SetToolTip(noSubMetaBadge, L("Altyazı yok"));
            badgesRow.Children.Add(noSubMetaBadge);
        }

        if (item.IsLiked)
        {
            badgesRow.Children.Add(MakeBadge(
                $"♥ {L("Favori")}",
                ColorHelper.FromArgb(0x33, 0xFF, 0x4D, 0x6D),
                ColorHelper.FromArgb(0xFF, 0xFF, 0x85, 0x9D)));
        }

        if (item.Rating > 0)
        {
            badgesRow.Children.Add(MakeBadge(
                item.RatingBadgeText,
                ColorHelper.FromArgb(0x33, 0xFF, 0xC8, 0x3D),
                ColorHelper.FromArgb(0xFF, 0xFF, 0xD8, 0x6B)));
        }

        foreach (var listName in item.CustomListNames)
        {
            badgesRow.Children.Add(MakeBadge(
                $"☰ {listName}",
                ColorHelper.FromArgb(0x33, 0x2B, 0x7A, 0xFF),
                ColorHelper.FromArgb(0xFF, 0x8C, 0xB8, 0xFF)));
        }

        var actionsRow = rendered.ActionsRow;
        actionsRow.Children.Clear();

        var playButton = new Button
        {
            Padding = new Thickness(10, 4, 10, 4),
            CornerRadius = new CornerRadius(6),
            FontSize = 12,
            Content = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 5,
                Children =
                {
                    new FontIcon { Glyph = "\uE768", FontSize = 11 },
                    new TextBlock { Text = L("Oynat"), FontSize = 12, FontWeight = FontWeights.SemiBold },
                },
            },
        };
        playButton.Click += (_, _) => VideoSelected?.Invoke(item.VideoPath);
        actionsRow.Children.Add(playButton);

        if (_vm.IsLoggedIn)
        {
            var likeButton = new Button
            {
                Padding = new Thickness(10, 4, 10, 4),
                CornerRadius = new CornerRadius(6),
                FontSize = 12,
                Content = new StackPanel
                {
                    Orientation = Orientation.Horizontal,
                    Spacing = 5,
                    Children =
                    {
                        new FontIcon
                        {
                            Glyph = item.IsLiked ? "\uEB52" : "\uEB51",
                            FontSize = 12,
                            Foreground = item.IsLiked
                                ? new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0x4D, 0x6D))
                                : new SolidColorBrush(Colors.White),
                        },
                        new TextBlock { Text = item.LikeButtonText, FontSize = 12 },
                    },
                },
            };
            likeButton.Click += (_, _) => _vm.ToggleItemLike(item.VideoPath);
            actionsRow.Children.Add(likeButton);

            var starsRow = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 1,
                VerticalAlignment = VerticalAlignment.Center,
            };
            var goldBrush = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0xC8, 0x3D));
            var dimBrush = new SolidColorBrush(ColorHelper.FromArgb(0x80, 0xFF, 0xFF, 0xFF));

            for (var s = 1; s <= 5; s++)
            {
                var starNumber = s;
                var active = starNumber <= item.Rating;
                var starBtn = new Button
                {
                    Padding = new Thickness(4, 2, 4, 2),
                    MinWidth = 24,
                    MinHeight = 24,
                    CornerRadius = new CornerRadius(4),
                    Background = new SolidColorBrush(Colors.Transparent),
                    BorderThickness = new Thickness(0),
                    Content = new FontIcon
                    {
                        Glyph = active ? "\uE735" : "\uE734",
                        FontSize = 13,
                        Foreground = active ? goldBrush : dimBrush,
                    },
                };
                var tip = L("%lld yıldız", (long)starNumber);
                ToolTipService.SetToolTip(starBtn, tip);
                AutomationProperties.SetName(starBtn, tip);
                starBtn.Click += (_, _) =>
                {
                    var nextRating = item.Rating == starNumber ? 0 : starNumber;
                    _vm.SetItemRating(item.VideoPath, nextRating);
                };
                starsRow.Children.Add(starBtn);
            }

            actionsRow.Children.Add(starsRow);

            // "+ Listeye ekle" menu button on card
            var cardListFlyout = new MenuFlyout();
            cardListFlyout.Opening += (_, _) => PlayerEngagementBar.PopulateAddToListFlyout(cardListFlyout, _vm, item.VideoPath);

            var addToListBtn = new Button
            {
                Padding = new Thickness(10, 4, 10, 4),
                CornerRadius = new CornerRadius(6),
                FontSize = 12,
                Flyout = cardListFlyout,
                Content = new StackPanel
                {
                    Orientation = Orientation.Horizontal,
                    Spacing = 5,
                    Children =
                    {
                        new FontIcon { Glyph = "\uE710", FontSize = 11 },
                        new TextBlock { Text = L("+ Listeye ekle"), FontSize = 12 },
                    },
                },
            };
            actionsRow.Children.Add(addToListBtn);
        }
    }

    private async Task LoadOrCaptureCardThumbnailAsync(VideoCardViewModel item, Image targetImage)
    {
        if (string.IsNullOrWhiteSpace(item.VideoPath)) return;

        if (_thumbnailImageCache.TryGetValue(item.VideoPath, out var existingBmp))
        {
            targetImage.Source = existingBmp;
            return;
        }

        if (!_thumbnailLoadingPaths.Add(item.VideoPath))
        {
            return;
        }

        try
        {
            var bytes = item.ThumbnailBytes;
            if (bytes is null || bytes.Length == 0)
            {
                var pathCopy = item.VideoPath;
                bytes = await Task.Run(() => _vm.GetEncryptedThumbnail(pathCopy));
            }
            if (bytes is { Length: > 0 })
            {
                await ApplyAndCacheThumbnailBytesAsync(item.VideoPath, bytes, targetImage);
                return;
            }

            if (!File.Exists(item.VideoPath))
            {
                return;
            }

            byte[]? capturedBytes = null;
            await _thumbnailExtractGate.WaitAsync();
            try
            {
                var file = await StorageFile.GetFileFromPathAsync(item.VideoPath);
                using var thumb = await file.GetThumbnailAsync(ThumbnailMode.VideosView, 240, ThumbnailOptions.UseCurrentScale);
                if (thumb is not null && thumb.Size > 0)
                {
                    capturedBytes = new byte[thumb.Size];
                    using var reader = new DataReader(thumb.GetInputStreamAt(0));
                    await reader.LoadAsync((uint)thumb.Size);
                    reader.ReadBytes(capturedBytes);
                }
            }
            finally
            {
                _thumbnailExtractGate.Release();
            }

            if (capturedBytes is { Length: > 0 })
            {
                if (_vm.IsLoggedIn)
                {
                    var pathToSave = item.VideoPath;
                    var bytesToSave = capturedBytes;
                    _ = Task.Run(() => _vm.SaveEncryptedThumbnail(pathToSave, bytesToSave));
                }
                await ApplyAndCacheThumbnailBytesAsync(item.VideoPath, capturedBytes, targetImage);
            }
        }
        catch
        {
            // Ignore thumbnail extraction errors for unsupported or inaccessible files.
        }
        finally
        {
            _thumbnailLoadingPaths.Remove(item.VideoPath);
        }
    }

    private async Task ApplyAndCacheThumbnailBytesAsync(string videoPath, byte[] bytes, Image targetImage)
    {
        using var stream = new InMemoryRandomAccessStream();
        using (var writer = new DataWriter(stream))
        {
            writer.WriteBytes(bytes);
            await writer.StoreAsync();
            await writer.FlushAsync();
            writer.DetachStream();
        }
        stream.Seek(0);

        var bmp = new BitmapImage
        {
            DecodePixelWidth = 192,
        };
        await bmp.SetSourceAsync(stream);
        _thumbnailImageCache[videoPath] = bmp;
        targetImage.Source = bmp;

        foreach (var card in _renderedCards)
        {
            if (card.ThumbImage.Source is null &&
                string.Equals(card.VideoPath, videoPath, StringComparison.OrdinalIgnoreCase))
            {
                card.ThumbImage.Source = bmp;
            }
        }
    }

    private static Border MakeBadge(string text, Windows.UI.Color bg, Windows.UI.Color fg) =>
        new()
        {
            Padding = new Thickness(6, 2, 6, 2),
            CornerRadius = new CornerRadius(5),
            Background = new SolidColorBrush(bg),
            VerticalAlignment = VerticalAlignment.Center,
            Child = new TextBlock
            {
                Text = text,
                FontSize = 10.5,
                FontWeight = FontWeights.SemiBold,
                Foreground = new SolidColorBrush(fg),
            },
        };
}
