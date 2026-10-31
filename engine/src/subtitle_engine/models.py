"""Motor veri tipleri ve on-device model kataloğu.

Katalog, arayüzün model listesini (yaklaşık boyut, göreli hız/doğruluk, desteklenen
diller) ve indirme durumunu besler. İndirme ilerlemesi yalnızca katalogdaki modeller
için yayınlanır; katalog dışı adlar ("turbo", "tiny" gibi) faster-whisper'ın kendi
indirmesine bırakılır.
"""
import time
from collections.abc import Callable, Sequence
from dataclasses import asdict, dataclass, field
from typing import Literal

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

Backend = Literal["whisper", "onnx"]


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


# Sıra arayüzdeki liste sırasıdır: küçükten büyüğe.
KATALOG: tuple[ModelInfo, ...] = (
    ModelInfo(id="base", repo="Systran/faster-whisper-base", size_mb=145, speed=5, accuracy=2),
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
        files=(
            "config.json",
            "v3_e2e_rnnt_vocab.txt",
            "v3_e2e_rnnt_encoder.int8.onnx",
            "v3_e2e_rnnt_decoder.int8.onnx",
            "v3_e2e_rnnt_joint.int8.onnx",
        ),
    ),
    ModelInfo(id="small", repo="Systran/faster-whisper-small", size_mb=465, speed=4, accuracy=3),
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
        files=("config.json", "vocab.txt", "encoder-model.int8.onnx", "decoder-model.int8.onnx"),
    ),
    ModelInfo(id="medium", repo="Systran/faster-whisper-medium", size_mb=1500, speed=3, accuracy=4),
    ModelInfo(id="large-v3-turbo", repo="mobiuslabsgmbh/faster-whisper-large-v3-turbo", size_mb=1600, speed=2, accuracy=4),
    ModelInfo(id="large-v3", repo="Systran/faster-whisper-large-v3", size_mb=3000, speed=1, accuracy=5),
)
KATALOG_BY_ID = {m.id: m for m in KATALOG}

DISPLAY_NAMES = {"gigaam-v3": "GigaAM", "parakeet-v3": "Parakeet", "canary-1b-v2": "Canary"}
TURKISH_LANGUAGE_NAMES = {
    "ru": "Rusça",
    "pt": "Portekizce",
    "es": "İspanyolca",
    "tr": "Türkçe",
    "en": "İngilizce",
}


def find(model_id: str) -> ModelInfo | None:
    return KATALOG_BY_ID.get(model_id)


def is_downloaded(model_id: str) -> bool:
    """Model yerel Hugging Face önbelleğinde mi? Yalnız diske bakar, ağa çıkmaz."""
    info = find(model_id)
    if info is None:
        return False
    if info.backend == "onnx":
        # onnx-asr dosyaları tek tek sorgular; VAD ortak ve küçük bir bağımlılık.
        return all(_cached(info.repo, name) for name in info.files) and _cached(VAD_REPO, VAD_FILE)
    return _cached(info.repo, "model.bin")


def _cached(repo: str, filename: str) -> bool:
    return isinstance(huggingface_hub.try_to_load_from_cache(repo, filename), str)


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
    if info.backend == "onnx":
        return _ensure_onnx(info, on_progress)

    report = _reporter(_total_bytes(info), on_progress)
    path = huggingface_hub.snapshot_download(
        info.repo,
        allow_patterns=list(ALLOW_PATTERNS),
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
    return _repo_bytes(info.repo, ALLOW_PATTERNS, info.size_mb * _MIB)


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
