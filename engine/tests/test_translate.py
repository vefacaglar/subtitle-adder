"""Translation layer tests: no network, CTranslate2/tokenizer fakes are used."""
import io
import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

from subtitle_engine import cli, models, translate
from subtitle_engine.models import Sentence, Word


class FakeTokenizer:
    """Splits on whitespace; when decoding it joins the tokens as is."""

    def encode(self, text):
        return text.split()

    def decode(self, tokens):
        return " ".join(tokens)


class FakeCt2:
    def __init__(self, outputs: list[str] | None = None):
        self.calls: list[dict] = []
        self.outputs = outputs

    def translate_batch(self, source, target_prefix=None, **kwargs):
        self.calls.append({"source": source, "target_prefix": target_prefix, "kwargs": kwargs})
        if self.outputs is None:
            outputs = [f"çeviri {i}" for i in range(len(source))]
        else:  # share the translations given in the test across batches in order
            outputs, self.outputs = self.outputs[: len(source)], self.outputs[len(source) :]
        return [SimpleNamespace(hypotheses=[[text]]) for text in outputs]


@pytest.fixture
def make_translator(monkeypatch):
    def factory(model_id: str, outputs: list[str] | None = None):
        fake = FakeCt2(outputs)
        monkeypatch.setattr(translate, "_load_tokenizer", lambda directory, model_id: FakeTokenizer())
        monkeypatch.setattr(
            translate, "_load_ct2_translator", lambda path, device, compute_type: fake
        )
        monkeypatch.setattr(models, "model_dir", lambda mid: Path("/cache") / mid)
        return translate.Ct2Translator(model_id), fake

    return factory


def sentence(id: int, start: float, end: float, text: str, words: list[Word] | None = None) -> Sentence:
    return Sentence(id=id, start=start, end=end, text=text, words=words or [])


def test_madlad_uses_target_prefix_and_keeps_timing(make_translator):
    source = sentence(3, 1.25, 2.75, "Merhaba 42 dünya.", [Word(1.25, 1.5, "Merhaba")])
    translator, fake = make_translator("madlad-3b", ["Merhaba 42 dünya çevirisi"])

    result = translator.translate([source], "es")

    call = fake.calls[0]
    # MADLAD puts the target language tag at the start of the source input; it uses no decoder prefix.
    assert call["source"] == [["<2es>", "Merhaba", "42", "dünya.", "</s>"]]
    assert call["target_prefix"] is None
    assert call["kwargs"]["beam_size"] == 4
    assert call["kwargs"]["max_decoding_length"] > 0
    assert result[0].id == 3 and result[0].start == 1.25 and result[0].end == 2.75
    assert result[0].text == "Merhaba 42 dünya çevirisi"
    assert result[0].words == []


@pytest.mark.parametrize(
    ("source_language", "target_language", "source_token", "target_token"),
    [
        ("tr", "ru", "tur_Latn", "rus_Cyrl"),
        ("pt", "es", "por_Latn", "spa_Latn"),
        ("en", "tr", "eng_Latn", "tur_Latn"),
    ],
)
def test_nllb_uses_flores_codes(
    make_translator, source_language, target_language, source_token, target_token
):
    translator, fake = make_translator("nllb-1.3b", ["çevrilmiş"])

    result = translator.translate([sentence(1, 0, 1, "merhaba")], target_language, source=source_language)

    assert fake.calls[0]["source"] == [[source_token, "merhaba", "</s>"]]
    assert fake.calls[0]["target_prefix"] == [[target_token]]
    assert result[0].text == "çevrilmiş"


def test_madlad_ignores_source_language(make_translator):
    translator, fake = make_translator("madlad-3b", ["hola"])

    result = translator.translate([sentence(1, 0, 1, "Hallo.")], "es", source="de")

    assert result[0].text == "hola"
    assert fake.calls[0]["source"] == [["<2es>", "Hallo.", "</s>"]]


def test_nllb_requires_source_language(make_translator):
    translator, _ = make_translator("nllb-1.3b")

    with pytest.raises(models.ModelError, match="kaynak dil"):
        translator.translate([sentence(1, 0, 1, "merhaba")], "ru")


def test_unsupported_language_is_turkish_error(make_translator):
    translator, fake = make_translator("madlad-3b")

    with pytest.raises(models.ModelError, match="desteklemiyor"):
        translator.translate([sentence(1, 0, 1, "merhaba")], "de")
    assert fake.calls == []


def test_blank_sentences_are_not_sent_and_times_survive(make_translator):
    sentences = [
        sentence(1, 0.0, 1.0, "Bir."),
        sentence(2, 1.0, 2.0, "   "),
        sentence(3, 2.0, 3.0, "İki."),
    ]
    translator, fake = make_translator("madlad-3b", ["one", "two"])

    result = translator.translate(sentences, "en")

    assert fake.calls[0]["source"] == [["<2en>", "Bir.", "</s>"], ["<2en>", "İki.", "</s>"]]
    assert [(s.id, s.start, s.end) for s in result] == [(1, 0.0, 1.0), (2, 1.0, 2.0), (3, 2.0, 3.0)]
    assert [s.text for s in result] == ["one", "   ", "two"]
    assert all(s.words == [] for s in result)


def test_batches_and_progress(make_translator, monkeypatch):
    monkeypatch.setattr(translate, "BATCH_SIZE", 2)
    translator, fake = make_translator("madlad-3b", ["a", "b", "c", "d", "e"])
    progress: list[float] = []

    result = translator.translate(
        [sentence(i, i, i + 1, f"cümle {i}") for i in range(5)], "es", on_progress=progress.append
    )

    assert [len(call["source"]) for call in fake.calls] == [2, 2, 1]
    assert progress == [0.4, 0.8, 1.0]
    assert [s.text for s in result] == ["a", "b", "c", "d", "e"]


def test_language_tag_does_not_leak_into_text(make_translator):
    translator, _ = make_translator("madlad-3b", ["<2es> hola"])

    result = translator.translate([sentence(1, 0, 1, "Merhaba.")], "es")

    assert result[0].text == "hola"


def test_empty_hypothesis_falls_back_to_source(make_translator):
    translator, _ = make_translator("madlad-3b", [""])

    result = translator.translate([sentence(1, 0, 1, "Metin.")], "es")

    assert result[0].text == "Metin."  # do not produce an empty subtitle


def test_cli_json_event_order_and_translation_event(capsys, monkeypatch):
    payload = [
        {"id": 1, "start": 0.0, "end": 2.0, "text": "Merhaba.", "words": []},
        {"id": 2, "start": 2.0, "end": 4.0, "text": " ", "words": []},
    ]
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(payload)))
    monkeypatch.setattr(models, "needs_download", lambda model_id: True)
    monkeypatch.setattr(
        models,
        "ensure_model",
        lambda model_id, on_progress=None: (on_progress(0.5), on_progress(1.0), "/cache/madlad")[-1],
    )

    class FakeTranslator:
        def translate(self, sentences, target, source=None, on_progress=None):
            on_progress(0.5)
            on_progress(1.0)
            return [
                sentence(s.id, s.start, s.end, "Hola." if s.text.strip() else s.text) for s in sentences
            ]

    monkeypatch.setattr(translate, "create_translator", lambda model_id: FakeTranslator())

    assert cli.main(["translate", "--to", "es", "--json"]) == 0

    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    stages = [(e["stage"], e["fraction"]) for e in events if e["event"] == "stage"]
    assert stages == [
        ("downloading_model", 0.0),
        ("downloading_model", 0.5),
        ("downloading_model", 1.0),
        ("loading_model", None),
        ("translating", 0.0),
        ("translating", 0.5),
        ("translating", 1.0),
    ]
    translated = events[-1]
    assert translated["event"] == "translation" and translated["language"] == "es"
    assert [s["text"] for s in translated["sentences"]] == ["Hola.", " "]
    assert all(s["words"] == [] for s in translated["sentences"])


def test_cli_nllb_without_source_is_turkish_error(capsys, monkeypatch):
    monkeypatch.setattr(sys, "stdin", io.StringIO("[]"))

    assert cli.main(["translate", "--to", "ru", "--model", "nllb-1.3b", "--json"]) == 1

    event = json.loads(capsys.readouterr().out)
    assert event["event"] == "error" and "kaynak dil" in event["message"]


def test_cli_unsupported_language_is_turkish_error(capsys, monkeypatch):
    monkeypatch.setattr(sys, "stdin", io.StringIO("[]"))

    assert cli.main(["translate", "--to", "de", "--json"]) == 1

    event = json.loads(capsys.readouterr().out)
    assert event["event"] == "error" and "desteklemiyor" in event["message"]


def test_cli_writes_srt_without_json(capsys, monkeypatch):
    payload = [{"id": 1, "start": 0.0, "end": 2.0, "text": "Merhaba.", "words": []}]
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(payload)))
    monkeypatch.setattr(models, "needs_download", lambda model_id: False)
    monkeypatch.setattr(
        translate,
        "create_translator",
        lambda model_id: SimpleNamespace(
            translate=lambda sentences, target, source=None, on_progress=None: [
                sentence(s.id, s.start, s.end, "Hola.") for s in sentences
            ]
        ),
    )

    assert cli.main(["translate", "--to", "es"]) == 0

    out = capsys.readouterr().out
    assert "1\n00:00:00,000 --> 00:00:02,000\nHola.\n" == out


@pytest.mark.slow
def test_real_madlad_translates_tr_es_and_ru_pt():
    """Real model: short tr→es and ru→pt sentences must be neither empty nor identical to the source."""
    models.ensure_model("madlad-3b")
    translator = translate.create_translator("madlad-3b")

    cases = [
        ("tr", "es", "Merhaba, nasılsın?"),
        ("ru", "pt", "Привет, как дела?"),
    ]
    for source_language, target_language, text in cases:
        result = translator.translate([sentence(1, 0.0, 2.0, text)], target_language, source=source_language)
        translated = result[0].text.strip()
        assert translated
        assert translated.casefold() != text.casefold()

    # Numbers and proper names must be preserved.
    result = translator.translate(
        [sentence(2, 0.0, 2.0, "Oda numarası 42, Mehmet ile görüşün.")], "es", source="tr"
    )
    assert "42" in result[0].text and "Mehmet" in result[0].text


def test_translate_keeps_speaker(make_translator):
    translator, _ = make_translator("madlad-3b")
    sentences = [
        Sentence(id=1, start=0.0, end=1.0, text="Merhaba.", speaker=0),
        Sentence(id=2, start=1.0, end=2.0, text=" ", speaker=1),
        Sentence(id=3, start=2.0, end=3.0, text="Selam.", speaker=None),
    ]
    result = translator.translate(sentences, target="es")
    assert [r.speaker for r in result] == [0, 1, None]


def test_llama_translator_builds_direct_prompt_with_dialect_and_context(monkeypatch):
    monkeypatch.setattr(translate, "LLM_BATCH_SIZE", 2)
    monkeypatch.setattr(translate, "LLM_CONTEXT_LINES", 2)
    calls: list[list[dict[str, str]]] = []
    replies = [
        "<think>Thinking about Brazilian Portuguese and Mexican Spanish slang...</think>\n[1] Ne haber dostum?\n[2] Hiçbir şey olmaz, merak etme.",
        "[1] Harika görünüyorsun!",
    ]

    def fake_complete(messages: list[dict[str, str]]) -> str:
        calls.append(messages)
        return replies.pop(0)

    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=fake_complete)
    sentences = [
        Sentence(id=1, start=0.0, end=1.5, text="E aí, mano?", speaker=0),
        Sentence(id=2, start=1.5, end=3.0, text="No pasa nada, güey.", speaker=1),
        Sentence(id=3, start=3.0, end=4.5, text="Tá mandando muito bem!", speaker=0),
    ]
    progress: list[float] = []
    result = translator.translate(sentences, target="tr", source="pt", on_progress=progress.append)

    assert len(calls) == 2
    sys_prompt = calls[0][0]["content"]
    assert "Brazilian Portuguese" in sys_prompt
    assert "Turkish" in sys_prompt
    assert "[1] E aí, mano?\n[2] No pasa nada, güey." in calls[0][1]["content"]
    # Second batch includes preceding lines as context
    assert "Previous lines (for context only, do NOT translate):" in calls[1][1]["content"]
    assert "- E aí, mano?" in calls[1][1]["content"]
    assert "[1] Tá mandando muito bem!" in calls[1][1]["content"]
    assert [r.text for r in result] == [
        "Ne haber dostum?",
        "Hiçbir şey olmaz, merak etme.",
        "Harika görünüyorsun!",
    ]
    assert [r.speaker for r in result] == [0, 1, 0]
    assert progress == [2 / 3, 1.0]


def test_llama_translator_retries_missing_line_in_batch():
    calls: list[list[dict[str, str]]] = []
    replies = [
        "[1] Birinci satır çevirisi",  # Line [2] omitted by the LLM in batch
        "[1] İkinci satır tekil çevirisi",  # Single-line retry
    ]

    def fake_complete(messages: list[dict[str, str]]) -> str:
        calls.append(messages)
        return replies.pop(0)

    translator = translate.LlamaTranslator("qwen3.5-4b", completion_fn=fake_complete)
    sentences = [
        Sentence(id=1, start=0.0, end=1.0, text="Line one."),
        Sentence(id=2, start=1.0, end=2.0, text="Line two."),
    ]
    result = translator.translate(sentences, target="tr", source="en")

    assert len(calls) == 2
    assert [r.text for r in result] == ["Birinci satır çevirisi", "İkinci satır tekil çevirisi"]

