//! Appearance tests: ports of LocalizationAndAppearanceTests (appearance part), AppearanceFontTests,
//! AppearanceSettingsReadClampedValuesAndReset, OverlayTextTests and PresentationTests (palette, display modes).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sa_core::appearance::{
    AppearanceChange, AppearanceSettings, HexColor, OverlayBackground, OverlayStyle, OverlayText, RgbColor, SpeakerPalette,
    SubtitleDisplay, ThemeChoice, TranslationEntry, Translations, interface_font_family,
};
use sa_core::engine_types::Sentence;
use sa_core::l10n::{LanguageState, UiLanguage};
use sa_core::settings::{InMemorySettingsStore, SettingsStore};

static SHARED_LANGUAGE: Mutex<()> = Mutex::new(());

fn recorder(appearance: &AppearanceSettings) -> (Arc<Mutex<Vec<AppearanceChange>>>, sa_core::Subscription) {
    let raised = Arc::new(Mutex::new(Vec::new()));
    let r = raised.clone();
    let sub = appearance.subscribe(move |c| r.lock().unwrap().push(*c));
    (raised, sub)
}

#[test]
fn theme_mapping() {
    assert_eq!(ThemeChoice::System.raw(), "system");
    assert_eq!(ThemeChoice::Light.raw(), "light");
    assert_eq!(ThemeChoice::Dark.raw(), "dark");
    assert_eq!(ThemeChoice::parse(Some("dark")), Some(ThemeChoice::Dark));
    assert_eq!(ThemeChoice::parse(Some("sepia")), None);
    assert_eq!(ThemeChoice::parse(Some("Dark")), None);
    assert_eq!(ThemeChoice::parse(None), None);
}

#[test]
fn overlay_style_clamps_stored_values() {
    let style = OverlayStyle::from_stored("Menlo", 5.0, "zzz", "nope", -1.0);
    assert_eq!(style.size_fraction, OverlayStyle::MAX_SIZE);
    assert_eq!(style.text_hex, OverlayStyle::DEFAULT_TEXT_HEX);
    assert_eq!(style.background, OverlayBackground::Box);
    assert_eq!(style.background_opacity, 0.0);
    assert_eq!(style.font_family, "Menlo");
    assert_eq!(OverlayStyle::from_stored("", f64::NAN, "#000000", "shadow", 7.0).size_fraction, OverlayStyle::DEFAULT_SIZE);
    assert_eq!(OverlayStyle::from_stored("", 0.0, "#000000", "shadow", 7.0).background_opacity, 1.0);
    assert_eq!(OverlayStyle::from_stored("", 0.0, "#000000", "shadow", f64::INFINITY).background_opacity, OverlayStyle::DEFAULT_OPACITY);
    assert_eq!(OverlayStyle::from_stored("", 0.0, "#000000", "shadow", 1.0).size_fraction, OverlayStyle::MIN_SIZE);
    // A parseable hex is kept verbatim.
    assert_eq!(OverlayStyle::from_stored("", 0.05, " ff8000 ", "outline", 0.5).text_hex, " ff8000 ");
    assert_eq!(OverlayStyle::from_stored("", 0.05, " ff8000 ", "outline", 0.5).text_color(), RgbColor { r: 255, g: 128, b: 0 });
    assert_eq!(OverlayStyle::from_stored("", 0.05, "nope", "outline", 0.5).text_color(), RgbColor::WHITE);
}

#[test]
fn overlay_font_size_and_derived_values() {
    let style = OverlayStyle::standard();
    assert!((style.font_size(1000.0) - 50.0).abs() < 1e-9);
    assert_eq!(style.font_size(100.0), 12.0); // minimum readable size
    assert_eq!(style.outline_thickness(100.0), 1.0);
    assert!((style.outline_thickness(1000.0) - 2.25).abs() < 1e-9);
    assert_eq!(style.outline_thickness(4000.0), 2.5);
    assert_eq!(style.shadow_opacity(), 0.6);
    assert_eq!(OverlayStyle::from_stored("", 0.05, "#FFFFFF", "shadow", 0.1).shadow_opacity(), 0.5);
    let offsets = OverlayStyle::outline_offsets(2.0);
    assert_eq!(offsets[0], (-2.0, 0.0));
    let d = offsets[7].0;
    assert!((d * d * 2.0 - 4.0).abs() < 1e-9, "diagonals lie on the same circle");
}

#[test]
fn hex_color_round_trip() {
    let (r, g, b) = HexColor::parse("#FF8000").unwrap();
    assert_eq!(r, 1.0);
    assert!((g - 128.0 / 255.0).abs() < 1e-4);
    assert_eq!(b, 0.0);
    assert_eq!(HexColor::format(r, g, b), "#FF8000");
    assert_eq!(HexColor::parse("12345"), None);
    assert_eq!(HexColor::parse("#GGGGGG"), None);
    assert_eq!(HexColor::parse("00ff00").map(|c| c.1), Some(1.0));
    assert_eq!(HexColor::parse("##00ff00"), None);
    assert_eq!(HexColor::parse("+0ff00"), None);
    assert_eq!(HexColor::format(2.0, -1.0, 0.5), "#FF0080");
    assert_eq!(HexColor::parse_rgb("#007aff"), Some(RgbColor { r: 0, g: 122, b: 255 }));
    assert_eq!(HexColor::format_rgb(RgbColor { r: 1, g: 2, b: 171 }), "#0102AB");
}

#[test]
fn appearance_settings_read_clamped_values_and_reset() {
    let store = InMemorySettingsStore::new();
    let appearance = AppearanceSettings::new(store.clone());
    assert_eq!(appearance.theme(), ThemeChoice::System);
    assert_eq!(appearance.style(), OverlayStyle::standard());
    let (raised, _sub) = recorder(&appearance);
    appearance.set_theme(ThemeChoice::Dark);
    appearance.set_size(1.0);
    appearance.set_background(OverlayBackground::Shadow);
    assert_eq!(store.get_string("appTheme").as_deref(), Some("dark"));
    assert_eq!(appearance.style().size_fraction, OverlayStyle::MAX_SIZE);
    assert_eq!(store.get_double("overlayFontSize"), Some(1.0), "stored raw, clamped on read");
    assert_eq!(store.get_string("overlayBackgroundStyle").as_deref(), Some("shadow"));
    {
        let raised = raised.lock().unwrap();
        assert!(raised.contains(&AppearanceChange::Theme));
        assert!(raised.contains(&AppearanceChange::Style));
        // One overlay key change raises the eight style notifications in order.
        let after_theme: Vec<_> = raised.iter().skip(1).take(8).copied().collect();
        assert_eq!(after_theme, AppearanceSettings::STYLE_CHANGES);
    }
    appearance.reset_overlay();
    assert_eq!(appearance.style(), OverlayStyle::standard());
    assert_eq!(appearance.theme(), ThemeChoice::Dark);
    for key in ["overlayFontFamily", "overlayFontSize", "overlayTextColor", "overlayBackgroundStyle", "overlayBackgroundOpacity"] {
        assert!(store.raw(key).is_none(), "{key} removed");
    }
}

#[test]
fn ui_font_persists_independently_and_survives_subtitle_reset() {
    let store = InMemorySettingsStore::new();
    let appearance = AppearanceSettings::new(store.clone());
    appearance.set_font_family("Arial");
    appearance.set_background(OverlayBackground::Outline);
    let (raised, _sub) = recorder(&appearance);
    appearance.set_ui_font_family("  Verdana  ");
    assert_eq!(appearance.ui_font_family(), "Verdana");
    assert_eq!(appearance.style().font_family, "Arial");
    assert!(raised.lock().unwrap().contains(&AppearanceChange::UiFontFamily));
    assert!(!raised.lock().unwrap().contains(&AppearanceChange::Style));

    let restored = AppearanceSettings::new(store.clone());
    assert_eq!(restored.ui_font_family(), "Verdana");
    assert_eq!(restored.background(), OverlayBackground::Outline);
    restored.reset_overlay();
    assert_eq!(restored.style(), OverlayStyle::standard());
    assert_eq!(restored.ui_font_family(), "Verdana");
    restored.set_ui_font_family("");
    assert_eq!(AppearanceSettings::new(store).ui_font_family(), "");
}

#[test]
fn background_modes_round_trip_without_changing_existing_choices() {
    for (stored, expected) in [("box", OverlayBackground::Box), ("shadow", OverlayBackground::Shadow), ("outline", OverlayBackground::Outline)] {
        let appearance = AppearanceSettings::new(InMemorySettingsStore::new());
        appearance.set_background(expected);
        assert_eq!(appearance.style().background, expected);
        assert_eq!(expected.raw(), stored);
        assert_eq!(OverlayBackground::parse(Some(stored)), Some(expected));
        assert!(OverlayBackground::ALL.contains(&expected));
    }
}

#[test]
fn raw_setters_store_invalid_values_and_getters_sanitize() {
    let store = InMemorySettingsStore::new();
    let appearance = AppearanceSettings::new(store.clone());
    appearance.set_text_hex("not a color");
    assert_eq!(store.get_string("overlayTextColor").as_deref(), Some("not a color"));
    assert_eq!(appearance.text_hex(), "#FFFFFF");
    appearance.set_size(f64::NAN);
    assert!(store.raw("overlayFontSize").is_none(), "NaN removes");
    appearance.set_opacity(0.25);
    assert_eq!(appearance.opacity(), 0.25);
    appearance.set_font_family("  Menlo ");
    assert_eq!(appearance.font_family(), "  Menlo ", "the overlay font is not trimmed");
}

#[test]
fn percent_texts_follow_the_language_and_refresh() {
    let _g = SHARED_LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    let saved = LanguageState::shared().language();
    let appearance = AppearanceSettings::new(InMemorySettingsStore::new());
    LanguageState::shared().set_language(UiLanguage::Tr);
    assert_eq!(appearance.size_text(), "%5");
    assert_eq!(appearance.opacity_text(), "%60");
    LanguageState::shared().set_language(UiLanguage::En);
    assert_eq!(appearance.size_text(), "5%");
    appearance.set_size(0.045);
    assert_eq!(appearance.size_text(), "5%", "4.5 rounds half away from zero");
    let (raised, _sub) = recorder(&appearance);
    appearance.refresh_localization();
    assert_eq!(*raised.lock().unwrap(), [AppearanceChange::SizeText, AppearanceChange::OpacityText]);
    LanguageState::shared().set_language(saved);
}

#[test]
fn titles_are_localized() {
    let _g = SHARED_LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    let saved = LanguageState::shared().language();
    LanguageState::shared().set_language(UiLanguage::En);
    assert_eq!(ThemeChoice::Dark.title(), "Dark");
    assert_eq!(OverlayBackground::Outline.title(), "Outline");
    assert_eq!(SubtitleDisplay::Both.title(), "Both");
    LanguageState::shared().set_language(UiLanguage::Tr);
    assert_eq!(ThemeChoice::System.title(), "Sistem");
    assert_eq!(OverlayBackground::Box.title(), "Kutu");
    assert_eq!(SubtitleDisplay::Translation.title(), "Çeviri");
    LanguageState::shared().set_language(saved);
}

#[test]
fn interface_font_falls_back_to_the_system_font() {
    let installed = vec!["Menlo".to_owned(), "Helvetica Neue".to_owned()];
    assert_eq!(interface_font_family(" Menlo ", &installed).as_deref(), Some("Menlo"));
    assert_eq!(interface_font_family("Removed Font", &installed), None);
    assert_eq!(interface_font_family("   ", &installed), None);
}

// ---------------------------------------------------------------- display modes, palette

#[test]
fn display_modes_round_trip() {
    for mode in SubtitleDisplay::ALL {
        assert_eq!(SubtitleDisplay::parse(Some(mode.raw())), Some(mode));
    }
    assert_eq!(SubtitleDisplay::parse(Some("x")), None);
}

#[test]
fn speaker_palette_wraps_and_handles_negatives() {
    assert_eq!(SpeakerPalette::color(0, false), SpeakerPalette::color(SpeakerPalette::COUNT as i64, false));
    assert_eq!(SpeakerPalette::color(SpeakerPalette::COUNT as i64 - 1, false), SpeakerPalette::color(-1, false));
    assert_eq!(SpeakerPalette::color(0, false).hex(), "#007AFF");
    assert_ne!(SpeakerPalette::color(0, false), SpeakerPalette::color(0, true));
}

// ---------------------------------------------------------------- OverlayText

fn speaker_line(id: i64, start: f64, text: &str, speaker: Option<i64>) -> Sentence {
    let mut s = Sentence::new(id, start, start + 1.0, text);
    s.speaker = speaker;
    s
}

fn dialogue() -> Vec<Sentence> {
    vec![speaker_line(1, 0.0, "Hello.", Some(0)), speaker_line(2, 1.0, "More.", Some(0)), speaker_line(3, 2.0, "- Hi.", Some(1))]
}

fn translations(list: &[Sentence]) -> Translations {
    list.iter()
        .map(|s| {
            (s.uid, TranslationEntry { text: format!("T:{}", s.text), source_text: s.text.clone(), language: "es".into(), needs_review: false })
        })
        .collect()
}

fn text(list: &[Sentence], index: usize, display: SubtitleDisplay) -> Option<String> {
    OverlayText::compose(list, &translations(list), display, list[index].uid)
}

#[test]
fn dash_follows_turn_rule_on_original() {
    let list = dialogue();
    assert_eq!(text(&list, 0, SubtitleDisplay::Original).as_deref(), Some("- Hello."));
    assert_eq!(text(&list, 1, SubtitleDisplay::Original).as_deref(), Some("More."));
}

#[test]
fn already_dashed_shown_text_gets_no_second_dash() {
    let list = dialogue();
    assert_eq!(text(&list, 2, SubtitleDisplay::Original).as_deref(), Some("- Hi."));
    // The translation text has no dash of its own, so it does get one.
    assert_eq!(text(&list, 2, SubtitleDisplay::Translation).as_deref(), Some("- T:- Hi."));
}

#[test]
fn both_mode_dashes_each_line() {
    let list = dialogue();
    assert_eq!(text(&list, 0, SubtitleDisplay::Both).as_deref(), Some("- Hello.\n- T:Hello."));
    assert_eq!(text(&list, 1, SubtitleDisplay::Both).as_deref(), Some("More.\nT:More."));
}

#[test]
fn translation_falls_back_to_original_without_entry() {
    let list = dialogue();
    let output = OverlayText::compose(&list, &HashMap::new(), SubtitleDisplay::Translation, list[0].uid);
    assert_eq!(output.as_deref(), Some("- Hello."));
}

#[test]
fn no_dash_without_speakers_and_null_speakers_do_not_reset_the_turn() {
    let plain = vec![Sentence::new(1, 0.0, 1.0, "A")];
    assert_eq!(OverlayText::compose(&plain, &HashMap::new(), SubtitleDisplay::Original, plain[0].uid).as_deref(), Some("A"));
    let list = vec![
        speaker_line(1, 0.0, "a", Some(0)),
        speaker_line(2, 1.0, "b", None),
        speaker_line(3, 2.0, "c", Some(0)),
        speaker_line(4, 3.0, "d", Some(1)),
    ];
    let t = |i: usize| OverlayText::compose(&list, &HashMap::new(), SubtitleDisplay::Original, list[i].uid);
    assert_eq!(t(0).as_deref(), Some("- a"));
    assert_eq!(t(1).as_deref(), Some("b"));
    assert_eq!(t(2).as_deref(), Some("c"));
    assert_eq!(t(3).as_deref(), Some("- d"));
}

#[test]
fn unknown_uid_or_blank_text_gives_nothing() {
    let list = dialogue();
    assert_eq!(OverlayText::compose(&list, &translations(&list), SubtitleDisplay::Original, uuid::Uuid::new_v4()), None);
    let blank = vec![Sentence::new(1, 0.0, 1.0, "   ")];
    assert_eq!(OverlayText::compose(&blank, &HashMap::new(), SubtitleDisplay::Both, blank[0].uid), None);
    let mut entries = HashMap::new();
    entries.insert(
        blank[0].uid,
        TranslationEntry { text: "  Hola  ".into(), source_text: "x".into(), language: "es".into(), needs_review: false },
    );
    assert_eq!(OverlayText::compose(&blank, &entries, SubtitleDisplay::Both, blank[0].uid).as_deref(), Some("Hola"));
    assert!(entries[&blank[0].uid].is_stale(&blank[0]));
}
