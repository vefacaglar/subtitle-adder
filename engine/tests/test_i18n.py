"""Bilingual user-facing text: tr (default) and en, via SUBTITLE_ENGINE_LANG or --lang."""
import json
import re

import pytest

from subtitle_engine import cli, i18n, models, translate


def test_default_is_turkish():
    assert i18n.current() == "tr"
    assert models.support_error(models.find("gigaam-v3"), "tr") == "GigaAM yalnız Rusça destekler."


def test_env_selects_english(monkeypatch):
    monkeypatch.setenv(i18n.ENV_VAR, "en")
    assert models.support_error(models.find("gigaam-v3"), "tr") == "GigaAM supports only Russian."


@pytest.mark.parametrize("value", ["", "de", "EN-us", "turkish"])
def test_invalid_env_falls_back_to_turkish(monkeypatch, value):
    monkeypatch.setenv(i18n.ENV_VAR, value)
    assert i18n.current() == "tr"


def test_env_value_is_case_insensitive(monkeypatch):
    monkeypatch.setenv(i18n.ENV_VAR, " EN ")
    assert i18n.current() == "en"


def test_every_key_has_both_languages_and_same_placeholders():
    field = re.compile(r"\{(\w+)\}")
    for key, texts in i18n.MESSAGES.items():
        assert set(texts) == set(i18n.LANGUAGES), key
        assert all(texts.values()), key
        assert set(field.findall(texts["tr"])) == set(field.findall(texts["en"])), key


def test_unsupported_language_and_translation_errors(monkeypatch):
    nllb = "nllb-1.3b"
    with pytest.raises(models.ModelError, match="kaynak dil"):
        translate.check_request(nllb, "en")
    with pytest.raises(models.ModelError, match="Bilinmeyen çeviri modeli: base"):
        translate.check_request("base", "en")
    monkeypatch.setenv(i18n.ENV_VAR, "en")
    with pytest.raises(models.ModelError, match="requires a source language"):
        translate.check_request(nllb, "en")
    with pytest.raises(models.ModelError, match="Unknown translation model: base"):
        translate.check_request("base", "en")


def test_cli_error_event_follows_env(monkeypatch, capsys):
    monkeypatch.setenv(i18n.ENV_VAR, "en")
    assert cli.main(["delete-model", "nope", "--json"]) == 1
    event = json.loads(capsys.readouterr().out)
    assert event == {"event": "error", "message": "Unknown model: nope. The 'models' command lists the valid ids."}


def test_cli_lang_flag_overrides_env(monkeypatch, capsys):
    monkeypatch.setenv(i18n.ENV_VAR, "en")
    assert cli.main(["--lang", "tr", "delete-model", "nope", "--json"]) == 1
    assert "Bilinmeyen model: nope" in json.loads(capsys.readouterr().out)["message"]


def test_cli_lang_flag_invalid_value_means_turkish(capsys):
    assert cli.main(["--lang", "xx", "delete-model", "nope", "--json"]) == 1
    assert "Bilinmeyen model" in json.loads(capsys.readouterr().out)["message"]


def test_cli_lang_flag_does_not_leak_between_calls(capsys):
    cli.main(["--lang", "en", "delete-model", "nope", "--json"])
    capsys.readouterr()
    cli.main(["delete-model", "nope", "--json"])
    assert "Bilinmeyen model" in json.loads(capsys.readouterr().out)["message"]


def test_speakers_without_diarize_error_english(capsys):
    assert cli.main(["--lang", "en", "transcribe", "x.mp4", "--speakers", "2", "--json"]) == 1
    assert json.loads(capsys.readouterr().out)["message"] == "--speakers can only be used with --diarize."


def test_models_table_headers_english(monkeypatch, capsys):
    monkeypatch.setattr(cli, "cached_disk_mb", lambda: {})
    monkeypatch.setattr(cli, "is_downloaded", lambda _id: False)
    assert cli.main(["--lang", "en", "models"]) == 0
    header = capsys.readouterr().out.splitlines()[0]
    assert "accuracy" in header and "cache" in header and "doğruluk" not in header


def test_partial_delete_name_follows_language(monkeypatch):
    monkeypatch.setenv(i18n.ENV_VAR, "en")
    assert models.display_name("speaker-diarization") == "Speaker separation"
    assert models.language_name("es") == "Spanish"
    assert models.language_name("xx") == "xx"


def test_cli_lang_flag_after_subcommand(capsys):
    assert cli.main(["delete-model", "nope", "--lang", "en", "--json"]) == 1
    assert "Unknown model: nope" in json.loads(capsys.readouterr().out)["message"]


def test_subcommand_lang_wins_over_global(capsys):
    assert cli.main(["--lang", "tr", "delete-model", "nope", "--lang", "en", "--json"]) == 1
    assert "Unknown model: nope" in json.loads(capsys.readouterr().out)["message"]
    assert cli.main(["--lang", "en", "delete-model", "nope", "--json"]) == 1
    assert "Unknown model: nope" in json.loads(capsys.readouterr().out)["message"]


def test_empty_external_llm_url_error_is_localized(monkeypatch):
    with pytest.raises(models.ModelError, match="Harici LLM sunucusu adresi boş olamaz."):
        translate.normalize_external_chat_url("  ")
    monkeypatch.setenv(i18n.ENV_VAR, "en")
    with pytest.raises(models.ModelError, match="The external LLM server address cannot be empty."):
        translate.normalize_external_chat_url("/")


@pytest.mark.parametrize("raw", ["", "  ", "/", "http://", "https://", "http:///", "HTTP://", "http://:1234"])
def test_external_llm_url_without_a_host_is_rejected(raw):
    with pytest.raises(models.ModelError, match="Harici LLM sunucusu adresi boş olamaz."):
        translate.normalize_external_chat_url(raw)


def test_external_llm_failure_names_the_server_not_llama_cpp(monkeypatch):
    def refuse(req, timeout=None):
        raise translate.urllib.error.URLError("[Errno 61] Connection refused")

    monkeypatch.setattr(translate.urllib.request, "urlopen", refuse)
    monkeypatch.setattr(translate, "_resolve_lan_url", lambda url: url)
    translator = translate.LlamaTranslator(
        "gemma-4-e4b", external_url="192.168.1.50:1234", external_model="qwen3"
    )
    with pytest.raises(models.ModelError) as tr:
        translator._complete([{"role": "user", "content": "hi"}])
    assert "Harici LLM sunucusuna ulaşılamadı (http://192.168.1.50:1234/v1/chat/completions)" in str(tr.value)
    assert "Connection refused" in str(tr.value) and "llama" not in str(tr.value).lower()
    monkeypatch.setenv(i18n.ENV_VAR, "en")
    with pytest.raises(models.ModelError) as en:
        translator._complete([{"role": "user", "content": "hi"}])
    assert str(en.value).startswith("Could not reach the external LLM server (http://192.168.1.50:1234")
    assert "llama" not in str(en.value).lower()
