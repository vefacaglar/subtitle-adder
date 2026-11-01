"""Model adapters: prompt roles, context mapping and backend-specific request fields."""
import json
from types import SimpleNamespace

import pytest

from subtitle_engine import translate
from subtitle_engine.models import Sentence
from subtitle_engine.translation_profiles import build_hy_mt_prompt, profile_for_model


@pytest.mark.parametrize("model,family", [
    ("gemma3:27b", "gemma-legacy"),
    ("google/gemma-3-12b-it", "gemma-legacy"),
    ("bartowski/gemma-2-9b-it-GGUF:Q4_K_M", "gemma-legacy"),
    ("gemma3n:e4b", "gemma-legacy"),
    ("gemma4:12b", "gemma4"),
    ("unsloth/gemma-4-E4B-it-Q4_K_M.gguf", "gemma4"),
    ("Qwen/Qwen3-4B", "qwen3"),
    ("qwen3:8b", "qwen3"),
    ("Qwen/Qwen3.5-4B", "qwen3.5"),
    ("qwen3.5:9b", "qwen3.5"),
    ("translategemma:4b", "translategemma"),
    ("google/translate_gemma-4b-it", "translategemma"),
    ("hf.co/tencent/HY-MT1.5-1.8B-GGUF:Q4_K_M", "hy-mt1.5"),
    ("tencent/Hunyuan-MT-7B", "hunyuan-mt"),
    ("Hunyuan-MT-Chimera-7B", "generic"),
    ("Qwen3-4B-Instruct-2507", "generic"),
    ("Qwen3-30B-A3B-Thinking-2507", "generic"),
    ("Qwen3-VL-8B", "generic"),
    ("Qwen2.5:7b", "generic"),
    ("qwen3.6:27b", "generic"),
    ("gemma40:4b", "generic"),
    ("custom-model", "generic"),
])
def test_profile_detection_including_tags_quantizations_and_distinct_variants(model, family):
    assert profile_for_model(model).name == family


@pytest.mark.parametrize("model,roles,soft_switch", [
    ("gemma3:27b", ["user"], False),
    ("gemma-4-e4b", ["system", "user"], False),
    ("qwen3:8b", ["system", "user"], True),
    ("qwen3.5:4b", ["system", "user"], False),
    ("custom-model", ["system", "user"], False),
])
def test_chat_translation_and_retry_keep_model_roles_and_context(model, roles, soft_switch):
    calls = []
    replies = iter(["invalid response", "[1] Merhaba."])

    def complete(messages):
        calls.append(messages)
        return next(replies)

    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=complete,
        external_url="http://127.0.0.1:1234", external_model=model)
    result = translator.translate([Sentence(42, 1, 2, "Hello.")], "tr", source="en")
    assert result[0].id == 42 and result[0].text == "Merhaba."
    for messages in calls:
        assert [m["role"] for m in messages] == roles
        assert "[1] Hello." in messages[-1]["content"]
        assert messages[-1]["content"].endswith("/no_think") == soft_switch
        assert "subtitle translator" in messages[0]["content"]
    assert "previous attempt" in calls[1][-1]["content"]


@pytest.mark.parametrize("url,ollama", [
    ("http://127.0.0.1:1234", False),
    ("http://127.0.0.1:11434", True),
    ("http://127.0.0.1:9000/api/chat", True),
])
@pytest.mark.parametrize("model,thinking,hy_mt", [
    ("qwen3.5:4b", True, False),
    ("gemma4:12b", True, False),
    ("gemma3:27b", False, False),
    ("translategemma:4b", False, False),
    ("hy-mt1.5:7b", False, True),
    ("custom-model", False, False),
])
def test_http_options_match_model_and_server_not_toolbar_selection(monkeypatch, url, ollama, model, thinking, hy_mt):
    requests = []

    class Response:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def read(self):
            message = {"content": "[1] Merhaba."}
            return json.dumps({"message": message} if ollama else {"choices": [{"message": message}]}).encode()

    def urlopen(request, timeout):
        requests.append((request.full_url, json.loads(request.data)))
        return Response()

    monkeypatch.setattr(translate.urllib.request, "urlopen", urlopen)
    translator = translate.LlamaTranslator("gemma-4-e4b", external_url=url, external_model=model)
    assert translator._complete([{"role": "user", "content": "test"}]) == "[1] Merhaba."
    endpoint, body = requests[0]
    assert body["model"] == model
    assert endpoint.endswith("/api/chat") == ollama
    assert ("think" in body) == (thinking and ollama)
    assert ("chat_template_kwargs" in body) == (thinking and not ollama)
    if thinking:
        assert (body["think"] if ollama else body["chat_template_kwargs"]["enable_thinking"]) is False
    sampling = body["options"] if ollama else body
    assert sampling["temperature"] == (0.7 if hy_mt else 0.2)
    if hy_mt:
        assert sampling["top_p"] == 0.6
        if ollama:
            assert sampling["top_k"] == 20 and sampling["repeat_penalty"] == 1.05
        else:
            assert "top_k" not in sampling and "repetition_penalty" not in sampling
            assert "repeat_penalty" not in sampling
    if ollama:
        assert sampling["num_predict"] == 1536
        assert "max_tokens" not in body and "temperature" not in body
    else:
        assert body["max_tokens"] == 1536
        assert "options" not in body


def test_local_qwen_uses_catalog_profile_without_external_server(monkeypatch):
    translator = translate.LlamaTranslator("qwen3.5-4b", completion_fn=lambda _: "")
    translator._completion_fn = None
    translator._process = SimpleNamespace(poll=lambda: None)
    translator._port = 8765
    captured = []

    class Response:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def read(self):
            return b'{"choices":[{"message":{"content":"[1] hello"}}]}'

    def urlopen(request, timeout):
        captured.append(json.loads(request.data))
        return Response()

    monkeypatch.setattr(translate.urllib.request, "urlopen", urlopen)
    try:
        translator._complete([{"role": "user", "content": "test"}])
    finally:
        translator._process = None
    assert captured[0]["chat_template_kwargs"] == {"enable_thinking": False}
    assert "options" not in captured[0] and "think" not in captured[0]


def test_hy_mt15_context_is_reference_not_extra_translation(monkeypatch):
    monkeypatch.setattr(translate, "LLM_BATCH_SIZE", 2)
    calls = []
    replies = iter(["[1] Müdür Hanım geldi.\n[2] Onu bekleyin.", "[1] Buyurun Müdür Hanım."])

    def complete(messages):
        calls.append(messages)
        return next(replies)

    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=complete,
        external_url="http://127.0.0.1:11434", external_model="hy-mt1.5:7b")
    result = translator.translate([
        Sentence(10, 0, 2, "The director has arrived."),
        Sentence(11, 2, 4, "Wait for her."),
        Sentence(12, 4, 6, "Please come in, director."),
    ], "tr", source="en")
    assert all(len(messages) == 1 and messages[0]["role"] == "user" for messages in calls)
    prompt = calls[1][0]["content"]
    assert "The director has arrived." in prompt
    assert "Previous translations (terminology reference only):\nMüdür Hanım geldi." in prompt
    assert "=>" not in prompt
    assert "不需要翻译上文" in prompt
    assert prompt.endswith("[1] Please come in, director.")
    assert [s.id for s in result] == [10, 11, 12]
    assert result[-1].text == "Buyurun Müdür Hanım."


def test_hunyuan_legacy_window_preserves_subtitle_ids(monkeypatch):
    monkeypatch.setattr(translate, "LLM_BATCH_SIZE", 1)
    calls = []

    def complete(messages):
        calls.append(messages)
        return "[1] Merhaba.\n[2] Hoş geldin."

    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=complete,
        external_url="http://127.0.0.1:1234", external_model="tencent/Hunyuan-MT-7B")
    result = translator.translate([Sentence(10, 0, 1, "Hello."), Sentence(11, 1, 2, "Welcome.")], "tr", "en")
    assert [s.text for s in result] == ["Merhaba.", "Hoş geldin."]
    assert all(len(messages) == 1 for messages in calls)
    assert calls[0][0]["content"].startswith("Translate the following segment into Turkish,")


def test_hy_mt_chinese_pair_uses_version_specific_template():
    assert build_hy_mt_prompt(["Hello"], "zh", "en").startswith("将以下文本翻译为中文")
    assert build_hy_mt_prompt(["你好"], "tr", "zh", legacy=True).startswith("把下面的文本翻译成土耳其语")


def test_gemma4_thought_channel_cannot_leak_reasoning_or_fake_line_ids():
    reply = "<|channel>thought\n[1] This is reasoning.\n<channel|>[1] Merhaba."
    assert translate.parse_numbered_translations(reply, 1) == {1: "Merhaba."}
    assert translate.parse_numbered_translations("<|channel>thought\n[1] Unfinished", 1) == {}
