"""Adım 4: cümleleri SRT / VTT / JSON olarak yazar."""
import json
from pathlib import Path

from .models import Sentence

FORMATS = ("srt", "vtt", "json")


def timestamp(seconds: float, sep: str) -> str:
    ms = round(seconds * 1000)
    h, ms = divmod(ms, 3_600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02}:{m:02}:{s:02}{sep}{ms:03}"


def render(sentences: list[Sentence], fmt: str) -> str:
    if fmt == "srt":
        return "\n".join(
            f"{i}\n{timestamp(s.start, ',')} --> {timestamp(s.end, ',')}\n{s.text}\n"
            for i, s in enumerate(sentences, 1)
        )
    if fmt == "vtt":
        return "WEBVTT\n\n" + "\n".join(
            f"{timestamp(s.start, '.')} --> {timestamp(s.end, '.')}\n{s.text}\n" for s in sentences
        )
    if fmt == "json":
        return json.dumps([s.to_dict() for s in sentences], ensure_ascii=False, indent=2)
    raise ValueError(f"Bilinmeyen format: {fmt}")


def write(sentences: list[Sentence], path: Path, fmt: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(render(sentences, fmt), encoding="utf-8")
    return path
