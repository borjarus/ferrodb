//! Finding the driver library to load. The core implements
//! [`LibraryLocator`] on top of its driver manager (downloaded artifacts);
//! [`DefaultLocator`] covers builds without one and tests: an explicit
//! path in the data source, an environment variable, then a list of
//! directories.

use std::path::PathBuf;

use ferrodb_api::{Error, Result};
use ferrodb_drivers::ResolvedConfig;

use crate::artifacts::library_file_name;

/// Data source property with an explicit library path (custom builds,
/// tests).
pub const LIBRARY_PROPERTY: &str = "driver.library";

/// Where the library of an ADBC flavor is on disk.
pub trait LibraryLocator: Send + Sync {
    /// Path of the library for driver `driver` (`snowflake`, `bigquery`,
    /// a generic id) whose library is named `library`
    /// (`adbc_driver_snowflake`). [`Error::DriverMissing`] when it is not
    /// downloaded.
    fn locate(&self, driver: &str, library: &str, cfg: &ResolvedConfig) -> Result<PathBuf>;
}

/// `driver.library` property → `FERRODB_ADBC_<DRIVER>` environment
/// variable → `dirs` (each searched for the platform file name, also in a
/// `<driver>` sub-directory).
#[derive(Clone, Debug, Default)]
pub struct DefaultLocator {
    pub dirs: Vec<PathBuf>,
}

impl DefaultLocator {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self { dirs }
    }

    /// Environment variable checked for `driver`.
    pub fn env_var(driver: &str) -> String {
        format!(
            "FERRODB_ADBC_{}",
            driver
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        )
    }
}

impl LibraryLocator for DefaultLocator {
    fn locate(&self, driver: &str, library: &str, cfg: &ResolvedConfig) -> Result<PathBuf> {
        if let Some(p) = cfg.prop(LIBRARY_PROPERTY) {
            let p = PathBuf::from(p);
            return if p.is_file() {
                Ok(p)
            } else {
                Err(Error::Connection(format!(
                    "ADBC driver library not found: {}",
                    p.display()
                )))
            };
        }
        if let Some(p) = std::env::var_os(Self::env_var(driver)).map(PathBuf::from)
            && p.is_file()
        {
            return Ok(p);
        }
        let file = library_file_name(library, std::env::consts::OS);
        for d in &self.dirs {
            for candidate in [d.join(&file), d.join(driver).join(&file)] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err(Error::DriverMissing(driver.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_then_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir
            .path()
            .join(library_file_name("adbc_driver_x", std::env::consts::OS));
        std::fs::write(&file, b"x").unwrap();
        let loc = DefaultLocator::new(vec![dir.path().to_owned()]);
        let cfg = ResolvedConfig::new("x");
        assert_eq!(loc.locate("x", "adbc_driver_x", &cfg).unwrap(), file);
        assert!(matches!(
            loc.locate("y", "adbc_driver_y", &cfg),
            Err(Error::DriverMissing(_))
        ));
        let cfg = cfg.with(LIBRARY_PROPERTY, "/no/such/file");
        assert!(loc.locate("x", "adbc_driver_x", &cfg).is_err());
        assert_eq!(DefaultLocator::env_var("my-db"), "FERRODB_ADBC_MY_DB");
    }
}
