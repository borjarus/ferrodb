//! Test support shared by this crate's tests and the driver TCK: a bridge
//! over a driver directory in `target/`, with the jars of the requested
//! drivers. Returns `None` (the test is skipped) without Java ≥ 17, without
//! `ferrodb-jdbc-host.jar`, or when jars are missing and downloads are not
//! allowed (`FERRODB_TCK_JDBC` unset).

use std::{path::PathBuf, sync::Arc};

use ferrodb_driver_manager::{Catalog, DriverManager};

use crate::{Bridge, find_host_jar};

/// `target/ferrodb-drivers-test` of the workspace.
pub fn drivers_dir() -> PathBuf {
    std::env::var_os("FERRODB_DRIVERS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ferrodb-drivers-test")
        })
}

/// A bridge able to load the drivers `ids`, or why not.
pub fn bridge(ids: &[&str]) -> Result<Arc<Bridge>, String> {
    let jar =
        find_host_jar().ok_or("ferrodb-jdbc-host.jar not built (mvn -f java/jdbc-host package)")?;
    let manager = Arc::new(DriverManager::new(drivers_dir(), Catalog::builtin()));
    if manager.java().is_none() {
        return Err("no Java runtime >= 17".into());
    }
    let download = std::env::var_os("FERRODB_TCK_JDBC").is_some();
    for id in ids {
        if manager.installed(id).is_none() {
            if !download {
                return Err(format!(
                    "{id}: jars not installed (set FERRODB_TCK_JDBC=1 to download)"
                ));
            }
            manager
                .download(id, None)
                .map_err(|e| format!("{id}: {e}"))?;
        }
    }
    Ok(Bridge::new(manager, Some(jar)))
}
