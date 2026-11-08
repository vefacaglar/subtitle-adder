//! Catalog completeness for the Settings area (CatalogCompletenessTests.EveryUsedKeyHasTurkishAndEnglish /
//! `everyKeyInThePlatformSourcesIsInTheCatalogInBothLanguages`): every key scanned from `src/settings_window/*.rs`
//! and the settings Slint files of a tree has `tr` and `en` in that platform's chain. Rust keys are checked against
//! both chains, except the ones only one face produces.

use sa_core::l10n::scan::{KeyScope, scan_rust, scan_slint};
use sa_core::l10n::{CatalogChain, CatalogId, is_embedded};

/// Keys only the Windows face produces from Rust (LAN section, video cache, file associations, the C# test
/// messages of the inline form). The macOS chain need not have them.
const WINDOWS_ONLY: &[&str] = &[
    "Dosya türü ilişkilendirmeleri kaldırıldı.",
    "Dosya türü ilişkilendirmeleri kaldırılamadı.",
    "Video ve altyazı dosya türleri Windows'a kaydedildi. “Varsayılan uygulamalar…”dan Subtitle Adder'ı varsayılan oynatıcı yapabilirsin.",
    "Dosya türleri Windows'a kaydedilemedi.",
    "Şifreli önbellek kullanımı: %@ / %lld GB",
    "Önbellek kullanımı hesaplanıyor…",
    "Hız %lld/5 · Doğruluk %lld/5",
    "Sağlayıcı güncellendi ve etkinleştirildi.",
    "%@ kaydedildi ve etkinleştirildi.",
    "%@ boşaltıldı.",
];
/// Keys only the macOS face produces from Rust (key cache captions, the editor sheet, offline refusal).
const MAC_ONLY: &[&str] = &[
    "Çeviri",
    "Anahtar okunuyor…",
    "Anahtar okunamadı",
    "Kayıtlı anahtar korunur: %@",
    "Anahtar okunamadı; boş bırakırsan korunur",
    "Kayıtlı anahtar kaydedilince silinecek.",
    "Alan boş bırakılırsa kayıtlı anahtar korunur.",
    "API anahtarı okunamadı. Alan boş bırakılırsa anahtar silinmez.",
    "Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz.",
    "“%@” sağlayıcısı silinsin mi?",
    "Kayıtlı API anahtarı da silinir.",
    "%@ için API anahtarı okunamadı. Anahtarı yeniden girip tekrar dene.",
    "Açıkken internete hiç bağlanılmaz ve eksik modeller indirilmez; bulut çeviri sağlayıcısı (%@) da kullanılmaz. Kapalıyken çevrilecek metin bu sağlayıcıya gönderilir. Ses hiçbir durumda dışarı gönderilmez.",
    "%@ bir bulut hizmeti: çevrilecek metin ona gönderilir. Ses ve video gönderilmez.",
];

fn files(dir: &str, ext: &str) -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root).unwrap_or_else(|e| panic!("{}: {e}", root.display())) {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == ext) {
            out.push((path.display().to_string(), std::fs::read_to_string(&path).unwrap()));
        }
    }
    assert!(!out.is_empty(), "no sources in {dir}");
    out
}

fn missing(chain: CatalogChain, slint_dir: &str, skip: &[&str]) -> Vec<String> {
    let mut missing = Vec::new();
    let mut check = |file: &str, key: &str, line: usize, scope: KeyScope, rust: bool| {
        let ok = match scope {
            KeyScope::App => chain.has_both_languages(key),
            KeyScope::Engine => CatalogChain::Engine.has_both_languages(key),
        };
        if !ok && !(rust && skip.contains(&key)) {
            missing.push(format!("{file}:{line} {key:?}"));
        }
    };
    for (file, src) in files("src/settings_window", "rs") {
        // The fakes and the screenshot scenes are not shipped UI.
        if file.ends_with("testing.rs") || file.ends_with("shot.rs") {
            continue;
        }
        for k in scan_rust(&src) {
            check(&file, &k.key, k.line, k.scope, true);
        }
    }
    for (file, src) in files(slint_dir, "slint") {
        for k in scan_slint(&src) {
            check(&file, &k.key, k.line, k.scope, false);
        }
    }
    missing
}

#[test]
fn scanner_sees_the_settings_keys() {
    let rust: Vec<String> =
        files("src/settings_window", "rs").into_iter().flat_map(|(_, s)| scan_rust(&s)).map(|k| k.key).collect();
    for key in ["Lütfen geçerli bir sunucu adresi girin.", "Anahtar yok", "API Sağlayıcısını Düzenle"] {
        assert!(rust.iter().any(|k| k == key), "{key}");
    }
    let win: Vec<String> =
        files("ui/win/settings", "slint").into_iter().flat_map(|(_, s)| scan_slint(&s)).map(|k| k.key).collect();
    for key in ["Ayarlar", "Arayüz dili", "Harici LLM sunucusu (Yerel ağ)", "Önbelleği temizle", "Varsayılana döndür"] {
        assert!(win.iter().any(|k| k == key), "{key}");
    }
    let mac: Vec<String> =
        files("ui/mac/settings", "slint").into_iter().flat_map(|(_, s)| scan_slint(&s)).map(|k| k.key).collect();
    for key in ["Çeviri", "API sağlayıcısı ekle…", "Modelleri seç", "Kayıtlı anahtarı sil"] {
        assert!(mac.iter().any(|k| k == key), "{key}");
    }
}

#[test]
fn every_windows_settings_key_has_turkish_and_english() {
    if !is_embedded(CatalogId::Windows) {
        return;
    }
    let m = missing(CatalogChain::Windows, "ui/win/settings", MAC_ONLY);
    assert!(m.is_empty(), "missing in the Windows chain:\n{}", m.join("\n"));
}

#[test]
fn every_mac_settings_key_has_turkish_and_english() {
    if !is_embedded(CatalogId::DesktopMac) {
        return;
    }
    let m = missing(CatalogChain::Mac, "ui/mac/settings", WINDOWS_ONLY);
    assert!(m.is_empty(), "missing in the macOS chain:\n{}", m.join("\n"));
}
