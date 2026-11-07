"""Naming parity driver: prints Python's WAV names, temp/marker names and source identities for the cases the
Rust test passes on stdin (JSON list of [video, out_dir]); one JSON object per case on stdout.

Usage: python py_naming.py < cases.json   (run with the Python engine's venv: engine/.venv)
"""
import json
import sys
from pathlib import Path

from subtitle_engine import pipeline


def main():
    cases = json.loads(sys.stdin.buffer.read().decode("utf-8"))
    out = []
    for video, out_dir in cases:
        v, o = Path(video), Path(out_dir)
        wav = pipeline.wav_path_for(v, o)
        out.append(
            {
                "stem": pipeline._safe_out_stem(v, o),
                "hash": pipeline._source_hash(v),
                "wav": str(wav),
                "marker": str(pipeline._source_marker_path(wav)),
                "identity": pipeline._source_identity(v),
            }
        )
    sys.stdout.buffer.write(json.dumps(out, ensure_ascii=True).encode("ascii"))


if __name__ == "__main__":
    main()
