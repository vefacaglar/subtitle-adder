//! Catalog completeness for the strings `sa-core` itself shows (CatalogCompletenessTests, per platform chain):
//! every key scanned from this crate's sources plus the keys it looks up indirectly (match arms, tables) has `tr`
//! and `en` in one catalog of the chain.

use sa_core::l10n::scan::{KeyScope, scan_rust};
use sa_core::l10n::{CatalogChain, CatalogId, is_embedded};

/// Keys passed to `l()` through a variable (titles of enums, model names, language names).
const INDIRECT_KEYS: &[&str] = &[
    "Sistem", "Açık", "Koyu", "Kutu", "Gölge", "Kontur", "Orijinal", "Çeviri", "İkisi", "Hızlı", "Dengeli", "Doğru",
    "Çok doğru (turbo)", "En doğru", "Parakeet v3 (hızlı, çok dilli)", "Canary (çok dilli)", "GigaAM (Rusça)", "NLLB-200 1.3B",
    "Qwen 3.5 4B", "MADLAD-400 3B", "Gemma 4 E4B", "Qwen 3.5 9B", "Gemma 4 12B", "Konuşmacı ayırma", "Otomatik",
    "Otomatik algılama", "Türkçe (tr)", "Türkçe", "Portekizce – Brezilya (pt)", "Portekizce", "Portekizce – Brezilya",
    "İspanyolca (es)", "İspanyolca", "Rusça (ru)", "Rusça", "İngilizce (en)", "İngilizce", "Almanca (de)", "Almanca",
    "Fransızca (fr)", "Fransızca", "Japonca (ja)", "Japonca", "Çince (zh)", "Çince", "Geri Al", "Yinele",
    // Undo action names (catalog keys localized when shown).
    "Konuşmacıyı Değiştir", "Metni Değiştir", "Satırları Birleştir", "Satırı Böl", "Satırı Sil", "Satırları Sil",
    "Zamanlamayı Değiştir", "Çeviriyi Değiştir", "Seçilenleri Yeniden Çevir",
];

/// Keys only the macOS app shows (its Edit menu titles live in the macOS overlay catalog).
const MAC_ONLY_KEYS: &[&str] = &["Geri Al: %@", "Yinele: %@"];

/// Keys only the Windows app shows (undo/redo button tooltips; macOS uses the Edit menu titles).
const WINDOWS_ONLY_KEYS: &[&str] = &["%@ İşlemini Geri Al", "%@ İşlemini Yinele"];

fn sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push((path.display().to_string(), std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    out
}

fn missing(chain: CatalogChain, skip: &[&str]) -> Vec<String> {
    let mut missing = Vec::new();
    for (file, source) in sources() {
        if file.contains("l10n") {
            continue; // the localization layer's own doc examples
        }
        for key in scan_rust(&source) {
            let ok = match key.scope {
                KeyScope::App => chain.has_both_languages(&key.key),
                KeyScope::Engine => CatalogChain::Engine.has_both_languages(&key.key),
            };
            if !ok && !skip.contains(&key.key.as_str()) {
                missing.push(format!("{file}:{} {:?}", key.line, key.key));
            }
        }
    }
    for key in INDIRECT_KEYS {
        if !chain.has_both_languages(key) {
            missing.push(format!("indirect {key:?}"));
        }
    }
    missing
}

#[test]
fn scanner_finds_the_core_keys() {
    let all: Vec<String> = sources().into_iter().flat_map(|(_, s)| scan_rust(&s)).map(|k| k.key).collect();
    for key in ["%@ İşlemini Geri Al", "%lld%%", "Toplam %@ disk kullanılıyor.", "Hız %lld/5 · Doğruluk %lld/5"] {
        assert!(all.iter().any(|k| k == key), "{key}");
    }
}

#[test]
fn every_core_key_has_turkish_and_english_on_windows() {
    if !is_embedded(CatalogId::Windows) {
        return; // a macOS build without `all-catalogs`
    }
    let missing = missing(CatalogChain::Windows, MAC_ONLY_KEYS);
    assert!(missing.is_empty(), "{missing:#?}");
}

#[test]
fn every_core_key_has_turkish_and_english_on_mac() {
    if !is_embedded(CatalogId::DesktopMac) {
        return; // a Windows build without `all-catalogs`
    }
    let missing = missing(CatalogChain::Mac, WINDOWS_ONLY_KEYS);
    assert!(missing.is_empty(), "{missing:#?}");
}
