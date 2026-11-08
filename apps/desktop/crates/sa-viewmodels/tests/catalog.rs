//! Catalog completeness for the view-model core (CatalogCompletenessTests): every key scanned from `src/**/*.rs` has
//! `tr` and `en` in a single catalog of each platform chain, except the keys only one platform produces.

use sa_core::l10n::CatalogChain;
use sa_core::l10n::scan::{KeyScope, scan_rust};

/// Keys only the Windows build produces (guarded by `PlatformPolicy` knobs, or results only the Windows provider
/// form shows: the macOS sheet closes without a status line).
const WINDOWS_ONLY: &[&str] = &[
    "%lld video · SSD indeksinde %lld dosya",
    "Altyazılı",
    "%@ kaydedildi ve etkinleştirildi.",
    "Sağlayıcı bulunamadı.",
    "Sağlayıcı güncellendi ve etkinleştirildi.",
    "Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (Ctrl+Enter)",
    "Profil açık değilken izleme geçmişi tutulmaz. Klasör gezgininden bir video açabilir veya geçmişinizi görmek için giriş yapabilirsiniz.",
];
/// Keys only the macOS build produces.
const MAC_ONLY: &[&str] = &[];

fn sources(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push((path.display().to_string(), std::fs::read_to_string(&path).unwrap()));
        }
    }
}

fn missing(chain: CatalogChain, skip: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(!files.is_empty());
    let mut missing = Vec::new();
    for (file, src) in files {
        for k in scan_rust(&src) {
            let ok = match k.scope {
                KeyScope::App => chain.has_both_languages(&k.key),
                KeyScope::Engine => CatalogChain::Engine.has_both_languages(&k.key),
            };
            if !ok && !skip.contains(&k.key.as_str()) {
                missing.push(format!("{file}:{} {:?}", k.line, k.key));
            }
        }
    }
    missing
}

#[test]
fn every_viewmodel_key_is_in_the_windows_chain() {
    let m = missing(CatalogChain::Windows, MAC_ONLY);
    assert!(m.is_empty(), "missing in the Windows chain:\n{}", m.join("\n"));
}

#[test]
fn every_viewmodel_key_is_in_the_mac_chain() {
    let m = missing(CatalogChain::Mac, WINDOWS_ONLY);
    assert!(m.is_empty(), "missing in the macOS chain:\n{}", m.join("\n"));
}
