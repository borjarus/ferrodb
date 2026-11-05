//! # ferrodb-ui
//!
//! Views and their presentation state. No query logic and no I/O to databases
//! here: everything goes through [`ferrodb_api::Backend`], held in
//! [`AppState`]. In phases 1–2 that backend is the mock.

pub mod actions;
pub mod i18n;
pub mod keymap;
pub mod menus;
pub mod output;
pub mod palette;
pub mod panels;
pub mod project;
pub mod settings;
pub mod settings_view;
pub mod workspace;

use std::{path::PathBuf, sync::Arc};

use ferrodb_api::Backend;
use ferrodb_ui_kit::{
    gpui::{App, Global},
    theme,
};

pub use workspace::Workspace;

/// Developer-only control of backend scenarios (implemented for the mock by
/// the application; absent for a real backend).
pub trait ScenarioControl: Send + Sync {
    /// `(id, fluent message id)` of each scenario.
    fn scenarios(&self) -> Vec<&'static str>;
    fn current(&self) -> &'static str;
    fn set(&self, id: &str);
}

/// Where global files live.
#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }
    pub fn keymap_file(&self) -> PathBuf {
        self.config_dir.join("keymap.toml")
    }
}

/// Global application state shared by all views.
#[derive(Clone)]
pub struct AppState {
    pub backend: Arc<dyn Backend>,
    pub scenarios: Option<Arc<dyn ScenarioControl>>,
    /// `None` in tests: nothing is written to disk.
    pub paths: Option<AppPaths>,
}

impl Global for AppState {}

impl AppState {
    pub fn global(cx: &App) -> &AppState {
        cx.global::<AppState>()
    }

    pub fn backend(cx: &App) -> Arc<dyn Backend> {
        cx.global::<AppState>().backend.clone()
    }
}

/// Initializes the UI layer: GPUI Kit, globals, settings-derived theme and
/// locale, panel registry and the output log. Call once before opening windows.
pub fn init(state: AppState, cx: &mut App) {
    ferrodb_ui_kit::init(cx);

    let settings = match &state.paths {
        Some(p) => settings::AppSettings::load(&p.settings_file()),
        None => settings::AppSettings::default(),
    };
    let store = settings::SettingsStore {
        settings,
        path: state.paths.as_ref().map(|p| p.settings_file()),
    };
    i18n::set_locale(store.settings.locale());
    let s = store.settings.clone();
    cx.set_global(store);
    cx.set_global(state);

    apply_look(&s, cx);
    output::init(cx);
    panels::register(cx);
    menus::install(cx);
}

/// Applies appearance and fonts from settings.
pub fn apply_look(s: &settings::AppSettings, cx: &mut App) {
    theme::apply_appearance(s.appearance(), None, cx);
    let family = (!s.editor_font_family.is_empty()).then_some(s.editor_font_family.as_str());
    theme::apply_fonts(s.ui_font_size, s.editor_font_size, family, cx);
}
