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
        ("de", "tr", "deu_Latn", "tur_Latn"),
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
        translator.translate([sentence(1, 0, 1, "merhaba")], "ko")
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


def test_empty_hypothesis_retries_then_marks_line_for_review(make_translator):
    translator, fake = make_translator("madlad-3b", [""])

    result = translator.translate([sentence(1, 0, 1, "Metin.")], "es")
    assert result[0].text == "Metin." and result[0].needs_review
    assert len(fake.calls) == 2


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
        ("translating", None),
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

    assert cli.main(["translate", "--to", "ko", "--json"]) == 1

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


def test_llama_translator_retries_missing_line_with_whole_batch():
    calls: list[list[dict[str, str]]] = []
    replies = [
        "[1] Birinci satır çevirisi",  # Line [2] omitted by the LLM in batch
        "[1] Birinci satır çevirisi\n[2] İkinci satır çevirisi",
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
    assert "[1] Line one.\n[2] Line two." in calls[1][1]["content"]
    assert [r.text for r in result] == ["Birinci satır çevirisi", "İkinci satır çevirisi"]


def test_normalize_external_chat_url_handles_lm_studio_ollama_and_raw_ip():
    assert translate.normalize_external_chat_url("192.168.1.50:1234") == "http://192.168.1.50:1234/v1/chat/completions"
    assert translate.normalize_external_chat_url("http://192.168.1.50:11434/") == "http://192.168.1.50:11434/v1/chat/completions"
    assert translate.normalize_external_chat_url("http://192.168.1.50:1234/v1") == "http://192.168.1.50:1234/v1/chat/completions"
    assert (
        translate.normalize_external_chat_url("http://mac-studio.local:8080/v1/chat/completions")
        == "http://mac-studio.local:8080/v1/chat/completions"
    )


def test_external_llm_skips_local_model_download_and_posts_to_remote_server(monkeypatch):
    monkeypatch.setenv(translate.EXTERNAL_LLM_URL_ENV_VAR, "192.168.1.42:1234")
    monkeypatch.setenv(translate.EXTERNAL_LLM_MODEL_ENV_VAR, "gemma-3-27b-it")
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *a, **kw: None)

    captured_requests: list[tuple[str, dict]] = []

    class FakeHttpResponse:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def read(self) -> bytes:
            return json.dumps(
                {"choices": [{"message": {"content": "[1] Merhaba M1 Max!"}}]}
            ).encode("utf-8")

    def fake_urlopen(req, timeout=None):
        captured_requests.append((req.full_url, json.loads(req.data.decode("utf-8"))))
        return FakeHttpResponse()

    monkeypatch.setattr(translate.urllib.request, "urlopen", fake_urlopen)

    # Even if the toolbar model is madlad-3b (not downloaded locally), external LLM mode uses LlamaTranslator over LAN.
    translator = translate.create_translator("madlad-3b")
    result = translator.translate([Sentence(id=1, start=0.0, end=1.5, text="Hello M1 Max!")], target="tr", source="en")

    assert [r.text for r in result] == ["Merhaba M1 Max!"]
    assert len(captured_requests) == 1
    assert captured_requests[0][0] == "http://192.168.1.42:1234/v1/chat/completions"
    assert captured_requests[0][1]["model"] == "gemma-3-27b-it"


def test_external_llm_api_key_attaches_bearer_header_and_retries_without_chat_template_kwargs(monkeypatch):
    monkeypatch.setenv(translate.EXTERNAL_LLM_URL_ENV_VAR, "https://opencode.ai/zen/go/v1")
    monkeypatch.setenv(translate.EXTERNAL_LLM_MODEL_ENV_VAR, "qwen3-235b")
    monkeypatch.setenv(translate.EXTERNAL_LLM_API_KEY_ENV_VAR, "sk-opencode-test-key-123")
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *a, **kw: None)

    captured_requests: list[tuple[str, dict[str, str], dict]] = []

    class FakeHttpResponse:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def read(self) -> bytes:
            return json.dumps(
                {"choices": [{"message": {"content": "[1] Merhaba OpenCode Go!"}}]}
            ).encode("utf-8")

    def fake_urlopen(req, timeout=None):
        headers = {k.lower(): v for k, v in req.header_items()}
        body = json.loads(req.data.decode("utf-8")) if req.data else {}
        captured_requests.append((req.full_url, headers, body))
        if "chat_template_kwargs" in body:
            raise translate.urllib.error.HTTPError(
                req.full_url, 400, "Bad Request: unrecognized field chat_template_kwargs", {}, None
            )
        return FakeHttpResponse()

    monkeypatch.setattr(translate.urllib.request, "urlopen", fake_urlopen)

    translator = translate.create_translator("madlad-3b")
    result = translator.translate(
        [Sentence(id=1, start=0.0, end=1.5, text="Hello OpenCode Go!")],
        target="tr",
        source="en",
    )

    assert [r.text for r in result] == ["Merhaba OpenCode Go!"]
    assert len(captured_requests) == 2
    assert captured_requests[0][0] == "https://opencode.ai/inference/go/openai/v1/chat/completions"
    assert captured_requests[0][1]["authorization"] == "Bearer sk-opencode-test-key-123"
    assert captured_requests[0][1]["user-agent"] == "SubtitleAdder/1.0"
    assert "chat_template_kwargs" in captured_requests[0][2]
    assert "chat_template_kwargs" not in captured_requests[1][2]
    assert captured_requests[1][1]["authorization"] == "Bearer sk-opencode-test-key-123"
    assert captured_requests[0][1]["x-opencode-session"] == captured_requests[1][1]["x-opencode-session"]


def test_temp_checkpoint_path_unaffected_by_external_api_key(monkeypatch):
    sentences = [Sentence(id=1, start=0.0, end=1.5, text="Hello world")]
    monkeypatch.setenv(translate.EXTERNAL_LLM_URL_ENV_VAR, "https://opencode.ai/zen/go/v1")
    monkeypatch.setenv(translate.EXTERNAL_LLM_MODEL_ENV_VAR, "glm-5")
    monkeypatch.setenv(translate.EXTERNAL_LLM_API_KEY_ENV_VAR, "sk-secret-alpha")
    path_a = translate._temp_checkpoint_path("gemma-4-e4b", "tr", "en", sentences, "prompt")

    monkeypatch.setenv(translate.EXTERNAL_LLM_API_KEY_ENV_VAR, "sk-secret-beta")
    path_b = translate._temp_checkpoint_path("gemma-4-e4b", "tr", "en", sentences, "prompt")

    assert path_a == path_b
    assert "sk-secret" not in str(path_a)


@pytest.mark.parametrize("base", [
    "https://opencode.ai/zen/go/v1",
    "https://opencode.ai/zen/go/v1/chat/completions/",
    "https://opencode.ai/inference/go/openai/v1",
    "https://opencode.ai/inference/go/openai/v1/chat/completions",
])
def test_opencode_v2_chat_and_model_discovery_endpoints(base):
    chat = translate.normalize_external_chat_url(base)
    assert chat == "https://opencode.ai/inference/go/openai/v1/chat/completions"
    assert translate._external_models_url(chat) == "https://opencode.ai/inference/go/v1/models"


def test_opencode_mapping_does_not_rewrite_custom_proxies():
    custom = "https://proxy.example/zen/go/v1"
    assert translate.normalize_external_chat_url(custom) == custom + "/chat/completions"


@pytest.mark.parametrize("status", [400, 403, 429])
def test_remote_errors_keep_gateway_reason_without_key_and_do_not_retry(monkeypatch, status):
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *a, **kw: None)
    key = "sk-test-secret"
    requests = []

    def reject(req, timeout=None):
        requests.append(req)
        body = json.dumps({"error": {"message": f"Global region is required; key {key}"}}).encode()
        raise translate.urllib.error.HTTPError(req.full_url, status, "Bad Request", {}, io.BytesIO(body))

    monkeypatch.setattr(translate.urllib.request, "urlopen", reject)
    translator = translate.LlamaTranslator(
        "madlad-3b", external_url="https://opencode.ai/zen/go/v1",
        external_model="deepseek-v4.1-flash", external_api_key=key,
    )
    with pytest.raises(models.ModelError) as error:
        translator.translate([Sentence(id=1, start=0, end=1, text="Hello world")], "tr", "en")
    assert len(requests) == 1
    assert f"HTTP {status}" in str(error.value)
    assert "Global region is required" in str(error.value)
    assert key not in str(error.value)
    assert "[redacted]" in str(error.value)


def test_http_error_does_not_dump_html_or_large_body():
    error = translate.urllib.error.HTTPError("https://example.com", 502, "Bad Gateway", {}, io.BytesIO(b"<html>" * 3000))
    assert translate._http_error_detail(error, None) == "HTTP Error 502: Bad Gateway"


def test_opencode_session_is_stable_and_isolated_from_other_translators():
    def translator(url):
        return translate.LlamaTranslator("madlad-3b", external_url=url, external_model="deepseek-v4.1-flash")

    first = translator("https://opencode.ai/zen/go/v1")
    second = translator("https://opencode.ai/inference/go/openai/v1")
    assert first._request_headers()["x-opencode-session"] == first._request_headers()["x-opencode-session"]
    assert first._request_headers()["x-opencode-session"] != second._request_headers()["x-opencode-session"]
    assert "x-opencode-session" not in translator("http://localhost:1234/v1")._request_headers()


@pytest.mark.parametrize("url,model,disabled", [
    ("https://opencode.ai/zen/go/v1", "deepseek-v4.1-flash", True),
    ("https://opencode.ai/inference/go/openai/v1", "deepseek-v4-pro", True),
    ("https://api.deepseek.com/v1", "deepseek-flash", True),
    ("https://opencode.ai/inference/go/openai/v1", "glm-5.3-flash", False),
    ("http://localhost:1234/v1", "deepseek-v4.1-flash", False),
    ("https://proxy.example/v1", "deepseek-v4.1-flash", False),
])
def test_deepseek_subtitles_disable_thinking_only_on_supported_gateways(monkeypatch, capsys, url, model, disabled):
    calls = []
    secret = "sk-private-test"

    class Response:
        def __enter__(self): return self
        def __exit__(self, *args): return False
        def read(self):
            return json.dumps({"choices": [{"finish_reason": "stop", "message": {
                "content": "[1] Merhaba.", "reasoning_content": "private reasoning text"
            }}]}).encode()

    def urlopen(req, timeout):
        calls.append(json.loads(req.data))
        return Response()

    monkeypatch.setattr(translate.urllib.request, "urlopen", urlopen)
    client = translate.LlamaTranslator("madlad-3b", external_url=url, external_model=model, external_api_key=secret)
    assert client._complete([{"role": "user", "content": "private subtitle text"}]) == "[1] Merhaba."
    assert calls[0].get("thinking") == ({"type": "disabled"} if disabled else None)
    captured = capsys.readouterr()
    assert captured.out == ""
    assert "request_start" in captured.err and "request_complete" in captured.err
    for private in (secret, "private subtitle text", "private reasoning text", "Merhaba"):
        assert private not in captured.err


def test_pending_request_reports_wait_then_completion(monkeypatch):
    import threading
    waited = threading.Event()
    events = []

    def diagnostic(event, **fields):
        events.append(event)
        if event == "request_waiting": waited.set()

    class Response:
        def __enter__(self): return self
        def __exit__(self, *args): return False
        def read(self):
            assert waited.wait(2), "pending request should report that it is waiting"
            return b'{"choices":[{"finish_reason":"stop","message":{"content":"[1] Merhaba."}}]}'

    monkeypatch.setattr(translate, "_HTTP_TRACE_INTERVAL", 0.01)
    monkeypatch.setattr(translate, "_translation_diagnostic", diagnostic)
    monkeypatch.setattr(translate.urllib.request, "urlopen", lambda *a, **kw: Response())
    client = translate.LlamaTranslator("madlad-3b", external_url="https://opencode.ai/zen/go/v1", external_model="deepseek-v4.1-flash")
    client._complete([{"role": "user", "content": "test"}])
    assert events[0] == "request_start"
    assert "request_waiting" in events
    assert events[-1] == "request_complete"


def test_request_failure_diagnostics_do_not_dump_error_body(monkeypatch, capsys):
    def reject(req, timeout):
        raise translate.urllib.error.HTTPError(req.full_url, 429, "Rate limit", {}, io.BytesIO(b'{"error":{"message":"private response detail"}}'))
    monkeypatch.setattr(translate.urllib.request, "urlopen", reject)
    client = translate.LlamaTranslator("madlad-3b", external_url="https://opencode.ai/zen/go/v1", external_model="deepseek-v4.1-flash")
    with pytest.raises(models.ModelError):
        client._complete([{"role": "user", "content": "private subtitle"}])
    trace = capsys.readouterr().err
    assert '"http_status": 429' in trace
    assert "private" not in trace



