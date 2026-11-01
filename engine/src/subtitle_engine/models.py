"""Engine data types and the on-device model catalog.

The catalog feeds the app's model list (approximate size, relative speed/accuracy, supported
languages) and download status. Download progress is only published for catalog models;
names outside the catalog (like "turbo", "tiny") are left to faster-whisper's own download.
"""
import os
import re
import shutil
import stat
import time
from collections.abc import Callable, Sequence
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Literal

import huggingface_hub
from huggingface_hub.constants import HF_HUB_CACHE
from huggingface_hub.errors import CacheNotFound
from huggingface_hub.utils import filter_repo_objects
from tqdm import tqdm

from .i18n import MESSAGES, t

_SHARED_BLOBS_DIR = "blobs"  # huggingface_hub: cache-wide shared (xet) blob store
_SHARED_MARKER = ".huggingface-shared-blobs"
_SHARED_HASH = re.compile(r"[0-9a-f]{64}")


@dataclass(frozen=True)
class Word:
    start: float
    end: float
    text: str


@dataclass
class Transcript:
    language: str
    duration: float
    words: list[Word]


@dataclass
class Sentence:
    id: int
    start: float
    end: float
    text: str
    words: list[Word] = field(default_factory=list)
    speaker: int | None = None  # 0-based speaker; None when diarization was off

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> "Sentence":
        words = [Word(**w) for w in data.get("words", [])]
        return cls(
            id=data["id"],
            start=data["start"],
            end=data["end"],
            text=data["text"],
            words=words,
            speaker=data.get("speaker"),
        )


# --- Model catalog -------------------------------------------------------------

ProgressFn = Callable[[float], None]

# faster_whisper.utils.download_model does not download files outside these patterns;
# using the same list keeps download size and progress ratio consistent.
ALLOW_PATTERNS = ("config.json", "preprocessor_config.json", "model.bin", "tokenizer.json", "vocabulary.*")

# CTranslate2 translation models: weights, configuration and the shared vocabulary.
TRANSLATION_FILES = ("config.json", "model.bin", "shared_vocabulary.json", "tokenizer.json")
# The MADLAD T5 tokenizer ships as a sentencepiece model; the NLLB repo only has tokenizer.json.
MADLAD_FILES = (*TRANSLATION_FILES, "spiece.model")

# Translation languages declared in the catalog (ISO 639-1). The models know more;
# the UI and validation use this list.
TRANSLATION_LANGUAGES = ("tr", "en", "pt", "es", "ru")

# onnx-asr models use int8: both faster and smaller files.
VAD_REPO = "istupakov/silero-vad-onnx"
VAD_FILE = "silero_vad.onnx"

# Speaker diarization (sherpa-onnx): segmentation-3.0 int8 (~1.5 MB) and CAM++ embedding (~28 MB).
DIARIZATION_SEGMENTATION_REPO = "csukuangfj/sherpa-onnx-pyannote-segmentation-3-0"
DIARIZATION_SEGMENTATION_FILE = "model.int8.onnx"
DIARIZATION_EMBEDDING_REPO = "csukuangfj/speaker-embedding-models"
DIARIZATION_EMBEDDING_FILE = "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx"

# European languages supported by Parakeet TDT v3 and Canary 1B v2.
EUROPEAN_LANGUAGES = (
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it",
    "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
)

# Progress calls at most ~10 Hz so the UI event stream is not flooded.
PROGRESS_INTERVAL = 0.1

_MIB = 1024 * 1024

Backend = Literal["whisper", "onnx", "ctranslate2", "sherpa", "llama"]
ModelKind = Literal["transcription", "translation", "diarization"]


@dataclass(frozen=True)
class ModelInfo:
    """A model in the catalog. Display text such as names/descriptions lives in the app."""

    id: str
    repo: str  # whisper: faster_whisper.utils._MODELS; onnx: same as onnx_asr.resolver.model_repos
    size_mb: int  # approximate download size (fixed estimate)
    speed: int  # 1 (slowest) - 5 (fastest), relative
    accuracy: int  # 1 (lowest) - 5 (highest), relative
    backend: Backend = "whisper"
    languages: tuple[str, ...] | None = None  # None: supports all languages, like Whisper
    onnx_model: str | None = None  # onnx-asr model name (only backend="onnx")
    quantization: str | None = None  # onnx-asr quantization value
    files: tuple[str, ...] = ALLOW_PATTERNS  # file patterns to download within the repo
    kind: ModelKind = "transcription"  # translation models are listed separately in the UI
    license: str = "MIT"  # license from the model card (SPDX short name)
    extra: tuple[tuple[str, tuple[str, ...]], ...] = ()  # extra repos: (repo, files)
    owns_files_only: bool = False  # repos hold other models too: measure/delete only the listed files
    part_bytes: tuple[int, ...] = ()  # real download sizes aligned with _parts(), used as fallback estimates


# The order is the order of the list in the UI: small to large.
KATALOG: tuple[ModelInfo, ...] = (
    ModelInfo(id="base", repo="Systran/faster-whisper-base", size_mb=145, speed=5, accuracy=2, license="MIT"),
    ModelInfo(
        id="gigaam-v3",
        repo="istupakov/gigaam-v3-onnx",
        size_mb=216,
        speed=5,
        accuracy=4,
        backend="onnx",
        languages=("ru",),
        onnx_model="gigaam-v3-e2e-rnnt",
        quantization="int8",
        license="MIT",
        files=(
            "config.json",
            "v3_e2e_rnnt_vocab.txt",
            "v3_e2e_rnnt_encoder.int8.onnx",
            "v3_e2e_rnnt_decoder.int8.onnx",
            "v3_e2e_rnnt_joint.int8.onnx",
        ),
    ),
    ModelInfo(id="small", repo="Systran/faster-whisper-small", size_mb=465, speed=4, accuracy=3, license="MIT"),
    ModelInfo(
        id="parakeet-v3",
        repo="istupakov/parakeet-tdt-0.6b-v3-onnx",
        size_mb=640,
        speed=5,
        accuracy=4,
        backend="onnx",
        languages=EUROPEAN_LANGUAGES,
        onnx_model="nemo-parakeet-tdt-0.6b-v3",
        quantization="int8",
        license="CC-BY-4.0",
        files=(
            "config.json",
            "vocab.txt",
            "encoder-model.int8.onnx",
            "decoder_joint-model.int8.onnx",
        ),
    ),
    ModelInfo(
        id="canary-1b-v2",
        repo="istupakov/canary-1b-v2-onnx",
        size_mb=983,
        speed=2,
        accuracy=4,
        backend="onnx",
        languages=EUROPEAN_LANGUAGES,
        onnx_model="nemo-canary-1b-v2",
        quantization="int8",
        license="CC-BY-4.0",
        files=("config.json", "vocab.txt", "encoder-model.int8.onnx", "decoder-model.int8.onnx"),
    ),
    ModelInfo(id="medium", repo="Systran/faster-whisper-medium", size_mb=1500, speed=3, accuracy=4, license="MIT"),
    ModelInfo(
        id="large-v3-turbo",
        repo="mobiuslabsgmbh/faster-whisper-large-v3-turbo",
        size_mb=1600,
        speed=2,
        accuracy=4,
        license="MIT",
    ),
    ModelInfo(id="large-v3", repo="Systran/faster-whisper-large-v3", size_mb=3000, speed=1, accuracy=5, license="MIT"),
    ModelInfo(
        id="nllb-1.3b",
        repo="OpenNMT/nllb-200-distilled-1.3B-ct2-int8",
        size_mb=1336,
        speed=3,
        accuracy=4,
        backend="ctranslate2",
        languages=TRANSLATION_LANGUAGES,
        files=TRANSLATION_FILES,
        kind="translation",
        license="CC-BY-NC-4.0",  # no commercial use
    ),
    ModelInfo(
        id="qwen3.5-4b",
        repo="unsloth/Qwen3.5-4B-GGUF",
        size_mb=2614,
        speed=3,
        accuracy=4,
        backend="llama",
        languages=TRANSLATION_LANGUAGES,
        files=("Qwen3.5-4B-Q4_K_M.gguf",),
        kind="translation",
        license="Apache-2.0",
        owns_files_only=True,
        part_bytes=(2_740_937_888,),
    ),
    ModelInfo(
        id="madlad-3b",
        repo="Nextcloud-AI/madlad400-3b-mt-ct2-int8",
        size_mb=2839,
        speed=2,
        accuracy=5,
        backend="ctranslate2",
        languages=TRANSLATION_LANGUAGES,
        files=MADLAD_FILES,
        kind="translation",
        license="Apache-2.0",
    ),
    ModelInfo(
        id="gemma-4-e4b",
        repo="unsloth/gemma-4-E4B-it-GGUF",
        size_mb=4747,
        speed=2,
        accuracy=5,
        backend="llama",
        languages=TRANSLATION_LANGUAGES,
        files=("gemma-4-E4B-it-Q4_K_M.gguf",),
        kind="translation",
        license="Apache-2.0",
        owns_files_only=True,
        part_bytes=(4_977_171_584,),
    ),
    ModelInfo(
        id="qwen3.5-9b",
        repo="unsloth/Qwen3.5-9B-GGUF",
        size_mb=5417,
        speed=2,
        accuracy=5,
        backend="llama",
        languages=TRANSLATION_LANGUAGES,
        files=("Qwen3.5-9B-Q4_K_M.gguf",),
        kind="translation",
        license="Apache-2.0",
        owns_files_only=True,
        part_bytes=(5_680_522_464,),
    ),
    ModelInfo(
        id="gemma-4-12b",
        repo="unsloth/gemma-4-12b-it-GGUF",
        size_mb=6792,
        speed=1,
        accuracy=5,
        backend="llama",
        languages=TRANSLATION_LANGUAGES,
        files=("gemma-4-12b-it-Q4_K_M.gguf",),
        kind="translation",
        license="Apache-2.0",
        owns_files_only=True,
        part_bytes=(7_121_861_440,),
    ),
    ModelInfo(
        # Speaker diarization: pyannote segmentation-3.0 (MIT) + 3D-Speaker CAM++ embedding model
        # (Apache-2.0), run by sherpa-onnx. Both repos belong to this entry only.
        id="speaker-diarization",
        repo=DIARIZATION_SEGMENTATION_REPO,
        size_mb=29,
        speed=4,
        accuracy=3,
        backend="sherpa",
        files=(DIARIZATION_SEGMENTATION_FILE,),
        kind="diarization",
        license="MIT AND Apache-2.0",
        extra=((DIARIZATION_EMBEDDING_REPO, (DIARIZATION_EMBEDDING_FILE,)),),
        owns_files_only=True,  # the embedding repo holds dozens of unrelated models
        part_bytes=(1_540_506, 28_281_164),
    ),
)
KATALOG_BY_ID = {m.id: m for m in KATALOG}

DISPLAY_NAMES = {
    "gigaam-v3": "GigaAM",
    "parakeet-v3": "Parakeet",
    "canary-1b-v2": "Canary",
    "madlad-3b": "MADLAD",
    "nllb-1.3b": "NLLB",
    "qwen3.5-4b": "Qwen 3.5 4B",
    "gemma-4-e4b": "Gemma 4 E4B",
    "qwen3.5-9b": "Qwen 3.5 9B",
    "gemma-4-12b": "Gemma 4 12B",
}


def display_name(model_id: str) -> str:
    """Name shown to the user (the diarization model's name follows the UI language)."""
    if model_id == "speaker-diarization":
        return t("name_speaker_diarization")
    return DISPLAY_NAMES.get(model_id, model_id)


def language_name(code: str) -> str:
    """Language name in the UI language; unknown codes are returned as is."""
    key = f"lang_{code}"
    return t(key) if key in MESSAGES else code


class ModelError(Exception):
    """Model error with a localized message that can be shown to the user (the CLI turns it into an `error` event)."""


def find(model_id: str) -> ModelInfo | None:
    return KATALOG_BY_ID.get(model_id)


def _parts(info: ModelInfo) -> list[tuple[str, tuple[str, ...]]]:
    """All repos and files of a model: the main repo plus `extra`."""
    return [(info.repo, info.files), *info.extra]


def is_downloaded(model_id: str) -> bool:
    """Is the model in the local Hugging Face cache? Only looks at disk, no network."""
    info = find(model_id)
    if info is None:
        return False
    if info.kind == "diarization":
        return all(_cached(repo, name) for repo, files in _parts(info) for name in files)
    if info.kind == "translation":
        # Weights, configuration, vocabulary and tokenizer are all required.
        return all(_cached(info.repo, name) for name in info.files)
    if info.backend == "onnx":
        # onnx-asr queries files one by one; the VAD is a shared, small dependency.
        return all(_cached(info.repo, name) for name in info.files) and _cached(VAD_REPO, VAD_FILE)
    return _cached(info.repo, "model.bin")


def _cached(repo: str, filename: str) -> bool:
    return isinstance(huggingface_hub.try_to_load_from_cache(repo, filename), str)


OFFLINE_ENV_VAR = "SUBTITLE_ENGINE_OFFLINE"


def offline_requested() -> bool:
    """Strict offline mode (`--offline` or `SUBTITLE_ENGINE_OFFLINE=1`): the engine never uses the network."""
    return os.environ.get(OFFLINE_ENV_VAR, "").strip().lower() in ("1", "true", "yes", "on")


def go_offline() -> None:
    """Locks the Hugging Face libraries out of the network for the rest of the process.

    Called once the models a command needs are on disk (and at start in strict offline mode): every Hub
    request (update checks, telemetry) then fails immediately instead of being attempted.
    """
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
    huggingface_hub.constants.HF_HUB_OFFLINE = True
    huggingface_hub.constants.HF_HUB_DISABLE_TELEMETRY = True


def require_download(model_id: str) -> bool:
    """Like `needs_download`, but in strict offline mode a missing model is an error instead of a download."""
    missing = needs_download(model_id)
    if missing and offline_requested():
        raise ValueError(t("offline_missing_model", model=display_name(model_id)))
    return missing


def needs_download(model_id: str) -> bool:
    """Should a download stage be published: a model that is in the catalog but not in the cache."""
    return find(model_id) is not None and not is_downloaded(model_id)


def _scan_cache(cache_dir: str | Path | None = None):
    """Scans the local Hugging Face cache; returns None if the directory is missing. No network."""
    try:
        return huggingface_hub.scan_cache_dir(cache_dir or HF_HUB_CACHE)
    except (CacheNotFound, OSError, ValueError):
        return None


def _mb(size: int) -> int:
    return round(size / _MIB)


def cached_disk_mb(cache_dir: str | Path | None = None) -> dict[str, int]:
    """Actual disk usage (MB) of the model repos in the cache, by repo id."""
    cache = _scan_cache(cache_dir)
    if cache is None:
        return {}
    sizes = {repo.repo_id: _mb(repo.size_on_disk) for repo in cache.repos if repo.repo_type == "model"}
    # Models that own only some files of a shared repo: per-file sizes under "<repo>#<file>".
    owned = {repo: set(files) for info in KATALOG if info.owns_files_only for repo, files in _parts(info)}
    for repo in cache.repos:
        if repo.repo_type == "model" and repo.repo_id in owned:
            seen: dict[str, int] = {}
            for revision in repo.revisions:
                for file in revision.files:
                    if file.file_name in owned[repo.repo_id]:
                        seen[file.file_name] = file.size_on_disk
            sizes.update({f"{repo.repo_id}#{name}": _mb(size) for name, size in seen.items()})
    return sizes


def disk_mb(model_id: str, sizes: dict[str, int] | None = None) -> int | None:
    """Actual space (MB) taken by a downloaded model; None if not downloaded.

    `sizes` is the result of `cached_disk_mb()`; one scan is enough when building the list.
    """
    info = find(model_id)
    if info is None or not is_downloaded(model_id):
        return None
    table = cached_disk_mb() if sizes is None else sizes
    if info.owns_files_only:
        found = [table.get(f"{repo}#{name}") for repo, files in _parts(info) for name in files]
    else:
        found = [table.get(repo) for repo, _ in _parts(info)]
    if any(size is None for size in found):
        return None
    return sum(found)


def delete_model(model_id: str, cache_dir: str | Path | None = None) -> int:
    """Deletes the model's cached repos (all revisions and blobs) and returns the freed MB.

    Also cleans files in the shared blob store (`<cache>/blobs/`) that no other repo uses; those
    shared with another model stay. The return value is the space actually freed (it can be 0 for
    files shared with another model).

    Only touches the target repo folders: helper repos like the shared silero VAD and other
    models are preserved. Raises `ModelError` for a model outside the catalog or not downloaded.
    """
    info = find(model_id)
    if info is None:
        raise ModelError(t("unknown_model", model=model_id))

    # If other catalog entries share the same repo, deleting would break them too.
    mine = {repo for repo, _ in _parts(info)}
    shared = [
        other.id
        for other in KATALOG
        if other.id != info.id and mine & {repo for repo, _ in _parts(other)}
    ]
    if shared:
        raise ModelError(t("model_shared", model=info.id, others=", ".join(shared)))

    root = Path(cache_dir or HF_HUB_CACHE).expanduser().resolve()
    cache = _scan_cache(root)
    repos = []
    if cache is not None:
        repos = [
            (repo, found)
            for repo, _ in _parts(info)
            for found in cache.repos
            if found.repo_type == "model" and found.repo_id == repo
        ]
    if not repos:
        raise ModelError(t("model_not_downloaded_delete", model=info.id))

    freed = 0
    leftovers: list[str] = []
    for repo_id, repo in repos:
        try:
            if info.owns_files_only:
                owned = next(files for part, files in _parts(info) if part == repo_id)
                freed += _delete_owned_files(root, repo, repo_id, set(owned))
            else:
                freed += _delete_whole_repo(root, repo, repo_id)
        except OSError as exc:
            leftovers.append(f"{repo.repo_path} ({exc})")
        except ModelError as exc:
            leftovers.append(str(exc))
    if leftovers:
        # Keep going past a failing repo, then say honestly what happened instead of losing the freed bytes.
        raise ModelError(
            t("partial_delete", name=display_name(info.id), freed=_mb(freed), left="; ".join(leftovers))
        )
    return _mb(freed)


def _delete_whole_repo(root: Path, repo, repo_id: str) -> int:
    """Removes a repo folder entirely; returns the freed bytes (shared blobs swept afterwards)."""
    repo_path = repo.repo_path
    local_bytes, shared_targets = _inspect_repo_blobs(repo_path, root)
    # delete_revisions looks the commit id up in the whole cache; removing just this repo folder is safer.
    _remove_repo_leftovers(root, repo_path, repo_id)
    if repo_path.exists():
        raise ModelError(t("delete_incomplete", path=repo_path))
    return local_bytes + _sweep_shared_blobs(root, shared_targets)


def _delete_owned_files(root: Path, repo, repo_id: str, names: set[str]) -> int:
    """Deletes only `names` from a repo that other things may share: their snapshot entries and blobs.

    Blobs still referenced by another snapshot entry stay. Blobs in the shared store go through the
    usual sweep. The repo folder itself is removed only when nothing else remains in it.
    """
    repo_path = repo.repo_path
    doomed = [Path(f.file_path) for rev in repo.revisions for f in rev.files if f.file_name in names]
    kept_blobs = {
        _entry_blob(Path(f.file_path))
        for rev in repo.revisions
        for f in rev.files
        if Path(f.file_path) not in set(doomed)
    }
    freed, targets = 0, set()
    for link in doomed:
        blob = _entry_blob(link)  # the repo-local blob (or the file itself where symlinks are unavailable)
        if blob == link:  # copied file (no symlinks): it is its own data
            try:
                freed += link.stat().st_size
            except OSError:
                pass
        link.unlink(missing_ok=True)
        if blob == link or blob in kept_blobs:
            continue
        target = _shared_target(blob, root)
        if target is not None:
            targets.add(target)
        else:
            try:
                freed += blob.stat().st_size
            except OSError:
                pass
        blob.unlink(missing_ok=True)
    remaining = [p for p in (repo_path / "snapshots").rglob("*") if not p.is_dir()] if repo_path.exists() else []
    if not remaining:
        local, more = _inspect_repo_blobs(repo_path, root)
        _remove_repo_leftovers(root, repo_path, repo_id)
        if repo_path.exists():
            raise ModelError(t("delete_incomplete", path=repo_path))
        freed += local
        targets |= more
    return freed + _sweep_shared_blobs(root, targets)


def _entry_blob(link: Path) -> Path:
    try:
        return Path(os.path.normpath(link.parent / os.readlink(link))) if link.is_symlink() else link
    except OSError:
        return link


def _shared_target(link: Path, root: Path) -> Path | None:
    """If `link` is a symlink into the shared store (`<cache>/blobs/<2 hex>/<sha256>`), returns its target."""
    try:
        if not link.is_symlink():
            return None
        target = Path(os.path.normpath(link.parent / os.readlink(link)))
    except OSError:
        return None
    if (
        target.parent.parent != root / _SHARED_BLOBS_DIR
        or _SHARED_HASH.fullmatch(target.name) is None
        or target.parent.name != target.name[:2]
    ):
        return None
    return target


def _inspect_repo_blobs(repo_path: Path, root: Path) -> tuple[int, set[Path]]:
    """The repo's real bytes and shared-store targets: (bytes of real files owned by the repo, targets).

    Real files are the non-symlink files in `blobs/` and in `snapshots/`: where symlinks are
    unavailable (Windows without Developer Mode) huggingface_hub keeps the data in the snapshot
    itself, so `blobs/` is empty or holds only a duplicate.
    """
    local = 0
    targets: set[Path] = set()
    for folder, _, names in os.walk(repo_path / "snapshots", followlinks=False):
        for name in names:
            entry = Path(folder) / name
            try:
                if not entry.is_symlink():
                    local += entry.stat().st_size
            except OSError:
                pass
    try:
        entries = list((repo_path / "blobs").iterdir())
    except OSError:
        return 0, targets
    for entry in entries:
        target = _shared_target(entry, root)
        if target is not None:
            targets.add(target)
            continue
        try:
            if entry.is_file() and not entry.is_symlink():
                local += entry.stat().st_size
        except OSError:
            pass
    return local, targets


def _referenced_shared_blobs(root: Path) -> set[Path] | None:
    """Blob targets in the shared store referenced by all repos in the cache; None if unreadable (do not delete)."""
    referenced: set[Path] = set()
    try:
        repos = [p for p in root.iterdir() if p.name != _SHARED_BLOBS_DIR and p.is_dir() and "--" in p.name]
        for repo_dir in repos:
            blobs = repo_dir / "blobs"
            if not blobs.is_dir():
                continue
            for entry in blobs.iterdir():
                target = _shared_target(entry, root)
                if target is not None:
                    referenced.add(target)
    except OSError:
        return None
    return referenced


def _marker_ok(root: Path) -> bool:
    """Is the shared store in the layout we recognize: the marker file is a regular file whose content is exactly "1\\n"."""
    marker = root / _SHARED_BLOBS_DIR / _SHARED_MARKER
    try:
        return stat.S_ISREG(marker.lstat().st_mode) and marker.read_text() == "1\n"
    except (OSError, UnicodeError):
        return False


def _hf_sweep():
    """huggingface_hub's locked sweeper (private API); None if missing."""
    try:
        from huggingface_hub.utils import _shared_blobs

        func = _shared_blobs.sweep_shared_blob
        return func if callable(func) else None
    except (ImportError, AttributeError):
        return None


def _sweep_locked_fallback(root: Path, store_path: Path) -> int:
    """Under the `<sha>.lock` lock: deletes the blob and `.refs` if no other repo links it."""
    from filelock import FileLock

    lock = FileLock(store_path.with_name(store_path.name + ".lock"), mode=0o666)
    with lock:
        referenced = _referenced_shared_blobs(root)
        if referenced is None or store_path in referenced:
            return 0
        if store_path.is_symlink() or not store_path.is_file():
            return 0
        size = store_path.stat().st_size
        store_path.unlink()
        store_path.with_name(store_path.name + ".refs").unlink(missing_ok=True)
        return size


def _sweep_shared_blobs(root: Path, candidates: set[Path]) -> int:
    """Deletes the shared blobs of the deleted repo that no other repo links, and returns the bytes.

    First our own file system check (`.refs` is only a hint); then huggingface_hub's locked
    `sweep_shared_blob`, or if that is missing or its signature does not match, our own deletion
    under the `.lock` lock. If in doubt the blob is left alone. `.lock` files are never touched.
    """
    if not candidates or not _marker_ok(root):
        return 0
    referenced = _referenced_shared_blobs(root)
    if referenced is None:
        return 0
    sweep = _hf_sweep()
    freed = 0
    for store_path in candidates - referenced:
        try:
            if sweep is not None:
                try:
                    result = sweep(store_path, cache_dir=root)
                except TypeError:  # signature changed: fall back to our own locked path
                    result = _sweep_locked_fallback(root, store_path)
            else:
                result = _sweep_locked_fallback(root, store_path)
            freed += result if isinstance(result, int) else 0
        except OSError:
            continue
    return freed


def _remove_repo_leftovers(cache_root: Path, repo_path: Path, repo_id: str) -> None:
    """Deletes the target repo folder (revisions, blobs, `.incomplete` leftovers) only if it is inside the cache."""
    expected = f"models--{repo_id.replace('/', '--')}"
    if repo_path.name != expected or not repo_path.exists():
        return
    try:
        repo_path.resolve().relative_to(cache_root)  # path escaping the cache: do not touch
    except (OSError, ValueError):
        return
    for attempt in range(5):  # Windows: antivirus/indexer can briefly hold a freshly touched file
        shutil.rmtree(repo_path, ignore_errors=True)
        if not repo_path.exists():
            return
        time.sleep(0.1 * (attempt + 1))


def ensure_model(model_id: str, on_progress: ProgressFn | None = None) -> str | None:
    """Downloads the model if missing; reports 0..1 progress over total bytes to `on_progress`.

    Returns immediately if cached. Does nothing for names outside the catalog (returns None):
    faster-whisper downloads those itself while loading the model, and no progress is published.
    """
    info = find(model_id)
    if info is None or is_downloaded(model_id):
        return None
    if info.backend == "onnx":
        return _ensure_onnx(info, on_progress)
    if info.extra:
        return _ensure_parts(info, on_progress)

    report = _reporter(_total_bytes(info), on_progress)
    path = huggingface_hub.snapshot_download(
        info.repo,
        allow_patterns=list(info.files),
        tqdm_class=_progress_tqdm(report),
    )
    if on_progress is not None:
        on_progress(1.0)  # files are in place; the last report must not stay at 0.99
    return path


def _ensure_onnx(info: ModelInfo, on_progress: ProgressFn | None) -> str | None:
    """Downloads the onnx-asr model files and the shared silero VAD."""
    downloads: list[tuple[str, list[str], int]] = []
    if not all(_cached(info.repo, name) for name in info.files):
        downloads.append((info.repo, list(info.files), _repo_bytes(info.repo, info.files, info.size_mb * _MIB)))
    if not _cached(VAD_REPO, VAD_FILE):
        downloads.append((VAD_REPO, [VAD_FILE], _repo_bytes(VAD_REPO, (VAD_FILE,), 2 * _MIB)))

    report = _reporter(sum(size for _, _, size in downloads), on_progress)
    model_path: str | None = None
    base = 0
    for repo, patterns, size in downloads:
        path = huggingface_hub.snapshot_download(
            repo,
            allow_patterns=patterns,
            tqdm_class=_progress_tqdm(lambda downloaded, base=base: report(base + downloaded)),
        )
        if repo == info.repo:
            model_path = path
        base += size
    if on_progress is not None:
        on_progress(1.0)
    return model_path


def _fallback_bytes(info: ModelInfo, index: int) -> int:
    """Estimated download size of one part when the Hub cannot be queried."""
    if index < len(info.part_bytes):
        return info.part_bytes[index]
    return max(info.size_mb // len(_parts(info)), 1) * _MIB


def _ensure_parts(info: ModelInfo, on_progress: ProgressFn | None) -> str | None:
    """Downloads a multi-repo model (speaker diarization) through a single progress channel."""
    downloads = [
        (repo, list(files), _repo_bytes(repo, files, _fallback_bytes(info, index)))
        for index, (repo, files) in enumerate(_parts(info))
        if not all(_cached(repo, name) for name in files)
    ]
    report = _reporter(sum(size for _, _, size in downloads), on_progress)
    model_path: str | None = None
    base = 0
    for repo, patterns, size in downloads:
        path = huggingface_hub.snapshot_download(
            repo,
            allow_patterns=patterns,
            tqdm_class=_progress_tqdm(lambda downloaded, base=base: report(base + downloaded)),
        )
        if repo == info.repo:
            model_path = path
        base += size
    if on_progress is not None:
        on_progress(1.0)
    return model_path


def _repo_bytes(repo: str, patterns: Sequence[str], fallback: int = 0) -> int:
    """Total bytes of the files matching the patterns in the repo; an estimate if it cannot be queried."""
    try:
        siblings = huggingface_hub.HfApi().model_info(repo, files_metadata=True).siblings
    except Exception:  # network/HTTP error: the download itself produces its own error
        return fallback
    sizes = {f.rfilename: f.size or 0 for f in siblings}
    allowed = filter_repo_objects(list(sizes), allow_patterns=list(patterns))
    return sum(sizes.get(name, 0) for name in allowed) or fallback


def _total_bytes(info: ModelInfo) -> int:
    """Total bytes of the files to download in the repo; the catalog estimate if it cannot be queried."""
    fallback = info.part_bytes[0] if info.part_bytes else info.size_mb * _MIB
    return _repo_bytes(info.repo, info.files, fallback)


def model_dir(model_id: str) -> Path:
    """Local snapshot folder of a downloaded model; no network.

    CTranslate2 looks for the weights as a single file (model.bin); the folder path lives in the same
    snapshot. Raises a (localized) `ModelError` if the model is missing.
    """
    info = find(model_id)
    if info is None:
        raise ModelError(t("unknown_model", model=model_id))
    path = huggingface_hub.try_to_load_from_cache(info.repo, "model.bin")
    if not isinstance(path, str):
        raise ModelError(t("model_not_downloaded", model=info.id))
    return Path(path).parent


def model_file(model_id: str) -> Path:
    """Local cached file path of a single-file model (e.g. GGUF); no network."""
    info = find(model_id)
    if info is None or not info.files:
        raise ModelError(t("unknown_model", model=model_id))
    path = huggingface_hub.try_to_load_from_cache(info.repo, info.files[0])
    if not isinstance(path, str):
        raise ModelError(t("model_not_downloaded", model=info.id))
    return Path(path)


def support_error(info: ModelInfo, language: str) -> str:
    """Returns a (localized) explanation if the chosen language is not among those the model supports."""
    name = display_name(info.id)
    languages = info.languages or ()
    if len(languages) == 1:
        return t("only_language", name=name, language=language_name(languages[0]))
    return t("unsupported_language", name=name, language=language, supported=", ".join(languages))


def _reporter(total: int, on_progress: ProgressFn | None) -> Callable[[float], None]:
    """Turns the byte counter into 0..1 progress; never goes backwards, throttles calls."""
    if on_progress is None or total <= 0:
        return lambda downloaded: None
    state = {"fraction": 0.0, "at": 0.0}

    def report(downloaded: float) -> None:
        # Download bars can overtake each other, so only ensure_model reports 1.0.
        fraction = min(max(downloaded / total, 0.0), 0.99)
        if fraction <= state["fraction"]:
            return
        now = time.monotonic()
        if now - state["at"] < PROGRESS_INTERVAL:
            return
        state["fraction"], state["at"] = fraction, now
        on_progress(fraction)

    return report


def _progress_tqdm(report: Callable[[float], None]) -> type[tqdm]:
    """Wires snapshot_download's byte bars into a single progress channel."""

    class _ByteProgress(tqdm):
        def __init__(self, *args, **kwargs):
            # A disabled tqdm neither sets the `unit` field nor advances the `n` counter; we keep
            # our own counter and capture the unit at construction time.
            self._counts_bytes = kwargs.get("unit") == "B"
            self._bytes = kwargs.pop("initial", 0) or 0
            kwargs["disable"] = True  # keep bar output off stderr; progress goes through the protocol
            super().__init__(*args, **kwargs)

        def update(self, n: float | None = 1) -> None:
            super().update(n)
            if self._counts_bytes:  # transfer and reconstruction bars count bytes
                self._bytes += n or 0
                report(self._bytes)

    return _ByteProgress
