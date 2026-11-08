//! Appearance settings, overlay style and overlay text (win-core §2, mac-viewmodels §10, win-providers §10.4).

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use crate::engine_types::Sentence;
use crate::settings::SettingsStore;
use crate::subscription::{Subscribers, Subscription};

/// `appTheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    pub const fn raw(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }

    /// Exact, case-sensitive.
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw? {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    /// `Sistem` / `Açık` / `Koyu`.
    pub fn title(self) -> String {
        crate::l10n::l(match self {
            ThemeChoice::System => "Sistem",
            ThemeChoice::Light => "Açık",
            ThemeChoice::Dark => "Koyu",
        })
    }
}

/// `overlayBackgroundStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OverlayBackground {
    #[default]
    Box,
    Shadow,
    Outline,
}

impl OverlayBackground {
    pub const ALL: [OverlayBackground; 3] = [OverlayBackground::Box, OverlayBackground::Shadow, OverlayBackground::Outline];

    pub const fn raw(self) -> &'static str {
        match self {
            OverlayBackground::Box => "box",
            OverlayBackground::Shadow => "shadow",
            OverlayBackground::Outline => "outline",
        }
    }

    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw? {
            "box" => Some(Self::Box),
            "shadow" => Some(Self::Shadow),
            "outline" => Some(Self::Outline),
            _ => None,
        }
    }

    /// `Kutu` / `Gölge` / `Kontur`.
    pub fn title(self) -> String {
        crate::l10n::l(match self {
            OverlayBackground::Box => "Kutu",
            OverlayBackground::Shadow => "Gölge",
            OverlayBackground::Outline => "Kontur",
        })
    }
}

/// An sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl RgbColor {
    pub const WHITE: RgbColor = RgbColor { r: 255, g: 255, b: 255 };

    /// `#RRGGBB` uppercase.
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

/// `#RRGGBB` parsing/formatting (win-core §2.5).
pub struct HexColor;

impl HexColor {
    /// Trim, strip one `#`, exactly 6 hex digits → components 0…1.
    pub fn parse(hex: &str) -> Option<(f64, f64, f64)> {
        let v = Self::parse_u32(hex)?;
        Some((((v >> 16) & 0xFF) as f64 / 255.0, ((v >> 8) & 0xFF) as f64 / 255.0, (v & 0xFF) as f64 / 255.0))
    }
    pub fn parse_rgb(hex: &str) -> Option<RgbColor> {
        let (r, g, b) = Self::parse(hex)?;
        Some(RgbColor { r: Self::byte(r), g: Self::byte(g), b: Self::byte(b) })
    }
    /// Uppercase `#RRGGBB` with `round_half_away(clamp(x,0,1) * 255)`.
    pub fn format(r: f64, g: f64, b: f64) -> String {
        RgbColor { r: Self::byte(r), g: Self::byte(g), b: Self::byte(b) }.hex()
    }
    /// `format(RgbColor)` = [`RgbColor::hex`].
    pub fn format_rgb(color: RgbColor) -> String {
        color.hex()
    }

    fn parse_u32(hex: &str) -> Option<u32> {
        let text = crate::text::trim_dotnet(hex);
        let text = text.strip_prefix('#').unwrap_or(text);
        if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(text, 16).ok()
    }

    fn byte(x: f64) -> u8 {
        let v = if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) };
        crate::text::round_half_away_from_zero(v * 255.0) as u8
    }
}

/// Overlay style value (structural equality over all five fields; win-core §2.4).
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayStyle {
    pub font_family: String,
    pub size_fraction: f64,
    pub text_hex: String,
    pub background: OverlayBackground,
    pub background_opacity: f64,
}

impl OverlayStyle {
    pub const MIN_SIZE: f64 = 0.03;
    pub const MAX_SIZE: f64 = 0.09;
    pub const DEFAULT_SIZE: f64 = 0.05;
    pub const DEFAULT_TEXT_HEX: &'static str = "#FFFFFF";
    pub const DEFAULT_OPACITY: f64 = 0.6;

    /// Clamps size (non-finite → 0.05) and opacity (non-finite → 0.6); keeps `text_hex` unchanged when parseable,
    /// else `#FFFFFF`.
    pub fn new(font_family: String, size_fraction: f64, text_hex: String, background: OverlayBackground, background_opacity: f64) -> Self {
        let size_fraction =
            if size_fraction.is_finite() { size_fraction.clamp(Self::MIN_SIZE, Self::MAX_SIZE) } else { Self::DEFAULT_SIZE };
        let text_hex = if HexColor::parse(&text_hex).is_some() { text_hex } else { Self::DEFAULT_TEXT_HEX.to_owned() };
        let background_opacity =
            if background_opacity.is_finite() { background_opacity.clamp(0.0, 1.0) } else { Self::DEFAULT_OPACITY };
        Self { font_family, size_fraction, text_hex, background, background_opacity }
    }

    /// From stored strings (unknown background → Box).
    pub fn from_stored(font_family: &str, size_fraction: f64, text_hex: &str, background: &str, background_opacity: f64) -> Self {
        Self::new(
            font_family.to_owned(),
            size_fraction,
            text_hex.to_owned(),
            OverlayBackground::parse(Some(background)).unwrap_or_default(),
            background_opacity,
        )
    }

    /// `("", 0.05, "#FFFFFF", Box, 0.6)`.
    pub fn standard() -> Self {
        Self {
            font_family: String::new(),
            size_fraction: Self::DEFAULT_SIZE,
            text_hex: Self::DEFAULT_TEXT_HEX.into(),
            background: OverlayBackground::Box,
            background_opacity: Self::DEFAULT_OPACITY,
        }
    }

    /// `max(12, video_height * size_fraction)`.
    pub fn font_size(&self, video_height: f64) -> f64 {
        (video_height * self.size_fraction).max(12.0)
    }
    /// `clamp(font_size * 0.045, 1.0, 2.5)`.
    pub fn outline_thickness(&self, video_height: f64) -> f64 {
        (self.font_size(video_height) * 0.045).clamp(1.0, 2.5)
    }
    pub fn text_color(&self) -> RgbColor {
        HexColor::parse_rgb(&self.text_hex).unwrap_or(RgbColor::WHITE)
    }
    /// macOS `outline_offsets(r)`: the eight contour copies on one circle (`d = r/√2`).
    pub fn outline_offsets(radius: f64) -> [(f64, f64); 8] {
        let d = radius / std::f64::consts::SQRT_2;
        [(-radius, 0.0), (radius, 0.0), (0.0, -radius), (0.0, radius), (-d, -d), (d, -d), (-d, d), (d, d)]
    }
    /// `max(background_opacity, 0.5)`.
    pub fn shadow_opacity(&self) -> f64 {
        self.background_opacity.max(0.5)
    }
}

/// What changed on [`AppearanceSettings`] (C# property names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppearanceChange {
    Theme,
    UiFontFamily,
    FontFamily,
    Size,
    TextHex,
    Background,
    Opacity,
    SizeText,
    OpacityText,
    Style,
}

/// Settings keys of the appearance (win-core §2.1, mac-viewmodels §10.1).
pub mod keys {
    pub const THEME: &str = crate::settings::keys::APP_THEME;
    pub const UI_FONT: &str = crate::settings::keys::APP_FONT_FAMILY;
    pub const OVERLAY_FONT: &str = crate::settings::keys::OVERLAY_FONT_FAMILY;
    pub const OVERLAY_SIZE: &str = crate::settings::keys::OVERLAY_FONT_SIZE;
    pub const OVERLAY_TEXT_COLOR: &str = crate::settings::keys::OVERLAY_TEXT_COLOR;
    pub const OVERLAY_BACKGROUND: &str = crate::settings::keys::OVERLAY_BACKGROUND_STYLE;
    pub const OVERLAY_OPACITY: &str = crate::settings::keys::OVERLAY_BACKGROUND_OPACITY;
    /// What "Varsayılana döndür" resets (not the theme, not the interface font).
    pub const OVERLAY_KEYS: [&str; 5] = [OVERLAY_FONT, OVERLAY_SIZE, OVERLAY_TEXT_COLOR, OVERLAY_BACKGROUND, OVERLAY_OPACITY];
}

/// Observable appearance settings over a [`SettingsStore`] (win-core §2.6).
///
/// Notifications follow the store: `appTheme` → `Theme`; `appFontFamily` → `UiFontFamily` only; any key starting
/// with `overlay` → `FontFamily, Size, TextHex, Background, Opacity, SizeText, OpacityText, Style` in that order.
/// They run on the thread that changed the store (the UI thread in practice).
pub struct AppearanceSettings {
    store: Arc<dyn SettingsStore>,
    changed: Arc<Subscribers<AppearanceChange>>,
    _store_subscription: Subscription,
}

impl AppearanceSettings {
    /// The `overlay*` notification order.
    pub const STYLE_CHANGES: [AppearanceChange; 8] = [
        AppearanceChange::FontFamily,
        AppearanceChange::Size,
        AppearanceChange::TextHex,
        AppearanceChange::Background,
        AppearanceChange::Opacity,
        AppearanceChange::SizeText,
        AppearanceChange::OpacityText,
        AppearanceChange::Style,
    ];

    pub fn new(store: Arc<dyn SettingsStore>) -> Arc<Self> {
        let changed: Arc<Subscribers<AppearanceChange>> = Arc::new(Subscribers::new());
        let sink = changed.clone();
        let sub = store.subscribe(Box::new(move |key: &str| {
            if key == keys::THEME {
                sink.notify(&AppearanceChange::Theme);
            } else if key == keys::UI_FONT {
                sink.notify(&AppearanceChange::UiFontFamily);
            } else if key.starts_with("overlay") {
                for c in Self::STYLE_CHANGES {
                    sink.notify(&c);
                }
            }
        }));
        Arc::new(Self { store, changed, _store_subscription: sub })
    }
    pub fn theme(&self) -> ThemeChoice {
        ThemeChoice::parse(self.store.get_string(keys::THEME).as_deref()).unwrap_or_default()
    }
    pub fn set_theme(&self, v: ThemeChoice) {
        self.store.set_string(keys::THEME, v.raw());
    }
    pub fn ui_font_family(&self) -> String {
        self.store.get_string(keys::UI_FONT).unwrap_or_default()
    }
    /// Stored trimmed.
    pub fn set_ui_font_family(&self, v: &str) {
        self.store.set_string(keys::UI_FONT, crate::text::trim_dotnet(v));
    }
    pub fn style(&self) -> OverlayStyle {
        OverlayStyle::from_stored(
            &self.store.get_string(keys::OVERLAY_FONT).unwrap_or_default(),
            self.store.get_double(keys::OVERLAY_SIZE).unwrap_or(OverlayStyle::DEFAULT_SIZE),
            &self.store.get_string(keys::OVERLAY_TEXT_COLOR).unwrap_or_else(|| OverlayStyle::DEFAULT_TEXT_HEX.to_owned()),
            &self.store.get_string(keys::OVERLAY_BACKGROUND).unwrap_or_else(|| OverlayBackground::Box.raw().to_owned()),
            self.store.get_double(keys::OVERLAY_OPACITY).unwrap_or(OverlayStyle::DEFAULT_OPACITY),
        )
    }
    /// Overlay font family as stored (`""` = system font).
    pub fn font_family(&self) -> String {
        self.store.get_string(keys::OVERLAY_FONT).unwrap_or_default()
    }
    /// Not trimmed.
    pub fn set_font_family(&self, v: &str) {
        self.store.set_string(keys::OVERLAY_FONT, v);
    }
    /// The clamped size fraction (`style().size_fraction`).
    pub fn size(&self) -> f64 {
        self.style().size_fraction
    }
    /// Raw, unclamped; NaN removes.
    pub fn set_size(&self, v: f64) {
        self.store.set_double(keys::OVERLAY_SIZE, v);
    }
    /// `style().text_hex`.
    pub fn text_hex(&self) -> String {
        self.style().text_hex
    }
    /// Raw, even if invalid.
    pub fn set_text_hex(&self, v: &str) {
        self.store.set_string(keys::OVERLAY_TEXT_COLOR, v);
    }
    /// `style().background`.
    pub fn background(&self) -> OverlayBackground {
        self.style().background
    }
    pub fn set_background(&self, v: OverlayBackground) {
        self.store.set_string(keys::OVERLAY_BACKGROUND, v.raw());
    }
    /// `style().background_opacity`.
    pub fn opacity(&self) -> f64 {
        self.style().background_opacity
    }
    pub fn set_opacity(&self, v: f64) {
        self.store.set_double(keys::OVERLAY_OPACITY, v);
    }
    /// `L("%lld%%", round_half_away(size * 100))` → tr `%5`, en `5%`.
    pub fn size_text(&self) -> String {
        Self::percent_text(self.style().size_fraction)
    }
    pub fn opacity_text(&self) -> String {
        Self::percent_text(self.style().background_opacity)
    }
    /// `L("%lld%%", round_half_away(fraction * 100))`.
    pub fn percent_text(fraction: f64) -> String {
        crate::l!("%lld%%", crate::text::round_half_away_from_zero(fraction * 100.0) as i64)
    }
    /// "Varsayılana döndür": removes the five overlay keys only (in key order).
    pub fn reset_overlay(&self) {
        for key in keys::OVERLAY_KEYS {
            self.store.remove(key);
        }
    }
    /// Re-notify SizeText/OpacityText after a language switch.
    pub fn refresh_localization(&self) {
        self.changed.notify(&AppearanceChange::SizeText);
        self.changed.notify(&AppearanceChange::OpacityText);
    }
    pub fn subscribe(&self, handler: impl Fn(&AppearanceChange) + Send + Sync + 'static) -> Subscription {
        self.changed.subscribe(handler)
    }
}

/// macOS `InterfaceFont.font(family, installed)`: the family to apply, or `None` (system font) when the trimmed
/// family is empty or not installed (a removed font must not break the layout).
pub fn interface_font_family(family: &str, installed: &[String]) -> Option<String> {
    let name = crate::text::trim_dotnet(family);
    (!name.is_empty() && installed.iter().any(|f| f == name)).then(|| name.to_owned())
}

/// `subtitleDisplayMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SubtitleDisplay {
    #[default]
    Original,
    Translation,
    Both,
}

impl SubtitleDisplay {
    pub const ALL: [SubtitleDisplay; 3] = [SubtitleDisplay::Original, SubtitleDisplay::Translation, SubtitleDisplay::Both];

    pub const fn raw(self) -> &'static str {
        match self {
            SubtitleDisplay::Original => "original",
            SubtitleDisplay::Translation => "translation",
            SubtitleDisplay::Both => "both",
        }
    }
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw? {
            "original" => Some(Self::Original),
            "translation" => Some(Self::Translation),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
    /// `Orijinal` / `Çeviri` / `İkisi`.
    pub fn title(self) -> String {
        crate::l10n::l(match self {
            SubtitleDisplay::Original => "Orijinal",
            SubtitleDisplay::Translation => "Çeviri",
            SubtitleDisplay::Both => "İkisi",
        })
    }
}

/// A translated line keyed by the source line's uid (win-viewmodels §2.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TranslationEntry {
    pub text: String,
    pub source_text: String,
    pub language: String,
    pub needs_review: bool,
}

impl TranslationEntry {
    /// `source_text != sentence.text` (exact).
    pub fn is_stale(&self, sentence: &Sentence) -> bool {
        self.source_text != sentence.text
    }
}

/// Map of translations.
pub type Translations = HashMap<Uuid, TranslationEntry>;

/// Overlay text composition (win-providers §10.4 = mac-viewmodels §10.4).
pub struct OverlayText;

impl OverlayText {
    /// Lines for `uid` per display mode, dialogue dash applied, joined with `\n`; `None` when nothing is left.
    pub fn compose(sentences: &[Sentence], translations: &Translations, display: SubtitleDisplay, uid: Uuid) -> Option<String> {
        let index = sentences.iter().position(|s| s.uid == uid)?;
        let dash = starts_dialogue_turn(sentences, index);
        let original = crate::text::trim_dotnet(&sentences[index].text).to_owned();
        let translated = translations.get(&uid).map(|e| crate::text::trim_dotnet(&e.text).to_owned());
        let lines: Vec<String> = match display {
            SubtitleDisplay::Translation => vec![match translated {
                Some(t) if !t.is_empty() => t,
                _ => original,
            }],
            SubtitleDisplay::Both => vec![original, translated.unwrap_or_default()],
            SubtitleDisplay::Original => vec![original],
        };
        let shown: Vec<String> = lines.into_iter().filter(|l| !l.is_empty()).map(|l| apply_dash(dash, l)).collect();
        if shown.is_empty() { None } else { Some(shown.join("
")) }
    }
}

/// The engine's speaker dash rule (CLAUDE.md, `DialogueDashes`): with at least two distinct non-null speakers, a line
/// dashes when its speaker is non-null and differs from the last non-null speaker before it. (Same rule as
/// `sa_enginekit::editing`, kept here because this crate sits below it.)
fn starts_dialogue_turn(sentences: &[Sentence], index: usize) -> bool {
    if index >= sentences.len() {
        return false;
    }
    let mut speakers: Vec<i64> = sentences.iter().filter_map(|s| s.speaker).collect();
    speakers.sort_unstable();
    speakers.dedup();
    if speakers.len() < 2 {
        return false;
    }
    let previous = sentences[..index].iter().rev().find_map(|s| s.speaker);
    match sentences[index].speaker {
        Some(sp) => previous != Some(sp),
        None => false,
    }
}

/// `DialogueDash.Apply`: `"- " + text` unless it already starts with `"- "` or `"– "`.
fn apply_dash(dash: bool, text: String) -> String {
    if dash && !text.starts_with("- ") && !text.starts_with("– ") { format!("- {text}") } else { text }
}

/// Speaker colors (index modulo 6, negative-safe).
pub struct SpeakerPalette;

impl SpeakerPalette {
    pub const COUNT: usize = 6;
    pub const LIGHT: [RgbColor; 6] = [
        RgbColor { r: 0, g: 122, b: 255 },
        RgbColor { r: 255, g: 149, b: 0 },
        RgbColor { r: 40, g: 205, b: 65 },
        RgbColor { r: 175, g: 82, b: 222 },
        RgbColor { r: 255, g: 45, b: 85 },
        RgbColor { r: 89, g: 173, b: 196 },
    ];
    pub const DARK: [RgbColor; 6] = [
        RgbColor { r: 10, g: 132, b: 255 },
        RgbColor { r: 255, g: 159, b: 10 },
        RgbColor { r: 50, g: 215, b: 75 },
        RgbColor { r: 191, g: 90, b: 242 },
        RgbColor { r: 255, g: 55, b: 95 },
        RgbColor { r: 106, g: 196, b: 220 },
    ];

    pub fn color(index: i64, dark: bool) -> RgbColor {
        let i = index.rem_euclid(Self::COUNT as i64) as usize;
        if dark { Self::DARK[i] } else { Self::LIGHT[i] }
    }
}
