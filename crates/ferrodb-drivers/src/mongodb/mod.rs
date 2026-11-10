//! MongoDB (`mongodb` 3.10, rustls): `mongodb://` and `mongodb+srv://`
//! URIs, replica sets, sharding, TLS, SCRAM and X.509, read preference, SSH
//! tunnel (direct connection to one host).
//!
//! Every shell statement ([`ferrodb_lang::mql::Command`]) runs as a database
//! command; cursors are read as raw BSON (no decoding) and streamed in
//! batches. Cancellation drops the cursor and kills the server operation
//! (`killOp` of the operations tagged with this statement's `comment`).

mod conn;
mod exec;

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

pub use conn::MongoConnection;

pub const DEFAULT_PORT: u16 = 27017;

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(MongoDriver)]
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        data_model: DataModel::Document,
        language: QueryLanguage::Mql,
        access: AccessLayer::Native,
        language_level: LanguageLevel::A,
        // Replica sets and sharded clusters; a standalone server reports
        // that it has no transactions when one is started.
        transactions: TxSupport::MultiStatement,
        schema: SchemaKind::Inferred,
        joins: false,
        // Execution plans arrive in phase 7.
        explain: ExplainSupport::None,
        server_cancel: true,
        editable_results: true,
        cost_estimate: false,
        auth_methods: vec![AuthMethod::Password, AuthMethod::X509, AuthMethod::None],
    }
}

pub struct MongoDriver;

struct MongoVariant;

impl FamilyVariant for MongoVariant {
    fn driver_id(&self) -> DriverId {
        "mongodb".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "mql".into()
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
                    ObjectKind::Database,
                    &[ObjectKind::Collection, ObjectKind::View],
                ),
                level(ObjectKind::Collection, &[ObjectKind::Index]),
            ],
            key_separator: None,
        }
    }
    fn list_queries(&self, _path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        // Not SQL: the explorer uses the MongoDB introspector (ferrodb-meta).
        Vec::new()
    }
}

fn with_auth(p: DriverProperty, auth: &[AuthMethod]) -> DriverProperty {
    DriverProperty {
        auth: auth.to_vec(),
        ..p
    }
}

impl Driver for MongoDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let pw = [AuthMethod::Password];
        DriverDescriptor {
            id: "mongodb".into(),
            family: "mongodb".into(),
            name: "MongoDB".into(),
            category: DriverCategory::NoSql,
            capabilities: capabilities(),
            requirements: Vec::new(),
            properties: vec![
                property(
                    "uri",
                    "Connection string (mongodb:// or mongodb+srv://)",
                    None,
                    false,
                ),
                property("host", "Host", Some("localhost"), false),
                property("port", "Port", Some("27017"), false),
                property("database", "Database", Some("test"), false),
                with_auth(property("user", "User", None, false), &pw),
                secret_property("password", "Password", &pw),
                with_auth(
                    property("auth_source", "Authentication database", None, false),
                    &pw,
                ),
                with_auth(
                    property(
                        "auth_mechanism",
                        "Mechanism (SCRAM-SHA-256, SCRAM-SHA-1, PLAIN)",
                        None,
                        false,
                    ),
                    &pw,
                ),
                with_auth(
                    property("ssl.cert", "Client certificate + key (PEM)", None, false),
                    &[AuthMethod::X509],
                ),
                property("replica_set", "Replica set", None, false),
                property(
                    "read_preference",
                    "Read preference (primary, secondaryPreferred…)",
                    None,
                    false,
                ),
            ],
            default_port: Some(DEFAULT_PORT),
            url_template: Some("mongodb://{user}@{host}:{port}/{database}".into()),
        }
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(MongoVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let c = MongoConnection::connect(cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}
