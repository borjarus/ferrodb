//! `<config>/release.toml`: update channel and automatic checks, crash
//! report consent, time of the last update check.
//!
//! ```toml
//! consent = "ask"            # ask | always | never
//! last_update_check = 1795000000
//!
//! [updates]
//! channel = "stable"         # stable | beta
//! check_automatically = true
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ferrodb_api::{CrashConsent, UpdateSettings};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleaseSettings {
    pub consent: CrashConsent,
    /// Unix seconds of the last update check (successful or not).
    pub last_update_check: Option<i64>,
    pub updates: UpdateSettings,
}

/// The settings, in memory and (optionally) on disk.
pub struct SettingsFile {
    path: Option<PathBuf>,
    value: Mutex<ReleaseSettings>,
}

impl SettingsFile {
    pub const FILE: &'static str = "release.toml";

    /// Loads `<dir>/release.toml` (defaults when missing or broken).
    pub fn load(dir: Option<&Path>) -> Self {
        let path = dir.map(|d| d.join(Self::FILE));
        let value = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|t| match toml::from_str(&t) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(error = %e, "release.toml is invalid, using defaults");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            path,
            value: Mutex::new(value),
        }
    }

    pub fn get(&self) -> ReleaseSettings {
        self.value.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Changes and writes the settings (atomically: temp file + rename).
    pub fn update(&self, f: impl FnOnce(&mut ReleaseSettings)) -> std::io::Result<()> {
        let snapshot = {
            let mut v = self.value.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut v);
            v.clone()
        };
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&snapshot).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use ferrodb_api::UpdateChannel;

    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let s = SettingsFile::load(Some(dir.path()));
        assert_eq!(s.get(), ReleaseSettings::default());
        assert_eq!(s.get().consent, CrashConsent::Ask);
        s.update(|v| {
            v.consent = CrashConsent::Always;
            v.updates.channel = UpdateChannel::Beta;
            v.last_update_check = Some(42);
        })
        .unwrap();
        let again = SettingsFile::load(Some(dir.path()));
        assert_eq!(again.get(), s.get());
        let text = fs::read_to_string(dir.path().join("release.toml")).unwrap();
        assert!(text.contains("consent = \"always\""), "{text}");
        assert!(text.contains("channel = \"beta\""), "{text}");

        fs::write(dir.path().join("release.toml"), "consent = 7").unwrap();
        assert_eq!(
            SettingsFile::load(Some(dir.path())).get(),
            ReleaseSettings::default()
        );
    }
}
