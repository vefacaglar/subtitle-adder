"""Speaker diarization: word assignment, splitting, dash rule, pipeline and CLI wiring (no network)."""
import io
import json
import sys
from pathlib import Path

import pytest
from conftest import make_words

from subtitle_engine import cli, diarize, models, pipeline, subtitles
from subtitle_engine.diarize import SpeakerSegment, assign_speakers, normalize
from subtitle_engine.models import Sentence, Transcript, Word
from subtitle_engine.sentences import segment


def seg(start, end, speaker):
    return SpeakerSegment(start, end, speaker)


def test_word_gets_speaker_with_largest_overlap():
    words = [Word(0.0, 1.0, "a"), Word(1.0, 2.0, "b"), Word(2.0, 3.0, "c")]
    segments = [seg(0.0, 1.2, 0), seg(1.2, 3.0, 1)]
    # "b" overlaps speaker 0 for 0.2 s and speaker 1 for 0.8 s
    assert assign_speakers(words, segments) == [0, 1, 1]


def test_word_without_overlap_uses_nearest_segment():
    words = [Word(5.0, 5.4, "x"), Word(9.0, 9.4, "y")]
    segments = [seg(0.0, 4.0, 0), seg(10.0, 12.0, 1)]
    assert assign_speakers(words, segments) == [0, 1]


def test_no_segments_means_no_speakers():
    assert assign_speakers(make_words("a b c"), []) == [None, None, None]


def test_single_word_island_joins_surrounding_speaker():
    words = make_words("a b c d e")
    segments = [seg(0.0, 0.8, 0), seg(0.8, 1.2, 1), seg(1.2, 3.0, 0)]
    assert assign_speakers(words, segments) == [0, 0, 0, 0, 0]


def test_normalize_renumbers_by_first_appearance_and_drops_noise():
    segments = [seg(5.0, 9.0, 3), seg(0.0, 4.0, 7), seg(4.0, 4.3, 5)]
    result = normalize(segments)
    assert [(s.start, s.speaker) for s in result] == [(0.0, 0), (5.0, 1)]  # speaker 5 spoke 0.3 s
    kept = normalize(segments, drop_noise=False)
    assert [s.speaker for s in kept] == [0, 1, 2]


def test_sentence_splits_at_speaker_change():
    words = make_words("evet tamam olur peki sen nasılsın")
    result = segment(words, speakers=[0, 0, 0, 1, 1, 1])
    assert [(s.text, s.speaker) for s in result] == [("evet tamam olur", 0), ("peki sen nasılsın", 1)]
    assert [s.id for s in result] == [1, 2]


def test_no_speakers_keeps_old_behaviour():
    result = segment(make_words("Merhaba dünya. Nasılsın?"))
    assert [s.speaker for s in result] == [None, None]


def test_speaker_list_length_mismatch_is_rejected():
    with pytest.raises(ValueError):
        segment(make_words("a b"), speakers=[0])


def s(id, text, speaker):
    return Sentence(id=id, start=float(id), end=id + 0.9, text=text, speaker=speaker)


def test_dash_rule_two_speakers():
    sentences = [s(1, "Selam.", 0), s(2, "Nasılsın?", 0), s(3, "İyiyim.", 1), s(4, "Sen?", 0)]
    assert subtitles.speaker_dashes(sentences) == [True, False, True, True]
    srt = subtitles.render(sentences, "srt")
    assert "\n- Selam.\n" in srt and "\nNasılsın?\n" in srt and "\n- İyiyim.\n" in srt and "\n- Sen?\n" in srt
    vtt = subtitles.render(sentences, "vtt")
    assert "\n- Selam.\n" in vtt and "\nNasılsın?\n" in vtt and "\n- Sen?\n" in vtt


@pytest.mark.parametrize("speakers", [[0, 0, 0], [None, None, None]])
def test_no_dashes_for_single_or_null_speaker(speakers):
    sentences = [s(i, f"Cümle {i}.", sp) for i, sp in enumerate(speakers, 1)]
    assert not any(subtitles.speaker_dashes(sentences))
    assert "- " not in subtitles.render(sentences, "srt")


def test_json_keeps_speaker_field():
    data = json.loads(subtitles.render([s(1, "A.", 1), s(2, "B.", None)], "json"))
    assert [d["speaker"] for d in data] == [1, None]
    assert Sentence.from_dict(data[0]).speaker == 1
    assert Sentence.from_dict({"id": 1, "start": 0, "end": 1, "text": "x"}).speaker is None


def test_render_command_reads_speaker_from_stdin(capsys, monkeypatch):
    payload = [
        {"id": 1, "start": 0.0, "end": 1.0, "text": "Selam.", "words": [], "speaker": 0},
        {"id": 2, "start": 1.0, "end": 2.0, "text": "Merhaba.", "words": [], "speaker": 1},
    ]
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(payload)))
    assert cli.main(["render", "-f", "srt"]) == 0
    out = capsys.readouterr().out
    assert "- Selam." in out and "- Merhaba." in out


class FakeTranscriber:
    def transcribe(self, audio, language=None, on_progress=None):
        return Transcript(language="tr", duration=2.0, words=make_words("Selam nasılsın iyiyim sen"))


class FakeDiarizer:
    def __init__(self):
        self.calls = []

    def diarize(self, audio, num_speakers=None, on_progress=None):
        self.calls.append(num_speakers)
        on_progress and on_progress(0.5)
        # words last 0.4 s each: the first three belong to speaker 0
        return [seg(0.0, 1.2, 4), seg(1.2, 2.0, 2)]


def test_pipeline_diarize_emits_stages_and_speakers(tone_video, tmp_path, monkeypatch):
    stages = []
    monkeypatch.setattr(models, "needs_download", lambda model_id: model_id == diarize.MODEL_ID)
    monkeypatch.setattr(
        models, "ensure_model", lambda model_id, on_progress=None: (on_progress(0.5), on_progress(1.0))
    )
    fake = FakeDiarizer()
    result = pipeline.run(
        tone_video,
        FakeTranscriber,
        tmp_path,
        diarize=True,
        num_speakers=2,
        diarizer_factory=lambda: fake,
        on_stage=lambda stage, fraction: stages.append((stage, fraction)),
    )
    names = [name for name, _ in stages]
    assert names.index("downloading_model") < names.index("loading_model") < names.index("transcribing")
    assert names.index("transcribing") < names.index("diarizing") < names.index("segmenting")
    assert ("diarizing", 0.5) in stages
    assert fake.calls == [2]
    assert [(x.text, x.speaker) for x in result.sentences] == [
        ("Selam nasılsın iyiyim", 0),
        ("sen", 1),
    ]


def test_pipeline_without_diarize_leaves_speaker_null(tone_video, tmp_path):
    result = pipeline.run(tone_video, FakeTranscriber, tmp_path)
    assert all(x.speaker is None for x in result.sentences)


def test_cli_diarize_flag_and_speakers_validation(tone_video, tmp_path, capsys, monkeypatch):
    monkeypatch.setattr("subtitle_engine.transcribe.FasterWhisperTranscriber", lambda model: FakeTranscriber())
    monkeypatch.setattr(models, "needs_download", lambda model_id: False)
    monkeypatch.setattr(diarize, "create_diarizer", FakeDiarizer)
    argv = ["transcribe", str(tone_video), "-o", str(tmp_path), "--json"]

    assert cli.main([*argv, "--diarize", "--speakers", "2"]) == 0
    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    assert "diarizing" in [e["stage"] for e in events if e["event"] == "stage"]
    assert [x["speaker"] for x in events[-1]["sentences"]] == [0, 1]

    assert cli.main([*argv, "--speakers", "2"]) == 1  # --speakers needs --diarize
    assert json.loads(capsys.readouterr().out.splitlines()[-1])["event"] == "error"


def make_two_repo_cache(tmp_path):
    from test_models import make_cached_repo

    info = models.find(diarize.MODEL_ID)
    (extra_repo, (extra_file,)) = info.extra[0]
    make_cached_repo(tmp_path, info.repo, {info.files[0]: 1_048_576})
    make_cached_repo(tmp_path, extra_repo, {extra_file: 2_097_152})
    return info


def test_diarization_model_delete_removes_both_repos(tmp_path):
    info = make_two_repo_cache(tmp_path)
    assert models.delete_model(info.id, cache_dir=tmp_path) == 3
    assert not any(p.name.startswith("models--") for p in tmp_path.iterdir())


def test_diarization_model_disk_usage_sums_both_repos(monkeypatch):
    info = models.find(diarize.MODEL_ID)
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: True)
    seg_key = f"{info.repo}#{info.files[0]}"
    emb_key = f"{info.extra[0][0]}#{info.extra[0][1][0]}"
    assert models.disk_mb(info.id, {seg_key: 1, emb_key: 28, info.extra[0][0]: 5000}) == 29  # only owned files
    assert models.disk_mb(info.id, {seg_key: 1}) is None  # one file missing


def test_diarization_model_is_downloaded_needs_both_repos(monkeypatch):
    info = models.find(diarize.MODEL_ID)
    monkeypatch.setattr(models, "_cached", lambda repo, name: repo == info.repo)
    assert not models.is_downloaded(info.id)
    monkeypatch.setattr(models, "_cached", lambda repo, name: True)
    assert models.is_downloaded(info.id)


@pytest.mark.slow
def test_real_diarization_separates_two_voices(tmp_path):
    sample = Path(__file__).parents[2] / "samples" / "dialog-en.mp4"
    if not sample.exists():
        pytest.skip("samples/dialog-en.mp4 missing (see the two-voice sample command in CLAUDE.md)")
    from subtitle_engine.transcribe import create_transcriber

    models.ensure_model(diarize.MODEL_ID)
    result = pipeline.run(
        sample,
        lambda: create_transcriber("base"),
        tmp_path,
        language="en",
        model="base",
        formats=("srt", "json"),
        diarize=True,
    )
    speakers = [x.speaker for x in result.sentences]
    assert set(speakers) == {0, 1}
    # alternating turns: the speaker changes several times and every sentence has exactly one speaker
    changes = sum(a != b for a, b in zip(speakers, speakers[1:]))
    assert changes >= 4
    assert result.sentences[0].speaker == 0
    assert (tmp_path / "dialog-en.srt").read_text().startswith("1\n00:00:00")
    assert "- Hello" in (tmp_path / "dialog-en.srt").read_text()


# --- smoothing, boundary snapping ---------------------------------------------------------------

def tw(text, start, end):
    return Word(start, end, text)


def test_smoothing_keeps_a_complete_one_word_reply():
    words = [tw("Hazır", 0, 0.4), tw("mısın?", 0.4, 0.8), tw("Evet.", 0.8, 1.2), tw("Başlayalım", 1.2, 1.7), tw("hemen.", 1.7, 2.1)]
    segments = [seg(0.0, 0.85, 0), seg(0.85, 1.2, 1), seg(1.2, 2.2, 0)]
    assert assign_speakers(words, segments) == [0, 0, 1, 0, 0]


def test_smoothing_still_joins_a_noise_island_mid_sentence():
    words = make_words("bunu şimdi yapmak istiyorum çünkü")
    segments = [seg(0.0, 0.9, 0), seg(0.9, 1.2, 1), seg(1.2, 2.4, 0)]
    assert assign_speakers(words, segments) == [0] * 5


def test_alternating_single_word_turns_do_not_cascade():
    words = [tw(t, i * 0.5, i * 0.5 + 0.4) for i, t in enumerate(["A.", "B.", "C.", "D.", "E.", "F."])]
    segments = [seg(i * 0.5, i * 0.5 + 0.45, i % 2) for i in range(6)]
    assert assign_speakers(words, segments) == [0, 1, 0, 1, 0, 1]
    mid_sentence = make_words("a b c d e f")  # no punctuation, still no collapse
    assert assign_speakers(mid_sentence, [seg(w.start, w.end, i % 2) for i, w in enumerate(mid_sentence)]) == [0, 1, 0, 1, 0, 1]


def dersin_words():
    # dialog-tr: speaker 0 asks, the last word of the question is glued to the next turn.
    return [
        tw("Bu", 10.70, 10.95), tw("harika", 10.95, 11.4), tw("bir", 11.4, 11.6), tw("haber,", 11.6, 12.1),
        tw("ikinci", 12.1, 12.6), tw("bölüm", 12.6, 13.0), tw("ne", 13.0, 13.2), tw("zaman", 13.2, 13.6),
        tw("hazır", 13.6, 13.9), tw("olur", 13.9, 14.22), tw("dersin?", 14.22, 14.80),
        tw("Bence", 15.08, 15.4), tw("yaklaşık", 15.4, 15.9), tw("iki", 15.9, 16.1), tw("hafta.", 16.1, 16.5),
    ]


def test_forward_snap_returns_sentence_final_word_to_previous_speaker():
    segments = [seg(10.6, 14.24, 0), seg(14.24, 17.0, 1)]
    speakers = assign_speakers(dersin_words(), segments)
    assert speakers[:11] == [0] * 11  # "dersin?" stays with the asker
    assert speakers[11:] == [1] * 4
    result = segment(dersin_words(), speakers=speakers)
    assert [(x.text.split()[-1], x.speaker) for x in result][0] == ("dersin?", 0)


def test_backward_snap_moves_start_of_next_sentence_to_new_speaker():
    words = [
        tw("Tamam.", 0.0, 0.5), tw("Peki", 0.8, 1.1), tw("bunu", 1.1, 1.4), tw("yapalım", 1.4, 1.9), tw("mı?", 1.9, 2.2),
    ]
    segments = [seg(0.0, 1.45, 0), seg(1.45, 2.3, 1)]
    # "Peki bunu" starts B's sentence but the boundary arrived late
    assert assign_speakers(words, segments) == [0, 1, 1, 1, 1]


def test_change_right_after_sentence_end_is_never_moved():
    words = [tw("Hazır", 0, 0.4), tw("mısın?", 0.4, 0.8), tw("Evet.", 0.8, 1.2), tw("Tamam.", 1.2, 1.6)]
    segments = [seg(0.0, 0.8, 0), seg(0.8, 1.7, 1)]
    assert assign_speakers(words, segments) == [0, 0, 1, 1]


def test_snap_ignores_long_fragments():
    words = [tw("bir", 0, 0.5), tw("iki", 0.5, 1.0), tw("üç", 1.0, 1.5), tw("dört", 1.5, 2.0), tw("beş", 2.0, 3.0), tw("altı.", 3.0, 4.0)]
    # a 3-word / 1.5 s tail would be needed to reach the sentence end: leave the change alone
    segments = [seg(0.0, 1.5, 0), seg(1.5, 4.1, 1)]
    assert assign_speakers(words, segments) == [0, 0, 0, 1, 1, 1]


def test_assign_speakers_matches_naive_overlap_on_many_segments():
    import random

    rng = random.Random(1)
    t, segments = 0.0, []
    for _ in range(300):
        length = rng.uniform(0.5, 3.0)
        segments.append(seg(t, t + length, rng.randrange(3)))
        t += length + rng.choice([0.0, 0.0, 0.7])
    words = [tw(f"w{i}", i * 0.1, i * 0.1 + 0.08) for i in range(int(t / 0.1))]
    index = diarize._SegmentIndex(segments)

    def naive(word):
        overlap = {}
        for g in segments:
            shared = min(word.end, g.end) - max(word.start, g.start)
            if shared > 0:
                overlap[g.speaker] = overlap.get(g.speaker, 0.0) + shared
        if overlap:
            return max(overlap.items(), key=lambda kv: (kv[1], -kv[0]))[0]
        middle = (word.start + word.end) / 2
        return min(segments, key=lambda g: (diarize._distance(middle, g), g.start)).speaker

    assert [index.speaker_for(w) for w in words] == [naive(w) for w in words]


# --- post-clustering merge ----------------------------------------------------------------------

def test_merge_similar_speakers_merges_close_centroids_only():
    import numpy as np

    # Fake embedder: the "audio" is a constant array whose value picks the vector.
    vectors = {1.0: [1.0, 0.0, 0.0], 2.0: [0.98, 0.2, 0.0], 3.0: [0.0, 0.0, 1.0]}
    samples = np.concatenate([np.full(16000 * 2, v, dtype=np.float32) for v in (1.0, 2.0, 3.0, 1.0)])
    segments = [seg(0, 2, 0), seg(2, 4, 1), seg(4, 6, 2), seg(6, 8, 0)]
    merged = diarize.merge_similar_speakers(segments, lambda chunk: vectors[float(chunk[0])], samples)
    assert [s.speaker for s in merged] == [0, 0, 2, 0]  # 0 and 1 are the same person, 2 differs


def test_merge_pass_is_skipped_for_explicit_speaker_count(monkeypatch):
    calls = []
    monkeypatch.setattr(diarize, "merge_similar_speakers", lambda *a, **k: calls.append(1) or a[0])

    class FakeEngine:
        def process(self, samples, callback=None):
            class Result:
                def sort_by_start_time(self):
                    return [type("S", (), {"start": 0.0, "end": 2.0, "speaker": 0})(),
                            type("S", (), {"start": 2.0, "end": 4.0, "speaker": 1})()]
            return Result()

    d = diarize.SherpaDiarizer.__new__(diarize.SherpaDiarizer)
    d._default, d._build = FakeEngine(), lambda n: FakeEngine()
    monkeypatch.setattr(diarize, "read_wav", lambda path: [0.0])
    d.diarize(Path("x.wav"), num_speakers=2)
    assert calls == []
    d.diarize(Path("x.wav"))
    assert calls == [1]


# --- dash rule extras ---------------------------------------------------------------------------

def test_dash_carries_previous_speaker_across_null_sentences():
    sentences = [s(1, "A.", 0), s(2, "B.", None), s(3, "C.", 0), s(4, "D.", 1)]
    assert subtitles.speaker_dashes(sentences) == [True, False, False, True]


def test_dash_not_doubled_when_text_already_has_one():
    sentences = [s(1, "- Selam.", 0), s(2, "– Merhaba.", 1), s(3, "-Kısa.", 0)]
    srt = subtitles.render(sentences, "srt")
    assert "- - " not in srt and "- –" not in srt and "\n- Selam.\n" in srt
    assert "\n– Merhaba.\n" in srt and "\n- -Kısa.\n" in srt  # "-Kısa." has no space: not a dash


# --- model kind guards ---------------------------------------------------------------------------

def test_transcribe_rejects_non_transcription_catalog_models(tone_video, tmp_path, capsys, monkeypatch):
    def forbidden(*args, **kwargs):
        raise AssertionError("must fail before any download")

    monkeypatch.setattr(models, "ensure_model", forbidden)
    for model in ("speaker-diarization", "madlad-3b"):
        assert cli.main(["transcribe", str(tone_video), "-o", str(tmp_path), "-m", model, "--json"]) == 1
        event = json.loads(capsys.readouterr().out.splitlines()[-1])
        assert event["event"] == "error" and "konuşma tanıma modeli değil" in event["message"]


def test_translate_rejects_non_translation_models(capsys, monkeypatch):
    monkeypatch.setattr(sys, "stdin", io.StringIO("[]"))
    assert cli.main(["translate", "--to", "es", "-m", "speaker-diarization", "--json"]) == 1
    assert json.loads(capsys.readouterr().out.splitlines()[-1])["event"] == "error"


# --- owned files and partial deletes ------------------------------------------------------------

def owned_cache(tmp_path, shared=False):
    """Segmentation repo (owned file + a foreign file) and the embedding repo (owned + a foreign model)."""
    from test_models import make_cached_repo, make_shared_repo

    info = models.find(diarize.MODEL_ID)
    (emb_repo, (emb_file,)) = info.extra[0]
    if shared:
        seg_repo, store = make_shared_repo(tmp_path, info.repo, {
            info.files[0]: ("seg", 1 * 1024 * 1024), "model.onnx": ("seg-fp32", 4 * 1024 * 1024)})
        emb, _ = make_shared_repo(tmp_path, emb_repo, {
            emb_file: ("emb", 2 * 1024 * 1024), "other-model.onnx": ("other", 5 * 1024 * 1024)})
        return info, seg_repo, emb, store
    seg_repo = make_cached_repo(tmp_path, info.repo, {info.files[0]: 1024 * 1024, "model.onnx": 4 * 1024 * 1024})
    emb = make_cached_repo(tmp_path, emb_repo, {emb_file: 2 * 1024 * 1024, "other-model.onnx": 5 * 1024 * 1024})
    return info, seg_repo, emb, None


def test_disk_usage_counts_only_owned_files(tmp_path):
    info, *_ = owned_cache(tmp_path)
    sizes = models.cached_disk_mb(tmp_path)
    assert sizes[info.extra[0][0]] == 7  # whole repo
    assert sizes[f"{info.extra[0][0]}#{info.extra[0][1][0]}"] == 2
    assert sizes[f"{info.repo}#{info.files[0]}"] == 1


@pytest.mark.parametrize("shared", [False, True])
def test_delete_only_removes_owned_files_and_keeps_foreign_ones(tmp_path, shared):
    info, seg_repo, emb, store = owned_cache(tmp_path, shared)
    freed = models.delete_model(info.id, cache_dir=tmp_path)
    assert freed == 3
    snap = lambda repo: sorted(p.name for p in (repo / "snapshots").rglob("*") if not p.is_dir())  # noqa: E731
    assert snap(seg_repo) == ["model.onnx"] and snap(emb) == ["other-model.onnx"]
    assert (emb / "snapshots" / ("1" * 40) / "other-model.onnx").read_bytes() == b"x" * (5 * 1024 * 1024)
    if shared:
        left = [p.name for p in store.rglob("*") if p.is_file() and len(p.name) == 64]
        assert len(left) == 2  # only the two foreign blobs remain in the shared store


def test_delete_removes_repo_folder_when_nothing_else_is_left(tmp_path):
    from test_models import make_cached_repo

    info = models.find(diarize.MODEL_ID)
    emb_repo, (emb_file,) = info.extra[0]
    make_cached_repo(tmp_path, info.repo, {info.files[0]: 1024 * 1024})
    make_cached_repo(tmp_path, emb_repo, {emb_file: 2 * 1024 * 1024})
    assert models.delete_model(info.id, cache_dir=tmp_path) == 3
    assert not any(p.name.startswith("models--") for p in tmp_path.iterdir())


def test_partial_delete_reports_what_is_left_and_still_frees_the_rest(tmp_path, monkeypatch):
    info, seg_repo, emb, _ = owned_cache(tmp_path)
    real = models._delete_owned_files

    def flaky(root, repo, repo_id, names):
        if repo_id == info.repo:
            raise PermissionError("izin yok")
        return real(root, repo, repo_id, names)

    monkeypatch.setattr(models, "_delete_owned_files", flaky)
    with pytest.raises(models.ModelError, match=r"kısmen silindi \(2 MB boşaldı\); kalan: .*izin yok"):
        models.delete_model(info.id, cache_dir=tmp_path)
    assert not (emb / "snapshots" / ("1" * 40) / info.extra[0][1][0]).exists()  # the other repo was still cleaned
    assert (seg_repo / "snapshots" / ("1" * 40) / info.files[0]).exists()


def test_fallback_download_sizes_use_real_part_sizes():
    info = models.find(diarize.MODEL_ID)
    assert [models._fallback_bytes(info, i) for i in range(2)] == [1_540_506, 28_281_164]
