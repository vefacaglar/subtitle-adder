using SubtitleAdder.Core;

namespace SubtitleAdder.Core.Tests;

public class SelectedTranslationTests
{
    [Fact]
    public async Task SelectedRetryKeepsOtherTranslationsAndSnapshotsSelection()
    {
        var (vm, engine, _) = Make.ViewModel();
        using var lifetime = vm;
        vm.Language = "en";
        vm.TranslationTarget = "tr";
        vm.Sentences = Enumerable.Range(1, 10).Select(i => new Sentence(i, i, i + 1, $"Source {i}")).ToList();
        vm.Translate();
        engine.Finish(new EngineEvent.Translation("tr", vm.Sentences.Select(s => s with { Text = $"Eski {s.Id}" }).ToList()));
        await vm.CurrentJob;
        engine.Reset();
        vm.SetSelectedLines([vm.Sentences[1].Uid, vm.Sentences[8].Uid]);
        vm.TranslateSelected();
        Assert.False(vm.CanTranslateSelected);
        Assert.Equal(new[] { 1, 2, 3, 4, 7, 8, 9, 10 }, engine.TranslateCalls.Last().Sentences.Select(s => s.Id));
        vm.Selection = vm.Sentences[4].Uid;
        engine.Finish(new EngineEvent.Translation("tr", [
            new Sentence(1, 1, 2, "Context must not overwrite"),
            new Sentence(2, 2, 3, "Yeni 2"), new Sentence(9, 9, 10, "Yeni 9")
        ]));
        await vm.CurrentJob;
        Assert.Equal(Enumerable.Range(1, 10).Select(i => i is 2 or 9 ? $"Yeni {i}" : $"Eski {i}"),
            vm.ExportSentences(true).Select(s => s.Text));
        Assert.True(vm.CanTranslateSelected);
    }

    [Fact]
    public async Task ImportedPartialSubtitleCanBeRepairedAndSavedWithoutVideo()
    {
        var path = Path.Combine(Path.GetTempPath(), $"partial-{Guid.NewGuid()}.srt");
        try
        {
            await File.WriteAllTextAsync(path, "1\n00:00:01,000 --> 00:00:02,000\nHazır çeviri.\n\n2\n00:00:03,000 --> 00:00:04,500\nWhere are you going?\n\n3\n00:00:05,000 --> 00:00:06,000\nBu da hazır.\n");
            var (vm, engine, _) = Make.ViewModel();
            using var lifetime = vm;
            Assert.True(vm.TryLoadSubtitleFile(path));
            vm.Language = "en";
            vm.TranslationTarget = "tr";
            var original = vm.Sentences.ToList();
            vm.Selection = original[1].Uid;
            vm.TranslateSelected();
            engine.Finish(new EngineEvent.Translation("tr", [new Sentence(original[1].Id, 0, 99, "Nereye gidiyorsun?")]));
            await vm.CurrentJob;
            Assert.True(vm.CanSaveSubtitle);
            Assert.Equal(new[] { "Hazır çeviri.", "Nereye gidiyorsun?", "Bu da hazır." }, vm.ExportSentences(true).Select(s => s.Text));
            Assert.Equal(original.Select(s => (s.Uid, s.Start, s.End)), vm.ExportSentences(true).Select(s => (s.Uid, s.Start, s.End)));
            Assert.Equal("Hazır çeviri.|Nereye gidiyorsun?|Bu da hazır.", await File.ReadAllTextAsync(path));
            await vm.RenderAsync(SubtitleFormat.Srt, translated: true);
            Assert.All(engine.RenderCalls, call => Assert.Equal("Nereye gidiyorsun?", call.Sentences[1].Text));
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public async Task FailedRetryKeepsPreviousTextAndMarksItUntilEdited()
    {
        var (vm, engine, _) = Make.ViewModel();
        using var lifetime = vm;
        vm.Language = "en";
        vm.TranslationTarget = "tr";
        var sentence = new Sentence(1, 0, 1, "Where are you going?");
        vm.Sentences = [sentence];
        vm.Translate();
        engine.Finish(new EngineEvent.Translation("tr", [sentence with { Text = "Nereye gidiyorsun?" }]));
        await vm.CurrentJob;
        engine.Reset();
        vm.Selection = sentence.Uid;
        vm.TranslateSelected();
        engine.Finish(new EngineEvent.Translation("tr", [sentence with { NeedsReview = true }]));
        await vm.CurrentJob;
        var entry = vm.TranslationFor(sentence)!;
        Assert.Equal("Nereye gidiyorsun?", entry.Text);
        Assert.True(entry.NeedsReview);
        Assert.False(SubtitleViewModel.IsReusableTranslation(sentence, entry, "tr"));
        Assert.True(vm.ExportSentences(true)[0].NeedsReview);
        vm.SetTranslationText("Nereye böyle?", sentence.Uid);
        Assert.False(vm.TranslationFor(sentence)!.NeedsReview);
    }

    [Fact]
    public void RemovedAndUnknownLinesCannotRemainSelectedOrStartAJob()
    {
        var (vm, engine, _) = Make.ViewModel();
        using var lifetime = vm;
        var first = new Sentence(1, 0, 1, "One");
        var second = new Sentence(2, 1, 2, "Two");
        vm.Sentences = [first, second];
        vm.SetSelectedLines([first.Uid, second.Uid, Guid.NewGuid()]);
        Assert.Equal(2, vm.SelectedUids.Count);
        vm.Sentences = [second];
        Assert.Equal(second.Uid, Assert.Single(vm.SelectedUids));
        vm.Sentences = [];
        Assert.Empty(vm.SelectedUids);
        Assert.Null(vm.Selection);
        vm.TranslateSelected();
        Assert.Empty(engine.TranslateCalls);
    }
}
