"""User-facing text in Turkish (default) and English.

The language comes from `set_language` (the CLI `--lang` option) or else the `SUBTITLE_ENGINE_LANG`
environment variable (`tr` or `en`); anything else, or unset, means Turkish. Plain Python module so it
works inside the PyInstaller bundle without data files. Developer-facing text (stderr logs) is not here.
"""
import os

ENV_VAR = "SUBTITLE_ENGINE_LANG"
LANGUAGES = ("tr", "en")
DEFAULT = "tr"

_override: str | None = None


def normalize(value: str | None) -> str:
    value = (value or "").strip().lower()
    return value if value in LANGUAGES else DEFAULT


def set_language(value: str | None) -> None:
    """Pins the language (overrides the environment); `None` goes back to the environment."""
    global _override
    _override = None if value is None else normalize(value)


def current() -> str:
    return _override if _override is not None else normalize(os.environ.get(ENV_VAR))


MESSAGES: dict[str, dict[str, str]] = {
    "unknown_model": {
        "tr": "Bilinmeyen model: {model}. Geçerli kimlikleri 'models' komutu listeler.",
        "en": "Unknown model: {model}. The 'models' command lists the valid ids.",
    },
    "unknown_translation_model": {
        "tr": "Bilinmeyen çeviri modeli: {model}. Geçerli kimlikleri 'models' komutu listeler.",
        "en": "Unknown translation model: {model}. The 'models' command lists the valid ids.",
    },
    "unknown_translation_model_short": {
        "tr": "Bilinmeyen çeviri modeli: {model}.",
        "en": "Unknown translation model: {model}.",
    },
    "model_shared": {
        "tr": "{model} modeli {others} ile aynı dosyaları paylaşıyor; silinmedi.",
        "en": "The {model} model shares its files with {others}; not deleted.",
    },
    "model_not_downloaded_delete": {
        "tr": "{model} modeli indirilmemiş; silinecek dosya yok.",
        "en": "The {model} model is not downloaded; there is nothing to delete.",
    },
    "model_not_downloaded": {
        "tr": "{model} modeli indirilmemiş; önce indirilmesi gerekiyor.",
        "en": "The {model} model is not downloaded; it has to be downloaded first.",
    },
    "partial_delete": {
        "tr": "{name} modeli kısmen silindi ({freed} MB boşaldı); kalan: {left}",
        "en": "The {name} model was only partially deleted ({freed} MB freed); remaining: {left}",
    },
    "delete_incomplete": {
        "tr": "Model dosyaları tamamen silinemedi: {path}",
        "en": "The model files could not be fully deleted: {path}",
    },
    "only_language": {
        "tr": "{name} yalnız {language} destekler.",
        "en": "{name} supports only {language}.",
    },
    "unsupported_language": {
        "tr": "{name} {language} dilini desteklemiyor. Desteklenen diller: {supported}.",
        "en": "{name} does not support the language {language}. Supported languages: {supported}.",
    },
    "nllb_source_required": {
        "tr": "NLLB için kaynak dil (--from) zorunlu; ör. --from tr.",
        "en": "NLLB requires a source language (--from), e.g. --from tr.",
    },
    "cannot_translate_from": {
        "tr": "{name} {source} dilinden çeviri yapamıyor. Desteklenen diller: {supported}.",
        "en": "{name} cannot translate from {source}. Supported languages: {supported}.",
    },
    "nllb_language_undefined": {
        "tr": "NLLB {target} dili için tanımlı değil.",
        "en": "NLLB has no definition for the language {target}.",
    },
    "diarize_load_failed": {
        "tr": "Konuşmacı ayrıştırma modeli yüklenemedi; dosyalar bozuk olabilir.",
        "en": "The speaker diarization model could not be loaded; its files may be corrupt.",
    },
    "diarize_not_downloaded": {
        "tr": "Konuşmacı ayrıştırma modeli indirilmemiş; önce indirilmesi gerekiyor.",
        "en": "The speaker diarization model is not downloaded; it has to be downloaded first.",
    },
    "diarize_wav_format": {
        "tr": "Konuşmacı ayrıştırma 16 kHz mono 16-bit WAV ses bekler.",
        "en": "Speaker diarization expects 16 kHz mono 16-bit WAV audio.",
    },
    "speaker_list_length": {
        "tr": "Konuşmacı listesi kelime sayısıyla aynı uzunlukta olmalı.",
        "en": "The speaker list must have the same length as the word list.",
    },
    "speakers_min": {
        "tr": "Konuşmacı sayısı en az 1 olmalı.",
        "en": "The number of speakers must be at least 1.",
    },
    "offline_missing_model": {
        "tr": "{model} indirilmemiş. Çevrimdışı mod açıkken model indirilemez; Ayarlar'dan çevrimdışı modu kapatıp tekrar deneyin.",
        "en": "{model} is not downloaded. Models cannot be downloaded in offline mode; turn off offline mode in Settings and try again.",
    },
    "offline_missing_llama": {
        "tr": "LLM çalışma zamanı (llama.cpp) indirilmemiş. Çevrimdışı mod açıkken indirilemez; Ayarlar'dan çevrimdışı modu kapatıp tekrar deneyin.",
        "en": "The LLM runtime (llama.cpp) is not downloaded. It cannot be downloaded in offline mode; turn off offline mode in Settings and try again.",
    },
    "translategemma_source_required": {
        "tr": "TranslateGemma için kaynak dil gerekli. Konuşma dilini seçip tekrar deneyin.",
        "en": "TranslateGemma requires a source language. Select the spoken language and try again.",
    },
    "translation_invalid": {
        "tr": "{id} numaralı altyazı için geçerli çeviri alınamadı. Çeviri tamamlanmadı; yeniden deneyin veya farklı bir çeviri modeli seçin.",
        "en": "Could not obtain a valid translation for subtitle {id}. Translation is incomplete; retry or select a different translation model.",
    },
    "llama_unavailable": {
        "tr": "Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: {error}",
        "en": "Could not start the local LLM runtime (llama.cpp): {error}",
    },
    "external_llm_unreachable": {
        "tr": "Harici LLM sunucusuna ulaşılamadı ({url}): {error}",
        "en": "Could not reach the external LLM server ({url}): {error}",
    },
    "external_llm_url_empty": {
        "tr": "Harici LLM sunucusu adresi boş olamaz.",
        "en": "The external LLM server address cannot be empty.",
    },
    "not_transcription_model": {
        "tr": "{model} bir konuşma tanıma modeli değil; 'models' komutu geçerli modelleri listeler.",
        "en": "{model} is not a speech recognition model; the 'models' command lists the valid models.",
    },
    "speakers_needs_diarize": {
        "tr": "--speakers yalnız --diarize ile kullanılabilir.",
        "en": "--speakers can only be used with --diarize.",
    },
    "video_not_found": {
        "tr": "Video bulunamadı: {video}",
        "en": "Video not found: {video}",
    },
    "no_audio_track": {
        "tr": "Videoda ses izi yok.",
        "en": "The video has no audio track.",
    },
    "audio_read_failed": {
        "tr": "Ses okunamadı: {error}",
        "en": "Could not read the audio: {error}",
    },
    "unknown_format": {
        "tr": "Bilinmeyen format: {format}",
        "en": "Unknown format: {format}",
    },
    "summary": {
        "tr": "dil: {language} | {count} altyazı satırı",
        "en": "language: {language} | {count} subtitle lines",
    },
    "error_prefix": {"tr": "hata", "en": "error"},
    "deleted": {
        "tr": "{id} silindi, {freed} MB boşaldı",
        "en": "{id} deleted, {freed} MB freed",
    },
    "col_model": {"tr": "model", "en": "model"},
    "col_kind": {"tr": "tür", "en": "kind"},
    "col_size": {"tr": "boyut", "en": "size"},
    "col_disk": {"tr": "disk", "en": "disk"},
    "col_speed": {"tr": "hız", "en": "speed"},
    "col_accuracy": {"tr": "doğruluk", "en": "accuracy"},
    "col_license": {"tr": "lisans", "en": "license"},
    "col_cache": {"tr": "önbellek", "en": "cache"},
    "cached_yes": {"tr": "var", "en": "yes"},
    "cached_no": {"tr": "yok", "en": "no"},
    "name_speaker_diarization": {"tr": "Konuşmacı ayırma", "en": "Speaker separation"},
    "lang_ru": {"tr": "Rusça", "en": "Russian"},
    "lang_pt": {"tr": "Portekizce", "en": "Portuguese"},
    "lang_es": {"tr": "İspanyolca", "en": "Spanish"},
    "lang_tr": {"tr": "Türkçe", "en": "Turkish"},
    "lang_en": {"tr": "İngilizce", "en": "English"},
    "lang_de": {"tr": "Almanca", "en": "German"},
    "lang_fr": {"tr": "Fransızca", "en": "French"},
    "lang_ja": {"tr": "Japonca", "en": "Japanese"},
    "lang_zh": {"tr": "Çince", "en": "Chinese"},
}


def t(key: str, **params: object) -> str:
    """Message `key` in the current language with `params` filled in."""
    return MESSAGES[key][current()].format(**params)
