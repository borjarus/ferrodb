//! ADBC drivers (Snowflake, Google BigQuery; plan phase 8) in the core's
//! registry. Their libraries are downloaded on demand (ADR-007); until the
//! driver manager provides a [`ferrodb_adbc::LibraryLocator`], they are
//! looked up in `<data_dir>/drivers/adbc` (and through the
//! `FERRODB_ADBC_<DRIVER>` variables and the `driver.library` property).

use std::{path::Path, sync::Arc};

use ferrodb_adbc::DefaultLocator;
use ferrodb_drivers::Registry;

/// Directory searched for ADBC driver libraries.
pub fn library_dir(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("drivers").join("adbc")
}

/// `registry` plus the ADBC drivers.
pub fn with_adbc(mut registry: Registry, data_dir: Option<&Path>) -> Registry {
    let dirs = data_dir.map(library_dir).into_iter().collect();
    ferrodb_adbc::register(&mut registry, Arc::new(DefaultLocator::new(dirs)));
    registry
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
}
