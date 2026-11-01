"""Regression checks for contextual translation and rejecting corrupt model output."""
import json

import pytest

from subtitle_engine import cli, models, translate
from subtitle_engine.models import Sentence


def cues(*texts):
    return [Sentence(id=i + 10, start=i * 2, end=i * 2 + 2, text=text, speaker=0)
            for i, text in enumerate(texts)]


@pytest.mark.parametrize("reply", [
    "Here is the translation.",
    "[1] Merhaba.\nNote: This is a greeting.",
    "[1] Merhaba.\n[1] Başka bir şey.",
    "[7] Yanlış satır.",
    "<think>I should translate [1] hello",
    "[1] Merhaba.\n[2] Fazladan satır.",
    "",
])
def test_parser_does_not_turn_prose_or_ambiguous_numbering_into_subtitles(reply):
    assert translate.parse_numbered_translations(reply, 1) == {}


def test_parser_strips_closed_reasoning_and_accepts_fenced_numbered_lines():
    reply = "<think>Private reasoning.</think>\n```text\n[2] Nasılsın?\n[1] Merhaba.\n```"
    assert translate.parse_numbered_translations(reply, 2) == {1: "Merhaba.", 2: "Nasılsın?"}


def test_batch_boundary_keeps_continuing_sentence_with_its_start():
    source = cues("First.", "Second.", "Third.", "Fourth.", "If you", "want to", "come, call me.")
    batches = list(translate._translation_batches(list(enumerate(source)), 6))
    assert [[s.text for _, s in batch] for _, batch in batches] == [
        ["First.", "Second.", "Third.", "Fourth."],
        ["If you", "want to", "come, call me."],
    ]


def test_unpunctuated_dialogue_still_has_bounded_batches():
    source = cues(*["continuing dialogue" for _ in range(19)])
    batches = list(translate._translation_batches(list(enumerate(source)), 6))
    assert [len(batch) for _, batch in batches] == [6, 6, 6, 1]
    assert [s.id for _, batch in batches for _, s in batch] == [s.id for s in source]


def test_previous_translations_and_following_source_reach_next_batch(monkeypatch):
    monkeypatch.setattr(translate, "LLM_BATCH_SIZE", 2)
    calls = []
    replies = iter(["[1] Müdür Hanım geldi.\n[2] Onu bekleyin.", "[1] Buyurun Müdür Hanım."])

    def complete(messages):
        calls.append(messages)
        return next(replies)

    source = cues("The director has arrived.", "Wait for her.", "Please come in, director.")
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=complete)
    result = translator.translate(source, "tr", source="en")
    assert "- Please come in, director." in calls[0][1]["content"]
    assert "- The director has arrived. => Müdür Hanım geldi." in calls[1][1]["content"]
    assert [(s.id, s.start, s.end, s.speaker) for s in result] == [
        (s.id, s.start, s.end, s.speaker) for s in source
    ]
    assert all(not s.words for s in result)


@pytest.mark.parametrize("bad_reply", ["", "Explanation only.", "[1] Where are you going?"])
def test_invalid_single_line_is_retried_before_publishing(bad_reply):
    replies = iter([bad_reply, "[1] Nereye gidiyorsun?"])
    batches = []
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda _: next(replies))
    result = translator.translate(cues("Where are you going?"), "tr", source="en", on_batch=batches.append)
    assert result[0].text == "Nereye gidiyorsun?"
    assert len(batches) == 1 and batches[0][0].text == "Nereye gidiyorsun?"


@pytest.mark.parametrize("reply", ["", "[1] Where are you going?", "[1] Selam.\nExplanation."])
def test_repeated_invalid_response_is_an_error_not_completed_source_text(reply):
    calls, batches, progress = [], [], []

    def complete(messages):
        calls.append(messages)
        return reply

    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=complete)
    with pytest.raises(models.ModelError, match="10.*Çeviri tamamlanmadı"):
        translator.translate(cues("Where are you going?"), "tr", source="en",
                             on_batch=batches.append, on_progress=progress.append)
    assert len(calls) == 2
    assert batches == [] and progress == []


@pytest.mark.parametrize("text", ["Alice!", "New York", "2026", "OK."])
def test_names_numbers_and_short_shared_expressions_are_not_rejected(text):
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda _: f"[1] {text}")
    assert translator.translate(cues(text), "tr", source="en")[0].text == text


def test_shared_names_do_not_trigger_an_untranslated_false_positive():
    assert not translate._looks_untranslated("Alice, Bob and Charlie.", "Alice, Bob ve Charlie.", "en", "tr")
    assert not translate._looks_untranslated("Where are you going?", "Where are you going?", "en", "en")


@pytest.mark.parametrize("gap,speaker", [(2.0, 0), (0.0, 1)])
def test_batch_boundary_can_follow_a_pause_or_speaker_change(gap, speaker):
    source = cues(*["unpunctuated dialogue" for _ in range(7)])
    source[4].start += gap
    for s in source[4:]:
        s.speaker = speaker
    batches = list(translate._translation_batches(list(enumerate(source)), 6))
    assert [len(batch) for _, batch in batches] == [4, 3]


@pytest.mark.parametrize("ollama", [False, True])
def test_http_truncated_translation_is_retried_not_published(monkeypatch, ollama):
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *args: None)
    calls = []

    class Response:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def read(self):
            truncated = len(calls) == 1
            message = {"content": "[1] Yarın size" if truncated else "[1] Yarın size haber vereceğiz."}
            payload = (
                {"message": message, "done_reason": "length" if truncated else "stop"}
                if ollama else
                {"choices": [{"message": message, "finish_reason": "length" if truncated else "stop"}]}
            )
            return json.dumps(payload).encode("utf-8")

    def urlopen(request, timeout):
        calls.append(json.loads(request.data.decode("utf-8")))
        return Response()

    monkeypatch.setattr(translate.urllib.request, "urlopen", urlopen)
    translator = translate.LlamaTranslator(
        "gemma-4-e4b", external_url="http://127.0.0.1:11434" if ollama else "http://127.0.0.1:1234",
        external_model="test-model",
    )
    batches = []
    result = translator.translate(cues("We will let you know tomorrow."), "tr", source="en", on_batch=batches.append)
    assert len(calls) == 2
    assert result[0].text == "Yarın size haber vereceğiz."
    assert [batch[0].text for batch in batches] == [result[0].text]


def test_checkpoint_identity_includes_scene_metadata_and_server(monkeypatch, tmp_path):
    monkeypatch.setattr(translate.tempfile, "gettempdir", lambda: str(tmp_path))
    monkeypatch.delenv(translate.EXTERNAL_LLM_URL_ENV_VAR, raising=False)
    source = cues("Wait for me.")
    original = translate._temp_checkpoint_path("gemma-4-e4b", "tr", "en", source)
    source[0].speaker = 1
    changed_speaker = translate._temp_checkpoint_path("gemma-4-e4b", "tr", "en", source)
    source[0].start = 0.5
    changed_time = translate._temp_checkpoint_path("gemma-4-e4b", "tr", "en", source)
    monkeypatch.setenv(translate.EXTERNAL_LLM_URL_ENV_VAR, "http://127.0.0.1:1234")
    changed_server = translate._temp_checkpoint_path("gemma-4-e4b", "tr", "en", source)
    assert len({original, changed_speaker, changed_time, changed_server}) == 4


def test_checkpoint_identity_follows_the_prompt_and_the_configured_remote_model(monkeypatch, tmp_path):
    monkeypatch.setattr(translate.tempfile, "gettempdir", lambda: str(tmp_path))
    monkeypatch.delenv(translate.EXTERNAL_LLM_URL_ENV_VAR, raising=False)
    monkeypatch.delenv(translate.EXTERNAL_LLM_MODEL_ENV_VAR, raising=False)
    source = cues("Wait for me.")
    path = translate._temp_checkpoint_path
    base = path("gemma-4-e4b", "tr", "en", source, "prompt A")
    assert path("gemma-4-e4b", "tr", "en", source, "prompt A") == base
    assert path("gemma-4-e4b", "tr", "en", source, "prompt B") != base
    monkeypatch.setenv(translate.EXTERNAL_LLM_MODEL_ENV_VAR, "qwen3")
    assert path("gemma-4-e4b", "tr", "en", source, "prompt A") != base


def test_translators_for_other_servers_or_models_do_not_share_a_checkpoint(monkeypatch, tmp_path):
    """A blank model field means the server's own pick, so the model it resolves to decides the checkpoint too."""
    monkeypatch.delenv(translate.EXTERNAL_LLM_URL_ENV_VAR, raising=False)
    monkeypatch.delenv(translate.EXTERNAL_LLM_MODEL_ENV_VAR, raising=False)
    seen = []
    real = translate._temp_checkpoint_path

    def record(*args):
        seen.append(real(*args))
        return tmp_path / "unused.json"

    monkeypatch.setattr(translate, "_temp_checkpoint_path", record)
    monkeypatch.setattr(translate.tempfile, "gettempdir", lambda: str(tmp_path))
    source = cues("Wait for me.")

    def run(url, resolved_model):
        translator = translate.LlamaTranslator("madlad-3b", external_url=url)
        monkeypatch.setattr(translator, "_discover_external_model", lambda: resolved_model)
        monkeypatch.setattr(translator, "_complete", lambda _: "[1] Bekle beni.")
        translator.translate(source, "tr", source="en")

    run("http://10.0.0.2:1234", "qwen3")
    run("http://10.0.0.3:1234", "qwen3")
    run("http://10.0.0.2:1234", "gemma4")
    run("http://10.0.0.2:1234", "qwen3")
    assert len(set(seen[:3])) == 3
    assert seen[3] == seen[0]


@pytest.mark.parametrize("model", ["translategemma:4b", "google/translate-gemma-4b-it"])
def test_translategemma_uses_supported_user_template_and_discards_context_translations(monkeypatch, model):
    monkeypatch.setattr(translate, "LLM_BATCH_SIZE", 2)
    calls = []

    def complete(messages):
        calls.append(messages)
        return "[1] Merhaba.\n[2] Nasılsın?\n[3] İyiyim."

    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=complete,
                                           external_url="http://127.0.0.1:11434", external_model=model)
    source = cues("Hello.", "How are you?", "I'm fine.")
    batches = []
    result = translator.translate(source, "tr", source="en", on_batch=batches.append)
    assert len(calls) == 2
    for messages in calls:
        assert len(messages) == 1 and messages[0]["role"] == "user"
        prompt = messages[0]["content"]
        assert "English (en) to Turkish (tr)" in prompt
        assert "\n\n\n[1] Hello.\n[2] How are you?\n[3] I'm fine." in prompt
        assert "Previous lines" not in prompt and "Following lines" not in prompt
    assert [s.text for s in result] == ["Merhaba.", "Nasılsın?", "İyiyim."]
    assert [s.id for s in batches[0]] == [10, 11]
    assert [s.id for s in result] == [10, 11, 12]


def test_translategemma_without_source_fails_before_sending_text():
    calls = []
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda messages: calls.append(messages),
                                           external_url="http://127.0.0.1:11434", external_model="translategemma:4b")
    with pytest.raises(models.ModelError, match="TranslateGemma için kaynak dil gerekli"):
        translator.translate(cues("Hello there."), "tr")
    assert calls == []


def test_failed_later_batch_keeps_only_valid_checkpoint_and_resumes_with_context(monkeypatch, tmp_path):
    monkeypatch.setattr(translate, "LLM_BATCH_SIZE", 1)
    path = tmp_path / "translation.json"
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *args: path)
    replies = iter(["[1] Müdür Hanım geldi.", "", ""])
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda _: "")
    translator._completion_fn = None  # Exercise the real checkpoint path without a model or network.
    monkeypatch.setattr(translator, "_complete", lambda _: next(replies))
    source = cues("The director has arrived.", "Please come in, director.")
    with pytest.raises(models.ModelError):
        translator.translate(source, "tr", source="en")
    assert json.loads(path.read_text(encoding="utf-8")) == {"10": "Müdür Hanım geldi."}

    calls = []

    def complete(messages):
        calls.append(messages)
        return "[1] Buyurun Müdür Hanım."

    monkeypatch.setattr(translator, "_complete", complete)
    result = translator.translate(source, "tr", source="en")
    assert len(calls) == 1
    assert "=> Müdür Hanım geldi." in calls[0][1]["content"]
    assert result[1].text == "Buyurun Müdür Hanım."
    assert not path.exists()


def test_cli_emits_error_without_successful_translation_for_invalid_output(monkeypatch, capsys):
    import io
    import sys

    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda _: "Explanation only.")
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps([s.to_dict() for s in cues("Hello there.")])))
    monkeypatch.setattr(models, "require_download", lambda _: False)
    monkeypatch.setattr(translate, "create_translator", lambda _: translator)
    assert cli.main(["translate", "--to", "tr", "--from", "en", "--json"]) == 1
    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    assert events[-1]["event"] == "error"
    assert not any(event["event"] == "translation" for event in events)
