"""Model kataloğu ve indirme ilerlemesi testleri: ağa çıkmaz, HF çağrıları monkeypatch'lenir."""
import pytest

from subtitle_engine import models


def test_catalog_order_and_fields():
    assert [m.id for m in models.KATALOG] == [
        "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3",
    ]
    assert all(m.repo for m in models.KATALOG)
    assert all(m.size_mb > 0 for m in models.KATALOG)
    assert all(1 <= m.speed <= 5 and 1 <= m.accuracy <= 5 for m in models.KATALOG)
    assert [m.size_mb for m in models.KATALOG] == sorted(m.size_mb for m in models.KATALOG)


def test_catalog_backends_and_languages():
    by_id = {m.id: m for m in models.KATALOG}
    whisper = ("base", "small", "medium", "large-v3-turbo", "large-v3")
    onnx = ("parakeet-v3", "canary-1b-v2", "gigaam-v3")
    assert all(by_id[m].backend == "whisper" and by_id[m].languages is None for m in whisper)
    assert all(by_id[m].backend == "onnx" for m in onnx)
    assert {"pt", "es", "ru"} <= set(by_id["parakeet-v3"].languages)
    assert {"pt", "es", "ru"} <= set(by_id["canary-1b-v2"].languages)
    assert by_id["gigaam-v3"].languages == ("ru",)
    assert all(by_id[m].onnx_model and by_id[m].quantization and by_id[m].files for m in onnx)


def test_catalog_repos_match_faster_whisper():
    """Whisper repo kimlikleri faster-whisper'ın kendi tablosuyla aynı kalmalı."""
    from faster_whisper.utils import _MODELS

    whisper = [m for m in models.KATALOG if m.backend == "whisper"]
    assert {m.id: m.repo for m in whisper} == {m.id: _MODELS[m.id] for m in whisper}


def test_catalog_repos_match_onnx_asr():
    """onnx modelleri onnx-asr'ın indirdiği Hugging Face depolarına gitmeli."""
    from onnx_asr.resolver import model_repos

    onnx_models = [m for m in models.KATALOG if m.backend == "onnx"]
    assert {m.onnx_model: m.repo for m in onnx_models} == {
        m.onnx_model: model_repos[m.onnx_model] for m in onnx_models
    }


def test_support_error_explains_unsupported_language_in_turkish():
    assert models.support_error(models.find("gigaam-v3"), "tr") == "GigaAM yalnız Rusça destekler."
    message = models.support_error(models.find("parakeet-v3"), "tr")
    assert message.startswith("Parakeet tr dilini desteklemiyor") and "pt" in message


def test_is_downloaded_looks_at_local_cache_only(monkeypatch):
    def fake_cache(repo_id, filename):
        return "/cache/model.bin" if repo_id == "Systran/faster-whisper-small" else None

    def fail_download(*args, **kwargs):
        raise AssertionError("is_downloaded ağa çıkmamalı")

    monkeypatch.setattr(models.huggingface_hub, "try_to_load_from_cache", fake_cache)
    monkeypatch.setattr(models.huggingface_hub, "snapshot_download", fail_download)

    assert models.is_downloaded("small") is True
    assert models.is_downloaded("base") is False
    assert models.needs_download("base") is True
    assert models.needs_download("small") is False
    assert models.is_downloaded("turbo") is False  # katalog dışı: önbellek sorgusu yok
    assert models.needs_download("turbo") is False  # indirme aşaması yayınlanmaz


def test_ensure_model_reports_monotonic_byte_progress(monkeypatch):
    total = 1000
    calls: list[dict] = []
    fractions: list[float] = []

    def fake_download(repo_id, allow_patterns, tqdm_class):
        calls.append({"repo_id": repo_id, "allow_patterns": allow_patterns})
        bar = tqdm_class(total=total, initial=0, unit="B", desc="Reconstructing")
        for step in (100, 100, 200, 100):
            bar.update(step)
        bar.close()
        return "/cache/small"

    monkeypatch.setattr(models, "PROGRESS_INTERVAL", 0)
    monkeypatch.setattr(models, "_total_bytes", lambda info: total)
    monkeypatch.setattr(models.huggingface_hub, "try_to_load_from_cache", lambda repo_id, filename: None)
    monkeypatch.setattr(models.huggingface_hub, "snapshot_download", fake_download)

    assert models.ensure_model("small", on_progress=fractions.append) == "/cache/small"

    assert calls == [{"repo_id": "Systran/faster-whisper-small", "allow_patterns": list(models.ALLOW_PATTERNS)}]
    assert fractions == sorted(fractions)  # artan
    assert all(0.0 <= f <= 1.0 for f in fractions)
    assert fractions[-1] == 1.0
    assert fractions[0] < 1.0


def test_ensure_model_returns_immediately_when_cached(monkeypatch):
    monkeypatch.setattr(models.huggingface_hub, "try_to_load_from_cache", lambda repo_id, filename: "/cache/model.bin")
    monkeypatch.setattr(models.huggingface_hub, "snapshot_download", lambda *a, **kw: pytest.fail("indirilmemeli"))

    assert models.ensure_model("base", on_progress=lambda f: pytest.fail("ilerleme bildirilmemeli")) is None


def test_ensure_model_leaves_unknown_models_to_faster_whisper(monkeypatch):
    monkeypatch.setattr(models.huggingface_hub, "snapshot_download", lambda *a, **kw: pytest.fail("indirilmemeli"))

    assert models.ensure_model("turbo") is None


def test_onnx_download_state_checks_catalog_files_and_vad(monkeypatch):
    info = models.find("gigaam-v3")
    present = {(info.repo, name) for name in info.files} | {(models.VAD_REPO, models.VAD_FILE)}
    monkeypatch.setattr(
        models.huggingface_hub,
        "try_to_load_from_cache",
        lambda repo, filename: "/cache/file" if (repo, filename) in present else None,
    )
    monkeypatch.setattr(
        models.huggingface_hub, "snapshot_download", lambda *a, **kw: pytest.fail("ağa çıkılmamalı")
    )

    assert models.is_downloaded("gigaam-v3") is True
    present.remove((info.repo, info.files[-1]))
    assert models.is_downloaded("gigaam-v3") is False
    assert models.needs_download("gigaam-v3") is True


def test_ensure_model_onnx_downloads_model_files_and_shared_vad(monkeypatch):
    info = models.find("parakeet-v3")
    calls: list[dict] = []
    fractions: list[float] = []

    def fake_download(repo_id, allow_patterns, tqdm_class):
        calls.append({"repo_id": repo_id, "allow_patterns": allow_patterns})
        bar = tqdm_class(total=100, initial=0, unit="B")
        bar.update(100)
        bar.close()
        return f"/cache/{repo_id}"

    monkeypatch.setattr(models, "PROGRESS_INTERVAL", 0)
    monkeypatch.setattr(models, "_repo_bytes", lambda repo, patterns, fallback=0: 100)
    monkeypatch.setattr(models.huggingface_hub, "try_to_load_from_cache", lambda repo, filename: None)
    monkeypatch.setattr(models.huggingface_hub, "snapshot_download", fake_download)

    assert models.ensure_model("parakeet-v3", on_progress=fractions.append) == f"/cache/{info.repo}"
    assert calls == [
        {"repo_id": info.repo, "allow_patterns": list(info.files)},
        {"repo_id": models.VAD_REPO, "allow_patterns": [models.VAD_FILE]},
    ]
    assert fractions == sorted(fractions) and fractions[-1] == 1.0
