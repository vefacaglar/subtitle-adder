//! User-facing text in Turkish (default) and English (port of `i18n.py`).
//!
//! The language comes from [`set_language`] (the CLI `--lang` option) or else the `SUBTITLE_ENGINE_LANG`
//! environment variable (`tr` or `en`, after Python `strip().lower()`); anything else, or unset, means Turkish.
//! The environment is read on every [`current`] call (tests change it).
//!
//! Messages use `{name}` placeholders (Python `str.format` with named fields only, no format specs).
//! Values are substituted with `Display`; pass Python-formatted text (see [`crate::pyfmt`],
//! [`crate::paths::py_path_str`]) where Python would print a float or a path.

use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

/// Environment variable selecting the message language.
pub const ENV_VAR: &str = "SUBTITLE_ENGINE_LANG";
/// Supported message languages.
pub const LANGUAGES: [&str; 2] = ["tr", "en"];
/// Default language.
pub const DEFAULT: Lang = Lang::Tr;

/// A message language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Tr,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Tr => "tr",
            Lang::En => "en",
        }
    }
}

// 0 = no override (environment decides), 1 = tr, 2 = en.
static OVERRIDE: AtomicU8 = AtomicU8::new(0);

/// Python `normalize(value)`: `value.strip().lower()` if it is `tr`/`en`, else Turkish (`None` → Turkish).
pub fn normalize(value: Option<&str>) -> Lang {
    let v = crate::pyfmt::strip(value.unwrap_or("")).to_lowercase();
    if v == "en" { Lang::En } else { Lang::Tr }
}

/// Pins the language (overrides the environment); `None` goes back to the environment.
pub fn set_language(value: Option<&str>) {
    let code = match value {
        None => 0,
        Some(v) => match normalize(Some(v)) {
            Lang::Tr => 1,
            Lang::En => 2,
        },
    };
    OVERRIDE.store(code, Ordering::SeqCst);
}

/// The current message language: the pinned one, else `SUBTITLE_ENGINE_LANG`.
pub fn current() -> Lang {
    match OVERRIDE.load(Ordering::SeqCst) {
        1 => Lang::Tr,
        2 => Lang::En,
        _ => normalize(std::env::var(ENV_VAR).ok().as_deref()),
    }
}

/// `(key, tr, en)` — verbatim copy of `MESSAGES` in `i18n.py`, same order.
pub static MESSAGES: &[(&str, &str, &str)] = &[
    ("unknown_model", "Bilinmeyen model: {model}. Geçerli kimlikleri 'models' komutu listeler.", "Unknown model: {model}. The 'models' command lists the valid ids."),
    ("unknown_translation_model", "Bilinmeyen çeviri modeli: {model}. Geçerli kimlikleri 'models' komutu listeler.", "Unknown translation model: {model}. The 'models' command lists the valid ids."),
    ("unknown_translation_model_short", "Bilinmeyen çeviri modeli: {model}.", "Unknown translation model: {model}."),
    ("model_shared", "{model} modeli {others} ile aynı dosyaları paylaşıyor; silinmedi.", "The {model} model shares its files with {others}; not deleted."),
    ("model_not_downloaded_delete", "{model} modeli indirilmemiş; silinecek dosya yok.", "The {model} model is not downloaded; there is nothing to delete."),
    ("model_not_downloaded", "{model} modeli indirilmemiş; önce indirilmesi gerekiyor.", "The {model} model is not downloaded; it has to be downloaded first."),
    ("partial_delete", "{name} modeli kısmen silindi ({freed} MB boşaldı); kalan: {left}", "The {name} model was only partially deleted ({freed} MB freed); remaining: {left}"),
    ("delete_incomplete", "Model dosyaları tamamen silinemedi: {path}", "The model files could not be fully deleted: {path}"),
    ("only_language", "{name} yalnız {language} destekler.", "{name} supports only {language}."),
    ("preview_no_streams", "Dosyada oynatılabilecek görüntü veya ses bulunamadı.", "The file contains no playable video or audio."),
    ("unsupported_language", "{name} {language} dilini desteklemiyor. Desteklenen diller: {supported}.", "{name} does not support the language {language}. Supported languages: {supported}."),
    ("nllb_source_required", "NLLB için kaynak dil (--from) zorunlu; ör. --from tr.", "NLLB requires a source language (--from), e.g. --from tr."),
    ("cannot_translate_from", "{name} {source} dilinden çeviri yapamıyor. Desteklenen diller: {supported}.", "{name} cannot translate from {source}. Supported languages: {supported}."),
    ("nllb_language_undefined", "NLLB {target} dili için tanımlı değil.", "NLLB has no definition for the language {target}."),
    ("diarize_load_failed", "Konuşmacı ayrıştırma modeli yüklenemedi; dosyalar bozuk olabilir.", "The speaker diarization model could not be loaded; its files may be corrupt."),
    ("diarize_not_downloaded", "Konuşmacı ayrıştırma modeli indirilmemiş; önce indirilmesi gerekiyor.", "The speaker diarization model is not downloaded; it has to be downloaded first."),
    ("diarize_wav_format", "Konuşmacı ayrıştırma 16 kHz mono 16-bit WAV ses bekler.", "Speaker diarization expects 16 kHz mono 16-bit WAV audio."),
    ("speaker_list_length", "Konuşmacı listesi kelime sayısıyla aynı uzunlukta olmalı.", "The speaker list must have the same length as the word list."),
    ("speakers_min", "Konuşmacı sayısı en az 1 olmalı.", "The number of speakers must be at least 1."),
    ("offline_missing_model", "{model} indirilmemiş. Çevrimdışı mod açıkken model indirilemez; Ayarlar'dan çevrimdışı modu kapatıp tekrar deneyin.", "{model} is not downloaded. Models cannot be downloaded in offline mode; turn off offline mode in Settings and try again."),
    ("offline_missing_llama", "LLM çalışma zamanı (llama.cpp) indirilmemiş. Çevrimdışı mod açıkken indirilemez; Ayarlar'dan çevrimdışı modu kapatıp tekrar deneyin.", "The LLM runtime (llama.cpp) is not downloaded. It cannot be downloaded in offline mode; turn off offline mode in Settings and try again."),
    ("translategemma_source_required", "TranslateGemma için kaynak dil gerekli. Konuşma dilini seçip tekrar deneyin.", "TranslateGemma requires a source language. Select the spoken language and try again."),
    ("translation_invalid", "{id} numaralı altyazı için geçerli çeviri alınamadı. Çeviri tamamlanmadı; yeniden deneyin veya farklı bir çeviri modeli seçin.", "Could not obtain a valid translation for subtitle {id}. Translation is incomplete; retry or select a different translation model."),
    ("llama_unavailable", "Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: {error}", "Could not start the local LLM runtime (llama.cpp): {error}"),
    ("external_llm_unreachable", "Harici LLM sunucusuna ulaşılamadı ({url}): {error}", "Could not reach the external LLM server ({url}): {error}"),
    ("external_llm_url_empty", "Harici LLM sunucusu adresi boş olamaz.", "The external LLM server address cannot be empty."),
    ("external_llm_key_insecure", "API anahtarı şifrelenmemiş bağlantıyla (http) gönderilmez; adresi https:// ile girin.", "The API key is not sent over an unencrypted (http) connection; enter the address with https://."),
    ("offline_external_llm", "Çevrimdışı mod açık: çeviri internetteki bir sağlayıcıya ({host}) gönderilemez. Çevrimdışı modu kapatın ya da yerel bir model/sunucu seçin.", "Offline mode is on: translation can't be sent to an internet provider ({host}). Turn offline mode off or choose a local model/server."),
    ("not_transcription_model", "{model} bir konuşma tanıma modeli değil; 'models' komutu geçerli modelleri listeler.", "{model} is not a speech recognition model; the 'models' command lists the valid models."),
    ("speakers_needs_diarize", "--speakers yalnız --diarize ile kullanılabilir.", "--speakers can only be used with --diarize."),
    ("video_not_found", "Video bulunamadı: {video}", "Video not found: {video}"),
    ("no_audio_track", "Videoda ses izi yok.", "The video has no audio track."),
    ("audio_read_failed", "Ses okunamadı: {error}", "Could not read the audio: {error}"),
    ("unknown_format", "Bilinmeyen format: {format}", "Unknown format: {format}"),
    ("summary", "dil: {language} | {count} altyazı satırı", "language: {language} | {count} subtitle lines"),
    ("error_prefix", "hata", "error"),
    ("deleted", "{id} silindi, {freed} MB boşaldı", "{id} deleted, {freed} MB freed"),
    ("col_model", "model", "model"),
    ("col_kind", "tür", "kind"),
    ("col_size", "boyut", "size"),
    ("col_disk", "disk", "disk"),
    ("col_speed", "hız", "speed"),
    ("col_accuracy", "doğruluk", "accuracy"),
    ("col_license", "lisans", "license"),
    ("col_cache", "önbellek", "cache"),
    ("cached_yes", "var", "yes"),
    ("cached_no", "yok", "no"),
    ("name_speaker_diarization", "Konuşmacı ayırma", "Speaker separation"),
    ("lang_ru", "Rusça", "Russian"),
    ("lang_pt", "Portekizce", "Portuguese"),
    ("lang_es", "İspanyolca", "Spanish"),
    ("lang_tr", "Türkçe", "Turkish"),
    ("lang_en", "İngilizce", "English"),
    ("lang_de", "Almanca", "German"),
    ("lang_fr", "Fransızca", "French"),
    ("lang_ja", "Japonca", "Japanese"),
    ("lang_zh", "Çince", "Chinese"),
];

/// The raw template of `key` in `lang`, or `None` for an unknown key.
pub fn message(key: &str, lang: Lang) -> Option<&'static str> {
    MESSAGES.iter().find(|(k, _, _)| *k == key).map(|(_, tr, en)| match lang {
        Lang::Tr => *tr,
        Lang::En => *en,
    })
}

/// Is `key` in the message table?
pub fn has_key(key: &str) -> bool {
    MESSAGES.iter().any(|(k, _, _)| *k == key)
}

/// Replaces `{name}` placeholders in `template` (single pass; substituted text is not re-scanned).
/// An unknown placeholder is left as is (Python would raise `KeyError`; callers always pass all names).
pub fn format_template(template: &str, params: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match params.iter().find(|(n, _)| *n == name) {
                    Some((_, v)) => out.push_str(&v.to_string()),
                    None => {
                        debug_assert!(false, "missing message parameter {name:?}");
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Message `key` in the current language with `params` filled in (Python `t(key, **params)`).
///
/// Panics on an unknown key (a programming error, `KeyError` in Python). See also the [`t!`](crate::t) macro.
pub fn t(key: &str, params: &[(&str, &dyn Display)]) -> String {
    let template = message(key, current()).unwrap_or_else(|| panic!("unknown message key {key:?}"));
    format_template(template, params)
}

/// `t!("key", name = value, ...)` = [`t`]`("key", &[("name", &value), ...])`.
#[macro_export]
macro_rules! t {
    ($key:expr $(, $name:ident = $val:expr)* $(,)?) => {
        $crate::i18n::t($key, &[$((stringify!($name), &$val as &dyn ::std::fmt::Display)),*])
    };
}

/// The `{name}` placeholders of a template, in order of appearance.
pub fn placeholders(template: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                names.push(&after[..close]);
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    names
}

/// `models.language_name(code)`: the localized language name (`lang_<code>` key) or the code itself.
pub fn language_name(code: &str) -> String {
    let key = format!("lang_{code}");
    if has_key(&key) { t(&key, &[]) } else { code.to_string() }
}

/// `models.DISPLAY_NAMES` (Whisper ids are shown as themselves).
pub static DISPLAY_NAMES: &[(&str, &str)] = &[
    ("gigaam-v3", "GigaAM"),
    ("parakeet-v3", "Parakeet"),
    ("canary-1b-v2", "Canary"),
    ("madlad-3b", "MADLAD"),
    ("nllb-1.3b", "NLLB"),
    ("qwen3.5-4b", "Qwen 3.5 4B"),
    ("gemma-4-e4b", "Gemma 4 E4B"),
    ("qwen3.5-9b", "Qwen 3.5 9B"),
    ("gemma-4-12b", "Gemma 4 12B"),
];

/// `models.display_name(id)`: localized for `speaker-diarization`, else `DISPLAY_NAMES.get(id, id)`.
pub fn display_name(model_id: &str) -> String {
    if model_id == "speaker-diarization" {
        return t("name_speaker_diarization", &[]);
    }
    DISPLAY_NAMES
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| model_id.to_string())
}
