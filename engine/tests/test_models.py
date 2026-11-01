"""Tests for the model catalog, cache size, deletion and download progress: no network."""
import contextlib
import hashlib
import os
import shutil

import pytest

from subtitle_engine import models

MIB = 1024 * 1024


def make_cached_repo(cache_dir, repo_id, files, revision="1" * 40):
    """Imitates the real HF cache layout: blobs + snapshots + refs (no network)."""
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
        except OSError:  # symlink permission may be missing on Windows
            shutil.copyfile(blob, target)
    refs = repo / "refs"
    refs.mkdir(exist_ok=True)
    (refs / "main").write_text(revision)
    return repo


def test_catalog_order_and_fields():
    assert [m.id for m in models.KATALOG] == [
        "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3",
        "nllb-1.3b", "madlad-3b", "speaker-diarization",
    ]
    assert all(m.repo for m in models.KATALOG)
    assert all(m.size_mb > 0 for m in models.KATALOG)
    assert all(1 <= m.speed <= 5 and 1 <= m.accuracy <= 5 for m in models.KATALOG)
    for kind in ("transcription", "translation"):
        sizes = [m.size_mb for m in models.KATALOG if m.kind == kind]
        assert sizes == sorted(sizes)  # each group is small to large in the UI


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
    """Whisper repo ids must stay identical to faster-whisper's own table."""
    from faster_whisper.utils import _MODELS

    whisper = [m for m in models.KATALOG if m.backend == "whisper"]
    assert {m.id: m.repo for m in whisper} == {m.id: _MODELS[m.id] for m in whisper}


def test_catalog_repos_match_onnx_asr():
    """onnx models must point to the Hugging Face repos that onnx-asr downloads."""
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
        raise AssertionError("is_downloaded must not hit the network")

    monkeypatch.setattr(models.huggingface_hub, "try_to_load_from_cache", fake_cache)
    monkeypatch.setattr(models.huggingface_hub, "snapshot_download", fail_download)

    assert models.is_downloaded("small") is True
    assert models.is_downloaded("base") is False
    assert models.needs_download("base") is True
    assert models.needs_download("small") is False
    assert models.is_downloaded("turbo") is False  # outside the catalog: no cache lookup
    assert models.needs_download("turbo") is False  # no download stage is published


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
    assert fractions == sorted(fractions)  # increasing
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
        models.huggingface_hub, "snapshot_download", lambda *a, **kw: pytest.fail("must not hit the network")
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
        models.huggingface_hub, "snapshot_download", lambda *a, **kw: pytest.fail("must not hit the network")
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


def make_shared_repo(cache_dir, repo_id, files, revision="1" * 40):
    """huggingface_hub 2.x xet layout: blobs live under `<cache>/blobs/<2 hex>/<sha256>`, the repo's blobs/ is a symlink.

    `files`: name -> (content key, bytes); the same key in two repos yields the same shared blob.
    """
    store = cache_dir / "blobs"
    store.mkdir(exist_ok=True)
    (store / ".huggingface-shared-blobs").write_text("1\n")
    repo = cache_dir / f"models--{repo_id.replace('/', '--')}"
    blobs, snapshot = repo / "blobs", repo / "snapshots" / revision
    blobs.mkdir(parents=True, exist_ok=True)
    snapshot.mkdir(parents=True, exist_ok=True)
    for name, (key, size) in files.items():
        digest = hashlib.sha256(key.encode()).hexdigest()
        payload = store / digest[:2] / digest
        payload.parent.mkdir(exist_ok=True)
        payload.write_bytes(b"x" * size)
        with (store / digest[:2] / f"{digest}.refs").open("a") as refs:
            refs.write(f"{repo.name}/blobs/{digest}\n")
        (store / digest[:2] / f"{digest}.lock").write_bytes(b"")
        link = blobs / digest
        if not link.is_symlink():
            link.symlink_to(os.path.relpath(payload, blobs))
        (snapshot / name).symlink_to(os.path.relpath(link, snapshot))
    (repo / "refs").mkdir(exist_ok=True)
    (repo / "refs" / "main").write_text(revision)
    return repo, store


def test_disk_mb_counts_shared_store_targets_once(tmp_path):
    # The same blob under two file names and in two revisions: counted once.
    files = {"model.bin": ("a", 3 * MIB), "alias.bin": ("a", 3 * MIB), "config.json": ("b", 1000)}
    make_shared_repo(tmp_path, "Systran/faster-whisper-base", files)
    make_shared_repo(tmp_path, "Systran/faster-whisper-base", files, revision="2" * 40)
    assert models.cached_disk_mb(tmp_path) == {"Systran/faster-whisper-base": 3}


def test_delete_model_sweeps_shared_blobs_but_keeps_ones_other_repos_use(tmp_path):
    repo, store = make_shared_repo(
        tmp_path,
        "Systran/faster-whisper-base",
        {"model.bin": ("only-base", 3 * MIB), "tokenizer.json": ("common", 2 * MIB)},
    )
    other, _ = make_shared_repo(
        tmp_path,
        "Systran/faster-whisper-small",
        {"model.bin": ("only-small", 2 * MIB), "tokenizer.json": ("common", 2 * MIB)},
    )
    vad, _ = make_shared_repo(tmp_path, models.VAD_REPO, {models.VAD_FILE: ("vad", 1 * MIB)})
    digest = lambda key: hashlib.sha256(key.encode()).hexdigest()  # noqa: E731
    payload = lambda key: store / digest(key)[:2] / digest(key)  # noqa: E731

    assert models.delete_model("base", cache_dir=tmp_path) == 3  # only the blob owned by base is freed

    assert not repo.exists()
    assert not payload("only-base").exists()
    assert not payload("only-base").with_name(digest("only-base") + ".refs").exists()
    assert payload("only-base").with_name(digest("only-base") + ".lock").exists()  # the lock file is not touched
    assert payload("common").is_file() and payload("only-small").is_file() and payload("vad").is_file()
    assert other.is_dir() and vad.is_dir()
    assert (other / "snapshots" / ("1" * 40) / "tokenizer.json").read_bytes() == b"x" * (2 * MIB)


def test_delete_model_frees_shared_blob_when_last_user_is_deleted(tmp_path):
    make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("common", 2 * MIB)})
    make_shared_repo(tmp_path, "Systran/faster-whisper-small", {"model.bin": ("common", 2 * MIB)})

    assert models.delete_model("base", cache_dir=tmp_path) == 0
    assert models.delete_model("small", cache_dir=tmp_path) == 2
    assert [p for p in (tmp_path / "blobs").rglob("*") if p.is_file() and p.suffix == "" and p.name[0] != "."] == []


def test_delete_model_leaks_shared_blob_without_store_marker(tmp_path):
    repo, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", MIB)})
    (store / ".huggingface-shared-blobs").unlink()

    assert models.delete_model("base", cache_dir=tmp_path) == 0
    assert not repo.exists()
    assert any(p.is_file() and len(p.name) == 64 for p in store.rglob("*"))


def test_delete_model_legacy_layout_counts_repo_blobs(tmp_path):
    repo = make_cached_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": 3 * MIB})
    assert models.delete_model("base", cache_dir=tmp_path) == 3
    assert not repo.exists()


@pytest.fixture(params=["hf_sweep", "fallback"])
def sweep_mode(request, monkeypatch):
    if request.param == "fallback":
        monkeypatch.setattr(models, "_hf_sweep", lambda: None)
    return request.param


def _payload(store, key):
    d = hashlib.sha256(key.encode()).hexdigest()
    return store / d[:2] / d


def test_sweep_both_paths_delete_unshared(tmp_path, sweep_mode):
    _, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", 2 * MIB)})
    assert models.delete_model("base", cache_dir=tmp_path) == 2
    assert not _payload(store, "a").exists()


def test_link_pointing_outside_cache_is_not_deleted(tmp_path, sweep_mode):
    cache = tmp_path / "cache"
    cache.mkdir()
    outside = tmp_path / "disari.bin"
    outside.write_bytes(b"x" * MIB)
    repo, _ = make_shared_repo(cache, "Systran/faster-whisper-base", {"model.bin": ("a", MIB)})
    (repo / "blobs" / "abs").symlink_to(outside)
    (repo / "blobs" / "rel").symlink_to(os.path.relpath(outside, repo / "blobs"))

    models.delete_model("base", cache_dir=cache)

    assert outside.read_bytes() == b"x" * MIB


def test_reference_from_dataset_folder_keeps_blob(tmp_path, sweep_mode):
    _, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", MIB)})
    d = hashlib.sha256(b"a").hexdigest()
    ds = tmp_path / "datasets--a--b" / "blobs"
    ds.mkdir(parents=True)
    (ds / d).symlink_to(os.path.relpath(_payload(store, "a"), ds))
    with (store / d[:2] / f"{d}.refs").open("a") as refs:
        refs.write(f"datasets--a--b/blobs/{d}\n")

    assert models.delete_model("base", cache_dir=tmp_path) == 0
    assert _payload(store, "a").is_file()


def test_unreadable_other_repo_blocks_deletion(tmp_path, sweep_mode):
    _, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", MIB)})
    other, _ = make_shared_repo(tmp_path, "Systran/faster-whisper-small", {"model.bin": ("b", MIB)})
    blobs = other / "blobs"
    blobs.chmod(0)
    try:
        if os.access(blobs, os.R_OK):
            pytest.skip("permissions not enforced (root/Windows)")
        with contextlib.suppress(models.ModelError):  # the scan may fail on an unreadable repo
            models.delete_model("base", cache_dir=tmp_path)
        # Even if the scan is skipped, the sweeper deletes nothing while an unreadable repo exists.
        assert models._sweep_shared_blobs(tmp_path.resolve(), {_payload(tmp_path.resolve() / "blobs", "a")}) == 0
    finally:
        blobs.chmod(0o755)
    assert _payload(store, "a").is_file()


@pytest.mark.parametrize("content", ["2\n", "1", ""])
def test_marker_content_mismatch_deletes_nothing(tmp_path, sweep_mode, content):
    _, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", MIB)})
    (store / ".huggingface-shared-blobs").write_text(content)
    assert models.delete_model("base", cache_dir=tmp_path) == 0
    assert _payload(store, "a").is_file()


def test_marker_symlink_deletes_nothing(tmp_path, sweep_mode):
    _, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", MIB)})
    real = tmp_path / "gercek"
    real.write_text("1\n")
    (store / ".huggingface-shared-blobs").unlink()
    (store / ".huggingface-shared-blobs").symlink_to(real)
    assert models.delete_model("base", cache_dir=tmp_path) == 0
    assert _payload(store, "a").is_file()


def test_regular_repo_blob_with_shared_store_present(tmp_path, sweep_mode):
    repo, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", 2 * MIB)})
    (repo / "blobs" / ("f" * 40)).write_bytes(b"x" * (3 * MIB))  # regular file (not xet)

    assert models.delete_model("base", cache_dir=tmp_path) == 5
    assert not repo.exists() and not _payload(store, "a").exists()


def test_delete_failure_raises_turkish_error(tmp_path, monkeypatch):
    repo, store = make_shared_repo(tmp_path, "Systran/faster-whisper-base", {"model.bin": ("a", MIB)})
    monkeypatch.setattr(models, "_remove_repo_leftovers", lambda *a: None)

    with pytest.raises(models.ModelError, match="tamamen silinemedi"):
        models.delete_model("base", cache_dir=tmp_path)
    assert _payload(store, "a").is_file()
