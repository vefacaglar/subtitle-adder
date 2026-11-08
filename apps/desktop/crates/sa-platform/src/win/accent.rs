//! System accent colors (win-ui-main §5): `Windows.UI.ViewManagement.UISettings.GetColorValue(Accent…)`, kept up to
//! date through `ColorValuesChanged`. The Fluent tokens the app uses are `AccentFillColorDefault` (light: AccentDark1,
//! dark: AccentLight2) and `AccentTextFillColorPrimary` (light: AccentDark2, dark: AccentLight3).

use windows::Foundation::TypedEventHandler;
use windows::UI::ViewManagement::{UIColorType, UISettings};
use windows::core::IInspectable;

/// An sRGB color with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// `0xAARRGGBB`.
    pub const fn argb(self) -> u32 {
        (self.a as u32) << 24 | (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

/// The accent color and its six shades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccentPalette {
    pub accent: Rgba,
    pub dark1: Rgba,
    pub dark2: Rgba,
    pub dark3: Rgba,
    pub light1: Rgba,
    pub light2: Rgba,
    pub light3: Rgba,
}

impl AccentPalette {
    /// The Windows default blue (`#0078D4` and its shades), used when UISettings is unavailable.
    pub const DEFAULT: AccentPalette = AccentPalette {
        accent: Rgba::rgb(0x00, 0x78, 0xD4),
        dark1: Rgba::rgb(0x00, 0x5A, 0x9E),
        dark2: Rgba::rgb(0x00, 0x42, 0x75),
        dark3: Rgba::rgb(0x00, 0x26, 0x42),
        light1: Rgba::rgb(0x42, 0x9C, 0xE3),
        light2: Rgba::rgb(0x76, 0xB9, 0xED),
        light3: Rgba::rgb(0x99, 0xCC, 0xF7),
    };

    /// The current system palette (`None` when UISettings cannot be created).
    pub fn system() -> Option<AccentPalette> {
        let settings = UISettings::new().ok()?;
        read(&settings)
    }

    /// [`Self::system`] or [`Self::DEFAULT`].
    pub fn current() -> AccentPalette {
        Self::system().unwrap_or(Self::DEFAULT)
    }

    /// `AccentFillColorDefault`: AccentDark1 (light theme) / AccentLight2 (dark theme).
    pub fn fill(&self, dark: bool) -> Rgba {
        if dark { self.light2 } else { self.dark1 }
    }

    /// `AccentTextFillColorPrimary`: AccentDark2 (light theme) / AccentLight3 (dark theme).
    pub fn text(&self, dark: bool) -> Rgba {
        if dark { self.light3 } else { self.dark2 }
    }
}

fn read(settings: &UISettings) -> Option<AccentPalette> {
    let get = |t: UIColorType| settings.GetColorValue(t).ok().map(|c| Rgba { r: c.R, g: c.G, b: c.B, a: c.A });
    Some(AccentPalette {
        accent: get(UIColorType::Accent)?,
        dark1: get(UIColorType::AccentDark1)?,
        dark2: get(UIColorType::AccentDark2)?,
        dark3: get(UIColorType::AccentDark3)?,
        light1: get(UIColorType::AccentLight1)?,
        light2: get(UIColorType::AccentLight2)?,
        light3: get(UIColorType::AccentLight3)?,
    })
}

/// Whether the system app theme is dark (UISettings `Background` is dark), for `appTheme = system`.
pub fn system_uses_dark_theme() -> Option<bool> {
    let settings = UISettings::new().ok()?;
    let c = settings.GetColorValue(UIColorType::Background).ok()?;
    // Light theme: white background; dark theme: black.
    Some((c.R as u32 + c.G as u32 + c.B as u32) < 3 * 128)
}

/// Calls the handler with the new palette whenever the system colors change (`ColorValuesChanged`, raised on a
/// background thread: the handler posts to the UI thread itself). Dropping the watcher unsubscribes.
pub struct AccentWatcher {
    settings: UISettings,
    token: i64,
}

impl AccentWatcher {
    pub fn start(handler: Box<dyn Fn(AccentPalette) + Send + Sync>) -> Option<AccentWatcher> {
        let settings = UISettings::new().ok()?;
        let token = settings
            .ColorValuesChanged(&TypedEventHandler::<UISettings, IInspectable>::new(move |sender, _| {
                if let Some(s) = sender.as_ref()
                    && let Some(p) = read(s)
                {
                    handler(p);
                }
                Ok(())
            }))
            .ok()?;
        Some(AccentWatcher { settings, token })
    }
}

impl Drop for AccentWatcher {
    fn drop(&mut self) {
        let _ = self.settings.RemoveColorValuesChanged(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fluent_tokens_pick_the_documented_shades() {
        let p = AccentPalette::DEFAULT;
        assert_eq!(p.fill(false), p.dark1);
        assert_eq!(p.fill(true), p.light2);
        assert_eq!(p.text(false), p.dark2);
        assert_eq!(p.text(true), p.light3);
        assert_eq!(Rgba::rgb(0x00, 0x78, 0xD4).argb(), 0xFF00_78D4);
    }

    #[test]
    fn the_system_palette_is_readable_and_watchable() {
        let p = AccentPalette::current();
        assert_eq!(p.accent.a, 255);
        let _ = system_uses_dark_theme();
        let w = AccentWatcher::start(Box::new(|_| {}));
        assert!(w.is_some(), "UISettings works in a desktop process");
    }
}
