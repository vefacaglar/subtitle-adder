import io
import json
import sys
from pathlib import Path

import pytest
from conftest import make_words

from subtitle_engine import cli
from subtitle_engine.models import Transcript


class FakeTranscriber:
    def transcribe(self, audio, language=None, on_progress=None):
        on_progress and on_progress(0.5)
        return Transcript(language="tr", duration=2.0, words=make_words("Merhaba dünya. Görüşürüz."))


def run_cli(argv, capsys, monkeypatch):
    monkeypatch.setattr("subtitle_engine.transcribe.FasterWhisperTranscriber", lambda model: FakeTranscriber())
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
