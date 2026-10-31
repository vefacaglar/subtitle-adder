"""Model kataloğu ve indirme ilerlemesi testleri: ağa çıkmaz, HF çağrıları monkeypatch'lenir."""
import pytest

from subtitle_engine import models


def test_catalog_order_and_fields():
    assert [m.id for m in models.KATALOG] == ["base", "small", "medium", "large-v3-turbo", "large-v3"]
    assert all(m.repo for m in models.KATALOG)
    assert all(m.size_mb > 0 for m in models.KATALOG)
    assert all(1 <= m.speed <= 5 and 1 <= m.accuracy <= 5 for m in models.KATALOG)
    assert [m.size_mb for m in models.KATALOG] == sorted(m.size_mb for m in models.KATALOG)


def test_catalog_repos_match_faster_whisper():
    """Katalogdaki repo kimlikleri faster-whisper'ın kendi tablosuyla aynı kalmalı."""
    from faster_whisper.utils import _MODELS

    assert {m.id: m.repo for m in models.KATALOG} == {m.id: _MODELS[m.id] for m in models.KATALOG}


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
