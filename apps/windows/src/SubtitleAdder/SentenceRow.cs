using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Xaml.Media;
using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder;

/// <summary>
/// One row of the line list. The view model replaces <see cref="SubtitleViewModel.Sentences"/> as a whole on every
/// change; rows are kept per <see cref="Sentence.Uid"/> and updated in place so a focused text box keeps its caret.
/// Values are display-ready (computed from the view model); nothing here decides anything.
/// Plain properties rather than [ObservableProperty]: the XAML pre-compile pass does not run source generators.
/// </summary>
public sealed class SentenceRow(Guid uid) : ObservableObject
{
    public Guid Uid { get; } = uid;

    public string Number { get => field; set => SetProperty(ref field, value); } = "";
    public string TimeText { get => field; set => SetProperty(ref field, value); } = "";
    public string Text { get => field; set => SetProperty(ref field, value); } = "";
    public bool ShowsSpeaker { get => field; set => SetProperty(ref field, value); }
    public string SpeakerText { get => field; set => SetProperty(ref field, value); } = "";
    public bool HasSpeaker { get => field; set => SetProperty(ref field, value); }
    public Brush? SpeakerBrush { get => field; set => SetProperty(ref field, value); }
    public bool HasTranslation { get => field; set => SetProperty(ref field, value); }
    public string TranslationText { get => field; set => SetProperty(ref field, value); } = "";
    public bool IsStale { get => field; set => SetProperty(ref field, value); }
    public bool IsActive { get => field; set => SetProperty(ref field, value); }

    /// <summary>The list item's accessible name (ListViewItem reads the item's ToString).</summary>
    public override string ToString() => $"{Number}. {Text}";
}
