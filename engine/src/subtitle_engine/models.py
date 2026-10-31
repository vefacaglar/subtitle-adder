"""Motor veri tipleri ve on-device model kataloğu.

Katalog, arayüzün model listesini (yaklaşık boyut, göreli hız/doğruluk) ve indirme
durumunu besler. İndirme ilerlemesi yalnızca katalogdaki modeller için yayınlanır;
katalog dışı adlar ("turbo", "tiny" gibi) faster-whisper'ın kendi indirmesine bırakılır.
"""
import time
from collections.abc import Callable
from dataclasses import asdict, dataclass, field

import huggingface_hub
from huggingface_hub.utils import filter_repo_objects
from tqdm import tqdm


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

# İlerleme çağrıları en fazla ~10 Hz: UI olay akışı boğulmasın.
PROGRESS_INTERVAL = 0.1

_MIB = 1024 * 1024


@dataclass(frozen=True)
class ModelInfo:
    """Katalogdaki bir Whisper modeli. Görünen ad/açıklama gibi metinler uygulamada tutulur."""

    id: str
    repo: str  # faster_whisper.utils._MODELS ile aynı (test bunu doğrular)
    size_mb: int  # yaklaşık indirme boyutu (sabit tahmin)
    speed: int  # 1 (en yavaş) - 5 (en hızlı), göreli
    accuracy: int  # 1 (en düşük) - 5 (en yüksek), göreli


# Sıra arayüzdeki liste sırasıdır: küçükten büyüğe.
KATALOG: tuple[ModelInfo, ...] = (
    ModelInfo(id="base", repo="Systran/faster-whisper-base", size_mb=145, speed=5, accuracy=2),
    ModelInfo(id="small", repo="Systran/faster-whisper-small", size_mb=465, speed=4, accuracy=3),
    ModelInfo(id="medium", repo="Systran/faster-whisper-medium", size_mb=1500, speed=3, accuracy=4),
    ModelInfo(id="large-v3-turbo", repo="mobiuslabsgmbh/faster-whisper-large-v3-turbo", size_mb=1600, speed=2, accuracy=4),
    ModelInfo(id="large-v3", repo="Systran/faster-whisper-large-v3", size_mb=3000, speed=1, accuracy=5),
)
KATALOG_BY_ID = {m.id: m for m in KATALOG}


def find(model_id: str) -> ModelInfo | None:
    return KATALOG_BY_ID.get(model_id)


def is_downloaded(model_id: str) -> bool:
    """Model yerel Hugging Face önbelleğinde mi? Yalnız diske bakar, ağa çıkmaz."""
    info = find(model_id)
    if info is None:
        return False
    cached = huggingface_hub.try_to_load_from_cache(info.repo, "model.bin")
    return isinstance(cached, str)


def needs_download(model_id: str) -> bool:
    """İndirme aşaması yayınlanmalı mı: katalogda olup önbellekte olmayan model."""
    return find(model_id) is not None and not is_downloaded(model_id)


def ensure_model(model_id: str, on_progress: ProgressFn | None = None) -> str | None:
    """Model yoksa indirir; toplam bayt üzerinden 0..1 ilerlemeyi `on_progress`e verir.

    Önbellekteyse hemen döner. Katalog dışı adlarda hiçbir şey yapmaz (None döner):
    onları faster-whisper modeli yüklerken kendi indirir, ilerleme yayınlanmaz.
    """
    info = find(model_id)
    if info is None or is_downloaded(model_id):
        return None

    report = _reporter(_total_bytes(info), on_progress)
    path = huggingface_hub.snapshot_download(
        info.repo,
        allow_patterns=list(ALLOW_PATTERNS),
        tqdm_class=_progress_tqdm(report),
    )
    if on_progress is not None:
        on_progress(1.0)  # dosyalar yerleşti; son bildirim 0.99'da kalmasın
    return path


def _total_bytes(info: ModelInfo) -> int:
    """Depodaki indirilecek dosyaların toplam baytı; sorgulanamazsa katalog tahmini."""
    estimate = info.size_mb * _MIB
    try:
        siblings = huggingface_hub.HfApi().model_info(info.repo, files_metadata=True).siblings
    except Exception:  # ağ/HTTP hatası: indirme zaten kendi hatasını üretir
        return estimate
    sizes = {f.rfilename: f.size or 0 for f in siblings}
    allowed = filter_repo_objects(list(sizes), allow_patterns=list(ALLOW_PATTERNS))
    return sum(sizes.get(name, 0) for name in allowed) or estimate


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
