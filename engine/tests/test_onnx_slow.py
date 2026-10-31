"""Gerçek onnx-asr modelleriyle uçtan uca testler (pytest -m slow)."""
from pathlib import Path

import pytest

from subtitle_engine import pipeline
from subtitle_engine.transcribe import create_transcriber

SAMPLES = Path(__file__).parents[2] / "samples"

CASES = [
    ("demo-pt.mp4", "parakeet-v3", "pt"),
    ("demo-es.mp4", "parakeet-v3", "es"),
    ("demo-ru.mp4", "parakeet-v3", "ru"),
    ("demo-ru.mp4", "gigaam-v3", "ru"),
    ("demo-pt.mp4", "canary-1b-v2", "pt"),
]


@pytest.mark.slow
@pytest.mark.parametrize(("sample", "model", "language"), CASES)
def test_onnx_model_transcribes_sample(tmp_path, sample, model, language):
    video = SAMPLES / sample
    if not video.exists():
        pytest.skip(f"samples/{sample} yok (örnek videolar üretilmemiş)")

    result = pipeline.run(
        video,
        lambda: create_transcriber(model),
        tmp_path,
        language=language,
        model=model,
    )

    assert result.language == language
    assert len(result.sentences) > 1
    words = [word for sentence in result.sentences for word in sentence.words]
    assert len(words) > 5
    assert all(0.0 <= word.start <= word.end <= result.duration + 0.01 for word in words)
    assert all(a.start <= b.start for a, b in zip(words, words[1:]))
    assert (tmp_path / f"{video.stem}.srt").is_file()
