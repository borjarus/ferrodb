//! Couchbase over its REST services (`reqwest`, rustls + ring):
//!
//! - the **query service** (`8093`, TLS `18093`) runs SQL++ with a
//!   `query_context` (the connection's bucket, optionally a scope) and a
//!   `client_context_id` per statement. The JSON response is read
//!   incrementally ([`stream`]): every element of `results` becomes a BSON
//!   document as soon as its bytes arrive and is sent to the sink before
//!   more bytes are read (backpressure down to the socket). `errors` come
//!   back with their code and position, `warnings` and `metrics` as
//!   notices. Cancelling closes the response and deletes the request from
//!   `system:active_requests`;
//! - the **cluster manager** (`8091`, TLS `18091`) lists buckets, scopes
//!   and collections ([`Request::Native`](crate::Request::Native) `buckets`,
//!   `scopes`; `indexes` reads `system:indexes`);
//! - **KV operations** (`kv_get`, `kv_insert`, `kv_upsert`, `kv_replace`,
//!   `kv_remove`) are SQL++ statements by key (`USE KEYS`, `UPSERT INTO …
//!   (KEY, VALUE)`). The binary memcached protocol (port 11210) would save
//!   a hop through the query service, but it needs vBucket maps, cluster
//!   topology tracking and SASL — the work of a full SDK — for operations
//!   an IDE issues one at a time;
//! - **transactions**: `BEGIN WORK` returns a `txid` that later statements,
//!   `COMMIT` and `ROLLBACK` carry (Couchbase Server 7.0+);
//! - authentication: user / password (HTTP basic) or an X.509 client
//!   certificate over TLS.
//!
//! Why HTTP and not the `couchbase` crate: the official SDK speaks the KV
//! protocol and wraps the query service behind its own row stream, with
//! its own TLS and cluster bootstrap; the IDE needs exactly the query
//! service with raw rows, positions of errors, `EXPLAIN`, `INFER`,
//! profiling, server-side cancel and the cluster manager listings — all
//! plain HTTP, on the TLS stack shared by every other driver.

mod conn;
pub mod sqlpp;
pub mod stream;

use std::sync::Arc;

use ferrodb_api::{
    AccessLayer, AuthMethod, Capabilities, DataModel, DriverCategory, DriverDescriptor, DriverId,
    DriverProperty, ExplainSupport, HierarchyLevel, HierarchySpec, LanguageLevel, ObjectKind,
    QueryLanguage, SchemaKind, SqlDialectId, TxSupport,
};

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig, Result,
    registry::{property, secret_property},
};

pub use conn::CouchbaseConnection;

/// Cluster manager (REST) port; the query service listens on 8093.
pub const DEFAULT_PORT: u16 = 8091;
pub const QUERY_PORT: u16 = 8093;
pub const TLS_PORT: u16 = 18091;
pub const TLS_QUERY_PORT: u16 = 18093;

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(CouchbaseDriver)]
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        data_model: DataModel::Document,
        language: QueryLanguage::SqlPlusPlus,
        access: AccessLayer::Native,
        language_level: LanguageLevel::B,
        // SQL++ transactions (BEGIN WORK … COMMIT), Couchbase Server 7.0+.
        transactions: TxSupport::MultiStatement,
        schema: SchemaKind::Inferred,
        joins: true,
        explain: ExplainSupport::Structured,
        server_cancel: true,
        editable_results: true,
        cost_estimate: false,
        auth_methods: vec![AuthMethod::Password, AuthMethod::X509],
    }
}

pub struct CouchbaseDriver;

struct CouchbaseVariant;

impl FamilyVariant for CouchbaseVariant {
    fn driver_id(&self) -> DriverId {
        "couchbase".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "sqlpp".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        capabilities().auth_methods
    }
    fn quirks(&self) -> Quirks {
        Quirks::default()
    }
    fn hierarchy(&self) -> HierarchySpec {
        let level = |kind, children: &[ObjectKind]| HierarchyLevel {
            kind,
            children: children.to_vec(),
        };
        HierarchySpec {
            levels: vec![
                level(ObjectKind::Bucket, &[ObjectKind::Scope]),
                level(ObjectKind::Scope, &[ObjectKind::Collection]),
                level(ObjectKind::Collection, &[ObjectKind::Index]),
            ],
            key_separator: None,
        }
    }
    fn list_queries(&self, _path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        // The explorer uses the Couchbase introspector (ferrodb-meta).
        Vec::new()
    }
}

impl Driver for CouchbaseDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let pw = [AuthMethod::Password];
        DriverDescriptor {
            id: "couchbase".into(),
            family: "couchbase".into(),
            name: "Couchbase".into(),
            category: DriverCategory::NoSql,
            capabilities: capabilities(),
            requirements: Vec::new(),
            properties: vec![
                property("host", "Host", Some("localhost"), true),
                property(
                    "port",
                    "Query service port (18093 with TLS)",
                    Some("8093"),
                    false,
                ),
                property(
                    "mgmt_port",
                    "Cluster manager port (18091 with TLS)",
                    Some("8091"),
                    false,
                ),
                property("database", "Bucket (query context)", None, false),
                property("scope", "Scope (query context)", None, false),
                DriverProperty {
                    auth: pw.to_vec(),
                    ..property("user", "User", None, false)
                },
                secret_property("password", "Password", &pw),
                DriverProperty {
                    auth: vec![AuthMethod::X509],
                    ..property("ssl.cert", "Client certificate + key (PEM)", None, false)
                },
                property(
                    "scan_consistency",
                    "Scan consistency (not_bounded, request_plus)",
                    Some("not_bounded"),
                    false,
                ),
            ],
            default_port: Some(DEFAULT_PORT),
            url_template: Some("couchbase://{host}/{database}".into()),
        }
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(CouchbaseVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let c = CouchbaseConnection::connect(cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}
