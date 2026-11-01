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


def _first_alpha(text: str) -> str:
    for ch in text:
        if ch.isalpha():
            return ch
    return ""


def _starts_upper(word: Word) -> bool:
    ch = _first_alpha(word.text)
    return bool(ch and ch.isupper())


def _starts_lower(word: Word) -> bool:
    ch = _first_alpha(word.text)
    return bool(ch and ch.islower())


def _ends_sentence(words: list[Word]) -> bool:
    return bool(words and _core(words[-1].text).endswith(SENTENCE_END))


def _ends_break(word: Word) -> bool:
    c = _core(word.text)
    return c.endswith(SENTENCE_END) or c.endswith(SOFT_BREAK)


def _merge_clitic_words(words: list[Word]) -> list[Word]:
    """Merges sub-word contraction/hyphen tokens (e.g. `c` + `'est`, `T` + `'es`, `celui` + `-ci`) into one Word."""
    if not words:
        return []
    merged: list[Word] = []
    for w in words:
        txt = w.text.strip()
        if not txt:
            continue
        if (
            merged
            and (w.start - merged[-1].end) <= 1.0
            and (
                (txt[0] in ("'", "’", "-") and any(ch.isalnum() for ch in txt[1:]))
                or (merged[-1].text.endswith(("'", "’", "-")) and any(ch.isalnum() for ch in merged[-1].text[:-1]))
            )
        ):
            prev = merged[-1]
            merged[-1] = Word(start=prev.start, end=max(prev.end, w.end), text=f"{prev.text}{txt}")
        else:
            merged.append(w)
    return merged


@dataclass
class SegmenterConfig:
    pause_gap: float = 1.2
    max_chars: int = 84
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
    """Closes the sentence at a word that ends with punctuation or is followed by a long silence,
    then runs a final realignment pass so trailing starters and mid-sentence pauses form coherent sentences."""
    words = _merge_clitic_words(words)
    result: list[list[Word]] = []
    current: list[Word] = []
    for i, word in enumerate(words):
        current.append(word)
        nxt = words[i + 1] if i + 1 < len(words) else None
        gap = (nxt.start - word.end) if nxt is not None else 0.0
        long_pause = nxt is not None and gap >= pause_gap
        # Avoid isolating a single unpunctuated short word (e.g. "Je", "Tu", "es") unless the silence is very long.
        if (
            long_pause
            and len(current) == 1
            and len(_core(word.text).strip()) <= 3
            and not _core(word.text).endswith(SENTENCE_END)
            and gap < max(pause_gap * 3.5, 4.2)
        ):
            long_pause = False
        if _core(word.text).endswith(SENTENCE_END) or long_pause or nxt is None:
            result.append(current)
            current = []
    return _realign_sentences(result)


def _realign_sentences(sentences: list[list[Word]]) -> list[list[Word]]:
    """Final pass over segmented sentences:
    1. Moves a trailing 1-2 word capitalized sentence starter (`... Je`, `... Tu en`, `... T'es`) at the end of an
       unpunctuated sentence onto the beginning of the next sentence when the next sentence starts with a lowercase
       continuation word (`suis...`, `penses...`, `es...`, `ridicule...`, `ne...`, `dois...`).
    2. Merges short unpunctuated fragments (`Je`, `Tu es`, `es`) into a following lowercase continuation so sentences
       split across dramatic mid-sentence pauses become complete, meaningful thoughts.
    """
    if len(sentences) <= 1:
        return sentences

    aligned = [list(s) for s in sentences if s]

    # Pass 1: detach trailing uppercase sentence starters before a lowercase continuation.
    for i in range(len(aligned) - 1):
        curr = aligned[i]
        nxt = aligned[i + 1]
        if not curr or not nxt or _ends_sentence(curr) or not _starts_lower(nxt[0]):
            continue
        if (nxt[0].start - curr[-1].end) > 6.0:
            continue
        split_at: int | None = None
        for k in range(len(curr) - 1, max(0, len(curr) - 3), -1):
            if k >= 1 and _starts_upper(curr[k]) and not any(_ends_break(w) for w in curr[k:]):
                split_at = k
                break
        if split_at is not None:
            tail = curr[split_at:]
            aligned[i] = curr[:split_at]
            aligned[i + 1] = tail + nxt

    # Pass 2: merge short unpunctuated starter/middle fragments into a lowercase continuation.
    merged: list[list[Word]] = []
    idx = 0
    while idx < len(aligned):
        curr = aligned[idx]
        while idx + 1 < len(aligned):
            nxt = aligned[idx + 1]
            if not curr or not nxt or _ends_sentence(curr) or not _starts_lower(nxt[0]):
                break
            if (nxt[0].start - curr[-1].end) > 6.0:
                break
            should_merge = (
                len(curr) == 1
                or (len(curr) <= 3 and _starts_upper(curr[0]))
                or (len(nxt) == 1 and not _starts_upper(nxt[0]))
            )
            if not should_merge:
                break
            curr = curr + nxt
            idx += 1
        if curr:
            merged.append(curr)
        idx += 1

    return merged


def fit(words: list[Word], config: SegmenterConfig) -> list[list[Word]]:
    """Splits a sentence that exceeds the limits, preferably at a comma or clause starter, otherwise at the limit."""
    result: list[list[Word]] = []
    while len(words) > 1 and _too_long(words, config):
        cut = 1
        while cut < len(words) and not _too_long(words[: cut + 1], config):
            cut += 1
        soft = [i + 1 for i in range(cut - 1) if _core(words[i].text).endswith(SOFT_BREAK)]
        if soft and soft[-1] >= cut // 2:
            cut = soft[-1]
        else:
            upper = [i for i in range(max(2, cut // 3), cut) if _starts_upper(words[i])]
            if upper:
                cut = upper[-1]
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
