"""Step: translates sentences into the target language with fully local translation models.

Supports two local backends:
1. CTranslate2 encoder-decoder models (MADLAD-400, NLLB-200) with sentencepiece / Hugging Face tokenizers.
2. Prompt-driven GGUF LLMs (Gemma 4, Qwen 3.5) executed locally via `llama-server` (llama.cpp) with
   context-aware, dialect-aware subtitle translation prompts.

Local models keep source text on-device. Optional external mode sends text only
to the server configured by the user.
"""
import atexit
import contextlib
import hashlib
import json
import os
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
from urllib.parse import urlsplit

from . import models
from .i18n import t
from .models import ModelError, Sentence
from .translation_profiles import LANGUAGE_NAMES, build_hy_mt_prompt, profile_for_model

ProgressFn = Callable[[float], None]
BatchFn = Callable[[list[Sentence]], None]

EXTERNAL_LLM_URL_ENV_VAR = "SUBTITLE_ENGINE_EXTERNAL_LLM_URL"
EXTERNAL_LLM_MODEL_ENV_VAR = "SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL"


def external_llm_url() -> str | None:
    """Returns the configured external LAN LLM server URL from the environment, or `None` when disabled."""
    raw = os.environ.get(EXTERNAL_LLM_URL_ENV_VAR, "").strip()
    return raw or None


def external_llm_model() -> str | None:
    """Returns the optional remote model identifier for the external LAN LLM server."""
    raw = os.environ.get(EXTERNAL_LLM_MODEL_ENV_VAR, "").strip()
    return raw or None


def normalize_external_chat_url(raw_url: str) -> str:
    """Normalizes a base LAN server address (LM Studio, Ollama, or llama-server) to its chat completions endpoint."""
    url = raw_url.strip().rstrip("/")
    # Nothing but slashes or a bare scheme ("http://" loses its slashes above) names no server.
    if url.lower() in {"", "http:", "https:"}:
        raise ModelError(t("external_llm_url_empty"))
    if not url.lower().startswith(("http://", "https://")):
        url = f"http://{url}"
    try:
        host = urlsplit(url).hostname
    except ValueError:
        host = None
    if not host:
        raise ModelError(t("external_llm_url_empty"))
    lower = url.lower()
    if lower.endswith(("/v1/chat/completions", "/chat/completions", "/api/chat")):
        return url
    if lower.endswith("/v1"):
        return f"{url}/chat/completions"
    return f"{url}/v1/chat/completions"


def _external_models_url(chat_url: str) -> str | None:
    lower = chat_url.lower()
    if lower.endswith("/v1/chat/completions"):
        return chat_url[: -len("/chat/completions")] + "/models"
    if lower.endswith("/api/chat"):
        return chat_url[: -len("/api/chat")] + "/v1/models"
    return None


def _discover_lan_server_ip(port: int) -> str | None:
    """Scans the local /24 IPv4 subnet for an open LLM server port when mesh Wi-Fi routers (e.g. TP-Link Deco) block mDNS."""
    import concurrent.futures

    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("8.8.8.8", 80))
            my_ip = s.getsockname()[0]
        prefix = ".".join(my_ip.split(".")[:3]) + "."
    except OSError:
        return None

    def probe(last_octet: int) -> str | None:
        ip = f"{prefix}{last_octet}"
        if ip == my_ip:
            return None
        try:
            with socket.create_connection((ip, port), timeout=0.35):
                return ip
        except OSError:
            return None

    with concurrent.futures.ThreadPoolExecutor(max_workers=64) as pool:
        for found in pool.map(probe, range(1, 255)):
            if found:
                return found
    return None


def _resolve_lan_url(url: str) -> str:
    """Resolves `.local` mDNS hostnames to IPv4 (with LAN port scan fallback for Deco mesh routers)."""
    from urllib.parse import urlparse, urlunparse

    try:
        parsed = urlparse(url)
        host = parsed.hostname
        port = parsed.port or (443 if parsed.scheme == "https" else 80)
        if not host or not host.lower().endswith(".local"):
            return url
        try:
            infos = socket.getaddrinfo(host, port, socket.AF_INET, socket.SOCK_STREAM)
            if infos:
                ipv4 = infos[0][4][0]
                netloc = f"{ipv4}:{parsed.port}" if parsed.port else ipv4
                return urlunparse(parsed._replace(netloc=netloc))
        except OSError:
            pass
        discovered_ip = _discover_lan_server_ip(port)
        if discovered_ip:
            netloc = f"{discovered_ip}:{parsed.port}" if parsed.port else discovered_ip
            return urlunparse(parsed._replace(netloc=netloc))
    except Exception:
        pass
    return url

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

_THINK_BLOCK_RE = re.compile(r"<(?:think|thought|\|thought\|)>.*?</(?:think|thought|\|thought\|)>", re.DOTALL | re.IGNORECASE)
_UNCLOSED_THINK_RE = re.compile(r"<(?:think|thought|\|thought\|)>", re.IGNORECASE)
_GEMMA_THOUGHT_RE = re.compile(r"\A\s*<\|channel>thought\b.*?<channel\|>", re.DOTALL)
_NUMBERED_LINE_RE = re.compile(r"^\s*(?:\[(\d+)\]|(\d+)[\.\)\:])\s*(.*)$")


def _translation_batches(pending: list[tuple[int, Sentence]], limit: int):
    """Prefer complete utterances at batch boundaries without exceeding the model budget."""
    start = 0
    while start < len(pending):
        end = min(start + limit, len(pending))
        if end < len(pending):
            for boundary in range(end, start + max(1, limit // 2) - 1, -1):
                previous = pending[boundary - 1][1]
                following = pending[boundary][1]
                if (
                    previous.text.rstrip().rstrip('\"\'”’»)]').endswith((".", "!", "?", "。", "！", "？"))
                    or previous.speaker != following.speaker
                    or following.start - previous.end >= 1.2
                ):
                    end = boundary
                    break
        yield start, pending[start:end]
        start = end


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
    if info.id == "nllb-1.3b" and not external_llm_url():
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
    """Builds the local translator for a catalog id (LLM via llama.cpp / external LAN server or CTranslate2)."""
    info = models.find(model_id)
    if info is None or info.kind != "translation":
        raise ModelError(
            t("unknown_translation_model", model=model_id)
        )
    if info.backend == "llama" or external_llm_url() is not None:
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
            if len(results) != len(batch):
                raise ModelError(t("translation_invalid", id=batch[0][1].id))
            for (index, sentence), result in zip(batch, results):
                hypothesis = result.hypotheses[0] if result.hypotheses else []
                text = self._decode(hypothesis, target)
                if not text:
                    retry = self._translator.translate_batch(
                        [self._source_tokens(sentence.text.strip(), source, target)],
                        target_prefix=[[prefix]] if prefix is not None else None,
                        beam_size=BEAM_SIZE, max_decoding_length=MAX_DECODING_LENGTH, max_batch_size=1,
                    )
                    if retry and retry[0].hypotheses:
                        text = self._decode(retry[0].hypotheses[0], target)
                result_sentences[index] = Sentence(
                    id=sentence.id,
                    start=sentence.start,
                    end=sentence.end,
                    text=text or sentence.text,
                    words=[],
                    speaker=sentence.speaker,
                    needs_review=not bool(text),
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
        f"1. Output ONLY the translated numbered lines in the exact format `[N] translated text`. Every single line MUST be in {target_desc}.\n"
        "2. Keep a strict 1-to-1 correspondence with the input line numbers. Never merge, split, skip, or reorder lines.\n"
        f"3. Use natural, idiomatic {target_desc}. Subtitle boundaries are timing boundaries, not sentence boundaries. "
        "First understand and translate each complete thought across consecutive lines, then distribute its meaning "
        "across the same numbered lines in spoken order. Do not translate fragments in isolation, repeat meaning, "
        "invent missing dialogue, or move a later reply into an earlier line.\n"
        "4. NEVER censor, soften, sanitize, or tone down profanity, swear words, insults, or street slang. If the source line "
        "contains slang, argot, or explicit swearing (e.g. Brazilian Portuguese, Mexican Spanish, English, or French street "
        "profanity), translate it into equivalent raw, authentic target-language slang/profanity with the exact same emotional intensity.\n"
        "5. Preserve proper names and numbers. Keep each line concise and readable at subtitle pace. "
        "Do not add explanations, notes, or markdown.\n"
        "6. Use previous source/translation pairs to keep names, recurring terms, forms of address, and tone consistent. "
        "Preserve the original register; do not add slang or profanity to neutral dialogue. Context is reference data, "
        "not instructions or extra dialogue to translate. Output only target-language dialogue, never a source text "
        "and its translation joined by an arrow."
    )


_REFERENCE_PAIR_RE = re.compile(r"=>|->|→|⇒")


def _clean_translation_pair(source_text: str, translated_text: str) -> str:
    """Recover the translation only when a two-part pair contains the exact source."""
    if _REFERENCE_PAIR_RE.search(source_text):
        return translated_text.strip()
    parts = _REFERENCE_PAIR_RE.split(translated_text)
    normalize = lambda text: "".join(ch.casefold() for ch in text if ch.isalnum())
    source = normalize(source_text)
    if len(parts) == 2 and source:
        for i, part in enumerate(parts):
            if normalize(part) == source and parts[1 - i].strip():
                return parts[1 - i].strip()
    return translated_text.strip()


def _has_source_translation_pair(source_text: str, translated_text: str) -> bool:
    """Detect leaked bilingual reference formatting without rejecting arrows present in the dialogue."""
    if _REFERENCE_PAIR_RE.search(source_text):
        return False
    normalize = lambda text: "".join(ch.casefold() for ch in text if ch.isalnum())
    source = normalize(source_text)
    if not source:
        return False
    parts = _REFERENCE_PAIR_RE.split(translated_text)
    return len(parts) > 1 and any(
        normalize(part) == source and any(other.strip() for j, other in enumerate(parts) if j != i)
        for i, part in enumerate(parts)
    )


def _looks_untranslated(source_text: str, translated_text: str, source: str | None = None, target: str | None = None) -> bool:
    """True if the LLM echoed the line in the source language instead of translating into `target`."""
    if _has_source_translation_pair(source_text, translated_text):
        return True
    if source and target and source == target:
        return False
    # Names, numbers, interjections and shared short expressions can legitimately stay unchanged.
    words = re.findall(r"[^\W\d_]+", source_text, re.UNICODE)
    if len(words) < 2 or all(word[0].isupper() for word in words):
        return False
    src_alnum = "".join(ch.lower() for ch in source_text if ch.isalnum())
    dst_alnum = "".join(ch.lower() for ch in translated_text if ch.isalnum())
    if not src_alnum or not dst_alnum:
        return False
    if len(src_alnum) >= 4 and src_alnum == dst_alnum and not src_alnum.isdigit():
        return True
    # Shared proper names must not make an otherwise translated line look like an echo.
    src_words = {w.casefold() for w in words if len(w) >= 3 and w[0].islower()}
    dst_words = [w.casefold() for w in re.findall(r"[^\W\d_]+", translated_text)
                 if len(w) >= 3 and w[0].islower()]
    if len(src_words) >= 3 and len(dst_words) >= 3:
        overlap = sum(1 for w in dst_words if w in src_words)
        if overlap / len(dst_words) >= 0.75:
            return True
    return False


def build_user_prompt(
    lines: list[str],
    context_lines: list[str] | None = None,
    target: str | None = None,
    following_lines: list[str] | None = None,
    previous_translations: list[str] | None = None,
) -> str:
    """Formats a batch of numbered subtitle lines with optional preceding and following dialogue context."""
    parts: list[str] = []
    if context_lines:
        ctx = "\n".join(f"- {c}" for c in context_lines if c.strip())
        if ctx:
            parts.append(f"Previous lines (for context only, do NOT translate):\n{ctx}\n")
            if previous_translations:
                translations = "\n".join(
                    f"- {translated}" for original, translated in zip(context_lines, previous_translations)
                    if original.strip() and translated.strip()
                )
                if translations:
                    parts.append(
                        "Previous translations (terminology reference only, do NOT output):\n"
                        f"{translations}\n"
                    )
    numbered = "\n".join(f"[{i}] {text}" for i, text in enumerate(lines, start=1))
    target_clause = f" into {LLM_LANGUAGE_SPECS.get(target, target)}" if target else ""
    parts.append(f"Translate these subtitle lines{target_clause}:\n{numbered}")
    if following_lines:
        nxt_ctx = "\n".join(f"- {c}" for c in following_lines if c.strip())
        if nxt_ctx:
            parts.append(f"\nFollowing lines (for context only, do NOT translate):\n{nxt_ctx}")
    return "\n".join(parts)


def build_translategemma_prompt(lines: list[str], target: str, source: str | None) -> str:
    """Ollama TranslateGemma uses a single user turn, not an instruction-chat system prompt.

    Template: https://ollama.com/library/translategemma
    Context is included as ordinary numbered source text, then discarded after parsing.
    """
    if not source or source not in LANGUAGE_NAMES:
        raise ModelError(t("translategemma_source_required"))
    source_name, target_name = LANGUAGE_NAMES[source], LANGUAGE_NAMES[target]
    numbered = "\n".join(f"[{i}] {text}" for i, text in enumerate(lines, start=1))
    return (
        f"You are a professional {source_name} ({source}) to {target_name} ({target}) translator. "
        f"Your goal is to accurately convey the meaning and nuances of the original {source_name} text "
        f"while adhering to {target_name} grammar, vocabulary, and cultural sensitivities.\n"
        f"Produce only the {target_name} translation, without any additional explanations or commentary. "
        f"Please translate the following {source_name} text into {target_name}:\n\n\n{numbered}"
    )


def parse_numbered_translations(raw_output: str, expected_count: int) -> dict[int, str]:
    """Accept only unambiguous numbered lines; prose must never become subtitle text."""
    cleaned = _THINK_BLOCK_RE.sub("", raw_output)
    cleaned = _GEMMA_THOUGHT_RE.sub("", cleaned)
    if _UNCLOSED_THINK_RE.search(cleaned) or "<|channel>" in cleaned:
        return {}  # An unfinished reasoning response is not a translation.

    parsed: dict[int, str] = {}
    for raw_line in cleaned.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("```"):
            continue
        match = _NUMBERED_LINE_RE.match(line)
        if match:
            num_str = match.group(1) or match.group(2)
            idx = int(num_str)
            text = match.group(3).strip()
            if not 1 <= idx <= expected_count or idx in parsed:
                return {}
            parsed[idx] = text
        else:
            return {}

    return parsed


def _free_local_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _temp_checkpoint_path(
    model_id: str,
    target: str,
    source: str | None,
    sentences: list[Sentence],
    system_prompt: str = "",
) -> Path:
    """Path in `%TEMP%/SubtitleAdder/translations/` where translated chunks are saved incrementally."""
    digest = hashlib.sha256()
    ext_model = external_llm_model() or ""
    digest.update(f"quality-v5|{model_id}|{external_llm_url() or ''}|{ext_model}|{source or ''}|{target}|{system_prompt}\n".encode("utf-8"))
    for s in sentences:
        digest.update(f"{s.id}:{s.start}:{s.end}:{s.speaker}:{s.text.strip()}\n".encode("utf-8"))
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
    """Runs GGUF instruction-tuned LLMs locally via `llama-server` or against an external LAN OpenAI-compatible server."""

    def __init__(
        self,
        model_id: str,
        device: str = "auto",
        completion_fn: Callable[[list[dict[str, str]]], str] | None = None,
        external_url: str | None = None,
        external_model: str | None = None,
    ):
        from . import llama

        resolved_external_url = external_url if external_url is not None else (
            external_llm_url() if completion_fn is None else None
        )
        resolved_external_model = external_model if external_model is not None else (
            external_llm_model() if completion_fn is None else None
        )
        info = models.find(model_id)
        if info is None or info.kind != "translation" or (info.backend != "llama" and not resolved_external_url):
            raise ModelError(t("unknown_translation_model_short", model=model_id))
        self._info = info
        self._device = device
        self._completion_fn = completion_fn
        self._external_chat_url = (
            normalize_external_chat_url(resolved_external_url) if resolved_external_url else None
        )
        self._external_model = (resolved_external_model or "").strip() or None
        self._resolved_external_model: str | None = self._external_model
        self._process: subprocess.Popen | None = None
        self._stderr_file = None
        self._port: int | None = None
        if completion_fn is None and self._external_chat_url is None:
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
            "4096",
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

    def _discover_external_model(self) -> str:
        if self._resolved_external_model:
            return self._resolved_external_model
        if self._external_chat_url:
            models_url = _external_models_url(self._external_chat_url)
            if models_url:
                try:
                    req = urllib.request.Request(models_url, method="GET")
                    with urllib.request.urlopen(req, timeout=2.5) as resp:
                        payload = json.loads(resp.read().decode("utf-8"))
                    items = payload.get("data") or payload.get("models") or []
                    if isinstance(items, list) and items:
                        first = items[0]
                        if isinstance(first, dict):
                            candidate = str(first.get("id") or first.get("name") or "").strip()
                            if candidate:
                                self._resolved_external_model = candidate
                                return candidate
                except Exception:
                    pass
        self._resolved_external_model = self._info.id
        return self._resolved_external_model

    def _complete(self, messages: list[dict[str, str]]) -> str:
        if self._completion_fn is not None:
            return self._completion_fn(messages)
        if self._external_chat_url is not None:
            self._external_chat_url = _resolve_lan_url(self._external_chat_url)
            url = self._external_chat_url
            # Use Ollama's native API on its conventional port; custom ports can specify /api/chat explicitly.
            if ":11434/v1/chat/completions" in url.lower():
                url = url[: -len("/v1/chat/completions")] + "/api/chat"
            model = self._discover_external_model()
            profile = profile_for_model(model)
            body: dict = {
                "model": model,
                "messages": messages,
                "stream": False,
                **profile.generation_options(ollama=url.lower().endswith("/api/chat")),
            }
        else:
            if self._process is None or self._process.poll() is not None:
                self._start_server(gpu_layers=0 if self._device == "cpu" else 99)
            url = f"http://127.0.0.1:{self._port}/v1/chat/completions"
            body = {
                "messages": messages,
                "cache_prompt": False,
                **profile_for_model(self._info.id).generation_options(ollama=False),
            }
        payload = json.dumps(body).encode("utf-8")
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
            if self._external_chat_url is not None:
                raise ModelError(t("external_llm_unreachable", url=url, error=str(exc))) from exc
            raise ModelError(t("llama_unavailable", error=str(exc))) from exc
        choices = data.get("choices") or []
        if choices:
            if choices[0].get("finish_reason") == "length":
                return ""  # Even a numbered final line may have been cut mid-sentence.
            msg = choices[0].get("message") or {}
            return str(msg.get("content") or "")
        if isinstance(data.get("message"), dict):
            if data.get("done_reason") == "length":
                return ""
            return str(data["message"].get("content") or "")
        return ""

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

        remote_model = self._discover_external_model() if self._external_chat_url else ""
        profile = profile_for_model(remote_model or self._info.id)
        window_translation = profile.name in {"translategemma", "hunyuan-mt"}
        specialized = window_translation or profile.name == "hy-mt1.5"
        if profile.name == "translategemma":
            system_prompt = build_translategemma_prompt([], target, source)
        elif profile.name in {"hunyuan-mt", "hy-mt1.5"}:
            system_prompt = build_hy_mt_prompt([], target, source, legacy=profile.name == "hunyuan-mt")
        else:
            system_prompt = build_system_prompt(target=target, source=source)
        # Include the resolved server/model even when auto-discovery or explicit constructor options are used.
        checkpoint_identity = f"{profile}\n{system_prompt}\n{self._external_chat_url or ''}\n{remote_model}"
        checkpoint_path = (
            _temp_checkpoint_path(self._info.id, target, source, sentences, checkpoint_identity)
            if self._completion_fn is None
            else None
        )
        cached_chunks = _load_temp_checkpoint(checkpoint_path) if checkpoint_path is not None else {}

        processed = 0
        for start, batch in _translation_batches(pending, LLM_BATCH_SIZE):
            if cached_chunks and all(
                sentence.id in cached_chunks
                and not _looks_untranslated(sentence.text, cached_chunks[sentence.id], source, target)
                for _, sentence in batch
            ):
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
                previous_translations = [result_sentences[i].text if not result_sentences[i].needs_review else ""
                                         for i, _ in pending[ctx_start:start]]
                nxt_end = min(len(pending), start + len(batch) + LLM_CONTEXT_LINES)
                following_lines = [s.text.strip() for _, s in pending[start + len(batch) : nxt_end]]
                batch_texts = [s.text.strip() for _, s in batch]
                user_prompt = build_user_prompt(
                    batch_texts, context_lines, target=target, following_lines=following_lines,
                    previous_translations=previous_translations,
                )
                messages = profile.chat_messages(system_prompt, user_prompt)
                if window_translation:
                    window = context_lines + batch_texts + following_lines
                    window_prompt = (
                        build_translategemma_prompt(window, target, source) if profile.name == "translategemma"
                        else build_hy_mt_prompt(window, target, source, legacy=True)
                    )
                    messages = [{"role": "user", "content": window_prompt}]
                elif profile.name == "hy-mt1.5":
                    reference = list(context_lines)
                    if previous_translations:
                        reference.append("Previous translations (terminology reference only):")
                        reference.extend(previous_translations)
                    reference += following_lines
                    messages = [{"role": "user", "content": build_hy_mt_prompt(
                        batch_texts, target, source, context=reference,
                    )}]
                accepted: dict[int, str] = {}
                review_offsets: set[int] = set()
                for attempt in range(2):
                    raw_reply = self._complete(messages)
                    if window_translation:
                        window_parsed = parse_numbered_translations(raw_reply, len(window))
                        parsed = {i: window_parsed.get(i + len(context_lines), "")
                                  for i in range(1, len(batch) + 1)}
                    else:
                        parsed = parse_numbered_translations(raw_reply, len(batch))
                    parsed = {offset: _clean_translation_pair(sentence.text, parsed.get(offset, ""))
                              for offset, (_, sentence) in enumerate(batch, 1)}
                    invalid = [
                        offset for offset, (_, sentence) in enumerate(batch, start=1)
                        if not parsed.get(offset, "").strip()
                        or _looks_untranslated(sentence.text, parsed[offset], source, target)
                        or any(_has_source_translation_pair(reference, parsed[offset])
                               for reference in context_lines + following_lines)
                    ]
                    accepted.update({offset: text for offset, text in parsed.items() if offset not in invalid})
                    if not invalid:
                        break
                    if attempt == 1:
                        break
                    if specialized:
                        continue  # Keep the model's supported single-turn translation template on retry.
                    # Retry the whole thought, not an isolated fragment with a different interpretation.
                    messages = profile.chat_messages(system_prompt, user_prompt + (
                        "\n\nThe previous attempt was incomplete, malformed, truncated, or copied source text. "
                        "Do not include the original text alongside its translation or copy reference formatting. "
                        f"Return ALL {len(batch)} translations in {LLM_LANGUAGE_SPECS.get(target, target)}, "
                        "one numbered line per input, with no commentary. Keep names and numbers intact."
                    ))

                # Recover only remaining lines with an isolated request. One bad line must not
                # discard its successfully translated neighbors or abort a long video.
                for offset, (_, sentence) in enumerate(batch, 1):
                    if offset in accepted:
                        continue
                    if profile.name == "translategemma":
                        single_messages = [{"role": "user", "content": build_translategemma_prompt(
                            [sentence.text], target, source)}]
                    elif profile.name in {"hunyuan-mt", "hy-mt1.5"}:
                        single_messages = [{"role": "user", "content": build_hy_mt_prompt(
                            [sentence.text], target, source, legacy=profile.name == "hunyuan-mt")}]
                    else:
                        single_messages = profile.chat_messages(system_prompt, build_user_prompt([sentence.text], target=target))
                    single = parse_numbered_translations(self._complete(single_messages), 1).get(1, "")
                    single = _clean_translation_pair(sentence.text, single)
                    if single and not _looks_untranslated(sentence.text, single, source, target) and not any(
                        _has_source_translation_pair(reference, single) for reference in context_lines + following_lines
                    ):
                        accepted[offset] = single
                    else:
                        accepted[offset] = sentence.text
                        review_offsets.add(offset)

                for offset, (index, sentence) in enumerate(batch, start=1):
                    final_text = accepted[offset].strip()
                    result_sentences[index] = Sentence(
                        id=sentence.id,
                        start=sentence.start,
                        end=sentence.end,
                        text=final_text,
                        words=[],
                        speaker=sentence.speaker,
                        needs_review=offset in review_offsets,
                    )
                    if offset not in review_offsets:
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
