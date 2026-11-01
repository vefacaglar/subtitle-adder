from conftest import make_words

from subtitle_engine.models import Word
from subtitle_engine.sentences import SegmenterConfig, segment


def test_splits_on_sentence_end_punctuation():
    result = segment(make_words("Merhaba dünya. Nasılsın? İyiyim!"))
    assert [s.text for s in result] == ["Merhaba dünya.", "Nasılsın?", "İyiyim!"]
    assert [s.id for s in result] == [1, 2, 3]


def test_splits_on_long_pause():
    words = make_words("bir iki üç dört")
    words = words[:2] + [Word(w.start + 3, w.end + 3, w.text) for w in words[2:]]
    assert [s.text for s in segment(words)] == ["bir iki", "üç dört"]


def test_long_sentence_prefers_comma_break():
    text = "Bugün sabah erkenden kalktım, kahvaltı yaptım ve sonra uzun bir yürüyüşe çıktım."
    result = segment(make_words(text), SegmenterConfig(max_chars=50, max_duration=100))
    assert result[0].text == "Bugün sabah erkenden kalktım,"
    assert all(len(s.text) <= 50 for s in result)
    assert " ".join(s.text for s in result) == text


def test_long_sentence_splits_on_duration():
    result = segment(make_words("a b c d e f g h i j", step=0.5), SegmenterConfig(max_chars=1000, max_duration=2))
    assert len(result) > 1
    assert all(s.end - s.start <= 2 for s in result)


def test_times_come_from_words():
    words = make_words("Selam millet.")
    sentence = segment(words)[0]
    assert (sentence.start, sentence.end) == (words[0].start, words[-1].end)


def test_empty_input():
    assert segment([]) == []


def test_very_long_unpunctuated_sentence_does_not_recurse():
    words = make_words(" ".join(["kelime"] * 20_000), step=0.1)
    result = segment(words, SegmenterConfig(max_chars=40, max_duration=100, pause_gap=100))
    assert len(result) > 1000
    assert sum(len(s.words) for s in result) == 20_000


def test_sentence_end_before_closing_quote_or_bracket():
    result = segment(make_words('Dedi ki "Evet." Sonra (gitti!) Bitti'))
    assert [s.text for s in result] == ['Dedi ki "Evet."', "Sonra (gitti!)", "Bitti"]


def test_soft_break_before_closing_quote():
    text = 'Bugün sabah "erkenden kalktım," kahvaltı yaptım ve sonra uzun bir yürüyüşe çıktım.'
    result = segment(make_words(text), SegmenterConfig(max_chars=50, max_duration=100))
    assert result[0].text == 'Bugün sabah "erkenden kalktım,"'


def test_realigns_trailing_starters_and_mid_sentence_pauses():
    # Simulates dramatic pauses mid-sentence where Whisper capitalizes sentence starts
    # and lowercases continuations after a pause, plus split clitics/hyphens.
    words = [
        Word(10.0, 10.4, "Bon"),
        Word(10.5, 10.8, "Je"),
        Word(12.8, 13.2, "suis"),
        Word(13.3, 13.9, "désolée"),
        Word(14.1, 14.3, "Tu"),
        Word(14.4, 14.7, "en"),
        Word(17.2, 17.5, "penses"),
        Word(17.6, 17.8, "quoi"),
        Word(17.9, 18.1, "alors"),
        Word(18.2, 18.3, "de"),
        Word(18.4, 18.6, "celui"),
        Word(18.6, 18.7, "-ci"),
        Word(18.9, 19.1, "Tu"),
        Word(22.6, 22.8, "es"),
        Word(26.3, 26.8, "ridicule"),
        Word(27.0, 27.1, "T"),
        Word(27.1, 27.3, "'es"),
        Word(28.9, 29.4, "moche"),
    ]
    result = segment(words, SegmenterConfig(pause_gap=1.2, max_chars=84, max_duration=10.0))
    assert [s.text for s in result] == [
        "Bon",
        "Je suis désolée",
        "Tu en penses quoi alors de celui-ci",
        "Tu es ridicule",
        "T'es moche",
    ]
