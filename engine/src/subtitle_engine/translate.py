"""Adım: cümleleri tamamen yerel çeviri modelleriyle hedef dile çevirir.

Modeller CTranslate2 biçimindedir. MADLAD'ın T5 tokenizer'ı sentencepiece ile,
NLLB'nin BPE tokenizer'ı Hugging Face `tokenizer.json` dosyasından (`tokenizers`,
Rust) okunur; transformers/torch gerekmez. Kaynak metin hiçbir zaman makineden
çıkmaz; ağ yalnız model indirmek için kullanılır.
"""
from collections.abc import Callable
from pathlib import Path
from typing import Protocol

from . import models
from .models import ModelError, Sentence

ProgressFn = Callable[[float], None]

# NLLB kaynak/hedef dil kodları FLORES-200 biçimindedir; MADLAD ISO 639-1 kodunu
# "<2xx>" token'ına çevirir ve kaynak dil etiketi istemez.
NLLB_LANGUAGE_CODES = {
    "tr": "tur_Latn",
    "pt": "por_Latn",
    "es": "spa_Latn",
    "ru": "rus_Cyrl",
    "en": "eng_Latn",
}
MADLAD_PREFIX = "<2{}>"

# Cümleler tek tek değil, bu boyutta toplu çevrilir; ilerleme her partiden sonra bildirilir.
BATCH_SIZE = 16
BEAM_SIZE = 4
MAX_DECODING_LENGTH = 256
# CT2 modellerinde add_source_eos kapalı; HF tokenizer'ları gibi kaynağın sonuna ekliyoruz.
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
    """İstek katalog ve dil desteğiyle uyumlu mu? Değilse Türkçe `ModelError` yükseltir."""
    info = models.find(model_id)
    if info is None or info.kind != "translation":
        raise ModelError(
            f"Bilinmeyen çeviri modeli: {model_id}. Geçerli kimlikleri 'models' komutu listeler."
        )
    supported = info.languages or ()
    if target not in supported:
        raise ModelError(models.support_error(info, target))
    if info.id == "nllb-1.3b":
        if not source:
            raise ModelError("NLLB için kaynak dil (--from) zorunlu; ör. --from tr.")
        if source not in supported:
            name = models.DISPLAY_NAMES.get(info.id, info.id)
            raise ModelError(
                f"{name} {source} dilinden çeviri yapamıyor. Desteklenen diller: {', '.join(supported)}."
            )
        if source not in NLLB_LANGUAGE_CODES or target not in NLLB_LANGUAGE_CODES:
            raise ModelError(f"NLLB {target} dili için tanımlı değil.")
    # MADLAD kaynak dil etiketi kullanmaz; --from verilse de yok sayılır.
    return info


def create_translator(model_id: str, device: str = "cpu", compute_type: str = "int8") -> Translator:
    """Katalog kimliğine göre CTranslate2 çeviriciyi kurar (CPU'da int8)."""
    info = models.find(model_id)
    if info is None or info.kind != "translation":
        raise ModelError(
            f"Bilinmeyen çeviri modeli: {model_id}. Geçerli kimlikleri 'models' komutu listeler."
        )
    return Ct2Translator(model_id, device=device, compute_type=compute_type)


class Ct2Translator:
    """CTranslate2 T5 (MADLAD) ve M2M100 (NLLB) modellerini tek protokolde çalıştırır."""

    def __init__(self, model_id: str, device: str = "cpu", compute_type: str = "int8"):
        info = models.find(model_id)
        if info is None or info.kind != "translation":
            raise ModelError(f"Bilinmeyen çeviri modeli: {model_id}.")
        self._info = info
        directory = models.model_dir(model_id)
        self._tokenizer = _load_tokenizer(directory, model_id)
        self._translator = _load_ct2_translator(directory, device, compute_type)

    def translate(
        self,
        sentences: list[Sentence],
        target: str,
        source: str | None = None,
        on_progress: ProgressFn | None = None,
    ) -> list[Sentence]:
        check_request(self._info.id, target, source)
        # Boş satırlar modele gitmez; zamanları ve kimlikleri korunarak metinsiz döner.
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
            results = self._translator.translate_batch(
                sources,
                target_prefix=[[prefix] for _ in batch] if prefix is not None else None,
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
        return None  # MADLAD hedef dil etiketini kaynak girdinin başında ister

    def _source_tokens(self, text: str, source: str | None, target: str) -> list[str]:
        if self._info.id == "nllb-1.3b":
            return [NLLB_LANGUAGE_CODES[source], *self._tokenizer.encode(text), EOS_TOKEN]
        tagged = self._tokenizer.encode(f"{MADLAD_PREFIX.format(target)} {text}")
        return [*tagged, EOS_TOKEN]

    def _decode(self, hypothesis: list[str], target: str) -> str:
        text = self._tokenizer.decode(hypothesis).strip()
        # Model hedef dil etiketini yine de üretirse altyazıya sızmasın.
        tag = self._target_prefix(target)
        if tag is None:
            tag = MADLAD_PREFIX.format(target)
        if text.startswith(tag):
            text = text[len(tag) :].lstrip(" :,")
        return text


class _HfTokenizer:
    """Hugging Face tokenizer.json (NLLB BPE); transformers/torch gerekmez."""

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
    """T5/MADLAD tokenizer'ı; depodaki spiece.model doğrudan yüklenir."""

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
