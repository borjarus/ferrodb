//! TDS family: Microsoft SQL Server and Azure SQL (`tiberius`, rustls).
//!
//! `tiberius` is vendored with small patches (`third_party/tiberius`): the
//! raw token stream of a batch (row counts, `PRINT` / `RAISERROR` messages,
//! column precision and length), TDS attention for cancellation, and
//! `sql_variant` / CLR types decoded instead of panicking.
//!
//! - Authentication: SQL Server logins; Windows integrated (SSPI: Kerberos
//!   or NTLM as negotiated, the current Windows user — or NTLM with an
//!   explicit `DOMAIN\user` and password) on SQL Server; Microsoft Entra ID
//!   tokens on Azure SQL ([`entra`]).
//! - TLS: `ssl.mode` disable / prefer / require (encrypted, certificate not
//!   checked: "trust server certificate") / verify-full (system roots or
//!   `ssl.ca`); property `trust_server_certificate=true` forces trust.
//! - Batches: `GO` lines split the text ([`batch`]); every result set,
//!   row count and message of a batch reaches the sink; errors carry the
//!   line they occurred on.
//! - Cancellation: TDS attention on the same connection; the session (its
//!   temporary tables, `USE`, `SET` options) survives.
//! - Read-only: `ApplicationIntent=ReadOnly` (routes to readable
//!   secondaries) and statements that change data refused before they are
//!   sent (SQL Server has no read-only sessions).
//! - Schemas: SQL Server has no session default schema; `set_schema` keeps
//!   the chosen schema for the session (explorer, unqualified scripts are
//!   still resolved by the user's default schema) or switches the database
//!   with `USE` when the name is a database.

mod batch;
mod conn;
mod decode;
pub mod entra;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use ferrodb_api::{
    AuthMethod, DriverDescriptor, DriverId, DriverProperty, HierarchySpec, ObjectKind, Result,
    SqlDialectId,
};

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig,
    registry::{
        descriptor, network_properties, property, relational_capabilities, relational_hierarchy,
    },
};

/// One product of the TDS family.
pub struct Flavor {
    pub id: &'static str,
    pub name: &'static str,
    pub default_port: u16,
    pub default_database: Option<&'static str>,
    pub default_user: &'static str,
    pub auth: fn() -> Vec<AuthMethod>,
    /// Azure SQL: encryption is mandatory and the server may redirect.
    pub azure: bool,
}

pub static SQLSERVER: Flavor = Flavor {
    id: "sqlserver",
    name: "Microsoft SQL Server",
    default_port: 1433,
    default_database: Some("master"),
    default_user: "sa",
    auth: || vec![AuthMethod::Password, AuthMethod::Kerberos],
    azure: false,
};

pub static AZURESQL: Flavor = Flavor {
    id: "azuresql",
    name: "Azure SQL Server",
    default_port: 1433,
    default_database: None,
    default_user: "",
    auth: || vec![AuthMethod::Password, AuthMethod::EntraId],
    azure: true,
};

/// The T-SQL dialect id (`assets/dialects/tsql.toml`).
pub const DIALECT: &str = "tsql";

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![
        Arc::new(TdsDriver(&SQLSERVER)),
        Arc::new(TdsDriver(&AZURESQL)),
    ]
}

pub struct TdsDriver(pub &'static Flavor);

struct TdsVariant(&'static Flavor);

/// `N'…'` literal.
pub fn nliteral(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

/// `[name]`, doubling `]`.
pub fn bracket(s: &str) -> String {
    format!("[{}]", s.replace(']', "]]"))
}

/// Explorer listings from the `sys` catalog views of the current database.
pub fn list_queries(path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
    match path {
        // Role schemas (db_owner…) hold nothing; `sys` and
        // `INFORMATION_SCHEMA` come last.
        [] => vec![ListQuery {
            sql: "select s.name as name from sys.schemas s \
                  where s.schema_id < 16384 or s.schema_id > 16399 \
                  order by case when s.name = schema_name() then 0 \
                  when s.name in (N'sys', N'INFORMATION_SCHEMA', N'guest') then 2 else 1 end, s.name"
                .into(),
            kind: ObjectKind::Schema,
            has_children: true,
        }],
        [(ObjectKind::Schema, s)] => [("sys.tables", ObjectKind::Table), ("sys.views", ObjectKind::View)]
            .into_iter()
            .map(|(view, kind)| ListQuery {
                sql: format!(
                    "select o.name as name from {view} o \
                     where o.schema_id = schema_id({}) order by o.name",
                    nliteral(s)
                ),
                kind,
                has_children: true,
            })
            .collect(),
        [
            (ObjectKind::Schema, s),
            (ObjectKind::Table | ObjectKind::View, t),
        ] => vec![ListQuery {
            sql: format!(
                "select c.name as name, type_name(c.user_type_id) as type from sys.columns c \
                 where c.object_id = object_id(quotename({}) + N'.' + quotename({})) \
                 order by c.column_id",
                nliteral(s),
                nliteral(t)
            ),
            kind: ObjectKind::Column,
            has_children: false,
        }],
        _ => Vec::new(),
    }
}

impl FamilyVariant for TdsVariant {
    fn driver_id(&self) -> DriverId {
        self.0.id.into()
    }
    fn dialect(&self) -> SqlDialectId {
        DIALECT.into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        (self.0.auth)()
    }
    fn quirks(&self) -> Quirks {
        // DDL is transactional in SQL Server (except a few statements such
        // as CREATE DATABASE, which refuse to run inside a transaction).
        Quirks {
            transactional_ddl: true,
            read_only_sessions: false,
        }
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(ObjectKind::Schema)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        list_queries(path)
    }
}

fn extra_properties(f: &Flavor) -> Vec<DriverProperty> {
    let mut v = vec![property(
        "trust_server_certificate",
        "Trust the server certificate",
        Some(if f.azure { "false" } else { "true" }),
        false,
    )];
    if f.azure {
        v.extend(entra::properties());
    } else {
        v.push(DriverProperty {
            auth: vec![AuthMethod::Kerberos],
            ..property(
                "principal",
                "Windows account (DOMAIN\\user; empty = current user)",
                None,
                false,
            )
        });
    }
    v
}

impl Driver for TdsDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let f = self.0;
        let auth = (f.auth)();
        let mut props = network_properties(f.default_port, f.default_database);
        if !f.azure {
            for p in props.iter_mut().filter(|p| p.key == "password") {
                // An explicit Windows account authenticates with NTLM.
                p.auth.push(AuthMethod::Kerberos);
            }
        }
        props.extend(extra_properties(f));
        let mut d = descriptor(
            f.id,
            "tds",
            f.name,
            relational_capabilities(DIALECT, auth),
            props,
            Some(f.default_port),
            "sqlserver://{host}:{port};database={database}",
        );
        if f.azure {
            d.category = ferrodb_api::DriverCategory::AnalyticalCloud;
        }
        d
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(TdsVariant(self.0))
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(conn::connect(self.0, cfg))
    }
}
