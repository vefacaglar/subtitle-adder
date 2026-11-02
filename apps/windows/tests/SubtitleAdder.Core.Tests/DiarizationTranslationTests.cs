using SubtitleAdder.Core;
using SubtitleAdder.EngineKit;

namespace SubtitleAdder.Core.Tests;

public class DiarizationTranslationTests
{
    [Theory]
    [InlineData(1)]
    [InlineData(8)]
    [InlineData(14)]
    public async Task PendingTextTranslatesWhileDiarizationIsStillRunning(int sentenceCount)
    {
        var previous = (ExternalLlmState.Shared.Enabled, ExternalLlmState.Shared.Url, ExternalLlmState.Shared.Model);
        var engine = new PipelinedTestEngineClient();
        using var vm = new SubtitleViewModel(new ModelCatalog(engine), new InMemorySettingsStore());
        try
        {
            vm.OfflineMode = false;
            vm.AutoSaveSubtitle = false;
            vm.Language = "en";
            vm.TranslationTarget = "tr";
            vm.ExternalLlmEnabled = true;
            vm.ExternalLlmUrl = "http://translation.test/v1";
            vm.ExternalLlmModel = "remote-model";
            vm.ExternalLlmAsync = true;
            vm.Diarize = true;
            vm.OpenVideo("diarization-overlap.mp4");
            vm.GenerateAndTranslate();
            var sentences = Enumerable.Range(1, sentenceCount)
                .Select(i => new Sentence(i, i * 2, i * 2 + 1, $"English sentence {i}.")).ToArray();
            var transcription = Assert.Single(engine.TranscribeCalls).Channel;
            transcription.Writer.TryWrite(new EngineEvent.Partial("en", sentences));
            await Make.Eventually(() => vm.Sentences.Count == sentenceCount, "ASR text");
            if (sentenceCount >= 12)
                await Make.Eventually(() => engine.TranslateCalls.Count == 1, "initial background batch");
            else
                Assert.Empty(engine.TranslateCalls);

            transcription.Writer.TryWrite(new EngineEvent.Stage(EngineStage.Diarizing, null));
            await Make.Eventually(() => vm.Status is JobStatus.Working w && w.Label.Id.StartsWith("stage:Diarizing:"),
                "diarization started");
            await Make.Eventually(() => engine.TranslateCalls.Count == 1, "translation before diarization result");

            var first = engine.TranslateCalls[0].Sentences;
            engine.FinishTranslate(0, new EngineEvent.Translation("tr",
                first.Select(s => new Sentence(s.Id, s.Start, s.End, $"Çeviri {s.Id}")).ToArray()));
            if (sentenceCount >= 12)
            {
                // The two held-back lines must drain without another Partial or Result event.
                await Make.Eventually(() => engine.TranslateCalls.Count == 2, "final two lines during diarization");
                var tail = engine.TranslateCalls[1].Sentences;
                engine.FinishTranslate(1, new EngineEvent.Translation("tr",
                    tail.Select(s => new Sentence(s.Id, s.Start, s.End, $"Çeviri {s.Id}")).ToArray()));
            }
            await Make.Eventually(() => vm.Translations.Count == sentenceCount, "all text translated before speaker result");
            Assert.False(transcription.Reader.Completion.IsCompleted);
            Assert.True(vm.IsWorking);
            var callsBeforeResult = engine.TranslateCalls.Count;
            engine.FinishTranscribe(0, Make.Result("en", "", sentences));
            await vm.CurrentJob.WaitAsync(TimeSpan.FromSeconds(10));
            Assert.Equal(callsBeforeResult, engine.TranslateCalls.Count);
        }
        finally
        {
            vm.Cancel();
            (ExternalLlmState.Shared.Enabled, ExternalLlmState.Shared.Url, ExternalLlmState.Shared.Model) = previous;
        }
    }
}
