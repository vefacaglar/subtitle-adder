using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

public class TranslationContextTests
{
    [Fact]
    public void AsyncChunkIncludesNeighboringDialogueWithoutRenumbering()
    {
        var all = Enumerable.Range(10, 12).Select(i => new Sentence(i, i, i + 1, $"Line {i}")).ToList();
        var request = SubtitleViewModel.WithTranslationContext(all, [all[4], all[5]]);
        Assert.Equal(new[] { 12, 13, 14, 15, 16, 17 }, request.Select(s => s.Id));
        Assert.Equal(all[4].Uid, request[2].Uid);
    }

    [Fact]
    public void MissingLinesKeepTheirOwnContextInTimelineOrder()
    {
        var all = Enumerable.Range(1, 12).Select(i => new Sentence(i, i, i + 1, $"Line {i}")).ToList();
        var request = SubtitleViewModel.WithTranslationContext(all, [all[10], all[0]]);
        Assert.Equal(new[] { 1, 2, 3, 9, 10, 11, 12 }, request.Select(s => s.Id));
        Assert.Empty(SubtitleViewModel.WithTranslationContext(all, []));
    }

    [Fact]
    public async Task ExtraTranslationIdsCannotOverwriteUnrelatedSubtitlesByPosition()
    {
        var (vm, engine, _) = Make.ViewModel();
        vm.OpenVideo(@"C:\v\clip.mp4");
        vm.Language = "tr";
        vm.Generate();
        engine.Finish(Make.Result(new Sentence(1, 0, 1, "Bir."), new Sentence(2, 1, 2, "İki.")));
        await vm.CurrentJob;
        engine.Reset();
        vm.Translate();
        engine.Finish(new EngineEvent.Translation("en", [
            new Sentence(99, 0, 1, "Context only."), new Sentence(2, 1, 2, "Two.")
        ]));
        await vm.CurrentJob;
        Assert.Null(vm.TranslationFor(vm.Sentences[0]));
        Assert.Equal("Two.", vm.TranslationFor(vm.Sentences[1])?.Text);
        vm.Dispose();
    }
}
