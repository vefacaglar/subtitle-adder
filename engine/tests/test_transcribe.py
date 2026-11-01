"""Tests for building words from tokens and for onnx-asr backend selection: no network."""
import sys
import types
import wave
from pathlib import Path

import pytest

from subtitle_engine.transcribe import (
    OnnxAsrTranscriber,
    VAD_OPTIONS,
    create_transcriber,
    words_from_tokens,
)


def test_words_from_subword_tokens_and_trailing_punctuation():
    words = words_from_tokens(
        ["▁Mer", "ha", "ba", "▁dün", "ya", "."],
        [0.0, 0.2, 0.3, 0.6, 0.8, 1.0],
        segment_start=2.0,
        segment_end=3.0,
    )

    assert [w.text for w in words] == ["Merhaba", "dünya."]
    assert (words[0].start, words[0].end) == (2.0, 2.6)  # start of the next word
    assert (words[1].start, words[1].end) == (2.6, 3.0)  # segment end


def test_words_handle_space_markers_and_opening_punctuation():
    words = words_from_tokens(
        [" H", "ola", ",", " ¿", "qué", " tal", "?"],
        [0.0, 0.1, 0.3, 0.4, 0.5, 0.7, 0.9],
        segment_start=0.0,
        segment_end=1.0,
    )

    assert [w.text for w in words] == ["Hola,", "¿qué", "tal?"]


def test_words_standalone_marker_starts_new_word():
    words = words_from_tokens(
        ["▁гу", "лять", " ", "гу", "лять"],
        [0.0, 0.2, 0.4, 0.5, 0.8],
        segment_start=0.0,
        segment_end=1.0,
    )

    assert [w.text for w in words] == ["гулять", "гулять"]
    assert [w.start for w in words] == sorted(w.start for w in words)
    assert words[1].start == 0.5


def test_words_filter_special_tokens():
    words = words_from_tokens(
        ["<|en|>", "▁merhaba", "<blk>", "▁dünya"],
        [0.0, 0.1, 0.2, 0.3],
        segment_start=0.0,
        segment_end=1.0,
    )

    assert [w.text for w in words] == ["merhaba", "dünya"]
    assert words[0].start == 0.1


def test_words_estimate_timestamps_when_backend_has_none():
    words = words_from_tokens(
        [" O", "lá", ",", " tudo", " bem", "?"],
        None,
        segment_start=1.0,
        segment_end=3.0,
    )

    assert [w.text for w in words] == ["Olá,", "tudo", "bem?"]
    assert words[0].start == 1.0 and words[-1].end == 3.0
    assert all(a.end <= b.start for a, b in zip(words, words[1:]))
    assert all(w.end <= 3.0 for w in words)


class FakeSegment:
    def __init__(self, start, end, tokens, timestamps):
        self.start = start
        self.end = end
        self.tokens = tokens
        self.timestamps = timestamps


class FakeAdapter:
    def __init__(self, segments):
        self.segments = segments
        self.vad_args = None
        self.kwargs = None

    def with_vad(self, vad, **kwargs):
        self.vad_args = (vad, kwargs)
        return self

    def with_timestamps(self):
        return self

    def recognize(self, audio, **kwargs):
        self.kwargs = kwargs
        return iter(self.segments)


@pytest.fixture
def wav_file(tmp_path: Path) -> Path:
    path = tmp_path / "audio.wav"
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(16_000)
        wav.writeframes(b"\0\0" * 32_000)  # 2 s
    return path


def install_fake_onnx_asr(monkeypatch, adapter):
    loaded: dict[str, tuple] = {}

    def load_model(name, quantization=None, providers=None):
        loaded["model"] = (name, quantization)
        loaded["model_providers"] = tuple(providers) if providers else None
        return adapter

    def load_vad(name, providers=None):
        loaded["vad"] = (name,)
        loaded["vad_providers"] = tuple(providers) if providers else None
        return object()

    module = types.SimpleNamespace(load_model=load_model, load_vad=load_vad)
    monkeypatch.setitem(sys.modules, "onnx_asr", module)
    return loaded


def test_onnx_transcriber_uses_vad_timestamps_and_absolute_words(wav_file, monkeypatch):
    adapter = FakeAdapter([
        FakeSegment(0.0, 1.0, ["▁Mer", "ha", "ba"], [0.0, 0.1, 0.2]),
        FakeSegment(1.2, 2.0, ["▁dün", "ya", "."], [0.0, 0.2, 0.4]),
    ])
    loaded = install_fake_onnx_asr(monkeypatch, adapter)
    progress: list[float] = []

    transcript = OnnxAsrTranscriber("nemo-canary-1b-v2", quantization="int8").transcribe(
        wav_file, language="pt", on_progress=progress.append
    )

    assert loaded == {
        "model": ("nemo-canary-1b-v2", "int8"),
        "model_providers": ("CPUExecutionProvider",),
        "vad": ("silero",),
        "vad_providers": ("CPUExecutionProvider",),
    }
    assert adapter.vad_args[1] == VAD_OPTIONS
    assert adapter.kwargs == {"language": "pt", "target_language": "pt"}  # Canary also needs the target language
    assert transcript.language == "pt" and transcript.duration == 2.0
    assert [w.text for w in transcript.words] == ["Merhaba", "dünya."]
    assert (transcript.words[1].start, transcript.words[1].end) == (1.2, 2.0)
    assert progress == [0.5, 1.0]


def test_onnx_single_language_model_reports_russian(wav_file, monkeypatch):
    adapter = FakeAdapter([FakeSegment(0.0, 2.0, ["▁При", "вет"], [0.0, 0.2])])
    install_fake_onnx_asr(monkeypatch, adapter)

    transcript = OnnxAsrTranscriber("gigaam-v3-e2e-rnnt").transcribe(wav_file)

    assert transcript.language == "ru"
    assert adapter.kwargs == {"language": "ru"}


def test_onnx_multilingual_model_without_language_reports_unknown(wav_file, monkeypatch):
    adapter = FakeAdapter([FakeSegment(0.0, 2.0, ["▁Hal", "lo"], [0.0, 0.2])])
    install_fake_onnx_asr(monkeypatch, adapter)

    transcript = OnnxAsrTranscriber("nemo-parakeet-tdt-0.6b-v3").transcribe(wav_file)

    assert transcript.language == "?"
    assert adapter.kwargs == {}


def test_create_transcriber_selects_backend(monkeypatch):
    calls: list[tuple] = []

    def fake_whisper(model_id):
        calls.append(("whisper", model_id))
        return "whisper"

    def fake_onnx(model_name, quantization):
        calls.append(("onnx", model_name, quantization))
        return "onnx"

    monkeypatch.setattr("subtitle_engine.transcribe.FasterWhisperTranscriber", fake_whisper)
    monkeypatch.setattr("subtitle_engine.transcribe.OnnxAsrTranscriber", fake_onnx)

    assert create_transcriber("small") == "whisper"
    assert create_transcriber("turbo") == "whisper"  # outside the catalog: faster-whisper
    assert create_transcriber("gigaam-v3") == "onnx"

    assert calls == [
        ("whisper", "small"),
        ("whisper", "turbo"),
        ("onnx", "gigaam-v3-e2e-rnnt", "int8"),
    ]
