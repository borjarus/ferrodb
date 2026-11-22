//! # ferrodb-adbc
//!
//! The ADBC access layer (ADR-005, plan phase 8): any ADBC driver library,
//! loaded at run time through `adbc_driver_manager`, becomes a FerroDb
//! [`Driver`]. Results arrive as Arrow record batches straight from the
//! driver; this crate re-chunks them to the sink's batch size.
//!
//! - [`Flavor`] — what differs per database: descriptor, connection
//!   options, listings (Snowflake, Google BigQuery, generic);
//! - [`LibraryLocator`] — where the library is (the core implements it on
//!   top of its driver manager; [`DefaultLocator`] for tests and builds
//!   without one);
//! - [`artifacts`] — the published library builds (PyPI wheels: URL,
//!   SHA-256, path inside the archive, license) and their extraction;
//! - [`bigquery`] — the BigQuery REST API for cost estimates (dry runs),
//!   plans (job statistics), jobs and their cancellation.
//!
//! Driver-specific calls go through `Request::Native`:
//!
//! | method | arguments | result columns |
//! |---|---|---|
//! | `get_objects` | `depth` (`catalogs`/`schemas`/`tables`/`columns`/`all`), `catalog`, `schema`, `table`, `table_type`, `column` | [`native::OBJECT_COLUMNS`] |
//! | `get_table_schema` | `catalog`, `schema`, `table` | `column_name`, `arrow_type`, `is_nullable` |
//! | `get_info` | — | `info_name`, `info_value` |
//! | `get_table_types` | — | `table_type` |
//! | `warehouses`, `roles`, `databases`, `current_context` (Snowflake) | — | as `SHOW …` |
//! | `use_warehouse`, `use_role`, `use_database` (Snowflake) | `name` | — |
//! | `dry_run` (BigQuery) | `sql` | `bytes`, `statement_type`, `job` (JSON) |
//! | `explain` (BigQuery) | `sql`, `analyze` (`true` runs the query) | `plan` (job JSON) |
//! | `jobs` (BigQuery) | `state` (`running`/`pending`/`done`), `limit` | [`bigquery::JOB_COLUMNS`] |
//! | `cancel_job` (BigQuery) | `job_id`, `location` | — |
//!
//! The driver manager crate itself contains the FFI; this crate needs no
//! `unsafe` code.

pub mod artifacts;
pub mod bigquery;
mod conn;
pub mod errors;
pub mod flavor;
pub mod locate;
pub mod native;

use std::sync::Arc;

use ferrodb_api::{
    AuthMethod, DriverDescriptor, DriverId, HierarchySpec, ObjectKind, SqlDialectId,
};
use ferrodb_drivers::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, Registry, ResolvedConfig,
    Result,
};

pub use conn::AdbcConnection;
pub use flavor::Flavor;
pub use locate::{DefaultLocator, LIBRARY_PROPERTY, LibraryLocator};

/// A database reached through an ADBC driver library.
pub struct AdbcDriver {
    flavor: Flavor,
    locator: Arc<dyn LibraryLocator>,
}

impl AdbcDriver {
    pub fn new(flavor: Flavor, locator: Arc<dyn LibraryLocator>) -> Self {
        Self { flavor, locator }
    }

    pub fn flavor(&self) -> &Flavor {
        &self.flavor
    }
}

struct AdbcVariant(Flavor);

impl FamilyVariant for AdbcVariant {
    fn driver_id(&self) -> DriverId {
        self.0.id().into()
    }
    fn dialect(&self) -> SqlDialectId {
        self.0.dialect().into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        self.0.auth()
    }
    fn quirks(&self) -> Quirks {
        self.0.quirks()
    }
    fn hierarchy(&self) -> HierarchySpec {
        self.0.hierarchy()
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        self.0.list_queries(path)
    }
}

impl Driver for AdbcDriver {
    fn descriptor(&self) -> DriverDescriptor {
        self.flavor.descriptor()
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(AdbcVariant(self.flavor.clone()))
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let library = self
                .locator
                .locate(self.flavor.id(), self.flavor.library(), cfg)?;
            let c = AdbcConnection::open(self.flavor.clone(), library, cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}

/// The Snowflake and BigQuery drivers.
pub fn drivers(locator: Arc<dyn LibraryLocator>) -> Vec<Arc<dyn Driver>> {
    vec![
        Arc::new(AdbcDriver::new(Flavor::Snowflake, locator.clone())),
        Arc::new(AdbcDriver::new(Flavor::BigQuery, locator)),
    ]
}

/// Adds the ADBC drivers to a registry.
pub fn register(registry: &mut Registry, locator: Arc<dyn LibraryLocator>) {
    for d in drivers(locator) {
        registry.register(d);
    }
}
