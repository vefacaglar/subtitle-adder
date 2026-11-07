"""End-to-end parity harness: Python reference engine vs the staged Rust engine.

Each case runs both engines with the same arguments, stdin, environment, cwd and --out-dir path (the folder is
emptied between the two runs, and the produced files are moved aside), so stdout must be byte-identical.
Usage (Python reference engine in engine/.venv, Rust engine staged into engine-rs/dist by scripts/stage-runtime.ps1):
    engine/.venv/Scripts/python.exe engine-rs/tools/parity/parity.py [case-name-substring ...]
PARITY_DIR (default <temp>/se-parity) holds samples/ (made by synth.ps1, then synth.sh), inputs/, results/ and
summary.txt. The translate/render cases read inputs/tr_sentences.json and inputs/en_dia_sentences.json, which are
written from the Python results of transcribe_tr_json and diarize_en_json, so run those cases first.
"""
import tempfile
import difflib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(os.environ.get("PARITY_DIR") or Path(tempfile.gettempdir()) / "se-parity").resolve()
S = HERE / "samples"
WORK = HERE / "work"          # shared --out-dir / cwd for both engines
RES = HERE / "results"
REPO = Path(__file__).resolve().parents[3]
PY = REPO / "engine" / ".venv" / "Scripts" / "subtitle-engine.exe"
RS = REPO / "engine-rs" / "dist" / "subtitle-engine.exe"
TMP = HERE / "tmp"
HF = HERE / "hfcache"
REAL = Path.home() / ".cache" / "huggingface" / "hub"


def seed_cache():
    if HF.exists():
        shutil.rmtree(HF)
    HF.mkdir()
    for r in ("models--csukuangfj--sherpa-onnx-pyannote-segmentation-3-0", "models--csukuangfj--speaker-embedding-models", "models--Systran--faster-whisper-base"):
        shutil.copytree(REAL / r, HF / r, symlinks=True)
            # isolated tempfile.gettempdir() for both


def sentences_of(path):
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        ev = json.loads(line)
        if ev.get("event") == "result":
            return ev["sentences"]
    raise SystemExit(f"no result in {path}")


# (name, args, stdin_file_or_None, extra_env, pre_steps)
# OUT is replaced by the shared out dir. pre: list of arg lists run (with the same engine) before the case.
CASES = [
    ("models_json", ["models", "--json"], None, {}, []),
    ("models_table_tr", ["models"], None, {}, []),
    ("models_table_en", ["--lang", "en", "models"], None, {}, []),
    ("transcribe_en_json", ["transcribe", str(S / "dialog-en.mp4"), "-m", "base", "-l", "en", "--out-dir", "OUT", "-f", "srt", "vtt", "json", "--json"], None, {}, []),
    ("transcribe_tr_json", ["transcribe", str(S / "dialog-tr.mp4"), "-m", "base", "-l", "tr", "--out-dir", "OUT", "-f", "srt", "vtt", "json", "--json"], None, {}, []),
    ("transcribe_autolang_json", ["transcribe", str(S / "dialog-tr.mp4"), "-m", "base", "--out-dir", "OUT", "--json"], None, {}, []),
    ("transcribe_tr_text", ["transcribe", str(S / "dialog-tr.mp4"), "-m", "base", "-l", "tr", "--out-dir", "OUT", "-f", "srt"], None, {}, []),
    ("transcribe_en_text_lang_en", ["transcribe", str(S / "dialog-en.mp4"), "-m", "base", "-l", "en", "--out-dir", "OUT", "--lang", "en"], None, {}, []),
    ("diarize_en_json", ["transcribe", str(S / "dialog-en.mp4"), "-m", "base", "-l", "en", "--out-dir", "OUT", "-f", "srt", "vtt", "json", "--diarize", "--json"], None, {}, []),
    ("diarize_tr_json", ["transcribe", str(S / "dialog-tr.mp4"), "-m", "base", "-l", "tr", "--out-dir", "OUT", "-f", "srt", "json", "--diarize", "--json"], None, {}, []),
    ("diarize_en_speakers2", ["transcribe", str(S / "dialog-en.mp4"), "-m", "base", "-l", "en", "--out-dir", "OUT", "-f", "srt", "--diarize", "--speakers", "2", "--json"], None, {}, []),
    ("extract_audio_json", ["extract-audio", str(S / "dialog-en.mp4"), "--out-dir", "OUT", "--json"], None, {}, []),
    ("extract_audio_text", ["extract-audio", str(S / "dialog-tr.mp4"), "--out-dir", "OUT"], None, {}, []),
    ("extract_then_transcribe", ["transcribe", str(S / "dialog-en.mp4"), "-m", "base", "-l", "en", "--out-dir", "OUT", "--json"], None, {},
     [["extract-audio", str(S / "dialog-en.mp4"), "--out-dir", "OUT", "--json"]]),
    ("transcribe_wav_input", ["transcribe", str(S / "en.wav"), "-m", "base", "-l", "en", "--out-dir", "OUT", "-f", "json", "--json"], None, {}, []),
    ("translate_tr_en_json", ["translate", "--to", "en", "--model", "madlad-3b", "--json"], "tr_sentences.json", {}, []),
    ("translate_tr_en_srt", ["translate", "--to", "en"], "tr_sentences.json", {}, []),
    ("translate_en_es_json", ["translate", "--to", "es", "--from", "en", "--json"], "en_dia_sentences.json", {}, []),
    ("render_srt", ["render", "--format", "srt"], "en_dia_sentences.json", {}, []),
    ("render_vtt", ["render", "--format", "vtt"], "en_dia_sentences.json", {}, []),
    ("render_json", ["render", "--format", "json"], "en_dia_sentences.json", {}, []),
    ("render_srt_out", ["render", "--format", "srt", "--out", "OUT/r.srt"], "tr_sentences.json", {}, []),
    ("preview_mkv", ["preview", str(S / "clip.mkv"), "--output", "OUT/p.mp4"], None, {}, []),
    ("preview_mp4", ["preview", str(S / "dialog-tr.mp4"), "--output", "OUT/p.mp4"], None, {}, []),
    ("preview_missing", ["preview", str(S / "nope.mkv"), "--output", "OUT/p.mp4"], None, {}, []),
    # error cases
    ("err_bad_model", ["transcribe", str(S / "dialog-en.mp4"), "-m", "nosuchmodel", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_translation_model_for_asr", ["transcribe", str(S / "dialog-en.mp4"), "-m", "madlad-3b", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_diar_model_for_asr_en", ["--lang", "en", "transcribe", str(S / "dialog-en.mp4"), "-m", "speaker-diarization", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_offline_missing_model", ["--offline", "transcribe", str(S / "dialog-en.mp4"), "-m", "parakeet-v3", "-l", "en", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_offline_env_missing_model", ["transcribe", str(S / "dialog-en.mp4"), "-m", "gigaam-v3", "-l", "ru", "--out-dir", "OUT", "--json"], None, {"SUBTITLE_ENGINE_OFFLINE": "1"}, []),
    ("err_offline_translate_llama", ["--offline", "translate", "--to", "en", "--model", "qwen3.5-4b", "--json"], "tr_sentences.json", {}, []),
    ("err_gigaam_tr", ["transcribe", str(S / "dialog-tr.mp4"), "-m", "gigaam-v3", "-l", "tr", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_speakers_without_diarize", ["transcribe", str(S / "dialog-en.mp4"), "-m", "base", "--speakers", "2", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_speakers_zero", ["transcribe", str(S / "dialog-en.mp4"), "-m", "base", "--diarize", "--speakers", "0", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_missing_video", ["transcribe", str(S / "missing.mp4"), "-m", "base", "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_missing_video_text", ["transcribe", str(S / "missing.mp4"), "-m", "base", "--out-dir", "OUT"], None, {}, []),
    ("err_extract_missing", ["extract-audio", str(S / "missing.mp4"), "--out-dir", "OUT", "--json"], None, {}, []),
    ("err_translate_bad_lang", ["translate", "--to", "xx", "--json"], "tr_sentences.json", {}, []),
    ("err_translate_nllb_no_from", ["translate", "--to", "en", "--model", "nllb-1.3b", "--json"], "tr_sentences.json", {}, []),
    ("err_translate_asr_model", ["translate", "--to", "en", "--model", "base", "--json"], "tr_sentences.json", {}, []),
    ("err_translate_external_no_host", ["translate", "--to", "en", "--json"], "tr_sentences.json", {"SUBTITLE_ENGINE_EXTERNAL_LLM_URL": "http://"}, []),
    ("err_translate_external_key_http", ["translate", "--to", "en", "--json"], "tr_sentences.json",
     {"SUBTITLE_ENGINE_EXTERNAL_LLM_URL": "http://example.com/v1", "SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY": "sk-test"}, []),
    ("err_translate_external_offline_cloud", ["--offline", "translate", "--to", "en", "--json"], "tr_sentences.json",
     {"SUBTITLE_ENGINE_EXTERNAL_LLM_URL": "https://openrouter.ai/api/v1"}, []),
    ("err_render_bad_json", ["render", "--format", "srt"], "bad.json", {}, []),
    ("err_render_bad_format", ["render", "--format", "ass"], "tr_sentences.json", {}, []),
    ("err_delete_not_downloaded", ["delete-model", "parakeet-v3", "--json"], None, {}, []),
    ("err_delete_unknown_en", ["--lang", "en", "delete-model", "nosuch"], None, {}, []),
    ("usage_no_args", [], None, {}, []),
    ("usage_help", ["--help"], None, {}, []),
    ("usage_transcribe_help", ["transcribe", "--help"], None, {}, []),
    ("usage_unknown_cmd", ["frobnicate"], None, {}, []),
    ("usage_transcribe_no_video", ["transcribe"], None, {}, []),
    ("usage_offline_after_sub", ["transcribe", str(S / "dialog-en.mp4"), "--offline"], None, {}, []),
    ("usage_bad_format", ["transcribe", str(S / "dialog-en.mp4"), "-f", "ass"], None, {}, []),
    ("usage_speakers_not_int", ["transcribe", str(S / "dialog-en.mp4"), "--diarize", "--speakers", "two"], None, {}, []),
    ("usage_translate_no_to", ["translate"], None, {}, []),
    ("usage_bad_lang", ["--lang", "de", "models"], None, {}, []),
    ("version", ["--version"], None, {}, []),
    ("models_json_tempcache", ["models", "--json"], None, {"HF_HUB_CACHE": str(HF)}, [seed_cache]),
    ("delete_diarization_json", ["delete-model", "speaker-diarization", "--json"], None, {"HF_HUB_CACHE": str(HF)}, [seed_cache]),
    ("delete_base_text_en", ["--lang", "en", "delete-model", "base"], None, {"HF_HUB_CACHE": str(HF)}, [seed_cache]),
    ("models_after_delete", ["models", "--json"], None, {"HF_HUB_CACHE": str(HF)}, [seed_cache, ["delete-model", "base"]]),
    ("err_offline_tempcache_no_diar", ["--offline", "transcribe", str(S / "dialog-en.mp4"), "-m", "base", "--diarize", "--out-dir", "OUT", "--json"], None,
     {"HF_HUB_CACHE": str(HF)}, [seed_cache, ["delete-model", "speaker-diarization"]]),
    ("preview_stdin_dash", ["preview", "-", "--output", "OUT/p.mp4"], "clip.mkv", {}, []),
    ("transcribe_lang_en_error", ["--lang", "en", "transcribe", str(S / "dialog-tr.mp4"), "-m", "gigaam-v3", "-l", "tr", "--out-dir", "OUT"], None, {}, []),
]


def run(exe, args, stdin, env_extra):
    env = dict(os.environ)
    for k in list(env):
        if k.startswith("SUBTITLE_ENGINE_") and k not in ("SUBTITLE_ENGINE_SHERPA_DIR",):
            del env[k]
    env.update(env_extra)
    env["TMP"] = env["TEMP"] = str(TMP)
    data = (HERE / "inputs" / stdin).read_bytes() if stdin else b""
    t = time.time()
    p = subprocess.run([str(exe), *args], input=data, capture_output=True, env=env, cwd=str(WORK))
    return p.returncode, p.stdout, p.stderr, time.time() - t


def fresh(d):
    if d.exists():
        shutil.rmtree(d)
    d.mkdir(parents=True)


def snapshot(dest):
    """Move WORK and TMP contents to dest."""
    fresh(dest)
    for src, name in ((WORK, "out"), (TMP, "tmp"), (HF, "hf")):
        if not src.exists():
            continue
        shutil.copytree(src, dest / name)


def tree(d):
    return {str(p.relative_to(d)).replace("\\", "/"): p.read_bytes() for p in sorted(d.rglob("*")) if p.is_file()}


def one(case):
    name, args, stdin, env, pre = case
    args = [a.replace("OUT", str(WORK)) if a.startswith("OUT") else a for a in args]
    pre = [step if callable(step) else [a.replace("OUT", str(WORK)) if a.startswith("OUT") else a for a in step] for step in pre]
    out = {}
    for tag, exe in (("py", PY), ("rs", RS)):
        fresh(WORK)
        fresh(TMP)
        for step in pre:
            step() if callable(step) else run(exe, step, None, env)
        rc, so, se, dt = run(exe, args, stdin, env)
        d = RES / name / tag
        snapshot(d)
        (d / "stdout").write_bytes(so)
        (d / "stderr").write_bytes(se)
        (d / "rc").write_text(str(rc))
        out[tag] = (rc, so, se, {**tree(d / "out"), **({"hf/" + k: v for k, v in tree(d / "hf").items()} if (d / "hf").exists() and "HF_HUB_CACHE" in env else {})}, dt)
    (prc, pso, pse, pfiles, pdt), (rrc, rso, rse, rfiles, rdt) = out["py"], out["rs"]
    status = []
    status.append("rc=" + ("ok" if prc == rrc else f"DIFF({prc}/{rrc})"))
    status.append("stdout=" + ("ok" if pso == rso else "DIFF"))
    status.append("stderr=" + ("ok" if pse == rse else "diff"))
    fd = [k for k in sorted(set(pfiles) | set(rfiles)) if pfiles.get(k) != rfiles.get(k)]
    status.append("files=" + ("ok" if not fd else "DIFF:" + ",".join(fd)))
    line = f"{name:40s} {' '.join(status)}  py {pdt:.1f}s rs {rdt:.1f}s"
    print(line, flush=True)
    if pso != rso:
        a = pso.decode("utf-8", "replace").splitlines()
        b = rso.decode("utf-8", "replace").splitlines()
        diff = list(difflib.unified_diff(a, b, "py", "rs", n=0, lineterm=""))
        (RES / name / "stdout.diff").write_text("\n".join(diff), encoding="utf-8")
    return line


def prepare_inputs():
    inp = HERE / "inputs"
    inp.mkdir(parents=True, exist_ok=True)
    if (S / "clip.mkv").exists():
        shutil.copy(S / "clip.mkv", inp / "clip.mkv")
    for case, name in (("transcribe_tr_json", "tr_sentences.json"), ("diarize_en_json", "en_dia_sentences.json")):
        src = RES / case / "py" / "stdout"
        if src.exists():
            (inp / name).write_text(json.dumps(sentences_of(src), ensure_ascii=False), encoding="utf-8")
    (inp / "bad.json").write_text("[{\"id\": 1, \"start\": 0}", encoding="utf-8")


if __name__ == "__main__":
    prepare_inputs()
    RES.mkdir(exist_ok=True)
    filt = sys.argv[1:]
    lines = []
    for c in CASES:
        if filt and not any(f in c[0] for f in filt):
            continue
        lines.append(one(c))
    with open(HERE / "summary.txt", "a", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
