//! ClickHouse over its HTTP interface (port 8123, TLS 8443), plan phase 8.
//!
//! Why HTTP and `reqwest`: the HTTP interface is the one ClickHouse keeps
//! stable across versions and cloud offerings, it can return results as an
//! Arrow IPC stream (`FORMAT ArrowStream`, no conversion on our side), and
//! each statement can carry its own `query_id` and settings. `reqwest`
//! gives streaming bodies (backpressure: an unread body stops the socket),
//! pooling, timeouts and DNS overrides (SSH tunnels) on top of `hyper`; TLS
//! uses the workspace's rustls + `ring` configuration.
//!
//! - one ClickHouse session per connection (`session_id`), so `USE db`,
//!   `SET` and temporary tables persist between statements;
//! - every statement gets a `query_id`; cancelling sends `KILL QUERY WHERE
//!   query_id = …` on a separate request and drops the response;
//! - errors carry the exception code name and the position of syntax
//!   errors ([`error`]);
//! - read-only sessions use the `readonly = 2` setting (reads and `SET`
//!   only);
//! - no transactions ([`ferrodb_api::TxSupport::None`]);
//! - imports load tab-separated text with `INSERT … FORMAT TabSeparated`
//!   (the `COPY` text format is the same).
//!
//! Connection options typed as `option.<setting>=<value>` are passed to
//! ClickHouse as settings of every statement.

mod conn;
mod error;
mod types;

use std::sync::Arc;

use ferrodb_api::{
    AuthMethod, DriverCategory, DriverDescriptor, DriverId, ExplainSupport, HierarchySpec,
    ObjectKind, SqlDialectId, TxSupport,
};

pub use conn::ClickHouseConnection;

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig, Result,
    registry::{
        descriptor, network_properties, property, relational_capabilities, relational_hierarchy,
    },
};

pub const DEFAULT_PORT: u16 = 8123;
pub const DEFAULT_TLS_PORT: u16 = 8443;

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(ClickHouseDriver)]
}

/// ClickHouse string literal: backslash escapes are always on.
pub fn ch_literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// ClickHouse identifier in backticks.
pub fn ch_ident(s: &str) -> String {
    format!("`{}`", s.replace('\\', "\\\\").replace('`', "\\`"))
}

pub struct ClickHouseDriver;

struct ClickHouseVariant;

impl FamilyVariant for ClickHouseVariant {
    fn driver_id(&self) -> DriverId {
        "clickhouse".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "clickhouse".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        vec![AuthMethod::Password]
    }
    fn quirks(&self) -> Quirks {
        Quirks {
            transactional_ddl: false,
            read_only_sessions: true,
        }
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(ObjectKind::Database)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        list_queries(path)
    }
}

const VIEW_ENGINES: &str = "('View', 'MaterializedView', 'LiveView', 'WindowView')";

fn list_queries(path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
    match path {
        [] => vec![ListQuery {
            sql: "select name from system.databases \
                  where name not in ('INFORMATION_SCHEMA', 'information_schema') \
                  order by name = currentDatabase() desc, name"
                .into(),
            kind: ObjectKind::Database,
            has_children: true,
        }],
        [(ObjectKind::Database, d)] => {
            let db = ch_literal(d);
            [
                (format!("engine not in {VIEW_ENGINES}"), ObjectKind::Table),
                (format!("engine in {VIEW_ENGINES}"), ObjectKind::View),
            ]
            .into_iter()
            .map(|(cond, kind)| ListQuery {
                sql: format!(
                    "select name, engine as detail from system.tables \
                     where database = {db} and not is_temporary and {cond} order by name"
                ),
                kind,
                has_children: true,
            })
            .collect()
        }
        [
            (ObjectKind::Database, d),
            (ObjectKind::Table | ObjectKind::View, t),
        ] => vec![ListQuery {
            sql: format!(
                "select name, type as detail from system.columns \
                 where database = {} and table = {} order by position",
                ch_literal(d),
                ch_literal(t)
            ),
            kind: ObjectKind::Column,
            has_children: false,
        }],
        _ => Vec::new(),
    }
}

impl Driver for ClickHouseDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let mut caps = relational_capabilities("clickhouse", vec![AuthMethod::Password]);
        caps.language_level = ferrodb_api::LanguageLevel::B;
        caps.transactions = TxSupport::None;
        caps.explain = ExplainSupport::Structured;
        caps.editable_results = true;
        let mut props = network_properties(DEFAULT_PORT, Some("default"));
        props.push(property(
            "ssl.mode",
            "TLS (disable / require / verify-full)",
            Some("disable"),
            false,
        ));
        let mut d = descriptor(
            "clickhouse",
            "clickhouse",
            "ClickHouse",
            caps,
            props,
            Some(DEFAULT_PORT),
            "clickhouse://{user}@{host}:{port}/{database}",
        );
        d.category = DriverCategory::AnalyticalCloud;
        d
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(ClickHouseVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let c = ClickHouseConnection::open(cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}

#[cfg(test)]
mod tests;
