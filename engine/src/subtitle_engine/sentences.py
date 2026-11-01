"""Step 3: splits the word stream into sentences, and long sentences into subtitle lines that fit the screen."""
from collections.abc import Sequence
from dataclasses import dataclass

from .i18n import t
from .models import Sentence, Word

SENTENCE_END = (".", "!", "?", "…")
SOFT_BREAK = (",", ";", ":")
_CLOSERS = "\"')]»”’"


def _core(text: str) -> str:
    """Strips trailing closing quotes/brackets: `"Evet."` ends a sentence."""
    return text.rstrip(_CLOSERS)


@dataclass
class SegmenterConfig:
    pause_gap: float = 1.2
    max_chars: float = 84
    max_duration: float = 6.0


def segment(
    words: list[Word],
    config: SegmenterConfig = SegmenterConfig(),
    speakers: Sequence[int | None] | None = None,
) -> list[Sentence]:
    """Splits words into subtitle lines. If `speakers` is parallel to the words, it also splits at every
    speaker change: a line never contains two speakers."""
    lines: list[tuple[list[Word], int | None]] = []
    for run, speaker in speaker_runs(words, speakers):
        lines.extend(
            (line, speaker) for sentence in split_sentences(run, config.pause_gap) for line in fit(sentence, config)
        )
    return [_join(line, i, speaker) for i, (line, speaker) in enumerate(lines, 1)]


def speaker_runs(
    words: list[Word], speakers: Sequence[int | None] | None
) -> list[tuple[list[Word], int | None]]:
    """Consecutive words of the same speaker. Without `speakers`, a single run (speaker None)."""
    if not words:
        return []
    if speakers is None:
        return [(list(words), None)]
    if len(speakers) != len(words):
        raise ValueError(t("speaker_list_length"))
    runs: list[tuple[list[Word], int | None]] = []
    for word, speaker in zip(words, speakers):
        if runs and runs[-1][1] == speaker:
            runs[-1][0].append(word)
        else:
            runs.append(([word], speaker))
    return runs


def split_sentences(words: list[Word], pause_gap: float) -> list[list[Word]]:
    """Closes the sentence at a word that ends with punctuation or is followed by a long silence."""
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


def _is_clause_start(prev_word: Word, curr_word: Word) -> bool:
    """True when `curr_word` looks like an unpunctuated new clause starter (e.g. capitalized word or after a pause)."""
    if curr_word.start - prev_word.end >= 0.28:
        return True
    prev_core = _core(prev_word.text).strip()
    curr_core = _core(curr_word.text).strip()
    if not prev_core or not curr_core:
        return False
    return curr_core[0].isupper() and not prev_core[0].isupper()


def fit(words: list[Word], config: SegmenterConfig) -> list[list[Word]]:
    """Splits a sentence that exceeds the limits, preferably at a comma or clause boundary, otherwise at the limit."""
    result: list[list[Word]] = []
    while len(words) > 1 and _too_long(words, config):
        cut = 1
        while cut < len(words) and not _too_long(words[: cut + 1], config):
            cut += 1
        soft = [i + 1 for i in range(cut - 1) if _core(words[i].text).endswith(SOFT_BREAK)]
        if soft and soft[-1] >= cut // 2:
            cut = soft[-1]
        else:
            clause_breaks = [
                i for i in range(max(1, cut // 2), cut) if _is_clause_start(words[i - 1], words[i])
            ]
            if clause_breaks:
                cut = clause_breaks[-1]
        result.append(words[:cut])
        words = words[cut:]
    result.append(words)
    return result


def _too_long(words: list[Word], config: SegmenterConfig) -> bool:
    chars = sum(len(w.text) for w in words) + len(words) - 1
    return chars > config.max_chars or words[-1].end - words[0].start > config.max_duration


def _join(words: list[Word], id: int, speaker: int | None = None) -> Sentence:
    return Sentence(
        id=id,
        start=words[0].start,
        end=words[-1].end,
        text=" ".join(w.text for w in words),
        words=list(words),
        speaker=speaker,
    )
