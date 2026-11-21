//! Redis (`redis` 1.8 on tokio, rustls/ring): standalone servers, Redis
//! Cluster (slot routing by the client, key scans across every master) and
//! Sentinel (master discovery), TLS and mutual TLS, ACL user / password,
//! database index, RESP3 with a RESP2 option, SSH tunnel (standalone).
//!
//! Why `redis`: the maintained reference client (redis-rs), async on
//! tokio, with cluster and Sentinel support and rustls without aws-lc;
//! `fred` would also do but pulls more runtime machinery than a console
//! needs.
//!
//! A console line is one command (arguments parsed like `redis-cli`), its
//! reply rendered as a table ([`reply`]). Read-only sources refuse writes by
//! classifying commands client-side ([`crate::kv::classify`]). The key
//! browser uses native calls ([`crate::kv::method`], [`native`]).
//! Cancellation unblocks blocking commands (`CLIENT UNBLOCK` from a second
//! connection); other commands are abandoned and the connection reopened.

mod conn;
pub mod native;
pub mod reply;

use std::sync::Arc;

use ferrodb_api::{
    AccessLayer, AuthMethod, Capabilities, DataModel, DriverCategory, DriverDescriptor, DriverId,
    DriverProperty, ExplainSupport, HierarchySpec, LanguageLevel, ObjectKind, QueryLanguage,
    SchemaKind, SqlDialectId, TxSupport,
};

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig, Result,
    registry::{property, secret_property},
};

pub use conn::{RedisConnection, Topology, cluster_masters, info_field, parse_hosts};

pub const DEFAULT_PORT: u16 = 6379;
pub const DEFAULT_SENTINEL_PORT: u16 = 26379;

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(RedisDriver)]
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        data_model: DataModel::KeyValue,
        language: QueryLanguage::RedisCommands,
        access: AccessLayer::Native,
        language_level: LanguageLevel::B,
        // `MULTI … EXEC` in one go, no interactive transactions.
        transactions: TxSupport::SingleStatement,
        schema: SchemaKind::KeysOnly,
        joins: false,
        explain: ExplainSupport::None,
        // `CLIENT UNBLOCK` for blocking commands.
        server_cancel: true,
        editable_results: true,
        cost_estimate: false,
        auth_methods: vec![AuthMethod::None, AuthMethod::Password],
    }
}

pub struct RedisDriver;

struct RedisVariant;

impl FamilyVariant for RedisVariant {
    fn driver_id(&self) -> DriverId {
        "redis".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "redis".into()
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
        let level = |kind, children: &[ObjectKind]| ferrodb_api::HierarchyLevel {
            kind,
            children: children.to_vec(),
        };
        HierarchySpec {
            levels: vec![
                level(
                    ObjectKind::Database,
                    &[ObjectKind::KeyPrefix, ObjectKind::Key],
                ),
                level(
                    ObjectKind::KeyPrefix,
                    &[ObjectKind::KeyPrefix, ObjectKind::Key],
                ),
            ],
            key_separator: Some(":".into()),
        }
    }
    fn list_queries(&self, _path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        // Not SQL: the explorer uses the Redis introspector (ferrodb-meta).
        Vec::new()
    }
}

fn with_auth(p: DriverProperty, auth: &[AuthMethod]) -> DriverProperty {
    DriverProperty {
        auth: auth.to_vec(),
        ..p
    }
}

impl Driver for RedisDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let pw = [AuthMethod::Password];
        DriverDescriptor {
            id: "redis".into(),
            family: "redis".into(),
            name: "Redis".into(),
            category: DriverCategory::NoSql,
            capabilities: capabilities(),
            requirements: Vec::new(),
            properties: vec![
                property(
                    "host",
                    "Host (cluster seeds / Sentinels: host:port, …)",
                    Some("localhost"),
                    true,
                ),
                property("port", "Port", Some("6379"), false),
                property("database", "Database index", Some("0"), false),
                with_auth(property("user", "User (ACL)", None, false), &pw),
                secret_property("password", "Password", &pw),
                property(
                    "mode",
                    "Mode (standalone, cluster, sentinel)",
                    Some("standalone"),
                    false,
                ),
                property("sentinel.master", "Sentinel master name", None, false),
                property("sentinel.user", "Sentinel user", None, false),
                secret_property("sentinel.password", "Sentinel password", &[]),
                property("protocol", "Protocol (resp3, resp2)", Some("resp3"), false),
                property("ssl.cert", "Client certificate (PEM)", None, false),
                property("ssl.key", "Client key (PEM)", None, false),
            ],
            default_port: Some(DEFAULT_PORT),
            url_template: Some("redis://{host}:{port}/{database}".into()),
        }
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(RedisVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let c = RedisConnection::connect(cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}
