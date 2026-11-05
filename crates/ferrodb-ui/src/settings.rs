//! Application settings (`settings.toml` in the global configuration directory).
//!
//! Settings are a GPUI global; views observe it (`cx.observe_global::<SettingsStore>`)
//! and every change is written back to disk immediately.

use std::path::{Path, PathBuf};

use ferrodb_ui_kit::{
    gpui::{App, BorrowAppContext as _, Global},
    theme::Appearance,
};
use serde::{Deserialize, Serialize};

use crate::i18n::Locale;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// `system` | `light` | `dark`
    pub appearance: String,
    /// `en` | `pl`; empty = follow the system.
    pub locale: String,
    pub ui_font_size: f32,
    pub editor_font_size: f32,
    /// Monospace font for consoles; empty = theme default.
    pub editor_font_family: String,
    pub show_line_numbers: bool,
    /// Most recently used first.
    pub recent_projects: Vec<PathBuf>,
    pub last_project: Option<PathBuf>,
    /// Show the developer menu (mock scenarios).
    pub developer_mode: bool,
    /// Last mock scenario id.
    pub dev_scenario: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            appearance: "system".into(),
            locale: String::new(),
            ui_font_size: 14.0,
            editor_font_size: 13.0,
            editor_font_family: String::new(),
            show_line_numbers: true,
            recent_projects: Vec::new(),
            last_project: None,
            developer_mode: cfg!(debug_assertions),
            dev_scenario: "normal".into(),
        }
    }
}

impl AppSettings {
    pub fn appearance(&self) -> Appearance {
        Appearance::from_id(&self.appearance)
    }

    pub fn locale(&self) -> Locale {
        Locale::from_id(&self.locale).unwrap_or_else(Locale::from_system)
    }

    pub fn push_recent(&mut self, project: &Path) {
        self.recent_projects.retain(|p| p != project);
        self.recent_projects.insert(0, project.to_path_buf());
        self.recent_projects.truncate(10);
        self.last_project = Some(project.to_path_buf());
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(?path, error = %e, "invalid settings file; using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        write_atomic(path, text.as_bytes())
    }
}

/// Writes via a temporary file + rename so a crash never leaves half a file.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Global holding the settings and where they persist (`None` in tests).
pub struct SettingsStore {
    pub settings: AppSettings,
    pub path: Option<PathBuf>,
}

impl Global for SettingsStore {}

impl SettingsStore {
    pub fn get(cx: &App) -> &AppSettings {
        &cx.global::<SettingsStore>().settings
    }

    /// Applies `f`, persists and notifies observers.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut AppSettings)) {
        cx.update_global::<SettingsStore, _>(|store, _| {
            f(&mut store.settings);
            if let Some(path) = &store.path
                && let Err(e) = store.settings.save(path)
            {
                tracing::error!(?path, error = %e, "failed to save settings");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_tolerate_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut s = AppSettings {
            locale: "pl".into(),
            ..Default::default()
        };
        s.push_recent(Path::new("/tmp/a"));
        s.save(&path).unwrap();
        assert_eq!(AppSettings::load(&path), s);

        std::fs::write(&path, "locale = \"en\"\nfuture_option = 1\n").unwrap();
        let loaded = AppSettings::load(&path);
        assert_eq!(loaded.locale, "en");
        assert_eq!(loaded.ui_font_size, AppSettings::default().ui_font_size);
    }
}
