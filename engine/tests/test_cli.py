import io
import json
import sys
from pathlib import Path

import pytest
from conftest import make_words

from subtitle_engine import cli, models
from subtitle_engine.models import Transcript


class FakeTranscriber:
    def transcribe(self, audio, language=None, on_progress=None):
        on_progress and on_progress(0.5)
        return Transcript(language="tr", duration=2.0, words=make_words("Merhaba dünya. Görüşürüz."))


def run_cli(argv, capsys, monkeypatch):
    monkeypatch.setattr("subtitle_engine.transcribe.FasterWhisperTranscriber", lambda model: FakeTranscriber())
    monkeypatch.setattr("subtitle_engine.models.needs_download", lambda model_id: False)  # ağa çıkma
    code = cli.main(argv)
    return code, [json.loads(line) for line in capsys.readouterr().out.splitlines()]


def test_transcribe_json_event_stream(tone_video, tmp_path, capsys, monkeypatch):
    code, events = run_cli(["transcribe", str(tone_video), "-o", str(tmp_path), "-f", "srt", "vtt", "--json"], capsys, monkeypatch)
    assert code == 0
    stages = [e["stage"] for e in events if e["event"] == "stage"]
    assert stages[0] == "extracting_audio" and "transcribing" in stages and stages[-1] == "segmenting"
    result = events[-1]
    assert result["event"] == "result" and result["language"] == "tr"
    assert [s["text"] for s in result["sentences"]] == ["Merhaba dünya.", "Görüşürüz."]
    assert all(Path(p).is_file() for p in result["outputs"])


def test_errors_become_events(tmp_path, capsys, monkeypatch):
    code, events = run_cli(["transcribe", str(tmp_path / "yok.mp4"), "-o", str(tmp_path), "--json"], capsys, monkeypatch)
    assert code == 1
    assert events == [{"event": "stage", "stage": "extracting_audio", "fraction": None},
                      {"event": "error", "message": events[-1]["message"]}]


def test_render_from_stdin(capsys, monkeypatch):
    payload = [{"id": 1, "start": 0, "end": 1, "text": "Düzenlendi.", "words": []}]
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(payload)))
    assert cli.main(["render", "-f", "srt"]) == 0
    assert capsys.readouterr().out == "1\n00:00:00,000 --> 00:00:01,000\nDüzenlendi.\n"


def test_models_json_lists_catalog_and_cache_state(capsys, monkeypatch):
    monkeypatch.setattr(cli, "is_downloaded", lambda model_id: model_id in {"base", "small"})
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: model_id in {"base", "small"})
    monkeypatch.setattr(
        cli,
        "cached_disk_mb",
        lambda: {"Systran/faster-whisper-base": 141, "Systran/faster-whisper-small": 464},
    )

    assert cli.main(["models", "--json"]) == 0
    lines = capsys.readouterr().out.splitlines()
    assert len(lines) == 1
    event = json.loads(lines[0])
    assert event["event"] == "models"
    assert [m["id"] for m in event["models"]] == [
        "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3",
        "nllb-1.3b", "madlad-3b", "speaker-diarization",
    ]
    assert set(event["models"][0]) == {
        "id", "kind", "backend", "languages", "size_mb", "disk_mb", "speed", "accuracy", "license", "downloaded",
    }
    assert event["models"][1]["backend"] == "onnx" and event["models"][1]["languages"] == ["ru"]
    assert event["models"][0]["languages"] is None
    assert event["models"][0]["kind"] == "transcription" and event["models"][0]["license"] == "MIT"
    assert event["models"][-3]["kind"] == "translation"
    assert event["models"][-3]["license"] == "CC-BY-NC-4.0"
    assert event["models"][-2]["license"] == "Apache-2.0"
    assert event["models"][-1]["kind"] == "diarization" and event["models"][-1]["backend"] == "sherpa"
    assert event["models"][-1]["license"] == "MIT AND Apache-2.0"
    assert [m["downloaded"] for m in event["models"]] == [True, False, True] + [False] * 8
    assert [m["disk_mb"] for m in event["models"]] == [141, None, 464] + [None] * 8


def test_models_table_without_json(capsys, monkeypatch):
    monkeypatch.setattr(cli, "is_downloaded", lambda model_id: model_id == "base")
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: model_id == "base")
    monkeypatch.setattr(cli, "cached_disk_mb", lambda: {"Systran/faster-whisper-base": 141})

    assert cli.main(["models"]) == 0
    out = capsys.readouterr().out
    assert out.splitlines()[0].startswith("model") and "disk" in out.splitlines()[0]
    assert "base" in out and "141 MB" in out and "—" in out
    assert "var" in out and "yok" in out


def test_delete_model_json_event(capsys, monkeypatch):
    calls: list[str] = []

    def fake_delete(model_id):
        calls.append(model_id)
        return 123

    monkeypatch.setattr(cli, "delete_model", fake_delete)

    assert cli.main(["delete-model", "base", "--json"]) == 0
    assert calls == ["base"]
    assert capsys.readouterr().out == '{"event": "deleted", "id": "base", "freed_mb": 123}\n'


def test_delete_model_error_becomes_event(capsys, monkeypatch):
    from subtitle_engine.models import ModelError

    def fail(model_id):
        raise ModelError(f"{model_id} modeli indirilmemiş; silinecek dosya yok.")

    monkeypatch.setattr(cli, "delete_model", fail)

    assert cli.main(["delete-model", "base", "--json"]) == 1
    assert json.loads(capsys.readouterr().out) == {
        "event": "error",
        "message": "base modeli indirilmemiş; silinecek dosya yok.",
    }


def test_delete_model_human_output(capsys, monkeypatch):
    monkeypatch.setattr(cli, "delete_model", lambda model_id: 123)

    assert cli.main(["delete-model", "base"]) == 0
    assert capsys.readouterr().out == "base silindi, 123 MB boşaldı\n"


def test_downloading_model_stage_precedes_loading(tone_video, tmp_path, capsys, monkeypatch):
    from subtitle_engine import models

    reported: list[float] = []

    def fake_ensure(model_id, on_progress=None):
        assert model_id == "medium"
        for fraction in (0.25, 0.5, 0.99, 1.0):
            reported.append(fraction)
            on_progress(fraction)
        return "/cache/medium"

    monkeypatch.setattr("subtitle_engine.transcribe.FasterWhisperTranscriber", lambda model: FakeTranscriber())
    monkeypatch.setattr(models, "needs_download", lambda model_id: model_id == "medium")
    monkeypatch.setattr(models, "ensure_model", fake_ensure)

    code = cli.main(["transcribe", str(tone_video), "-o", str(tmp_path), "-m", "medium", "--json"])
    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]

    assert code == 0
    stages = [(e["stage"], e["fraction"]) for e in events if e["event"] == "stage"]
    download = [fraction for stage, fraction in stages if stage == "downloading_model"]
    assert download[0] == 0.0 and download[-1] == 1.0
    assert download == sorted(download) and all(0.0 <= f <= 1.0 for f in download)
    assert reported == [0.25, 0.5, 0.99, 1.0]  # ilerleme aynen protokole yansır
    assert stages.index(("downloading_model", 0.0)) < stages.index(("loading_model", None))
    assert events[-1]["event"] == "result"


def test_onnx_catalog_model_uses_onnx_transcriber(tone_video, tmp_path, capsys, monkeypatch):
    from subtitle_engine import models

    calls: list[tuple] = []

    def fake_onnx(name, quantization):
        calls.append((name, quantization))
        return FakeTranscriber()

    monkeypatch.setattr("subtitle_engine.transcribe.OnnxAsrTranscriber", fake_onnx)
    monkeypatch.setattr(models, "needs_download", lambda model_id: False)

    code = cli.main(["transcribe", str(tone_video), "-m", "parakeet-v3", "-l", "pt", "-o", str(tmp_path), "--json"])
    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]

    assert code == 0 and events[-1]["event"] == "result"
    assert calls == [("nemo-parakeet-tdt-0.6b-v3", "int8")]


def test_language_mismatch_is_turkish_error(tmp_path, capsys):
    code = cli.main(["transcribe", str(tmp_path / "yok.mp4"), "-m", "gigaam-v3", "-l", "tr", "--json"])
    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]

    assert code == 1
    assert events == [{"event": "error", "message": "GigaAM yalnız Rusça destekler."}]


def test_cached_model_skips_downloading_stage(tone_video, tmp_path, capsys, monkeypatch):
    from subtitle_engine import models

    monkeypatch.setattr("subtitle_engine.transcribe.FasterWhisperTranscriber", lambda model: FakeTranscriber())
    monkeypatch.setattr(models, "needs_download", lambda model_id: False)
    monkeypatch.setattr(models, "ensure_model", lambda *a, **kw: pytest.fail("indirilmemeli"))

    code = cli.main(["transcribe", str(tone_video), "-o", str(tmp_path), "--json"])
    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]

    assert code == 0
    assert "downloading_model" not in [e["stage"] for e in events if e["event"] == "stage"]


@pytest.mark.slow
def test_real_stt_on_sample(tmp_path):
    sample = Path(__file__).parents[2] / "samples" / "demo.mp4"
    if not sample.exists():
        pytest.skip("samples/demo.mp4 yok (CLAUDE.md'deki komutla üret)")
    assert cli.main(["transcribe", str(sample), "-m", "base", "-l", "tr", "-o", str(tmp_path)]) == 0


def test_render_utf8_pipes_regardless_of_locale_encoding():
    """Windows'taki gibi ANSI kod sayfası (cp1254) ayarlıyken de UTF-8 okunur/yazılır."""
    import os
    import subprocess

    payload = [{"id": 1, "start": 0.0, "end": 1.0, "text": "こんにちは dünya İş", "words": []}]
    env = {**os.environ, "PYTHONIOENCODING": "cp1254", "PYTHONUTF8": "0"}
    proc = subprocess.run(
        [sys.executable, "-m", "subtitle_engine.cli", "render"],
        input=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
        capture_output=True,
        env=env,
    )
    assert proc.returncode == 0, proc.stderr.decode("utf-8", "replace")
    assert "こんにちは dünya İş" in proc.stdout.decode("utf-8")
