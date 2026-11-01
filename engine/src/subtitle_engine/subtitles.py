"""Step 4: writes sentences as SRT / VTT / JSON."""
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


def speaker_dashes(sentences: list[Sentence]) -> list[bool]:
    """Which sentences get a leading "- ": with at least two distinct speakers, every sentence whose speaker
    differs from the previous one (the first included). None with one speaker or no speaker info."""
    if len({s.speaker for s in sentences if s.speaker is not None}) < 2:
        return [False] * len(sentences)
    dashes: list[bool] = []
    previous: int | None = None  # last known speaker; sentences without one do not reset it
    for s in sentences:
        dashes.append(s.speaker is not None and s.speaker != previous)
        if s.speaker is not None:
            previous = s.speaker
    return dashes


def _texts(sentences: list[Sentence]) -> list[str]:
    return [
        "- " + s.text if dash and not _has_dash(s.text) else s.text
        for s, dash in zip(sentences, speaker_dashes(sentences))
    ]


def _has_dash(text: str) -> bool:
    """Does the (possibly user-edited) text already start with a dialogue dash?"""
    return text.startswith(("- ", "\u2013 "))


def render(sentences: list[Sentence], fmt: str) -> str:
    if fmt == "srt":
        return "\n".join(
            f"{i}\n{timestamp(s.start, ',')} --> {timestamp(s.end, ',')}\n{text}\n"
            for i, (s, text) in enumerate(zip(sentences, _texts(sentences)), 1)
        )
    if fmt == "vtt":
        return "WEBVTT\n\n" + "\n".join(
            f"{timestamp(s.start, '.')} --> {timestamp(s.end, '.')}\n{text}\n"
            for s, text in zip(sentences, _texts(sentences))
        )
    if fmt == "json":
        return json.dumps([s.to_dict() for s in sentences], ensure_ascii=False, indent=2)
    raise ValueError(f"Bilinmeyen format: {fmt}")


def write(sentences: list[Sentence], path: Path, fmt: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(render(sentences, fmt), encoding="utf-8")
    return path
