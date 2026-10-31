import json

from subtitle_engine.models import Sentence, Word
from subtitle_engine.subtitles import render

SAMPLE = [
    Sentence(1, 0, 1.5, "Merhaba.", [Word(0, 1.5, "Merhaba.")]),
    Sentence(2, 3661.007, 3662.5, "Görüşürüz."),
]


def test_srt():
    assert render(SAMPLE, "srt") == (
        "1\n00:00:00,000 --> 00:00:01,500\nMerhaba.\n\n"
        "2\n01:01:01,007 --> 01:01:02,500\nGörüşürüz.\n"
    )


def test_vtt():
    assert render(SAMPLE, "vtt").startswith("WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nMerhaba.\n")


def test_json_roundtrip_keeps_unicode():
    text = render(SAMPLE, "json")
    assert "Görüşürüz." in text
    assert [Sentence.from_dict(d) for d in json.loads(text)] == SAMPLE
