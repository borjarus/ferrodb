//! Catalogs in memory and on disk (`<data dir>/metadata/<source>.json`).
//!
//! Catalogs are immutable `Arc`s: an update builds a new catalog and swaps
//! it in, so readers (explorer, completion, search) never wait for a
//! running introspection. Files are written atomically (temporary file +
//! rename) and ignored when unreadable or from another cache version.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ferrodb_api::DataSourceId;

use crate::model::{CACHE_VERSION, Catalog};

#[derive(Default)]
pub struct Catalogs {
    dir: Option<PathBuf>,
    map: Mutex<HashMap<DataSourceId, Option<Arc<Catalog>>>>,
}

fn file_name(ds: &DataSourceId) -> String {
    let safe: String = ds
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.json")
}

/// Reads a catalog file; `None` when missing, unreadable or outdated.
pub fn read(path: &Path) -> Option<Catalog> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<Catalog>(&bytes) {
        Ok(c) if c.version == CACHE_VERSION => Some(c),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable metadata cache");
            None
        }
    }
}

pub fn write(path: &Path, catalog: &Catalog) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec(catalog).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

impl Catalogs {
    /// `dir = None`: memory only.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            map: Mutex::default(),
        }
    }

    fn path(&self, ds: &DataSourceId) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(file_name(ds)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<DataSourceId, Option<Arc<Catalog>>>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The catalog of `ds`, read from disk on first use (blocking I/O:
    /// call [`Self::preload`] in the background at startup).
    pub fn get(&self, ds: &DataSourceId) -> Option<Arc<Catalog>> {
        if let Some(c) = self.lock().get(ds) {
            return c.clone();
        }
        let loaded = self.path(ds).and_then(|p| read(&p)).map(Arc::new);
        self.lock().entry(ds.clone()).or_insert(loaded).clone()
    }

    /// Cached catalog if already in memory (never touches the disk).
    pub fn peek(&self, ds: &DataSourceId) -> Option<Arc<Catalog>> {
        self.lock().get(ds).cloned().flatten()
    }

    /// Reads the catalogs of `sources` from disk.
    pub fn preload(&self, sources: &[DataSourceId]) {
        for ds in sources {
            let _ = self.get(ds);
        }
    }

    /// Replaces the catalog and writes it to disk.
    pub fn put(&self, ds: &DataSourceId, catalog: Catalog) -> Arc<Catalog> {
        let arc = Arc::new(catalog);
        self.lock().insert(ds.clone(), Some(arc.clone()));
        if let Some(p) = self.path(ds)
            && let Err(e) = write(&p, &arc)
        {
            tracing::warn!(path = %p.display(), error = %e, "metadata cache write failed");
        }
        arc
    }

    /// Forgets the source (removed data source).
    pub fn remove(&self, ds: &DataSourceId) {
        self.lock().remove(ds);
        if let Some(p) = self.path(ds) {
            let _ = std::fs::remove_file(p);
        }
    }
}
