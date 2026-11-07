"""Generates golden_segment.json: random word streams segmented and rendered by the Python engine.

Run with the reference Python engine (CPython 3.11):

    engine/.venv/Scripts/python.exe -I engine-rs/crates/se-text/tests/fixtures/gen_golden.py
"""
import json
import random
from pathlib import Path

from subtitle_engine.models import Word
from subtitle_engine.sentences import SegmenterConfig, segment
from subtitle_engine.subtitles import render

OUT = Path(__file__).resolve().parent / "golden_segment.json"

POOL = [
    "bir", "Ahmet", "geldi,", "tamam.", "Evet?", '"Evet."', "(gitti!)", "c", "'est", "-ci", "c'", "x-", " y ",
    "T", "Je", "suis", "İyi", "ıslak", "…", "a;", "b:", "x" * 50, "Ünal", "çok", "Şimdi", "«Bak»", "“Ne?”",
    "Evet. ", " ", "\u001cok\u001f", "ǅab", "ःa", "中文", "Ⅻ", "-", "'", "’s", "Dur!", "kalktım,\"",
    "ve", "sonra", "Ece", "ile", "eve", "döndük", "ama", "Bu", "o", "mı?", "sen", "Peki",
]


def main() -> None:
    rng = random.Random(20261107)
    cases = []
    for n in range(250):
        t, words = 0.0, []
        int_times = rng.random() < 0.1
        for _ in range(rng.randrange(0, 36)):
            t += rng.choice([0.0, 0.1, 0.3, 1.2, 2.0, 4.2, 4.3, 6.5, 7.0])
            d = round(rng.uniform(0.05, 0.9), 3)
            if int_times:
                words.append(Word(int(t), int(t) + 1, rng.choice(POOL)))
            else:
                words.append(Word(round(t, 3), round(t + d, 3), rng.choice(POOL)))
            t += d
        speakers = [rng.choice([0, 1, 2, None]) for _ in words] if rng.random() < 0.5 else None
        config = SegmenterConfig(rng.choice([1.2, 0.5, 3.0]), rng.choice([10, 40, 84, -5]), rng.choice([2.0, 6.0, 100.0]))
        out = segment(words, config, speakers)
        cases.append({
            "words": [[w.start, w.end, w.text] for w in words],
            "speakers": speakers,
            "config": [config.pause_gap, config.max_chars, config.max_duration],
            "srt": render(out, "srt"),
            "vtt": render(out, "vtt") if n < 60 else None,
            "sentences": json.dumps([s.to_dict() for s in out], ensure_ascii=False),
            "json": render(out, "json") if n < 60 else None,
        })
    OUT.write_text(json.dumps(cases, ensure_ascii=False), encoding="utf-8", newline="\n")
    print(OUT, OUT.stat().st_size)


if __name__ == "__main__":
    main()
