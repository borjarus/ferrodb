//! # ferrodb-ui-kit
//!
//! Facade over GPUI Kit (ADR-001). Application views import GPUI and its
//! components **only through this crate**, so a GPUI Kit upgrade (or the
//! plan-B switch to egui) is contained here.
//!
//! - [`gpui`] — the GPUI core (`use ferrodb_ui_kit::gpui::*`)
//! - [`kit`] — the styled component library
//! - [`theme`] — FerroDb theme helpers (environment colors, theme switching)
//! - [`icons`] — icon set used by the application
//! - [`assets`] — the application asset source

pub mod icons;
pub mod theme;

/// GPUI core (re-exported GPUI Kit root).
pub mod gpui {
    pub use gpui_kit::*;
}

/// Styled component library (GPUI Component).
pub mod kit {
    pub use gpui_kit::component::*;
}

/// Lower-level GPUI Base layer (dock state, Root).
pub mod base {
    pub use gpui_kit::base::*;
}

/// UI integration-test helpers (`test-support` feature).
#[cfg(feature = "test-support")]
pub mod test {
    pub use gpui_kit::test::*;
}

pub use gpui_kit::actions;

use gpui_kit::{App, AssetSource, SharedString};
use std::borrow::Cow;

/// Asset source of the application: the full Lucide catalog (database, table,
/// key… icons are not part of the default component set).
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        gpui_kit::assets::AllAssets.list(path)
    }
}

/// Creates the platform application with FerroDb assets.
pub fn application() -> gpui_kit::Application {
    gpui_kit::application().with_assets(AppAssets)
}

/// Initializes GPUI Kit layers. Call once at startup (and in every UI test).
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
}

/// Opens a window wrapped in the GPUI Kit Root (dialogs, notifications, sheets).
pub use gpui_kit::open_window;
