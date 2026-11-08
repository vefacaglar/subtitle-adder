//! Localization tests: ports of LanguageEnvTests (localization part), LocalizationAndAppearanceTests (catalog part),
//! the win-core §12 printf/LanguageState/AppLanguage/LocalizedMessage tests and the recommended new tests.

use std::sync::{Arc, Mutex};

use sa_core::engine_types::EngineError;
use sa_core::l10n::{
    AppLanguage, AppLanguageChange, Arg, CatalogChain, CatalogId, LanguageChoice, LanguageState, LocalizedMessage,
    StringCatalog, UiLanguage, catalog, engine_string, is_embedded, localize_in, printf,
};
use sa_core::settings::{InMemorySettingsStore, SettingsStore};

/// Tests that read or write the process-wide `LanguageState::shared()` run one at a time.
static SHARED_LANGUAGE: Mutex<()> = Mutex::new(());

fn s(v: &str) -> Arg {
    Arg::from(v)
}

// ---------------------------------------------------------------- UiLanguage

#[test]
fn system_language_falls_back_to_english_unless_turkish() {
    assert_eq!(UiLanguage::system(&["tr-TR", "en-US"]), UiLanguage::Tr);
    assert_eq!(UiLanguage::system(&["tr"]), UiLanguage::Tr);
    assert_eq!(UiLanguage::system(&["de-DE", "tr-TR"]), UiLanguage::Tr);
    assert_eq!(UiLanguage::system(&["de-DE", "en-US", "tr-TR"]), UiLanguage::En);
    assert_eq!(UiLanguage::system(&["pt-BR"]), UiLanguage::En);
    assert_eq!(UiLanguage::system::<&str>(&[]), UiLanguage::En);
    assert_eq!(UiLanguage::system(&["tr_TR"]), UiLanguage::Tr);
    assert_eq!(UiLanguage::system(&["TR-tr"]), UiLanguage::Tr);
}

#[test]
fn parses_language_codes() {
    assert_eq!(UiLanguage::parse("EN"), Some(UiLanguage::En));
    assert_eq!(UiLanguage::parse("tr"), Some(UiLanguage::Tr));
    assert_eq!(UiLanguage::parse(" en "), Some(UiLanguage::En));
    assert_eq!(UiLanguage::parse("de"), None);
    assert_eq!(UiLanguage::Tr.code(), "tr");
    assert_eq!(UiLanguage::En.code(), "en");
}

// ---------------------------------------------------------------- engine strings

#[test]
fn engine_strings_are_translated() {
    assert_eq!(engine_string("Motor model listesi yazmadı.", Some(UiLanguage::En), &[]), "The engine did not output a model list.");
    assert_eq!(engine_string("Motor model listesi yazmadı.", Some(UiLanguage::Tr), &[]), "Motor model listesi yazmadı.");
    assert_eq!(
        engine_string("Motor hata koduyla çıktı (%lld). %@", Some(UiLanguage::En), &[Arg::Int(3), s("x")]),
        "The engine exited with an error code (3). x"
    );
    assert_eq!(
        engine_string("Motor hata koduyla çıktı (%lld). %@", Some(UiLanguage::Tr), &[Arg::Int(3), s("x")]),
        "Motor hata koduyla çıktı (3). x"
    );
    assert_eq!(
        sa_core::engine_string!("Motor hata koduyla çıktı (%lld). %@", Some(UiLanguage::En), 3, "x"),
        "The engine exited with an error code (3). x"
    );
}

#[test]
fn missing_key_falls_back_to_key() {
    assert_eq!(engine_string("Yok böyle bir şey %d", Some(UiLanguage::En), &[Arg::Int(5)]), "Yok böyle bir şey 5");
    assert_eq!(localize_in(CatalogChain::native(), UiLanguage::En, "Katalogda yok", &[]), "Katalogda yok");
}

#[test]
fn catalogs_are_searched_in_order() {
    let override_json = r#"{"sourceLanguage":"tr","strings":{"Motor model listesi yazmadı.":{"localizations":{"en":{"stringUnit":{"state":"translated","value":"Override"}}}}},"version":"1.0"}"#;
    let custom = StringCatalog::parse(override_json);
    let chain = [&custom, catalog(CatalogId::EngineKit)];
    let find = |lang: &str| chain.iter().find_map(|c| c.lookup("Motor model listesi yazmadı.", lang)).unwrap_or("Motor model listesi yazmadı.");
    assert_eq!(find("en"), "Override");
    assert_eq!(find("tr"), "Motor model listesi yazmadı.");
    assert!(catalog(CatalogId::EngineKit).len() >= 5);
}

#[test]
fn engine_not_found_error_is_localized_at_display() {
    assert_eq!(
        EngineError::not_found_text(Some(UiLanguage::En)),
        "Subtitle engine not found (is engine/dist built, or is SUBTITLE_ENGINE set?)."
    );
    assert_eq!(
        EngineError::not_found_text(Some(UiLanguage::Tr)),
        "Altyazı motoru bulunamadı (engine/dist derlendi mi, ya da SUBTITLE_ENGINE ayarlı mı?)."
    );
}

// ---------------------------------------------------------------- catalog parsing

#[test]
fn catalog_parse_ignores_wrong_types_and_keeps_order() {
    let json = r#"{"strings":{
        "b":{"localizations":{"en":{"stringUnit":{"value":"B"}}}},
        "a":{"extractionState":"manual","localizations":{"tr":{"stringUnit":{"value":"A"}},"en":{"stringUnit":{"value":7}}}},
        "c":{"localizations":{"en":"oops"}},
        "d":{"comment":"no units"}
    }}"#;
    let c = StringCatalog::parse(json);
    assert_eq!(c.keys().collect::<Vec<_>>(), vec!["b", "a"]);
    assert_eq!(c.lookup("a", "tr"), Some("A"));
    assert_eq!(c.lookup("a", "en"), None);
    assert!(!c.contains_key("c"));
    assert!(StringCatalog::parse("{ not json").is_empty());
    assert!(StringCatalog::parse("[]").is_empty());
    assert!(StringCatalog::parse(r#"{"version":"1.0"}"#).is_empty());
}

#[test]
fn embedded_catalogs_load() {
    assert!(catalog(CatalogId::App).len() >= 400);
    assert_eq!(catalog(CatalogId::EngineKit).len(), 9);
    if is_embedded(CatalogId::Windows) {
        assert!(catalog(CatalogId::Windows).len() >= 250);
        assert!(!catalog(CatalogId::DesktopWindows).is_empty());
    }
    if is_embedded(CatalogId::DesktopMac) {
        assert!(!catalog(CatalogId::DesktopMac).is_empty());
    }
}

#[test]
fn every_embedded_catalog_entry_has_both_languages() {
    for id in [CatalogId::App, CatalogId::EngineKit, CatalogId::Windows, CatalogId::DesktopWindows, CatalogId::DesktopMac] {
        let c = catalog(id);
        for key in c.keys() {
            assert!(c.has(key, "tr") && c.has(key, "en"), "{id:?} key {key:?} lacks tr or en");
        }
    }
}

#[test]
fn desktop_overlays_never_shadow_existing_strings() {
    for (overlay, chain) in [(CatalogId::DesktopWindows, CatalogChain::Windows), (CatalogId::DesktopMac, CatalogChain::Mac)] {
        if !is_embedded(overlay) {
            continue;
        }
        for key in catalog(overlay).keys() {
            for id in chain.ids().iter().filter(|id| **id != overlay) {
                assert!(!catalog(*id).contains_key(key), "{overlay:?} key {key:?} already exists in {id:?}");
            }
        }
    }
}

// ---------------------------------------------------------------- app catalog values

#[test]
fn app_catalog_has_english_and_turkish() {
    let chain = CatalogChain::native();
    assert_eq!(localize_in(chain, UiLanguage::En, "Altyazı üret", &[]), "Generate Subtitles");
    assert_eq!(localize_in(chain, UiLanguage::Tr, "Altyazı üret", &[]), "Altyazı üret");
    assert_eq!(localize_in(chain, UiLanguage::En, "%lld satır · dil: %@", &[Arg::Int(3), s("tr")]), "Lines: 3 · language: tr");
    assert_eq!(
        localize_in(chain, UiLanguage::En, "%@ için %@ seçildi.", &[s("Turkish"), s("Fast")]),
        "Selected Fast for Turkish."
    );
    assert_eq!(localize_in(chain, UiLanguage::En, "Çevriliyor… %lld%%", &[Arg::Int(42)]), "Translating… 42%");
    assert_eq!(localize_in(chain, UiLanguage::Tr, "Çevriliyor… %lld%%", &[Arg::Int(42)]), "Çevriliyor… %42");
    assert_eq!(localize_in(chain, UiLanguage::En, "Katalogda yok", &[]), "Katalogda yok");
}

#[test]
fn mac_chain_uses_the_app_catalog_values() {
    let chain = CatalogChain::Mac;
    assert_eq!(localize_in(chain, UiLanguage::Tr, "Ses ayrılıyor… %lld%%", &[Arg::Int(42)]), "Ses ayrılıyor… %42");
    assert_eq!(localize_in(chain, UiLanguage::En, "Dosya kaydedilemedi: %@", &[s("x")]), "Could not save the file: x");
    assert_eq!(localize_in(chain, UiLanguage::Tr, "%lld%%", &[Arg::Int(5)]), "%5");
    assert_eq!(localize_in(chain, UiLanguage::En, "%lld%%", &[Arg::Int(5)]), "5%");
}

#[cfg(any(windows, feature = "all-catalogs"))]
#[test]
fn windows_chain_prefers_the_windows_catalog() {
    let chain = CatalogChain::Windows;
    assert_eq!(localize_in(chain, UiLanguage::Tr, "Ses ayrılıyor… %lld%%", &[Arg::Int(42)]), "Ses ayrılıyor… 42%");
    // Not in the Windows catalog: falls through to the App catalog.
    assert_eq!(localize_in(chain, UiLanguage::Tr, "Çevriliyor… %lld%%", &[Arg::Int(42)]), "Çevriliyor… %42");
    assert_eq!(localize_in(chain, UiLanguage::En, "Dosya kaydedilemedi: %@", &[s("x")]), "Couldn't save the file: x");
    assert_eq!(localize_in(chain, UiLanguage::En, "%@ İşlemini Geri Al", &[s("Delete Line")]), "Undo Delete Line");
    // The App catalog is part of the Windows chain.
    assert_eq!(localize_in(chain, UiLanguage::En, "Satırları Sil", &[]), "Delete Lines");
    // UNIFIED.md §8: the missing Windows entry.
    assert_eq!(localize_in(chain, UiLanguage::En, "Altyazı ve çeviri", &[]), "Subtitles and translation");
    // EngineKit is the last fallback of the app chain.
    assert_eq!(localize_in(chain, UiLanguage::En, "Zaman aşımı", &[]), "Timed out");
}

#[cfg(any(not(windows), feature = "all-catalogs"))]
#[test]
fn mac_chain_has_the_new_desktop_strings() {
    let chain = CatalogChain::Mac;
    assert_eq!(
        localize_in(chain, UiLanguage::En, "API anahtarı bu Mac'te şifrelenmiş olarak saklanır.", &[]),
        "The API key is stored encrypted on this Mac."
    );
    assert_eq!(localize_in(chain, UiLanguage::En, "%@ Uygulamasından Çık", &[s("Subtitle Adder")]), "Quit Subtitle Adder");
    assert_eq!(localize_in(chain, UiLanguage::En, "Geri Al: %@", &[s("Delete Line")]), "Undo Delete Line");
}

// ---------------------------------------------------------------- printf

#[test]
fn printf_specifiers_are_converted() {
    let tr = UiLanguage::Tr;
    assert_eq!(printf("%%%lld", tr, &[Arg::Int(42)]), "%42");
    assert_eq!(printf("%lld%%", tr, &[Arg::Int(42)]), "42%");
    assert_eq!(printf("%2$@ %1$@ %3$ld", tr, &[s("a"), s("b"), Arg::Int(7)]), "b a 7");
    assert_eq!(printf("%@ %d", tr, &[s("x"), Arg::Int(3)]), "x 3");
    assert_eq!(printf("%.1f", tr, &[Arg::Float(1.5)]), "1,5");
    assert_eq!(printf("%.1f", UiLanguage::En, &[Arg::Float(1.5)]), "1.5");
    assert_eq!(printf("trailing %", tr, &[Arg::Int(1)]), "trailing %");
    // No args → unformatted (the C# test localizes against an empty catalog: the key comes back verbatim).
    assert_eq!(localize_in(CatalogChain::Engine, tr, "%lld%%", &[]), "%lld%%");
    assert_eq!(localize_in(CatalogChain::Mac, tr, "%lld%%", &[]), "%%%lld");
}

#[test]
fn printf_edge_cases() {
    let tr = UiLanguage::Tr;
    let en = UiLanguage::En;
    // Unknown conversion copies the specifier and consumes an argument.
    assert_eq!(printf("%x %@", en, &[Arg::Int(1), s("b")]), "%x b");
    // Positional 0 → missing argument → "".
    assert_eq!(printf("[%0$@]", en, &[s("a")]), "[]");
    // Missing arguments.
    assert_eq!(printf("%@-%@", en, &[s("a")]), "a-");
    // printf itself formats a missing argument as "" (the "no args → unformatted" rule belongs to localize).
    assert_eq!(printf("%lld", en, &[]), "");
    // Floats through %@ use the culture separator; integers have no grouping.
    assert_eq!(printf("%@", tr, &[Arg::Float(2.25)]), "2,25");
    assert_eq!(printf("%@", en, &[Arg::Float(1.0)]), "1");
    assert_eq!(printf("%d", en, &[Arg::Int(1234567)]), "1234567");
    assert_eq!(printf("%d", en, &[Arg::Int(-5)]), "-5");
    // %d of a float rounds half to even (Convert.ToInt64); numeric strings parse.
    assert_eq!(printf("%d %d %d", en, &[Arg::Float(2.5), Arg::Float(3.5), s("12")]), "2 4 12");
    // Booleans.
    assert_eq!(printf("%@ %d", en, &[Arg::Bool(true), Arg::Bool(true)]), "True 1");
    // %f: default precision 6, ties away from zero on the exact binary value.
    assert_eq!(printf("%f", en, &[Arg::Float(1.5)]), "1.500000");
    assert_eq!(printf("%.2f", en, &[Arg::Float(0.125)]), "0.13");
    assert_eq!(printf("%.2f", en, &[Arg::Float(-0.125)]), "-0.13");
    assert_eq!(printf("%.0f", en, &[Arg::Float(2.5)]), "3");
    assert_eq!(printf("%.1f", en, &[Arg::Float(9.95)]), "9.9"); // 9.95 is below the tie in binary
    assert_eq!(printf("%.1f", en, &[Arg::Float(0.25)]), "0.3");
    assert_eq!(printf("%.2f", en, &[Arg::Float(9.995)]), "9.99");
    assert_eq!(printf("%.1f", en, &[Arg::Float(99.95)]), "100.0");
    assert_eq!(printf("%.f", en, &[Arg::Float(0.5)]), "1");
    // Flags, width and length modifiers are skipped.
    assert_eq!(printf("%05lld|%-3@|%+d", en, &[Arg::Int(7), s("x"), Arg::Int(3)]), "7|x|3");
    assert_eq!(printf("%1$@ %1$@", en, &[s("r")]), "r r");
    // Multi-byte text around specifiers.
    assert_eq!(printf("Çeviri (%@) “ok”", tr, &[s("en")]), "Çeviri (en) “ok”");
}

// ---------------------------------------------------------------- LanguageState

#[test]
fn language_state_notifies_changes() {
    let state = LanguageState::new(UiLanguage::Tr);
    assert_eq!(state.language(), UiLanguage::Tr);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let _sub = state.subscribe(move |l| s2.lock().unwrap().push(*l));
    state.set_language(UiLanguage::En);
    state.set_language(UiLanguage::En);
    state.set_language(UiLanguage::Tr);
    assert_eq!(*seen.lock().unwrap(), vec![UiLanguage::En, UiLanguage::Tr]);
    assert_eq!(state.revision(), 2);
}

#[test]
fn dropped_subscription_stops_notifications() {
    let state = LanguageState::new(UiLanguage::Tr);
    let count = Arc::new(Mutex::new(0));
    let c2 = count.clone();
    let sub = state.subscribe(move |_| *c2.lock().unwrap() += 1);
    state.set_language(UiLanguage::En);
    drop(sub);
    state.set_language(UiLanguage::Tr);
    assert_eq!(*count.lock().unwrap(), 1);
}

#[test]
fn l_uses_the_shared_language_at_call_time() {
    let _g = SHARED_LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    let shared = LanguageState::shared();
    let before = shared.language();
    shared.set_language(UiLanguage::En);
    assert_eq!(sa_core::l("Altyazı üret"), "Generate Subtitles");
    assert_eq!(sa_core::l!("%lld satır · dil: %@", 3, "tr"), "Lines: 3 · language: tr");
    shared.set_language(UiLanguage::Tr);
    assert_eq!(sa_core::l("Altyazı üret"), "Altyazı üret");
    assert_eq!(sa_core::k("Satırı Sil"), "Satırı Sil");
    shared.set_language(before);
}

// ---------------------------------------------------------------- LanguageChoice / AppLanguage

#[test]
fn language_choice_resolution() {
    assert_eq!(LanguageChoice::Turkish.resolved(&["en-US"]), UiLanguage::Tr);
    assert_eq!(LanguageChoice::English.resolved(&["tr-TR"]), UiLanguage::En);
    assert_eq!(LanguageChoice::System.resolved(&["tr-TR"]), UiLanguage::Tr);
    assert_eq!(LanguageChoice::System.resolved(&["fr-FR"]), UiLanguage::En);
    assert_eq!(LanguageChoice::parse(Some("en")), Some(LanguageChoice::English));
    assert_eq!(LanguageChoice::parse(Some("EN")), None);
    assert_eq!(LanguageChoice::parse(None), None);
    assert_eq!(LanguageChoice::Turkish.title(), "Türkçe");
    assert_eq!(LanguageChoice::English.title(), "English");
}

#[test]
fn app_language_applies_immediately_and_persists() {
    let store = InMemorySettingsStore::new();
    let state = LanguageState::new(UiLanguage::En);
    let app = AppLanguage::new(store.clone(), || vec!["tr-TR".to_owned()], state.clone());
    assert_eq!(app.choice(), LanguageChoice::System);
    assert_eq!(state.language(), UiLanguage::Tr);
    let events = Arc::new(Mutex::new(Vec::new()));
    let e2 = events.clone();
    let _sub = app.subscribe(move |c| e2.lock().unwrap().push(*c));

    app.set_choice(LanguageChoice::English);
    assert_eq!(state.language(), UiLanguage::En);
    assert_eq!(store.get_string("uiLanguage").as_deref(), Some("en"));
    assert_eq!(*events.lock().unwrap(), vec![AppLanguageChange::Choice, AppLanguageChange::Effective]);
    app.set_choice(LanguageChoice::English); // equal → nothing
    assert_eq!(events.lock().unwrap().len(), 2);
    assert!(app.needs_relaunch());

    let reloaded = AppLanguage::new(store.clone(), || vec!["tr-TR".to_owned()], state.clone());
    assert_eq!(reloaded.choice(), LanguageChoice::English);
    reloaded.set_choice(LanguageChoice::System);
    assert_eq!(state.language(), UiLanguage::Tr);

    let fresh = AppLanguage::new(InMemorySettingsStore::new(), || vec!["de-DE".to_owned(), "fr-FR".to_owned()], state.clone());
    assert_eq!(fresh.effective(), UiLanguage::En);
}

#[test]
fn app_language_treats_unknown_stored_values_as_system() {
    let store = InMemorySettingsStore::from_json(r#"{"uiLanguage":"EN"}"#);
    let state = LanguageState::new(UiLanguage::Tr);
    let app = AppLanguage::new(store, Vec::new, state.clone());
    assert_eq!(app.choice(), LanguageChoice::System);
    assert_eq!(state.language(), UiLanguage::En);
}

// ---------------------------------------------------------------- LocalizedMessage

#[test]
fn localized_message_compares_by_id_and_follows_the_language() {
    let _g = SHARED_LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(LocalizedMessage::key("Hazır"), LocalizedMessage::key("Hazır"));
    assert_ne!(LocalizedMessage::key("Hazır"), LocalizedMessage::verbatim("Hazır"));
    assert_eq!(LocalizedMessage::key("Hazır").id(), "key:Hazır");
    assert_eq!(LocalizedMessage::verbatim("x").id(), "verbatim:x");
    let shared = LanguageState::shared();
    let before = shared.language();
    let msg = LocalizedMessage::key("Hazır");
    shared.set_language(UiLanguage::En);
    assert_eq!(msg.text(), "Ready");
    shared.set_language(UiLanguage::Tr);
    assert_eq!(msg.text(), "Hazır");
    let nf = LocalizedMessage::error(&EngineError::NotFound);
    assert_eq!(nf.id(), "error:notFound");
    shared.set_language(UiLanguage::En);
    assert!(nf.text().starts_with("Subtitle engine not found"));
    let failed = LocalizedMessage::error(&EngineError::Failed("boom".into()));
    assert_eq!(failed.id(), "error:failed:boom");
    assert_eq!(failed.text(), "boom");
    shared.set_language(before);
}

// ---------------------------------------------------------------- scanner

#[test]
fn scanner_recognizes_rust_and_slint_forms() {
    let rust = r#"
        let a = l("Altyazı üret");
        let b = sa_core::l!("%lld satır · dil: %@", n, lang);
        let c = k("Satırı Sil");
        let d = LocalizedMessage::key("Hazır");
        // l("commented out")
        let e = engine_string("Zaman aşımı", None, &[]);
        let f = localize(lang, "Sistem", &[]);
        let g = l("say \"hi\"");
    "#;
    let keys = sa_core::l10n::scan::scan_rust(rust);
    let names: Vec<_> = keys.iter().map(|k| k.key.as_str()).collect();
    assert_eq!(names, vec!["Altyazı üret", "%lld satır · dil: %@", "Satırı Sil", "Hazır", "Zaman aşımı", "Sistem", "say \"hi\""]);
    assert_eq!(keys[4].scope, sa_core::l10n::scan::KeyScope::Engine);

    let slint = r#"
        Text { text: Loc.t("Altyazı üret"); }
        // Text { text: Loc.t("ignored"); }
        Button { text: Loc.t1("%@ için %@ seçildi.", root.x); tooltip: Loc.t2("a", "b", "c"); }
    "#;
    let keys: Vec<_> = sa_core::l10n::scan::scan_slint(slint).into_iter().map(|k| k.key).collect();
    assert_eq!(keys, vec!["Altyazı üret", "%@ için %@ seçildi.", "a"]);
}
