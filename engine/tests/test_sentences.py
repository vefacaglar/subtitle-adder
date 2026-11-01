from conftest import make_words

from subtitle_engine.models import Word
from subtitle_engine.sentences import SegmenterConfig, fit, segment


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


# --- fit(): where a too-long, comma-less line is cut ---------------------------------------------------------------

FIT = SegmenterConfig(max_chars=40, max_duration=100, pause_gap=100)


def _fit_lines(text: str, config: SegmenterConfig = FIT) -> list[str]:
    lines = [" ".join(w.text for w in part) for part in fit(make_words(text), config)]
    # However it is cut, nothing is lost or reordered and every line is within the limits.
    assert " ".join(lines) == text
    assert all(len(line) <= config.max_chars for line in lines)
    return lines


def test_fit_cuts_before_a_capitalized_word_when_there_is_no_comma():
    # 8 words fit; "Ahmet" is the last of them and starts a new clause.
    lines = _fit_lines("ben dün akşam eve geç geldim çünkü Ahmet ile konuştum ve sonra uyudum")
    assert lines == ["ben dün akşam eve geç geldim çünkü", "Ahmet ile konuştum ve sonra uyudum"]


def test_fit_takes_the_last_capitalized_word_in_the_window():
    lines = _fit_lines("ben dün Ali ile geç gittik Ece ile eve döndük ve çok yorulduk")
    assert lines[0] == "ben dün Ali ile geç gittik"
    assert lines[1].startswith("Ece ")


def test_fit_ignores_a_capitalized_word_in_the_first_two_positions():
    lines = _fit_lines("bugün Ahmet eve gelmedi çünkü yağmur yağıyordu ve hava çok soğuktu", SegmenterConfig(max_chars=30, max_duration=100, pause_gap=100))
    assert lines[0] == "bugün Ahmet eve gelmedi çünkü"


def test_fit_ignores_a_capitalized_word_before_the_start_of_the_window():
    # 20 one-letter words fit, so the window opens at index 6.
    early = "a b C d e f g h i j k l m n o p q r s t u v w"
    assert [len(line.split()) for line in _fit_lines(early)] == [20, 3]
    boundary = "a b c d e f G h i j k l m n o p q r s t u v w"
    assert [len(line.split()) for line in _fit_lines(boundary)] == [6, 17]
    before = "a b c d e Ff g h i j k l m n o p q r s t u v w"
    assert [len(line.split()) for line in _fit_lines(before)] == [20, 3]


def test_fit_without_any_clause_start_cuts_at_the_hard_limit():
    lines = _fit_lines("kelime " * 11 + "kelime")
    assert lines[0] == "kelime kelime kelime kelime kelime" and len(lines) == 3


def test_fit_prefers_a_comma_in_the_second_half_over_a_capitalized_word():
    # The comma ends word 12 (>= 20 // 2); the capitalized word at index 16 would cut later.
    comma = "a b c d e f g h i j k l, m n o p Q r s t u v w"
    assert [len(line.split()) for line in _fit_lines(comma)] == [12, 11]


def test_fit_falls_back_to_a_capitalized_word_when_the_comma_is_too_early():
    # The comma ends word 4 (< 10), so it is not worth a short first line.
    early_comma = "a b c, d e f g h i j k l m n o p Q r s t u v w"
    assert [len(line.split()) for line in _fit_lines(early_comma)] == [16, 7]


def test_fit_leaves_a_short_sentence_alone():
    words = make_words("Kısa bir cümle")
    assert fit(words, FIT) == [words]
