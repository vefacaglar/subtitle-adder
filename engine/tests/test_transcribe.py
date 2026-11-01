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


def test_whisper_falls_back_to_cpu_when_cuda_libraries_fail(wav_file, monkeypatch):
    """A GPU without the CUDA runtime libraries fails on the first batch; the run restarts on the CPU."""
    import types

    from subtitle_engine import cuda
    from subtitle_engine.transcribe import FasterWhisperTranscriber

    loads = []

    class FakeWhisperModel:
        def __init__(self, size, device, compute_type, **kwargs):
            loads.append((device, compute_type))
            self.device = device

        def transcribe(self, *args, **kwargs):
            if self.device == "cuda":
                raise RuntimeError("Library cublas64_12.dll is not found or cannot be loaded")
            word = types.SimpleNamespace(start=0.0, end=0.5, word=" Hello")
            segment = types.SimpleNamespace(end=0.5, words=[word])
            return iter([segment]), types.SimpleNamespace(language="en", duration=1.0)

    monkeypatch.setitem(sys.modules, "faster_whisper", types.SimpleNamespace(WhisperModel=FakeWhisperModel))
    monkeypatch.setattr(cuda, "cuda_usable", lambda: True)

    transcript = FasterWhisperTranscriber("base").transcribe(wav_file)

    assert loads == [("cuda", "float16"), ("cpu", "int8")]
    assert [w.text for w in transcript.words] == ["Hello"]


def test_cpu_fallback_after_a_mid_run_cuda_error_keeps_progress_monotonic(wav_file, monkeypatch):
    """A CUDA error after some segments restarts on the CPU without the progress jumping back to 0."""
    import types

    from subtitle_engine import cuda
    from subtitle_engine.transcribe import FasterWhisperTranscriber

    def segment(end):
        return types.SimpleNamespace(end=end, words=[types.SimpleNamespace(start=end - 0.1, end=end, word=" w")])

    class FakeWhisperModel:
        def __init__(self, size, device, compute_type, **kwargs):
            self.device = device

        def transcribe(self, *args, **kwargs):
            def segments():
                yield segment(2.0)
                yield segment(6.0)
                if self.device == "cuda":
                    raise RuntimeError("CUDA failed with error out of memory")
                yield segment(10.0)

            return segments(), types.SimpleNamespace(language="en", duration=10.0)

    monkeypatch.setitem(sys.modules, "faster_whisper", types.SimpleNamespace(WhisperModel=FakeWhisperModel))
    monkeypatch.setattr(cuda, "cuda_usable", lambda: True)

    progress: list[float] = []
    transcript = FasterWhisperTranscriber("base").transcribe(wav_file, on_progress=progress.append)

    assert progress == [0.2, 0.6, 0.6, 1.0]
    assert len(transcript.words) == 3


def test_whisper_uses_cpu_when_cuda_is_not_usable(monkeypatch):
    import types

    from subtitle_engine import cuda
    from subtitle_engine.transcribe import FasterWhisperTranscriber

    loads = []
    fake = types.SimpleNamespace(WhisperModel=lambda size, device, compute_type, **kw: loads.append((device, compute_type)))
    monkeypatch.setitem(sys.modules, "faster_whisper", fake)
    monkeypatch.setattr(cuda, "cuda_usable", lambda: False)

    FasterWhisperTranscriber("base")

    assert loads == [("cpu", "int8")]


def test_unrelated_runtime_errors_are_not_treated_as_cuda_failures():
    from subtitle_engine.cuda import is_cuda_library_error

    assert is_cuda_library_error(RuntimeError("Library cudnn_ops64_9.dll is not found"))
    assert not is_cuda_library_error(RuntimeError("Invalid input features shape"))


def test_cached_whisper_model_loads_without_contacting_the_hub(monkeypatch):
    import types

    from subtitle_engine import cuda, models
    from subtitle_engine.transcribe import FasterWhisperTranscriber

    calls = []
    fake = types.SimpleNamespace(WhisperModel=lambda size, **kw: calls.append(kw["local_files_only"]))
    monkeypatch.setitem(sys.modules, "faster_whisper", fake)
    monkeypatch.setattr(cuda, "cuda_usable", lambda: False)

    monkeypatch.setattr(models, "is_downloaded", lambda model_id: True)
    FasterWhisperTranscriber("base")
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: False)
    FasterWhisperTranscriber("base")

    assert calls == [True, False]



def test_whisper_passes_fast_dialogue_vad_and_decoding_options(wav_file, monkeypatch):
    import types

    from subtitle_engine import cuda
    from subtitle_engine.transcribe import FasterWhisperTranscriber, WHISPER_VAD_OPTIONS

    captured: dict = {}

    class FakeWhisperModel:
        def __init__(self, size, **kwargs):
            pass

        def transcribe(self, audio, **kwargs):
            captured.update(kwargs)
            return iter([]), types.SimpleNamespace(language="pt", duration=1.0)

    monkeypatch.setitem(sys.modules, "faster_whisper", types.SimpleNamespace(WhisperModel=FakeWhisperModel))
    monkeypatch.setattr(cuda, "cuda_usable", lambda: False)

    FasterWhisperTranscriber("large-v3-turbo").transcribe(wav_file, language="pt")

    assert captured["vad_parameters"] == WHISPER_VAD_OPTIONS
    assert captured["vad_parameters"]["min_silence_duration_ms"] == 250
    assert captured["vad_parameters"]["max_speech_duration_s"] == 14.0
    assert captured["condition_on_previous_text"] is False
    assert captured["no_speech_threshold"] == 0.8
    assert captured["log_prob_threshold"] == -1.2


def test_whisper_streams_chunks_and_emits_partial_words_on_long_wav(tmp_path, monkeypatch):
    import types
    import wave

    from subtitle_engine import cuda
    from subtitle_engine import transcribe as transcribe_mod
    from subtitle_engine.transcribe import FasterWhisperTranscriber

    monkeypatch.setattr(transcribe_mod, "TRANSCRIBE_CHUNK_SECONDS", 1.0)
    monkeypatch.setattr(transcribe_mod, "CHUNK_SEARCH_TAIL_SECONDS", 0.2)

    long_wav = tmp_path / "long.wav"
    with wave.open(str(long_wav), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(16_000)
        wav.writeframes(b"\x00\x00" * (16_000 * 3))  # 3 seconds -> 3 chunks of ~1s

    chunk_calls: list[int] = []

    class FakeWhisperModel:
        def __init__(self, size, **kwargs):
            pass

        def transcribe(self, audio_input, **kwargs):
            idx = len(chunk_calls) + 1
            chunk_calls.append(idx)
            word = types.SimpleNamespace(start=0.1, end=0.4, word=f" chunk{idx}")
            seg = types.SimpleNamespace(end=0.4, words=[word])
            return iter([seg]), types.SimpleNamespace(language="pt", duration=1.0)

    monkeypatch.setitem(sys.modules, "faster_whisper", types.SimpleNamespace(WhisperModel=FakeWhisperModel))
    monkeypatch.setattr(cuda, "cuda_usable", lambda: False)

    partial_batches: list[list[str]] = []
    progress: list[float] = []
    transcript = FasterWhisperTranscriber("base").transcribe(
        long_wav,
        on_progress=progress.append,
        on_words=lambda words, lang: partial_batches.append([w.text for w in words]),
    )

    assert len(chunk_calls) >= 3
    assert [w.text for w in transcript.words] == [f"chunk{i}" for i in range(1, len(chunk_calls) + 1)]
    assert transcript.words[1].start > transcript.words[0].end
    assert partial_batches[0] == ["chunk1"]
    assert partial_batches[-1] == [f"chunk{i}" for i in range(1, len(chunk_calls) + 1)]
    assert progress[-1] == 1.0
