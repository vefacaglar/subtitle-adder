"""Offline behaviour: the network is used only to download a missing model, and never in strict offline mode."""
import json

import huggingface_hub
import pytest

from subtitle_engine import cli, models, pipeline


def test_go_offline_blocks_hub_requests():
    models.go_offline()

    assert huggingface_hub.constants.is_offline_mode()
    with pytest.raises(huggingface_hub.errors.OfflineModeIsEnabled):
        huggingface_hub.get_session().get("https://huggingface.co/api/models/Systran/faster-whisper-base")


def test_local_commands_lock_the_network_out(capsys):
    assert cli.main(["models", "--json"]) == 0
    capsys.readouterr()
    assert huggingface_hub.constants.is_offline_mode()


@pytest.mark.parametrize("flag", ["env", "cli"])
def test_strict_offline_reports_missing_model_before_any_work(monkeypatch, tmp_path, capsys, flag):
    monkeypatch.setenv("SUBTITLE_ENGINE_LANG", "en")
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: False)
    monkeypatch.setattr(models, "ensure_model", lambda *a, **k: pytest.fail("must not download"))
    monkeypatch.setattr(pipeline, "extract_audio", lambda *a, **k: pytest.fail("must not start work"))
    args = ["transcribe", str(tmp_path / "video.mp4"), "-m", "base", "-o", str(tmp_path), "--json"]
    if flag == "env":
        monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")
    else:
        args = ["--offline", *args]

    assert cli.main(args) == 1
    event = json.loads(capsys.readouterr().out.strip().splitlines()[-1])
    assert event["event"] == "error"
    assert "offline mode" in event["message"]


def test_strict_offline_message_is_turkish_by_default(monkeypatch):
    monkeypatch.delenv("SUBTITLE_ENGINE_LANG", raising=False)
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: False)

    with pytest.raises(ValueError, match="Çevrimdışı mod"):
        models.require_download("base")


def test_cached_models_need_no_network_and_downloads_stay_allowed(monkeypatch):
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: True)
    assert models.require_download("base") is False

    monkeypatch.setattr(models, "is_downloaded", lambda model_id: False)
    assert models.require_download("base") is True  # not strict: the download goes ahead
    assert not huggingface_hub.constants.is_offline_mode()
