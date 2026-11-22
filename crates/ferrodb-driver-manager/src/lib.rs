//! # ferrodb-driver-manager
//!
//! Everything a non-native driver needs on disk (ADR-007, plan 8.2):
//!
//! - [`Catalog`] — artifacts (JDBC jars now; ADBC libraries and runtimes use
//!   the same [`Artifact`] type) with real Maven Central coordinates,
//!   versions, SHA-256 and licenses;
//! - downloads with progress, verification before anything is installed,
//!   version selection and update detection;
//! - offline installation from a local file (still verified against the
//!   catalog) and custom driver files (level C, unverified);
//! - the Java runtime: an installed JDK / JRE or a downloaded Temurin JRE.
//!
//! Layout under the data directory given by the core:
//!
//! ```text
//! <dir>/state.json                  installed versions, custom files, runtime
//! <dir>/jdbc/<id>/<version>/*.jar   catalog artifacts
//! <dir>/jdbc/<id>/custom/*.jar      user files
//! <dir>/runtime/<release>/          downloaded JRE
//! ```
//!
//! Every call is blocking (disk, network): the core runs them on its
//! blocking pool.

pub mod catalog;
pub mod download;
pub mod java;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

pub use catalog::{Artifact, ArtifactFile, ArtifactKind, ArtifactVersion, Catalog};
use ferrodb_api::DriverStatus;
pub use java::JavaRuntime;
use serde::{Deserialize, Serialize};

/// Java feature version the JDBC host needs.
pub const MIN_JAVA: u32 = 17;
/// Temurin feature version downloaded when no suitable Java is installed.
pub const DOWNLOAD_JAVA: u32 = 21;

#[derive(Debug)]
pub enum ManagerError {
    Io(std::io::Error),
    Network(String),
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    Untrusted(String),
    Archive(String),
    NotFound(String),
    Unsupported(String),
}

impl std::error::Error for ManagerError {}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Network(e) => write!(f, "download failed: {e}"),
            Self::Checksum {
                file,
                expected,
                actual,
            } => write!(
                f,
                "checksum mismatch for {file}: expected SHA-256 {expected}, got {actual}"
            ),
            Self::Untrusted(u) => write!(f, "refusing to download from an untrusted URL: {u}"),
            Self::Archive(e) => write!(f, "archive: {e}"),
            Self::NotFound(e) => write!(f, "{e}"),
            Self::Unsupported(e) => write!(f, "not supported: {e}"),
        }
    }
}

impl From<std::io::Error> for ManagerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ManagerError> for ferrodb_api::Error {
    fn from(e: ManagerError) -> Self {
        match e {
            ManagerError::NotFound(m) => Self::NotFound(m),
            ManagerError::Unsupported(m) => Self::Unsupported(m),
            other => Self::Other(other.to_string()),
        }
    }
}

/// What is installed for one driver.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// Catalog version; `None` for custom files.
    #[serde(default)]
    pub version: Option<String>,
    /// Files relative to the manager directory.
    pub files: Vec<PathBuf>,
    /// User-provided files (level C, unverified).
    #[serde(default)]
    pub custom: bool,
    /// Driver class given by the user for custom files.
    #[serde(default)]
    pub driver_class: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    drivers: BTreeMap<String, Installed>,
    /// Licenses accepted (artifact id → version of the acceptance).
    #[serde(default)]
    licenses: BTreeMap<String, String>,
}

/// Files to load for a driver, verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverFiles {
    pub files: Vec<PathBuf>,
    /// Class to load; `None` = discover through `java.sql.Driver` services.
    pub driver_class: Option<String>,
    pub version: Option<String>,
    pub custom: bool,
    /// Lowest Java feature version the files run on (0 = unknown).
    pub min_java: u32,
}

type Listener = Arc<dyn Fn(&str, &DriverStatus) + Send + Sync>;

pub struct DriverManager {
    dir: PathBuf,
    catalog: Catalog,
    state: Mutex<State>,
    /// Transient statuses (downloading, failed).
    transient: Mutex<HashMap<String, DriverStatus>>,
    /// Files verified in this process: (path, modified) → checksum ok.
    verified: Mutex<HashSet<(PathBuf, Option<SystemTime>)>>,
    listeners: Mutex<Vec<Listener>>,
    java: Mutex<Option<JavaRuntime>>,
}

impl DriverManager {
    /// A manager storing files under `dir` (created on first download).
    pub fn new(dir: impl Into<PathBuf>, catalog: Catalog) -> Self {
        let dir = dir.into();
        let state = fs::read(dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            dir,
            catalog,
            state: Mutex::new(state),
            transient: Mutex::default(),
            verified: Mutex::default(),
            listeners: Mutex::default(),
            java: Mutex::default(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Called with every status change (downloads, installs, failures).
    pub fn on_status(&self, f: impl Fn(&str, &DriverStatus) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Arc::new(f));
    }

    fn emit(&self, id: &str, status: &DriverStatus) {
        let ls: Vec<Listener> = self
            .listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for l in ls {
            l(id, status);
        }
    }

    fn set_transient(&self, id: &str, s: Option<DriverStatus>) {
        let mut t = self.transient.lock().unwrap_or_else(|p| p.into_inner());
        match s {
            Some(s) => {
                t.insert(id.to_owned(), s);
            }
            None => {
                t.remove(id);
            }
        }
    }

    fn save(&self, state: &State) -> Result<(), ManagerError> {
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("state.json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(state).map_err(|e| ManagerError::Archive(e.to_string()))?,
        )?;
        fs::rename(tmp, self.dir.join("state.json"))?;
        Ok(())
    }

    fn installed_entry(&self, id: &str) -> Option<Installed> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drivers
            .get(id)
            .cloned()
    }

    /// What is installed for `id`.
    pub fn installed(&self, id: &str) -> Option<Installed> {
        self.installed_entry(id)
            .filter(|i| i.files.iter().all(|f| self.dir.join(f).is_file()))
    }

    /// Versions offered by the catalog, newest first.
    pub fn versions(&self, id: &str) -> Vec<String> {
        self.catalog
            .get(id)
            .map(|a| a.versions.iter().map(|v| v.version.clone()).collect())
            .unwrap_or_default()
    }

    /// Java feature version used to pick compatible artifact versions: the
    /// detected runtime, or the one that would be downloaded.
    fn java_feature(&self) -> u32 {
        self.java_cached().map_or(DOWNLOAD_JAVA, |j| j.feature)
    }

    /// The newest catalog version usable with the available Java.
    pub fn latest(&self, id: &str) -> Option<String> {
        let feature = self.java_feature();
        self.catalog
            .get(id)
            .and_then(|a| a.latest(Some(feature)))
            .map(|v| v.version.clone())
    }

    pub fn status(&self, id: &str) -> DriverStatus {
        if let Some(t) = self
            .transient
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
        {
            return t.clone();
        }
        match self.installed(id) {
            Some(i) if i.custom => DriverStatus::Custom {
                files: i.files.len(),
            },
            Some(Installed {
                version: Some(v), ..
            }) => match self.latest(id) {
                Some(latest) if latest != v && self.is_newer(id, &latest, &v) => {
                    DriverStatus::UpdateAvailable {
                        installed: v,
                        latest,
                    }
                }
                _ => DriverStatus::Downloaded { version: v },
            },
            _ => DriverStatus::NotDownloaded,
        }
    }

    /// `a` comes before `b` in the catalog (versions are listed newest first).
    fn is_newer(&self, id: &str, a: &str, b: &str) -> bool {
        let vs = self.versions(id);
        match (
            vs.iter().position(|v| v == a),
            vs.iter().position(|v| v == b),
        ) {
            (Some(x), Some(y)) => x < y,
            (Some(_), None) => true,
            _ => false,
        }
    }

    fn version_of(&self, id: &str, version: Option<&str>) -> Result<ArtifactVersion, ManagerError> {
        let a = self.catalog.get(id).ok_or_else(|| {
            ManagerError::NotFound(format!("driver `{id}` is not in the catalog"))
        })?;
        let v = match version {
            Some(v) => a.version(v).ok_or_else(|| {
                ManagerError::NotFound(format!("version {v} of `{id}` is not in the catalog"))
            })?,
            None => a
                .latest(Some(self.java_feature()))
                .ok_or_else(|| ManagerError::NotFound(format!("no version of `{id}`")))?,
        };
        Ok(v.clone())
    }

    /// License of an artifact: (name, URL).
    pub fn license(&self, id: &str) -> Option<(String, String)> {
        self.catalog
            .get(id)
            .map(|a| (a.license.clone(), a.license_url.clone()))
    }

    /// Records that the user accepted the license of `id` (shown before the
    /// first download).
    pub fn accept_license(&self, id: &str) -> Result<(), ManagerError> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let lic = self
            .catalog
            .get(id)
            .map(|a| a.license.clone())
            .unwrap_or_default();
        st.licenses.insert(id.to_owned(), lic);
        let snapshot = st.clone();
        drop(st);
        self.save(&snapshot)
    }

    pub fn license_accepted(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .licenses
            .contains_key(id)
    }

    fn version_dir(&self, id: &str, version: &str) -> PathBuf {
        PathBuf::from("jdbc").join(id).join(version)
    }

    fn record(&self, id: &str, inst: Installed) -> Result<(), ManagerError> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.drivers.insert(id.to_owned(), inst);
        st.licenses.entry(id.to_owned()).or_insert_with(|| {
            self.catalog
                .get(id)
                .map(|a| a.license.clone())
                .unwrap_or_default()
        });
        let snapshot = st.clone();
        drop(st);
        self.save(&snapshot)
    }

    /// Downloads and verifies `version` of `id` (`None` = newest usable),
    /// reporting progress as `Downloading` statuses. Using a driver implies
    /// accepting its license (the UI shows it before calling this).
    pub fn download(&self, id: &str, version: Option<&str>) -> Result<String, ManagerError> {
        let v = self.version_of(id, version)?;
        let files: Vec<ArtifactFile> = v.files_for_this_platform().cloned().collect();
        let total: u64 = files.iter().filter_map(|f| f.size).sum();
        let rel_dir = self.version_dir(id, &v.version);
        let mut done_before = 0u64;
        let mut rel_files = Vec::new();
        self.set_transient(
            id,
            Some(DriverStatus::Downloading {
                progress_permille: 0,
            }),
        );
        self.emit(
            id,
            &DriverStatus::Downloading {
                progress_permille: 0,
            },
        );
        let r = (|| {
            for f in &files {
                let rel = rel_dir.join(f.local_name());
                let dest = self.dir.join(&rel);
                let ok_already = dest.is_file()
                    && download::sha256_file(&dest)
                        .map(|h| h.eq_ignore_ascii_case(&f.sha256))
                        .unwrap_or(false);
                if !ok_already {
                    let last = Mutex::new(0u16);
                    let base = done_before;
                    let progress = |done: u64, size: Option<u64>| {
                        let all = if total > 0 { total } else { size.unwrap_or(0) };
                        if all == 0 {
                            return;
                        }
                        let p = (((base + done) * 1000) / all).min(999) as u16;
                        let mut l = last.lock().unwrap_or_else(|x| x.into_inner());
                        if p >= *l + 10 {
                            *l = p;
                            let s = DriverStatus::Downloading {
                                progress_permille: p,
                            };
                            self.set_transient(id, Some(s.clone()));
                            self.emit(id, &s);
                        }
                    };
                    match &f.extract {
                        None => download::download(&f.url, &f.sha256, &dest, &progress)?,
                        Some(member) => {
                            let archive = self
                                .dir
                                .join("downloads")
                                .join(f.url.rsplit('/').next().unwrap_or("archive"));
                            download::download(&f.url, &f.sha256, &archive, &progress)?;
                            let out = download::extract(
                                &archive,
                                dest.parent().unwrap_or(&self.dir),
                                Some(member),
                            )?;
                            let _ = fs::remove_file(&archive);
                            if out != dest {
                                fs::rename(out, &dest)?;
                            }
                        }
                    }
                }
                done_before += f.size.unwrap_or(0);
                self.mark_verified(&dest);
                rel_files.push(rel);
            }
            self.record(
                id,
                Installed {
                    version: Some(v.version.clone()),
                    files: rel_files,
                    custom: false,
                    driver_class: None,
                },
            )
        })();
        match r {
            Ok(()) => {
                self.set_transient(id, None);
                self.emit(id, &self.status(id));
                Ok(v.version)
            }
            Err(e) => {
                let s = DriverStatus::Failed(e.to_string());
                self.set_transient(id, Some(s.clone()));
                self.emit(id, &s);
                Err(e)
            }
        }
    }

    /// Offline installation: `paths` are local copies of the catalog files of
    /// `version` (`None` = the version whose checksums match). Every file is
    /// verified against the catalog.
    pub fn install_offline(
        &self,
        id: &str,
        version: Option<&str>,
        paths: &[PathBuf],
    ) -> Result<String, ManagerError> {
        let a = self.catalog.get(id).ok_or_else(|| {
            ManagerError::NotFound(format!("driver `{id}` is not in the catalog"))
        })?;
        let hashes: Vec<(PathBuf, String)> = paths
            .iter()
            .map(|p| Ok((p.clone(), download::sha256_file(p)?)))
            .collect::<Result<_, ManagerError>>()?;
        let candidates: Vec<&ArtifactVersion> = match version {
            Some(v) => a.version(v).into_iter().collect(),
            None => a.versions.iter().collect(),
        };
        let v = candidates
            .into_iter()
            .find(|v| {
                v.files_for_this_platform().all(|f| {
                    hashes
                        .iter()
                        .any(|(_, h)| h.eq_ignore_ascii_case(&f.sha256))
                })
            })
            .ok_or_else(|| ManagerError::Checksum {
                file: paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                expected: format!("the files of a catalog version of `{id}`"),
                actual: hashes
                    .iter()
                    .map(|(_, h)| h.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            })?;
        let rel_dir = self.version_dir(id, &v.version);
        let mut rel_files = Vec::new();
        for f in v.files_for_this_platform() {
            let (src, _) = hashes
                .iter()
                .find(|(_, h)| h.eq_ignore_ascii_case(&f.sha256))
                .expect("matched above");
            let rel = rel_dir.join(f.local_name());
            let dest = self.dir.join(&rel);
            download::install_file(src, Some(&f.sha256), &dest)?;
            self.mark_verified(&dest);
            rel_files.push(rel);
        }
        self.record(
            id,
            Installed {
                version: Some(v.version.clone()),
                files: rel_files,
                custom: false,
                driver_class: None,
            },
        )?;
        self.set_transient(id, None);
        self.emit(id, &self.status(id));
        Ok(v.version.clone())
    }

    /// Uses the user's own files for `id` (level C: unverified). The files
    /// are copied, so moving the originals does not break the data source.
    pub fn use_custom(
        &self,
        id: &str,
        paths: &[PathBuf],
        driver_class: Option<String>,
    ) -> Result<(), ManagerError> {
        if paths.is_empty() {
            return Err(ManagerError::NotFound("no driver files given".into()));
        }
        let rel_dir = PathBuf::from("jdbc").join(id).join("custom");
        let _ = fs::remove_dir_all(self.dir.join(&rel_dir));
        let mut files = Vec::new();
        for p in paths {
            if !p.is_file() {
                return Err(ManagerError::NotFound(format!("{} not found", p.display())));
            }
            let name = p
                .file_name()
                .ok_or_else(|| ManagerError::NotFound(p.display().to_string()))?;
            let rel = rel_dir.join(name);
            download::install_file(p, None, &self.dir.join(&rel))?;
            files.push(rel);
        }
        self.record(
            id,
            Installed {
                version: None,
                files,
                custom: true,
                driver_class: driver_class.filter(|c| !c.trim().is_empty()),
            },
        )?;
        self.set_transient(id, None);
        self.emit(id, &self.status(id));
        Ok(())
    }

    /// Forgets the installation of `id` (files are deleted).
    pub fn remove(&self, id: &str) -> Result<(), ManagerError> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.drivers.remove(id);
        let snapshot = st.clone();
        drop(st);
        let _ = fs::remove_dir_all(self.dir.join("jdbc").join(id));
        self.save(&snapshot)?;
        self.emit(id, &DriverStatus::NotDownloaded);
        Ok(())
    }

    fn mark_verified(&self, path: &Path) {
        let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
        self.verified
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((path.to_owned(), mtime));
    }

    /// The files to load for `id`, verified again against the catalog before
    /// use (once per process and file version). Custom files are not
    /// verified.
    pub fn driver_files(&self, id: &str) -> Result<DriverFiles, ManagerError> {
        let inst = self.installed(id).ok_or_else(|| {
            ManagerError::NotFound(format!(
                "the JDBC driver for `{id}` is not downloaded (Drivers → Download)"
            ))
        })?;
        let abs: Vec<PathBuf> = inst.files.iter().map(|f| self.dir.join(f)).collect();
        let artifact = self.catalog.get(id);
        if !inst.custom {
            let v = inst
                .version
                .as_deref()
                .and_then(|v| artifact.and_then(|a| a.version(v)));
            for (p, rel) in abs.iter().zip(&inst.files) {
                let mtime = fs::metadata(p).and_then(|m| m.modified()).ok();
                let key = (p.clone(), mtime);
                if self
                    .verified
                    .lock()
                    .unwrap_or_else(|x| x.into_inner())
                    .contains(&key)
                {
                    continue;
                }
                let name = rel.file_name().map(|n| n.to_string_lossy().into_owned());
                let expected = v.and_then(|v| {
                    v.files
                        .iter()
                        .find(|f| Some(f.local_name()) == name)
                        .map(|f| f.sha256.clone())
                });
                if let Some(expected) = expected {
                    let got = download::sha256_file(p)?;
                    if !got.eq_ignore_ascii_case(&expected) {
                        return Err(ManagerError::Checksum {
                            file: p.display().to_string(),
                            expected,
                            actual: got,
                        });
                    }
                }
                self.verified
                    .lock()
                    .unwrap_or_else(|x| x.into_inner())
                    .insert(key);
            }
        }
        Ok(DriverFiles {
            files: abs,
            driver_class: if inst.custom {
                inst.driver_class.clone()
            } else {
                artifact.and_then(|a| a.driver_class.clone())
            },
            min_java: inst
                .version
                .as_deref()
                .and_then(|v| artifact.and_then(|a| a.version(v)))
                .map_or(0, |v| v.min_java),
            version: inst.version.clone(),
            custom: inst.custom,
        })
    }

    fn runtime_dir(&self) -> PathBuf {
        self.dir.join("runtime")
    }

    fn java_cached(&self) -> Option<JavaRuntime> {
        let mut j = self.java.lock().unwrap_or_else(|p| p.into_inner());
        if j.is_none() {
            *j = java::detect(MIN_JAVA, &java::downloaded(&self.runtime_dir()));
        }
        j.clone()
    }

    /// A Java runtime ≥ [`MIN_JAVA`]: installed or downloaded earlier.
    pub fn java(&self) -> Option<JavaRuntime> {
        self.java_cached()
    }

    /// Downloads a Temurin JRE ([`DOWNLOAD_JAVA`]) into the data directory.
    pub fn download_java(&self) -> Result<JavaRuntime, ManagerError> {
        let id = "java-runtime";
        let progress = |done: u64, total: Option<u64>| {
            if let Some(t) = total.filter(|t| *t > 0) {
                let s = DriverStatus::Downloading {
                    progress_permille: ((done * 1000) / t).min(999) as u16,
                };
                self.emit(id, &s);
            }
        };
        let r = java::download_temurin(DOWNLOAD_JAVA, &self.runtime_dir(), &progress)?;
        *self.java.lock().unwrap_or_else(|p| p.into_inner()) = Some(r.clone());
        self.emit(
            id,
            &DriverStatus::Downloaded {
                version: r.version.clone(),
            },
        );
        Ok(r)
    }
}

#[cfg(test)]
mod tests;
