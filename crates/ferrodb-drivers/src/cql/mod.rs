//! Apache Cassandra through the ScyllaDB driver (`scylla` 1.10, fully
//! compatible with Cassandra): contact points, local data center, password
//! authentication, keyspace (`USE`), consistency and serial consistency as
//! properties and per console (`CONSISTENCY QUORUM`, cqlsh style), paging
//! with the server's paging state and backpressure, every CQL type
//! ([`types`]), LWT results (`[applied]`), server warnings as notices.
//!
//! No joins, no interactive transactions (`BEGIN BATCH … APPLY BATCH` is
//! one statement), no server-side cancel: cancelling abandons the request.
//! TLS is not compiled in: the driver's rustls feature depends on
//! `rustls` with its default provider (aws-lc-rs, a C build), which this
//! project does not use (`docs/phase-8.md`).

mod conn;
pub mod types;

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

pub use conn::{CqlConnection, Shell, is_read, parse_consistency, parse_serial, shell};

pub const DEFAULT_PORT: u16 = 9042;

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(CqlDriver)]
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        data_model: DataModel::Tabular,
        language: QueryLanguage::Cql,
        access: AccessLayer::Native,
        language_level: LanguageLevel::B,
        transactions: TxSupport::SingleStatement,
        schema: SchemaKind::Strict,
        joins: false,
        explain: ExplainSupport::None,
        server_cancel: false,
        editable_results: true,
        cost_estimate: false,
        auth_methods: vec![AuthMethod::Password, AuthMethod::None],
    }
}

pub struct CqlDriver;

struct CqlVariant;

impl FamilyVariant for CqlVariant {
    fn driver_id(&self) -> DriverId {
        "cassandra".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "cql".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        capabilities().auth_methods
    }
    fn quirks(&self) -> Quirks {
        Quirks {
            transactional_ddl: false,
            read_only_sessions: false,
        }
    }
    fn hierarchy(&self) -> HierarchySpec {
        let level = |kind, children: &[ObjectKind]| HierarchyLevel {
            kind,
            children: children.to_vec(),
        };
        HierarchySpec {
            levels: vec![
                level(
                    ObjectKind::Keyspace,
                    &[
                        ObjectKind::Table,
                        ObjectKind::MaterializedView,
                        ObjectKind::Function,
                    ],
                ),
                level(
                    ObjectKind::Table,
                    &[
                        ObjectKind::Column,
                        ObjectKind::PrimaryKey,
                        ObjectKind::Index,
                    ],
                ),
                level(ObjectKind::MaterializedView, &[ObjectKind::Column]),
            ],
            key_separator: None,
        }
    }
    fn list_queries(&self, _path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        // The explorer uses the Cassandra introspector (ferrodb-meta).
        Vec::new()
    }
}

fn with_auth(p: DriverProperty, auth: &[AuthMethod]) -> DriverProperty {
    DriverProperty {
        auth: auth.to_vec(),
        ..p
    }
}

impl Driver for CqlDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let pw = [AuthMethod::Password];
        DriverDescriptor {
            id: "cassandra".into(),
            family: "cql".into(),
            name: "Apache Cassandra".into(),
            category: DriverCategory::NoSql,
            capabilities: capabilities(),
            requirements: Vec::new(),
            properties: vec![
                property(
                    "host",
                    "Contact points (host[:port], …)",
                    Some("localhost"),
                    true,
                ),
                property("port", "Port", Some("9042"), false),
                property("database", "Keyspace", None, false),
                with_auth(property("user", "User", None, false), &pw),
                secret_property("password", "Password", &pw),
                property("local_dc", "Local data center", None, false),
                property(
                    "consistency",
                    "Consistency (LOCAL_ONE, QUORUM, LOCAL_QUORUM, ALL…)",
                    Some("LOCAL_ONE"),
                    false,
                ),
                property(
                    "address_translation",
                    "Address translation (none, contact_point: every node through the contact point)",
                    Some("none"),
                    false,
                ),
                property(
                    "serial_consistency",
                    "Serial consistency (SERIAL, LOCAL_SERIAL)",
                    None,
                    false,
                ),
            ],
            default_port: Some(DEFAULT_PORT),
            url_template: Some("cassandra://{host}:{port}/{database}".into()),
        }
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(CqlVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let c = CqlConnection::connect(cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}
