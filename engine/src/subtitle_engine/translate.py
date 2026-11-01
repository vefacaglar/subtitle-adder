"""Step: translates sentences into the target language with fully local translation models.

Supports two local backends:
1. CTranslate2 encoder-decoder models (MADLAD-400, NLLB-200) with sentencepiece / Hugging Face tokenizers.
2. Prompt-driven GGUF LLMs (Gemma 4, Qwen 3.5) executed locally via `llama-server` (llama.cpp) with
   context-aware, dialect-aware subtitle translation prompts.

The source text never leaves the machine; the network is only used to download models/runtimes.
"""
import atexit
import contextlib
import hashlib
import json
import re
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from pathlib import Path
from typing import Protocol

from . import models
from .i18n import t
from .models import ModelError, Sentence

ProgressFn = Callable[[float], None]
BatchFn = Callable[[list[Sentence]], None]

# NLLB source/target language codes use the FLORES-200 format; MADLAD turns the ISO 639-1 code
# into a "<2xx>" token and needs no source language tag.
NLLB_LANGUAGE_CODES = {
    "tr": "tur_Latn",
    "pt": "por_Latn",
    "es": "spa_Latn",
    "ru": "rus_Cyrl",
    "en": "eng_Latn",
    "de": "deu_Latn",
    "fr": "fra_Latn",
    "ja": "jpn_Jpan",
    "zh": "zho_Hans",
}
MADLAD_PREFIX = "<2{}>"

# Dialect-aware language descriptions for direct LLM subtitle translation prompts.
LLM_LANGUAGE_SPECS = {
    "tr": "Turkish (natural, authentic everyday spoken Turkish, preserving street slang, idioms, and raw profanity without softening or dubbing-style censorship)",
    "pt": "Portuguese (including Brazilian Portuguese vernacular, street slang, profanity, and idioms)",
    "es": "Spanish (including Mexican and Latin American Spanish vernacular, street slang, profanity, and idioms)",
    "en": "English (natural, authentic spoken English, preserving slang, idioms, and raw profanity)",
    "ru": "Russian (natural, authentic spoken Russian, preserving slang, idioms, and raw profanity)",
    "de": "German (natural, authentic spoken German, preserving slang, idioms, and raw profanity)",
    "fr": "French (natural, authentic contemporary spoken French, preserving argot, verlan, slang, and raw profanity)",
    "ja": "Japanese (natural, authentic spoken Japanese with matching casual/rough conversational register, slang, and profanity)",
    "zh": "Simplified Chinese (natural, authentic spoken Mandarin Chinese, preserving colloquialisms, slang, and raw profanity)",
}

# Sentences are translated in batches of this size, not one by one; progress is reported after each batch.
BATCH_SIZE = 16
LLM_BATCH_SIZE = 6
LLM_CONTEXT_LINES = 2
BEAM_SIZE = 4
MAX_DECODING_LENGTH = 256
# add_source_eos is off in CT2 models; we append it to the end of the source like HF tokenizers do.
EOS_TOKEN = "</s>"

_THINK_BLOCK_RE = re.compile(r"<(?:think|thought|\|thought\|)>.*?</(?:think|thought|\|thought\|)>", re.DOTALL)
_UNCLOSED_THINK_RE = re.compile(r"\A\s*<(?:think|thought|\|thought\|)>.*?(?=\[\d+\])", re.DOTALL)
_NUMBERED_LINE_RE = re.compile(r"^\s*(?:\[(\d+)\]|(\d+)[\.\)\:])\s*(.*)$")


class Translator(Protocol):
    def translate(
        self,
        sentences: list[Sentence],
        target: str,
        source: str | None = None,
        on_progress: ProgressFn | None = None,
        on_batch: BatchFn | None = None,
    ) -> list[Sentence]: ...


def check_request(model_id: str, target: str, source: str | None = None) -> models.ModelInfo:
    """Is the request compatible with the catalog and language support? If not, raises a (localized) `ModelError`."""
    info = models.find(model_id)
    if info is None or info.kind != "translation":
        raise ModelError(
            t("unknown_translation_model", model=model_id)
        )
    supported = info.languages or ()
    if target not in supported:
        raise ModelError(models.support_error(info, target))
    if info.id == "nllb-1.3b":
        if not source:
            raise ModelError(t("nllb_source_required"))
        if source not in supported:
            name = models.display_name(info.id)
            raise ModelError(
                t("cannot_translate_from", name=name, source=source, supported=", ".join(supported))
            )
        if source not in NLLB_LANGUAGE_CODES or target not in NLLB_LANGUAGE_CODES:
            raise ModelError(t("nllb_language_undefined", target=target))
    # MADLAD uses no source language tag; --from is ignored even if given.
    return info


def create_translator(model_id: str, device: str = "auto", compute_type: str = "int8") -> Translator:
    """Builds the local translator for a catalog id (LLM via llama.cpp or CTranslate2)."""
    info = models.find(model_id)
    if info is None or info.kind != "translation":
        raise ModelError(
            t("unknown_translation_model", model=model_id)
        )
    if info.backend == "llama":
        return LlamaTranslator(model_id, device=device)
    return Ct2Translator(model_id, device=device, compute_type=compute_type)


class Ct2Translator:
    """Runs CTranslate2 T5 (MADLAD) and M2M100 (NLLB) models behind a single protocol."""

    def __init__(self, model_id: str, device: str = "auto", compute_type: str = "int8"):
        from . import cuda

        info = models.find(model_id)
        if info is None or info.kind != "translation":
            raise ModelError(t("unknown_translation_model_short", model=model_id))
        if device == "auto":
            device = "cuda" if cuda.cuda_usable() else "cpu"
        elif device == "cuda":
            cuda.register_libraries()
        self._info = info
        self._device = device
        self._compute_type = compute_type
        self._directory = models.model_dir(model_id)
        self._tokenizer = _load_tokenizer(self._directory, model_id)
        self._translator = _load_ct2_translator(self._directory, device, compute_type)

    def translate(
        self,
        sentences: list[Sentence],
        target: str,
        source: str | None = None,
        on_progress: ProgressFn | None = None,
        on_batch: BatchFn | None = None,
    ) -> list[Sentence]:
        from . import cuda

        check_request(self._info.id, target, source)
        # Empty lines are not sent to the model; they come back textless with their times and ids preserved.
        result_sentences = [
            Sentence(id=s.id, start=s.start, end=s.end, text=s.text, words=[], speaker=s.speaker)
            for s in sentences
        ]
        pending = [(index, sentence) for index, sentence in enumerate(sentences) if sentence.text.strip()]
        if not pending:
            if on_progress is not None:
                on_progress(1.0)
            return result_sentences

        prefix = self._target_prefix(target)
        processed = 0
        for start in range(0, len(pending), BATCH_SIZE):
            batch = pending[start : start + BATCH_SIZE]
            sources = [
                self._source_tokens(sentence.text.strip(), source, target) for _, sentence in batch
            ]
            target_prefix = [[prefix] for _ in batch] if prefix is not None else None
            try:
                results = self._translator.translate_batch(
                    sources,
                    target_prefix=target_prefix,
                    beam_size=BEAM_SIZE,
                    max_decoding_length=MAX_DECODING_LENGTH,
                    max_batch_size=BATCH_SIZE,
                )
            except RuntimeError as exc:
                if self._device != "cuda" or not cuda.is_cuda_library_error(exc):
                    raise
                print(f"CUDA unavailable, falling back to CPU: {exc}", file=sys.stderr)
                self._device = "cpu"
                self._translator = _load_ct2_translator(self._directory, "cpu", self._compute_type)
                results = self._translator.translate_batch(
                    sources,
                    target_prefix=target_prefix,
                    beam_size=BEAM_SIZE,
                    max_decoding_length=MAX_DECODING_LENGTH,
                    max_batch_size=BATCH_SIZE,
                )
            for (index, sentence), result in zip(batch, results):
                hypothesis = result.hypotheses[0] if result.hypotheses else []
                text = self._decode(hypothesis, target)
                result_sentences[index] = Sentence(
                    id=sentence.id,
                    start=sentence.start,
                    end=sentence.end,
                    text=text or sentence.text,
                    words=[],
                    speaker=sentence.speaker,
                )
            processed += len(batch)
            if on_batch is not None:
                last_index = batch[-1][0]
                on_batch(result_sentences[: last_index + 1])
            if on_progress is not None:
                on_progress(min(processed / len(pending), 1.0))
        return result_sentences

    def _target_prefix(self, target: str) -> str | None:
        if self._info.id == "nllb-1.3b":
            return NLLB_LANGUAGE_CODES[target]
        return None  # MADLAD wants the target language tag at the start of the source input

    def _source_tokens(self, text: str, source: str | None, target: str) -> list[str]:
        if self._info.id == "nllb-1.3b":
            return [NLLB_LANGUAGE_CODES[source], *self._tokenizer.encode(text), EOS_TOKEN]
        tagged = self._tokenizer.encode(f"{MADLAD_PREFIX.format(target)} {text}")
        return [*tagged, EOS_TOKEN]

    def _decode(self, hypothesis: list[str], target: str) -> str:
        text = self._tokenizer.decode(hypothesis).strip()
        # If the model emits the target language tag anyway, it must not leak into the subtitle.
        tag = self._target_prefix(target)
        if tag is None:
            tag = MADLAD_PREFIX.format(target)
        if text.startswith(tag):
            text = text[len(tag) :].lstrip(" :,")
        return text


class _HfTokenizer:
    """Hugging Face tokenizer.json (NLLB BPE); transformers/torch are not needed."""

    def __init__(self, path: Path):
        from tokenizers import Tokenizer

        self._tokenizer = Tokenizer.from_file(str(path))

    def encode(self, text: str) -> list[str]:
        return list(self._tokenizer.encode(text, add_special_tokens=False).tokens)

    def decode(self, tokens: list[str]) -> str:
        ids = [self._tokenizer.token_to_id(token) for token in tokens]
        return self._tokenizer.decode(
            [token_id for token_id in ids if token_id is not None], skip_special_tokens=True
        )


class _SentencePieceTokenizer:
    """T5/MADLAD tokenizer; the repo's spiece.model is loaded directly."""

    def __init__(self, path: Path):
        import sentencepiece as spm

        self._processor = spm.SentencePieceProcessor(model_file=str(path))

    def encode(self, text: str) -> list[str]:
        return list(self._processor.encode(text, out_type=str))

    def decode(self, tokens: list[str]) -> str:
        return self._processor.decode([self._processor.piece_to_id(token) for token in tokens])


def _load_tokenizer(directory: Path, model_id: str):
    if model_id == "madlad-3b":
        return _SentencePieceTokenizer(directory / "spiece.model")
    return _HfTokenizer(directory / "tokenizer.json")


def _load_ct2_translator(path: Path, device: str, compute_type: str):
    import ctranslate2

    return ctranslate2.Translator(str(path), device=device, compute_type=compute_type)


# --- Direct Prompt-Based LLM Subtitle Translation (Gemma 4 / Qwen 3.5) ---------


def build_system_prompt(target: str, source: str | None = None) -> str:
    """Builds the direct system prompt for subtitle translation, including dialect/slang guidance."""
    target_desc = LLM_LANGUAGE_SPECS.get(target, target)
    source_clause = (
        f"from {LLM_LANGUAGE_SPECS.get(source, source)} "
        if source
        else (
            "from the source dialogue (including regional vernacular such as Brazilian Portuguese "
            "or Mexican/Latin American Spanish) "
        )
    )
    return (
        f"You are an uncensored, native-level film and video subtitle translator. Translate the numbered subtitle lines "
        f"{source_clause}into {target_desc}.\n"
        "Rules:\n"
        "1. Output ONLY the translated numbered lines in the exact format `[N] translated text`.\n"
        "2. Keep a strict 1-to-1 correspondence with the input line numbers. Never merge, split, skip, or reorder lines.\n"
        "3. Use completely natural, everyday spoken language as real native speakers actually talk. Avoid stiff, robotic, "
        "or literal word-for-word translations.\n"
        "4. NEVER censor, soften, sanitize, or tone down profanity, swear words, insults, or street slang. If the source line "
        "contains slang, argot, or explicit swearing (e.g. Brazilian Portuguese, Mexican Spanish, English, or French street "
        "profanity), translate it into equivalent raw, authentic target-language slang/profanity with the exact same emotional intensity.\n"
        "5. Preserve proper names and numbers. Keep each line concise and readable at subtitle pace. "
        "Do not add explanations, notes, or markdown."
    )


def build_user_prompt(lines: list[str], context_lines: list[str] | None = None) -> str:
    """Formats a batch of numbered subtitle lines with optional preceding dialogue context."""
    parts: list[str] = []
    if context_lines:
        ctx = "\n".join(f"- {c}" for c in context_lines if c.strip())
        if ctx:
            parts.append(f"Previous lines (for context only, do NOT translate):\n{ctx}\n")
    numbered = "\n".join(f"[{i}] {text}" for i, text in enumerate(lines, start=1))
    parts.append(f"Translate these subtitle lines:\n{numbered}")
    return "\n".join(parts)


def parse_numbered_translations(raw_output: str, expected_count: int) -> dict[int, str]:
    """Extracts 1-based `[N] text` lines from the LLM output, stripping any reasoning blocks."""
    cleaned = _THINK_BLOCK_RE.sub("", raw_output)
    cleaned = _UNCLOSED_THINK_RE.sub("", cleaned).strip()

    parsed: dict[int, str] = {}
    current_idx: int | None = None
    for raw_line in cleaned.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("```"):
            continue
        match = _NUMBERED_LINE_RE.match(line)
        if match:
            num_str = match.group(1) or match.group(2)
            idx = int(num_str)
            text = match.group(3).strip()
            if 1 <= idx <= expected_count:
                parsed[idx] = text
                current_idx = idx
        elif current_idx is not None and current_idx in parsed:
            parsed[current_idx] = f"{parsed[current_idx]} {line}".strip()

    if not parsed and expected_count > 0:
        non_empty = [ln.strip() for ln in cleaned.splitlines() if ln.strip() and not ln.strip().startswith("```")]
        if len(non_empty) == expected_count:
            for i, text in enumerate(non_empty, start=1):
                parsed[i] = text
        elif expected_count == 1 and non_empty:
            parsed[1] = " ".join(non_empty)

    return parsed


def _free_local_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _temp_checkpoint_path(model_id: str, target: str, source: str | None, sentences: list[Sentence]) -> Path:
    """Path in `%TEMP%/SubtitleAdder/translations/` where translated chunks are saved incrementally."""
    digest = hashlib.sha256()
    digest.update(f"{model_id}|{source or ''}|{target}\n".encode("utf-8"))
    for s in sentences:
        digest.update(f"{s.id}:{s.text.strip()}\n".encode("utf-8"))
    folder = Path(tempfile.gettempdir()) / "SubtitleAdder" / "translations"
    folder.mkdir(parents=True, exist_ok=True)
    return folder / f"chunk-{digest.hexdigest()[:20]}.json"


def _load_temp_checkpoint(path: Path) -> dict[int, str]:
    try:
        if not path.is_file():
            return {}
        raw = json.loads(path.read_text(encoding="utf-8"))
        return {int(k): str(v) for k, v in raw.items() if str(v).strip()}
    except Exception:
        return {}


def _save_temp_checkpoint(path: Path, cache: dict[int, str]) -> None:
    with contextlib.suppress(Exception):
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(cache, ensure_ascii=False), encoding="utf-8")
        tmp.replace(path)


def _delete_temp_checkpoint(path: Path | None) -> None:
    if path is None:
        return
    with contextlib.suppress(Exception):
        path.unlink(missing_ok=True)
    with contextlib.suppress(Exception):
        path.with_suffix(".json.tmp").unlink(missing_ok=True)


class LlamaTranslator:
    """Runs GGUF instruction-tuned LLMs (Gemma 4, Qwen 3.5) locally via `llama-server`."""

    def __init__(
        self,
        model_id: str,
        device: str = "auto",
        completion_fn: Callable[[list[dict[str, str]]], str] | None = None,
    ):
        from . import llama

        info = models.find(model_id)
        if info is None or info.kind != "translation" or info.backend != "llama":
            raise ModelError(t("unknown_translation_model_short", model=model_id))
        self._info = info
        self._device = device
        self._completion_fn = completion_fn
        self._process: subprocess.Popen | None = None
        self._stderr_file = None
        self._port: int | None = None
        if completion_fn is None:
            self._model_path = models.model_file(model_id)
            server = llama.llama_server_path()
            if server is None:
                raise ModelError(t("offline_missing_llama"))
            self._server_path = server
            self._start_server(gpu_layers=0 if device == "cpu" else 99)
            atexit.register(self.close)

    def _start_server(self, gpu_layers: int) -> None:
        self.close()
        port = _free_local_port()
        cmd = [
            str(self._server_path),
            "-m",
            str(self._model_path),
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "-ngl",
            str(gpu_layers),
            "-fa",
            "on",
            "-np",
            "1",
            "-c",
            "2048",
            "--reasoning-budget",
            "0",
        ]
        creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0) if sys.platform == "win32" else 0
        try:
            # Never use subprocess.PIPE for stderr without a reader thread: on Windows its 4 KB pipe
            # buffer fills up after ~6-8 translation batches and deadlocks llama-server mid-run.
            stderr_file = tempfile.TemporaryFile(mode="w+", encoding="utf-8", errors="replace")
            proc = subprocess.Popen(
                cmd,
                stdout=subprocess.DEVNULL,
                stderr=stderr_file,
                cwd=str(self._server_path.parent),
                creationflags=creationflags,
            )
        except OSError as exc:
            raise ModelError(t("llama_unavailable", error=str(exc))) from exc

        self._process = proc
        self._stderr_file = stderr_file
        self._port = port
        health_url = f"http://127.0.0.1:{port}/health"
        deadline = time.monotonic() + 90.0
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                err_tail = ""
                if self._stderr_file is not None:
                    with contextlib.suppress(Exception):
                        self._stderr_file.seek(0)
                        err_tail = (self._stderr_file.read() or "").strip()[-400:]
                self.close()
                if gpu_layers > 0 and self._device == "auto":
                    print("llama-server GPU init failed, retrying on CPU...", file=sys.stderr)
                    self._start_server(gpu_layers=0)
                    return
                raise ModelError(t("llama_unavailable", error=err_tail or f"exit code {proc.returncode}"))
            try:
                with urllib.request.urlopen(health_url, timeout=1.0) as resp:
                    if resp.status == 200:
                        return
            except (urllib.error.URLError, TimeoutError, OSError):
                time.sleep(0.15)

        self.close()
        raise ModelError(t("llama_unavailable", error="timed out waiting for llama-server /health"))

    def close(self) -> None:
        proc = self._process
        stderr_file = self._stderr_file
        self._process = None
        self._stderr_file = None
        self._port = None
        if proc is not None:
            with contextlib.suppress(Exception):
                if proc.poll() is None:
                    proc.terminate()
                    try:
                        proc.wait(timeout=3.0)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait(timeout=2.0)
        if stderr_file is not None:
            with contextlib.suppress(Exception):
                stderr_file.close()

    def __del__(self) -> None:
        with contextlib.suppress(Exception):
            self.close()

    def _complete(self, messages: list[dict[str, str]]) -> str:
        if self._completion_fn is not None:
            return self._completion_fn(messages)
        if self._process is None or self._process.poll() is not None:
            self._start_server(gpu_layers=0 if self._device == "cpu" else 99)
        url = f"http://127.0.0.1:{self._port}/v1/chat/completions"
        payload = json.dumps(
            {
                "messages": messages,
                "temperature": 0.2,
                "top_p": 0.9,
                "max_tokens": 512,
                "cache_prompt": False,
                "chat_template_kwargs": {"enable_thinking": False},
            }
        ).encode("utf-8")
        req = urllib.request.Request(
            url,
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=300.0) as resp:
                data = json.loads(resp.read().decode("utf-8"))
        except Exception as exc:
            raise ModelError(t("llama_unavailable", error=str(exc))) from exc
        choices = data.get("choices") or []
        if not choices:
            return ""
        msg = choices[0].get("message") or {}
        return str(msg.get("content") or "")

    def translate(
        self,
        sentences: list[Sentence],
        target: str,
        source: str | None = None,
        on_progress: ProgressFn | None = None,
        on_batch: BatchFn | None = None,
    ) -> list[Sentence]:
        check_request(self._info.id, target, source)
        result_sentences = [
            Sentence(id=s.id, start=s.start, end=s.end, text=s.text, words=[], speaker=s.speaker)
            for s in sentences
        ]
        pending = [(index, sentence) for index, sentence in enumerate(sentences) if sentence.text.strip()]
        if not pending:
            if on_progress is not None:
                on_progress(1.0)
            return result_sentences

        system_prompt = build_system_prompt(target=target, source=source)
        checkpoint_path = (
            _temp_checkpoint_path(self._info.id, target, source, sentences)
            if self._completion_fn is None
            else None
        )
        cached_chunks = _load_temp_checkpoint(checkpoint_path) if checkpoint_path is not None else {}

        processed = 0
        for start in range(0, len(pending), LLM_BATCH_SIZE):
            batch = pending[start : start + LLM_BATCH_SIZE]
            if cached_chunks and all(sentence.id in cached_chunks for _, sentence in batch):
                for index, sentence in batch:
                    result_sentences[index] = Sentence(
                        id=sentence.id,
                        start=sentence.start,
                        end=sentence.end,
                        text=cached_chunks[sentence.id],
                        words=[],
                        speaker=sentence.speaker,
                    )
            else:
                ctx_start = max(0, start - LLM_CONTEXT_LINES)
                context_lines = [s.text.strip() for _, s in pending[ctx_start:start]]
                batch_texts = [s.text.strip() for _, s in batch]
                messages = [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": build_user_prompt(batch_texts, context_lines)},
                ]
                raw_reply = self._complete(messages)
                parsed = parse_numbered_translations(raw_reply, len(batch))

                for offset, (index, sentence) in enumerate(batch, start=1):
                    text = parsed.get(offset, "").strip()
                    if not text and len(batch) > 1:
                        # Single-line retry if the model omitted one numbered line in the batch.
                        single_messages = [
                            {"role": "system", "content": system_prompt},
                            {"role": "user", "content": build_user_prompt([sentence.text.strip()])},
                        ]
                        single_parsed = parse_numbered_translations(self._complete(single_messages), 1)
                        text = single_parsed.get(1, "").strip()
                    final_text = text or sentence.text
                    result_sentences[index] = Sentence(
                        id=sentence.id,
                        start=sentence.start,
                        end=sentence.end,
                        text=final_text,
                        words=[],
                        speaker=sentence.speaker,
                    )
                    if text:
                        cached_chunks[sentence.id] = final_text
                if checkpoint_path is not None:
                    _save_temp_checkpoint(checkpoint_path, cached_chunks)

            processed += len(batch)
            if on_batch is not None:
                last_index = batch[-1][0]
                on_batch(result_sentences[: last_index + 1])
            if on_progress is not None:
                on_progress(min(processed / len(pending), 1.0))
        _delete_temp_checkpoint(checkpoint_path)
        return result_sentences
