# Subtitle Adder

A desktop app that adds subtitles to video automatically, running entirely locally. The UI is native on every platform (macOS/SwiftUI first, then Windows/WinUI); the work is done in a shared Python engine.

## Architecture

```
apps/macos  (SwiftUI)  ─┐
apps/windows (WinUI)   ─┼─ subprocess + JSON line events ─▶  engine/ (Python)
                        ┘
```

Engine pipeline (`engine/src/subtitle_engine/`):

1. **Audio extraction** — `audio.py` (PyAV): video → 16 kHz mono WAV
2. **Speech-to-text** — `transcribe.py`: `Transcriber` protocol, default `FasterWhisperTranscriber` (on-device). Produces a `Transcript` with word timestamps.
3. **Sentence segmentation** — `sentences.py`: sentences by punctuation/silence; a long sentence is split at a comma or at the `max_chars`/`max_duration` limit.
4. **Subtitles** — `subtitles.py`: SRT / VTT / JSON.
5. **Speaker diarization (optional)** — `diarize.py`: `[(start, end, speaker)]` via sherpa-onnx; assigns a speaker to each word, and `sentences.py` splits at speaker changes.

`pipeline.py` runs the five steps end to end (diarization is optional), `cli.py` exposes them.

## Engine protocol (version 1)

`subtitle-engine transcribe <video> --out-dir D [--language tr] [--model small] [--format srt vtt json] --json`
writes one JSON event per line to stdout:

- `{"event": "stage", "stage": "extracting_audio|downloading_model|loading_model|transcribing|diarizing|segmenting|translating", "fraction": 0.42 | null}`
- `{"event": "models", "models": [{"id", "kind", "backend", "languages", "size_mb", "disk_mb", "speed", "accuracy", "license", "downloaded"}]}`
- `{"event": "deleted", "id": "small", "freed_mb": 464}`
- `{"event": "result", "language", "duration", "audio", "outputs": [...], "sentences": [{"id", "start", "end", "text", "speaker": 0 | null, "words": [{"start", "end", "text"}]}]}`
- `{"event": "translation", "language": "es", "sentences": [{"id", "start", "end", "text", "speaker", "words": []}]}`
- `{"event": "error", "message": "..."}` (exit code 1)

In the `models` event, `kind` is `"transcription"` (speech recognition), `"translation"` or `"diarization"` (speaker diarization); `backend` is `"whisper"` (faster-whisper), `"onnx"` (onnx-asr), `"ctranslate2"` (translation) or `"sherpa"` (sherpa-onnx, diarization). `license` is the SPDX short name from the model card; `speaker-diarization` consists of two models, so its value is an SPDX expression (`"MIT AND Apache-2.0"`). `languages` is `null` (all languages) for Whisper and `speaker-diarization`, and a list of supported ISO 639-1 codes for the others. Catalog ids: `base`, `small`, `medium`, `large-v3-turbo`, `large-v3` (Whisper, MIT) and `parakeet-v3` (NVIDIA Parakeet TDT 0.6B v3, CC-BY-4.0), `canary-1b-v2` (NVIDIA Canary 1B v2, CC-BY-4.0), `gigaam-v3` (GigaAM v3 e2e RNN-T, `ru` only, MIT); the speaker diarization model `speaker-diarization` (pyannote segmentation-3.0 int8, MIT + 3D-Speaker CAM++ embedding, Apache-2.0; `license` `"MIT AND Apache-2.0"`, ~29 MB, a single entry spanning two HF repos); the translation models `madlad-3b` (MADLAD-400 3B, Apache-2.0, default) and `nllb-1.3b` (NLLB-200 1.3B, CC-BY-NC-4.0, no commercial use). Parakeet and Canary support 25 European languages (including `pt`, `es`, `ru`); `tr` is only in Whisper. The translation models support `tr`, `en`, `pt`, `es`, `ru`. If the chosen language is not in the model's `languages` list, the engine returns a localized explanation in an `error` event (e.g. "GigaAM yalnız Rusça destekler.").

`downloading_model` is emitted before `loading_model`, and only when a catalog model (`models.py`) is not in the cache; `fraction` is downloaded bytes over total bytes (0..1, at most ~10 Hz). For onnx models the shared silero VAD file is downloaded in this stage too. Names outside the catalog (`turbo`, `tiny`, etc.) are left to faster-whisper's own download and this stage is not emitted.

With `--diarize`, `downloading_model` can appear twice in one run: first for the transcription model (if missing), then for `speaker-diarization` (if missing), both before `loading_model`; each run goes 0..1 on its own. `transcribe -m` accepts only `kind` `"transcription"` catalog models: anything else (for example `speaker-diarization`, `madlad-3b`) gets a localized `error` before any download. `translate --model` likewise accepts only translation models.

`subtitle-engine transcribe ... --diarize [--speakers N]`: speaker diarization (fully local, off by default). When on, `downloading_model` (only if the `speaker-diarization` model is not cached; emitted before `loading_model`) and a `diarizing` stage (`fraction` 0..1, after `transcribing`) are emitted, and every `result` sentence carries `speaker`: a 0-based integer numbered by first appearance (0 is the first person to speak), or `null` when diarization was off. The speaker count is automatic (clustering threshold, speakers with under 1 s of speech are dropped as noise) unless `--speakers N` is given; `--speakers` without `--diarize` is an error. Auto mode also merges clusters whose centroid embeddings (bounded sample, same CAM++ model) have cosine similarity of at least 0.65, because clustering over-splits long recordings; this is skipped with `--speakers N`. Each word gets the speaker with the largest time overlap (nearest segment if none); a lone word between two longer runs of one speaker joins them unless it is a complete short reply, and a speaker change in the middle of a sentence snaps to the nearest sentence end when that moves at most 2 words / 0.8 s. `sentences.py` splits at every speaker change, so a subtitle never contains two speakers. `translate` keeps `speaker`.

Speaker dash rule (SRT and VTT, computed at render time, never stored in `text`): if the sentences contain at least two distinct non-null speakers, every sentence whose speaker differs from the previous known speaker (sentences with a `null` speaker are skipped and do not reset it; the first sentence included) is prefixed with `- ` unless its text already starts with `- ` or `– `; a sentence continuing with the same speaker gets none. With one speaker or all `null` there are no dashes. JSON keeps `speaker` as is.

`subtitle-engine render --format srt [--out F]`: reads a JSON array of sentences from stdin (as edited in the app; the `speaker` field is read if present, and the dash rule is applied here) and writes the subtitle text. Subtitle formats are produced only in the engine.

`subtitle-engine translate --to <lang> [--from <lang>] [--model madlad-3b] [--json]`: reads the same JSON array of sentences as `render` from stdin and translates every sentence with a fully local model; `id`/`start`/`end`/`speaker` are kept, `text` is translated, `words` becomes an empty list. With `--json` it emits, in order, `downloading_model` (`fraction` 0..1, if needed), `loading_model`, `translating` (`fraction` 0..1) stages and finally the `translation` event; without `--json` the translated SRT is written to stdout. The default model is MADLAD-400 and it needs no source language; `--from` is required for `nllb-1.3b`. An unsupported language or a missing source language produces a localized `error` event and exit code 1. Model files and the tokenizer are downloaded to the Hugging Face cache on first use; no audio or text is sent out.

`subtitle-engine models [--json]`: lists the model catalog (`base`, `gigaam-v3`, `small`, `parakeet-v3`, `canary-1b-v2`, `medium`, `large-v3-turbo`, `large-v3`, `nllb-1.3b`, `madlad-3b`, `speaker-diarization`) with kind, approximate size, relative speed/accuracy, backend, license, supported languages and local cache status; no network. `disk_mb` is the actual disk usage of the model's repo in the cache (MB, rounded to an integer; shared VAD excluded), or `null` if not downloaded. `--json` writes a single `models` event line, otherwise a readable table. For `speaker-diarization` (two repos, one of which also holds dozens of unrelated models) `disk_mb` sums only the catalog-owned files of both repos, not the repos' full size.

`subtitle-engine delete-model <id> [--json]`: deletes a downloaded model's repo from the cache with all its revisions and blobs; touches only that repo folder, the shared silero VAD and other models are preserved. Files in the shared blob store (`<cache>/blobs/`) that no other repo uses are cleaned too; `freed_mb` is the space actually freed (it can be 0 for files shared with another model). `--json` writes a single line `{"event": "deleted", "id": "...", "freed_mb": 123}`. For a model outside the catalog or not downloaded, a clear localized `error` event and exit code 1 are returned. For `speaker-diarization`, only the owned files (their snapshot entries and blobs, shared-store sweep included) are deleted from both repos, and a repo folder is removed only when nothing else is left in it. If one repo fails, the others are still cleaned and the result is a localized `error` saying the model was partially deleted, how many MB were freed and what remains.

Message language: every user-facing text the engine produces (`error` messages, the non-`--json` output, language and model display names) is Turkish or English. It is chosen by `--lang tr|en` (accepted before or after the subcommand; the subcommand-level value wins) or else the `SUBTITLE_ENGINE_LANG` environment variable (`tr`|`en`, case-insensitive); unset or any other value means `tr`. JSON shapes do not change. The macOS app sets `SUBTITLE_ENGINE_LANG` to its current UI language on every engine launch.

Network: the engine uses the network only to download a missing catalog model. Once the models a command needs are on disk (and from the start for `render`, `models`, `delete-model`) it sets the Hugging Face libraries offline (`HF_HUB_OFFLINE`, telemetry off), so no request is even attempted; cached Whisper models load with `local_files_only`. Strict offline mode (`--offline` before the subcommand, or `SUBTITLE_ENGINE_OFFLINE=1`) never uses the network: a missing model gives a localized `error` event (exit code 1) before any work starts.

stderr is a free-form log (model loading warnings, etc.); the apps redirect it to a file, and it is not part of the protocol. When the protocol changes, `PROTOCOL_VERSION` in `cli.py`, `EngineEvent.swift` and this section are updated together. These stay in version 1 as backward-compatible additions: the `downloading_model` stage, the `models` command, the `backend`/`languages`/`disk_mb` fields in the `models` event, the `delete-model` command and the `deleted` event, the onnx model ids, the `translate` command, the `translating` stage, the `translation` event, the `kind`/`license` fields in the `models` event; also the `diarizing` stage, the `transcribe --diarize/--speakers` flags, the `speaker` field on sentences, the `speaker-diarization` catalog entry (`kind` `"diarization"`, `backend` `"sherpa"`) and the speaker dash in render; also `SUBTITLE_ENGINE_LANG` and `--lang`; also `SUBTITLE_ENGINE_OFFLINE` and `--offline` (`PROTOCOL_VERSION` does not change).

The app finds the engine in this order: the `SUBTITLE_ENGINE` environment variable → `Contents/Resources/subtitle-engine/subtitle-engine` inside the bundle (PyInstaller onedir; otherwise `Contents/MacOS/subtitle-engine`) → in development, `uv run --project engine subtitle-engine`.

## Commands

```bash
cd engine && uv sync && uv run pytest              # engine tests
cd engine && uv run pytest -m slow                 # real STT (samples/demo.mp4)
cd engine && uv run subtitle-engine transcribe ../samples/demo.mp4 -m base -l tr -o ../output -f srt vtt json
cd apps/macos && swift build && swift test         # app + EngineKit tests
cd apps/macos && ENGINE_E2E=1 swift test           # Swift → engine end to end
cd apps/macos && swift run SubtitleAdderApp
scripts/package-macos.sh                           # build/Subtitle Adder.app (needs no uv/repo)
```

Packaging: `engine/subtitle-engine.spec` builds the engine as a PyInstaller onedir (`build` dependency group); the script assembles the `.app` and signs it inside-out (verified with `codesign --verify --deep --strict`). The default is an ad-hoc signature, which only works on the machine that built it. `SIGN_IDENTITY="Developer ID Application: ..."` signs with the hardened runtime + `scripts/macos/engine.entitlements` (only `disable-library-validation`); `NOTARY_PROFILE=<notarytool profile>` does notarization + staple; `MAKE_DMG=1` produces a `.dmg`; `SKIP_ENGINE_BUILD=1` reuses a prebuilt engine. The engine lives under `Contents/Resources` because codesign rejects non-Mach-O files under `Frameworks`/`MacOS`; every Mach-O file inside it is signed individually. Model files are not included in the package; they are downloaded on first use.

Two-voice sample for diarization (macOS; English voices, alternating turns):

```bash
cd samples && i=0; while IFS='|' read -r v t; do i=$((i+1)); say -v "$v" -o t$i.aiff "$t"; afconvert -f WAVE -d LEI16@16000 -c 1 t$i.aiff t$i.wav; done <<'EOF'
Samantha (English (US))|Hello, how are you doing today? I wanted to ask you about the project.
Daniel|I am doing well, thank you. The project is going fine, we finished the first part yesterday.
Samantha (English (US))|That is great news. When do you think the second part will be ready?
Daniel|I think we will need about two more weeks. There are still a few problems to solve.
Samantha (English (US))|Okay, that sounds reasonable. Please keep me updated.
Daniel|Of course. I will send you a short report on Friday.
EOF
ffmpeg -y $(for n in 1 2 3 4 5 6; do echo -n "-i t$n.wav "; done) -filter_complex "concat=n=6:v=0:a=1[a]" -map "[a]" en.wav
ffmpeg -y -f lavfi -i color=c=black:s=320x240:r=10 -i en.wav -shortest -c:v libx264 -c:a aac dialog-en.mp4
```

Sample video:

```bash
mkdir -p samples && say -v Yelda -o samples/speech.aiff "Merhaba, bu bir altyazı testidir. Sen ne yapacaksın?"
ffmpeg -y -f lavfi -i color=c=black:s=320x240 -i samples/speech.aiff -shortest -c:v libx264 -c:a aac samples/demo.mp4
```

Whisper models are downloaded to the Hugging Face cache on first use (`~/.cache/huggingface/`).

## Agents (`.claude/agents/`)

Model allocation: exploration Haiku, code-writing agents Sonnet, review Opus.

- `explorer` (Haiku) — read-only exploration, API verification
- `media-engineer` (Sonnet) — audio extraction, media (engine)
- `stt-engineer` (Sonnet) — STT backends, models (engine)
- `subtitle-engineer` (Sonnet) — sentence splitting, subtitle formats (engine)
- `macos-ui-engineer` (Sonnet) — SwiftUI app and EngineKit
- `module-reviewer` (Opus) — pre-commit review (writes no code)

The same agents also run with DeepSeek (pi, opencode provider, thinking max):

```bash
scripts/pi-agent.sh subtitle-engineer "<task>"
PI_MODEL=opencode-go/deepseek-v4-pro:max scripts/pi-agent.sh stt-engineer "<task>"
```

The script passes the `.claude/agents/<agent>.md` definition as the system prompt; explorer and module-reviewer are read-only. pi sessions are kept under `~/.pi/agent/sessions/`.

The main session is the coordinator: it hands work to agents, runs it through tests and review, makes small topical commits, and does not push unless asked.

## Rules

- License is MIT (open source). Models bundled with the app or used by default must be openly licensed; models under non-commercial licenses (e.g. NLLB) are downloaded only on request and are marked in the UI.

- The UI is native and polished; business logic does not go into the UI, it lives in the engine.
- The engine makes no platform-specific assumptions (no system ffmpeg, no hard-coded paths); it works the same on macOS and Windows.
- Python ≥3.11 (uv), Swift 6 strict concurrency, macOS 15+.
- User-facing text is localized in Turkish (default) and English: the macOS app uses String Catalogs (`Localizable.xcstrings` in `SubtitleAdderApp` and `EngineKit`, looked up through `L()` so the language switches immediately in Settings); the engine uses `i18n.py`. Every new user-facing string needs both languages. Developer-facing text — code comments, docs, scripts, tests, commit messages — is English.
- Local only: no cloud STT or any service that sends audio out. The network is used only to download models.
- State and resume point: `.claude/progress/STATE.md`.
