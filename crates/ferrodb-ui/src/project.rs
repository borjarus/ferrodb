//! Projects: a directory with a `.ferrodb/` folder holding per-project state
//! (plan 5.10): `layout.json` now; data sources and consoles in later phases.

use std::path::{Path, PathBuf};

use ferrodb_ui_kit::kit::dock::DockAreaState;
use serde::{Deserialize, Serialize};

use crate::settings::write_atomic;

/// Version of the saved dock layout. Bump when panel names or the default
/// arrangement change incompatibly; older layouts are then discarded.
pub const LAYOUT_VERSION: usize = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
}

/// What we store in `layout.json` besides the dock state.
#[derive(Debug, Serialize, Deserialize)]
pub struct SavedLayout {
    pub version: usize,
    pub dock: DockAreaState,
}

impl Project {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join(".ferrodb"))?;
        Ok(Self { root })
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join(".ferrodb")
    }

    pub fn layout_path(&self) -> PathBuf {
        self.state_dir().join("layout.json")
    }

    /// The saved layout, if present, readable and of the current version.
    pub fn load_layout(&self) -> Option<DockAreaState> {
        let text = std::fs::read_to_string(self.layout_path()).ok()?;
        match serde_json::from_str::<SavedLayout>(&text) {
            Ok(l) if l.version == LAYOUT_VERSION => Some(l.dock),
            Ok(l) => {
                tracing::info!(
                    found = l.version,
                    expected = LAYOUT_VERSION,
                    "discarding old layout"
                );
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "invalid layout.json; using default layout");
                None
            }
        }
    }

    pub fn save_layout(&self, dock: DockAreaState) -> std::io::Result<()> {
        let saved = SavedLayout {
            version: LAYOUT_VERSION,
            dock,
        };
        let json = serde_json::to_vec_pretty(&saved).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(self.state_dir())?;
        write_atomic(&self.layout_path(), &json)
    }

    pub fn delete_layout(&self) {
        let _ = std::fs::remove_file(self.layout_path());
    }
}

/// The project used when none was opened yet.
pub fn default_project_root(data_dir: &Path) -> PathBuf {
    data_dir.join("default-project")
}
