"""Regression for Qwen emitting the source => translation pair seen in a subtitle."""
import json

import pytest

from subtitle_engine import models, translate
from subtitle_engine.models import Sentence


SOURCE = "eu não consigo abrir minha boca daquele jeito"
TARGET = "ben o şekilde ağzımı açamıyorum"


@pytest.mark.parametrize("arrow", ["=>", "->", "→", "⇒"])
@pytest.mark.parametrize("reverse", [False, True])
def test_bilingual_pair_is_detected_even_when_target_text_dilutes_source_overlap(arrow, reverse):
    parts = [SOURCE, TARGET]
    if reverse:
        parts.reverse()
    output = f" {arrow} ".join(parts)
    assert translate._looks_untranslated(SOURCE, output, "pt", "tr")


def test_pair_detection_ignores_case_quotes_and_punctuation():
    assert translate._has_source_translation_pair(SOURCE, f'"{SOURCE.upper()}." => {TARGET}')


@pytest.mark.parametrize("arrow", ["=>", "->", "→", "⇒"])
@pytest.mark.parametrize("reverse", [False, True])
def test_unambiguous_pair_is_cleaned_in_either_order(arrow, reverse):
    parts = [SOURCE, TARGET]
    if reverse:
        parts.reverse()
    assert translate._clean_translation_pair(SOURCE, f" {arrow} ".join(parts)) == TARGET


def test_literal_or_ambiguous_arrows_are_not_cut():
    assert translate._clean_translation_pair("A => B", "A => B") == "A => B"
    ambiguous = f"{SOURCE} => {TARGET} => extra"
    assert translate._clean_translation_pair(SOURCE, ambiguous) == ambiguous


@pytest.mark.parametrize("source,output", [
    (SOURCE, TARGET),
    ("The sign says A => B.", "Tabelada A => B yazıyor."),
    ("A -> B", "A → B"),
    ("Alice!", "Alice!"),
    ("2026", "2026"),
    ("Use the arrow symbol.", "=> simgesini kullan."),
])
def test_real_dialogue_and_literal_arrows_are_not_rejected(source, output):
    assert not translate._has_source_translation_pair(source, output)


def test_qwen_recovers_screenshot_example_without_extra_inference():
    replies = iter([f"[1] {SOURCE} => {TARGET}", f"[1] {TARGET}"])
    calls, batches = [], []

    def complete(messages):
        calls.append(messages)
        return next(replies)

    translator = translate.LlamaTranslator("qwen3.5-4b", completion_fn=complete)
    result = translator.translate([Sentence(7, 10, 13, SOURCE)], "tr", "pt", on_batch=batches.append)
    assert len(calls) == 1
    assert not result[0].needs_review
    assert [s.text for batch in batches for s in batch] == [TARGET]
    assert (result[0].id, result[0].start, result[0].end) == (7, 10, 13)


def test_repeated_pair_is_cleaned_and_saved_as_translation(monkeypatch, tmp_path):
    path = tmp_path / "checkpoint.json"
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *args: path)
    translator = translate.LlamaTranslator("qwen3.5-4b", completion_fn=lambda _: "")
    translator._completion_fn = None
    monkeypatch.setattr(translator, "_complete", lambda _: f"[1] {SOURCE} => {TARGET}")
    batches, progress = [], []
    result = translator.translate([Sentence(7, 10, 13, SOURCE)], "tr", "pt",
                                  on_batch=batches.append, on_progress=progress.append)
    assert not path.exists()
    assert result[0].text == TARGET and not result[0].needs_review
    assert len(batches) == 1 and progress == [1.0]


def test_leaked_pair_in_checkpoint_is_retranslated(monkeypatch, tmp_path):
    path = tmp_path / "checkpoint.json"
    path.write_text(json.dumps({"7": f"{SOURCE} => {TARGET}"}), encoding="utf-8")
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *args: path)
    calls = []
    translator = translate.LlamaTranslator("qwen3.5-4b", completion_fn=lambda _: "")
    translator._completion_fn = None

    def complete(messages):
        calls.append(messages)
        return f"[1] {TARGET}"

    monkeypatch.setattr(translator, "_complete", complete)
    result = translator.translate([Sentence(7, 10, 13, SOURCE)], "tr", "pt")
    assert len(calls) == 1 and result[0].text == TARGET
    assert not path.exists()


def test_copy_of_a_different_context_line_pair_is_rejected(monkeypatch):
    monkeypatch.setattr(translate, "LLM_BATCH_SIZE", 1)
    replies = iter(["[1] Merhaba.", "[1] Hello. => Merhaba.", "[1] Nasılsın?"])
    calls = []

    def complete(messages):
        calls.append(messages)
        return next(replies)

    translator = translate.LlamaTranslator("qwen3.5-4b", completion_fn=complete)
    result = translator.translate([Sentence(1, 0, 1, "Hello."), Sentence(2, 1, 2, "How are you?")], "tr", "en")
    assert len(calls) == 3
    assert [s.text for s in result] == ["Merhaba.", "Nasılsın?"]
