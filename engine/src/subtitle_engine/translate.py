"""Step: translates sentences into the target language with fully local translation models.

The models are in CTranslate2 format. MADLAD's T5 tokenizer is read with sentencepiece,
NLLB's BPE tokenizer from the Hugging Face `tokenizer.json` file (`tokenizers`,
Rust); transformers/torch are not needed. The source text never leaves the machine;
the network is only used to download models.
"""
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Protocol

from . import models
from .i18n import t
from .models import ModelError, Sentence

ProgressFn = Callable[[float], None]

# NLLB source/target language codes use the FLORES-200 format; MADLAD turns the ISO 639-1 code
# into a "<2xx>" token and needs no source language tag.
NLLB_LANGUAGE_CODES = {
    "tr": "tur_Latn",
    "pt": "por_Latn",
    "es": "spa_Latn",
    "ru": "rus_Cyrl",
    "en": "eng_Latn",
}
MADLAD_PREFIX = "<2{}>"

# Sentences are translated in batches of this size, not one by one; progress is reported after each batch.
BATCH_SIZE = 16
BEAM_SIZE = 4
MAX_DECODING_LENGTH = 256
# add_source_eos is off in CT2 models; we append it to the end of the source like HF tokenizers do.
EOS_TOKEN = "</s>"


class Translator(Protocol):
    def translate(
        self,
        sentences: list[Sentence],
        target: str,
        source: str | None = None,
        on_progress: ProgressFn | None = None,
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
    """Builds the CTranslate2 translator for a catalog id (GPU when CUDA is usable, otherwise CPU)."""
    info = models.find(model_id)
    if info is None or info.kind != "translation":
        raise ModelError(
            t("unknown_translation_model", model=model_id)
        )
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
