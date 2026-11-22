//! ADBC drivers (Snowflake, Google BigQuery; plan phase 8) in the core's
//! registry. Their libraries are downloaded on demand (ADR-007) by the
//! driver manager: [`catalog_artifacts`] turns the wheels listed by
//! `ferrodb-adbc` into driver manager artifacts, and [`ManagerLocator`]
//! loads what it installed. A library can also be given explicitly (the
//! `driver.library` property, `FERRODB_ADBC_<DRIVER>`) or dropped into
//! `<data_dir>/drivers/adbc`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ferrodb_adbc::{DefaultLocator, LibraryLocator, artifacts};
use ferrodb_api::Result;
use ferrodb_driver_manager::{
    Artifact, ArtifactFile, ArtifactKind, ArtifactVersion, DriverManager,
};
use ferrodb_drivers::{Registry, ResolvedConfig};

/// Directory searched for ADBC driver libraries put there by hand.
pub fn library_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("drivers").join("adbc")
}

/// `registry` plus the ADBC drivers, without a driver manager (tests).
pub fn with_adbc(mut registry: Registry, data_dir: Option<&Path>) -> Registry {
    let dirs = data_dir.map(library_dir).into_iter().collect();
    ferrodb_adbc::register(&mut registry, Arc::new(DefaultLocator::new(dirs)));
    registry
}

/// The ADBC libraries as driver manager artifacts: one artifact per driver,
/// one version per release, one file per platform (the library extracted
/// from its wheel under the name the platform loader expects).
pub fn catalog_artifacts() -> Vec<Artifact> {
    let mut by_driver: BTreeMap<&str, Vec<&artifacts::Artifact>> = BTreeMap::new();
    for a in artifacts::ARTIFACTS {
        by_driver.entry(a.driver).or_default().push(a);
    }
    by_driver
        .into_iter()
        .map(|(driver, builds)| {
            let first = builds[0];
            let mut versions: Vec<ArtifactVersion> = Vec::new();
            for b in &builds {
                let file = ArtifactFile {
                    url: b.url.to_owned(),
                    sha256: b.sha256.to_owned(),
                    size: Some(b.size),
                    file_name: Some(artifacts::library_file_name(b.library, b.os)),
                    extract: Some(b.path_in_archive.to_owned()),
                    os: Some(b.os.to_owned()),
                    arch: Some(b.arch.to_owned()),
                };
                match versions.iter_mut().find(|v| v.version == b.version) {
                    Some(v) => v.files.push(file),
                    None => versions.push(ArtifactVersion {
                        version: b.version.to_owned(),
                        min_java: 0,
                        files: vec![file],
                    }),
                }
            }
            Artifact {
                id: driver.to_owned(),
                kind: ArtifactKind::NativeLibrary,
                name: format!("ADBC driver ({})", first.library),
                coordinates: first.library.replace('_', "-"),
                license: first.license.to_owned(),
                license_url: "https://www.apache.org/licenses/LICENSE-2.0".into(),
                driver_class: None,
                versions,
            }
        })
        .collect()
}

/// Libraries installed by the driver manager first, then the explicit and
/// hand-placed ones.
pub struct ManagerLocator {
    pub manager: Arc<DriverManager>,
    pub fallback: DefaultLocator,
}

impl LibraryLocator for ManagerLocator {
    fn locate(&self, driver: &str, library: &str, cfg: &ResolvedConfig) -> Result<PathBuf> {
        // An explicit library wins over the downloaded one.
        if cfg.prop(ferrodb_adbc::LIBRARY_PROPERTY).is_none()
            && let Ok(files) = self.manager.driver_files(driver)
            && let Some(f) = files.files.into_iter().next()
        {
            return Ok(f);
        }
        self.fallback.locate(driver, library, cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adbc_drivers_are_registered() {
        let r = with_adbc(Registry::empty(), Some(Path::new("/data")));
        let ids: Vec<String> = r.list().iter().map(|d| d.id().to_string()).collect();
        assert_eq!(ids, ["snowflake", "bigquery"]);
        let bq = r.get(&"bigquery".into()).unwrap();
        assert!(bq.capabilities().cost_estimate);
        assert_eq!(bq.family().as_str(), "adbc");
    }

    #[test]
    fn wheels_become_driver_manager_artifacts() {
        let a = catalog_artifacts();
        let ids: Vec<&str> = a.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["bigquery", "snowflake"]);
        for art in &a {
            assert_eq!(art.kind, ArtifactKind::NativeLibrary);
            let v = &art.versions[0];
            let win = v
                .files
                .iter()
                .find(|f| f.os.as_deref() == Some("windows"))
                .expect("a Windows build");
            assert!(win.local_name().ends_with(".dll"), "{}", win.local_name());
            assert!(win.extract.as_deref().unwrap().ends_with(".so"));
        }
    }
}
