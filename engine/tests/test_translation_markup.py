"""Reject model-added markup through normal retry/review handling; do not just strip it."""
import json

import pytest

from subtitle_engine import translate
from subtitle_engine.models import Sentence


SOURCE = "Sua língua, véia, horrorosa."
BAD = "*Kafan* *kötü* *kadın* *kıskanç* *yapıyor*."


@pytest.mark.parametrize("text", [BAD, "**Merhaba.**", "_Merhaba._", "`Merhaba.`"])
def test_added_markup_is_detected(text):
    assert translate._has_unrequested_markup(SOURCE, text)


@pytest.mark.parametrize("source,translated", [
    ("*Hello.*", "*Merhaba.*"),
    ("Use user_name.", "user_name kullan."),
    ("Two times three.", "2 * 3"),
    ("A censored word.", "s*ktir"),
    ("Hello.", "Merhaba."),
])
def test_literal_symbols_and_existing_source_formatting_are_preserved(source, translated):
    assert not translate._has_unrequested_markup(source, translated)


def test_screenshot_output_is_retried_instead_of_cleaned_and_published():
    replies = iter([f"[1] {BAD}", "[1] Test çevirisi."])
    batches = []
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda _: next(replies))
    result = translator.translate([Sentence(7, 10, 12, SOURCE)], "tr", "pt", on_batch=batches.append)
    assert result[0].text == "Test çevirisi." and not result[0].needs_review
    assert [s.text for batch in batches for s in batch] == ["Test çevirisi."]


def test_repeated_markup_remains_saveable_but_is_marked_for_review():
    calls = []
    def complete(messages):
        calls.append(messages)
        return f"[1] {BAD}"
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=complete)
    result = translator.translate([Sentence(7, 10, 12, SOURCE)], "tr", "pt")
    assert len(calls) == 3
    assert result[0].text == SOURCE and result[0].needs_review


def test_marked_up_checkpoint_does_not_bypass_validation(monkeypatch, tmp_path):
    path = tmp_path / "checkpoint.json"
    path.write_text(json.dumps({"7": BAD}), encoding="utf-8")
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *args: path)
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda _: "")
    translator._completion_fn = None
    calls = []
    def complete(messages):
        calls.append(messages)
        return "[1] Test çevirisi."
    monkeypatch.setattr(translator, "_complete", complete)
    result = translator.translate([Sentence(7, 10, 12, SOURCE)], "tr", "pt")
    assert len(calls) == 1 and result[0].text == "Test çevirisi."
