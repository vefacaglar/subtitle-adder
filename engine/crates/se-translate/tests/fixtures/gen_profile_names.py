"""Generates profile_names.json: [model name, profile_for_model(name).name] pairs from the Python engine.

Run with the engine venv: engine/.venv/Scripts/python.exe gen_profile_names.py
"""
import itertools
import json
from pathlib import Path

from subtitle_engine.translation_profiles import profile_for_model

BASES = ["gemma", "GEMMA", "qwen", "QWEN", "translate gemma", "HY_MT1.5", "hunyuan-mt-7b",
         "ﬁgemma", "qwen qwen"]
SEPS = ["", "-", "_", " ", "　", "--", ".", ":"]
TAILS = ["4", "3", "3n", "2", "3.5", "35", "40", "4b", "3n:e4b", "3.5:9b", "3:8b", "3-instruct", "3-vl",
         "3-next", "3\n", "4\n", "3.5.1", "3_thinking", "1", ""]
ENDS = ["", ":12b", "/x", ".gguf", "\n", " x"]

names = sorted({b + s + t + e for b, s, t, e in itertools.product(BASES, SEPS, TAILS, ENDS)})
names += ["Straße-gemma4", "qwen3\n\n", "Kqwen3", "GEMMA 4", "Hunyuan MT 7B chimera",
          "x/gemma-4", "İqwen3"]
rows = [[n, profile_for_model(n).name] for n in names]
Path(__file__).with_name("profile_names.json").write_text(json.dumps(rows, ensure_ascii=False), encoding="utf-8")
print(len(rows))
