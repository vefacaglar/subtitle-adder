"""Model kataloğu, önbellek boyutu, silme ve indirme ilerlemesi testleri: ağa çıkmaz."""
import hashlib
import os
import shutil

import pytest

from subtitle_engine import models

MIB = 1024 * 1024


def make_cached_repo(cache_dir, repo_id, files, revision="1" * 40):
    """Gerçek HF önbellek yapısını taklit eder: blobs + snapshots + refs (ağ yok)."""
    repo = cache_dir / f"models--{repo_id.replace('/', '--')}"
    blobs, snapshot = repo / "blobs", repo / "snapshots" / revision
    blobs.mkdir(parents=True, exist_ok=True)
    snapshot.mkdir(parents=True, exist_ok=True)
    for name, size in files.items():
        blob = blobs / hashlib.sha256(f"{repo_id}/{revision}/{name}".encode()).hexdigest()
        blob.write_bytes(b"x" * size)
        target = snapshot / name
        target.parent.mkdir(parents=True, exist_ok=True)
        try:
            target.symlink_to(os.path.relpath(blob, target.parent))
        except OSError:  # Windows'ta sembolik bağ izni olmayabilir
            shutil.copyfile(blob, target)
    refs = repo / "refs"
    refs.mkdir(exist_ok=True)
    (refs / "main").write_text(revision)
    return repo


def test_catalog_order_and_fields():
    assert [m.id for m in models.KATALOG] == [
        "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3",
        "nllb-1.3b", "madlad-3b",
    ]
    assert all(m.repo for m in models.KATALOG)
    assert all(m.size_mb > 0 for m in models.KATALOG)
    assert all(1 <= m.speed <= 5 and 1 <= m.accuracy <= 5 for m in models.KATALOG)
    for kind in ("transcription", "translation"):
        sizes = [m.size_mb for m in models.KATALOG if m.kind == kind]
        assert sizes == sorted(sizes)  # her grup arayüzde küçükten büyüğe


def test_catalog_backends_and_languages():
    by_id = {m.id: m for m in models.KATALOG}
    whisper = ("base", "small", "medium", "large-v3-turbo", "large-v3")
    onnx = ("parakeet-v3", "canary-1b-v2", "gigaam-v3")
    assert all(by_id[m].backend == "whisper" and by_id[m].languages is None for m in whisper)
    assert all(by_id[m].backend == "onnx" for m in onnx)
    assert all(by_id[m].kind == "transcription" for m in (*whisper, *onnx))
    assert {"pt", "es", "ru"} <= set(by_id["parakeet-v3"].languages)
    assert {"pt", "es", "ru"} <= set(by_id["canary-1b-v2"].languages)
    assert by_id["gigaam-v3"].languages == ("ru",)
    assert all(by_id[m].onnx_model and by_id[m].quantization and by_id[m].files for m in onnx)


def test_translation_catalog_kind_languages_and_licenses():
    by_id = {m.id: m for m in models.KATALOG}
    for model_id in ("madlad-3b", "nllb-1.3b"):
        info = by_id[model_id]
        assert info.kind == "translation"
        assert info.backend == "ctranslate2"
        assert {"tr", "pt", "es", "ru", "en"} <= set(info.languages)
        assert set(models.TRANSLATION_FILES) <= set(info.files)
    assert by_id["madlad-3b"].files == models.MADLAD_FILES
    assert "spiece.model" in by_id["madlad-3b"].files
    assert by_id["madlad-3b"].license == "Apache-2.0"
    assert by_id["nllb-1.3b"].license == "CC-BY-NC-4.0"


def test_catalog_licenses_are_explicit():
    licenses = {m.id: m.license for m in models.KATALOG}
    assert licenses["base"] == licenses["small"] == licenses["medium"] == "MIT"
    assert licenses["large-v3"] == licenses["large-v3-turbo"] == "MIT"
    assert licenses["gigaam-v3"] == "MIT"
    assert licenses["parakeet-v3"] == licenses["canary-1b-v2"] == "CC-BY-4.0"
    assert all(licenses.values())


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
    message = models.support_error(models.find("madlad-3b"), "de")
    assert message.startswith("MADLAD de dilini desteklemiyor") and "es" in message


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


def test_translation_download_state_requires_all_files(monkeypatch):
    info = models.find("madlad-3b")
    present = {(info.repo, name) for name in info.files}
    monkeypatch.setattr(
        models.huggingface_hub,
        "try_to_load_from_cache",
        lambda repo, filename: "/cache/file" if (repo, filename) in present else None,
    )
    monkeypatch.setattr(
        models.huggingface_hub, "snapshot_download", lambda *a, **kw: pytest.fail("ağa çıkılmamalı")
    )

    assert models.is_downloaded("madlad-3b") is True
    present.remove((info.repo, "tokenizer.json"))
    assert models.is_downloaded("madlad-3b") is False
    assert models.needs_download("madlad-3b") is True


def test_ensure_translation_model_downloads_only_its_files(monkeypatch):
    info = models.find("nllb-1.3b")
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

    assert models.ensure_model("nllb-1.3b", on_progress=fractions.append) == f"/cache/{info.repo}"
    assert calls == [{"repo_id": info.repo, "allow_patterns": list(info.files)}]
    assert fractions == sorted(fractions) and fractions[-1] == 1.0


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


def test_cached_disk_mb_reads_real_repo_sizes(tmp_path):
    make_cached_repo(
        tmp_path,
        "Systran/faster-whisper-base",
        {"model.bin": 2 * MIB + 600_000, "config.json": 1000},
    )
    make_cached_repo(tmp_path, models.VAD_REPO, {models.VAD_FILE: 2_327_554})

    sizes = models.cached_disk_mb(tmp_path)

    assert sizes == {
        "Systran/faster-whisper-base": 3,
        models.VAD_REPO: 2,
    }


def test_disk_mb_is_null_for_models_not_downloaded(tmp_path, monkeypatch):
    make_cached_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": 2 * MIB})
    sizes = models.cached_disk_mb(tmp_path)
    monkeypatch.setattr(models, "is_downloaded", lambda model_id: model_id == "base")

    assert models.disk_mb("base", sizes) == 2
    assert models.disk_mb("small", sizes) is None
    assert models.disk_mb("turbo", sizes) is None


def test_cached_disk_mb_is_empty_without_cache_dir(tmp_path):
    assert models.cached_disk_mb(tmp_path / "yok") == {}


def test_delete_model_removes_all_revisions_and_keeps_other_repos(tmp_path):
    target = make_cached_repo(tmp_path, "istupakov/gigaam-v3-onnx", {"encoder.onnx": 400_000})
    make_cached_repo(tmp_path, "istupakov/gigaam-v3-onnx", {"encoder.onnx": 600_000}, revision="2" * 40)
    other = make_cached_repo(tmp_path, "Systran/faster-whisper-small", {"model.bin": 900_000})
    vad = make_cached_repo(tmp_path, models.VAD_REPO, {models.VAD_FILE: 2_327_554})

    assert models.delete_model("gigaam-v3", cache_dir=tmp_path) == 1

    assert not target.exists()
    assert other.is_dir() and vad.is_dir()
    assert models.cached_disk_mb(tmp_path) == {
        "Systran/faster-whisper-small": 1,
        models.VAD_REPO: 2,
    }


def test_delete_model_cleans_up_repo_without_revisions(tmp_path):
    repo = tmp_path / "models--Systran--faster-whisper-base"
    (repo / "snapshots").mkdir(parents=True)
    (repo / "blobs").mkdir()
    (repo / "blobs" / "partial.incomplete").write_bytes(b"x" * 1000)

    assert models.delete_model("base", cache_dir=tmp_path) == 0
    assert not repo.exists()


def test_delete_model_errors_are_turkish(tmp_path):
    with pytest.raises(models.ModelError, match="Bilinmeyen model: turbo"):
        models.delete_model("turbo", cache_dir=tmp_path)
    with pytest.raises(models.ModelError, match="indirilmemiş"):
        models.delete_model("small", cache_dir=tmp_path)
    with pytest.raises(models.ModelError, match="indirilmemiş"):
        models.delete_model("small", cache_dir=tmp_path / "yok")


def test_delete_model_removes_translation_repo(tmp_path):
    target = make_cached_repo(
        tmp_path, "OpenNMT/nllb-200-distilled-1.3B-ct2-int8", {"model.bin": 1_000_000}
    )
    other = make_cached_repo(tmp_path, "Systran/faster-whisper-small", {"model.bin": 900_000})

    assert models.delete_model("nllb-1.3b", cache_dir=tmp_path) == 1
    assert not target.exists() and other.is_dir()


def test_translation_model_dir_returns_cached_snapshot(tmp_path, monkeypatch):
    repo = make_cached_repo(tmp_path, "Nextcloud-AI/madlad400-3b-mt-ct2-int8", {"model.bin": 1000})
    snapshot = repo / "snapshots" / ("1" * 40)
    monkeypatch.setattr(
        models.huggingface_hub,
        "try_to_load_from_cache",
        lambda repo_id, filename: str(snapshot / filename)
        if repo_id == "Nextcloud-AI/madlad400-3b-mt-ct2-int8"
        else None,
    )

    assert models.model_dir("madlad-3b") == snapshot


def test_delete_model_refuses_catalog_entries_sharing_a_repo(tmp_path, monkeypatch):
    repo = make_cached_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": 1000})
    clone = models.ModelInfo(
        id="base-kopya", repo="Systran/faster-whisper-base", size_mb=1, speed=1, accuracy=1
    )
    monkeypatch.setattr(models, "KATALOG", models.KATALOG + (clone,))
    monkeypatch.setattr(models, "KATALOG_BY_ID", {m.id: m for m in models.KATALOG})

    with pytest.raises(models.ModelError, match="paylaşıyor"):
        models.delete_model("base", cache_dir=tmp_path)
    assert (repo / "blobs").is_dir()
