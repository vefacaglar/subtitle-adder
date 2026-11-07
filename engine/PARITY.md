# Rust engine vs Python engine: end-to-end parity

Date: 2026-11-07. Machine: Windows 11 x86_64 (AMD CPU, 12 threads, no NVIDIA GPU). Reference: the Python engine in
`engine/.venv` (faster-whisper 1.2.1 / ctranslate2 4.8.4 wheel, PyAV 18.1 with FFmpeg 8.1.2, sherpa-onnx 2.0.1).
Rust: `cargo build --workspace --release`, staged into `engine/dist` by `scripts/stage-runtime.ps1` and run from
there, so it found its DLLs next to the exe and not on PATH. Real models come from `~/.cache/huggingface/hub`:
whisper `base`, `madlad-3b`, `speaker-diarization`.

## How it was run

- Samples: two-voice dialogues made with Windows SAPI (`tools/parity/synth.ps1` + `synth.sh`).
  - English: Zira and David, 6 turns, 33.8 s.
  - Turkish: Tolga, with turns 2/4/6 pitch-shifted by 1.35x to act as a second voice, 6 turns, 29.4 s.
  - Each was muxed as an H.264/AAC mp4. A 6 s VP9/Opus `clip.mkv` was also made for `preview`.
- Harness: `tools/parity/parity.py`. Each case runs both engines with the same arguments, stdin, environment, cwd,
  `--out-dir` path and isolated `TMP`/`TEMP`. The out dir is emptied between the two runs, so paths in stdout must
  match byte for byte. Cases that delete models use a seeded throwaway `HF_HUB_CACHE`.
- Compared for every case: exit code, stdout bytes, stderr bytes, and every file written to the out dir (and to the
  temp cache).

## Results (65 cases)

| Area | Cases | rc | stdout | stderr | files |
|---|---|---|---|---|---|
| `models` (`--json`, tr/en table, temp cache, after delete) | 5 | = | = | = | = |
| `delete-model` (speaker-diarization `--json`, base text en, not downloaded, unknown) | 4 | = | = | = | = (cache trees identical) |
| `extract-audio` (`--json`, text, missing file) | 3 | = | = | = | = (WAV + `.source.json` marker identical) |
| `render` srt/vtt/json/`--out` (diarized input, so speaker dashes) | 4 | = | = | = | = |
| `preview` mp4 stream copy, mkv VP9/Opus transcode | 2 | = | = | = | differ, FFmpeg build (A) |
| `preview` missing source | 1 | = | = | = | = |
| `preview -` (mkv on stdin) | 1 | Py 1 / Rs 0 | = | differs | differs (D) |
| `transcribe -m base` en/tr, `--json`/text, `-f srt vtt json`, auto language, WAV input, extract→reuse | 7 | = | model output (B) | = | model output (B) |
| `transcribe --diarize` en, tr, `--speakers 2` | 3 | = | model output (B) | = | model output (B) |
| `translate` madlad-3b tr→en `--json` and SRT | 2 | = | 3 of 13 lines (C) | = | = |
| `translate --to es --from en --json` (16 diarized lines) | 1 | = | = | = | = |
| Error cases (listed below) | 21 | = | = | = | = |
| Usage, help, version | 11 | = | = | = | = |

`=` means byte-identical.

The 21 error cases:
- bad model id;
- translation or diarization model passed to `-m`;
- strict offline with a missing model (`--offline` and `SUBTITLE_ENGINE_OFFLINE=1`);
- offline llama model;
- offline run with `speaker-diarization` missing;
- GigaAM with `tr` (in tr and en);
- `--speakers` without `--diarize`, and `--speakers 0`;
- missing video (json and text);
- `translate`: bad target, NLLB without `--from`, ASR model;
- external LLM: no host, API key over plain http, cloud server in offline mode;
- `render`: broken JSON, and bad format.

The 11 usage cases: no arguments, `--help`, `transcribe --help`, unknown command, missing video, `--offline` after
the subcommand, bad `-f`, a non-integer `--speakers`, `translate` without `--to`, bad `--lang`, and `--version`.

These match byte for byte in every run (other differences are explained below):
- The order of stage events, including `downloading_model`/`loading_model` and the `diarizing` null-then-fraction
  sequence;
- the `extracting_audio` fractions;
- the full list of `diarizing` fractions;
- `language`, `duration`, `audio` and `outputs`;
- the per-word speaker assignment, en and tr;
- all localized error texts.

The Rust engine is deterministic across runs (checked by running transcribe tr twice).

Timing on the same machine:

| Command | Python | Rust |
|---|---|---|
| transcribe tr, base | 2.8 s | 1.7 s |
| CLI startup | about 0.5 s | about 0.02 s |
| madlad translate, 13 lines | 4.2–4.6 s | 5.7–5.9 s |

Rust translate is slower: the CT2 build (C) and an unwarmed model load are possible causes, not yet measured.

## Explained differences

**(A) `preview` output bytes: FFmpeg build, not Rust code.**
- With stream copy (mp4), only one byte differs: the `Lavf62.12.102` vs `.103` encoder tag.
- With the VP9→H.264/AAC transcode, the `Lavc`/`Lavf` tags differ and so does a short run (~270 bytes) of encoded
  payload.
- Proof: I ran the same Rust exe with PyAV's own FFmpeg DLLs (`engine/.venv/Lib/site-packages/av.libs`, renamed to
  the plain names). Its output is byte-identical to Python's, payload included.
- So the difference is the dev SDK (BtbN FFmpeg n8.1.3, libx264 core 165) against PyAV's FFmpeg 8.1.2 build.
- For byte parity, packaging would have to ship the same FFmpeg build as PyAV.

**(B) Whisper words: CTranslate2 numerics.**
- English: the same 81 words with the same text; start and end times differ by at most 0.04 s (1–2 frames of 20 ms).
- Turkish: the same 56 words, but 4 differ in punctuation or case (for example `gidiyor.` vs `gidiyor,`, `İlk` vs
  `ilk`). Times differ by at most 0.06 s. Because sentences split at punctuation, the result has 13 sentences in
  Python and 11 in Rust.
- `partial` events differ only by their wall-clock throttling (0.5 s), which is not deterministic in either engine.
- Root cause: I fed the Python reference's exact mel features and VAD speech segments into the Rust decoder
  (`se-asr` example `fw_inject`). It still differs from Python in 20 of 56 Turkish words. So the decoder itself is
  the source. The Rust build uses CTranslate2 4.8.5 from ct2rs 0.10.4 with oneDNN 3.14.1 and the MKL prebuild;
  the Python wheel uses CTranslate2 4.8.4 with oneDNN 3.1.1. The int8 GEMM results differ and borderline beams flip.
- The Rust mel features (about 1e-7 off) add further small changes: injected run vs normal Rust run, 15 of 56
  words.
- Diarization is not affected: per-word speakers and the progress list are identical. Earlier se-diarize tests,
  given identical words, produced identical segments.

**(C) MADLAD translation text: same CTranslate2/oneDNN cause as (B).**
- tr→en: 3 of 13 lines differ ("Hello" vs "Hi", "is going" vs "'s going", "part" vs "episode").
- en→es: identical.
- Ids, times, speakers, the batch prefixes of incremental events and the fractions are identical.
- se-translate's parity work showed that the Rust output moves with `ONEDNN_MAX_CPU_ISA` and Python's does not.

**(D) `preview -` with a Matroska file on stdin.**
- Python fails on Windows with `[WinError 32]` and leaves a partial `p.mp4` behind.
- Rust succeeds, and its output equals the file-path preview of the same clip.
- This is the deliberate Windows deviation in the audio spec: piped stdin is treated as not seekable, and the output
  is not left half-written.

## Not covered here

- Model downloads (`downloading_model` with real network) were not run, to avoid downloading.
- These were not run end to end: the CUDA path, the llama-server/GGUF translators, real external LLM servers, and
  the onnx ASR models (parakeet, canary, gigaam are not in the cache).
- The asr checkpoint path (audio longer than 180 s) was not run here; se-asr tested it at crate level.
- macOS/Linux builds were not run (`scripts/stage-runtime.sh` is written but not run on a Mac).

## Build and test notes

- `cargo build --workspace --release` builds with no warnings (6.5 min from clean).
- `cargo test --workspace -j 2`: 581 passed, 0 failed, 16 ignored (real-model tests).
- With the default `-j` (12) the test build failed spuriously with "crate `onednn_src` / `intel_onemkl_prebuild` /
  ... required to be available in rlib format". Those debug rlibs are 911 MB and 634 MB, and many parallel rustc
  processes loading them run out of memory. `-j 2` (or a few jobs) builds reliably.
- Runtime files next to `subtitle-engine.exe`:
  - FFmpeg: `avcodec-62`, `avdevice-62`, `avfilter-11`, `avformat-62`, `avutil-60`, `swresample-6`, `swscale-9`;
  - `libiomp5md.dll`;
  - `onnxruntime.dll` and `sherpa-onnx-c-api.dll` (from sherpa-onnx 2.0.1).

  `stage-runtime.ps1` copies them and smoke-tests with a clean PATH. A diarized transcribe also ran from `dist` with
  only `SYSTEMROOT`, `USERPROFILE`, `TEMP` and a System32-only PATH.
