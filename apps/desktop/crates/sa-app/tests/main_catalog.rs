//! Catalog completeness for the main-window area (Windows `CatalogCompletenessTests.EveryUsedKeyHasTurkishAndEnglish`,
//! macOS `everyKeyInThe…SourcesIsInTheCatalogInBothLanguages`): every key scanned from `src/main_window/*.rs` and the
//! main-window Slint files of a tree has `tr` and `en` in that platform's chain. Rust keys are checked against both
//! chains, except the ones only one face produces (the controller branches on the platform).

use sa_core::l10n::scan::{KeyScope, scan_rust, scan_slint};
use sa_core::l10n::{CatalogChain, CatalogId, is_embedded};

/// Keys only the Windows face produces from Rust (`if mac { … } else { … }` branches and the Windows demo scene).
const WINDOWS_ONLY: &[&str] = &["K%lld", "Dosya Gezgini'nde aç", "Dosya konumunu aç", "Videolar", "Tamamlandı"];
/// Keys only the macOS face produces from Rust.
const MAC_ONLY: &[&str] = &[];

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

fn missing(chain: CatalogChain, slint_dirs: &[&str], skip: &[&str]) -> Vec<String> {
    let mut missing = Vec::new();
    let mut check = |file: &str, key: &str, line: usize, scope: KeyScope| {
        let ok = match scope {
            KeyScope::App => chain.has_both_languages(key),
            KeyScope::Engine => CatalogChain::Engine.has_both_languages(key),
        };
        if !ok && !skip.contains(&key) {
            missing.push(format!("{file}:{line} {key:?}"));
        }
    };
    for (file, src) in files("src/main_window", "rs") {
        // The demo fills look-check scenes with sample content; its keys are real catalog keys too.
        for k in scan_rust(&src) {
            check(&file, &k.key, k.line, k.scope);
        }
    }
    for dir in slint_dirs {
        for (file, src) in files(dir, "slint") {
            for k in scan_slint(&src) {
                check(&file, &k.key, k.line, k.scope);
            }
        }
    }
    missing
}

#[test]
fn scanner_sees_the_main_window_keys() {
    let rust: Vec<String> = files("src/main_window", "rs").into_iter().flat_map(|(_, s)| scan_rust(&s)).map(|k| k.key).collect();
    for key in ["Henüz altyazı yok", "Video oynatılamadı: %@"] {
        assert!(rust.iter().any(|k| k == key), "{key}");
    }
    let win: Vec<String> = files("ui/win/main", "slint").into_iter().flat_map(|(_, s)| scan_slint(&s)).map(|k| k.key).collect();
    for key in ["Video seç", "Komple çevir", "Videoyu buraya sürükle", "Tümünü Çevir ve Kaydet"] {
        assert!(win.iter().any(|k| k == key), "{key}");
    }
    let mac: Vec<String> = files("ui/mac/main", "slint").into_iter().flat_map(|(_, s)| scan_slint(&s)).map(|k| k.key).collect();
    for key in ["Video seç", "Gösterilen", "Çeviri listesi"] {
        assert!(mac.iter().any(|k| k == key), "{key}");
    }
}

#[test]
fn every_windows_main_window_key_has_turkish_and_english() {
    if !is_embedded(CatalogId::Windows) {
        return;
    }
    let m = missing(CatalogChain::Windows, &["ui/win/main"], MAC_ONLY);
    assert!(m.is_empty(), "missing in the Windows chain:\n{}", m.join("\n"));
}

#[test]
fn every_mac_main_window_key_has_turkish_and_english() {
    if !is_embedded(CatalogId::DesktopMac) {
        return;
    }
    let m = missing(CatalogChain::Mac, &["ui/mac/main"], WINDOWS_ONLY);
    assert!(m.is_empty(), "missing in the macOS chain:\n{}", m.join("\n"));
}
