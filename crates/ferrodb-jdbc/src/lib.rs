//! # ferrodb-jdbc
//!
//! The JDBC bridge client (ADR-006, plan 8.2): databases without a native
//! Rust driver — Oracle, IBM Db2, Sybase ASE, H2, Apache Derby, HyperSQL,
//! Exasol, Vertica, Apache Hive and custom JDBC drivers (level C) — are
//! reached through `ferrodb-jdbc-host`, a small Java process:
//!
//! - [`bridge`] — starts the host lazily on the first connection, watches
//!   it, restarts it after a crash ([`ferrodb_api::BridgeState`] for the
//!   Services panel); connections of a dead process report a connection
//!   error;
//! - [`protocol`] — frames on the host's stdin / stdout: JSON-RPC requests,
//!   Arrow IPC result streams, credits for backpressure;
//! - [`conn`] — [`ferrodb_drivers::Connection`] on a host-side JDBC
//!   connection, `DatabaseMetaData` as [`ferrodb_drivers::Request::Native`]
//!   calls, execution plans;
//! - [`flavor`] — the databases as data: descriptors, URLs (embedded file /
//!   in-memory URLs for H2, Derby and HyperSQL), explorer queries, schema
//!   switching.
//!
//! Driver jars and the Java runtime come from `ferrodb-driver-manager`.

pub mod bridge;
pub mod conn;
pub mod errors;
pub mod flavor;
pub mod protocol;
#[doc(hidden)]
pub mod testing;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub use bridge::{Bridge, find_host_jar};
use ferrodb_api::{
    AuthMethod, DriverDescriptor, DriverId, Error, HierarchySpec, ObjectKind, Result, SqlDialectId,
};
use ferrodb_driver_manager::ManagerError;
use ferrodb_drivers::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig,
    registry::relational_hierarchy,
};
pub use flavor::{FLAVORS, Flavor, flavor};
use serde_json::json;

/// Connection ids, unique per process lifetime of the client.
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// The drivers of every JDBC flavor, sharing one bridge.
pub fn drivers(bridge: &Arc<Bridge>) -> Vec<Arc<dyn Driver>> {
    FLAVORS
        .iter()
        .map(|f| {
            Arc::new(JdbcDriver {
                flavor: f,
                bridge: bridge.clone(),
            }) as Arc<dyn Driver>
        })
        .collect()
}

pub struct JdbcDriver {
    flavor: &'static Flavor,
    bridge: Arc<Bridge>,
}

impl JdbcDriver {
    pub fn new(flavor: &'static Flavor, bridge: Arc<Bridge>) -> Self {
        Self { flavor, bridge }
    }

    pub fn flavor(&self) -> &'static Flavor {
        self.flavor
    }
}

struct JdbcVariant(&'static Flavor);

impl FamilyVariant for JdbcVariant {
    fn driver_id(&self) -> DriverId {
        self.0.id.into()
    }
    fn dialect(&self) -> SqlDialectId {
        self.0.dialect.into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        self.0.auth.to_vec()
    }
    fn quirks(&self) -> Quirks {
        Quirks {
            transactional_ddl: self.0.transactional_ddl,
            // The connection refuses writes itself where the driver only
            // takes `setReadOnly` as a hint.
            read_only_sessions: true,
        }
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(self.0.container)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        self.0.list_queries(path)
    }
}

impl Driver for JdbcDriver {
    fn descriptor(&self) -> DriverDescriptor {
        self.flavor.descriptor(self.bridge.manager().catalog())
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(JdbcVariant(self.flavor))
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let f = self.flavor;
            let manager = self.bridge.manager().clone();
            let id = f.id;
            let files = tokio::task::spawn_blocking(move || manager.driver_files(id))
                .await
                .map_err(|e| Error::Other(e.to_string()))?
                .map_err(|e| match e {
                    ManagerError::NotFound(_) => Error::DriverMissing(id.to_owned()),
                    other => Error::Connection(other.to_string()),
                })?;
            // A driver built for a newer Java fails deep inside the class
            // loader: say it plainly.
            if let Some(java) = self.bridge.manager().java()
                && files.min_java > java.feature
            {
                return Err(Error::Connection(format!(
                    "{} {} needs Java {}, the bridge runs on Java {} ({}): install a newer Java                      or pick an older driver version",
                    f.name,
                    files.version.as_deref().unwrap_or_default(),
                    files.min_java,
                    java.feature,
                    java.java.display()
                )));
            }
            let endpoint = if f.uses_network(cfg) {
                Some(cfg.endpoint(f.port.unwrap_or(0)).await?)
            } else {
                None
            };
            let (host, port) = endpoint
                .as_ref()
                .map(|e| (e.host.clone(), e.port))
                .unwrap_or_else(|| (cfg.host().to_owned(), f.port.unwrap_or(0)));
            let url = f.url(cfg, &host, port).map_err(Error::Connection)?;
            let mut props = serde_json::Map::new();
            if let Some(u) = cfg.user() {
                props.insert("user".into(), json!(u));
            }
            if let Some(p) = cfg.password() {
                props.insert("password".into(), json!(p));
            }
            if f.id == "oracle" {
                props.insert("v$session.program".into(), json!(cfg.application_name));
            }
            for (k, v) in cfg.options() {
                if k != "connect_timeout" {
                    props.insert(k.into(), json!(v));
                }
            }
            let class = if files.custom {
                files
                    .driver_class
                    .clone()
                    .or_else(|| cfg.prop("driver_class").map(str::to_owned))
            } else {
                files.driver_class.clone()
            };
            let proc = self.bridge.process().await?;
            let conn_id = format!("c{}", NEXT_CONN.fetch_add(1, Ordering::Relaxed));
            let params = json!({
                "conn": conn_id,
                "driver_class": class.unwrap_or_default(),
                "jars": files.files.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                "url": url,
                "properties": props,
                "read_only": cfg.read_only,
            });
            let info = match tokio::time::timeout(cfg.connect_timeout, proc.call("connect", params))
                .await
            {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    proc.notify("close", json!({ "conn": conn_id }));
                    return Err(match e {
                        Error::Query(q) => Error::Connection(q.to_string()),
                        other => other,
                    });
                }
                Err(_) => {
                    proc.notify("close", json!({ "conn": conn_id }));
                    return Err(Error::Connection(format!(
                        "timed out after {} s",
                        cfg.connect_timeout.as_secs()
                    )));
                }
            };
            let version = format!(
                "{} {}",
                info["product"].as_str().unwrap_or(f.name),
                info["version"].as_str().unwrap_or_default()
            )
            .trim()
            .to_owned();
            proc.connection_opened();
            Ok(Box::new(conn::JdbcConnection {
                proc,
                id: conn_id,
                flavor: f,
                version,
                read_only: cfg.read_only,
                broken: false,
                current: Arc::new(AtomicU64::new(0)),
                _endpoint: endpoint,
            }) as Box<dyn Connection>)
        })
    }
}
