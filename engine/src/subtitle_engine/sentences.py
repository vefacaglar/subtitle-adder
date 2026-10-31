"""Adım 3: kelime akışını cümlelere, uzun cümleleri ekrana sığan altyazı satırlarına böler."""
from dataclasses import dataclass

from .models import Sentence, Word

SENTENCE_END = (".", "!", "?", "…")
SOFT_BREAK = (",", ";", ":")
_CLOSERS = "\"')]»”’"


def _core(text: str) -> str:
    """Sondaki kapanış tırnak/parantezlerini atar: `"Evet."` cümle sonudur."""
    return text.rstrip(_CLOSERS)


@dataclass
class SegmenterConfig:
    pause_gap: float = 1.2
    max_chars: int = 84
    max_duration: float = 6.0


def segment(words: list[Word], config: SegmenterConfig = SegmenterConfig()) -> list[Sentence]:
    lines = [line for sentence in split_sentences(words, config.pause_gap) for line in fit(sentence, config)]
    return [_join(line, i) for i, line in enumerate(lines, 1)]


def split_sentences(words: list[Word], pause_gap: float) -> list[list[Word]]:
    """Noktalama ya da uzun sessizlikle biten kelimede cümleyi kapatır."""
    result: list[list[Word]] = []
    current: list[Word] = []
    for i, word in enumerate(words):
        current.append(word)
        nxt = words[i + 1] if i + 1 < len(words) else None
        long_pause = nxt is not None and nxt.start - word.end >= pause_gap
        if _core(word.text).endswith(SENTENCE_END) or long_pause or nxt is None:
            result.append(current)
            current = []
    return result


def fit(words: list[Word], config: SegmenterConfig) -> list[list[Word]]:
    """Sınırı aşan cümleyi tercihen virgülde, yoksa sınırda böler. Metin kaybı olmaz."""
    result: list[list[Word]] = []
    while len(words) > 1 and _too_long(words, config):
        cut = 1
        while cut < len(words) and not _too_long(words[: cut + 1], config):
            cut += 1
        soft = [i + 1 for i in range(cut - 1) if _core(words[i].text).endswith(SOFT_BREAK)]
        if soft and soft[-1] >= cut // 2:
            cut = soft[-1]
        result.append(words[:cut])
        words = words[cut:]
    result.append(words)
    return result


def _too_long(words: list[Word], config: SegmenterConfig) -> bool:
    chars = sum(len(w.text) for w in words) + len(words) - 1
    return chars > config.max_chars or words[-1].end - words[0].start > config.max_duration


def _join(words: list[Word], id: int) -> Sentence:
    return Sentence(
        id=id,
        start=words[0].start,
        end=words[-1].end,
        text=" ".join(w.text for w in words),
        words=list(words),
    )
