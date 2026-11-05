//! Theme helpers. Views read colors via [`ActiveTheme`] and never construct
//! GPUI Kit themes directly.

use ferrodb_api::Environment;
use gpui_kit::{App, Hsla, Pixels, Window, hsla, px};

pub use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};

/// User-selectable appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

/// Applies the appearance to all windows.
pub fn apply_appearance(appearance: Appearance, window: Option<&mut Window>, cx: &mut App) {
    match appearance {
        Appearance::System => Theme::sync_system_appearance(window, cx),
        Appearance::Light => Theme::change(ThemeMode::Light, window, cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, window, cx),
    }
}

/// Applies font settings (UI and editor sizes, families).
pub fn apply_fonts(ui_size: f32, mono_size: f32, mono_family: Option<&str>, cx: &mut App) {
    let mono_family = mono_family.map(|s| s.to_owned());
    Theme::update(cx, |t| {
        t.font_size = px(ui_size);
        t.mono_font_size = px(mono_size);
        if let Some(f) = mono_family {
            t.mono_font_family = f.into();
        }
    });
}

pub fn is_dark(cx: &App) -> bool {
    cx.theme().is_dark()
}

/// Color marking a data source environment (e.g. production = red).
/// `None` means "no marker".
pub fn environment_color(env: Environment, cx: &App) -> Option<Hsla> {
    let dark = is_dark(cx);
    let l = if dark { 0.55 } else { 0.45 };
    match env {
        Environment::None => None,
        Environment::Development => Some(hsla(142. / 360., 0.55, l, 1.)),
        Environment::Test => Some(hsla(210. / 360., 0.65, l, 1.)),
        Environment::Staging => Some(hsla(38. / 360., 0.85, l, 1.)),
        Environment::Production => Some(hsla(0., 0.75, l, 1.)),
    }
}

/// Standard spacing tokens used by FerroDb views.
pub mod spacing {
    use super::*;
    pub fn row_height() -> Pixels {
        px(24.)
    }
    pub fn toolbar_height() -> Pixels {
        px(32.)
    }
}
