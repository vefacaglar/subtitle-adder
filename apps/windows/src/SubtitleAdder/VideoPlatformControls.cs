using System.ComponentModel;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;
using Windows.Foundation;
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

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e) => Refresh();
    private void OnLocalizationChanged(object? sender, EventArgs e) => Refresh();

    private void AttemptLogin()
    {
        if (_vm.TryLogin(_usernameBox.Text, _passwordBox.Password))
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
        if (_vm.TryRegister(_usernameBox.Text, _passwordBox.Password))
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
/// and interactive 5-star rating bar (<c>\uE734</c>/<c>\uE735</c>).
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

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e) => Refresh();
    private void OnLocalizationChanged(object? sender, EventArgs e) => Refresh();

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
/// When logged out: displays a centered authentication card explaining that watch history, view counts, likes,
/// and ratings are protected by a local profile, with inline Username &amp; Password fields and Login/Register buttons.
/// When logged in: displays filter pills ("Keşfet", "İzleme geçmişi", "Beğenilenler", "En çok izlenenler", "Puanlananlar"),
/// search box ("Videolarda ara…"), and a scrollable feed of rich 16:9 cinema cards with view counts, CC badges,
/// playback progress bars, like buttons, and 5-star ratings.
/// </summary>
public sealed class VideoPlatformHubView : Grid
{
    private VideoPlatformViewModel _vm;

    // Logged-out auth card elements
    private readonly Border _authGateContainer;
    private readonly TextBlock _authTitle;
    private readonly TextBlock _authDescription;
    private readonly StackPanel _authExistingProfilesRow;
    private readonly TextBox _authUsernameBox;
    private readonly PasswordBox _authPasswordBox;
    private readonly TextBlock _authErrorText;
    private readonly Button _authLoginButton;
    private readonly Button _authRegisterButton;

    // Logged-in platform hub elements
    private readonly Grid _hubContainer;
    private readonly Dictionary<VideoFeedFilter, Button> _filterButtons = [];
    private readonly TextBox _searchBox;
    private readonly TextBlock _activeProfileBadgeText;
    private readonly Button _hubLogoutButton;
    private readonly StackPanel _cardsPanel;
    private readonly Border _emptyStateCard;
    private readonly TextBlock _emptyStateTitle;
    private readonly TextBlock _emptyStateSubtitle;
    private bool _updatingSearchBox;

    /// <summary>Fired when the user clicks a video card in the platform feed to open and play it immediately.</summary>
    public event Action<string>? VideoSelected;

    public VideoPlatformHubView() : this(null)
    {
    }

    public VideoPlatformHubView(VideoPlatformViewModel? viewModel)
    {
        _vm = viewModel ?? VideoPlatformViewModel.Shared;

        // 1. Build Logged-Out Auth Gate Card
        _authGateContainer = BuildAuthGateCard(
            out _authTitle,
            out _authDescription,
            out _authExistingProfilesRow,
            out _authUsernameBox,
            out _authPasswordBox,
            out _authErrorText,
            out _authLoginButton,
            out _authRegisterButton);

        // 2. Build Logged-In Hub Container
        _hubContainer = BuildHubContainer(
            out _searchBox,
            out _activeProfileBadgeText,
            out _hubLogoutButton,
            out _cardsPanel,
            out _emptyStateCard,
            out _emptyStateTitle,
            out _emptyStateSubtitle);

        Children.Add(_authGateContainer);
        Children.Add(_hubContainer);

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

    private Border BuildAuthGateCard(
        out TextBlock titleBlock,
        out TextBlock descBlock,
        out StackPanel existingProfilesRow,
        out TextBox usernameBox,
        out PasswordBox passwordBox,
        out TextBlock errorText,
        out Button loginBtn,
        out Button registerBtn)
    {
        var card = new Border
        {
            MaxWidth = 420,
            Padding = new Thickness(28, 26, 28, 26),
            CornerRadius = new CornerRadius(14),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x18, 0x80, 0x80, 0x88)),
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x30, 0x80, 0x80, 0x88)),
            BorderThickness = new Thickness(1),
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

    private Grid BuildHubContainer(
        out TextBox searchBox,
        out TextBlock activeProfileBadgeText,
        out Button logoutButton,
        out StackPanel cardsPanel,
        out Border emptyStateCard,
        out TextBlock emptyStateTitle,
        out TextBlock emptyStateSubtitle)
    {
        var root = new Grid
        {
            RowSpacing = 12,
            Padding = new Thickness(16, 12, 16, 12),
            Visibility = Visibility.Collapsed,
        };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });

        // Top bar: filter pills + search box + active profile chip
        var topBar = new Grid { ColumnSpacing = 12, VerticalAlignment = VerticalAlignment.Center };
        topBar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        topBar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        topBar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

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
                     VideoFeedFilter.MostWatched,
                     VideoFeedFilter.TopRated,
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
            pill.Click += (_, _) => _vm.ActiveFilter = captured;
            _filterButtons[captured] = pill;
            pillsPanel.Children.Add(pill);
        }

        Grid.SetColumn(pillsPanel, 0);
        topBar.Children.Add(pillsPanel);

        searchBox = new TextBox
        {
            MinWidth = 180,
            MaxWidth = 320,
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center,
        };
        searchBox.TextChanged += (s, _) =>
        {
            if (_updatingSearchBox) return;
            _vm.SearchQuery = ((TextBox)s).Text;
        };
        Grid.SetColumn(searchBox, 1);
        topBar.Children.Add(searchBox);

        var profileRightStack = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            VerticalAlignment = VerticalAlignment.Center,
        };
        activeProfileBadgeText = new TextBlock
        {
            FontSize = 12,
            Opacity = 0.8,
            VerticalAlignment = VerticalAlignment.Center,
        };
        logoutButton = new Button
        {
            Padding = new Thickness(10, 5, 10, 5),
            CornerRadius = new CornerRadius(6),
            FontSize = 12,
        };
        logoutButton.Click += (_, _) =>
        {
            _vm.Logout();
            _authPasswordBox.Password = "";
            Refresh();
        };
        profileRightStack.Children.Add(activeProfileBadgeText);
        profileRightStack.Children.Add(logoutButton);
        Grid.SetColumn(profileRightStack, 2);
        topBar.Children.Add(profileRightStack);

        Grid.SetRow(topBar, 0);
        root.Children.Add(topBar);

        // Scrollable video cards area
        var contentGrid = new Grid();

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
            MaxWidth = 380,
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

        emptyStateCard = new Border
        {
            Padding = new Thickness(24),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Child = emptyStack,
            Visibility = Visibility.Collapsed,
        };
        contentGrid.Children.Add(emptyStateCard);

        cardsPanel = new StackPanel
        {
            Spacing = 10,
            Orientation = Orientation.Vertical,
        };
        var scroller = new ScrollViewer
        {
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            Content = cardsPanel,
        };
        contentGrid.Children.Add(scroller);

        Grid.SetRow(contentGrid, 1);
        root.Children.Add(contentGrid);

        return root;
    }

    private void AttemptInlineLogin()
    {
        if (_vm.TryLogin(_authUsernameBox.Text, _authPasswordBox.Password))
        {
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
        if (_vm.TryRegister(_authUsernameBox.Text, _authPasswordBox.Password))
        {
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

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e) => Refresh();
    private void OnLocalizationChanged(object? sender, EventArgs e) => Refresh();

    public void Refresh()
    {
        var loggedIn = _vm.IsLoggedIn;
        _authGateContainer.Visibility = loggedIn ? Visibility.Collapsed : Visibility.Visible;
        _hubContainer.Visibility = loggedIn ? Visibility.Visible : Visibility.Collapsed;

        // Localize auth gate
        _authTitle.Text = L("Yerel Video Platformu");
        _authDescription.Text = L("İzleme geçmişi, görüntülenme sayısı, beğeni ve yıldız puanları yerel profilinize kaydedilir. Devam etmek için giriş yapın veya yeni profil oluşturun.");
        _authUsernameBox.PlaceholderText = L("Kullanıcı adı");
        _authPasswordBox.PlaceholderText = L("Şifre");
        _authLoginButton.Content = L("Giriş yap");
        _authRegisterButton.Content = L("Profil oluştur");

        if (!loggedIn)
        {
            RebuildAuthExistingProfileChips();
            RefreshAuthGateError();
            return;
        }

        // Localize filter pills
        UpdateFilterPill(VideoFeedFilter.All, L("Keşfet"));
        UpdateFilterPill(VideoFeedFilter.History, L("İzleme geçmişi"));
        UpdateFilterPill(VideoFeedFilter.Liked, L("Beğenilenler"));
        UpdateFilterPill(VideoFeedFilter.MostWatched, L("En çok izlenenler"));
        UpdateFilterPill(VideoFeedFilter.TopRated, L("Puanlananlar"));

        _searchBox.PlaceholderText = L("Videolarda ara…");
        if (_searchBox.Text != _vm.SearchQuery)
        {
            _updatingSearchBox = true;
            _searchBox.Text = _vm.SearchQuery;
            _updatingSearchBox = false;
        }

        _activeProfileBadgeText.Text = $"{_vm.CurrentUsername} · {_vm.ProfileSummaryText}";
        _hubLogoutButton.Content = L("Çıkış yap");

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
        _cardsPanel.Children.Clear();
        var items = _vm.FeedItems;

        if (items.Count == 0)
        {
            _emptyStateCard.Visibility = Visibility.Visible;
            _emptyStateTitle.Text = _vm.ActiveFilter switch
            {
                VideoFeedFilter.History => L("Henüz izleme geçmişi yok"),
                VideoFeedFilter.Liked => L("Henüz beğenilen video yok"),
                VideoFeedFilter.MostWatched => L("Henüz izlenen video yok"),
                VideoFeedFilter.TopRated => L("Henüz puanlanan video yok"),
                _ => L("Gösterilecek video bulunamadı"),
            };
            _emptyStateSubtitle.Text = L("Bir video açıp izlediğinizde, beğendiğinizde veya puanladığınızda burada YouTube tarzı kartlar halinde listelenir.");
            return;
        }

        _emptyStateCard.Visibility = Visibility.Collapsed;
        foreach (var item in items)
        {
            _cardsPanel.Children.Add(BuildVideoCard(item));
        }
    }

    private Border BuildVideoCard(VideoCardViewModel item)
    {
        var card = new Border
        {
            CornerRadius = new CornerRadius(12),
            Padding = new Thickness(12),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0x1C, 0x80, 0x80, 0x8A)),
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x2A, 0x80, 0x80, 0x8A)),
            BorderThickness = new Thickness(1),
        };

        var layout = new Grid { ColumnSpacing = 14 };
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(192) });
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        // Left: Dark 16:9 cinema preview header (192x108) with play glyph, CC badge, view count pill, and progress bar
        var thumbBg = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(1, 1) };
        thumbBg.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0xFF, 0x16, 0x18, 0x22), Offset = 0 });
        thumbBg.GradientStops.Add(new GradientStop { Color = ColorHelper.FromArgb(0xFF, 0x25, 0x2A, 0x3A), Offset = 1 });

        var thumbGrid = new Grid
        {
            Width = 192,
            Height = 108,
            Background = thumbBg,
        };

        // Play button circle in center
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

        // Top-left sidecar subtitle badge ("CC")
        if (item.HasSidecarSubtitle)
        {
            var ccBadge = new Border
            {
                Margin = new Thickness(7, 7, 0, 0),
                Padding = new Thickness(6, 2, 6, 2),
                CornerRadius = new CornerRadius(4),
                Background = new SolidColorBrush(ColorHelper.FromArgb(0xD8, 0x2B, 0x7A, 0xFF)),
                HorizontalAlignment = HorizontalAlignment.Left,
                VerticalAlignment = VerticalAlignment.Top,
                Child = new TextBlock
                {
                    Text = "CC",
                    FontSize = 10,
                    FontWeight = FontWeights.Bold,
                    Foreground = new SolidColorBrush(Colors.White),
                },
            };
            ToolTipService.SetToolTip(ccBadge, L("Altyazı hazır (CC)"));
            thumbGrid.Children.Add(ccBadge);
        }

        // Top-right view count pill ("3 görüntüleme")
        var viewsPill = new Border
        {
            Margin = new Thickness(0, 7, 7, 0),
            Padding = new Thickness(6, 2, 6, 2),
            CornerRadius = new CornerRadius(4),
            Background = new SolidColorBrush(ColorHelper.FromArgb(0xC8, 0x00, 0x00, 0x00)),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Top,
            Child = new TextBlock
            {
                Text = item.ViewsText,
                FontSize = 10.5,
                FontWeight = FontWeights.SemiBold,
                Foreground = new SolidColorBrush(Colors.White),
            },
        };
        thumbGrid.Children.Add(viewsPill);

        // Bottom-right duration pill if known
        if (!string.IsNullOrEmpty(item.DurationText))
        {
            var durationPill = new Border
            {
                Margin = new Thickness(0, 0, 7, 9),
                Padding = new Thickness(5, 1, 5, 1),
                CornerRadius = new CornerRadius(4),
                Background = new SolidColorBrush(ColorHelper.FromArgb(0xD0, 0x00, 0x00, 0x00)),
                HorizontalAlignment = HorizontalAlignment.Right,
                VerticalAlignment = VerticalAlignment.Bottom,
                Child = new TextBlock
                {
                    Text = item.DurationText,
                    FontSize = 10.5,
                    Foreground = new SolidColorBrush(Colors.White),
                },
            };
            thumbGrid.Children.Add(durationPill);
        }

        // Bottom playback progress bar if partially watched
        if (item.HasProgress)
        {
            var progressTrack = new Border
            {
                Height = 4,
                Background = new SolidColorBrush(ColorHelper.FromArgb(0x55, 0xFF, 0xFF, 0xFF)),
                VerticalAlignment = VerticalAlignment.Bottom,
                HorizontalAlignment = HorizontalAlignment.Stretch,
                Child = new Border
                {
                    Height = 4,
                    Width = Math.Clamp(192 * item.ProgressRatio, 4, 192),
                    Background = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xFF, 0x2E, 0x4C)),
                    HorizontalAlignment = HorizontalAlignment.Left,
                },
            };
            thumbGrid.Children.Add(progressTrack);
        }

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

        // Right: Video title, folder path, last watched timestamp, Like button, and 5-star rating buttons
        var detailsStack = new StackPanel
        {
            Spacing = 5,
            VerticalAlignment = VerticalAlignment.Center,
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

        var metaText = new TextBlock
        {
            Text = $"{item.ViewsText} · {item.LastWatchedText}",
            FontSize = 11.5,
            Opacity = 0.78,
        };
        detailsStack.Children.Add(metaText);

        // Engagement actions row on card: Play button, Heart (Like) button, and 5-star rating buttons
        var actionsRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Margin = new Thickness(0, 4, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };

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
        detailsStack.Children.Add(actionsRow);

        Grid.SetColumn(detailsStack, 1);
        layout.Children.Add(detailsStack);

        card.Child = layout;
        return card;
    }
}
