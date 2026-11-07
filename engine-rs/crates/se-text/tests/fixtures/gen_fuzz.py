"""Generates golden_fuzz.json: a second, independent random corpus segmented by the Python engine, aimed at
the boundaries (pause/gap thresholds, overlapping and unsorted times, long unpunctuated runs for `fit`,
realignment starters, tiny or negative limits, non-finite times).

Run with the reference Python engine (CPython 3.11):

    engine/.venv/Scripts/python.exe -I engine-rs/crates/se-text/tests/fixtures/gen_fuzz.py
"""
import json
import math
import random
from pathlib import Path

from subtitle_engine.models import Word
from subtitle_engine.sentences import SegmenterConfig, segment
from subtitle_engine.subtitles import render

OUT = Path(__file__).resolve().parent / "golden_fuzz.json"

POOL = [
    "Je", "Tu", "T'es", "es", "suis", "penses", "ne", "dois", "ridicule", "en", "c'", "est", "'est", "-ci",
    "celui", "qu'", "il", "Il", "Elle", "mais,", "donc;", "voilà:", "Oui.", "Non!", "Quoi?", "Bon…", "»", "«Bon",
    "(Ah)", "[rire]", "x" * 30, "Ünal", "İstanbul", "ılık", "Çok", "şey,", "Ve", "ve", "Ama", "ama", "bu", "Bu",
    "gerçekten", "uzun", "bir", "cümle", "ki", "hiç", "bitmiyor", "123", "4,", "'", "-", "’", "’s", "l’", "homme",
    " ", "\t", " Bak ", "ǅungla", "ⅰx", "Σ", "ς", "中文。", "日本", "–", "- Evet", "a.", "B.", "\"Evet.\"", "ok\"",
]
GAPS = [-0.5, 0.0, 0.05, 0.3, 0.99, 1.0, 1.01, 1.19, 1.2, 1.21, 2.5, 4.19, 4.2, 4.21, 5.99, 6.0, 6.01, 10.0]
DURS = [0.0, 0.1, 0.25, 0.5, 0.9, 1.5, -0.2]


def main() -> None:
    rng = random.Random(77031)
    cases = []
    for n in range(350):
        t, words = rng.choice([0.0, 0.0, 3599.5, -2.0]), []
        long_run = rng.random() < 0.3
        pool = [w for w in POOL if not w.rstrip("\"')]»”’").endswith((".", "!", "?", "…"))] if long_run else POOL
        for _ in range(rng.randrange(0, 91 if long_run else 40)):
            t += rng.choice(GAPS) if rng.random() < 0.6 else round(rng.uniform(0, 0.4), 3)
            d = rng.choice(DURS)
            words.append(Word(t, t + d, rng.choice(pool)))
            t += max(d, 0.0)
        if words and rng.random() < 0.03:
            i = rng.randrange(len(words))
            words[i] = Word(rng.choice([math.nan, math.inf]), words[i].end, words[i].text)
        speakers = [rng.choice([0, 1, None]) for _ in words] if rng.random() < 0.3 else None
        if speakers and rng.random() < 0.5:  # long same-speaker runs
            speakers = [speakers[0] if rng.random() < 0.9 else 1 for _ in words]
        config = SegmenterConfig(
            rng.choice([1.2, 1.2, 0.0, 1.0, 2.0]),
            rng.choice([0, 1, 5, 20, 42, 84, 84, 200]),
            rng.choice([0.0, 1.0, 3.0, 6.0, 6.0, 1e9, -1.0]),
        )
        out = segment(words, config, speakers)
        try:
            srt = render(out, "srt")
        except (ValueError, OverflowError) as exc:
            srt = "ERROR: " + str(exc)
        cases.append({
            "words": [[w.start, w.end, w.text] for w in words],
            "speakers": speakers,
            "config": [config.pause_gap, config.max_chars, config.max_duration],
            "srt": srt,
            "sentences": json.dumps([s.to_dict() for s in out], ensure_ascii=False),
        })
    OUT.write_text(json.dumps(cases, ensure_ascii=False), encoding="utf-8", newline="\n")
    print(OUT, OUT.stat().st_size)


if __name__ == "__main__":
    main()
