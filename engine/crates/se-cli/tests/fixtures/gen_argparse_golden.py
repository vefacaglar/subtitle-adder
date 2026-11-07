"""Generates argparse_golden.json: the Python engine's real parser on many argv cases.

Run with the engine venv: engine/.venv/Scripts/python.exe -I gen_argparse_golden.py <engine/src> <out.json>
Each case records exit code (None = parsed), stdout, stderr, and the parsed namespace.
"""
import contextlib
import io
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, sys.argv[1])
os.environ["COLUMNS"] = "80"
from subtitle_engine import cli  # noqa: E402

CASES = [
    [], ["-h"], ["--help"], ["--he"], ["--version"], ["--v"], ["--version", "models"], ["-h", "models"],
    ["bogus"], ["--lang"], ["--lang", "en"], ["--lang=en", "models"], ["--lan", "en", "models"], ["--off", "models"],
    ["--offline", "models", "--json"], ["--lang", "en", "--lang", "xx", "models"], ["-x"], ["-x", "models"],
    ["--lang", "en", "--", "models"], ["models", "--", "--json"], ["models", "--offline"], ["models", "--version"],
    ["models", "-h"], ["models", "--js"], ["models", "--json=x"], ["models", "--json="], ["models", "x"],
    ["models", "--lang", "en"], ["models", "--lang=", "--json"], ["models", "--lang"],
    ["extract-audio"], ["extract-audio", "-h"], ["extract-audio", "v.mp4"], ["extract-audio", "v.mp4", "-o", "d", "--json"],
    ["extract-audio", "v.mp4", "-od"], ["extract-audio", "v.mp4", "-o=d"], ["extract-audio", "v.mp4", "--out", "d"],
    ["extract-audio", "-1"], ["extract-audio", "-1.5", "-o", "-2"], ["extract-audio", "-x"], ["extract-audio", "a b"],
    ["extract-audio", "--a b"], ["extract-audio", "-"],
    ["preview"], ["preview", "x"], ["preview", "-h"], ["preview", "-", "--output", "o.mp4", "--json"],
    ["preview", "x", "--out", "o"], ["preview", "x", "--output"],
    ["transcribe"], ["transcribe", "-h"], ["transcribe", "x.mp4"], ["transcribe", "x.mp4", "-f"],
    ["transcribe", "x.mp4", "-l"], ["transcribe", "-f", "srt", "vtt", "x.mp4"], ["transcribe", "x.mp4", "-f", "srt", "srt", "json"],
    ["transcribe", "x.mp4", "--ma", "3"], ["transcribe", "x.mp4", "--max-c", "3"], ["transcribe", "x.mp4", "--max-chars", "abc"],
    ["transcribe", "x.mp4", "--max-chars", " 1_000 "], ["transcribe", "x.mp4", "--max-chars", "1__0"],
    ["transcribe", "x.mp4", "--max-chars", "+7"], ["transcribe", "x.mp4", "--max-chars", "-7"],
    ["transcribe", "x.mp4", "--max-chars", "١٢"], ["transcribe", "x.mp4", "--max-chars", "1.0"],
    ["transcribe", "x.mp4", "--speakers", "2.5"], ["transcribe", "x.mp4", "--speak", "2", "--dia"],
    ["transcribe", "x.mp4", "--max-duration", "x"], ["transcribe", "x.mp4", "--max-duration", "nan"],
    ["transcribe", "x.mp4", "--max-duration", "-Infinity"], ["transcribe", "x.mp4", "--max-duration", "1_0.2_5e1"],
    ["transcribe", "x.mp4", "--max-duration", ".5"], ["transcribe", "x.mp4", "--max-duration", "5."],
    ["transcribe", "x.mp4", "--max-duration", "1e"], ["transcribe", "x.mp4", "--max-duration", "_1"],
    ["transcribe", "x.mp4", "--form=json"], ["transcribe", "x.mp4", "-fjson"], ["transcribe", "x.mp4", "-f=vtt"],
    ["transcribe", "x.mp4", "-f", "xx"], ["transcribe", "x.mp4", "--l", "tr"], ["transcribe", "x.mp4", "--la", "tr"],
    ["transcribe", "x.mp4", "--langu", "tr"], ["transcribe", "x.mp4", "-l", "", "-m", "base"],
    ["transcribe", "--offline", "x.mp4"], ["transcribe", "--bogus", "x", "--json"], ["transcribe", "x.mp4", "y.mp4"],
    ["transcribe", "x.mp4", "--speakers", "-1", "--json"], ["transcribe", "x.mp4", "-l", "-1"],
    ["transcribe", "x.mp4", "-l", "-x"], ["transcribe", "x.mp4", "-o"], ["transcribe", "x.mp4", "--json", "-h"],
    ["transcribe", "x.mp4", "-hx"], ["transcribe", "x.mp4", "-h=x"], ["transcribe", "x.mp4", "--diarize=1"],
    ["transcribe", "-m", "base", "-l", "tr", "--max-chars", "40", "--max-duration", "3.5", "--diarize",
     "--speakers", "2", "-o", "out", "-f", "srt", "vtt", "--json", "--", "x.mp4"],
    ["transcribe", "x.mp4", "--lang", "en", "-o", "o"], ["--lang", "tr", "transcribe", "x.mp4", "--lang", "en"],
    ["translate"], ["translate", "-h"], ["translate", "--to"], ["translate", "--to", "en"],
    ["translate", "--to", "en", "--from", "tr", "-m", "nllb-1.3b", "--json"], ["translate", "--t", "en", "--fr", "tr"],
    ["translate", "--to=es", "-mqwen3.5-4b"], ["translate", "x"], ["translate", "--to", "en", "x"],
    ["render"], ["render", "-h"], ["render", "-f", "xx"], ["render", "--out"], ["render", "-f", "srt", "-f", "json"],
    ["render", "-o", "a.srt", "-f", "vtt"], ["render", "-o", ""], ["render", "--json"], ["render", "-f", "srt", "vtt"],
    ["delete-model"], ["delete-model", "-h"], ["delete-model", "base"], ["delete-model", "a", "b"],
    ["delete-model", "nope", "--lang", "en", "--json"], ["delete-model", "nope", "--lang"],
    ["delete-model", "--json", "--", "-x"], ["delete-model", "--", "--", "x"], ["delete-model", "-5"],
    ["delete-model", "--lang", "tr", "nope", "--lang", "en"], ["extract-audio", "--", "-x", "--json"],
    ["models", "-", "x"], ["--lang=", "models"], ["--lang", "", "models"], ["", "models"], ["models", ""],
    ["--offline", "--offline", "models"], ["extract-audio", "v", "-o", "a", "-o", "b"],
]


def plain(value):
    if isinstance(value, Path):
        return {"path": str(value)}
    if isinstance(value, list):
        return [plain(v) for v in value]
    return value


out = []
for argv in CASES:
    stdout, stderr = io.StringIO(), io.StringIO()
    code = None
    ns = None
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        try:
            args = cli.build_parser().parse_args(argv)
            ns = {k: plain(v) for k, v in vars(args).items() if k != "handler"}
        except SystemExit as exc:
            code = exc.code if isinstance(exc.code, int) else (0 if exc.code is None else 1)
    out.append({"argv": argv, "code": code, "stdout": stdout.getvalue(), "stderr": stderr.getvalue(), "ns": ns})

with open(sys.argv[2], "w", encoding="utf-8", newline="\n") as f:
    json.dump(out, f, ensure_ascii=False, indent=1)
print(len(out), "cases")
