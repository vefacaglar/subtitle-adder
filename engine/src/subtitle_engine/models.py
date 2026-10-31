"""Motor veri tipleri ve on-device model kataloğu.

Katalog, arayüzün model listesini (yaklaşık boyut, göreli hız/doğruluk, desteklenen
diller) ve indirme durumunu besler. İndirme ilerlemesi yalnızca katalogdaki modeller
için yayınlanır; katalog dışı adlar ("turbo", "tiny" gibi) faster-whisper'ın kendi
indirmesine bırakılır.
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

_SHARED_BLOBS_DIR = "blobs"  # huggingface_hub: önbellek çapında paylaşılan (xet) blob deposu
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

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> "Sentence":
        words = [Word(**w) for w in data.get("words", [])]
        return cls(id=data["id"], start=data["start"], end=data["end"], text=data["text"], words=words)


# --- Model kataloğu -------------------------------------------------------------

ProgressFn = Callable[[float], None]

# faster_whisper.utils.download_model bu desenlerin dışındaki dosyaları indirmez;
# aynı listeyi kullanınca indirme boyutu ve ilerleme oranı örtüşür.
ALLOW_PATTERNS = ("config.json", "preprocessor_config.json", "model.bin", "tokenizer.json", "vocabulary.*")

# CTranslate2 çeviri modelleri: ağırlıklar, yapılandırma ve paylaşılan sözlük.
TRANSLATION_FILES = ("config.json", "model.bin", "shared_vocabulary.json", "tokenizer.json")
# MADLAD T5 tokenizer'ı sentencepiece modeliyle gelir; NLLB deposunda yalnız tokenizer.json var.
MADLAD_FILES = (*TRANSLATION_FILES, "spiece.model")

# Katalogda ilan edilen çeviri dilleri (ISO 639-1). Modeller daha fazlasını bilir;
# arayüz ve doğrulama bu listeyi kullanır.
TRANSLATION_LANGUAGES = ("tr", "en", "pt", "es", "ru")

# onnx-asr modelleri int8 kullanır: hem hızlı hem de dosyalar daha küçük.
VAD_REPO = "istupakov/silero-vad-onnx"
VAD_FILE = "silero_vad.onnx"

# Parakeet TDT v3 ve Canary 1B v2'nin desteklediği Avrupa dilleri.
EUROPEAN_LANGUAGES = (
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it",
    "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
)

# İlerleme çağrıları en fazla ~10 Hz: UI olay akışı boğulmasın.
PROGRESS_INTERVAL = 0.1

_MIB = 1024 * 1024

Backend = Literal["whisper", "onnx", "ctranslate2"]
ModelKind = Literal["transcription", "translation"]


@dataclass(frozen=True)
class ModelInfo:
    """Katalogdaki bir model. Görünen ad/açıklama gibi metinler uygulamada tutulur."""

    id: str
    repo: str  # whisper: faster_whisper.utils._MODELS; onnx: onnx_asr.resolver.model_repos ile aynı
    size_mb: int  # yaklaşık indirme boyutu (sabit tahmin)
    speed: int  # 1 (en yavaş) - 5 (en hızlı), göreli
    accuracy: int  # 1 (en düşük) - 5 (en yüksek), göreli
    backend: Backend = "whisper"
    languages: tuple[str, ...] | None = None  # None: Whisper gibi tüm dilleri destekler
    onnx_model: str | None = None  # onnx-asr model adı (yalnız backend="onnx")
    quantization: str | None = None  # onnx-asr quantization değeri
    files: tuple[str, ...] = ALLOW_PATTERNS  # repo içinde indirilecek dosya desenleri
    kind: ModelKind = "transcription"  # çeviri modelleri arayüzde ayrı listelenir
    license: str = "MIT"  # model kartındaki lisans (SPDX kısa adı)


# Sıra arayüzdeki liste sırasıdır: küçükten büyüğe.
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
        license="CC-BY-NC-4.0",  # ticari kullanım yok
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
)
KATALOG_BY_ID = {m.id: m for m in KATALOG}

DISPLAY_NAMES = {
    "gigaam-v3": "GigaAM",
    "parakeet-v3": "Parakeet",
    "canary-1b-v2": "Canary",
    "madlad-3b": "MADLAD",
    "nllb-1.3b": "NLLB",
}
TURKISH_LANGUAGE_NAMES = {
    "ru": "Rusça",
    "pt": "Portekizce",
    "es": "İspanyolca",
    "tr": "Türkçe",
    "en": "İngilizce",
}


class ModelError(Exception):
    """Kullanıcıya Türkçe gösterilebilir model hatası (CLI bunu `error` olayına çevirir)."""


def find(model_id: str) -> ModelInfo | None:
    return KATALOG_BY_ID.get(model_id)


def is_downloaded(model_id: str) -> bool:
    """Model yerel Hugging Face önbelleğinde mi? Yalnız diske bakar, ağa çıkmaz."""
    info = find(model_id)
    if info is None:
        return False
    if info.kind == "translation":
        # Ağırlık, yapılandırma, sözlük ve tokenizer birlikte gerekli.
        return all(_cached(info.repo, name) for name in info.files)
    if info.backend == "onnx":
        # onnx-asr dosyaları tek tek sorgular; VAD ortak ve küçük bir bağımlılık.
        return all(_cached(info.repo, name) for name in info.files) and _cached(VAD_REPO, VAD_FILE)
    return _cached(info.repo, "model.bin")


def _cached(repo: str, filename: str) -> bool:
    return isinstance(huggingface_hub.try_to_load_from_cache(repo, filename), str)


def needs_download(model_id: str) -> bool:
    """İndirme aşaması yayınlanmalı mı: katalogda olup önbellekte olmayan model."""
    return find(model_id) is not None and not is_downloaded(model_id)


def _scan_cache(cache_dir: str | Path | None = None):
    """Yerel Hugging Face önbelleğini tarar; dizin yoksa None döner. Ağa çıkmaz."""
    try:
        return huggingface_hub.scan_cache_dir(cache_dir or HF_HUB_CACHE)
    except (CacheNotFound, OSError, ValueError):
        return None


def _mb(size: int) -> int:
    return round(size / _MIB)


def cached_disk_mb(cache_dir: str | Path | None = None) -> dict[str, int]:
    """Önbellekteki model depolarının gerçek disk kullanımı (MB), repo kimliğine göre."""
    cache = _scan_cache(cache_dir)
    if cache is None:
        return {}
    return {repo.repo_id: _mb(repo.size_on_disk) for repo in cache.repos if repo.repo_type == "model"}


def disk_mb(model_id: str, sizes: dict[str, int] | None = None) -> int | None:
    """İndirilmiş modelin kapladığı gerçek alan (MB); indirilmemişse None.

    `sizes`, `cached_disk_mb()` sonucudur; liste üretirken tek tarama yeter.
    """
    info = find(model_id)
    if info is None or not is_downloaded(model_id):
        return None
    return (cached_disk_mb() if sizes is None else sizes).get(info.repo)


def delete_model(model_id: str, cache_dir: str | Path | None = None) -> int:
    """Modelin önbellekteki deposunu (tüm revizyonlar ve blob'lar) siler, boşalan MB'ı döndürür.

    Paylaşılan blob deposunda (`<önbellek>/blobs/`) başka hiçbir repo'nun kullanmadığı dosyaları da
    temizler; başka modelle paylaşılanlar kalır. Dönen değer gerçekten boşalan alandır (başka modelle
    paylaşılan dosyalarda 0 olabilir).

    Yalnız hedef repo klasörüne dokunur: ortak silero VAD gibi yardımcı depolar ve diğer
    modeller korunur. Katalog dışı ya da indirilmemiş modelde `ModelError` yükseltir.
    """
    info = find(model_id)
    if info is None:
        raise ModelError(f"Bilinmeyen model: {model_id}. Geçerli kimlikleri 'models' komutu listeler.")

    # Aynı repo'yu paylaşan katalog girdileri varsa silme diğerini de bozar.
    shared = [other.id for other in KATALOG if other.repo == info.repo and other.id != info.id]
    if shared:
        raise ModelError(f"{info.id} modeli {', '.join(shared)} ile aynı dosyaları paylaşıyor; silinmedi.")

    root = Path(cache_dir or HF_HUB_CACHE).expanduser().resolve()
    cache = _scan_cache(root)
    repo = None
    if cache is not None:
        repo = next(
            (r for r in cache.repos if r.repo_type == "model" and r.repo_id == info.repo),
            None,
        )
    if repo is None:
        raise ModelError(f"{info.id} modeli indirilmemiş; silinecek dosya yok.")

    repo_path = repo.repo_path
    local_bytes, shared_targets = _inspect_repo_blobs(repo_path, root)
    # delete_revisions commit kimliğini tüm önbellekte arar; yalnız bu repo klasörünü silmek daha güvenli.
    _remove_repo_leftovers(root, repo_path, info.repo)
    freed = local_bytes
    if repo_path.exists():
        raise ModelError(f"Model dosyaları tamamen silinemedi: {repo_path}")
    freed += _sweep_shared_blobs(root, shared_targets)
    return _mb(freed)


def _shared_target(link: Path, root: Path) -> Path | None:
    """`link` paylaşılan depoya (`<önbellek>/blobs/<2 hex>/<sha256>`) giden bir sembolik bağsa hedefini verir."""
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
    """Repo `blobs/` klasörü: (repo'ya ait gerçek dosyaların baytı, paylaşılan depodaki hedefler)."""
    local = 0
    targets: set[Path] = set()
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
    """Önbellekteki tüm repoların paylaşılan depoya bağlı blob hedefleri; okunamazsa None (silme)."""
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
    """Paylaşılan depo bizim tanıdığımız düzende mi: işaret dosyası düzenli dosya ve içeriği tam "1\\n"."""
    marker = root / _SHARED_BLOBS_DIR / _SHARED_MARKER
    try:
        return stat.S_ISREG(marker.lstat().st_mode) and marker.read_text() == "1\n"
    except (OSError, UnicodeError):
        return False


def _hf_sweep():
    """huggingface_hub'ın kilitli süpürücüsü (özel API); yoksa None."""
    try:
        from huggingface_hub.utils import _shared_blobs

        func = _shared_blobs.sweep_shared_blob
        return func if callable(func) else None
    except (ImportError, AttributeError):
        return None


def _sweep_locked_fallback(root: Path, store_path: Path) -> int:
    """`<sha>.lock` kilidi altında: başka repo bağlamıyorsa blob'u ve `.refs`i siler."""
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
    """Silinen repoya ait paylaşılan blob'lardan başka hiçbir repo'nun bağlamadığını siler, baytı döndürür.

    Önce kendi dosya sistemi denetimimiz (`.refs` yalnız ipucu); sonra huggingface_hub'ın kilitli
    `sweep_shared_blob`'u, o yoksa ya da imzası uymuyorsa `.lock` kilidi altında kendi silmemiz.
    Emin olunamazsa blob'a dokunulmaz. `.lock` dosyalarına hiç dokunulmaz.
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
                except TypeError:  # imza değişmiş: kendi kilitli yolumuza düş
                    result = _sweep_locked_fallback(root, store_path)
            else:
                result = _sweep_locked_fallback(root, store_path)
            freed += result if isinstance(result, int) else 0
        except OSError:
            continue
    return freed


def _remove_repo_leftovers(cache_root: Path, repo_path: Path, repo_id: str) -> None:
    """Hedef repo klasörünü (revizyonlar, blob'lar, `.incomplete` artıkları) yalnız önbellek içindeyse siler."""
    expected = f"models--{repo_id.replace('/', '--')}"
    if repo_path.name != expected or not repo_path.exists():
        return
    try:
        repo_path.resolve().relative_to(cache_root)  # önbellek dışına taşan yol: dokunma
    except (OSError, ValueError):
        return
    shutil.rmtree(repo_path, ignore_errors=True)


def ensure_model(model_id: str, on_progress: ProgressFn | None = None) -> str | None:
    """Model yoksa indirir; toplam bayt üzerinden 0..1 ilerlemeyi `on_progress`e verir.

    Önbellekteyse hemen döner. Katalog dışı adlarda hiçbir şey yapmaz (None döner):
    onları faster-whisper modeli yüklerken kendi indirir, ilerleme yayınlanmaz.
    """
    info = find(model_id)
    if info is None or is_downloaded(model_id):
        return None
    if info.backend == "onnx":
        return _ensure_onnx(info, on_progress)

    report = _reporter(_total_bytes(info), on_progress)
    path = huggingface_hub.snapshot_download(
        info.repo,
        allow_patterns=list(info.files),
        tqdm_class=_progress_tqdm(report),
    )
    if on_progress is not None:
        on_progress(1.0)  # dosyalar yerleşti; son bildirim 0.99'da kalmasın
    return path


def _ensure_onnx(info: ModelInfo, on_progress: ProgressFn | None) -> str | None:
    """onnx-asr model dosyalarını ve ortak silero VAD'ı indirir."""
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


def _repo_bytes(repo: str, patterns: Sequence[str], fallback: int = 0) -> int:
    """Depodaki desenlere uyan dosyaların toplam baytı; sorgulanamazsa tahmin."""
    try:
        siblings = huggingface_hub.HfApi().model_info(repo, files_metadata=True).siblings
    except Exception:  # ağ/HTTP hatası: indirme zaten kendi hatasını üretir
        return fallback
    sizes = {f.rfilename: f.size or 0 for f in siblings}
    allowed = filter_repo_objects(list(sizes), allow_patterns=list(patterns))
    return sum(sizes.get(name, 0) for name in allowed) or fallback


def _total_bytes(info: ModelInfo) -> int:
    """Depodaki indirilecek dosyaların toplam baytı; sorgulanamazsa katalog tahmini."""
    return _repo_bytes(info.repo, info.files, info.size_mb * _MIB)


def model_dir(model_id: str) -> Path:
    """İndirilmiş modelin yerel anlık görüntü klasörü; ağa çıkmaz.

    CTranslate2 ağırlıkları tek dosya (model.bin) olarak arar; klasör yolu da aynı
    anlık görüntüdedir. Model yoksa Türkçe `ModelError` yükseltir.
    """
    info = find(model_id)
    if info is None:
        raise ModelError(f"Bilinmeyen model: {model_id}. Geçerli kimlikleri 'models' komutu listeler.")
    path = huggingface_hub.try_to_load_from_cache(info.repo, "model.bin")
    if not isinstance(path, str):
        raise ModelError(f"{info.id} modeli indirilmemiş; önce indirilmesi gerekiyor.")
    return Path(path).parent


def support_error(info: ModelInfo, language: str) -> str:
    """Seçilen dil modelin destekledikleri arasında değilse Türkçe açıklama döndürür."""
    name = DISPLAY_NAMES.get(info.id, info.id)
    languages = info.languages or ()
    if len(languages) == 1:
        language_name = TURKISH_LANGUAGE_NAMES.get(languages[0], languages[0])
        return f"{name} yalnız {language_name} destekler."
    return f"{name} {language} dilini desteklemiyor. Desteklenen diller: {', '.join(languages)}."


def _reporter(total: int, on_progress: ProgressFn | None) -> Callable[[float], None]:
    """Bayt sayacını 0..1 ilerlemeye çevirir; geri gitmez, çağrıları seyreltir."""
    if on_progress is None or total <= 0:
        return lambda downloaded: None
    state = {"fraction": 0.0, "at": 0.0}

    def report(downloaded: float) -> None:
        # İndirme barları birbirini geçebildiği için 1.0'ı yalnız ensure_model bildirir.
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
    """snapshot_download'un bayt barlarını tek ilerleme kanalına bağlar."""

    class _ByteProgress(tqdm):
        def __init__(self, *args, **kwargs):
            # Kapalı tqdm ne `unit` alanını atar ne de `n` sayacını ilerletir; kendi
            # sayacımızı tutuyoruz ve birimi kuruluş anında yakalıyoruz.
            self._counts_bytes = kwargs.get("unit") == "B"
            self._bytes = kwargs.pop("initial", 0) or 0
            kwargs["disable"] = True  # bar çıktısı stderr'e dökülmesin; ilerleme protokolde
            super().__init__(*args, **kwargs)

        def update(self, n: float | None = 1) -> None:
            super().update(n)
            if self._counts_bytes:  # transfer ve yeniden kurma barları bayt sayar
                self._bytes += n or 0
                report(self._bytes)

    return _ByteProgress
